FROM --platform=$BUILDPLATFORM debian:bookworm-slim

# We remove everything apt-related after the install to keep the image size small
RUN apt update && apt install cowsay -y && rm -rf /var/cache/apt/archives /var/lib/apt/lists

ENTRYPOINT [ "/usr/games/cowsay", "This is seriously cool!" ]
