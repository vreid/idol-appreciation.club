#!/usr/bin/env bash
# Load test: dense-pile placements vs the tile render path (SWR admission
# control). Proves the pile-wedge fix: z0 serves STALE instantly while 700+
# placements land in one area, and placement latency stays at baseline.
#
# Boots a throwaway postgres (Docker) + the release backend with a file://
# store, seeds a dense pile, then hammers z0 concurrently with more
# placements. Prints per-phase latency stats; asserts the invariants.
#
# Usage: backend/scripts/load_tile_swr.sh [PLACEMENTS]   (default 700)
set -euo pipefail

N="${1:-700}"
NET=loadswd-net
PG=loadswd-pg
PORT=$((20000 + RANDOM % 20000))
BASE="http://127.0.0.1:${PORT}"
WORK="$(mktemp -d)"
BIN="$(mktemp -d)"
cleanup() {
    [[ -n "${APP_PID:-}" ]] && kill "$APP_PID" 2>/dev/null || true
    docker rm -f "$PG" >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
    rm -rf "$WORK" "$BIN"
}
trap cleanup EXIT

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; exit 1; }

cd "$(dirname "$0")/.."

# Use mise's pinned toolchain when present (repo rust is mise-managed).
CARGO="cargo"
if command -v mise >/dev/null 2>&1 && mise exec -- cargo --version >/dev/null 2>&1; then
    CARGO="mise exec -- cargo"
fi

docker network create "$NET" >/dev/null
PGPORT=$((21000 + RANDOM % 20000))
docker run -d --name "$PG" --network "$NET" -p "127.0.0.1:${PGPORT}:5432" \
    -e POSTGRES_PASSWORD=postgres postgres:16-alpine >/dev/null
for i in $(seq 1 30); do
    docker exec "$PG" pg_isready -U postgres >/dev/null 2>&1 && break
    sleep 0.5
done
docker exec "$PG" pg_isready -U postgres >/dev/null || fail "postgres not ready"

echo "building release backend..."
$CARGO build --release --quiet
cp target/release/backend "$BIN/backend"

DATABASE_URL="postgres://postgres:postgres@127.0.0.1:${PGPORT}/postgres" \
S3_ENDPOINT="file://${WORK}/s3" S3_BUCKET=load \
SESSION_SECRET=00000000000000000000000000000000abc \
LISTEN_ADDR="127.0.0.1:${PORT}" PUBLIC_BASE_URL="http://localhost:${PORT}" \
STATIC_DIR=/nonexistent RUST_LOG=warn \
    "$BIN/backend" >"$WORK/backend.log" 2>&1 &
APP_PID=$!
for i in $(seq 1 50); do
    curl -sf "$BASE/api/health" >/dev/null && break
    sleep 0.2
done
curl -sf "$BASE/api/health" >/dev/null || fail "backend not healthy"
pass "backend up on :${PORT}"

# Seed account with cooldown exemption (placement bursts without 429s).
curl -sf -X POST "$BASE/api/auth/register" -H 'Content-Type: application/json' \
    -d '{"username":"loader","password":"hunter2hunter2"}' >/dev/null
COOKIE=$(curl -sf -i -X POST "$BASE/api/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"loader","password":"hunter2hunter2"}' | grep -i '^set-cookie' | cut -d' ' -f2 | tr -d '\r')
curl -sf -X POST "$BASE/api/admin/accounts/loader/cooldown-exempt" \
    -H "Cookie: $COOKIE" -H 'Content-Type: application/json' -d '{"exempt":true}' >/dev/null

# A 32px red PNG via the seeder (one byte-identical upload → dedup).
IMG="$(mktemp)"
python3 - "$IMG" <<'EOF'
import struct, sys, zlib
w = h = 32
raw = b''.join(b'\x00' + bytes([200, 30, 30]) * w for _ in range(h))
def chunk(tag, data):
    c = tag + data
    return struct.pack('>I', len(data)) + c + struct.pack('>I', zlib.crc32(c))
png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) \
    + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b'')
open(sys.argv[1], 'wb').write(png)
EOF

place() { # x y -> "status latency" on stdout
    local x="$1" y="$2"
    curl -s --max-time 30 --connect-timeout 2 \
        -o /dev/null -w '%{http_code} %{time_total}\n' \
        -X POST "$BASE/api/place" -H "Cookie: $COOKIE" \
        -F "x=$x" -F "y=$y" -F "size=48" -F "image=@$IMG;type=image/png"
}

