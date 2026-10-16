# Backend architecture

The backend is a Rust **workspace** of three crates plus one binary each (the
api's binary `backend`, the worker's binary `backend-worker`). The shared,
pixel-free foundation lives in `backend/crates/core` (`iac-core`: world math,
config, DB query helpers, object store, metrics, telemetry, the error taxonomy,
the pure-SQL `chunk_seqs` version bumps and the `versions_note`
cache-invalidation hook); the pixel work lives in `backend/crates/worker`
(`iac-worker`: `img` — every libvips call — the `render_jobs` claim loops, the
`ingest` consumer and the shared `pixel` engine; ONLY this crate depends on
libvips, per docs/scaling-architecture.md § Code and images); and the HTTP
service lives in `backend/crates/api` (`iac-api`, binary `backend`), which
re-exports the core modules at its root so `crate::*` paths stay stable. The
API crate does not link libvips at all — the distroless API image is exact
(bytes are only ever stored and served); pixel work exists solely in worker
processes. The worker binary runs the render_jobs loops
and the ingest consumer WITHOUT the HTTP server; the chart deploys it as the
`backend-worker` Deployment, values-gated dark
(`backend.worker.replicaCount: 0`). Modules (all under `crates/api/src` except
the core and worker sets):

| module          | responsibility                                                  |
| --------------- | --------------------------------------------------------------- |
| `world`         | core · pure tile/chunk/rect geometry (unit + property tested)   |
| `config`        | core · env parsing, cooldown and size knobs                     |
| `chunk_seqs`    | core · pure-SQL chunk version bumps (api + worker share)        |
| `versions_note` | core · in-process cache-invalidation hook (worker → api)        |
| `store`         | core · object storage (S3-compatible; `file://` dev mode)       |
| `img`           | worker · all libvips calls, bytes-in/bytes-out API              |
| `pixel`         | worker · the shared decode/strip/hash/derivative engine         |
| `render_jobs`   | worker · render_jobs claim loops (chunk + parent jobs)          |
| `ingest`        | worker · Phase 4 quarantine consumer behind the 202 upload path |
| `chunks`        | api · chunk raster mutation + moderation replay (transitional)  |
| `tiles`         | api · pyramid rendering, maxSeq staleness, caches               |
| `place`         | api · `POST /api/place`, admin soft-delete pipeline             |
| `api`           | api · near / center / health / reports / image counts           |
| `handlers`      | api · register / login / logout / me                            |
| `auth`          | api · HMAC-signed stateless cookies, `Authed` extractor         |
| `events`        | api · Postgres LISTEN/NOTIFY → broadcast → SSE                  |
| `statics`       | api · SPA hosting with graceful no-frontend fallback            |
| `router`        | api · route table + body limit + tracing                        |
| `error`         | core · every failure maps to JSON `{"error": "..."}`            |

## World math

Native world: 268,435,456 px per side (256 · 2^20). `y` grows down. Tiles at
zoom z are 256 px and cover `[x·256·2^(20−z), (x+1)·256·2^(20−z))` — half-open.
Chunks are 1024 px, keys `chunks/{cx}/{cy}.webp`, `cx = floor(x/1024)`.

Rects are half-open `[x, x+w) × [y, y+h)`. **Touching edges do not intersect**
(zero-area overlap never counts) — for cooldowns, tile seq coverage and center
coverage alike. In SQL this is expressed as `box &&` over _closed integer_ boxes
`[x, x+w-1]`: on integer grids closed-box overlap is exactly half-open rect
intersection, and it keeps the GiST index usable (`placements.bbox` is a
generated stored column).

Zoom 18 is the chunk boundary (`256·2^2 = 1024`): tiles at z ≥ 18 render from
their single covering chunk; lower zooms render from the four child tiles. Tiles
with `maxSeq == 0` (no art ever intersected) short-circuit to a constant paper
tile, which keeps cold pyramid renders proportional to the art spine instead of
4^z.

## Image processing: libvips crate (option a)

