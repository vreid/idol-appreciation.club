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

FROM rust:trixie AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libvips-dev=8.16.1-1+deb13u2 \
        pkg-config \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
# Registry + git caches keep dependency builds warm across image builds.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --bin backend

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libvips42=8.16.1-1+deb13u2 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --shell /usr/sbin/nologin idol
WORKDIR /app
COPY --from=builder /app/target/release/backend /usr/local/bin/backend
ENV LISTEN_ADDR=0.0.0.0:8080
# STATIC_DIR is optional (dev convenience); when unset or missing the
# server serves a tiny JSON at / instead of the SPA.
USER idol
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/backend"]
