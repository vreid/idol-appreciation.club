# syntax=docker/dockerfile:1
# Multi-stage build. NOTE: the builder needs libvips-dev (the `libvips`
# crate links against it); the runtime only needs libvips42.

FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends libvips-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies only — this layer caches unless deps change.
RUN cargo chef cook --release --recipe-path recipe.json
COPY src ./src
RUN cargo build --release --bin backend

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends libvips42 ca-certificates \
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
