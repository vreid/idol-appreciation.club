## Design summary

`POST /api/admin/import` (admin cookie) places many images per call. Against the
per-placement path's ~4 s server-side p50, the engine exists to make bulk
seeding possible on the CURRENT 4-vCPU/8 GB node class (SLO: ≥10 img/s
sustained; stretch 100k ≤ 3 h).

- **Manifest**: one JSON document `{batch_id, prewarm?, entries:[{source_sha,
  derivative_sha?, x, y, w, h, rot}]}` referencing content-addressed objects
  that already exist. Originals arrive via the bulk preupload
  `POST /api/admin/import/originals` (multipart, repeated `image` fields), which
  runs the exact `/api/place` pixel stage (strip → resave → hash → deterministic
  derivative capped to `IMPORT_DERIVATIVE_LONG_EDGE`, default 1024) and returns
  `[{source_sha, derivative_sha, w, h}]`. `derivative_sha` may be omitted in the
  manifest (resolves from the newest placement of `source_sha`); `w`/`h` are
  verified against the derivative's real dimensions (400 on mismatch — chunk
  paints and moderation replays must composite identically).
- **Pipeline** (per sequential 500-entry block): validate+plan → dedupe
  (in-block collapse + committed-entry pre-check) → derivative prefetch (≤8
  concurrent GETs into a 64 MiB cache) → **chunk-disjoint connected components**
  (union-find over shared chunk keys) run in parallel (default 2) under sorted
  chunk-lock sets → ONE seq allocation round trip
  (`nextval FROM generate_series`), chunk pass in 2×2-chunk super-blocks
  (`img::composite_group`: decode+rotate each unique `(sha, rot)` overlay at
  most once per block under a 256 MiB decoded budget; encode each chunk exactly
  once; byte-identical to the moderation path's `replay_canvas`, unit-tested),
  ONE multi-row `INSERT … ON CONFLICT DO NOTHING`, commit.
- **The vips-stripped-sha gotcha**: the server hashes the STRIPPED + RESAVED
  bytes (libvips drops EXIF/metadata and re-encodes), so `source_sha` /
  `derivative_sha` are **not** the sha256 of the raw uploaded file. Manifests
  MUST reference the shas returned by preupload (or `source_sha` resolved
  server-side). A raw-file sha will 400 as unresolvable/missing — by design.
- **Seq semantics**: `placements.seq` is UNIQUE and `ORDER BY seq` is the paint
  order moderation replay depends on, so a literal single shared seq per chunk
  is impossible. "One seq bump per chunk per batch" is implemented as one
  allocation ROUND TRIP per chunk-group (contiguous, ascending in entry order).
- **Tiles**: nothing renders during ingest — no `tiles` row ⇒ affected tiles
  go stale implicitly via `tiles.seq`. Optional `prewarm: true` renders
  z18–z20 (21 tiles/chunk) through the NORMAL serving path strictly
  post-commit, so prewarmed tiles are indistinguishable from lazily served
  ones. SSE: one coarse `import` event per block (type/batch_id/seq/count/
  union bbox); web ignores unknown event types, no web change needed.
- **Metrics**: `import_batches_total`, `import_images_total`,
  `import_chunk_writes_total`, `import_duration_seconds`,
  `chunk_write_amplification` gauge.
- **Reseed plumbing**: `deploy/reseed-job.yaml` — a kubectl-apply (deliberately
  NOT chart-templated: a one-shot ops action tied to a wipe must never be
  re-fired by routine `helm upgrade`; hence no `reseed.enabled` gate either)
  initContainer+Job that clones this repo at a pinned SHA and drives the import
  in-cluster.

Rebased over #70 (stale-serve tiles + render queue): prewarm now resolves via
`Stale::Never` (renders fresh, never sheds — matching the parent-render
path's guarantee).

## Benchmark (local container; post-wipe real-hardware run decides the SLO)

`scripts/bench-import.sh 2000 12000 2500` — throwaway stack on ONE node
budget: backend container `--cpus=4 --memory=6g`, postgres `--cpus=1
--memory=2g`, tmpfs file store, fresh DB. (Count arg yielded 2500 entries —
the last-chunk underflow it exposed is fixed in this PR.)

| metric | value |
|---|---|
| images imported | 2500 (100 uniques, dense pile, ±15° rot) |
| wall time | 513 s (one batch, one 2500-entry block) |
| **sustained rate** | **4.9 img/s** |
| chunk writes | 2318 |
| `chunk_write_amplification` | 1.08 (2500 imgs / 2318 writes) |
| S3 ops | 2417 GET / 2318 PUT, 0 failed |
| duration histogram | single observation in the ≤600 s bucket |

Where the wall is: the pixel stage, not storage or PG. Rotos are
continuous-degree (±15°, 2 decimals), so nearly every entry is a UNIQUE
`(sha, rot)` overlay — decode+rotate runs per entry, then each 2×2
super-block re-encodes its 4 chunk rasters (webp Q90). tmpfs flatters the
S3 leg, and the laptop's shared CPU penalizes the pixel leg, so this is the
local-container rate, not a hardware verdict. **The ≥10/s SLO is decided by
the post-wipe 100k run on the real 4-vCPU node** (runbook below).

Amplification is honest geometry, not an engine knob: `CHUNK_PX` is 1024 and
derivatives are capped to 1024 long edge, so a radius-12000 uniform disc
spreads 2500 entries over ~500 chunks (~5 entries/chunk). Density is what
moves the gauge: the same engine collapses a 21-entry mini-pile to 2 chunk
writes (amp 10.5, integration-tested) and the within-block collapse holds
for any density.

## Resume recipe

Client-side chunking with idempotent resume — re-running the same command
is always safe:

```bash
scripts/import-bulk.sh "$BASE_URL" "$ADMIN_USER" "$ADMIN_PASS" \
  --count 100000 --chunk 2500 --tag reseed-postwipe --prewarm
```

- every chunk's `batch_id` is `uuid5("import-bulk:{tag}:{index}")`; entry ids
  derive from `(batch_id, "sha|sha|x|y|w|h|rot")` via uuid5 — a killed run
  resumes by re-running the SAME command: committed entries are pre-checked
  and skip (reported as `duplicates`), the rest import;
- server-side: a re-POST of an identical manifest is a pure no-op (0 writes,
  no seq churn — integration-tested);
- raster paints are at-least-once (same exposure window as `/api/place`);
  a crash between PUT and commit loses nothing a resume doesn't repaint
  pixel-identically;
- knobs: `IMPORT_BLOCK_ENTRIES` (500), `IMPORT_GROUP_CONCURRENCY` (2),
  `IMPORT_DERIVATIVE_LONG_EDGE` (1024); 256 MiB request cap (~1.5M entries).

## Post-wipe 100k runbook (executable checklist)

The reseed runs as a DEDICATED IN-CLUSTER JOB — not laptop-driven. One-time
edits, then apply/watch/verify:

- [ ] **Pin the source**: in `deploy/reseed-job.yaml` set `SOURCE_SHA` to the
      merge commit of this PR (the clone carries test-assets AND the import
      scripts at that SHA, so manifest rng discipline is pinned too).
- [ ] **Set the credential**: replace the `reseed-admin` Secret placeholders
      (`pw-reseed-admin-user` / `pw-reseed-admin-pw-replace-before-apply`)
      — or skip that block and `kubectl -n idol create secret generic
      reseed-admin --from-literal=username=... --from-literal=password=...`.
      NOTE: first account on a fresh stack is admin — this Job registers it.
- [ ] **Apply** (ConfigMap + Secret + Job, kubeconform-clean):

  ```bash
  kubectl apply -f deploy/reseed-job.yaml
  ```

- [ ] **Watch** (one progress line per chunk: imported/dup/writes/rate/amp):

  ```bash
  kubectl -n idol logs -f job/reseed      # or k9s → :jobs
  ```

- [ ] **Verify** after `==> done`:

  ```bash
  # counters: import_images_total=100000, chunk_write_amplification as measured
  kubectl -n idol exec deploy/backend -- wget -qO- localhost:8080/api/metrics | grep -E '^import_|^chunk_write'
  # cross-zoom tile verify: same ETag chain z18→z20 over a dense region
  #   (fresh renders, no X-Tile-Stale), e.g. around the pile center:
  kubectl -n idol port-forward deploy/backend 8080:8080
  for z in 18 19 20; do curl -sD- -o /tmp/t.webp "http://localhost:8080/api/tiles/$z/32768/32768.webp" | grep -iE '^etag|x-tile-stale'; done
  # and the public canvas: near-placements count sanity at the pile center
  curl -s 'http://localhost:8080/api/placements/near?x=134217728&y=134217728&r=4096' | head -c 300
  ```

- [ ] **Tear down**: `kubectl -n idol delete job reseed` (Job stays for
      inspection until then; no ttl).
- [ ] **Re-fire for themed reseeds**: bump `SOURCE_SHA` + `RESEED_TAG`
      (fresh tag = fresh run; same tag = resume), re-apply. The food batch
      is already in `test-assets/idols`.

Gates: `task backend:ci` green locally (fmt+clippy+unit tests in the pinned
libvips 8.16.1 image); full integration suite (21 tests incl. the four
import tests + advisory locks) and `tile_queue` (5) run green against
real Postgres via testcontainers. **DRAFT + DISARMED: the merge is the
wipe's last gate — do not merge until the fresh stack is up.**
