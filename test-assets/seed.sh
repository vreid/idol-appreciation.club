#!/usr/bin/env bash
# Seed a deployed idol-appreciation.club canvas with the AI-generated test images.
#
# Usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD
#
# Layout (native px; world center "0/0" = 134217728,134217728):
#   Default (no SEED_COUNT): the classic 16-image layout — 01-solo centered
#   EXACTLY on the world center (center-holder leaderboard), 02..16 on a
#   4-column ring of 512px slots, non-overlapping.
#
#   SEED_COUNT=N (default 10000): scale-seed mode. N cells of a 100x100
#   grid (640px pitch, 512px slots, centered on 0/0) are filled CYCLICALLY
#   from the 100 unique images, every file always at the 512 long edge —
#   so every repeat produces byte-identical derivatives → same sha256 →
#   the server's content-addressed dedup keeps ~100 derivative objects
#   regardless of N. Cell (50,50) is reserved for 01-solo exactly on the
#   center: if /api/center already shows a placement covering 0/0 the
#   cell is skipped (holder preserved); otherwise 01-solo is placed first
#   from the seed account.
#
# Modes (env):
#   SEED_COUNT=N        scale-seed N grid cells (default unset = classic 16)
#   SEED_DRY_RUN=1      compute placements, verify non-overlap, post nothing
#   SEED_STATE_DIR=DIR  resume markers (default ./seed-state-<user>); on
#                       rerun, cells with a marker are skipped
#   SEED_ACCOUNTS_FILE  JSON [{"username":"..","password":".."},..] — extra
#                       fan-out accounts for the FAR cells (cooldown is
#                       per-account, so N accounts ≈ N× rate); near-center
#                       cells ALWAYS go to the seed account
#   SEED_SLEEP=S        floor sleep between posts (default 1.5; use 0.25
#                       when the seed account is cooldown-exempt)
#   SEED_NEAR_PX=8192   radius around 0/0 reserved for the seed account
#
# Robustness: resumable (marker per cell), aborts after >3 consecutive
# non-201s, logs every non-201 with cell coordinates.
#
# Requires: curl, python3 (image headers parsed directly).
set -euo pipefail

BASE_URL=${1:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}
SEED_USERNAME=${2:?usage: seed.sh BASE_URL SEED_USERNAME SEED_PASSWORD}
SEED_PASSWORD=${3:?usage: seed.sh BASE_URL SEED_PASSWORD}

SEED_DRY_RUN=${SEED_DRY_RUN:-0}
SEED_COUNT=${SEED_COUNT:-0}
SEED_STATE_DIR=${SEED_STATE_DIR:-./seed-state-${SEED_USERNAME}}
SEED_ACCOUNTS_FILE=${SEED_ACCOUNTS_FILE:-}
SEED_SLEEP=${SEED_SLEEP:-}
SEED_NEAR_PX=${SEED_NEAR_PX:-8192}
GRID=100
SLOT=512
GUTTER=128
PITCH=$((SLOT + GUTTER)) # 640
FAIL_LIMIT=3

IDOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/idols"
WORLD_CENTER=134217728
CX=$WORLD_CENTER
CY=$WORLD_CENTER

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

