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

"${RUNNER}" network create "${NET}" >/dev/null

"${RUNNER}" run -d --name "${PG}" \
  --network "${NET}" \
  -e POSTGRES_PASSWORD=postgres \
  postgres:16-alpine >/dev/null

echo "waiting for postgres…"
for _ in $(seq 1 60); do
  if "${RUNNER}" exec "${PG}" pg_isready -U postgres >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
"${RUNNER}" exec "${PG}" pg_isready -U postgres >/dev/null

"${RUNNER}" run -d --name "${API}" \
  --network "${NET}" \
  -p "127.0.0.1:${BASE_PORT}:8080" \
  -e LISTEN_ADDR=0.0.0.0:8080 \
  -e "DATABASE_URL=postgres://postgres:postgres@${PG}:5432/postgres" \
  -e S3_ENDPOINT=file:///tmp/idol-contract-store \
  -e S3_BUCKET=contract-store \
  -e "SESSION_SECRET=${SESSION_SECRET}" \
  -e STATIC_DIR=/nonexistent \
  -e COOLDOWN_EMPTY_SECS=1 \
  -e COOLDOWN_PAINT_SECS=0 \
  "${IMAGE}" /app/target/release/backend >/dev/null

BASE_URL="http://127.0.0.1:${BASE_PORT}"
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
BASE_URL="${BASE_URL}" pnpm exec vp test --config vitest.contract.config.ts
