#!/usr/bin/env bash
# Bulk-import driver for the worker's import engine (docs/backend.md
# "Batch import"). Uploads the unique originals ONCE, generates
# deterministic scrapbook-pile manifests (same per-k rng discipline as
# seed.sh, via scripts/import-manifest.py), and drives
# `backend-worker import-batch` in client-side chunks with idempotent
# resume: every chunk's batch_id is derived from the run tag + chunk
# index (uuid5), so re-running the same command skips already-committed
# entries — a killed run resumes without double-placing.
#
# Usage:
#   import-bulk.sh [--count N] [--radius R] [--chunk C] [--tag TAG]
#                  [--prewarm] [--idols-dir DIR]
#
# Required env (exactly what the worker binary reads):
#   DATABASE_URL, S3_ENDPOINT, S3_BUCKET, S3_ACCESS_KEY_ID,
#   S3_SECRET_ACCESS_KEY, SESSION_SECRET
# Optional: WORKER_BIN (default: `cargo run -r -p iac-worker --bin
#   backend-worker --` run inside backend/)
#
#   count   total placements to import (default 10000)
#   chunk   entries per import-batch invocation (default 750). FIXED PER
#           RUN: chunk size feeds the entry ids, so changing --chunk
#           mid-run re-places the same coordinates — use a new --tag
#   tag     run identity for batch ids (default "bulk"; reuse to resume)
#   idols   unique sources (default test-assets/idols)
#
# Preuploads are cached: client (sha256,size) -> worker response map is
# kept in ${TMPDIR:-/tmp}/import-bulk-preupload-cache-<db>.json (keyed
# per database: a wiped/re-seeded backend loses the content-addressed
# objects, and a stale cache then 400s the import — delete it to force a
# full re-upload), so re-running (any tag) only uploads sources the
# worker has never seen. The engine dedupes by source sha anyway; the
# cache saves the bandwidth and the ~3.3s/source decode.
#
# Secrets are env only, and are never echoed. Prints one progress line
# per chunk plus a final sustained-rate summary. A chunk run is retried
# up to 4x (idempotent by batch_id) with backoff.
set -euo pipefail

COUNT=10000
CHUNK=750
TAG=bulk
RADIUS=100000
SIGMA=7.5
CX=134217728
CY=134217728
PREWARM=0
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IDOLS_DIR="$(cd "$SCRIPT_DIR/../test-assets/idols" && pwd)"

while [ $# -gt 0 ]; do
  case $1 in
    --count) COUNT=$2; shift 2 ;;
    --chunk) CHUNK=$2; shift 2 ;;
    --tag) TAG=$2; shift 2 ;;
    --radius) RADIUS=$2; shift 2 ;;
    --sigma) SIGMA=$2; shift 2 ;;
    --prewarm) PREWARM=1; shift ;;
    --idols-dir) IDOLS_DIR=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 1 ;;
  esac
done
[ "$CHUNK" -gt 0 ] || { echo "--chunk must be > 0" >&2; exit 1; }
: "${DATABASE_URL:?DATABASE_URL is required}"
: "${S3_ENDPOINT:?S3_ENDPOINT is required}"
: "${S3_BUCKET:?S3_BUCKET is required}"

# The worker runs the identical engine the former HTTP route ran, with
# the same env contract as the api binary.
worker() {
  if [ -n "${WORKER_BIN:-}" ]; then
    "$WORKER_BIN" "$@"
  else
    (cd "$SCRIPT_DIR/../backend" && cargo run -r -q -p iac-worker --bin backend-worker -- "$@")
  fi
}

TMP=$(mktemp -d /tmp/import-bulk.XXXXXX)
trap 'rm -rf "$TMP"' EXIT

