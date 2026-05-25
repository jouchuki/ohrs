# syntax=docker/dockerfile:1.7
#
# Golden image for openharness-rs (ohrs).
#
# Multi-stage build:
#   1. builder  — compiles the `oh` binary in release mode using BuildKit
#                 cache mounts for the cargo registry/git and the target dir.
#   2. runtime  — minimal Debian slim base with just the binary + the runtime
#                 libraries it needs (glibc + CA certs for rustls).
#
# Build (BuildKit required, default on modern Docker):
#   docker build -t openharness-rs:dev .
#
# The version label/tag is normally injected by CI from git; see release.yml.

# ---- builder ---------------------------------------------------------------
FROM rust:1.94-bookworm AS builder

WORKDIR /build

# Copy the full workspace. .dockerignore keeps target/ and .git out of context.
COPY . .

# Compile only the `oh` binary (the `openharness` bin is identical — same
# src/main.rs). Cache mounts persist the registry, git deps and the target
# directory across builds. We copy the binary out of the cache-mounted target
# dir at the end of the same RUN, because cache mounts are not present in the
# resulting image layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin oh \
    && cp /build/target/release/oh /usr/local/bin/oh

# ---- runtime ---------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates: rustls (reqwest) verifies TLS against the system trust store.
# No openssl needed — the project builds reqwest with default-features=false +
# rustls-tls. libgcc/glibc come with the base image; the binary is dynamically
# linked (it uses libloading for plugins, so a full dynamic linker is required).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --create-home --uid 10001 ohrs
USER ohrs
WORKDIR /home/ohrs

COPY --from=builder /usr/local/bin/oh /usr/local/bin/oh

# OCI labels — overwritten/augmented by docker/metadata-action in CI.
ARG OHRS_VERSION="0.1.0"
ARG OHRS_VCS_REF="unknown"
LABEL org.opencontainers.image.title="openharness-rs" \
      org.opencontainers.image.description="OpenHarness CLI (oh) — AI-powered coding assistant" \
      org.opencontainers.image.source="https://github.com/HKUDS/openharness-rs" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${OHRS_VERSION}" \
      org.opencontainers.image.revision="${OHRS_VCS_REF}"

ENTRYPOINT ["oh"]
