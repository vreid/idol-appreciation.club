# Backend architecture

The backend is a single Rust service: HTTP API, tile-pyramid renderer and static
file host. Source lives in `backend/src`, one module per concern:

| module     | responsibility                                         |
| ---------- | ------------------------------------------------------ |
| `world`    | pure tile/chunk/rect geometry (unit + property tested) |
| `config`   | env parsing, cooldown and size knobs                   |
| `img`      | all libvips calls, bytes-in/bytes-out API              |
| `store`    | object storage (S3-compatible; `file://` dev mode)     |
| `chunks`   | chunk raster mutation + moderation replay              |
| `tiles`    | pyramid rendering, maxSeq staleness, caches            |
| `place`    | `POST /api/place`, admin soft-delete pipeline          |
| `api`      | near / center / health / reports / image counts        |
| `handlers` | register / login / logout / me                         |
| `auth`     | HMAC-signed stateless cookies, `Authed` extractor      |
| `events`   | Postgres LISTEN/NOTIFY → broadcast → SSE               |
| `statics`  | SPA hosting with graceful no-frontend fallback         |
| `router`   | route table + body limit + tracing                     |
| `error`    | every failure maps to JSON `{"error": "..."}`          |

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
   single guarded UPDATE — zero rows ⇒ rollback + `429` with `Retry-After`:

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

`POST /api/admin/import` (admin cookie) places many images in one call. SLO: **≥
10 images/second sustained on a single 4-vCPU / 8 GB node** (stretch: 100k
images ≤ 3 h) — against the per-placement path's ~4 s server-side p50 this is
the reason the engine exists. Motivation, measured on prod: pile mode rewrites
the SAME chunk rasters hundreds of times (~100× write amplification: per
placement one chunk GET+PUT set, one PG tx with three statements, one
`pg_notify`, one seq `nextval`).

Shape (single JSON document; `Content-Type: application/json`):

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

Entries reference EXISTING content-addressed objects. Originals get there via
the bulk preupload `POST /api/admin/import/originals` (repeated `image`
multipart fields; each runs the exact /api/place pixel stage — strip, resave,
hash, deterministic derivative — at the source dimensions capped to the canvas
derivative long edge, `IMPORT_DERIVATIVE_LONG_EDGE`, default 1024 ≈ one chunk),
which returns `[{source_sha, derivative_sha, w, h}]` to build the manifest from.
The cap is a measured, deliberate policy: at the sources' native 2–4 MP the
pixel stage wall was ~1.4 img/s of pure decode+rotate across chunk spans; at
1024 the same pipeline clears the SLO with headroom. Originals stay
full-resolution content-addressed objects — only the canvas derivative is
scaled. `derivative_sha` is optional in the manifest: when omitted it resolves
from the newest placement of `source_sha` (one `DISTINCT ON` round trip per
batch); multi-size originals must pass it explicitly. The server verifies
`w`/`h` against the derivative's real dimensions (header-only vips probe, once
per unique sha) — chunk paints and moderation replays composite at the bitmap's
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
5. Each component holds its chunk locks (sorted) across the whole commit — the
   exact HTTP-path invariant — then: ONE seq allocation round trip
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
invalidate through the normal `tiles.seq` staleness path, so no web change is
required.

Tiles: nothing is rendered during ingest — writing no `tiles` row makes affected
tiles stale implicitly (their stored seq falls below the live maxSeq), which IS
the dirty-marking. `prewarm: true` renders z18–z20 tiles for the batch's touched
chunks through the normal serving path strictly after the last commit (21 tiles
per chunk), so prewarmed tiles are indistinguishable from lazily served ones.

Resume/idempotency: re-POST the same manifest with the same `batch_id` —
identical entry ids are skipped, so a killed run resumes without double-placing.
Raster paints are at-least-once (the same exposure window as /api/place: paint
before commit; a kill between PUT and commit loses nothing that a resume doesn't
repaint pixel-identically). The HTTP-path cross-check (concurrent placement
between our seq allocation and commit) is impossible by construction: allocation
happens under the group's locks.

Concurrency model: blocks are strictly sequential (cross-block chunk overlap
would race); within a block, components run on `IMPORT_GROUP_CONCURRENCY`
(default 2) executors. Rationale: libvips work is serialized process-wide
(`img::init`, crash-history driven), so extra executors only overlap S3/PG round
trips with another component's pixel stage while each holds ~4 MiB of decoded
raster — 2 saturates the SLO, more inflates memory without vips throughput.
`VIPS_CONCURRENCY` stays respected-by-subsumption: eval parallelism is pinned to
1 for stability, and the SLO is met without it.

