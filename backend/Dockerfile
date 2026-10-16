# syntax=docker/dockerfile:1
# Multi-stage build.
#
# libvips is PINNED to Debian trixie's 8.16.1-1+deb13u2 in both stages so
# the shipped binary always runs against a known libvips/libwebp build
# (image-pipeline behavior is libvips-version sensitive; sha256 content
# keys are environment-scoped, see docs/backend.md). CI gates run against
# Ubuntu 24.04's libvips 8.15.1 and must therefore never assert
# cross-version byte determinism.
#
# NOTE: the builder needs libvips-dev (the `libvips` crate links against
# it); the runtime only needs libvips42.

# Dependency caching (cargo-chef): `planner` reduces the crate to a recipe
# (manifests + target layout), `builder` compiles the dependencies from that
# recipe in their own layer, then the crate itself. The dependency layer is
# keyed only on the recipe, so a source-only change rebuilds just the crate.
# CI exports this layer cache to the registry (images.yml, BUILD_CACHE_REF);
# the BuildKit cache mounts below only help warm local builders.
# `builder` stays the full-source stage: task backend:image builds
# --target builder for the fmt/clippy/test gate.
FROM rust:trixie AS chef
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libvips-dev=8.16.1-1+deb13u2 \
        pkg-config \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --bin backend -p iac-api --bin backend-worker

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libvips42=8.16.1-1+deb13u2 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --shell /usr/sbin/nologin idol
WORKDIR /app
COPY --from=builder /app/target/release/backend /usr/local/bin/backend
COPY --from=builder /app/target/release/backend-worker /usr/local/bin/backend-worker
ENV LISTEN_ADDR=0.0.0.0:8080
# STATIC_DIR is optional (dev convenience); when unset or missing the
# server serves a tiny JSON at / instead of the SPA.
USER idol
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/backend"]