# scaled_size FILE -> "PWxPH" after the server scales the long edge to $SLOT.
scaled_size() {
  local d w h
  d=$(img_dims "$1"); w=${d%x*}; h=${d#*x}
  if [ "$w" -ge "$h" ]; then
    printf '%dx%d' "$SLOT" $(( SLOT * h / w ))
  else
    printf '%dx%d' $(( SLOT * w / h )) "$SLOT"
  fi
}

# --- accounts -----------------------------------------------------------------
# JARS[i] = cookie jar; ACC_USER[i]/ACC_PASS[i] = credentials. Index 0 is
# always the seed account.
ACC_USER=("$SEED_USERNAME"); ACC_PASS=("$SEED_PASSWORD")
JARS=("$(mktemp /tmp/idol-seed-jar.XXXXXX)")
JARS_CLEANUP=("${JARS[0]}")
FANOUT_N=0

if [ -n "$SEED_ACCOUNTS_FILE" ]; then
  if [ ! -f "$SEED_ACCOUNTS_FILE" ]; then
    echo "ERROR: SEED_ACCOUNTS_FILE=$SEED_ACCOUNTS_FILE not found" >&2; exit 1
  fi
  mapfile -t FANOUT < <(python3 - "$SEED_ACCOUNTS_FILE" <<'PYEOF'
import json, sys
for a in json.load(open(sys.argv[1])):
    print(a["username"], a["password"])
PYEOF
)
  FANOUT_N=${#FANOUT[@]}
  for ((i = 0; i < FANOUT_N; i++)); do
    u=${FANOUT[$i]% *}; p=${FANOUT[$i]#* }
    ACC_USER+=("$u"); ACC_PASS+=("$p")
    JARS+=("$(mktemp /tmp/idol-seed-jar.XXXXXX)")
    JARS_CLEANUP+=("${JARS[$((i + 1))]}")
  done
fi
trap 'rm -f "${JARS_CLEANUP[@]}"' EXIT

login_account() { # login_account INDEX -> 0 on success
  local i=$1 u=${ACC_USER[$1]} p=${ACC_PASS[$1]} jar=${JARS[$1]} code
  code=$(curl -sS -o /dev/null -w '%{http_code}' -c "$jar" -X POST \
    "${BASE_URL%/}/api/auth/register" -H 'Content-Type: application/json' \
    -d "{\"username\":\"${u}\",\"password\":\"${p}\"}") || return 1
  case $code in 2*|409) : ;; *) echo "  ! register $u HTTP $code" >&2; return 1 ;; esac
  code=$(curl -sS -o /dev/null -w '%{http_code}' -c "$jar" -X POST \
    "${BASE_URL%/}/api/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"${u}\",\"password\":\"${p}\"}") || return 1
  [ "$code" = "200" ] || { echo "  ! login $u HTTP $code (pending approval?)" >&2; return 1; }
  return 0
}

# --- helpers ------------------------------------------------------------------
place_cell() { # place_cell ACCOUNT_IDX FILE X Y CELL_TAG
  local ai=$1 file=$2 x=$3 y=$4 tag=$5
  local body code t tsum
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY  %-28s cell %s at (%d, %d)\n' "$(basename "$file")" "$tag" "$x" "$y"
    return 0
  fi
  t0=$(date +%s.%N)
  body=$(curl -sS -m 60 -b "${JARS[$ai]}" -X POST "${BASE_URL%/}/api/place" \
    -F "image=@${file}" -F "x=${x}" -F "y=${y}" -F "size=${SLOT}" \
    -w '\n%{http_code} %{time_total}') || {
    echo "NON-201 cell $tag: connection error" >&2; return 1; }
  t1=$(date +%s.%N)
  code_line=${body##*$'\n'}; body=${body%$'\n'*}
  code=${code_line%% *}; ttotal=${code_line#* }
  tsum=$(python3 -c "print(f'{$t1 - $t0:.2f}')")
  if [[ $code == 2* ]]; then
    LAT_SUM=$(python3 -c "print($LAT_SUM + $ttotal + 0)")
    printf 'OK  cell %s (%s) %.2fs\n' "$tag" "$(basename "$file")" "$tsum"
    mkdir -p "$SEED_STATE_DIR"
    : > "$SEED_STATE_DIR/cell-$tag.done"
    CONSEC_FAIL=0
    PLACED=$((PLACED + 1))
    return 0
  fi
  echo "NON-201 cell $tag: HTTP $code: $(echo "$body" | head -c 200)" >&2
  CONSEC_FAIL=$((CONSEC_FAIL + 1))
  FAILED=$((FAILED + 1))
  return 1
}

# --- modes --------------------------------------------------------------------
DRY_RUN=$SEED_DRY_RUN
PLACED=0; FAILED=0; CONSEC_FAIL=0; LAT_SUM=0
T0=$(date +%s)

if [ "$SEED_DRY_RUN" != "1" ]; then
  echo "==> Logging in seed account '${SEED_USERNAME}' at ${BASE_URL}"
  login_account 0 || { echo "ERROR: seed account login failed" >&2; exit 1; }
  for ((i = 1; i <= FANOUT_N; i++)); do
    login_account "$i" || { echo "ERROR: fanout account ${ACC_USER[$i]} login failed" >&2; exit 1; }
  done
  [ "$FANOUT_N" -gt 0 ] && echo "==> ${FANOUT_N} fan-out accounts logged in"
fi

F01=$(ls "$IDOLS_DIR"/01-*.* 2>/dev/null | head -1 || true)
[ -n "$F01" ] || { echo "ERROR: missing $IDOLS_DIR/01-*.*" >&2; exit 1; }
FILES=("$IDOLS_DIR"/*.jpg)
N_FILES=${#FILES[@]}
[ "$N_FILES" -ge 2 ] || { echo "ERROR: need >=2 images in $IDOLS_DIR" >&2; exit 1; }

if [ "$SEED_COUNT" -gt 0 ]; then
  # ---------------- scale-seed mode ----------------
  ORIGIN=$((CX - GRID * PITCH / 2))
  NEAR_CELLS=$(( (SEED_NEAR_PX + PITCH) / PITCH + 1 )) # cells of margin
  LO=$(( 50 - NEAR_CELLS )); HI=$(( 50 + NEAR_CELLS ))

  center_covered() {
    [ -f "$SEED_STATE_DIR/center.done" ] && return 0
    [ "$DRY_RUN" = "1" ] && return 1
    local c
    c=$(curl -sS -m 30 "${BASE_URL%/}/api/center") || return 1
    python3 - "$c" <<'PYEOF'
import json, sys
try:
    cur = json.loads(sys.argv[1])["current"]
    p = cur["placement"]
    cx = cy = 134217728
    print("1" if p["x"] <= cx < p["x"] + p["w"] and p["y"] <= cy < p["y"] + p["h"] else "0")
except Exception:
    print("0")
PYEOF
  }

  # Center cell (50,50): 01-solo exactly on 0/0 — only if not already covered.
  if center_covered | grep -q '^1$'; then
    echo "==> Center 0/0 already covered ($(curl -sS -m 30 "${BASE_URL%/}/api/center" | python3 -c 'import sys,json; print(json.load(sys.stdin)["current"]["username"])' 2>/dev/null || echo '?')) — preserving holder, skipping cell 50/50"
    mkdir -p "$SEED_STATE_DIR"; : > "$SEED_STATE_DIR/center.done"
  else
    echo "==> Center not covered — placing 01-solo on 0/0 from the seed account"
    dims=$(scaled_size "$F01"); pw=${dims%x*}; ph=${dims#*x}
    place_cell 0 "$F01" $(( CX - pw / 2 )) $(( CY - ph / 2 )) "center" || true
    [ "$CONSEC_FAIL" -lt "$FAIL_LIMIT" ] || { echo "ABORT: $FAIL_LIMIT consecutive failures" >&2; exit 1; }
  fi

  echo "==> SEED_COUNT=$SEED_COUNT on a ${GRID}x${GRID} grid (pitch ${PITCH}px), ${FANOUT_N} fan-out accounts"
  k=0
  for ((j = 0; j < GRID; j++)); do
    for ((i = 0; i < GRID; i++)); do
      [ "$i" -eq 50 ] && [ "$j" -eq 50 ] && continue        # center cell
      [ "$k" -lt "$SEED_COUNT" ] || break 2
      tag="${i}_${j}"
      [ -f "$SEED_STATE_DIR/cell-$tag.done" ] && { k=$((k + 1)); continue; }
      # cyclic file choice (never 01-solo: index 1..N_FILES-1)
      f=${FILES[1 + (k % (N_FILES - 1))]}
      dims=$(scaled_size "$f"); pw=${dims%x*}; ph=${dims#*x}
      x=$(( ORIGIN + i * PITCH + (PITCH - pw) / 2 ))
      y=$(( ORIGIN + j * PITCH + (PITCH - ph) / 2 ))
      # near-center cells always go to the seed account (index 0)
      if [ "$i" -ge "$LO" ] && [ "$i" -le "$HI" ] && [ "$j" -ge "$LO" ] && [ "$j" -le "$HI" ]; then
        ai=0
      elif [ "$FANOUT_N" -gt 0 ]; then
        ai=$(( 1 + k % FANOUT_N ))
      else
        ai=0
      fi
      place_cell "$ai" "$f" "$x" "$y" "$tag" || true # failures counted, not fatal (see abort below)
      [ "$CONSEC_FAIL" -lt "$FAIL_LIMIT" ] || {
        echo "ABORT: $FAIL_LIMIT consecutive non-201s (resume with the same command)" >&2; exit 1; }
      [ "$DRY_RUN" = "1" ] || sleep "$SEED_SLEEP"
      k=$((k + 1))
    done
  done
  echo "==> Done: placed $PLACED, failed $FAILED (SEED_COUNT mode)"
  exit 0
fi

# ---------------- classic 16-image mode ----------------
HALF=$((SLOT / 2))
STEP=$((HALF + GUTTER + SLOT))
JAR=${JARS[0]}

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

RECTS=()
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

center_place() {
  local file=$1 dims pw ph
  dims=$(scaled_size "$file"); pw=${dims%x*}; ph=${dims#*x}
  place "$file" $(( CX - pw / 2 )) $(( CY - ph / 2 ))
}

FILES16=()
for f in "$IDOLS_DIR"/0[2-9]-*.* "$IDOLS_DIR"/1[0-6]-*.*; do
  [ -e "$f" ] || continue
  FILES16+=("$f")
done
if [ "${#FILES16[@]}" -ne 15 ]; then
  echo "ERROR: expected 15 non-center images in $IDOLS_DIR (02..16), found ${#FILES16[@]}" >&2
  exit 1
fi
center_place "$F01"
ROWS=( -$((STEP + PITCH)) -$STEP $((HALF + GUTTER)) $((HALF + GUTTER + PITCH)) )
COLS=( -1216 -576 64 704 )
i=0
for ROW in "${ROWS[@]}"; do
  for COL in "${COLS[@]}"; do
    [ "$i" -lt "${#FILES16[@]}" ] || break
    place "${FILES16[$i]}" $(( CX + COL )) $(( CY + ROW ))
    i=$((i + 1))
  done
done

# pairwise non-overlap post-check (pure python3).
python3 - "${RECTS[@]}" <<'PYEOF'
import sys
rects = []
for a in sys.argv[1:]:
    x, y, w, h, name = a.split()
    rects.append((int(x), int(y), int(w), int(h), name))
bad = 0
for i in range(len(rects)):
    for j in range(i + 1, len(rects)):
        ax, ay, aw, ah, an = rects[i]
        bx, by, bw, bh, bn = rects[j]
        if ax < bx + bw and bx < ax + aw and ay < by + bh and by < ay + ah:
            print(f"OVERLAP: {an} <-> {bn}"); bad += 1
print(f"==> Checked {len(rects)} placements for pairwise overlap")
sys.exit(1 if bad else 0)
PYEOF
echo "==> OK: all placements pairwise non-overlapping"
