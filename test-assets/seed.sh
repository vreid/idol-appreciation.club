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
# Modes (env):
#   SEED_ALL=1      place ALL idols/*.jpg images: 01 centered on the world
#                   center, the rest centered in the cells of an 11x11 grid
#                   (512px cells, 128px gutters, 640px pitch) around it.
#   SEED_DRY_RUN=1  compute placements, verify pairwise non-overlap locally
#                   with a pure-python3 stub, and post nothing.
# Cooldown (300s) only applies to overlapping placements; empty-space
# placements need a ~1s gap, so we sleep 1.5s between posts.
#
# Requires: curl, python3 (no PIL needed - image headers are parsed directly);
# with SEED_DRY_RUN=1 only python3 is needed.
set -euo pipefail

BASE_URL=${1:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}
SEED_USERNAME=${2:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}
SEED_PASSWORD=${3:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}

SEED_ALL=${SEED_ALL:-0}       # 1 -> place every image on the full grid
DRY_RUN=${SEED_DRY_RUN:-0}    # 1 -> compute + verify placements only

IDOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/idols"
WORLD_CENTER=134217728
SLOT=512          # long edge placed for every image (>= native long edge? no: images are 1k, server scales down)
GUTTER=128
PITCH=$((SLOT + GUTTER)) # 640

JAR="$(mktemp)"
RECTS_FILE="$(mktemp)"
trap 'rm -f "$JAR" "$RECTS_FILE"' EXIT
RECTS=()

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
if [ "$DRY_RUN" = "1" ]; then
  echo "==> DRY RUN: computing placements only (no network requests)"
else

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

fi  # DRY_RUN

# --- placements ---------------------------------------------------------------
CX=$WORLD_CENTER; CY=$WORLD_CENTER
HALF=$((SLOT / 2))             # 256
STEP=$((HALF + GUTTER + SLOT)) # 256+128+512 = 896: top-left y of the nearest ring row above the center image

# scaled_size FILE -> prints "PWxPH" after the server scales the long edge to $SLOT
scaled_size() {
  local d w h
  d=$(img_dims "$1"); w=${d%x*}; h=${d#*x}
  if [ "$w" -ge "$h" ]; then
    printf '%dx%d' "$SLOT" $(( SLOT * h / w ))
  else
    printf '%dx%d' $(( SLOT * w / h )) "$SLOT"
  fi
}

place() { # place FILE X Y -> post (or just record in dry-run) + record the rect
  local file=$1 x=$2 y=$3 name dims pw ph
  name=$(basename "$file")
  dims=$(scaled_size "$file"); pw=${dims%x*}; ph=${dims#*x}
  RECTS+=("$x $y $pw $ph $name")
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY  %-28s at (%d, %d) size=%dx%d\n' "$name" "$x" "$y" "$pw" "$ph"
    return 0
  fi
  printf '==> Placing %-28s at (%d, %d) size=%d ... ' "$name" "$x" "$y" "$SLOT"
  post "${BASE_URL%/}/api/place" -b "$JAR" -X POST \
    -F "image=@${file}" -F "x=${x}" -F "y=${y}" -F "size=${SLOT}" >/dev/null
  echo "OK (201)"
  sleep 1.5
}

center_place() { # center_place FILE -> place FILE centered on the world center
  local file=$1 dims pw ph
  dims=$(scaled_size "$file"); pw=${dims%x*}; ph=${dims#*x}
  place "$file" $(( CX - pw / 2 )) $(( CY - ph / 2 ))
}

if [ "$SEED_ALL" = "1" ]; then
  # SEED_ALL: 01 centered exactly on the world center (center-holder
  # leaderboard), the other images centered in the cells of an 11x11 grid
  # around it. Grid pitch is 640px (512px cell + 128px gutter) so every
  # pair of placements is non-overlapping with >=128px clearance; the
  # middle cell (0,0) is left free for the center image.
  F01=$(ls "$IDOLS_DIR"/01-*.* 2>/dev/null | head -1 || true)
  [ -n "$F01" ] || { echo "ERROR: missing $IDOLS_DIR/01-*.*" >&2; exit 1; }
  FILES_ALL=()
  for f in "$IDOLS_DIR"/*.jpg; do
    [ -e "$f" ] || continue
    if [ "$f" != "$F01" ]; then FILES_ALL+=("$f"); fi
  done
  N=${#FILES_ALL[@]}
  echo "==> SEED_ALL: center image + ${N} images on an 11x11 grid (640px pitch)"
  center_place "$F01"
  i=0
  for ROW in -5 -4 -3 -2 -1 0 1 2 3 4 5; do
    for COL in -5 -4 -3 -2 -1 0 1 2 3 4 5; do
      if [ "$ROW" -eq 0 ] && [ "$COL" -eq 0 ]; then continue; fi
      [ "$i" -lt "$N" ] || break 2
      f="${FILES_ALL[$i]}"
      dims=$(scaled_size "$f"); pw=${dims%x*}; ph=${dims#*x}
      place "$f" $(( CX + COL * PITCH - pw / 2 )) $(( CY + ROW * PITCH - ph / 2 ))
      i=$((i + 1))
    done
  done
  echo "==> Done: $((N + 1)) images placed (SEED_ALL)."

else
  # Default 16-image layout: category order (02..16) mapped to ring slots,
  # row-major starting top-left.
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
  center_place "$F01"

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
fi

# --- verify: every computed placement rect is pairwise non-overlapping --------
if [ "${#RECTS[@]}" -eq 0 ]; then
  echo "ERROR: no placements were computed" >&2
  exit 1
fi
: > "$RECTS_FILE"
for r in "${RECTS[@]}"; do printf '%s\n' "$r" >> "$RECTS_FILE"; done
python3 - "$RECTS_FILE" <<'PYEOF'
import sys
rects = []
for line in open(sys.argv[1]):
    x, y, w, h, name = line.split(None, 4)
    rects.append((int(x), int(y), int(w), int(h), name.strip()))
bad = []
for i in range(len(rects)):
    for j in range(i + 1, len(rects)):
        a, b = rects[i], rects[j]
        if (a[0] < b[0] + b[2] and b[0] < a[0] + a[2]
                and a[1] < b[1] + b[3] and b[1] < a[1] + a[3]):
            bad.append((a[4], b[4]))
print(f"==> Checked {len(rects)} placements for pairwise overlap")
if bad:
    for n1, n2 in bad:
        print(f"OVERLAP: {n1} <-> {n2}", file=sys.stderr)
    sys.exit(f"{len(bad)} overlapping placement pairs")
print("==> OK: all placements pairwise non-overlapping")
PYEOF
