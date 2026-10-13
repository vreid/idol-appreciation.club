# Test idol images (AI-generated, fictional)

200 AI-generated images of **fictional K-pop idols** for testing the image
pipeline and seeding the live canvas: the original 100 in `idols/` and a
100-image food-themed batch in `idols-food/`.

> **AI-generated fictional idols, no real persons.** Every prompt frames the
> subject as a fictional idol; no real-person likenesses, group names, logos, or
> trademarks are used. Banner text is rendered as star symbols, the album
> sleeves are deliberately generic ("no readable text or logos"), and shows are
> described generically ("music-show stage", "award-show stage").

## Contents

- `idols/01-*.jpg` ... `idols/100-*.jpg` — 100 images across 100 categories (see
  `manifest.json` for the exact category, aspect ratio, dimensions, byte size,
  and full prompt for each file). Coverage: solo portraits and photocards
  (pastel, spring/summer/autumn/winter, Halloween, Christmas), units and pairs,
  4-7 member girl-group stages, dance breaks, music-show wins and encores,
  variety poses, airport fashion, rehearsal and studio choreography, fan
  meetings and fan signs, birthday cafe displays, album flat lays, beauty
  close-ups (prism, water-light, glitter), photobook concepts (flower field,
  city night, hanbok, sporty, y2k, cyber), behind the scenes (jacket making, MV
  set, makeup room, stage wings), lightstick ocean, festival fireworks,
  camping/picnic vlogs, and more.
- `idols-food/01-*.jpg` ... `idols-food/100-*.jpg` — second batch of 100 images:
  fictional idols eating food in random places (street stalls, convenience
  stores, cafes, picnics, BBQ tables, practice rooms, camping trips,
  variety-show food challenges), same photocard/candid aesthetic. See
  `manifest.json` (entries prefixed `idols-food/`).
- `manifest.json` — index of all 200 images with `file`, `category`,
  `aspect_ratio`, `width`, `height`, `bytes`, `prompt`, `cost_if_known`.
- `generate.py` — stdlib-only regeneration script (see below).
- `seed.sh` — places the images on a deployed canvas (see below).

## Female focus

About 90% of the images depict **female** fictional idols: solo portraits,
photocards, 2-3 member units, and 4-7 member girl groups across stage,
photobook, beauty, and behind-the-scenes concepts. A handful of shots are male
or coed, or contain no people (album flat lays, cafe display, lightstick-ocean
crowd), kept for image-pipeline variety.

## Food batch (`idols-food/`)

The 100 food-themed images follow the same recipe: a fictional idol (or unit or
group) with a specific dish in a specific setting — ramyeon at a convenience
store, tteokbokki at a street stall, Korean BBQ with the group, bingsu in a
pastel cafe, camping stew at night, mukbang streams, variety-show spicy-noodle
and dalgona challenges, festival treats, birthday cake, hotel breakfasts,
train-window rice balls, and a confetti finale feast. Aspect mix: 29x 1:1, 33x
3:4, 30x 16:9, plus one 9:16 and two each of 4:3, 3:2, and 2:3. Four hero shots
(55, 78, 85, 100) requested the `2k` tier, which again billed and dimensioned
identically to `1k` (1672x2508 / 2508x1672 / 2730x1536). Three images (09,
24, 95) initially failed on two consecutive HTTP 503 `rate_limiter_unavailable`
responses and were regenerated successfully on a follow-up pass.

## Total spend

Generated with `bytedance/seedream-v5.0-pro` via the NanoGPT Image API at
`resolution: "1k"` with per-image `aspect_ratio`: non-square images cost $0.0855
each (3:4 -> 1773x2364, 16:9 -> 2730x1536) and square images cost $0.04275 each
(1:1 -> 1024x1024). Four hero shots (30, 59, 64, 65) requested the `2k` tier; on
this endpoint it is billed and dimensioned identically to `1k`. Aspect mix
across the set: 46x 16:9, 39x 3:4, 15x 1:1.

