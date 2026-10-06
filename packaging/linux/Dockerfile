# Builds static (musl) Mellow binaries plus .deb and .rpm packages.
# Used by scripts/package-linux.sh; run per architecture with --platform.
FROM rust:1.90-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools rpm file \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
