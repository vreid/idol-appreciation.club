#!/usr/bin/env bash
# SLO gate for the batch import engine (docs/backend.md "Batch import"):
# ≥ 10 images/second sustained on ONE 4-vCPU node.
#
# Boots a THROWAWAY stack inside one node budget — backend constrained to
# --cpus=4 --memory=6g, postgres to --cpus=1 --memory=2g (≤ 5 cpu-seconds/s
# combined, vs the 4-vCPU/8GB node; PG idles well below its cap during
# import), file:// object store on tmpfs, fresh DB — then drives
# scripts/import-bulk.sh against it and prints the import metrics (rate,
# chunk writes, amplification) at the end.
#
# Usage: bench-import.sh COUNT RADIUS CHUNK [TAG]
#   e.g. bench-import.sh 10000 12000 2500 dense10k
#
# The backend image must exist: podman build -t idol-backend:bench backend/
set -euo pipefail

COUNT=${1:?usage: bench-import.sh COUNT RADIUS CHUNK [TAG]}
RADIUS=${2:?missing RADIUS}
CHUNK=${3:?missing CHUNK}
TAG=${4:-bench}
NET="bench-$TAG"
PW="bench-$TAG-admin-pw" # throwaway stack credential, not a secret
PORT=18080

cleanup() {
  podman rm -f -i "bench-backend-$TAG" "bench-pg-$TAG" >/dev/null 2>&1 || true
  podman network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

podman network create "$NET" >/dev/null
podman run -d --name "bench-pg-$TAG" --network "$NET" \
  --cpus=1 --memory=2g \
  -e POSTGRES_PASSWORD=bench -e POSTGRES_USER=bench -e POSTGRES_DB=bench \
  docker.io/library/postgres:16-alpine >/dev/null
echo "==> waiting for postgres"
for i in $(seq 1 60); do
  podman exec "bench-pg-$TAG" pg_isready -U bench >/dev/null 2>&1 && break
  [ "$i" = 60 ] && { echo "postgres never became ready" >&2; exit 1; }
  sleep 0.5
done

podman run -d --name "bench-backend-$TAG" --network "$NET" \
  --cpus=4 --memory=6g \
  --user 0:0 \
  -p "$PORT":8080 \
  -e DATABASE_URL=postgres://bench:bench@bench-pg-$TAG:5432/bench \
  -e S3_ENDPOINT=file:///data \
  -e S3_BUCKET=bench \
  -e SESSION_SECRET=bench-secret-0123456789abcdef0123456789abcdef \
  -e STATIC_DIR=/nonexistent \
  --tmpfs /data:size=8g \
  idol-backend:bench >/dev/null
echo "==> waiting for backend (migrations + listen)"
for i in $(seq 1 120); do
  curl -sf -m 2 "http://localhost:$PORT/api/health" >/dev/null 2>&1 && break
  [ "$i" = 120 ] && { echo "backend never became healthy" >&2; podman logs "bench-backend-$TAG" | tail -20 >&2; exit 1; }
  sleep 0.5
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
"$SCRIPT_DIR/import-bulk.sh" "http://localhost:$PORT" "$PW" "$PW" \
  --count "$COUNT" --radius "$RADIUS" --chunk "$CHUNK" --tag "$TAG"

echo "==> import metrics"
curl -s "http://localhost:$PORT/api/metrics" | grep -E '^import_|^chunk_write_amplification|^s3_ops_total'
