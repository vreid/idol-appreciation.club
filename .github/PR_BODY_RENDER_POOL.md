## What

Kills the render-path pool starvation (issue #120). A cold z < 18 render over
the dense pile is a multi-minute quad recursion whose 4-way `try_join_all`
fan-out put the whole subtree's SQL in flight against the pool — a render's pool
footprint spanned minutes, and two concurrent low-zoom GETs starved every
handler.

- **One-shot snapshot (`TileCtx`)**: ONE grouped per-chunk max-seq query over
  the GiST index (`CROSS JOIN LATERAL` chunk-span expansion), the root tile's
  stored row, and lazy per-level tiles-table snapshots — then the connection is
  released. The walk is **connection-free**: pure Rust + object storage +
  pixels, children resolved sequentially (≤1 query in flight per render). A leaf
  composites ≤4 chunk rasters; a parent composites ≤4 child tiles.
- **Snapshot semantics are deliberately not transaction-perfect**: snapshot
  seqs, render, store at snapshot seq; a placement landing mid-render advances
  the covering seq and the next GET stale-serves + re-enqueues (self-healing).
- **Pyramid gate**: 2 leases bound concurrent full-pyramid renders; entry GETs
  beyond the cap shed 202 + Retry-After instead of stacking another walk. The
  background worker is serial and ungated. Metrics: `tile_pyramid_active`,
  `tile_pyramid_sheds_total`, `shed` outcome in `tile_requests_total`.

## Production evidence (2h window, both replicas)

| slow-statement pattern                                                                      | count | min   | p50       | max   | total     |
| ------------------------------------------------------------------------------------------- | ----- | ----- | --------- | ----- | --------- |
| `SELECT COALESCE(max(seq),0) FROM placements WHERE removed_at IS NULL AND bbox && box(...)` | 1362  | 1.00s | **1.27s** | 2.31s | **1770s** |

The old path ran this per GET (and per walk node). The rewrite collapses it to
exactly **one** grouped statement per GET, run once before any pixel work, with
the connection released before the walk. The residual 1×/GET wide-box cost is
the known floor; the follow-up (maintained per-chunk seq rollup written at
place/import time — noted in docs) removes it entirely.

## Regression test (the incident, as a gate)

`import_pile_low_zoom_never_starves_placements` (tile_queue suite): imports a
**4096-entry pile** through the batch endpoint (all 64 z18 chunks of a z15
tile), fires a low-zoom GET, overlaps it with 8 paced placements — **placements
stay ≤5s (baseline ~ms)**, the render completes ≤120s, and the warm re-GET is
instant. On the pre-fix path the placements queue behind the walk's pool holds
and blow the assertion. Full suite: tile_queue 7/7, lib 65/65, fmt + clippy
`-D warnings` clean.

## Scope

Deliberately minimal: this is the starvation fix. Follow-ups queued immediately
after: (1) backend Dockerfile `COPY tests/` so CI actually runs the container
suites (they can't today — the gate image never saw `tests/`), (2) MinIO
testcontainer for a real S3 leg in the harness, (3) structural purity invariant
(pg_stat_statements-counted) + doctrine docs.
