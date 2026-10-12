# syntax=docker/dockerfile:1

# ---- build stage: vp toolchain + pnpm install + vp build ----
FROM node:24-alpine AS build
RUN apk add --no-cache bash curl
RUN curl -fsSL https://vite.plus | bash
ENV PATH="/root/.local/share/vite-plus/bin:${PATH}"
WORKDIR /app

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN vp install --frozen-lockfile

COPY . .
RUN vp build

# ---- runtime stage: Caddy serves the static SPA ----
FROM caddy:2-alpine
COPY Caddyfile /etc/caddy/Caddyfile
COPY --from=build /app/dist /usr/share/caddy

# Non-root: writable dirs for Caddy's data/config storage.
RUN mkdir -p /data /config \
  && chown -R 1000:1000 /data /config /usr/share/caddy /etc/caddy
USER 1000
ENV XDG_DATA_HOME=/data XDG_CONFIG_HOME=/config

EXPOSE 8080
