#!/usr/bin/env bash
# End-to-end smoke test: builds nothing — expects a local Docker daemon.
#
# Boots postgres + minio + the backend image, then exercises:
#   register → place → overlapping place within cooldown → 429
#   z20 + z0 tile fetch (non-empty bytes, correct ETag)
#   /api/center current holder → SSE place event
#
# Usage: backend/scripts/smoke.sh [IMAGE]   (default idol-backend:smoke)
set -euo pipefail

IMAGE="${1:-idol-backend:smoke}"
NET=smoke-net
PG=smoke-pg
MINIO=smoke-minio
APP=smoke-backend
PORT=8080
BASE="http://127.0.0.1:${PORT}"
JAR="$(mktemp)"
PUB="$(mktemp)"
cleanup() {
    docker rm -f "$APP" "$MINIO" "$PG" >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
    rm -f "$JAR" "$PUB"
}
trap cleanup EXIT

pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; exit 1; }

docker network create "$NET" >/dev/null
docker run -d --name "$PG" --network "$NET" \
    -e POSTGRES_PASSWORD=postgres postgres:16-alpine >/dev/null

# Object storage: real S3-compatible (minio). If the image can't be pulled,
# fall back to the dev-only file:// store inside the backend container.
S3_ENDPOINT="http://${MINIO}:9000"
S3_ENV=()
if docker pull -q minio/minio >/dev/null 2>&1; then
    docker run -d --name "$MINIO" --network "$NET" \
        -e MINIO_ROOT_USER=idol -e MINIO_ROOT_PASSWORD=idolsecret \
        minio/minio server /data >/dev/null
    S3_ENV=(-e S3_ACCESS_KEY_ID=idol -e S3_SECRET_ACCESS_KEY=idolsecret)
    echo "using minio for object storage"
else
    S3_ENDPOINT="file:///data/s3"
    echo "minio unavailable; using dev file:// object store"
fi

for _ in $(seq 1 30); do
    if docker exec "$PG" pg_isready -U postgres >/dev/null 2>&1; then break; fi
    sleep 1
done
docker exec "$PG" pg_isready -U postgres >/dev/null || fail "postgres never became ready"

docker run -d --name "$APP" --network "$NET" -p "${PORT}:8080" \
    -e DATABASE_URL="postgres://postgres:postgres@${PG}:5432/postgres" \
    -e S3_ENDPOINT="$S3_ENDPOINT" -e S3_BUCKET=idol-smoke "${S3_ENV[@]}" \
    -e SESSION_SECRET=smoke-secret-smoke-secret-smoke-32 \
    -e PUBLIC_BASE_URL="http://localhost:${PORT}" \
    "$IMAGE" >/dev/null

for _ in $(seq 1 30); do
    if curl -fsS "$BASE/api/health" >/dev/null 2>&1; then break; fi
    sleep 1
done
curl -fsS "$BASE/api/health" >/dev/null || fail "backend /api/health never became ready"
pass "health"

# --- register ---------------------------------------------------------------
curl -fsS -c "$JAR" -H 'Content-Type: application/json' \
    -d '{"username":"smokeuser","password":"smokepassword1"}' \
    "$BASE/api/auth/register" > "$PUB"
grep -q '"is_admin":true' "$PUB" || fail "first account should be admin"
pass "register (first account is admin)"

# --- place two overlapping images -------------------------------------------
# 16x16 solid red PNG (deterministic bytes baked below).
RED16_B64="iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQbwj0AAAAJklEQVR4nGP8z4APMKGKly5dMjAwMDCUKlUqSBXx8XFjIyMjIwMA9eADWdCYIqIAAAAASUVORK5CYII="
CX=134217728
X=$((CX - 16)); Y=$((CX - 16))
printf '%s' "$RED16_B64" | base64 -d > /tmp/smoke-red16.png
# Vips-generated opaque 16x16 PNG: solid dark red. (Regenerated locally
# with: vips black t.png 16 16 && vips draw_rect t.png t.png 0 0 16 16 200 30 30)
curl -fsS -b "$JAR" -F "image=@/tmp/smoke-red16.png;type=image/png" \
    -F "x=$X" -F "y=$Y" -F "size=16" "$BASE/api/place" > "$PUB"
grep -q '"seq":1' "$PUB" || fail "first place should return seq 1: $(cat "$PUB")"
pass "place #1 accepted at world center"

HTTP=$(curl -s -o "$PUB" -w '%{http_code}' -b "$JAR" \
    -F "image=@/tmp/smoke-red16.png;type=image/png" \
    -F "x=$((X + 4))" -F "y=$Y" -F "size=16" "$BASE/api/place")
[ "$HTTP" = "429" ] || fail "overlapping place within cooldown should 429, got $HTTP"
RETRY=$(curl -s -D - -o /dev/null -b "$JAR" \
    -F "image=@/tmp/smoke-red16.png;type=image/png" \
    -F "x=$((X + 4))" -F "y=$Y" -F "size=16" "$BASE/api/place" \
    | tr -d '\r' | awk 'tolower($1)=="retry-after:"{print $2}')
[ -n "$RETRY" ] && [ "$RETRY" -ge 1 ] 2>/dev/null || fail "429 must carry Retry-After"
pass "overlapping place within cooldown → 429 (Retry-After: $RETRY)"

# --- tiles -------------------------------------------------------------------
TX=$((CX / 256)); TY=$TX
HDRS=$(mktemp)
curl -fsS -D "$HDRS" "$BASE/api/tiles/20/$TX/$TY.webp" -o /tmp/smoke-z20.webp
grep -qi '^content-type: image/webp' "$HDRS" || fail "tile content-type must be image/webp"
grep -qi '^etag: "1"' "$HDRS" || fail "z20 tile ETag must be \"1\""
[ -s /tmp/smoke-z20.webp ] || fail "z20 tile must be non-empty"
curl -fsS -D "$HDRS" "$BASE/api/tiles/0/0/0.webp" -o /tmp/smoke-z0.webp
grep -qi '^etag: "1"' "$HDRS" || fail "z0 tile ETag must be \"1\""
[ -s /tmp/smoke-z0.webp ] || fail "z0 tile must be non-empty"
pass "z20 + z0 tiles served (non-empty, ETag \"1\")"

# --- center ------------------------------------------------------------------
curl -fsS "$BASE/api/center" > "$PUB"
grep -q '"current":{"username":"smokeuser"' "$PUB" || fail "center holder wrong: $(cat "$PUB")"
pass "/api/center shows current holder"

# --- SSE ---------------------------------------------------------------------
# Second account may place immediately (per-account cooldown is fresh);
# the SSE stream must deliver the resulting place event.
curl -fsS -c "${JAR}2" -H 'Content-Type: application/json' \
    -d '{"username":"smokeuser2","password":"smokepassword2"}' \
    "$BASE/api/auth/register" > /dev/null
curl -sN --max-time 15 "$BASE/api/events" > "$PUB" &
SSE_PID=$!
sleep 1
HTTP=$(curl -s -o /dev/null -w '%{http_code}' -b "${JAR}2" \
    -F "image=@/tmp/smoke-red16.png;type=image/png" \
    -F "x=$X" -F "y=$Y" -F "size=16" "$BASE/api/place")
[ "$HTTP" = "201" ] || fail "second account place should 201, got $HTTP"
wait $SSE_PID || true
grep -q '"type":"place"' "$PUB" || fail "SSE stream missing place event: $(cat "$PUB")"
pass "SSE received place event"

echo
echo "ALL SMOKE CHECKS PASSED"
