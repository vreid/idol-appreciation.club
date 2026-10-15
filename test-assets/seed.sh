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
#   SEED_COUNT=N (default 10000): scrapbook-pile mode. N placements are
#   scattered deterministically (per cell index) over a disc of
#   SEED_RADIUS native px around 0/0, at each image's ORIGINAL
#   dimensions (server cap 4096 long edge applies), each rotated by a
#   truncated-gaussian jitter (sigma SEED_ROT_SIGMA, clamped ±15°).
#   Deliberate overlaps — the pile is the point. Repeats of the same
#   file are byte-identical derivatives → same sha256 → the server's
#   content-addressed dedup keeps ~100 derivative objects regardless of
#   N. The center image stays exactly on 0/0 (rot 0): if /api/center
#   already shows a placement covering 0/0 it is skipped (holder
#   preserved), otherwise 01-solo is placed first from the seed account.
#
# Modes (env):
#   SEED_COUNT=N        scrapbook-pile N placements (default unset = classic 16)
#   SEED_RADIUS=R       pile disc radius in native px (default 100000)
#   SEED_ROT_SIGMA=S    rotation jitter stddev in degrees (default 7.5, clamp ±15)
#   SEED_DRY_RUN=1      compute placements, verify non-overlap, post nothing
#   SEED_STATE_DIR=DIR  resume markers (default ./seed-state-<user>); on
#                       rerun, cells with a marker are skipped
#   SEED_ACCOUNTS_FILE  JSON [{"username":"..","password":".."},..] — extra
#                       fan-out accounts for the FAR cells (cooldown is
#                       per-account, so N accounts ≈ N× rate); near-center
#                       cells ALWAYS go to the seed account
#   SEED_SLEEP=S        floor sleep between posts (default 0 — every seed
#                       account is flagged cooldown-exempt via the admin
#                       endpoint before placing; set >0 to throttle)
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
SEED_SLEEP=${SEED_SLEEP:-0}
SEED_RADIUS=${SEED_RADIUS:-100000}
SEED_ROT_SIGMA=${SEED_ROT_SIGMA:-7.5}
GRID=100
NAT=1024   # natural long edge of every pool image (idols + idols-food)
GUTTER=128
PITCH=$((NAT + GUTTER)) # 1152 — classic-mode ring spacing at full size
FAIL_LIMIT=3

IDOLS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/idols"
FOOD_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/idols-food"
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

# original_size FILE -> "PWxPH" at the image's ORIGINAL dimensions, long
# edge capped to the server's 4096.
original_size() {
  local d w h long
  d=$(img_dims "$1"); w=${d%x*}; h=${d#*x}
  long=$([[ $w -ge $h ]] && echo "$w" || echo "$h")
  if [ "$long" -gt 4096 ]; then
    if [ "$w" -ge "$h" ]; then
      printf '4096x%d\n' $(( 4096 * h / w ))
    else
      printf '%dx4096\n' $(( 4096 * w / h ))
    fi
  else
    printf '%dx%d\n' "$w" "$h"
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
  local attempt
  # Auth endpoints sit behind the edge burst limiter: retry 429s with a
  # short backoff, stagger accounts so a fresh fleet logs in cleanly.
  for attempt in 1 2 3 4; do
    code=$(curl -sS -o /dev/null -w '%{http_code}' -c "$jar" -X POST \
      "${BASE_URL%/}/api/auth/register" -H 'Content-Type: application/json' \
      -d "{\"username\":\"${u}\",\"password\":\"${p}\"}") || return 1
    [ "$code" = "429" ] || break
    sleep $((attempt * 2))
  done
  case $code in 2*|409) : ;; *) echo "  ! register $u HTTP $code" >&2; return 1 ;; esac
  for attempt in 1 2 3 4; do
    code=$(curl -sS -o /dev/null -w '%{http_code}' -c "$jar" -X POST \
      "${BASE_URL%/}/api/auth/login" -H 'Content-Type: application/json' \
      -d "{\"username\":\"${u}\",\"password\":\"${p}\"}") || return 1
    [ "$code" = "429" ] || break
    sleep $((attempt * 2))
  done
  [ "$code" = "200" ] || { echo "  ! login $u HTTP $code (pending approval?)" >&2; return 1; }
  sleep 0.3
  return 0
}

