#!/usr/bin/env bash
# Load-test wrapper: samples node/pod resources while loadtest.py runs,
# then prints them beside the verdict table. Requires the deploy
# kubeconfig (KUBECONFIG=/tmp/idol-deploy-kubeconfig) for the peaks.
# STAGED — run only after pipeline3's Phase 3 flip receipt.
set -euo pipefail
BASE=${1:?usage: loadtest-wrapper.sh BASE_URL ACCOUNTS_JSON}
ACCT=${2:?usage: loadtest-wrapper.sh BASE_URL ACCOUNTS_JSON}
OUT=/tmp/loadtest-$(date -u +%H%M%S)
mkdir -p "$OUT"

( while :; do
    date -u +%H:%M:%S >> "$OUT/nodes.txt"
    kubectl top nodes 2>/dev/null >> "$OUT/nodes.txt" || true
    kubectl top pods -n idol --sort-by=cpu 2>/dev/null >> "$OUT/pods.txt" || true
    sleep 10
  done ) &
SAMPLER=$!
trap 'kill $SAMPLER 2>/dev/null || true' EXIT

ulimit -n 16384 2>/dev/null || echo "WARN: ulimit -n failed; SSE hold will cap low" >&2
python3 "$(dirname "$0")/loadtest.py" "$BASE" --accounts "$ACCT" | tee "$OUT/verdict.txt"

echo "== resource peaks (see $OUT) =="
awk 'NR>1 {for (i=2;i<=NF;i++) if ($i ~ /%/) {gsub(/%/,"",$i); if ($i+0 > m) m=$i+0}} END {print "max node cpu%:", m}' "$OUT/nodes.txt" || true
tail -12 "$OUT/pods.txt" || true
