use anyhow::{anyhow, Context};
use aws_sdk_s3::config::{BehaviorVersion, Credentials};
use aws_types::region::Region;
use aws_types::sdk_config::SharedCredentialsProvider;
use aws_types::SdkConfig;
use clap::Parser;
use oci_distribution::manifest::{
    OciImageManifest, OciManifest, IMAGE_CONFIG_MEDIA_TYPE, IMAGE_MANIFEST_LIST_MEDIA_TYPE,
    IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_INDEX_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE,
};
use oci_distribution::secrets::RegistryAuth;
use oci_distribution::Reference;
use std::env::VarError;
use std::io::Write;

#[derive(Parser, Debug)]
struct Opts {
    /// The container image you want to mirror (e.g. `debian:bookworm-slim`)
    #[arg(long)]
    source_image: Reference,
    /// The reference the container image will have on your mirror, without including the path to
    /// the registry (e.g. `debian:bookworm-slim-mirrored`)
    #[arg(long)]
    target_image: Reference,
    /// The name of the bucket where the container image will be stored
    #[arg(long)]
    target_bucket: String,
}

fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize tokio runtime")?;

    rt.block_on(run(opts))
}

async fn run(opts: Opts) -> anyhow::Result<()> {
    let source_image_ref = opts.source_image;
    let target_repository = opts.target_image.repository();
    let target_bucket = opts.target_bucket;

    println!("Configuration:");
    println!("* Source image: `{}`", source_image_ref.whole());
    println!("* Target bucket: {target_bucket}");
    println!(
        "* Target image: `{}`",
        display_target_image(&opts.target_image)
    );

    // Pull the image's root manifest, associated to the provided tag or digest
    let oci_client =
        oci_distribution::Client::new(oci_distribution::client::ClientConfig::default());
    let (manifest_raw, manifest_digest) = oci_client
        .pull_manifest_raw(
            &source_image_ref,
            &RegistryAuth::Anonymous,
            &[
                IMAGE_MANIFEST_MEDIA_TYPE,
                IMAGE_MANIFEST_LIST_MEDIA_TYPE,
                OCI_IMAGE_MEDIA_TYPE,
                OCI_IMAGE_INDEX_MEDIA_TYPE,
            ],
        )
        .await
        .context("failed to pull root manifest")?;

    let manifest: OciManifest =
        serde_json::from_slice(&manifest_raw).context("invalid root manifest")?;

    let r2_client = r2_client()?;

    let mut image_manifests = Vec::new();
    match manifest {
        // We handle image manifests in a later step
        OciManifest::Image(image) => image_manifests.push((image, manifest_raw, manifest_digest)),
        OciManifest::ImageIndex(image_index) => {
            if let Some(target_tag) = opts.target_image.tag() {
                // Push the index under its tag name, if the target image uses a tag at all
                r2_client
                    .put_object()
                    .bucket(&target_bucket)
                    .key(manifest_key(target_repository, target_tag))
                    .body(manifest_raw.clone().into())
                    .content_type(OCI_IMAGE_INDEX_MEDIA_TYPE)
                    .send()
                    .await
                    .context("failed to push root manifest by tag")?;
            }

            // Push the index under its digest name
            r2_client
                .put_object()
                .bucket(&target_bucket)
                .key(manifest_key(target_repository, &manifest_digest))
                .body(manifest_raw.into())
                .content_type(OCI_IMAGE_INDEX_MEDIA_TYPE)
                .send()
                .await
                .context("failed to push root manifest by digest")?;

            // Enqueue manifests for later handling (each manifest is a "concrete" image, that is,
            // an image for a specific platform (OS + CPU architecture)
            for manifest in image_index.manifests {
                let manifest_digest = manifest.digest;
                let manifest_ref = Reference::with_digest(
                    source_image_ref.registry().to_string(),
                    source_image_ref.repository().to_string(),
                    manifest_digest.clone(),
                );
                let (manifest_raw, _) = oci_client
                    .pull_manifest_raw(
                        &manifest_ref,
                        &RegistryAuth::Anonymous,
                        &[IMAGE_MANIFEST_MEDIA_TYPE, OCI_IMAGE_MEDIA_TYPE],
                    )
                    .await
                    .context("failed to pull sub manifest")?;
                let manifest: OciImageManifest =
                    serde_json::from_slice(&manifest_raw).context("invalid sub manifest")?;
                image_manifests.push((manifest, manifest_raw, manifest_digest));
            }
        }
    }

    println!("{} manifests found", image_manifests.len());

    let mut buf = Vec::new();
    for (i, (manifest, manifest_raw, digest)) in image_manifests.into_iter().enumerate() {
        println!(
            "Mirroring manifest {} with {} layers...",
            i + 1,
            manifest.layers.len()
        );

        // Manifest
        r2_client
            .put_object()
            .bucket(&target_bucket)
            .key(manifest_key(target_repository, &digest))
            .body(manifest_raw.into())
            .content_type(OCI_IMAGE_MEDIA_TYPE)
            .send()
            .await
            .context("failed to push image manifest")?;

        // Layers
        for (layer_number, blob) in manifest.layers.into_iter().enumerate() {
            print!("* Mirroring layer {}...", layer_number + 1);
            std::io::stdout().flush()?;

            oci_client
                .pull_blob(&source_image_ref, &blob, &mut buf)
                .await
                .context("failed to pull blob")?;
            r2_client
                .put_object()
                .bucket(&target_bucket)
                .key(blob_key(target_repository, &blob.digest))
                .body(buf.into())
                .send()
                .await
                .context("failed to push blob")?;
            buf = Vec::new();

            println!(" Done!")
        }

        // Config
        oci_client
            .pull_blob(&source_image_ref, &manifest.config, &mut buf)
            .await
            .context("failed to pull blob")?;
        r2_client
            .put_object()
            .bucket(&target_bucket)
            .key(blob_key(target_repository, &manifest.config.digest))
            .body(buf.into())
            .content_type(IMAGE_CONFIG_MEDIA_TYPE)
            .send()
            .await
            .context("failed to push blob")?;
        buf = Vec::new();
    }

    println!("Successfully mirrored container image!");

    Ok(())
}

