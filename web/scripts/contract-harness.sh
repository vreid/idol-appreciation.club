#!/usr/bin/env bash
# Boots a REAL backend (pinned libvips gate image) + Postgres + the worker
# (ingest consumer + one render loop) for the contract integration suite
# (web/test/contract/), then runs vitest against it. Mirrors the
# container-runner conventions of the backend gates (RUNNER=podman
# locally, RUNNER=docker in CI) and cleans up on exit.
#
# Era note: the api binary never runs pixel work — a place answers 202
# and stays pending until a CONSUMER commits it. Without the worker the
# suite's cooldown/SSE expectations would time out, so the worker is part
# of the rig (docs/scaling-architecture.md "Upload flow").
#
# Taskfile web:contract is the only intended caller; BASE_PORT can be
# overridden for parallel local runs.
set -euo pipefail

RUNNER="${RUNNER:-podman}"
BASE_PORT="${CONTRACT_PORT:-18080}"
NET="idol-contract-net-$$"
PG="idol-contract-pg-$$"
API="idol-contract-api-$$"
WORKER="idol-contract-worker-$$"
STORE_VOL="idol-contract-store-$$"
STORE_DIR="/tmp/idol-contract-store-$$"
# 32-byte minimum enforced by the backend; value is throwaway per run.
SESSION_SECRET="contract-harness-0123456789abcdef0123456789abcdef"
IMAGE="${CONTRACT_IMAGE:-backend-builder}"

KEEP="${KEEP:-0}"
cleanup() {
  if [ "$KEEP" = "1" ]; then
    echo "KEEP=1: leaving containers up (api=$API worker=$WORKER pg=$PG)"
    return
  fi
  "${RUNNER}" rm -f "${API}" "${WORKER}" "${PG}" >/dev/null 2>&1 || true
  "${RUNNER}" volume rm -f "${STORE_VOL}" >/dev/null 2>&1 || true
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
    -e "S3_ENDPOINT=file://${STORE_DIR}" \
    -e S3_BUCKET=contract-store \
    -e "SESSION_SECRET=${SESSION_SECRET}" \
    -e STATIC_DIR=/nonexistent \
    -e COOLDOWN_EMPTY_SECS=1 \
    -e COOLDOWN_PAINT_SECS=1 \
    -e DATABASE_POOL_MAX=30 \
    -v "${STORE_VOL}:${STORE_DIR}" \
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

# First real DB round trip must succeed before anything else starts: this
# both proves the pool serves and gives the api's first connects a quiet
# window (a boot-time race with the worker's migrations once wedged the
# pool into 10s acquire timeouts).
settle=0
for _ in $(seq 1 30); do
  if curl -sf -m 3 "${BASE_URL}/api/center" >/dev/null 2>&1; then
    settle=1
    break
  fi
  sleep 1
done
if [ "$settle" != "1" ]; then
  echo "backend never served a DB request cleanly" >&2
  "${RUNNER}" logs "${API}" >&2 || true
  exit 1
fi

# The consumer, AFTER the api is settled: ingest Pipeline (202 flow) plus
# one render loop so painted tiles/versions events flow. The store is the
# SAME named volume the api mounted — quarantine objects are written by
# the api and read by the worker, so per-container filesystems would fail
# every job with permanent "not found".
"${RUNNER}" run -d --name "${WORKER}" \
  --network "${NET}" \
  -e "DATABASE_URL=postgres://postgres:postgres@${PG}:5432/postgres" \
  -e "S3_ENDPOINT=file://${STORE_DIR}" \
  -e S3_BUCKET=contract-store \
  -e "SESSION_SECRET=${SESSION_SECRET}" \
  -e RENDER_PIPELINE_ENABLED=true \
  -e RENDER_PIPELINE_WORKERS=1 \
  -e RENDER_PIPELINE_POLL_MS=250 \
  -e INGEST_PIPELINE_ENABLED=true \
  -e DATABASE_POOL_MAX=6 \
  -v "${STORE_VOL}:${STORE_DIR}" \
  "${IMAGE}" /app/target/release/backend-worker >/dev/null

# Give the worker's migrations + first claim loops a beat before the
# suite starts placing.
sleep 2

cd "$(dirname "$0")/.."
# NOT BASE_URL — vite overwrites that name in process.env with its app
# base ("/") under vitest, silently 404ing every request.
CONTRACT_BASE_URL="${BASE_URL}" pnpm exec vp test --config vitest.contract.config.ts
