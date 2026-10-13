# syntax=docker/dockerfile:1

# ---- build stage: vp toolchain + pnpm install + vp build ----
# Build context is the REPO ROOT (not web/): the bundle imports the shared
# wire contract from contract/zod. Taskfile image:web passes -f web/Dockerfile.
FROM node:24-alpine AS build
RUN apk add --no-cache bash curl
RUN curl -fsSL https://vite.plus | bash
ENV PATH="/root/.local/share/vite-plus/bin:${PATH}"
# Same pnpm pin as web's devEngines; corepack serves the contract install.
RUN corepack enable && corepack prepare pnpm@12.11.2 --activate
WORKDIR /app

COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN vp install --frozen-lockfile

# The contract package resolves its own zod from its node_modules; install
# it exactly like CI does (frozen lockfile).
COPY contract/ /contract/
RUN cd /contract && pnpm install --frozen-lockfile

COPY web/ ./
RUN vp build

# ---- runtime stage: Caddy serves the static SPA ----
FROM caddy:2-alpine
COPY web/Caddyfile /etc/caddy/Caddyfile
COPY --from=build /app/dist /usr/share/caddy

# Non-root: writable dirs for Caddy's data/config storage.
RUN mkdir -p /data /config \
  && chown -R 1000:1000 /data /config /usr/share/caddy /etc/caddy
USER 1000
ENV XDG_DATA_HOME=/data XDG_CONFIG_HOME=/config

EXPOSE 8080