fn manifest_key(repo: &str, tag_or_digest: &str) -> String {
    // example w/ tag: /v2/my-image/manifests/latest
    // example w/ digest: /v2/my-image/manifests/sha256:dabf91b69c191a1a0a1628fd6bdd029c0c4018041c7f052870bb13c5a222ae76
    format!("v2/{repo}/manifests/{tag_or_digest}")
}

fn blob_key(repo: &str, digest: &str) -> String {
    // example: /v2/my-image/blobs/sha256:a606584aa9aa875552092ec9e1d62cb98d486f51f389609914039aabd9414687
    format!("v2/{repo}/blobs/{digest}")
}

fn r2_client() -> anyhow::Result<aws_sdk_s3::Client> {
    let access_key_id = get_env_var("S3_ACCESS_KEY_ID")?;
    let secret_access_key = get_env_var("S3_SECRET_ACCESS_KEY")?;
    let api_url = get_env_var("S3_API_URL")?;

    let credentials = Credentials::new(
        access_key_id,
        secret_access_key,
        None,
        None,
        "custom-credentials-provider",
    );

    let config = SdkConfig::builder()
        .endpoint_url(api_url)
        .credentials_provider(SharedCredentialsProvider::new(credentials))
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new("auto"))
        .build();

    Ok(aws_sdk_s3::Client::new(&config))
}

fn get_env_var(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|e| match e {
        VarError::NotPresent => anyhow!("missing environment variable `{key}`"),
        VarError::NotUnicode(_) => {
            anyhow!("environment variable `{key}` has invalid unicode bytes")
        }
    })
}

fn display_target_image(reference: &Reference) -> String {
    let mut s = reference.repository().to_string();

    if s.starts_with("library/") {
        s.replace_range(0.."library/".len(), "");
    }

    if let Some(t) = reference.tag() {
        s.push(':');
        s.push_str(t);
    }
    if let Some(d) = reference.digest() {
        s.push('@');
        s.push_str(d);
    }

    s
}
