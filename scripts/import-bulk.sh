#!/usr/bin/env bash
# Bulk-import driver for POST /api/admin/import (docs/backend.md "Batch
# import"). Uploads the unique originals ONCE, generates deterministic
# scrapbook-pile manifests (same per-k rng discipline as seed.sh, via
# scripts/import-manifest.py), and drives the endpoint in client-side
# chunks with idempotent resume: every chunk's batch_id is derived from
# the run tag + chunk index (uuid5), so re-running the same command skips
# already-committed entries — a killed run resumes without double-placing.
#
# Usage:
#   import-bulk.sh BASE_URL ADMIN_USER ADMIN_PASS [--count N] [--radius R]
#                  [--chunk C] [--tag TAG] [--prewarm] [--idols-dir DIR]
#
#   count   total placements to import (default 10000)
#   chunk   entries per POST (default 10000; ~17 min at the 10 img/s SLO)
#   tag     run identity for batch ids (default "bulk"; reuse to resume)
#   idols   unique sources (default test-assets/idols)
#
# Credentials are arguments/env only and are never echoed. Prints one
# progress line per chunk plus a final sustained-rate summary.
set -euo pipefail

BASE_URL=${1:?usage: import-bulk.sh BASE_URL ADMIN_USER ADMIN_PASS [opts]}
ADMIN_USER=${2:?missing admin user}
ADMIN_PASS=${3:?missing admin password}
shift 3

COUNT=10000
CHUNK=10000
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

TMP=$(mktemp -d /tmp/import-bulk.XXXXXX)
trap 'rm -rf "$TMP"' EXIT
JAR="$TMP/jar"

FILES=("$IDOLS_DIR"/*.jpg)
N_FILES=${#FILES[@]}
[ "$N_FILES" -ge 2 ] || { echo "need >=2 idols in $IDOLS_DIR" >&2; exit 1; }

api() { local url=$1; shift; curl -sS -m 1800 -b "$JAR" "${BASE_URL%/}/${url}" "$@"; }

echo "==> login $ADMIN_USER at $BASE_URL"
code=$(curl -sS -o /dev/null -w '%{http_code}' -c "$JAR" -X POST \
  "${BASE_URL%/}/api/auth/register" -H 'Content-Type: application/json' \
  -d "{\"username\":\"${ADMIN_USER}\",\"password\":\"${ADMIN_PASS}\"}") || true
case $code in 2*|409|000) : ;; *) echo "register -> HTTP $code" >&2; exit 1 ;; esac
code=$(curl -sS -o /dev/null -w '%{http_code}' -c "$JAR" -X POST \
  "${BASE_URL%/}/api/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"${ADMIN_USER}\",\"password\":\"${ADMIN_PASS}\"}")
[ "$code" = "200" ] || { echo "login -> HTTP $code" >&2; exit 1; }

echo "==> preupload $N_FILES unique originals"
# map.json: [{"basename","source_sha","derivative_sha","w","h"},...]
curl_args=()
for f in "${FILES[@]}"; do curl_args+=(-F "image=@$f"); done
api "api/admin/import/originals" -X POST "${curl_args[@]}" > "$TMP/preupload.json" || true
python3 - "$TMP/preupload.json" "${FILES[@]##*/}" "$TMP/map.json" <<'PYEOF'
import json, sys
raw = open(sys.argv[1]).read()
maps = json.loads(raw)
names, out = sys.argv[2:-1], sys.argv[-1]
if len(names) != len(maps):
    sys.exit(f"mapping/file count mismatch: {len(maps)} maps for {len(names)} files; body: {raw[:400]}")
json.dump([{"basename": n, **m} for n, m in zip(names, maps)], open(out, "w"))
print(f"    {len(maps)} uniques: " + ", ".join(m["source_sha"][:8] for m in maps[:3]) + " ...")
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
    resp=$(curl -sS -m 1800 -b "$JAR" -X POST "${BASE_URL%/}/api/admin/import" \
      -H 'Content-Type: application/json' \
      --data-binary @"$TMP/m-$c.json" -w '\n%{http_code}') || {
      echo "chunk $c: connection error" >&2; attempt=$((attempt + 1)); [ $attempt -lt 4 ] || exit 1; continue; }
    code=${resp##*$'\n'}; body=${resp%$'\n'*}
    if [ "$code" != "200" ]; then
      echo "chunk $c: HTTP $code: $(echo "$body" | head -c 300)" >&2
      attempt=$((attempt + 1)); [ $attempt -lt 4 ] || exit 1
      sleep $((attempt * 2)); continue
    fi
    break
  done
  read -r imp dup wrt ms rate <<<"$(printf '%s' "$body" | python3 -c '
import json, sys
s = json.load(sys.stdin)
print(s["imported"], s["duplicates"], s["chunk_writes"], s["duration_ms"], round(s["images_per_second"], 2))
')"
  TOTAL_IMPORT=$((TOTAL_IMPORT + imp)); TOTAL_DUP=$((TOTAL_DUP + dup)); TOTAL_WRITES=$((TOTAL_WRITES + wrt))
  now=$(date +%s)
  elapsed=$((now - T0))
  sustained=$(python3 -c "print(round($TOTAL_IMPORT / max($elapsed, 1), 2))")
  amp=$(python3 -c "print(round($TOTAL_IMPORT / max($TOTAL_WRITES, 1), 1))")
  echo "chunk $((c + 1))/$N_CHUNKS: imported=$imp dup=$dup writes=$wrt rate=${rate}/s sustained=${sustained}/s amp=${amp} (${elapsed}s)"
done

elapsed=$(($(date +%s) - T0))
echo "==> done: imported=$TOTAL_IMPORT duplicates=$TOTAL_DUP chunk_writes=$TOTAL_WRITES"
echo "==> sustained: $(python3 -c "print(round($TOTAL_IMPORT / max($elapsed, 1), 2))") img/s over ${elapsed}s (SLO >= 10/s)"
