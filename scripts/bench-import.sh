#!/usr/bin/env bash
# SLO gate for the batch import engine (docs/backend.md "Batch import"):
# ≥ 10 images/second sustained on ONE 4-vCPU node.
#
# Boots a THROWAWAY stack inside one node budget — the worker constrained
# to --cpus=4 --memory=6g per invocation, postgres to --cpus=1
# --memory=2g (≤ 5 cpu-seconds/s combined, vs the 4-vCPU/8GB node; PG
# idles well below its cap during import), file:// object store on tmpfs,
# fresh DB — then drives scripts/import-bulk.sh against it and prints the
# per-chunk engine summaries (rate, chunk writes, amplification) at the
# end. The import_* Prometheus series live in the worker process, so the
# summaries (same counters) are the bench's evidence.
#
# Usage: bench-import.sh COUNT RADIUS CHUNK [TAG]
#   e.g. bench-import.sh 10000 12000 2500 dense10k
#
# The worker image must exist:
#   podman build --target runtime-worker -t idol-backend-worker:bench backend/
set -euo pipefail

COUNT=${1:?usage: bench-import.sh COUNT RADIUS CHUNK [TAG]}
RADIUS=${2:?missing RADIUS}
CHUNK=${3:?missing CHUNK}
TAG=${4:-bench}
NET="bench-$TAG"

cleanup() {
  podman rm -f -i "bench-pg-$TAG" >/dev/null 2>&1 || true
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

# Every worker invocation gets the full node budget (the engine is
# sequential across blocks, so one constrained container at a time).
worker_in_bench() {
  podman run --rm --network "$NET" \
    --cpus=4 --memory=6g \
    --user 0:0 \
    -v "$PWD/test-assets/idols":/srv/idols:ro \
    --tmpfs /data:size=8g \
    -e DATABASE_URL=postgres://bench:bench@bench-pg-$TAG:5432/bench \
    -e S3_ENDPOINT=file:///data \
    -e S3_BUCKET=bench \
    -e SESSION_SECRET=bench-secret-0123456789abcdef0123456789abcdef \
    idol-backend-worker:bench \
    "$@"
}

echo "==> migrations (one throwaway worker run)"
worker_in_bench import-originals >/dev/null

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Simplest honest flow: run the two subcommands from dedicated containers.
echo "==> preupload"
worker_in_bench import-originals /srv/idols/*.jpg > /tmp/bench-preupload-$TAG.json

echo "==> generating manifest (import-manifest.py)"
BATCH=$(python3 -c "import uuid; print(uuid.uuid5(uuid.NAMESPACE_URL, 'import-bench:$TAG'))")
"$SCRIPT_DIR/import-manifest.py" \
  --map /tmp/bench-preupload-$TAG.json \
  --offset 0 --count "$COUNT" --batch-id "$BATCH" \
  --radius "$RADIUS" --sigma 7.5 \
  --out /tmp/bench-manifest-$TAG.json

echo "==> import-batch ($COUNT entries)"
# Mount the manifest; the container writes the summary to stdout.
podman run --rm --network "$NET" \
  --cpus=4 --memory=6g \
  --user 0:0 \
  -v /tmp/bench-manifest-$TAG.json:/srv/manifest.json:ro \
  --tmpfs /data:size=8g \
  -e DATABASE_URL=postgres://bench:bench@bench-pg-$TAG:5432/bench \
  -e S3_ENDPOINT=file:///data \
  -e S3_BUCKET=bench \
  -e SESSION_SECRET=bench-secret-0123456789abcdef0123456789abcdef \
  idol-backend-worker:bench \
  import-batch /srv/manifest.json > /tmp/bench-summary-$TAG.json

python3 - <<'PYEOF'
import json
s = json.load(open("/tmp/bench-summary-$TAG.json"))
secs = max(s["duration_ms"] / 1000, 0.001)
print(f"imported={s['imported']} duplicates={s['duplicates']} chunk_writes={s['chunk_writes']}")
print(f"blocks={s['blocks']} components={s['components']} amp={s['chunk_write_amplification']}")
print(f"==> sustained: {round(s['imported'] / secs, 2)} img/s over {round(secs, 1)}s (SLO >= 10/s)")
PYEOF
