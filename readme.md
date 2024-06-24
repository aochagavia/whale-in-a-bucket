# Whale in a bucket

Experimental Rust tool to mirror container images from public registries to S3 (or S3-compatible)
buckets. See the [original blog post](TODO) for the story behind it.

### Try it out

```
$ docker run --rm pub-40af5d7df1e0402d9a92b982a6599860.r2.dev/cowsay

 _________________________
< This is seriously cool! >
 -------------------------
        \   ^__^
         \  (oo)\_______
            (__)\       )\/\
                ||----w |
                ||     ||
```

### Getting started

_Note: instead of using S3, we are using R2 for our examples. R2 buckets are much easier to expose
securely to the internet on a dedicated domain name (which is necessary to `docker pull` from them).
Fortunately, it doesn't matter whether you use R2 or S3, since they are API-compatible. As a matter
of fact, we use _the AWS SDK_ in the code, yet we know it works flawlessly with R2 credentials!_

Preparation:

- Create a public R2 bucket, either with a `r2.dev` subdomain or using your own domain (see
  [docs](https://developers.cloudflare.com/r2/buckets/public-buckets/)).
- Generate an API token with `Object Read & Write` permissions (see
  [docs](https://developers.cloudflare.com/r2/api/s3/tokens/)). Store the credentials in the
  `S3_ACCESS_KEY_ID` and `S3_SECRET_ACCESS_KEY` environment variables.
- Obtain the S3 API url associated to your account (see
  [docs](https://developers.cloudflare.com/r2/api/s3/api)) and store it in the `S3_API_URL`
  environment variable.

Mirror an image to your bucket (e.g. `alpine:latest`):

```bash
cargo run --release -- --source-image alpine:latest --target-image alpine:latest-mirrored --target-bucket my-bucket
```

Pull the container image from your bucket, using its public url:

```bash
docker pull https://{your-bucket-url}/alpine:latest-mirrored
```

### Limitations

The tool is meant as a proof-of-concept and has a bunch of minor limitations:

- No effort has been made to speed things up and there's clear low-hanging fruit. In particular, it
  would be easy to parallelize downloads and uploads, but I wanted to keep the code easy to follow.
  Also, we could refrain from re-uploading layers that are already present in the target repository,
  but we currently re-upload them anyway.
- There's no support for private registries. It would be very easy to add, but I didn't want to bloat
  the tool. Feel free to fork the repository or adapt the code!
- Each layer is uploaded to the bucket in a single request, instead of using multipart uploads. Big
  enough layers will run into size limits, but that can also easily be fixed by switching to
  multipart uploads.
- There's no support to pushing local images (i.e. those listed by `docker image ls`), so if you
  want to use this tool to get them on a bucket, you'll need to push them to a real registry first.
  Again, there's nothing fundamental in this limitation. Probably the best way to work around it is
  to create a proxy you can `docker push` to, which internally would transform the push into S3
  object uploads (I wrote a half-baked prototype proving that this is possible, but I only got it
  working with `skopeo` and currently don't have the time to get it working on `docker`). If you are
  up to the challenge, please open a PR with a registry proxy and I'll happily merge it!