Threading: libvips defaults each evaluation pipeline's threadpool to the host
core count (`VIPS_CONCURRENCY` unset ⇒ ncpus — see the upstream "VIPS and
threads" notes and `vips_concurrency_set`). Pipelines are per-operation, so
concurrent placements multiply that: two composites on the 4-core node meant 8
vips threads competing with tile serving (N=2 soak: tile empty-path mean 0.94s
vs 19ms baseline). The deployment sets `VIPS_CONCURRENCY=2` to bound aggregate
composite threads (~1 core for two concurrent placements) and keep cores
available for the async/serving paths. Maintainer guidance (libvips discussion
#4063): for throughput, keep threads-with-useful-work ≈ physical cores; libvips
also runs a few threads beyond the setting, and threadpools self-size down under
load.

Chosen: the `libvips` crate (2.3), in-process, against system libvips (Debian
`libvips-dev` 8.15+ / `libvips42` at runtime). Rationale:

- Build reliability: the crate ships pre-generated bindings; its `build.rs` only
  links `vips`/`glib`. No bindgen, no clang in the image builder — just
  `libvips-dev` + `pkg-config`.
- Single API for decode / strip / resize / composite / encode across jpeg, png,
  webp, gif and avif (heif), vs. multi-process `vips` CLI orchestration with
  temp files.

Version-skew rules (bindings are generated from a newer libvips than Debian
ships; every rule below was probed and is enforced in `img.rs`):

- Saves go through `image_write_to_buffer` with suffix option strings
  (`.webp[Q=85]`, `.jpg[Q=90,strip=TRUE]`, `.avif[Q=90,compression=av1]`) — only
  kwargs that are stable across libvips versions.
- No `thumbnail_image` (newer bindings pass kwargs like `input-profile` that
  8.15 rejects). Exact resize = premultiply → `resize` (Lanczos3 is the default
  kernel) → unpremultiply, then pin dimensions by extract/embed.
- No `ResizeOptions`/`ThumbnailImageOptions` structs: generated defaults pass
  `vscale=0` (collapses height).
- `EmbedOptions.background` length must match band count; generated
  `ArrayjoinOptions::default()` values are not C defaults.
- `arrayjoin` is avoided entirely: hspacing=0 is rejected on 8.15 and the
  failed-build path segfaults under load. Parent tiles join by
  resize→embed→composite instead.
- Synthetic blank canvases built from `black` + constants get a `multiband`
  interpretation libvips refuses to composite onto; virgin chunks embed the
  overlay directly instead (a 1×1 transparent PNG is baked into `img.rs` and
  decoded for the rare fully-empty chunk write).

## Upload pipeline (strip + hash recipe)

`POST /api/place` runs the pixel stage inside `spawn_blocking` (libvips images
are `!Send`, so no image ever crosses an `.await`):

1. Decode upload (magic-byte sniffing via the loader name; jpeg/png/webp/
   gif/avif-heif accepted; unknown → 400). Animated input collapses to frame 0;
   EXIF orientation is applied manually (the crate has no autorotate binding);
   result normalized to RGBA sRGB.
2. **Strip + re-save in the source codec**: jpeg → `.jpg[Q=90,strip=TRUE]`, png
   → `.png[strip=TRUE]`, webp → `.webp[Q=90,strip=TRUE]`, gif →
   `.gif[strip=TRUE]`, avif → `.avif[Q=90,compression=av1,strip=TRUE]`. sha256
   of **those bytes** is the content address: `originals/{sha256}.{ext}`. Raw
   uploads never touch storage; no EXIF (GPS) survives.
3. Derivative: re-decode the stripped bytes, `resize_exact` to the derived `w×h`
   (long edge = requested `size`, 16..4096, short edge rounds to nearest, min
   1), encode webp q85. The optional multipart `rot` (float degrees, CLOCKWISE,
   y-down, default 0, normalized mod 360 on write) does NOT enter the derivative
   — rotation is composite-time only, so content-addressed dedupe is unaffected
   by angle. Those bytes are deterministic for identical inputs and are hashed
   to `derivatives/{sha256}.webp`. Manifest
   `image_key`/`original_sha`/`derivative_sha` point at content keys; uploads
   use put-if-absent so repeats cost nothing.
4. `duplicate_count` (prior non-removed placements with the same `original_sha`)
   is returned additively; `GET /api/images/{sha256}/count` exposes it publicly.

Exact-byte dedupe only. Perceptual near-dup (pHash) is a possible future knob,
out of scope v1. Content hashes (originals and derivatives) are
**environment-scoped**: the product requirement is same-process stability (two
identical uploads inside one running backend dedupe to one object); byte
stability across libvips/libwebp builds is explicitly not required. A server
upgrade may therefore produce new hash keys for new uploads — old keys keep
resolving. The deployment pins libvips to Debian trixie's `8.16.1-1+deb13u2` in
both Dockerfile stages so prod behavior matches one known build (CI gates run
Ubuntu 24.04's libvips 8.15.1 and never assert cross-version byte determinism).

Three hardening rules came out of rotation work, all load-bearing against
libvips-rs 2.3 + libvips 8.15/8.16 — do not "simplify" them away:

- `img.rs` holds a process-wide **reentrant mutex around every libvips entry
  point**, and eval runs with `concurrency_set(1)`. Concurrent pipelines hit
  GObject double-free SIGSEGVs deep in `g_object_unref`.
- **Never `.clone()` a `VipsImage`**: the crate derives `Clone` as a raw pointer
  copy with no refcount — a cloned handle double-unrefs at drop. `ops::copy`
  makes a safe alias node.
- `SimilarityOptions::default()` wraps `vips_interpolate_nearest_static()` — a
  process-lifetime STATIC GObject — whose refcount the options' `Drop`
  decrements. `rotate_centered` therefore `mem::forget`s its options (a bounded
  ~100-byte leak per rotation).

## Placement, cooldown, ordering

Admission runs cheapest-first, and nothing pixel-sized happens before the last
cheap gate: multipart is streamed under the `MAX_UPLOAD_BYTES` wire cap, then
the image is **header-probed only** (`img::probe` — no pixel decode) for the
decoded-pixel budget `MAX_DECODE_PIXELS` (default 64 MPx = 256 MiB RGBA, sized
to keep 48 MPx camera sources placeable — animated inputs are measured on their
first frame, not the strip), then world bounds on the true AABB, then a
read-only **cooldown pre-check**. A decompression bomb (tiny upload, gigabytes
of declared pixels) is a `413` without materializing anything, and a cooled-down
account never spends the process-wide serialized libvips stage on a request the
transaction would reject. `decode` itself re-checks the budget on the header as
a backstop (`img::DEFAULT_MAX_DECODE_PIXELS` for internal callers over
already-validated stored bytes). The strip/resave stage preserves dimensions, so
the probed header determines the placement rect exactly.

Pipeline (all inside chunk-key locks, acquired sorted — deadlock-free, and paint
order always matches `seq` order):

1. Validate `size` bounds and world bounds (checked on the AABB of the rotated
   rect); build derivative + hashes. `bbox` (GiST) stores the AABB of the
   rotated rect — computed once by `world::rotated_aabb` (floor/ceil outward,
   quarter turns snapped exact) and written explicitly by the INSERT (the column
   was a generated box of the unrotated rect; migration 0004 dropped the
   expression when rotation landed).
2. Open tx: intersect test (GiST) → threshold = 300 s (paint) or 1 s (empty);
   **unless the account is `cooldown_exempt`** — the pre-check (step "then a
   read-only cooldown pre-check" above) and the transactional UPDATE both skip
   the floor for flagged accounts (#37:
   `POST/DELETE /api/admin/accounts/{username}/cooldown-exempt`, admin session
   required; the flag lives on `accounts.cooldown_exempt`, migration 0003).
   Regular users are byte-identical when the flag is false; the seeder flags its
   fan-out accounts so full-size pile placement runs at full speed
   (`test-assets/seed.sh`). Single guarded UPDATE — zero rows ⇒ rollback + `429`
   with `Retry-After`:

   ```sql
   UPDATE accounts SET last_placement_at = now()
   WHERE id = $1
     AND (last_placement_at IS NULL OR now() - last >= $threshold)
   ```

3. `seq = nextval('placement_seq')` inside the tx (burned seqs on rollback are
   fine — monotonic, not gapless).
4. Composite the derivative, rotated about its own center by `rot` (libvips
   `similarity`, clockwise-positive, transparent background, output pinned to
   the AABB), onto every chunk the AABB touches (fetch, vips composite at the
   AABB offset, put back). Hit-tests ("covers point") inverse-rotate the point
   and test the unrotated half-open rect (`world::rotated_contains`); cooldown
   overlap stays AABB-vs-AABB via the GiST index.
5. Insert the manifest row, `pg_notify('events', …)`, commit.

Both cooldown knobs (`COOLDOWN_PAINT_SECS`, `COOLDOWN_EMPTY_SECS`) are env-
configurable with spec defaults.

### Placement tags

Each placement carries a normalized label set: `placement_tags` (`placement_id`,
`tag`) — one label per row, applied by migration `0006_placement_tags.sql`.
Labels are lowercased, trimmed, deduped, and validated to 1..=32 chars of
`[a-z0-9_-]`, at most 8 per placement (`api::normalize_tags` — the backend is
the authority; the contract schema only fails fast client-side).

- `PUT /api/placements/{id}/tags` (auth; owner or admin, else `403`) replaces
  the whole set: DELETE + INSERT in one tx, response is the normalized
  `{id, tags}` (`tagsSchema`). Tag edits never touch tiles/images and don't bump
  `seq`, so no SSE event and no tile invalidation — reads pick the new labels up
  on their next fetch.
- Reads join the set everywhere a placement row ships: `placements/near`, dig
  stack entries, and the place response (aggregated
  `array_agg(tag ORDER BY tag)`).

## Batch import (bulk ingest)

The admin import is a **worker subcommand**, not an HTTP route: the API never
runs libvips, so bulk ingest runs wherever the worker binary does (docs/
scaling-architecture.md § Code and images):

```sh
backend-worker import-originals a.jpg b.jpg ...   # -> [{source_sha, derivative_sha, w, h}, ...]
backend-worker import-batch manifest.json         # -> {batch_id, imported, duplicates, chunk_writes, ...}
```

`import-originals` runs the exact former pixel stage per file — strip, resave,
hash, deterministic derivative — at the source dimensions capped to the canvas
derivative long edge (`IMPORT_DERIVATIVE_LONG_EDGE`, default 1024 ≈ one chunk)
and writes the content-addressed `originals/{sha}` + `derivatives/{sha}.webp`
objects. `import-batch` takes one JSON manifest:

```json
{
  "batch_id": "<client-chosen uuid>", // resume key, see below
  "prewarm": false, // optional post-batch z18-z20 render
  "entries": [
    {
      "source_sha": "<64 hex>",
      "derivative_sha": "<64 hex>",
      "x": 0,
      "y": 0,
      "w": 512,
      "h": 384,
      "rot": 7.5,
      "tags": ["duo", "acoustic"] // optional, normalized like the tags PUT
    }
  ]
}
```

Entries reference EXISTING content-addressed objects (the `import-originals`
output builds the manifest — `scripts/import-bulk.sh` does exactly this).
`derivative_sha` is optional in the manifest: when omitted it resolves from the
newest placement of `source_sha` (one `DISTINCT ON` round trip per batch);
multi-size originals must pass it explicitly. The engine verifies `w`/`h`
against the derivative's real dimensions (header-only vips probe, once per
unique sha) — chunk paints and moderation replays composite at the bitmap's
natural size, so a mismatch would break replay consistency and is rejected
(400).

Pipeline, per sequential **block** of `IMPORT_BLOCK_ENTRIES` (default 500):

1. Validate + plan every entry (sha shape, size bounds, finite `rot` →
   `normalize_rot`, world bounds on the rotated AABB — identical semantics to
   /api/place: `bbox` = AABB, `x/y/w/h` = unrotated rect, `placements.rot` float
   degrees). Entry identity =
   `uuidv5(NAMESPACE, uuidv5(batch_id, "sha|sha|x|y|w|h|rot"))`.
2. Dedupe: in-block duplicates collapse; committed entries are pre-checked
   (`SELECT id … = ANY($1)`) and skipped — they count as `duplicates`.
3. Prefetch the block's unique derivatives (≤ 8 concurrent S3 GETs) into a
   per-request cache (64 MiB / 512 entries, clear-on-overflow), so a pile re-sha
   pays ~zero GETs after its first block.
4. Group entries into **chunk-disjoint connected components** (union-find over
   shared chunk keys). Overlapping AABBs always share a chunk, so components
   never contend on rasters — they run in parallel with disjoint, sorted lock
   sets (deadlock-free by the same argument as the single-placement path).
5. Each component holds its chunk locks (sorted) across the whole commit —
   the same invariant the HTTP path held — then: ONE seq allocation round trip
   (`SELECT nextval('placement_seq') FROM generate_series(1, n)`, contiguous,
   ascending in entry order), the chunk pass, ONE multi-row
   `INSERT … SELECT unnest(…) … ON CONFLICT (id) DO NOTHING` (15 array binds),
   commit. The chunk pass runs in 2×2-chunk **super-blocks**: one vips section
   per super-block (`img::composite_group`) decodes + rotates each unique
   `(derivative_sha, rot)` overlay at most once within the block (bounded by a
   256 MiB decoded budget — decoded RGBA is 4 B/px and a dense super-block
   evicts rather than balloons), composites every member with libvips' built-in
   clipping, and encodes each chunk exactly once. Pixel output is byte-identical
   to the per-chunk `replay_canvas` the moderation path uses (unit-tested
   equivalence) — the win is decode/rotate reuse across the chunks an entry
   spans.

That collapses the four per-placement costs to per-chunk-group costs: chunk
GETs/PUTs, seq round trips, and inserts all scale with _touched chunks_, not
images. A literal single shared seq value per chunk is not possible
(`placements.seq` is UNIQUE and `ORDER BY seq` is the paint order moderation
replay depends on) — the group allocation is the faithful reading of "one seq
bump per chunk per batch": one round trip per chunk-group,
`chunk_write_amplification` proves the rest.

SSE: ONE coarse event per block, never per placement —
`{"type":"import","batch_id","seq":<max allocated>,"count":<n>, "bbox":[union AABB]}`.
The web client ignores unknown event types (`web/src/lib/sse.ts`), and tiles
invalidate through the pipeline's staleness path, so no web change is required.

Tiles: nothing is rendered during ingest — writing no `tiles` row makes affected
tiles stale implicitly (their stored seq falls below the live maxSeq), which IS
the dirty-marking. `prewarm: true` renders z18–z20 tiles for the batch's touched
chunks after the last commit (21 tiles per chunk) directly through the
pipeline's tile-cutting code, so prewarmed tiles are indistinguishable from
pipeline-cut ones.

Resume/idempotency: re-run the same manifest with the same `batch_id` —
identical entry ids are skipped, so a killed run resumes without double-placing.
Raster paints are at-least-once (paint before commit; a kill between PUT and
commit loses nothing that a resume doesn't repaint pixel-identically). The
cross-writer hazard (a concurrent placement between our seq allocation and
commit) is impossible by construction: allocation happens under the group's
locks.

Concurrency model: blocks are strictly sequential (cross-block chunk overlap
would race); within a block, components run on `IMPORT_GROUP_CONCURRENCY`
(default 2) executors. Rationale: libvips work is serialized process-wide
(`img::init`, crash-history driven), so extra executors only overlap S3/PG round
trips with another component's pixel stage while each holds ~4 MiB of decoded
raster — 2 saturates the SLO, more inflates memory without vips throughput.
`VIPS_CONCURRENCY` stays respected-by-subsumption: eval parallelism is pinned to
1 for stability, and the SLO is met without it.

Body/caps: a manifest is one file — for very large runs chunk client-side:
`scripts/import-bulk.sh` derives each chunk's `batch_id` as
`uuid5("import-bulk:{tag}:{index}")`, so re-running resumes every chunk
independently. Knobs: `IMPORT_BLOCK_ENTRIES` (500), `IMPORT_GROUP_CONCURRENCY`
(2).

Client driver (`scripts/import-bulk.sh`, env-driven against the worker
binary — no HTTP, no admin cookie):

- **Chunking**: default `--chunk 750`, sized so one `import-batch` run fits
  comfortably under an hour at the observed live rate (1.7 img/s single
  client, ~2.5 img/s with 4 concurrent workers — real in-region S3,
  latency-bound). Chunk size is **fixed per run**: it feeds the entry ids, so
  changing `--chunk` mid-run re-places the same coordinates — use a new
  `--tag`.
- **Retry**: a chunk run is retried up to 4× with backoff (safe: the engine
  commits per block and entry identity is `uuid5(batch_id, entry)`, so a
  timed-out-then-retried chunk can never double-place).
- **Preupload cache**: `(sha256,size) → response map` is kept in
  `/tmp/import-bulk-preupload-cache-<db>.json` (content-addressed, shared
  across tags, keyed per database). Re-runs only upload sources the worker has
  never seen — the engine dedupes by source sha regardless; the cache saves
  the repeated decode. After a full wipe/re-seed the cached derivative objects
  are gone: delete the cache file to force a full re-upload.
- **Live numbers** (production, 51,487 placements, radius 12000, via the
  former HTTP engine — the worker path is the identical code): ~2.49 img/s
  sustained over 2 h20m with 4 concurrent clients; single client ~1.7 img/s;
  the wall is per-entry chunk-canvas S3 round-trips + composite serialization,
  not CPU.

Counters (worker-process atomics, surfaced in the run summary JSON):
`import_batches_total`, `import_images_total`, `import_chunk_writes_total`,
`import_duration_seconds`, and the derived gauge `chunk_write_amplification`
(`import_images_total / import_chunk_writes_total`) — the write-amplification
number (the per-placement path is pinned near 1; amplification grows with pile
density — entries per touched chunk — and the engine preserves that collapse
within every block).

Measured (local bench, `scripts/bench-import.sh 2000 12000 2500`, worker
container pinned to 4 CPU / 6 GB, postgres 1 CPU / 2 GB, tmpfs file store,
fresh DB): 2500 images in 513 s → **4.9 images/s sustained**,
`chunk_write_amplification` 1.08 (2500 images / 2318 chunk writes), zero failed
S3 ops, one batch, p≤600 s duration bucket. The 1024-px chunk grid against
≤1024-px derivatives means a uniform-disc pile at radius 12000 covers ~500
chunks (~5 entries/chunk), so amplification near 1 is the honest geometry —
pile densification (smaller radius or higher count at fixed radius) is what
moves it, not an engine knob. The 4.9/s is the LOCAL container rate (laptop CPU
shared with the dev environment; tmpfs storage flatters the S3 leg); the ≥10/s
SLO is decided by the post-wipe 100k run on the real 4-vCPU node (see the
reseed runbook).

## Tile serving and admission control

`GET /api/tiles/{z}/{x}/{y}.webp` (axum 0.8 has no suffix captures, so the last
segment is a wildcard parsed as `<n>.webp`). The handler **never renders**: it
resolves bytes and versions only.

1. `maxSeq` over the tile bbox (GiST-backed) = the expected ETag / live version.
2. `maxSeq == 0` → the empty-canvas constant (tiny, immutable, `no-store`-safe),
   no storage round trip.
3. In-memory cache keyed `(z, x, y, seq)` — `z ≤ 4` never evicts (341 tiles
   max), higher zooms use an LRU (`TILE_CACHE_ENTRIES`, 2048).
4. Object-stored tiles are served only if the `tiles` table row's seq matches
   the live maxSeq (`hit_s3` on a cache miss); a stored-but-stale tile serves
   its stored bytes immediately with the stored seq as ETag plus
   `X-Tile-Stale: true` (`stale_served`).
5. No stored bytes at all → **404** with `X-Tile-Pending: true` (`miss`): the
   pipeline owns pixels now, and a cold tile appears when the worker first cuts
   it.

`ETag: "seq"`, `Cache-Control: public, max-age=1, stale-while-revalidate=30` —
`max-age=1` stops per-tile revalidation storms during placement bursts,
`stale-while-revalidate` serves tiles instantly while the next GET refreshes
them, the SSE-driven client refetch bounds visible staleness to ~1s past a
place/remove commit, and conditional GETs stay cheap 304s while the seq is
unchanged. A removal advances the chunk version past the old max (fresh
sequence draw), so the ETag changes and the affected tiles re-cut exactly once
by the pipeline.

### Server-side stale-while-revalidate (the pile-wedge fix)

Motivation (prod, 2026-10-13): at a ~700-placement pile, every placement bumped
the seq of every intersecting tile, so tile GETs over the dense area at low zoom
required _fresh renders on the request path_. Four concurrent center-tile
renders (z0/z8/z20) each pinned vips + a `spawn_blocking` thread for 15s+ and
starved the placement pipeline: place latency went 4.3 s → 60 s timeouts while
clients held the map open.

The first fix made GETs never render while stale bytes exist (serve stored
bytes + record demand); Phase 3 removes request-path rendering entirely — the
GET handler cannot allocate vips work, so the wedge class is gone by
construction and the admission machinery (render queue, queue cap, 202
shedding, in-process background worker, decoded-raster cache) went with it.
What remains is pure byte service: memory → object storage → 404, with
staleness decided by one indexed max-seq probe.

### The chunk-seq rollup (`chunk_seqs`, migration 0007)

Tile staleness asks one question per GET: _what is the newest version of the
content under this tile's rectangle?_ Answering it from `placements` (a GiST
recheck per request) re-derives on every read what changes only on write, and
the row count is pile-depth-dependent — the incident's other half: at 51k
placements a single low-zoom GET fetched its whole covering row set (hundreds of
MB across concurrent GETs) and OOM-killed pods. The doctrine now:

- **Writes maintain the rollup.** Every chunk a placement touches gets a row in
  `chunk_seqs (cx, cy, max_seq)`, bumped by `chunks::bump_chunk_seqs` in the
  SAME transaction as the write (place insert, import batch) — from the
  placement's rotated AABB, the same span compositing painted, so a corner chunk
  reached only by rotation invalidates too. **Removals re-bump**: a removal
  replays the chunk rasters and then draws a FRESH `nextval('placement_seq')` as
  the chunk's new version (no row is inserted). Versions therefore only move
  forward — a removed placement that was painted over still advances its chunks
  past the old max, tiles re-render exactly once, and (as a side effect) the
  `/api/dig` etag advances with the sequence.
- **Reads touch only the rollup.** `TileCtx::prefetch` fetches the PK range
  `cx/cy BETWEEN` the tile's chunk span — view cost independent of pile depth,
  no planner traps, placement rows never consulted at render time.
- **Seed at deploy.** Migration 0007 seeds the table from live placements'
  generated `bbox` corners; the seed runs once at deploy (2s at the current
  pile) and subsequent writes keep it fresh.
- **Bounded node pyramid.** The per-node max is precomputed bottom-up from the
  chunk map — one max-level per zoom between the entry tile and `BASE_Z` (~4/3 ×
  the chunk rows total). Materializing per-node memos on demand is a footgun
  this replaces: a z5 entry GET would recurse (4^13−1)/3 ≈ 22M nodes (~1.5 GB)
  before serving a byte (#150). The z≤4 memory cache similarly keeps ONLY the
  newest seq per coordinate (#155) — the ancestors of the whole pyramid
  re-render every quiet window while anyone is zoomed out, and keying by seq as
  well would leak one entry per render for the life of the pod.

### The render queue (`tile_render_jobs`) — removed with Phase 3

Migration 0005's `tile_render_jobs` queue, its in-process background worker,
the queue cap and 202 shedding, and the decoded-raster LRU existed to keep
renders OFF the request path while the API still rendered. Phase 3 moves pixel
work into the worker's `render_jobs` loops entirely, so the whole admission
layer is gone: GETs only serve bytes (see _Tile serving_ above), demand is the
`chunk_seqs`/`render_jobs` staleness signal, and the pipeline section below is
the single render path. Migration 0005 remains in history; fresh databases
never create the table (migrations 0005+0007 are superseded by 0008's
`render_jobs` + `chunk_seqs`).

### The render pipeline (`render_jobs`, migration 0008 — Phase 3, ships dark)

`docs/scaling-architecture.md` § "Chunk and tile pipeline" as code: a
version-driven job queue that brings chunks up to date and cuts their 21 zoom
18–20 tiles in the same pass, then bubbles versions up the pyramid through
parent jobs, so tile requests never render anything. It ships **dark**:
`RENDER_PIPELINE_ENABLED=false` (the default) keeps every code path above
exactly as-is — synchronous placement composites, synchronous removal replays.
Workers are a separate knob:
`RENDER_PIPELINE_WORKERS` (default 0) spawns the claim loops, and may be raised
BEFORE the enabled flip so a pre-seeded queue is painted while the request path
still composites (the staged cutover below). With zero workers and the flag off
nothing runs (test-enforced by
`render_jobs::pipeline_disabled_keeps_request_path_unchanged`). Migration 0008
applies at boot either way; the new columns back-fill untouched (`painted_seq`
0 = "nothing painted by the pipeline yet"). Cutover is the values flip plus a
worker deployment (see the rollout plan below).

- **Schema.** `chunk_seqs` gains `painted_seq` (the version the stored raster
  and tiles actually show — what is _painted_ next to what is _wanted_) and
  `needs_rebuild` (set by removals). `render_jobs` is one table keyed
  `(z, x, y)`: z = 18 is a **chunk job** (a chunk IS a zoom 18 tile), z < 18 are
  **parent jobs** rebuilt from their four children's current bytes.
  `state ∈ pending | running | failed`, `rerun` (changed while running),
  `not_before` (per-zoom delay), `claimed_at` (crash lease), `attempts`.
- **Claiming.** `FOR UPDATE SKIP LOCKED` — exactly one worker owns a tile, which
  replaces the request path's session advisory locks once enabled. Deeper zooms
  claim first (`ORDER BY z DESC`) so children finish before their parents;
  `running` rows past the lease (`RENDER_PIPELINE_LEASE_SECS`, 120 s) are
  reclaimable — the claim is the retry and bumps `attempts`.
- **Chunk job.** If `needs_rebuild` is set (or the raster object is missing
  under a nonzero `painted_seq`), repaint from BLANK: every live placement in
  sequence order (issue #159's fix, moved off the request path). Otherwise paint
  only placements with `seq > painted_seq` onto the current raster, reusing the
  import engine's `composite_group` (one vips section, decoded-overlay cache).
  Then cut the z 18–20 tiles from the raster already in memory and write chunk,
  tiles, the `tiles` staleness rows (monotone, `GREATEST`), then the version
  bookkeeping (below), then mark the zoom 17 parent dirty. The stale-serve read
  path above needs no changes: it heals from a freshly written chunk exactly as
  it heals from a placement composite today.
- **Parent job.** Fetch the four children's current bytes (children claim first
  and a missing child is a retryable error), stitch with `render_parent_tile`,
  write the object, and set the `tiles` row to the highest child version — which
  only grows (`GREATEST`). Then mark its own parent dirty, up to z 0.
- **Merging rules.** Marking dirty is an upsert: a burst folds into ONE job. A
  `running` job is never aborted — a change arriving meanwhile sets `rerun`, and
  the job re-queues itself when it finishes (aborting would starve busy chunks).
  Parent jobs wait out zoom-dependent `not_before` delays: z ≤ 10 at most every
  10–30 s (`RENDER_PIPELINE_PARENT_LOW_MS` + a coordinate spread across
  `RENDER_PIPELINE_PARENT_SPREAD_MS`), z 11–17 after a short quiet window
  (`RENDER_PIPELINE_PARENT_QUIET_MS`, 3 s). Finished rows are DELETED (a jobs
  table that keeps `done` rows forever makes every row change replica-replayed
  WAL). After 5 attempts (`MAX_JOB_ATTEMPTS`, spec constant) a
  job is parked `failed` instead of retrying forever; a fresh change revives it.
- **Version guard.** The finish transaction advances `painted_seq` with
  `GREATEST(painted_seq, $v)` — a slower job can never move a version backwards
  (the doc's `WHERE painted_seq < $v`, hardened: the equal case must still
  consume a matching `needs_rebuild` flag or a rebuild would hot-loop) — and
  clears `needs_rebuild` only when it still holds the value seen at claim time,
  so a removal landing mid-job survives. All finish statements run in one
  transaction with a consistent lock order (chunk_seqs → own job row → parent
  job row), the same order the place/remove transactions use.
- **Ordering.** Paint order must equal sequence order, but sequence numbers are
  drawn inside transactions that can commit out of order. Pipeline-on place
  transactions hold a single sequencer advisory lock (`pg_advisory_xact_lock`)
  from the `nextval` draw to commit, so commit order equals sequence order.
  Safety net: the pipeline chunk bump flags `needs_rebuild` when a placement
  arrives BELOW the chunk's recorded `max_seq` (out-of-order commit — possible
  from paths that don't take the lock, e.g. batch imports), and the flagged
  chunk job repaints from blank where order is the manifest order.
- **Removal.** Sets `removed_at`, advances the chunk's version (fresh sequence
  value), sets `needs_rebuild`, queues the chunk job, and returns. The rebuild
  costs ~80 ms per remaining image (~40 s in the production pile), which is why
  it belongs in the worker, not the request.
- **Worker.** One task per process (`render_jobs::spawn_worker`, spawned only
  when enabled), poll `RENDER_PIPELINE_POLL_MS` when idle; horizontal scale =
  more worker processes (the claim query is the only coordination). Logs use the
  queue grammar: `render job claimed`, `render job done` with
  `dur_ms`/`outcome`. Import batches keep painting synchronously in Phase 3 — a
  chunk they touched repaints once on next activity (idempotent pixels, bounded
  by one pass), because `painted_seq` starts at 0 for pipeline-unaware writes.

### The ingest pipeline (`ingest_jobs`, migration 0009 — Phase 4 upload flow, ships dark)

`docs/scaling-architecture.md` § "Upload flow" as code: the placement request
stops doing pixel work. `POST /api/place` validates cheaply (header probe,
bounds, cooldown), stashes the upload under `quarantine/{job_id}` in object
storage, inserts an `ingest_jobs` row and returns **202** with
`{id, state: "pending", x, y, w, h, rot}` (contract `placeAcceptedSchema`). The
in-process ingest consumer claims the job, runs the shared pixel engine
(`place::process_original` — decode, strip, re-save, hash, derivative), puts the
content-addressed `originals/{sha}` + `derivatives/{sha}.webp` objects, and
records the placement in one transaction that holds the sequencer lock from the
`nextval` draw to commit (commit order = sequence order, the doc's ordering
doctrine), advances chunk versions (`bump_chunk_seqs_pipeline`), queues
`render_jobs`, marks the job `placed` and notifies `placed` on the SSE stream —
or `place_failed` when it gives up. The client follows the events or polls
`GET /api/uploads/{id}`, whose body is exactly contract/zod's
`uploadStatusSchema` (`pending` / `placed` with placement id, seq and AABB /
`failed` with attempts, permanence and error); unknown ids are 404, and other
users' jobs are 404 too (no existence leak).

It ships **dark**: `INGEST_PIPELINE_ENABLED=false` (the default) keeps the
synchronous 201 path byte-identical — same body, inline composite, no
quarantine, no rows (test-enforced by
`quarantine::dark_path_stays_synchronous_201`). Migration 0009 applies at boot
either way.

- **Failure doctrine.** Validation-class errors — the client lied about the
  bytes, or libvips disagrees with the API's header read (decode failure, or
  decoded dimensions ≠ probed dimensions) — fail the job permanently on the
  first attempt. Everything else retries with a `RENDER_PIPELINE_POLL_MS`
  backoff up to 5 attempts (`MAX_INGEST_ATTEMPTS`), then parks `failed`. A
  terminal failure notifies `place_failed` (attempts, permanent, error) and
  REFUNDS the cooldown the 202 consumed: `last_placement_at` is cleared when no
  later placement has moved it, so the client can retry immediately.
- **Claiming.** `FOR UPDATE SKIP LOCKED`, oldest first; `running` rows past the
  lease (`RENDER_PIPELINE_LEASE_SECS`) are reclaimable — the claim is the retry
  and bumps `attempts`. The record transaction is atomic (placement row + job
  state commit together), so a crashed consumer's re-run cannot double-insert.
- **Removal rides the same queue.** `DELETE /api/admin/placements/{id}` under
  the flag returns **202** with a removal job; the consumer locks the placement
  row, sets `removed_at`, draws the version bump, sets `needs_rebuild`, queues
  the chunk jobs and notifies `remove`. Locking the row (rather than applying
  inline) also orders a delete behind the placement's own ingest.
  Already-removed targets complete idempotently; rapid double-deletes both 202
  and both finish.
- **Consumer.** One task per process (`ingest::spawn_consumer`, spawned only
  when enabled), poll `RENDER_PIPELINE_POLL_MS` when idle; queue depth is
  exposed as the `ingest_jobs_queue` gauge per state. The quarantine object is
  deleted after the placement commits (best-effort; a lifecycle rule on
  `quarantine/` backs up crashed consumers, doc step 3). `INGEST` is a query
  family in the per-family DB latency series.
- **Cutover order matters.** The consumer queues render jobs; with zero render
  workers the placement would be recorded but never painted. Raise
  `RENDER_PIPELINE_WORKERS` first, flip `RENDER_PIPELINE_ENABLED`, verify the
  tiles, and only then flip `INGEST_PIPELINE_ENABLED` (the full Phase 4 flip).

## Moderation

`DELETE /api/admin/placements/{id}` (403 for non-admins): soft delete
(`removed_at`), then affected chunks are **re-rendered from the manifest** (non-
removed, seq order, derivatives as sources) inside the same transaction, then a
`remove` NOTIFY fires. Center "current" follows the manifest: removing the top
holder rewinds to the previous covering placement (documented consequence of
manifest-derived history). With the render pipeline enabled (Phase 3, ships dark
— see the pipeline section above) the transaction instead advances the chunk
versions, sets `needs_rebuild` and queues chunk jobs; the same manifest-order
rebuild happens in the worker and the pixels converge to the identical raster.

## Dig stack

`GET /api/dig?x=&y=` (auth-free read) returns the dig stack: every placement
whose **rotated** rect contains the point, newest-first —
`{ stack: [{depth, id, username, x, y, w, h, rot, seq, created_at, tags], total, truncated }`,
depth 1 = topmost. Candidates come from the GiST bbox (`bbox @> point`),
membership is refined with the exact rotated hit-test
(`world::rotated_contains`), so an AABB corner hit that misses the rotated
pixels does not dig up. The stack is capped at 64 entries (`truncated: true`
reports a cut stack; `total` is the full size). The ETag is the global
`placement_seq` counter — the generator the tile-read staleness marker derives
from — so a matching If-None-Match is a cheap 304 and any new placement
invalidates every outstanding stack; removals surface via the SSE `remove` event
(a removed middle stratum does not move the seq).

## Feed

`GET /api/feed` (auth-free read) lists the newest placements, newest-first by
global `seq`, for the web's feed panel. Query parameters:

- `tag` (optional): filter to one normalized label — the input is trimmed and
  lowercased, then matched exactly; anything outside `1..=32` chars of
  `[a-z0-9_-]` is a `400` (same rule set as the tags PUT, via the same
  validator).
- `limit` (optional, default 20, capped 1..=50).
- `before` (optional): keyset cursor — return only placements with
  `seq < before`.

Response `{ items: NearPlacement[], next_before: number | null }`: `items` is
newest-first (tags included); `next_before` is the cursor for the next-older
page and is null once a page comes back short (exhausted). Keyset pagination
rather than offsets: seq is monotonic, so paging is stable under concurrent
placements and cheap (`seq` index, no `OFFSET` scans). Moderation follows the
other reads: removed placements drop out.

## Hot regions

`GET /api/hot` (auth-free read) summarizes where placements cluster over a
recent window, for the web's "Hot" panel. The grid is the z6 tile grid (64x64
cells of 4 Mi native px), addressed in tile coordinates 0..64 — aligned with the
tile pyramid so a cell IS a map region.

- `window` (optional, hours, default 24, 1..=168) bounds the query by
  `created_at >= now() - window`.
- `limit` (optional, default 8, 1..=24) bounds the response size.
- Score is the recency weighting: each placement contributes
  `1 / (1 + age_days)` — a fresh placement weighs ~1, a week-old one ~0.13.
  Cells sort by score (ties by raw count).

Response `{ window_hours, total, cells }`: `total` counts window placements
world-wide (not just the returned cells); `cells` is bounded to `limit` entries
`{x, y, count, score}`. Removed placements never count.

## Events

One `PgListener` task `LISTEN`s on channel `events` and fans payloads into a
tokio broadcast channel; SSE handlers subscribe. Payloads:
`{"type":"place","seq":N,"bbox":[x,y,w,h]}` and
`{"type":"remove","id":"…","bbox":[x,y,w,h]}`. Keepalive comments every 15 s.

## Metrics

`GET /api/metrics` serves Prometheus text format (version 0.0.4), public,
aggregates only. Hand-rolled (`crates/core/src/metrics.rs`) instead of the
`metrics`/`prometheus` crates: the series set is small and fixed, so every
series is a pre-created atomic (no registry, no locks on the hot path) and
`render` is a pure formatting walk. Series:

- `place_requests_total`, `place_duration_seconds` histogram — full placement
  pipeline (multipart through NOTIFY), 201s only.
- `tile_requests_total{outcome=hit_mem|hit_s3|empty|stale_served|miss}` +
  `tile_duration_seconds{outcome}` histograms — every tile GET resolution
  outcome. There is no `render` outcome: the GET path cannot render (Phase 3);
  `miss` = cold tile answered 404 `X-Tile-Pending`; `stale_served` =
  stale-but-stored bytes served on the spot.
- `s3_ops_total{op=get|put,ok=true|false}` + `s3_duration_seconds{op}`.
- `chunk_composites_total` — chunk alpha-composites while placing (the API's
  dark path; zero once the pipeline is live).
- `auth_rate_limited_total` — login attempts rejected by the login limiter.
- `db_pool_connections{state=idle|in_use}` gauges from sqlx (`num_idle()` /
  `size()`).

Worker-process series (the same atomics, updated by `render_jobs` / `ingest` /
the import engine inside the worker binary; the worker does not listen on HTTP,
so `/api/metrics` shows them only as zeros — the run summary JSON of
`backend-worker import-batch` is the per-run evidence, and the cutover watches
queue depth directly in Postgres):

- `render_jobs_queue{state=pending|running|failed}` — the Phase 3 queue depth,
  refreshed by the worker claim loop.
- `ingest_jobs_queue{state=pending|running|placed|failed}` — the Phase 4
  quarantine queue depth.
- `ingest_duration_seconds` histogram — ingest consumer job wall time.
- `import_batches_total`, `import_images_total`, `import_chunk_writes_total`,
  `import_duration_seconds` histogram (1 s → 3 h buckets) — the batch import
  engine (see Batch import); the derived gauge `chunk_write_amplification` =
  `import_images_total / import_chunk_writes_total`.

The exposition format is unit-tested by parsing it back (TYPE lines, label
syntax, ascending `le` buckets, `+Inf` == `_count`).

## Auth

Stateless HMAC-SHA256 cookie `session=<b64(payload)>.<b64(sig)>`, payload
`{sub, exp}` (30 days). `HttpOnly`, `SameSite=Lax`, `Secure` when
`PUBLIC_BASE_URL` starts with https. Passwords: argon2id. First account becomes
admin; a partial unique index makes "exactly one admin" race-free (loser retries
as a normal account). Trade-off: logout can only clear the cookie — a presented
token stays valid until expiry (no session store).

Admins can also grant a per-account **cooldown exemption**
(`POST /api/admin/accounts/{username}/cooldown-exempt` with `{"exempt": bool}`,
`204` idempotent, `404` unknown, `403` non-admin; the flag shows in
`GET /api/admin/accounts` as `cooldown_exempt`): exempt accounts skip both
cooldowns entirely (no `429` path) — intended for seeding/service accounts,
everyone else unchanged.

Registration approval gate (external signups only; local development is
unaffected): when `REGISTRATION_APPROVAL_REQUIRED=true`, new accounts start
unapproved — register answers `201 {username, pending: true}` with NO session
cookie, login answers `403 {"error":"account pending approval"}`, and an admin
approves via `GET /api/admin/accounts?pending=true` (oldest first) and
`POST /api/admin/accounts/{username}/approve` (`204`, idempotent, `404` unknown
user, `403` non-admin). The env defaults to false so backend/dev/e2e work never
touches the gate; production sets it true. The bootstrap admin is always
approved (and admins bypass the login gate), so a gated fresh deployment cannot
lock itself out; the migration backfills `approved=true` for every pre-gate
account.

Login throttling: every `POST /api/auth/login` spends one token from two
in-memory token buckets — per client IP (first `X-Forwarded-For` entry; direct
traffic folds into a shared `direct` key) and per `(username, IP)` pair —
capacity 10, refill 1 per 2 s, fail-closed (a rejected attempt spends nothing).
An empty bucket answers `429` + `Retry-After` with `auth_rate_limited_total`
counting it; the buckets are bounded (lazy prune past 4096 entries) and reset on
restart. Verification burns the same argon2 cost for unknown usernames as for
real ones, so response timing cannot enumerate accounts. The spend-before-verify
ordering means even a correct password cannot bypass an empty bucket — lockout
ends only by refill (or restart), never by succeeding.

## Configuration

Required: `DATABASE_URL`, `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY_ID`,
`S3_SECRET_ACCESS_KEY`, `SESSION_SECRET` (≥ 32 bytes). Optional with defaults:
`LISTEN_ADDR=0.0.0.0:8080`, `PUBLIC_BASE_URL`, `STATIC_DIR=../web/dist`,
`COOLDOWN_PAINT_SECS=300`, `COOLDOWN_EMPTY_SECS=1`, `MAX_UPLOAD_BYTES=10485760`,
`MAX_DECODE_PIXELS=67108864`, `MIN_SIZE=16`, `MAX_SIZE=4096`,
`TILE_CACHE_ENTRIES=2048`, `RUST_LOG=info`, `IMPORT_BLOCK_ENTRIES=500`, `IMPORT_GROUP_CONCURRENCY=2`, `IMPORT_DERIVATIVE_LONG_EDGE=1024`,
`REGISTRATION_APPROVAL_REQUIRED=false` (set true in production to gate external
signups behind admin approval), `RENDER_PIPELINE_ENABLED=false` (Phase 3 master switch),
`RENDER_PIPELINE_WORKERS=0`, `RENDER_PIPELINE_POLL_MS=1000`,
`RENDER_PIPELINE_LEASE_SECS=120`, `RENDER_PIPELINE_PARENT_QUIET_MS=3000`,
`RENDER_PIPELINE_PARENT_LOW_MS=10000`, `RENDER_PIPELINE_PARENT_SPREAD_MS=20000`,
`INGEST_PIPELINE_ENABLED=false` (see the two pipeline sections above).

`S3_ENDPOINT=file:///path` selects a local-filesystem store (dev only); anything
else is an S3 endpoint (`allow_http` for plain-http endpoints).

Static hosting is a dev convenience: when `STATIC_DIR` is missing the server
answers `/` with a tiny JSON and 404-JSON elsewhere; prod routes `/` to the
separate web pod.

## Local development

```sh
sudo apt install libvips-dev postgresql        # build + test deps
sudo service postgresql start
sudo -u postgres createuser -d -s idol   # local trust auth, no password
sudo -u postgres createdb -O idol idol
export DATABASE_URL=postgres://localhost/idol
export S3_ENDPOINT=file:///tmp/idol-s3 S3_BUCKET=dev
export SESSION_SECRET=$(head -c 32 /dev/urandom | xxd -p -c 64)
cargo run -p iac-api --bin backend             # API on :8080 (serving only)
cargo run -p iac-worker --bin backend-worker   # render loops + ingest consumer
```

Migrations: plain SQL in `migrations/`, embedded via `sqlx::migrate!`. Gotcha:
the macro does not re-expand on migration-file edits —
`cargo clean -p idol-appreciation-club-backend` after editing one. Comments
inside migrations must not contain `;`.

Tests:

- `cargo test` — unit + property tests (tile math, chunk keys, cooldown
  boundaries incl. 299.99 s vs 300 s, dedupe hashing, pyramid ops).
- `cargo test --test integration` (api) / `--test pipeline` + `--test
  quarantine` (worker) — full flows against testcontainers postgres 16 +
  filesystem store (requires Docker/Podman; the module default is postgres 11,
  so the tests pin 16-alpine for generated columns). The pipeline suites run
  with `--test-threads 2` (a shared `pipeline_serial` lock serializes the
  jobs-table tests).
- `backend/scripts/smoke.sh [IMAGE]` — boots postgres + minio + the built image
  and proves register → place → 429 → tiles → center → SSE end to end.
- `scripts/import-bulk.sh` / `scripts/bench-import.sh` — the bulk import
  driver and its SLO bench, both driving the worker subcommands directly (see
  Batch import).
- `S3_LIVE_CHECK=1 cargo test --test s3_live -- --ignored` — roundtrip against
  the REAL bucket (plus normal `S3_*` env); exercises the production buffered
  known-length PUT path (UpCloud rejects streamed/chunked SigV4 PUTs). Skipped
  everywhere else, so default runs never need cloud credentials. A second
  ignored test lists the `derivatives/` prefix — the dedup receipt (N placements
  of M unique images must keep ~M derivative objects).
- `test-assets/seed.sh BASE USER PASS` — classic 16-image layout; `SEED_COUNT=N`
  scale-seeds N cells of a 100×100 grid cyclically from the 100 unique images at
  a fixed 512 long edge (byte-identical derivatives → dedup), resumable via
  `SEED_STATE_DIR`, aborts after 3 consecutive non-201s, keeps near-center cells
  on the seed account (`SEED_NEAR_PX`, default 8192), and accepts
  `SEED_ACCOUNTS_FILE` (JSON) for per-account-cooldown fan-out.

CI note: fmt/clippy/test all need `libvips-dev` installed on the runner (the
worker crate; the api builds vips-free).

## Telemetry

OpenTelemetry tracing + trace-correlated logs, per
`docs/scaling-architecture.md` § Observability. **Off by default**: with no
`OTEL_EXPORTER_OTLP_ENDPOINT` the process emits exactly what it did before (JSON
logs on stdout), with zero new threads or exports.

_*Configuration — standard OTEL\_* environment only_* (no app knobs):

| Variable                      | Effect                                                                                                                        |
| ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | set ⇒ spans export over OTLP/HTTP+protobuf (batched); unset ⇒ logs only                                                       |
| `OTEL_SERVICE_NAME`           | resource `service.name`, default `api`                                                                                        |
| `OTEL_RESOURCE_ATTRIBUTES`    | `k=v,k=v` into the resource (version, environment, pod/node from the downward API)                                            |
| `OTEL_SDK_DISABLED`           | `true` forces logs-only even with an endpoint set                                                                             |
| `OTEL_TRACES_SAMPLER(_ARG)`   | default `parentbased_traceidratio` at 5%; also honors `always_on`, `always_off`, `traceidratio`, and their parent-based forms |

**What carries a span**

- HTTP requests: `http` span named by route template (`http.route`,
  `http.request.method`, `url.path`, `request_id`), parented from an inbound W3C
  `traceparent` when present; INFO — it is also the request's JSON access-log
  line (status + latency on close).
- SQL: `postgresql <family>` spans in `dbq::timed` — `db.system.name`,
  `db.query.summary`, `db.operation.name`, and the pool's endpoint
  (`rw`/`ro`/`pooler`/`primary`), DEBUG so stdout stays quiet while the export
  still gets them.
- libvips work: every `spawn_blocking` call site routes through
  `telemetry::blocking`, which carries the current span into the blocking thread
  — pixel spans join the request trace instead of becoming disconnected roots.

**Rules** (same doc): no orphan spans — `dbq::timed` creates its span only under
an existing one, so the workers' once-a-second polls stay spanless; W3C
`traceparent` propagator is installed whenever export is on; SIGTERM/SIGINT
force-flushes the batch processor.

**Log/trace join**: every JSON line's span block carries `trace_id`/`span_id`;
responses echo `x-request-id` (set per request by the request-id middleware).
Pin set: `opentelemetry` 0.33, `opentelemetry_sdk` 0.33, `opentelemetry-otlp`
0.33, `tracing-opentelemetry` 0.34.

## Versioned tile read path

Phase 2 of `docs/scaling-architecture.md`'s read path, paired with the client
half (#188 — contract/zod is the single source of truth).

- `GET /api/versions?bbox=x,y,w,h&z=<zoom>` →
  `{tiles: [{key: "z/x/y", version}]}` for the **rendered** tiles intersecting
  the bbox (version ≥ 1 = `tiles.seq`, the same number the ETag carries;
  never-rendered tiles are absent).
- Tile GETs accept `?v=<held version>`: `v` == current ⇒
  `public, max-age=31536000, immutable` (the bytes are version-pinned); `v`
  older ⇒ current bytes with `public, max-age=60`; `v` newer (client ahead of
  this pod — flip/rollback window) ⇒ `no-store`; absent ⇒ today's
  `max-age=1 + stale-while-revalidate=30`. Old clients send no `v` and see no
  change.
- SSE `/api/events?bbox&z` (both optional — old clients get everything, as
  today): `versions` payloads are coalesced into one event per connection per
  second and trimmed to the subscribed area; each tile's latest version wins.
- The per-pod version map (`versions.rs`) is a lazy read-through over
  `tiles.seq` (the optional `DATABASE_RO_URL` pool serves the lookups when
  configured). Every render write-through notes the new version and publishes it
  on the `events` NOTIFY channel, so every pod's map stays fresh and area
  subscribers learn within the 1s tick. `versions::note` is the seam #190's
  render pipeline plugs into.

## Known risks / limits

- Crash window: chunk rasters are written before the manifest row commits; a
  crash in between leaves orphan pixels that the next moderation replay repairs.
- Stateless logout cannot revoke a presented cookie (accepted; no session store
  by design).
- Concurrent place requests serialize per chunk set — by design, to keep paint
  order == seq order. Serialization is two-tier: in-process per-key locks (cheap
  same-pod) then session-level PG advisory locks on a dedicated connection held
  across the chunk GET→composite→PUT window (`chunks.rs`). Sorted-key
  acquisition keeps multi-chunk rects deadlock-free cross-instance; the lock
  must outlive the transaction because the object PUT is not transactional
  (commit-then-PUT would leave the read-modify-write window open).
- Tile render locks stay in-process only: cross-instance duplicate cold renders
  are benign (same seq ⇒ byte-identical output; last PUT wins with identical
  bytes). Multi-replica is now structurally safe for the write path but still
  needs validation at `replicas: 2` (in-memory tile cache hit rates, pool sizing
  under advisory-lock waits) before raising the replica count.
