# syntax=docker/dockerfile:1

# ---- build stage: vp toolchain + pnpm install + vp build ----
# Build context is the REPO ROOT (not web/): the bundle imports the shared
# wire contract from contract/zod. Taskfile image:web passes -f web/Dockerfile.
FROM node:24-alpine AS build
RUN apk add --no-cache bash curl
# Vendored copy of the https://vite.plus installer — no curl|bash: the
# script is reviewed repo content and its SHA-256 is checked at build.
# VP_VERSION pins the toolchain to the workspace catalog's vite-plus
# (web/pnpm-workspace.yaml), so image builds use the same vp as dev/CI;
# the installer integrity-checks the npm tarball (dist.integrity) itself.
COPY web/vp-install.sh /tmp/vp-install.sh
RUN echo "6eaf139e16d45849bebc4a7de0dbc2992fd3b3a86a3f37bf7922b685f3a60d9e  /tmp/vp-install.sh" | sha256sum -c - \
	&& VP_VERSION=1.1.0 bash /tmp/vp-install.sh
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
