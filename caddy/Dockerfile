# syntax=docker/dockerfile:1

# Custom Caddy edge build (docs/scaling-architecture.md, "Caddy,
# horizontally"): cache-handler with in-memory otter storage caches
# immutable-marked tile responses, and CertMagic's S3 shared storage lets
# two or more edge replicas serve the same certificates and ACME state
# with a single coordinated renewal. Context is caddy/ (task image:caddy).
#
# certmagic-s3/ is VENDORED, not fetched: upstream v1.5.0 (6c26852) lacks
# four fixes a production deployment of the same module needed
# (missing-object reporting in Stat/Load, credential-free logs, reload
# refresher cleanup, endpoint validation) — see certmagic-s3/VENDORED.md.

ARG CADDY_VERSION=2.11.7
# Digests resolved via the registry API, 2026-10-15. The builder digest
# is also the xcaddy pin (the image ships xcaddy for this Caddy version).
ARG CADDY_BUILDER_IMAGE=docker.io/library/caddy:2.11.7-builder@sha256:d842facfd63b960922e57444ff76fd8f0793a66dfdcf481af8026e1f4e6a39b4
ARG CADDY_IMAGE=docker.io/library/caddy:2.11.7@sha256:685ae3d7c34d9b5f8caeddc5d14051db7bbd88ff1f5a2fa0c16bd8ca328fb7af

FROM ${CADDY_BUILDER_IMAGE} AS build
WORKDIR /src
COPY certmagic-s3/ ./certmagic-s3/
ENV CGO_ENABLED=0
# Module versions pinned digest-exact: cache-handler v0.17.0
# (07d7af2130699eb840e1f4ed49ef817da4201c2e) and the otter storage
# plugin v0.0.20 (81250fe6fc63358ff9f12faa72b01f36e48ed60c); the
# certmagic-s3 replacement builds the vendored directory.
RUN xcaddy build \
    --output /out/caddy \
    --with github.com/caddyserver/cache-handler@v0.17.0 \
    --with github.com/darkweak/storages/otter/caddy@v0.0.20 \
    --with github.com/techknowlogick/certmagic-s3=/src/certmagic-s3
# Fail the build rather than the deployment if a module did not make it in.
RUN /out/caddy list-modules | grep -qx 'caddy.storage.s3' \
    && /out/caddy list-modules | grep -qx 'http.handlers.cache'

FROM ${CADDY_IMAGE}
COPY --from=build /out/caddy /usr/bin/caddy
# /data and /config are local scratch only: certificates and ACME state
# live in object storage (chart/caddy/Caddyfile, `storage s3`). Same
# uid/chown contract as the web image so either binary runs the pod.
RUN mkdir -p /data /config \
  && chown -R 1000:1000 /data /config
USER 1000
ENV XDG_DATA_HOME=/data XDG_CONFIG_HOME=/config
EXPOSE 8080 8443