FILES=("$IDOLS_DIR"/*.jpg)
N_FILES=${#FILES[@]}
[ "$N_FILES" -ge 2 ] || { echo "need >=2 idols in $IDOLS_DIR" >&2; exit 1; }

echo "==> preupload $N_FILES unique originals (skip-if-known cache)"
# map.json: [{"basename","source_sha","derivative_sha","w","h"},...]
# keyed client-side by the file's own sha256 — same file in, same map out
# (derivative shas are deterministic vips output), so a cache hit skips
# the upload entirely.
CACHE_KEY=$(printf '%s' "$DATABASE_URL" | sha256sum | cut -c1-8)
CACHE="${TMPDIR:-/tmp}/import-bulk-preupload-cache-${CACHE_KEY}.json"
if ! python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$CACHE" 2>/dev/null; then
  [ -e "$CACHE" ] && echo "    WARN unreadable cache at $CACHE — starting fresh" >&2
  echo '{}' > "$CACHE"
fi

unknown=()
for f in "${FILES[@]}"; do
  sha=$(sha256sum "$f" | awk '{print $1}')
  grep -q "\"$sha\"" "$CACHE" || unknown+=("$f")
done
echo "    known=$((${#FILES[@]} - ${#unknown[@]})) upload=${#unknown[@]}"

if [ "${#unknown[@]}" -gt 0 ]; then
  worker import-originals "${unknown[@]}" > "$TMP/preupload.json"
  python3 - "$TMP/preupload.json" "$CACHE" "${unknown[@]}" <<'PYEOF'
import json, sys, hashlib
raw = open(sys.argv[1]).read()
cache_path = sys.argv[2]
files = sys.argv[3:]
maps = json.loads(raw)
if len(files) != len(maps):
    sys.exit(f"mapping/file count mismatch: {len(maps)} maps for {len(files)} files; body: {raw[:400]}")
cache = json.load(open(cache_path))
for path, m in zip(files, maps):
    # The worker's source_sha is computed AFTER its normalization stage
    # (strip/resave), so it does NOT equal the client file's sha256 — the
    # cache is keyed by the CLIENT sha (deterministic per file bytes) and
    # only stores the server's response map. Correlation is positional:
    # import-originals returns maps in argument order.
    sha = hashlib.sha256(open(path, "rb").read()).hexdigest()
    cache[sha] = {"basename": path.rsplit("/", 1)[-1], **m}
json.dump(cache, open(cache_path + ".tmp", "w"))
import os
os.replace(cache_path + ".tmp", cache_path)
print(f"    uploaded {len(maps)} new uniques: " + ", ".join(m["source_sha"][:8] for m in maps[:3]) + " ...")
PYEOF
else
  echo "    all sources known — no upload"
fi

python3 - "$CACHE" "${FILES[@]}" "$TMP/map.json" <<'PYEOF'
import json, sys
cache, files, out = json.load(open(sys.argv[1])), sys.argv[2:-1], sys.argv[-1]
import hashlib
missing = [p for p in files if hashlib.sha256(open(p, "rb").read()).hexdigest() not in cache]
assert not missing, f"cache missing {len(missing)} sources after preupload"
json.dump(
    [cache[hashlib.sha256(open(p, "rb").read()).hexdigest()] for p in files],
    open(out, "w"),
)
PYEOF

# Deterministic batch id per (tag, chunk index).
gen_manifest() { # gen_manifest CHUNK_IDX ENTRY_COUNT
  python3 - "$1" "$2" "$TMP/map.json" "$TMP/m-$1.json" <<PYEOF
import sys, uuid, subprocess
idx = int(sys.argv[1])
batch = uuid.uuid5(uuid.NAMESPACE_URL, f"import-bulk:$TAG:{idx}")
cmd = [
    sys.executable, "$SCRIPT_DIR/import-manifest.py",
    "--map", sys.argv[3], "--offset", str(idx * $CHUNK),
    "--count", sys.argv[2], "--batch-id", str(batch),
    "--radius", "$RADIUS", "--sigma", "$SIGMA",
    "--cx", "$CX", "--cy", "$CY",
    "--out", sys.argv[4],
]
if "$PREWARM" == "1":
    cmd.append("--prewarm")
subprocess.run(cmd, check=True)
PYEOF
}

TOTAL_IMPORT=0; TOTAL_DUP=0; TOTAL_WRITES=0
T0=$(date +%s)
N_CHUNKS=$(( (COUNT + CHUNK - 1) / CHUNK ))
for ((c = 0; c < N_CHUNKS; c++)); do
  this_chunk=$CHUNK
  rem=$((COUNT - c * CHUNK))
  [ "$rem" -lt "$CHUNK" ] && this_chunk=$rem
  gen_manifest "$c" "$this_chunk"
  attempt=0
  while :; do
    if worker import-batch "$TMP/m-$c.json" > "$TMP/summary-$c.json" 2>"$TMP/err-$c.log"; then
      break
    fi
    echo "chunk $c: worker exited non-zero: $(tail -c 300 "$TMP/err-$c.log")" >&2
    attempt=$((attempt + 1)); [ $attempt -lt 4 ] || exit 1
    sleep $((attempt * 5)); continue
  done
  read -r imp dup wrt ms <<<"$(python3 -c '
import json, sys
s = json.load(open(sys.argv[1]))
print(s["imported"], s["duplicates"], s["chunk_writes"], s["duration_ms"])
' "$TMP/summary-$c.json")"
  TOTAL_IMPORT=$((TOTAL_IMPORT + imp)); TOTAL_DUP=$((TOTAL_DUP + dup)); TOTAL_WRITES=$((TOTAL_WRITES + wrt))
  now=$(date +%s)
  elapsed=$((now - T0))
  sustained=$(python3 -c "print(round($TOTAL_IMPORT / max($elapsed, 1), 2))")
  amp=$(python3 -c "print(round($TOTAL_IMPORT / max($TOTAL_WRITES, 1), 1))")
  rate=$(python3 -c "print(round($imp / max($ms / 1000, 0.001), 2))")
  echo "chunk $((c + 1))/$N_CHUNKS: imported=$imp dup=$dup writes=$wrt rate=${rate}/s sustained=${sustained}/s amp=${amp} (${elapsed}s)"
done

elapsed=$(($(date +%s) - T0))
echo "==> done: imported=$TOTAL_IMPORT duplicates=$TOTAL_DUP chunk_writes=$TOTAL_WRITES"
echo "==> sustained: $(python3 -c "print(round($TOTAL_IMPORT / max($elapsed, 1), 2))") img/s over ${elapsed}s (SLO >= 10/s)"