# --- helpers ------------------------------------------------------------------
place_cell() { # place_cell ACCOUNT_IDX FILE X Y CELL_TAG [ROT] [SIZE]
  local ai=$1 file=$2 x=$3 y=$4 tag=$5 rot=${6:-0} size=${7:-}
  local body code t tsum
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY  %-28s cell %s at (%d, %d)\n' "$(basename "$file")" "$tag" "$x" "$y"
    return 0
  fi
  t0=$(date +%s.%N)
  body=$(curl -sS -m 60 -b "${JARS[$ai]}" -X POST "${BASE_URL%/}/api/place" \
    -F "image=@${file}" -F "x=${x}" -F "y=${y}" -F "size=${size}" -F "rot=${rot}" \
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

  # Flag every seed account cooldown-exempt (admin endpoint, #37): the
  # seeder places at full speed, no cooldown floors. Seed account (index 0)
  # holds the admin session; non-admin → warn once, cooldowns apply.
  EXEMPT_OK=0; EXEMPT_FAIL=0
  for ((i = 0; i <= FANOUT_N; i++)); do
    u=${ACC_USER[$i]}
    code=$(curl -sS -o /dev/null -w '%{http_code}' -b "${JARS[0]}" \
      -X POST "${BASE_URL%/}/api/admin/accounts/${u}/cooldown-exempt" \
      -H 'Content-Type: application/json' -d '{"exempt":true}') || code=000
    if [ "$code" = "204" ]; then
      EXEMPT_OK=$((EXEMPT_OK + 1))
    else
      EXEMPT_FAIL=$((EXEMPT_FAIL + 1))
      echo "  ! cooldown-exempt ${u}: HTTP $code (admin session required — set SEED_SLEEP>0)" >&2
    fi
  done
  echo "==> cooldown-exempt: ${EXEMPT_OK} flagged, ${EXEMPT_FAIL} failed"
fi

F01=$(ls "$IDOLS_DIR"/01-*.* 2>/dev/null | head -1 || true)
[ -n "$F01" ] || { echo "ERROR: missing $IDOLS_DIR/01-*.*" >&2; exit 1; }
# Pool = stage idols + food idols (manifest categories), one flat cycle.
FILES=("$IDOLS_DIR"/*.jpg "$FOOD_DIR"/*.jpg)
N_FILES=${#FILES[@]}
[ "$N_FILES" -ge 2 ] || { echo "ERROR: need >=2 images in $IDOLS_DIR/$FOOD_DIR" >&2; exit 1; }

if [ "$SEED_COUNT" -gt 0 ]; then
  # ---------------- scrapbook pile mode ----------------
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
    dims=$(original_size "$F01"); pw=${dims%x*}; ph=${dims#*x}
    place_cell 0 "$F01" $(( CX - pw / 2 )) $(( CY - ph / 2 )) "center" 0 "$pw$([ "$ph" -gt "$pw" ] && echo "$ph" || echo "$pw")" || true
    [ "$CONSEC_FAIL" -lt "$FAIL_LIMIT" ] || { echo "ABORT: $FAIL_LIMIT consecutive failures" >&2; exit 1; }
  fi

  echo "==> SEED_COUNT=$SEED_COUNT scrapbook pile (radius ${SEED_RADIUS}px, rot sigma ${SEED_ROT_SIGMA}°)"
  k=0
  while [ "$k" -lt "$SEED_COUNT" ]; do
    tag="$k"
    if [ -f "$SEED_STATE_DIR/cell-$tag.done" ]; then k=$((k + 1)); continue; fi
    # cyclic file choice (never 01-solo: index 1..N_FILES-1)
    f=${FILES[1 + (k % (N_FILES - 1))]}
    # deterministic geometry per (k, file): uniform point in the disc,
    # image centered on it, truncated-gaussian rot jitter.
    read -r x y rot <<GEOEOF
$(python3 - "$k" "$f" "$SEED_RADIUS" "$SEED_ROT_SIGMA" "$CX" "$CY" "$(original_size "$f")" <<'PYEOF'
import math, random, sys
k, f = sys.argv[1], sys.argv[2]
R, sigma, CX, CY = float(sys.argv[3]), float(sys.argv[4]), int(sys.argv[5]), int(sys.argv[6])
dims = sys.argv[7]
pw, ph = (int(v) for v in dims.split("x"))
rng = random.Random(f"pile:{k}:{f}")
r = R * math.sqrt(rng.random())
theta = 2 * math.pi * rng.random()
x = int(CX + r * math.cos(theta) - pw / 2)
y = int(CY + r * math.sin(theta) - ph / 2)
rot = max(-15.0, min(15.0, rng.gauss(0.0, sigma)))
print(x, y, f"{rot:.2f}")
PYEOF
)
GEOEOF
    dims=$(original_size "$f"); pw=${dims%x*}; ph=${dims#*x}
    size=$pw
    [ "$ph" -gt "$size" ] && size=$ph
    place_cell 0 "$f" "$x" "$y" "$tag" "$rot" "$size" || true # failures counted, not fatal
    [ "$CONSEC_FAIL" -lt "$FAIL_LIMIT" ] || {
      echo "ABORT: $FAIL_LIMIT consecutive non-201s (resume with the same command)" >&2; exit 1; }
    if [ "$DRY_RUN" != "1" ] && [ "$SEED_SLEEP" != "0" ]; then sleep "$SEED_SLEEP"; fi
    k=$((k + 1))
  done
  echo "==> Done: placed $PLACED, failed $FAILED (SEED_COUNT mode)"
  exit 0
fi

# ---------------- classic 16-image mode ----------------
# Full-size rects with mixed aspects: layout is a dynamic 4-column shelf
# (row height = tallest image in the row + GUTTER), anchored on the
# world center.
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
  local file=$1 x=$2 y=$3 name dims pw ph size
  name=$(basename "$file")
  dims=$(original_size "$file"); pw=${dims%x*}; ph=${dims#*x}
  size=$pw; [ "$ph" -gt "$size" ] && size=$ph
  RECTS+=("$x $y $pw $ph $name")
  if [ "$DRY_RUN" = "1" ]; then
    printf 'DRY  %-28s at (%d, %d) size=%dx%d\n' "$name" "$x" "$y" "$pw" "$ph"
    return 0
  fi
  printf '==> Placing %-28s at (%d, %d) size=%d ... ' "$name" "$x" "$y" "$size"
  post "${BASE_URL%/}/api/place" -b "$JAR" -X POST \
    -F "image=@${file}" -F "x=${x}" -F "y=${y}" -F "size=${size}" >/dev/null
  echo "OK (201)"
  if [ "$SEED_SLEEP" != "0" ]; then sleep "$SEED_SLEEP"; fi
}

center_place() {
  local file=$1 dims pw ph
  dims=$(original_size "$file"); pw=${dims%x*}; ph=${dims#*x}
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
# Shelf layout: 4 columns, rows sized to their tallest member; dims are
# parsed in-process (PNG/JPEG headers, same logic as img_dims).
mapfile -t SLOTS < <(python3 - "$GUTTER" "${FILES16[@]}" <<'PYEOF'
import struct, sys

gutter = int(sys.argv[1])
files = sys.argv[2:]

def dims(path):
    data = open(path, 'rb').read(64 * 1024)
    if data[:8] == b'\x89PNG\r\n\x1a\n':
        w, h = struct.unpack('>II', data[16:24])
        return w, h
    i = 2
    while i + 9 < len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
            i += 2
            continue
        seglen = struct.unpack('>H', data[i + 2:i + 4])[0]
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            h, w = struct.unpack('>HH', data[i + 5:i + 9])
            return w, h
        i += 2 + seglen
    raise SystemExit(f"cannot parse dimensions of {path}")

items = []
for f in files:
    w, h = dims(f)
    if max(w, h) > 4096:
        if w >= h:
            w, h = 4096, 4096 * h // w
        else:
            w, h = 4096 * w // h, 4096
    items.append((f, w, h))

COLS = 4
rows = [items[i:i + COLS] for i in range(0, len(items), COLS)]
col_w = [max(w for _, w, _ in (rows[ri][ci] for ri in range(len(rows)) if ci < len(rows[ri]))) for ci in range(COLS)]
row_h = [max(h for _, _, h in r) for r in rows]
total_w = sum(col_w) + gutter * (COLS - 1)
x0 = -total_w // 2
# Shelf sits wholly BELOW the center image (01-solo occupies 0/0).
y0 = 2364 // 2 + gutter  # center image half-height + gutter
xs = []
cur = x0
for c in range(COLS):
    xs.append(cur)
    cur += col_w[c] + gutter
for ri, r in enumerate(rows):
    y = y0 + sum(row_h[:ri]) + (gutter * ri)
    for ci, (f, w, h) in enumerate(r):
        # center each image inside its shelf cell
        print(f, xs[ci] + (col_w[ci] - w) // 2, y + (row_h[ri] - h) // 2)
PYEOF
)
i=0
for ROW in "${SLOTS[@]}"; do
  read -r f x y <<< "$ROW"
  [ -n "${f:-}" ] || continue
  place "$f" $(( CX + x )) $(( CY + y ))
  i=$((i + 1))
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
