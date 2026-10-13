#!/usr/bin/env bash
# Boots a REAL backend (pinned libvips gate image) + Postgres for the
# contract integration suite (web/test/contract/), then runs vitest against
# it. Mirrors the container-runner conventions of the backend gates
# (RUNNER=podman locally, RUNNER=docker in CI) and cleans up on exit.
#
# Taskfile web:contract is the only intended caller; BASE_PORT can be
# overridden for parallel local runs.
set -euo pipefail

RUNNER="${RUNNER:-podman}"
BASE_PORT="${CONTRACT_PORT:-18080}"
NET="idol-contract-net-$$"
PG="idol-contract-pg-$$"
API="idol-contract-api-$$"
# 32-byte minimum enforced by the backend; value is throwaway per run.
SESSION_SECRET="contract-harness-0123456789abcdef0123456789abcdef"
IMAGE="${CONTRACT_IMAGE:-backend-builder}"

cleanup() {
  "${RUNNER}" rm -f "${API}" "${PG}" >/dev/null 2>&1 || true
  "${RUNNER}" network rm "${NET}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# The API publishes a fixed host port (BASE_PORT) — shared state across
# runs: a crashed/aborted earlier run leaks its API container and every
# later run dies on "address already in use". Reap leftovers bound to OUR
# port only (never touches containers of a parallel run on another port).
# Parsed from the `{{.Ports}}` column rather than a `publish=` ps filter:
# docker supports that filter, podman rejects it as invalid (exit 125).
leaked="$("${RUNNER}" ps --format '{{.ID}} {{.Ports}}' \
  | awk -v pat=":${BASE_PORT}->" '$2 ~ pat {print $1}')"
if [ -n "${leaked}" ]; then
  echo "port ${BASE_PORT} held by leftover container(s), reaping: $("${RUNNER}" ps --format '{{.Names}} {{.Ports}}' | awk -v pat=":${BASE_PORT}->" '$2 ~ pat {print $1}' | tr '\n' ' ')"
  printf '%s\n' "${leaked}" | xargs -r "${RUNNER}" rm -f
fi

"${RUNNER}" network create "${NET}" >/dev/null

"${RUNNER}" run -d --name "${PG}" \
  --network "${NET}" \
  -e POSTGRES_PASSWORD=postgres \
  postgres:16-alpine >/dev/null

echo "waiting for postgres…"
pg_ready=0
for _ in $(seq 1 60); do
  if "${RUNNER}" exec "${PG}" pg_isready -U postgres >/dev/null 2>&1; then
    pg_ready=1
    break
  fi
  # PG exited during init? Surface its logs instead of a naked exit code.
  if [ "$("${RUNNER}" inspect -f '{{.State.Running}}' "${PG}" 2>/dev/null)" != "true" ]; then
    "${RUNNER}" logs "${PG}" >&2 || true
    echo "postgres container exited during init" >&2
    exit 1
  fi
  sleep 1
done
if [ "${pg_ready}" != "1" ]; then
  "${RUNNER}" logs "${PG}" >&2 || true
  echo "postgres never became ready (60s)" >&2
  exit 1
fi

# Foreign listeners (other runners' leaks, dev services) can hold the port
# where our reaper can't reach them — walk up to 9 ports upward on bind
# collision rather than dying on "address already in use".
port="${BASE_PORT}"
api_started=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
  if err="$("${RUNNER}" run -d --name "${API}" \
    --network "${NET}" \
    -p "127.0.0.1:${port}:8080" \
    -e LISTEN_ADDR=0.0.0.0:8080 \
    -e "DATABASE_URL=postgres://postgres:postgres@${PG}:5432/postgres" \
    -e "S3_ENDPOINT=file:///tmp/idol-contract-store-$$" \
    -e S3_BUCKET=contract-store \
    -e "SESSION_SECRET=${SESSION_SECRET}" \
    -e STATIC_DIR=/nonexistent \
    -e COOLDOWN_EMPTY_SECS=1 \
    -e COOLDOWN_PAINT_SECS=1 \
    "${IMAGE}" /app/target/release/backend 2>&1)"; then
    api_started=1
    break
  fi
  if printf '%s' "${err}" | grep -q "address already in use"; then
    echo "port ${port} in use by an outside listener, trying $((port + 1))"
    # A failed bind still registers the container name — drop the husk.
    "${RUNNER}" rm -f "${API}" >/dev/null 2>&1 || true
    port=$((port + 1))
    continue
  fi
  printf '%s\n' "${err}" >&2
  exit 1
done
if [ "${api_started}" != "1" ]; then
  echo "no free host port in ${BASE_PORT}..$((port - 1))" >&2
  exit 1
fi

BASE_URL="http://127.0.0.1:${port}"
echo "waiting for backend at ${BASE_URL}…"
ready=0
for _ in $(seq 1 60); do
  if curl -sf "${BASE_URL}/api/health" >/dev/null 2>&1; then
    ready=1
    break
  fi
  # The API container exits loudly when it cannot reach the DB; surface it.
  if [ "$("${RUNNER}" inspect -f '{{.State.Running}}' "${API}" 2>/dev/null)" != "true" ]; then
    "${RUNNER}" logs "${API}" >&2 || true
    echo "backend container exited during boot" >&2
    exit 1
  fi
  sleep 1
done
if [ "${ready}" != "1" ]; then
  "${RUNNER}" logs "${API}" >&2 || true
  echo "backend never became healthy" >&2
  exit 1
fi

cd "$(dirname "$0")/.."
# NOT BASE_URL — vite overwrites that name in process.env with its app
# base ("/") under vitest, silently 404ing every request.
CONTRACT_BASE_URL="${BASE_URL}" pnpm exec vp test --config vitest.contract.config.ts
