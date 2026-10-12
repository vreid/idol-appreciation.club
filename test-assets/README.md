# Test idol images (AI-generated, fictional)

16 AI-generated images of **fictional K-pop idols** for testing the image
pipeline and seeding the live canvas.

> **AI-generated fictional idols, no real persons.** Every prompt frames the
> subject as a fictional idol; no real-person likenesses, group names, logos, or
> trademarks are used. The birthday-cafe banners use a star symbol instead of
> any name, and the album flat-lay design is deliberately generic.

## Contents

- `idols/01-*.jpg` ... `idols/16-*.jpg` — one image per category (see
  `manifest.json` for the exact category, aspect ratio, dimensions, byte size,
  and full prompt for each file).
- `manifest.json` — index of all images with `file`, `category`, `aspect_ratio`,
  `width`, `height`, `bytes`, `prompt`, `cost_if_known`.
- `generate.py` — stdlib-only regeneration script (see below).
- `seed.sh` — places the images on a deployed canvas (see below).

## Total spend

Generated with `bytedance/seedream-v5.0-pro` via the NanoGPT Image API at
`resolution: "1k"` with per-image `aspect_ratio`: non-square images cost $0.0855
each (3:4 -> 1773x2364, 16:9 -> 2730x1536) and square images cost $0.04275 each
(1:1 -> 1024x1024), as reported per request in `manifest.json`.

**Total spend: $1.2825** — $1.1970 for the 16 recorded generations plus $0.0855
for one earlier probe request used to confirm parameter support. Account balance
went from $37.68722706 to $36.40472706. Well under the $8 target / $10 hard cap.

## How they were made

1. Model metadata was discovered with
   `GET /api/v1/images/models/{model}/endpoints`: this model accepts
   `resolution` values `1:1 16:9 9:16 3:2 2:3 4:3 3:4 1k 2k` (no 4k) and up to 4
   output images. `1k` is the cheapest tier; combining `resolution: "1k"` with
   `aspect_ratio` yielded ~1.5k-2.7k JPEGs (1024x1024 for 1:1, 1773x2364 for
   3:4, 2730x1536 for 16:9). `POST /api/v1/images` with JSON
   `{model, prompt, n:1, resolution, aspect_ratio}` and the `x-api-key` header.
   Responses return `created`, `data` (`b64_json`), `cost`, `paymentSource`,
   `remainingBalance`, and `requestId`; `stream` is unsupported. No retries were
   needed; every request succeeded on the first attempt.
2. `generate.py` decoded each `b64_json`, verified it decodes as a JPEG (header
   walk to SOF + non-trivial size > 30 KB), and wrote it to `idols/`.

## Regenerating

```sh
cd test-assets
python3 generate.py             # all 16 (skips categories already
                                # in manifest.json)
python3 generate.py --only 05   # a single category, by number
```

Requires `NANO_GPT_API_KEY` in the project `.env`. Each new image costs about
$0.09 and is appended to `manifest.json`, which also tracks spend.

## Seeding a deployed canvas

`seed.sh` registers/logs in a seed account, then POSTs every image to
`/api/place` in a non-overlapping grid around the world center so each placement
lands on empty space (empty-space placements need only a ~1 s gap; the script
sleeps 1.5 s between posts). `01-solo-female-closeup` is centered exactly on the
world center `0/0` so the center-holder leaderboard is populated; the other 15
fill a 4-column ring of 512 px slots with 128 px gutters (two rows above, two
rows below the center image).

```sh
./seed.sh https://canvas.example.com seedbot 's3cret-password'
```

Requires only `curl` and `python3`. Exits non-zero with a clear message if the
base URL is unreachable or any placement returns a non-2xx status.