**Lifetime spend for the asset set: $9.23, under the $9.50 hard cap.** Account
balance went from $37.68722706 (fresh) to $28.45322706. The 16-image first run
cost $1.2825; the 84-image expansion was recorded at $7.9087 in `manifest.json`
`cost_if_known` sums ($7.9515 balance delta, which also covers
superseded/reattempted requests that the per-category manifest does not
double-count).

Note: the expansion re-attempt of `07-rehearsal` was skipped after two
consecutive HTTP 503s ("rate_limiter_unavailable"); `07-rehearsal.jpg` on disk
is the original first-run generation and remains valid.

**Food batch spend: $7.3529 actual** ($7.3102 tracked in `manifest.json`
`cost_if_known` plus $0.0427 for a probe request whose bytes were lost to a
tooling bug before the output directory existed — fixed by adding a `mkdir -p`
guard). Account balance went from $26.92761564 to $19.57461564 across the whole
food run. Pricing at generation time: $0.0855 per non-square `1k` image and
$0.04275 per square `1:1` image; `2k` billed identically.

## How they were made

1. Model metadata was discovered with
   `GET /api/v1/images/models/{model}/endpoints`: this model accepts
   `resolution` values `1:1 16:9 9:16 3:2 2:3 4:3 3:4 1k 2k` (no 4k) and up to 4
   output images. `POST /api/v1/images` with JSON
   `{model, prompt, n:1, resolution, aspect_ratio}` and the `x-api-key` header.
   Responses return `created`, `data` (`b64_json`), `cost`, `paymentSource`,
   `remainingBalance`, and `requestId`.
2. `generate.py` decodes each `b64_json`, verifies it decodes as a JPEG (header
   walk to SOF + non-trivial size > 30 KB), and writes it to `idols/` (or
   `idols-food/` with `--batch food`). Requests run sequentially with a small
   gap; HTTP 5xx/429 get a single retry, then the category is skipped and
   reported. A budget guard halts generation before the per-batch cap ($9.50
   lifetime for `idols/`, $12 runaway guard for a food-batch run), and the
   account balance is printed before and after each run
   (`POST /api/check-balance`). The API key is read from `.env` or the
   `NANO_GPT_API_KEY` environment variable.

## Regenerating

```sh
cd test-assets
python3 generate.py             # any missing categories (skips ones
                                # already in manifest.json)
python3 generate.py --only 05   # a single category, by number
python3 generate.py --batch food   # the idols-food/ batch (same skip logic)
```

Requires `NANO_GPT_API_KEY` in the project `.env`. Each new image costs
$0.04-$0.09 and is appended to `manifest.json`, which also tracks spend.

## Seeding a deployed canvas

`seed.sh` registers/logs in a seed account, then POSTs images to `/api/place` in
non-overlapping layouts so every placement lands on empty space (empty-space
placements need only a ~1 s gap; the script sleeps 1.5 s between posts).

- Default: the original 16-image layout — `01-solo-female-closeup` centered
  exactly on the world center `0/0` (populates the center-holder leaderboard)
  and the other 15 in a 4-column ring of 512 px slots with 128 px gutters.
- `SEED_ALL=1`: all 100 images — image 01 stays centered on the world center and
  the other 99 fill an 11x11 grid of 512 px cells with 128 px gutters (640 px
  pitch) around it, each image centered in its cell.
- `SEED_DRY_RUN=1`: computes placements and verifies pairwise non-overlap
  locally with a pure-python3 rectangle stub; nothing is posted (curl is not
  needed). The overlap check runs after real placements too.

```sh
./seed.sh https://canvas.example.com seedbot 's3cret-password'   # 16 images
SEED_ALL=1 ./seed.sh https://canvas.example.com seedbot 'pass'   # all 100
SEED_ALL=1 SEED_DRY_RUN=1 ./seed.sh http://unused.invalid/ u p   # dry run
```

A dry run ends with e.g. `==> Checked 100 placements for pairwise overlap` /
`==> OK: all placements pairwise non-overlapping`, and exits non-zero if any
pair overlaps. Requires `curl` and `python3` for real runs (dry run: python3
only). Exits non-zero with a clear message if the base URL is unreachable or any
placement returns a non-2xx status.