# Phase A: build the pile in one 512px area + warm z0.
CENTER=$((1 << 24))
echo "phase A: $N placements into one area + z0 warm..."
: >"$WORK/places_a.txt"
for i in $(seq 0 $((N - 1))); do
    x=$((CENTER + (i % 22) * 24))
    y=$((CENTER + (i / 22) * 24))
    place "$x" "$y" >>"$WORK/places_a.txt"
done
Z0="$BASE/api/tiles/0/0/0.webp"
Z0_ETAG_A=$(curl -sf -o /dev/null -w '%header{etag}' "$Z0")
pass "pile built; z0 warm at $Z0_ETAG_A"

# Phase B: hammer z0 while placing concurrently. z0 seq keeps bumping ⇒
# every GET should be a stale-serve (X-Tile-Stale) at ms latency, and
# placements should stay at phase-A latency. 4 continuous viewers ≈ real
# client load; unbounded curl fan-out would measure the client, not us.
echo "phase B: 4 z0 viewers for 25s while 120 more placements land..."
: >"$WORK/z0_b.txt"
: >"$WORK/places_b.txt"
z0_viewer() {
    local end=$((SECONDS + 25))
    while ((SECONDS < end)); do
        curl -s --max-time 5 --connect-timeout 2 \
            -o /dev/null -w '%{http_code} %{time_total}\n' "$Z0" >>"$WORK/z0_b.txt" || true
    done
}
VIEWER_PIDS=()
for _ in 1 2 3 4; do
    z0_viewer &
    VIEWER_PIDS+=($!)
done
for i in $(seq 0 119); do
    x=$((CENTER + 512 + (i % 12) * 24))
    y=$((CENTER + 512 + (i / 12) * 24))
    place "$x" "$y" >>"$WORK/places_b.txt"
done
# Bounded wait on the VIEWER pids only (the backend is a background job of
# this shell too — never touch it here); viewers end on their 25s budget.
for _ in $(seq 1 30); do
    kill -0 "${VIEWER_PIDS[0]}" 2>/dev/null || break
    sleep 1
done
kill "${VIEWER_PIDS[@]}" 2>/dev/null || true
wait "${VIEWER_PIDS[@]}" 2>/dev/null || true

stats() { # file label okstatus -> p50/p95/max in s
    awk -v lbl="$2" -v ok="$3" '
        $1 == ok { n++; v[n] = $2; if ($2 > max) max = $2 }
        END {
            if (n == 0) { printf "%s: n=0\n", lbl; exit 1 }
            asort(v);
            printf "%s: n=%d p50=%.3fs p95=%.3fs max=%.3fs\n", lbl, n,
                v[int(n * 0.5) < 1 ? 1 : int(n * 0.5)],
                v[int(n * 0.95) < 1 ? 1 : int(n * 0.95)], max
        }' "$1"
}
stats "$WORK/places_a.txt" "place latency  (phase A, pile building)" 201
stats "$WORK/places_b.txt" "place latency  (phase B, z0 hammered)" 201
stats "$WORK/z0_b.txt"     "z0 GET latency (phase B, concurrent)" 200

STALE=$(curl -sf -o /dev/null -w '%header{x-tile-stale}' "$Z0" || true)
M=$(curl -sf "$BASE/api/metrics")
STALE_N=$(awk '/^tile_requests_total\{outcome="stale_served"\} /{print $2}' <<<"$M")
BG_N=$(awk '/^tile_background_renders_total /{print $2}' <<<"$M")
DEPTH=$(awk '/^tile_render_queue_depth /{print $2}' <<<"$M")
echo "metrics: stale_served=$STALE_N background_renders=$BG_N queue_depth=$DEPTH (last z0 stale=$STALE)"

PLACE_MAX=$(awk '$1 == 201 { if ($2 > m) m = $2 } END { print m + 0 }' "$WORK/places_b.txt")
awk -v m="$PLACE_MAX" 'BEGIN { exit (m < 2.0) ? 0 : 1 }' \
    || fail "placement latency during z0 hammer exceeded 2s: ${PLACE_MAX}s"
pass "placements stayed sub-2s while z0 was hammered"

Z0_MAX=$(awk '$1 == 200 { if ($2 > m) m = $2 } END { print m + 0 }' "$WORK/z0_b.txt")
awk -v m="$Z0_MAX" 'BEGIN { exit (m < 1.0) ? 0 : 1 }' \
    || fail "z0 stale serve latency exceeded 1s: ${Z0_MAX}s"
pass "z0 served (stale) at ms latency under concurrent load"
