#!/usr/bin/env bash
# Seed a deployed idol-appreciation.club canvas with the AI-generated test images.
#
# Usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD
#
# Layout (native px; world center "0/0" = 134217728,134217728):
#   - 01-solo-female-closeup is centered EXACTLY on the world center so the
#     center-holder leaderboard is populated.
#   - The other 15 images fill a 4-column ring of 512px slots with 128px
#     gutters around the center image (2 rows above, 2 rows below), all slots
#     non-overlapping so every placement lands on empty space.
# Cooldown (300s) only applies to overlapping placements; empty-space
# placements need a ~1s gap, so we sleep 1.5s between posts.
#
# Requires: curl, python3 (no PIL needed - image headers are parsed directly).
set -euo pipefail

BASE_URL=${1:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}
SEED_USERNAME=${2:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}
SEED_PASSWORD=${3:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}

IDOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/idols"
WORLD_CENTER=134217728
SLOT=512          # long edge placed for every image (>= native long edge? no: images are 1k, server scales down)
GUTTER=128
PITCH=$((SLOT + GUTTER)) # 640

JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT

# img_dims FILE -> prints "WxH" parsed from PNG or JPEG headers (pure python3).
img_dims() {
  python3 - "$1" <<'PYEOF'
import struct, sys
data = open(sys.argv[1], 'rb').read(64 * 1024)
if data[:8] == b'\x89PNG\r\n\x1a\n':
    w, h = struct.unpack('>II', data[16:24]); print(f"{w}x{h}"); sys.exit(0)
if data[:2] == b'\xff\xd8':
    i = 2
    while i + 9 < len(data):
        if data[i] != 0xFF: i += 1; continue
        marker = data[i + 1]
        if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7: i += 2; continue
        seglen = struct.unpack('>H', data[i + 2:i + 4])[0]
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            h, w = struct.unpack('>HH', data[i + 5:i + 9]); print(f"{w}x{h}"); sys.exit(0)
        i += 2 + seglen
sys.exit(f"cannot parse dimensions of {sys.argv[1]}")
PYEOF
}

post() { # post URL curl-args... -> prints response body, fails hard on HTTP error
  local url=$1; shift
  local body code
  body=$(curl -sS -w '\n%{http_code}' "$@" "$url") || {
    echo "ERROR: request to $url failed (connection error - is BASE_URL correct?)" >&2; exit 1; }
  code=${body##*$'\n'}
  body=${body%$'\n'*}
  case $code in
    2*) printf '%s' "$body" ;;
    *)  echo "ERROR: $url returned HTTP $code: $body" >&2; exit 1 ;;
  esac
}

# --- auth: register (tolerate "already exists"), then login -------------------
echo "==> Registering seed account '${SEED_USERNAME}' at ${BASE_URL}"
REG_CODE=$(curl -sS -o /dev/null -w '%{http_code}' -c "$JAR" -X POST \
  "${BASE_URL%/}/api/auth/register" -H 'Content-Type: application/json' \
  -d "{\"username\":\"${SEED_USERNAME}\",\"password\":\"${SEED_PASSWORD}\"}") \
  || { echo "ERROR: cannot reach ${BASE_URL} (connection failed)" >&2; exit 1; }
case $REG_CODE in
  2*) echo "    registered (HTTP $REG_CODE)" ;;
  *)  echo "    register returned HTTP $REG_CODE (assuming account exists); logging in" ;;
esac

echo "==> Logging in"
post "${BASE_URL%/}/api/auth/login" -c "$JAR" -X POST \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"${SEED_USERNAME}\",\"password\":\"${SEED_PASSWORD}\"}" >/dev/null
echo "    login OK"

# --- placements ---------------------------------------------------------------
CX=$WORLD_CENTER; CY=$WORLD_CENTER
HALF=$((SLOT / 2))             # 256
STEP=$((HALF + GUTTER + SLOT)) # 256+128+512 = 896: top-left y of the nearest ring row above the center image

# category order (02..16) mapped to ring slots, row-major starting top-left.
# Ring pitch is 640px (512px slot + 128px gutter), centered on the world
# center. Row top-lefts (y offsets relative to center): -1536, -896, +384,
# +1024; the nearest rows sit 128px clear of the center image rect (the
# size-512 center image is 384x512, i.e. y -256..+256). Column top-lefts
# (x offsets relative to center): -1216, -576, +64, +704 (also 640px pitch).
# 16 slots, 15 used: bottom-right slot stays empty.
FILES=()
for f in "$IDOLS_DIR"/0[2-9]-*.* "$IDOLS_DIR"/1[0-6]-*.*; do
  [ -e "$f" ] || continue
  FILES+=("$f")
done
if [ "${#FILES[@]}" -ne 15 ]; then
  echo "ERROR: expected 15 non-center images in $IDOLS_DIR (02..16), found ${#FILES[@]}" >&2
  exit 1
fi

# 1) category 01 centered exactly on the world center
F01=$(ls "$IDOLS_DIR"/01-*.* 2>/dev/null | head -1 || true)
[ -n "$F01" ] || { echo "ERROR: missing $IDOLS_DIR/01-*.*" >&2; exit 1; }
DIMS=$(img_dims "$F01"); W=${DIMS%x*}; H=${DIMS#*x}
# server scales the long edge to $SLOT
if [ "$W" -ge "$H" ]; then PW=$SLOT; PH=$(( SLOT * H / W )); else PH=$SLOT; PW=$(( SLOT * W / H )); fi
X01=$(( CX - PW / 2 )); Y01=$(( CY - PH / 2 ))

place() { # place FILE X Y
  local file=$1 x=$2 y=$3 name
  name=$(basename "$file")
  printf '==> Placing %-28s at (%d, %d) size=%d ... ' "$name" "$x" "$y" "$SLOT"
  post "${BASE_URL%/}/api/place" -b "$JAR" -X POST \
    -F "image=@${file}" -F "x=${x}" -F "y=${y}" -F "size=${SLOT}" >/dev/null
  echo "OK (201)"
  sleep 1.5
}

place "$F01" "$X01" "$Y01"

# 2) ring slots for 02..16 (last slot unused)
ROWS=( -$((STEP + PITCH)) -$STEP $((HALF + GUTTER)) $((HALF + GUTTER + PITCH)) )
COLS=( -1216 -576 64 704 )
i=0
for ROW in "${ROWS[@]}"; do
  for COL in "${COLS[@]}"; do
    [ "$i" -lt "${#FILES[@]}" ] || break
    place "${FILES[$i]}" $(( CX + COL )) $(( CY + ROW ))
    i=$((i + 1))
  done
done

echo "==> Done: 16 images placed."
