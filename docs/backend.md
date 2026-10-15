# Backend architecture

The backend is a Cargo workspace of three crates: `iac-core` (shared
config/db/queue/world), `iac-api` (the HTTP service) and `iac-worker` (the
render/ingest loops, the only code that links libvips). Source lives in
`backend/crates`, one module per concern:

| crate / module        | responsibility                                         |
| --------------------- | ------------------------------------------------------ |
| `core::world`         | pure tile/chunk/rect geometry (unit + property tested) |
| `core::config`        | env parsing, cooldown and size knobs                   |
| `core::store`         | object storage (S3-compatible; `file://` dev mode)     |
| `core::queue`         | render_jobs/chunk_seqs write paths shared by both bins |
| `core::dbq`           | `timed()` DB-latency families + shared queries         |
| `worker::img`         | all libvips calls, bytes-in/bytes-out API              |
| `worker::chunks`      | chunk raster mutation + moderation replay              |
| `worker::render_jobs` | the claim loop: chunk paints + parent bubble-up        |
| `worker::ingest`      | quarantine processor: decode, hash, record, enqueue    |
| `api::tiles`          | versioned serving, maxSeq staleness, caches            |
| `api::place`          | `POST /api/place` (202 quarantine), admin soft-delete  |
| `api::api`            | near / center / health / reports / image counts        |
| `api::handlers`       | register / login / logout / me                         |
| `api::auth`           | HMAC-signed stateless cookies, `Authed` extractor      |
| `api::events`         | Postgres LISTEN/NOTIFY → broadcast → SSE               |
| `api::statics`        | SPA hosting with graceful no-frontend fallback         |
| `api::router`         | route table + body limit + tracing                     |
| `core::error`         | every failure maps to JSON `{"error": "..."}`          |

> NOTE (Phase 3/4 cutover, docs/scaling-architecture.md): the module table above
> is the POST-split layout; the prose below still describes pieces of the
> pre-split request-path renderer where marked. Sections marked **[pipeline]**
> describe the new render pipeline and its staged cutover.

## The render pipeline **[pipeline]**

Placement pixel work lives in a version-driven background pipeline
(docs/scaling-architecture.md "Chunk and tile pipeline"); the API never links
libvips. The flow for every placement:

1. `POST /api/place` validates, cooldown-checks, stores the upload under
   `quarantine/{uuid}` and inserts an `ingest_jobs` row — all in one transaction
   — and returns **202** `{id, state:"pending", x, y, w, h, rot}` (the AABB the
   art will occupy). The client resolves the upload through the
   `placed`/`place_failed` events or `GET /api/uploads/{id}` (owner-scoped poll
   fallback: `{state:"pending"}` → `{state:"placed", id, seq, bbox}` →
   `{state:"failed", attempts, permanent, error}`; 404 for other accounts' or
   unknown ids).
2. The **ingest worker** claims the job (`FOR UPDATE SKIP LOCKED`), decodes with
   libvips (authoritative header check — a disagreement with the API's pure-Rust
   probe fails the job), strips and re-hashes into `originals/{sha}`, builds the
   derivative into `derivatives/{sha}.webp`, then in one transaction: inserts
   the placement rows **under the sequencer advisory lock**
   (`queue::lock_sequencer`, docs § Ordering), advances `chunk_seqs` (a
   committed-below-`painted_seq` placement sets `needs_rebuild`), enqueues chunk
   jobs, notifies `placed` over the event stream and deletes the quarantine
   object.
3. The **render worker** claims due jobs from `render_jobs` (PK (z,x,y), states
   pending/running/failed, `rerun` flag, `not_before` delay, `claimed_at` lease,
   `attempts` ceiling of 5 → `failed`). A chunk job (z=18) repaints from blank
   when `needs_rebuild` (import batches and removals), else paints placements
   with `seq > painted_seq`; it cuts the z18/z19/z20 tiles in the same pass and
   writes each with the `WHERE painted_seq < $version` guard. Parent jobs (z<18)
   rebuild from the four children's current bytes, version = max child, and mark
   their own parent dirty.
4. Merging rules (docs): dirty-marking is an upsert — one row per tile; a
   running job is never aborted (the change sets `rerun` and the pass re-queues
   itself on finish); z≤10 parents wait `10–30 s` (coordinate-spread,
   anti-stampede), z11–17 wait a short quiet window; finished rows are deleted.

The API serves tiles from object storage with the versioned cache contract
(`tiles` table seq ↔ `x-tile-version`); a stale GET enqueues a job (best-effort,
non-transactional) and serves the stored bytes with `x-tile-stale: true`.

### Staged cutover (env, `RENDER_PIPELINE_*`)

| stage          | `RENDER_PIPELINE_ENABLED` | `RENDER_PIPELINE_WORKERS` | effect                                                                       |
| -------------- | ------------------------- | ------------------------- | ---------------------------------------------------------------------------- |
| dark (default) | `false`                   | `0`                       | workers quiesced; place still 202s and the ingest/queue rows simply wait     |
| stage 2        | `false`                   | `2`                       | workers burn the seeded backlog while the request path is already queue-only |
| stage 3        | `true`                    | `2`                       | full pipeline (the flag now only gates internal asserts + telemetry naming)  |

The chart ships stage 2/3 as values (`backend.renderPipelineEnabled`,
`backend.renderPipelineWorkers`); the worker Deployment
(`chart/templates/worker-deployment.yaml`) runs the same image as the API with
`command: /usr/local/bin/worker`, and KEDA (`worker-scaledobject.yaml`) scales
it 0..4 from `render_jobs` depth once `worker.autoscaling.enabled`.

### Rollout safety

- **Dark contract:** with the flag off, a place mutates no pixels, paints no
  chunks and enqueues nothing (enqueue rides the ingest record transaction) —
  `pipeline_disabled_keeps_request_path_unchanged` pins this byte-for-byte.
- **Ordering:** the sequencer-row-lock batch insert (docs § Ordering) is
  `queue::lock_sequencer` + `bump_chunk_seqs_pipeline`; a chunk job that finds a
  committed `seq ≤ painted_seq` repaints from blank instead of trusting the
  raster.
- **Removal:** admin delete sets `removed_at`, advances the chunk version to a
  fresh sequence value, sets `needs_rebuild` and enqueues in the same
  transaction, returns 204; the rebuild replays the remaining live set in seq
  order (ghost-free pixels are asserted by decode in
  `removal_rebuild_renders_ghost_free_pixels`).
- **Crash safety:** claims lease via `claimed_at` + `RENDER_PIPELINE_LEASE_SECS`
  (120 s default); a dead worker's job is re-claimable after the lease; 5 failed
  attempts park the row in `failed` (a fresh change revives it).