Body/caps: import routes carry a 256 MiB body cap (~1.5 M entries at ~150
B/entry; 100k ≈ 14 MiB). Workloads beyond that chunk client-side:
`scripts/import-bulk.sh` derives each chunk's `batch_id` as
`uuid5("import-bulk:{tag}:{index}")`, so re-running resumes every chunk
independently. Knobs: `IMPORT_BLOCK_ENTRIES` (500), `IMPORT_GROUP_CONCURRENCY`
(2).

Client driver (`import-bulk.sh`):

- **Chunking**: default `--chunk 750`, sized so one POST fits the 3600 s client
  timeout at the observed live rate (1.7 img/s single client, ~2.5 img/s with 4
  concurrent clients — real in-region S3, latency-bound; the node sits at ~0.8/4
  CPUs, so parallel clients scale sublinearly). Chunk size is **fixed per run**:
  it feeds the entry ids, so changing `--chunk` mid-run re-places the same
  coordinates — use a new `--tag`.
- **Retry**: a chunk POST is retried up to 4× with backoff (safe: the server
  commits per block and entry identity is `uuid5(batch_id, entry)`, so a
  timed-out-then-retried chunk can never double-place).
- **Preupload cache**: `(sha256,size) → response map` is kept in
  `/tmp/import-bulk-preupload-cache-<host>.json` (content-addressed, shared
  across tags, keyed per server). Re-runs only upload sources the server has
  never seen — the server dedupes by source sha regardless; the cache saves the
  100-file multipart and ~3.3 s/source of server-side decode per preupload.
  After a full wipe/re-seed the cached derivative objects are gone: delete the
  cache file to force a full re-upload.
- **Live numbers** (production, 51,487 placements, radius 12000): ~2.49 img/s
  sustained over 2 h20m with 4 concurrent clients; single client ~1.7 img/s; the
  wall is per-entry chunk-canvas S3 round-trips + composite serialization, not
  CPU.

Metrics (see Metrics): `import_batches_total`, `import_images_total`,
`import_chunk_writes_total`, `import_duration_seconds`, and the derived gauge
`chunk_write_amplification` (`import_images_total / import_chunk_writes_total`)
— the write-amplification number (the per-placement path is pinned near 1;
amplification grows with pile density — entries per touched chunk — and the
engine preserves that collapse within every block).

Measured (local bench, `scripts/bench-import.sh 2000 12000 2500`, backend
container pinned to 4 CPU / 6 GB, postgres 1 CPU / 2 GB, tmpfs file store, fresh
DB): 2500 images in 513 s → **4.9 images/s sustained**,
`chunk_write_amplification` 1.08 (2500 images / 2318 chunk writes), zero failed
S3 ops, one batch, p≤600 s duration bucket. The 1024-px chunk grid against
≤1024-px derivatives means a uniform-disc pile at radius 12000 covers ~500
chunks (~5 entries/chunk), so amplification near 1 is the honest geometry — pile
densification (smaller radius or higher count at fixed radius) is what moves it,
not an engine knob. The 4.9/s is the LOCAL container rate (laptop CPU shared
with the dev environment; tmpfs storage flatters the S3 leg); the ≥10/s SLO is
decided by the post-wipe 100k run on the real 4-vCPU node (see the reseed
runbook).

## Tile serving and admission control

`GET /api/tiles/{z}/{x}/{y}.webp` (axum 0.8 has no suffix captures, so the last
segment is a wildcard parsed as `<n>.webp`). Resolution order:

1. `maxSeq` over the tile bbox (GiST-backed) = the expected ETag.
2. In-memory cache keyed `(z, x, y, seq)` — `z ≤ 4` never evicts (341 tiles
   max), higher zooms use an LRU (`TILE_CACHE_ENTRIES`, 2048).
3. Object-stored tiles are trusted only if the `tiles` table row's seq matches
   the live maxSeq.
4. Render (leaf: chunk crop/scale; parent: four children — children always
   resolve fresh, a parent render never bakes stale child bytes) and write
   through object + row + memory.

`ETag: "maxSeq"`, `Cache-Control: public, max-age=1, stale-while-revalidate=30`
— four cooperating parts: `max-age=1` stops per-tile revalidation storms during
placement bursts, `stale-while-revalidate` serves tiles instantly while
refreshing them in the background, the SSE-driven client refetch bounds visible
staleness to ~1s past a place/remove commit, and (this change) the SERVER now
stale-serves from stored bytes instead of rendering on the request path; the
ETag keeps every revalidation a cheap 304 while the seq is unchanged. A removal
advances the chunk version past the old max (fresh sequence draw), so the ETag
changes and the affected tiles re-render exactly once.

