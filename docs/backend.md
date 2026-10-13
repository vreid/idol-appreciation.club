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
ETag keeps every revalidation a cheap 304 while the seq is unchanged. Removed
art lowers maxSeq ⇒ the ETag drops back and content re-renders from chunks.

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

`GET /api/dig?x=&y=` (auth-free read) returns the wheel-dig stack: every
placement whose **rotated** rect contains the point, newest-first —
`{ stack: [{depth, id, username, x, y, w, h, rot, seq, created_at}], total, truncated }`,
depth 1 = topmost. Candidates come from the GiST bbox (`bbox @> point`),
membership is refined with the exact rotated hit-test
(`world::rotated_contains`), so an AABB corner hit that misses the rotated
pixels does not dig up. The stack is capped at 64 entries (`truncated: true`
reports a cut stack; `total` is the full size — the UI's "3 of 12"). The ETag is
the global `placement_seq` counter — the generator the tile-read staleness
marker derives from — so a matching If-None-Match is a cheap 304 and any new
placement invalidates every outstanding stack; removals surface via the SSE
`remove` event (a removed middle stratum does not move the seq).

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

## Configuration

Required: `DATABASE_URL`, `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY_ID`,
`S3_SECRET_ACCESS_KEY`, `SESSION_SECRET` (≥ 32 bytes). Optional with defaults:
`LISTEN_ADDR=0.0.0.0:8080`, `PUBLIC_BASE_URL`, `STATIC_DIR=../web/dist`,
`COOLDOWN_PAINT_SECS=300`, `COOLDOWN_EMPTY_SECS=1`, `MAX_UPLOAD_BYTES=10485760`,
`MIN_SIZE=16`, `MAX_SIZE=4096`, `TILE_CACHE_ENTRIES=2048`,
`TILE_RENDER_QUIET_SECS=4`, `TILE_RENDER_MAX_DELAY_SECS=12`,
`TILE_RENDER_QUEUE_CAP=512`, `RUST_LOG=info`,
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