### Server-side stale-while-revalidate (the pile-wedge fix)

Motivation (prod, 2026-10-13): at a ~700-placement pile, every placement bumped
the seq of every intersecting tile, so tile GETs over the dense area at low zoom
required _fresh renders on the request path_. Four concurrent center-tile
renders (z0/z8/z20) each pinned vips + a `spawn_blocking` thread for 15s+ and
starved the placement pipeline: place latency went 4.3 s → 60 s timeouts while
clients held the map open. Renders were unbounded and competed directly with
placements for CPU and blocking-pool slots.

Rule now: **a GET that has stale bytes available never renders.** When the
stored tile exists but its seq is behind, the handler serves the stored bytes
immediately with the stored seq as ETag plus `X-Tile-Stale: true`, and records
demand for a re-render. Only a first-ever tile (no stored bytes at all) renders
inline — a cold tile must produce something — see _shedding_ below for the flood
exception. Conditional GETs keep working in the stale window: the stored ETag
still 304s (bytes match), so a poller that already has the stale bytes costs
nothing.

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

### The render queue (`tile_render_jobs`)

Demand is persisted in the `tile_render_jobs` table (migration 0005), one row
per tile coord with `state ∈ pending | rendering | done`, `enqueued_at` (the
max-delay anchor) and `last_bump_at` (the quiet anchor):

- **Enqueue** (one CTE roundtrip from a stale GET): pending row → bump
  `last_bump_at` (quiet reset, cap anchor preserved); `done` or absent →
  (re-)insert fresh; `rendering` → no-op (the running render already covers
  everything committed before it started; the next stale GET after it finishes
  re-enqueues).
- **Firing rule per coord: `min(quiet window, max delay)`.**
  `TILE_RENDER_QUIET_SECS` (default 4 s): a tile no stale GET has re-requested
  for this long fires — a burst of placements in one window folds into ONE
  render at the latest seq. `TILE_RENDER_MAX_DELAY_SECS` (default 12 s): hard
  cap since first enqueue, so a tile under _continuous_ bumps (a soak writing
  into one area forever) still renders at a bounded rate instead of starving.
  Tuning: raise the quiet window when placements cluster in bursts and clients
  poll aggressively (more folding); keep `max_delay` ≈ 2–3× the quiet window —
  it doubles as the worst-case staleness bound, and the background render
  duration histogram should sit comfortably under it. A worker crash mid-render
  leaves the row `rendering`; a 60 s lease makes it reclaimable, which is the
  retry (natural backoff — failed renders are not hot-looped).
- **One worker, one executor interface.** `spawn_render_worker` runs the default
  in-process executor (`tiles::RenderExecutor`) — a single task, so at most ONE
  background render runs at a time and renders can never monopolize vips or the
  blocking pool. Pool-sizing math: tokio's blocking pool defaults to 512
  threads; after the fix background renders occupy at most one slot, placement
  composites stay bounded by the chunk locks (in-process keys + PG advisory
  locks, see chunks), and vips eval runs with `concurrency_set(1)` per op (see
  the libvips hardening rules above) — so placement pixel work always has
  headroom; renders go through the queue ONLY, never a placement's path. The
  deployment's `VIPS_CONCURRENCY=2` clamp (#68) bounds threads per pipeline
  env-side; `concurrency_set(1)` overrides it for this service's in-process ops
  and remains the belt to the env's suspenders — the env is the binding cap for
  any future render-role worker that does not call `concurrency_set`. Render
  preemption is deliberately not implemented: a single render's wall-time damage
  is bounded by one worker slot + per-op vips concurrency.
- **Bounds and drop policy.** `TILE_RENDER_QUEUE_CAP` (default 512) bounds
  pending rows. Enqueue attempts beyond the cap are dropped — harmless, the next
  stale GET re-enqueues, and tiles WITH stored bytes are immune (they were
  already served stale on the spot). Tiles WITHOUT stored bytes use the depth as
  a congestion signal: at capacity, a first-ever tile GET is shed with
  `202 Accepted` + `Retry-After: 2` instead of rendering inline, so a
  render-storm flood of unique cold tiles can't turn into an unbounded render
  stampede (`no-store`, so nothing caches the 202). Empty tiles (`maxSeq` 0) are
  a zero-cost constant and are served instantly even under congestion. The cap
  is soft under enqueue races (a pre-statement snapshot count) — it is a flood
  guard, not an accounting invariant.
- **Decoded-raster cache.** Decoding a 1024×1024 chunk webp is the single
  largest per-render cost (~64% of a zoom-20 leaf render), and one chunk feeds
  up to 16 zoom-20 tiles, so the decoded raster is kept in a small LRU keyed by
  the **sha256 of the encoded blob** (`TILE_RASTER_CACHE_ENTRIES`, default 8 — a
  chunk raster is 4 MiB, so a ~32 MiB ceiling). Content addressing is the entire
  invalidation story: a rewritten chunk is a different blob hence a different
  key, so there is no TTL and no staleness window, and a hit is provably the
  pixels a fresh decode would have produced — rendered tile bytes are
  byte-identical to the pre-cache path (unit-tested). The cache is only ever
  touched while holding the process-wide `VIPS` lock. Caching the _encoded_ blob
  keyed `(cx, cy, seq)` is deliberately NOT done: chunks have no version of
  their own, and a placement removal that is not the chunk's max-seq placement
  changes the bytes without moving any seq, which would bake stale pixels under
  a fresh `seq`.
- **Connection-free render walk (issue #120).** A cold z < 18 render over a
  dense pile is a multi-minute quad recursion. Pre-#120 the recursion raced the
  pool (4-way fan-out put the whole subtree's queries in flight) and a render's
  pool footprint spanned minutes — two concurrent low-zoom GETs starved every
  other handler. Now each render takes a **one-shot snapshot** (per-chunk max
  live seq via one grouped GiST query, plus the tiles-table rows for the root
  and lazily, one brief hold per level, for deeper levels) and the walk itself
  is **connection-free**: pure Rust + object storage + pixels, children resolved
  sequentially. A leaf composites ≤4 chunk rasters; a parent composites ≤4 child
  tiles. Snapshot semantics are deliberately not transaction-perfect: a
  placement landing mid-render advances the covering seq, the next GET
  stale-serves + re-enqueues, and the system self-heals. On top of that, a
  **pyramid gate** (`TILE_PYRAMID_ACTIVE`, 2 leases) bounds concurrent
  full-pyramid renders; entry GETs beyond the cap shed with 202 + Retry-After
  instead of stacking another minutes-long walk. Pool doctrine: **a connection
  must never be held across external I/O (S3, vips)** — the audit table lives in
  the PR; the import batch handler is the remaining known offender (#119).
- **Decode-budget boundary.** `MAX_DECODE_PIXELS` (default 64 MPx) is an
  ingestion gate: the placement path probes and decodes uploads against it
  (`img::probe` / `img::decode`) before anything is stored. The raster cache's
  miss path decodes stored blobs with an unbudgeted internal decode
  (`img::decoded_blob`), on purpose: a chunk placed before a budget tightening
  (or under a since-lowered limit) must keep rendering, so reads never enforce
  the budget — only new placements do. The cache therefore never widens
  admission either; it can only re-use pixels that a placement decode already
  admitted, and a cached raster of a historical over-budget chunk serves reads
  exactly like a fresh one.
- **Per-render logs.** One log unit == one render entry (background queue job,
  inline GET render, prewarm tile), never a recursion node. Queue transitions
  log at info: `tile render queued` (fresh (re-)enqueues only — quiet-window
  bumps stay silent, so volume tracks renders, not stale GETs) with the
  post-enqueue `depth`; `tile render claimed` (the worker takes a row, lease
  reclaims included); `tile render done` with `dur_ms`, `outcome` (`rendered` =
  pixel work happened; `fresh_recheck` = the recheck found bytes already fresh —
  mem/S3 hit at the current seq — or nothing left to draw) and `bytes`. Any
  single render over 2 s additionally WARNs: `slow tile render` with `dur_ms`
  and the covering snapshot's bbox-probe `rows` count as the one-field context
  (the 2026-10-14 pile-wedge incident appeared as row explosions). Inline GET
  renders log the same `done` grammar at debug (default `RUST_LOG=info` keeps
  them out of pod logs) and promote to the slow WARN; prewarm tiles only WARN
  when slow.

The table IS the queue so a future dedicated render node group (out-of-process
worker Deployment, same image, render role, cheap 2c/2G nodes scaling 0..9) can
consume the identical claim query — `state` + `FOR UPDATE SKIP LOCKED` is the
entire coordination protocol, no advisory locks, and the read/serve path stays
untouched. To migrate, stop spawning the in-process worker in that deployment
and run the same loop from a render-role binary; multi-writer safety comes from
the claim query, per-tile write races are last-write-wins on identical-content
bytes (the same benignness that already makes cross-instance cold renders safe).

Public flood surfaces, for the record: SSE connections and tile GETs are the
unauthenticated surfaces a flood hits; placement writes stay bounded by the
per-account cooldowns (and admin exemption is an explicit, auditable flag).

## Moderation

`DELETE /api/admin/placements/{id}` (403 for non-admins): soft delete
(`removed_at`), then affected chunks are **re-rendered from the manifest** (non-
removed, seq order, derivatives as sources) inside the same transaction, then a
`remove` NOTIFY fires. Center "current" follows the manifest: removing the top
holder rewinds to the previous covering placement (documented consequence of
manifest-derived history).

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
aggregates only. Hand-rolled (`src/metrics.rs`) instead of the
`metrics`/`prometheus` crates: the series set is small and fixed, so every
series is a pre-created atomic (no registry, no locks on the hot path) and
`render` is a pure formatting walk. Series:

- `place_requests_total`, `place_duration_seconds` histogram — full placement
  pipeline (multipart through NOTIFY), 201s only.
- `tile_requests_total{outcome=hit_mem|hit_s3|render|empty|stale_served|shed}` +
  `tile_duration_seconds{outcome}` histograms — counts every tile resolution
  including parent-tile child fetches, not just HTTP GETs. `stale_served` =
  stale-but-stored bytes served on the spot; `shed` = cold GETs answered `202`
  by the render-queue cap.
- `tile_background_renders_total` + `tile_background_render_duration_seconds` —
  render-queue entries the worker completed (real renders and cheap
  already-fresh rechecks). Keep the duration histogram comfortably under
  `TILE_RENDER_MAX_DELAY_SECS`.
- `tile_render_queue_depth` (gauge, pending rows) /
  `tile_render_queue_dropped_total` (enqueue attempts shed by the cap or raced
  with an in-flight render — the next stale GET retries them).
- `s3_ops_total{op=get|put,ok=true|false}` + `s3_duration_seconds{op}`.
- `chunk_composites_total` — chunk alpha-composites while placing.
- `auth_rate_limited_total` — login attempts rejected by the login limiter.
- `import_batches_total`, `import_images_total`, `import_chunk_writes_total`,
  `import_duration_seconds` histogram (1 s → 3 h buckets) — the batch import
  engine (see Batch import); the derived gauge `chunk_write_amplification` =
  `import_images_total / import_chunk_writes_total`.
- `db_pool_connections{state=idle|in_use}` gauges from sqlx (`num_idle()` /
  `size()`).

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
`TILE_CACHE_ENTRIES=2048`, `TILE_RENDER_QUIET_SECS=4`,
`TILE_RENDER_MAX_DELAY_SECS=12`, `TILE_RENDER_QUEUE_CAP=512`,
`TILE_RASTER_CACHE_ENTRIES=8`, `RUST_LOG=info`, `IMPORT_BLOCK_ENTRIES=500`,
`IMPORT_GROUP_CONCURRENCY=2`, `IMPORT_DERIVATIVE_LONG_EDGE=1024`,
`REGISTRATION_APPROVAL_REQUIRED=false` (set true in production to gate external
signups behind admin approval).

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
cargo run                                       # API on :8080
```

Migrations: plain SQL in `migrations/`, embedded via `sqlx::migrate!`. Gotcha:
the macro does not re-expand on migration-file edits —
`cargo clean -p idol- appreciation-club-backend` after editing one. Comments
inside migrations must not contain `;`.

Tests:

- `cargo test` — unit + property tests (tile math, chunk keys, cooldown
  boundaries incl. 299.99 s vs 300 s, dedupe hashing, pyramid ops).
- `cargo test --test integration` — full HTTP flows against testcontainers
  postgres 16 + filesystem store (requires Docker; the module default is
  postgres 11, so the tests pin 16-alpine for generated columns).
- `backend/scripts/smoke.sh [IMAGE]` — boots postgres + minio + the built image
  and proves register → place → 429 → tiles → center → SSE end to end.
- `backend/scripts/load_tile_swr.sh [N]` — dense-pile load test (default 700
  placements in one area): builds the pile, then hammers z0 while placing
  concurrently; prints phase latency stats and asserts placements stay sub-2s
  and z0 stale-serves at ms latency (the pile-wedge regression test).
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

CI note: fmt/clippy/test all need `libvips-dev` installed on the runner.

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
