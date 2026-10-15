//! Phase 3 chunk-and-tile pipeline (docs/scaling-architecture.md,
//! "Chunk and tile pipeline"): a version-driven job queue that brings one
//! chunk up to date and cuts its 21 zoom 18–20 tiles in the same pass,
//! then bubbles versions up the pyramid through parent jobs, so tile
//! requests never render anything.
//!
//! Ships DARK: `RENDER_PIPELINE_ENABLED=false` (the default) keeps the
//! request path painting synchronously (`chunks::composite_placement`,
//! `chunks::replay_chunks`) and never spawns this worker; flipping the
//! value is the entire cutover. While enabled, `place` records the row,
//! advances `chunk_seqs` and queues a chunk job — it never touches
//! libvips — and removals flag `needs_rebuild` and return.
//!
//! Job model (migration 0008): one row per tile in `render_jobs`, PK
//! (z, x, y). z = 18 is a CHUNK job (a chunk IS a zoom 18 tile); z < 18
//! are PARENT jobs rebuilt from their four children's current bytes.
//! Exactly one worker owns a claimed tile (`FOR UPDATE SKIP LOCKED`),
//! which replaces the request path's session advisory locks.
//!
//! Merging rules:
//! - marking dirty is an upsert, so a burst folds into one job;
//! - a RUNNING job is never aborted — a change that arrives meanwhile
//!   sets `rerun` and the job re-queues itself when it finishes (aborting
//!   would starve busy chunks);
//! - parent jobs wait out zoom-dependent `not_before` delays (z ≤ 10 at
//!   most every 10–30 s, z 11–17 after a short quiet window) and claims
//!   order deeper zooms first, so children finish before their parents;
//! - finished rows are deleted; after 5 attempts a job is marked
//!   `failed` instead of retrying forever.
//!
//! Ordering: paint order must equal sequence order, but sequence numbers
//! are drawn inside transactions that can commit out of order. Place
//! transactions hold a single sequencer row lock from the `nextval` draw
//! to commit ([`lock_sequencer`]), so commit order equals sequence order.
//! Safety net: the pipeline chunk bump flags `needs_rebuild` when a
//! placement arrives BELOW the chunk's recorded max_seq (evidence of an
//! out-of-order commit — possible from paths that don't take the lock,
//! e.g. a batch import), forcing a from-blank rebuild whose paint order
//! is the manifest order.

use crate::error::{internal, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{
    chunk_key, tile_key, tile_native_size, tile_offset_in_chunk, BASE_Z, CHUNK_PX, MAX_Z,
};
use sqlx::{Postgres, Transaction};
use std::sync::atomic::{AtomicU64, Ordering};

/// A chunk job is always immediately claimable; only parent jobs wait.
const BASE_Z_I: i16 = BASE_Z as i16;

/// Failed-attempt ceiling (docs: "after 5 attempts a job is marked
/// `failed` instead of retrying forever, so a crash-inducing image can't
/// loop"). Spec-fixed, not configurable.
pub const MAX_JOB_ATTEMPTS: i16 = 5;

/// Transaction-scoped advisory key serializing seq draws to commits (the
/// "single sequencer row lock", docs § Ordering). Own class, disjoint
/// from the chunk locks in `chunks.rs`.
const SEQUENCER_LOCK_KEY: i64 = 0x10F02 << 40;

/// Decoded-overlay byte budget per composite_group call (same order as
/// the import engine's; one chunk per job, so this bounds the decoded
/// working set of one job).
const CHUNK_DECODE_BUDGET: u64 = 256 << 20;

/// Hold from the `nextval('placement_seq')` draw to commit in every
/// pipeline-on place transaction. Released automatically at commit.
pub async fn lock_sequencer(tx: &mut Transaction<'_, Postgres>) -> AppResult<()> {
    crate::dbq::timed(
        "render_jobs",
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(SEQUENCER_LOCK_KEY)
            .execute(&mut **tx),
    )
    .await?;
    Ok(())
}

/// `not_before` delay in ms for a dirty job at (z, x, y): chunk jobs are
/// immediate; z 11–17 wait out a short quiet window; z ≤ 10 fire at most
/// every `low_ms..low_ms+spread_ms` (default 10–30 s), spread
/// deterministically over coordinates so sibling parents don't stampede.
fn job_delay_ms(low_ms: u64, spread_ms: u64, quiet_ms: u64, z: i64, x: i64, y: i64) -> i64 {
    if z >= BASE_Z {
        0
    } else if z <= 10 {
        let step = spread_ms / 20;
        (low_ms + ((x + y).unsigned_abs() % 21) * step) as i64
    } else {
        quiet_ms as i64
    }
}

/// Mark tiles dirty: one upsert per (z, x, y), burst-coalescing. A pending
/// row keeps its (earliest) `not_before`; a running row gets `rerun`
/// (never aborted); a failed row is revived — a fresh change is a fresh
/// job. Runs inside the caller's transaction, AFTER its `chunk_seqs`
/// writes (shared lock order with the job finish path).
pub async fn mark_dirty(
    tx: &mut Transaction<'_, Postgres>,
    jobs: &[(i64, i64, i64, i64)], // (z, x, y, delay_ms)
) -> AppResult<()> {
    if jobs.is_empty() {
        return Ok(());
    }
    let mut zs = Vec::with_capacity(jobs.len());
    let mut xs = Vec::with_capacity(jobs.len());
    let mut ys = Vec::with_capacity(jobs.len());
    let mut delays = Vec::with_capacity(jobs.len());
    for (z, x, y, delay_ms) in jobs {
        zs.push(*z as i16);
        xs.push(*x as i32);
        ys.push(*y as i32);
        delays.push(*delay_ms as f64 / 1000.0);
    }
    crate::dbq::timed(
        "render_jobs",
        sqlx::query(
            "INSERT INTO render_jobs (z, x, y, not_before)
             SELECT z, x, y, now() + make_interval(secs => delay_ms)
             FROM unnest($1::int2[], $2::int4[], $3::int4[], $4::float8[])
                  AS j(z, x, y, delay_ms)
             ON CONFLICT (z, x, y) DO UPDATE SET
               state = CASE WHEN render_jobs.state = 'failed'
                            THEN 'pending' ELSE render_jobs.state END,
               attempts = CASE WHEN render_jobs.state = 'failed'
                               THEN 0 ELSE render_jobs.attempts END,
               claimed_at = CASE WHEN render_jobs.state = 'failed'
                                 THEN NULL ELSE render_jobs.claimed_at END,
               rerun = render_jobs.rerun OR render_jobs.state = 'running',
               not_before = CASE WHEN render_jobs.state = 'running'
                                 THEN render_jobs.not_before
                                 ELSE LEAST(render_jobs.not_before,
                                            EXCLUDED.not_before) END",
        )
        .bind(&zs)
        .bind(&xs)
        .bind(&ys)
        .bind(&delays)
        .execute(&mut **tx),
    )
    .await?;
    Ok(())
}

/// Queue chunk jobs (z = 18) for `chunks` — the place/remove write paths'
/// single entry point. Chunk jobs never wait.
pub async fn enqueue_chunks(
    tx: &mut Transaction<'_, Postgres>,
    chunks: &[(i64, i64)],
) -> AppResult<()> {
    let jobs = chunks
        .iter()
        .map(|&(cx, cy)| (BASE_Z, cx, cy, 0i64))
        .collect::<Vec<_>>();
    mark_dirty(tx, &jobs).await
}

/// Claim the next due job: pending past its `not_before`, plus running
/// rows past the crash lease (a worker died mid-job — the claim is the
/// retry). Deeper zooms first, then FIFO. Exactly one claimer wins a row
/// (`FOR UPDATE SKIP LOCKED`); the claim increments `attempts`.
#[doc(hidden)]
pub async fn claim_due(pool: &sqlx::PgPool, lease_secs: i64) -> AppResult<Option<(i64, i64, i64)>> {
    let row: Option<(i16, i32, i32)> = crate::dbq::timed(
        "render_jobs",
        sqlx::query_as(
            "WITH picked AS (
               SELECT z, x, y FROM render_jobs
               WHERE (state = 'pending' AND not_before <= now())
                  OR (state = 'running'
                      AND claimed_at <= now() - ($1::float8 * interval '1 second'))
               ORDER BY z DESC, not_before, x, y
               LIMIT 1
               FOR UPDATE SKIP LOCKED
             )
             UPDATE render_jobs j
             SET state = 'running', claimed_at = now(), attempts = j.attempts + 1
             FROM picked
             WHERE j.z = picked.z AND j.x = picked.x AND j.y = picked.y
             RETURNING j.z, j.x, j.y",
        )
        .bind(lease_secs as f64)
        .fetch_optional(pool),
    )
    .await?;
    Ok(row.map(|(z, x, y)| (i64::from(z), i64::from(x), i64::from(y))))
}

/// A claimed job failed: retry with one-poll-interval spacing until
/// [`MAX_JOB_ATTEMPTS`], then park the row in `failed` (revived by the
/// next real change via [`mark_dirty`]).
#[doc(hidden)]
pub async fn fail_job(
    pool: &sqlx::PgPool,
    z: i64,
    x: i64,
    y: i64,
    retry_delay_secs: f64,
) -> AppResult<()> {
    crate::dbq::timed(
        "render_jobs",
        sqlx::query(
            "UPDATE render_jobs SET
               state = CASE WHEN attempts >= $5 THEN 'failed' ELSE 'pending' END,
               claimed_at = NULL,
               not_before = CASE WHEN attempts >= $5 THEN not_before
                                 ELSE now() + make_interval(secs => $1) END
             WHERE z = $2 AND x = $3 AND y = $4 AND state = 'running'",
        )
        .bind(retry_delay_secs)
        .bind(z as i16)
        .bind(x as i32)
        .bind(y as i32)
        .bind(MAX_JOB_ATTEMPTS)
        .execute(pool),
    )
    .await?;
    Ok(())
}

// ----------------------------------------------------------- worker ----

/// Test seam (precedent: `tiles::RENDER_PAUSE_MS`): holds every claimed
/// job before its pixel stage so tests get deterministic running windows.
/// Always 0 in production.
static PAINT_PAUSE_MS: AtomicU64 = AtomicU64::new(0);

#[doc(hidden)]
pub fn set_paint_pause_for_tests(ms: u64) {
    PAINT_PAUSE_MS.store(ms, Ordering::Relaxed);
}

async fn pause_for_tests() {
    let pause = PAINT_PAUSE_MS.load(Ordering::Relaxed);
    if pause > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(pause)).await;
    }
}

/// Spawn the pipeline worker. Called once from `build_app`, only when the
/// pipeline is enabled. One task per process: at most one job at a time,
/// mirroring the request path's libvips serialization; horizontal scale
/// comes from more worker processes (claims are SKIP LOCKED).
pub fn spawn_worker(app: &App) {
    let app = app.clone();
    tokio::spawn(async move {
        loop {
            // Queue-depth gauges for the cutover watch (one cheap
            // aggregate per tick; also the worker's liveness signal —
            // frozen gauges = dead worker).
            if let Ok(rows) = sqlx::query_as::<_, (String, i64)>(
                "SELECT state, count(*)::bigint FROM render_jobs GROUP BY state",
            )
            .fetch_all(&app.pool)
            .await
            {
                let get = |s: &str| {
                    rows.iter()
                        .find(|(st, _)| st == s)
                        .map(|(_, n)| *n)
                        .unwrap_or(0)
                };
                crate::metrics::render_jobs_queue(get("pending"), get("running"), get("failed"));
            }
            let claimed = match claim_due(&app.pool, app.cfg.render_pipeline_lease_secs).await {
                Ok(Some((z, x, y))) => {
                    tracing::info!(z, x, y, "render job claimed");
                    let t0 = std::time::Instant::now();
                    let res = match z {
                        BASE_Z => run_chunk_job(&app, x, y).await,
                        _ => run_parent_job(&app, z, x, y).await,
                    };
                    match res {
                        Ok(()) => tracing::info!(
                            z,
                            x,
                            y,
                            dur_ms = t0.elapsed().as_millis() as u64,
                            outcome = "rendered",
                            "render job done"
                        ),
                        Err(err) => {
                            tracing::warn!(z, x, y, %err, "render job failed; will retry");
                            let _ = fail_job(
                                &app.pool,
                                z,
                                x,
                                y,
                                app.cfg.render_pipeline_poll_ms as f64 / 1000.0,
                            )
                            .await;
                        }
                    }
                    continue;
                }
                Ok(None) => None,
                Err(err) => {
                    tracing::error!(%err, "render job claim failed");
                    Some(std::time::Duration::from_secs(2))
                }
            };
            let sleep = claimed
                .unwrap_or(std::time::Duration::from_millis(
                    app.cfg.render_pipeline_poll_ms,
                ))
                .max(std::time::Duration::from_millis(10));
            tokio::time::sleep(sleep).await;
        }
    });
}

/// Execute one claimed job. Test seam ([`claim_due`] is exposed so tests
/// can drive claims deterministically without the spawned worker).
#[doc(hidden)]
pub async fn execute_job(app: &App, z: i64, x: i64, y: i64) -> AppResult<()> {
    match z {
        BASE_Z => run_chunk_job(app, x, y).await,
        _ => run_parent_job(app, z, x, y).await,
    }
}

// -------------------------------------------------------- chunk job ----

/// Bring chunk (cx, cy) up to date and cut its z 18–20 tiles. Pixel work
/// mirrors the import engine: `composite_group` paints the new placements
/// (or, on `needs_rebuild`, EVERY live placement from blank — the
/// removal fix) in sequence order; tiles are cut from the raster already
/// in memory.
async fn run_chunk_job(app: &App, cx: i64, cy: i64) -> AppResult<()> {
    pause_for_tests().await;
    let (max_seq, painted, rebuild_flag): (i64, i64, bool) = crate::dbq::timed(
        "render_jobs",
        sqlx::query_as(
            "SELECT max_seq, painted_seq, needs_rebuild
                 FROM chunk_seqs WHERE cx = $1 AND cy = $2",
        )
        .bind(cx)
        .bind(cy)
        .fetch_optional(&app.pool),
    )
    .await?
    .unwrap_or((0, 0, false));

    // The stored raster is the incremental basis — unless a rebuild is
    // flagged, or the object is gone while painted_seq > 0 (basis lost,
    // e.g. bucket pruning: only a from-blank repaint is sound).
    let basis = if rebuild_flag {
        None
    } else {
        crate::store::get_bytes(&app.store, &chunk_key(cx, cy)).await?
    };
    let rebuild = rebuild_flag || (basis.is_none() && painted > 0);

    let (qx0, qy0) = (cx * CHUNK_PX, cy * CHUNK_PX);
    let (qx1, qy1) = (qx0 + CHUNK_PX - 1, qy0 + CHUNK_PX - 1);
    // Incremental: only placements NEWER than what the raster shows.
    // Rebuild: every live placement, in seq order.
    let rows: Vec<(String, i64, i64, i32, i32, f32, i64)> = crate::dbq::timed(
        "tile_rows",
        sqlx::query_as(
            "SELECT image_key, x, y, w, h, rot, seq FROM placements
             WHERE removed_at IS NULL
               AND bbox && box(point($1::float8, $2::float8),
                               point($3::float8, $4::float8))
               AND ($5 OR seq > $6)
             ORDER BY seq",
        )
        .bind(qx0)
        .bind(qy0)
        .bind(qx1)
        .bind(qy1)
        .bind(rebuild)
        .bind(painted)
        .fetch_all(&app.pool),
    )
    .await?;

    // Version the stored raster and tiles will show: what was painted
    // before, what this pass paints, and — on a rebuild — the chunk's
    // recorded version (a rebuild whose live set is empty, e.g. every
    // image removed, still advances past the removal bump).
    let mut v = painted.max(if rebuild { max_seq } else { 0 });
    let mut overlays: Vec<(Vec<u8>, f32)> = Vec::with_capacity(rows.len());
    let mut members: Vec<img::OverlayPlacement> = Vec::with_capacity(rows.len());
    for (image_key, px, py, w, h, rot, seq) in rows {
        let Some(bytes) = crate::store::get_bytes(&app.store, &image_key).await? else {
            tracing::warn!(key = %image_key, "derivative missing during chunk paint");
            continue;
        };
        // The stored x/y are the UNROTATED rect's top-left; the painted
        // bitmap is the rotated AABB, offset from the chunk corner.
        let aabb = crate::world::rotated_aabb(px, py, i64::from(w), i64::from(h), rot);
        members.push((overlays.len(), aabb.x - qx0, aabb.y - qy0));
        overlays.push((bytes, rot));
        v = v.max(seq);
    }

    let advance = v > painted || rebuild;
    if advance {
        let canvas = if rebuild { None } else { basis };
        let encoded = if overlays.is_empty() {
            // Rebuild to blank (all art removed) or no-op rewrite of the
            // basis (incremental pass with nothing new — only reachable
            // on a rebuild-flagged chunk).
            let empty: [(Vec<u8>, i64, i64, f32); 0] = [];
            crate::telemetry::blocking(move || img::replay_canvas(canvas, &empty))
                .await
                .map_err(internal)?
                .map_err(internal)?
        } else {
            let chunks = vec![(cx, cy, canvas, members)];
            let encoded = crate::telemetry::blocking(move || {
                img::composite_group(&overlays, &chunks, CHUNK_DECODE_BUDGET)
            })
            .await
            .map_err(internal)??;
            encoded
                .into_iter()
                .next()
                .ok_or_else(|| internal("chunk paint output"))?
        };

        // Chunk raster first, then its 21 tiles (z 18–20) cut from the
        // bytes already in memory, then the versioned bookkeeping.
        crate::store::put(&app.store, &chunk_key(cx, cy), encoded.clone()).await?;
        let mut tile_zs = Vec::with_capacity(21);
        let mut tile_xs = Vec::with_capacity(21);
        let mut tile_ys = Vec::with_capacity(21);
        for z in BASE_Z..=MAX_Z {
            let span = 1i64 << (z - BASE_Z);
            for dy in 0..span {
                for dx in 0..span {
                    let (tx, ty) = (cx * span + dx, cy * span + dy);
                    let (ox, oy) = tile_offset_in_chunk(z, tx, ty);
                    let native = tile_native_size(z);
                    let bytes = encoded.clone();
                    let tile = crate::telemetry::blocking(move || {
                        img::render_leaf_tile(Some(bytes), ox, oy, native)
                    })
                    .await
                    .map_err(internal)??;
                    crate::store::put(&app.store, &tile_key(z, tx, ty), tile).await?;
                    tile_zs.push(z as i16);
                    tile_xs.push(tx as i32);
                    tile_ys.push(ty as i32);
                }
            }
        }
        // A chunk job's own rerun requeue never waits (delay 0); the z 17
        // parent marks with its zoom's delay inside finish_chunk.
        finish_chunk(
            app,
            cx,
            cy,
            v,
            true,
            rebuild_flag,
            &tile_zs,
            &tile_xs,
            &tile_ys,
        )
        .await
    } else {
        // Nothing new to paint (painted already == max_seq): bookkeeping
        // only — the parent may still need the bump notification.
        finish_chunk(app, cx, cy, painted, false, false, &[], &[], &[]).await
    }
}

/// Successful chunk-job finish. One transaction, guarded sequential
/// statements — the row lock taken by the first MATCHING statement is
/// held to commit, so a concurrent `mark_dirty` either lands fully before
/// (and is honored by the guards) or fully after (and re-marks a pending
/// or absent row), never into a lost `rerun`:
///
/// 1. rerun → re-queue (pending) — the next pass repaints the delta;
/// 2. guarded `painted_seq` advance (GREATEST, so a slower job can never
///    move the version backwards) and a rebuild-flag clear that only
///    fires when the flag still holds its claim-time value (a removal
///    that landed mid-job survives — the two conditions are independent,
///    so a version race can never strand a rebuild or hot-loop a chunk);
/// 3. flag still standing → re-queue (the demanded rebuild runs next);
/// 4. otherwise delete (finished rows are deleted) — unless a change
///    arrived meanwhile, then re-queue;
/// 5. mark the zoom 17 parent dirty.
///
/// The Phase 2 version seam (docs/backend.md): a rendered version is
/// noted in this pod's map and published as ONE transactional `versions`
/// NOTIFY inside the finish transaction — subscribers learn at commit,
/// other pods' maps follow via `note_payload`.
async fn queue_version_event(
    tx: &mut sqlx::PgConnection,
    written: &[(i64, i64, i64)],
    v: i64,
) -> AppResult<()> {
    if written.is_empty() || v <= 0 {
        return Ok(());
    }
    let tiles: Vec<serde_json::Value> = written
        .iter()
        .map(|(z, x, y)| {
            crate::versions::note(*z, *x, *y, v);
            serde_json::json!({ "key": format!("{z}/{x}/{y}"), "version": v })
        })
        .collect();
    let payload = serde_json::json!({ "type": "versions", "tiles": tiles }).to_string();
    crate::dbq::timed(
        "render_jobs",
        sqlx::query("SELECT pg_notify('events', $1)")
            .bind(payload)
            .execute(&mut *tx),
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn finish_chunk(
    app: &App,
    cx: i64,
    cy: i64,
    v: i64,
    advance: bool,
    rebuild_flag: bool,
    tile_zs: &[i16],
    tile_xs: &[i32],
    tile_ys: &[i32],
) -> AppResult<()> {
    let mut tx = app.pool.begin().await.map_err(internal)?;
    if advance && !tile_zs.is_empty() {
        // Tiles-table staleness rows for the cut tiles: monotone.
        crate::dbq::timed(
            "render_jobs",
            sqlx::query(
                "INSERT INTO tiles (z, x, y, seq)
                 SELECT z, x, y, $4 FROM unnest($1::int2[], $2::int4[], $3::int4[])
                      AS t(z, x, y)
                 ON CONFLICT (z, x, y) DO UPDATE
                 SET seq = GREATEST(tiles.seq, EXCLUDED.seq), rendered_at = now()",
            )
            .bind(tile_zs)
            .bind(tile_xs)
            .bind(tile_ys)
            .bind(v)
            .execute(&mut *tx),
        )
        .await?;
        let written: Vec<(i64, i64, i64)> = tile_zs
            .iter()
            .zip(tile_xs)
            .zip(tile_ys)
            .map(|((z, x), y)| (*z as i64, *x as i64, *y as i64))
            .collect();
        queue_version_event(&mut tx, &written, v).await?;
    }
    // 1. A change arrived while the job ran: re-queue, never abort.
    let requeued: Option<(i32,)> = crate::dbq::timed(
        "render_jobs",
        sqlx::query_as(
            "UPDATE render_jobs
             SET state = 'pending', rerun = false, claimed_at = NULL,
                 attempts = 0, not_before = now()
             WHERE z = $1 AND x = $2 AND y = $3 AND state = 'running' AND rerun
             RETURNING 1",
        )
        .bind(BASE_Z_I)
        .bind(cx as i32)
        .bind(cy as i32)
        .fetch_optional(&mut *tx),
    )
    .await?;
    let mut guard_ok = true;
    if requeued.is_none() && advance {
        // 2. Advance the painted version — `GREATEST`, so a slower job
        // can never move it backwards — and clear the rebuild flag ONLY
        // if it still holds the value seen at claim (a removal that
        // landed mid-job flipped it; its rebuild must run). The two
        // conditions are independent: a rebuild whose painted value is
        // superseded still consumed its flag, so a lost version race can
        // never hot-loop the chunk.
        let row: Option<(bool,)> = crate::dbq::timed(
            "render_jobs",
            sqlx::query_as(
                "UPDATE chunk_seqs
                 SET painted_seq = GREATEST(painted_seq, $3),
                     needs_rebuild = CASE WHEN needs_rebuild = $4
                                          THEN false ELSE needs_rebuild END
                 WHERE cx = $1 AND cy = $2
                 RETURNING needs_rebuild",
            )
            .bind(cx)
            .bind(cy)
            .bind(v)
            .bind(rebuild_flag)
            .fetch_optional(&mut *tx),
        )
        .await?;
        match row {
            // Row missing (no chunk_seqs row at all): nothing to protect.
            None => guard_ok = true,
            // A removal that landed mid-job leaves needs_rebuild true:
            // re-queue so its rebuild runs.
            Some((still_flagged,)) => guard_ok = !still_flagged,
        }
        if !guard_ok {
            // 3. Superseded or re-flagged mid-job: re-queue; the next
            // pass is a cheap catch-up (or the demanded rebuild).
            crate::dbq::timed(
                "render_jobs",
                sqlx::query(
                    "UPDATE render_jobs
                     SET state = 'pending', rerun = false, claimed_at = NULL,
                         not_before = now()
                     WHERE z = $3 AND x = $1 AND y = $2 AND state = 'running'",
                )
                .bind(cx as i32)
                .bind(cy as i32)
                .bind(BASE_Z_I)
                .execute(&mut *tx),
            )
            .await?;
        }
    }
    if requeued.is_none() && guard_ok {
        // 4. Success: delete — unless a change arrived meanwhile
        // (mark_dirty between our claim and here set rerun on the
        // still-running row), then re-queue.
        let deleted = crate::dbq::timed(
            "render_jobs",
            sqlx::query_as::<_, (i64,)>(
                "DELETE FROM render_jobs
                 WHERE z = $3 AND x = $1 AND y = $2
                   AND state = 'running' AND NOT rerun
                 RETURNING 1::bigint",
            )
            .bind(cx as i32)
            .bind(cy as i32)
            .bind(BASE_Z_I)
            .fetch_optional(&mut *tx),
        )
        .await?;
        if deleted.is_none() {
            crate::dbq::timed(
                "render_jobs",
                sqlx::query(
                    "UPDATE render_jobs
                     SET state = 'pending', rerun = false, claimed_at = NULL,
                         attempts = 0, not_before = now()
                     WHERE z = $3 AND x = $1 AND y = $2
                       AND state = 'running' AND rerun",
                )
                .bind(cx as i32)
                .bind(cy as i32)
                .bind(BASE_Z_I)
                .execute(&mut *tx),
            )
            .await?;
        }
    }
    // 5. Bubble the version up (zoom 17 parent waits out its quiet
    // window; chunk rows are immediate).
    mark_dirty(
        &mut tx,
        &[(
            BASE_Z - 1,
            cx.div_euclid(2),
            cy.div_euclid(2),
            job_delay_ms(
                app.cfg.render_pipeline_parent_low_ms,
                app.cfg.render_pipeline_parent_spread_ms,
                app.cfg.render_pipeline_parent_quiet_ms,
                BASE_Z - 1,
                cx.div_euclid(2),
                cy.div_euclid(2),
            ),
        )],
    )
    .await?;
    tx.commit().await.map_err(internal)?;
    Ok(())
}

// ------------------------------------------------------- parent job ----

/// Rebuild tile (z < 18) from its four children's CURRENT bytes; its
/// version is the highest child version, which only grows. Children are
/// claimed first (deeper zooms win) and delay longer, so their bytes are
/// expected to exist; a missing child means a deeper job is still pending
/// or failed — the error path retries, then parks the row failed.
async fn run_parent_job(app: &App, z: i64, x: i64, y: i64) -> AppResult<()> {
    pause_for_tests().await;
    let mut kids: Vec<Vec<u8>> = Vec::with_capacity(4);
    let mut v = 0i64;
    for (dx, dy) in [(0i64, 0i64), (1, 0), (0, 1), (1, 1)] {
        let (cx, cy) = (2 * x + dx, 2 * y + dy);
        let seq: Option<(i64,)> = crate::dbq::timed(
            "render_jobs",
            sqlx::query_as("SELECT seq FROM tiles WHERE z = $1 AND x = $2 AND y = $3")
                .bind((z + 1) as i16)
                .bind(cx as i32)
                .bind(cy as i32)
                .fetch_optional(&app.pool),
        )
        .await?;
        let bytes = crate::store::get_bytes(&app.store, &tile_key(z + 1, cx, cy)).await?;
        // A child with no row and no bytes is the read path's constant
        // empty tile (a chunk no job ever touched). Children DO claim
        // first, so an in-flight deeper job is not a concern here; if one
        // races anyway, its finish re-marks this parent and the next pass
        // bakes the real bytes.
        let (seq, bytes) = match (seq, bytes) {
            (Some((s,)), Some(b)) => (s, b),
            _ => (0, img::empty_tile_256().map_err(internal)?),
        };
        kids.push(bytes);
        v = v.max(seq);
    }
    let kids: [Vec<u8>; 4] = kids.try_into().map_err(|_| internal("children count"))?;
    let bytes = crate::telemetry::blocking(move || img::render_parent_tile(&kids))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    crate::store::put(&app.store, &tile_key(z, x, y), bytes).await?;
    finish_parent(app, z, x, y, v).await
}

/// Successful parent-job finish: monotone staleness row (its version is
/// the highest child version, which only grows), rerun-aware bookkeeping
/// (same guarded sequence as [`finish_chunk`]), own parent dirty (z > 0).
/// A rerun re-queue waits out the job's own zoom delay, keeping the
/// z ≤ 10 "at most every 10–30 s" bound.
#[doc(hidden)]
pub async fn finish_parent(app: &App, z: i64, x: i64, y: i64, v: i64) -> AppResult<()> {
    let own_delay = job_delay_ms(
        app.cfg.render_pipeline_parent_low_ms,
        app.cfg.render_pipeline_parent_spread_ms,
        app.cfg.render_pipeline_parent_quiet_ms,
        z,
        x,
        y,
    );
    let parent_delay = job_delay_ms(
        app.cfg.render_pipeline_parent_low_ms,
        app.cfg.render_pipeline_parent_spread_ms,
        app.cfg.render_pipeline_parent_quiet_ms,
        z - 1,
        x.div_euclid(2),
        y.div_euclid(2),
    );
    let mut tx = app.pool.begin().await.map_err(internal)?;
    crate::dbq::timed(
        "render_jobs",
        sqlx::query(
            "INSERT INTO tiles (z, x, y, seq) VALUES ($1, $2, $3, $4)
             ON CONFLICT (z, x, y) DO UPDATE
             SET seq = GREATEST(tiles.seq, EXCLUDED.seq), rendered_at = now()",
        )
        .bind(z as i16)
        .bind(x as i32)
        .bind(y as i32)
        .bind(v)
        .execute(&mut *tx),
    )
    .await?;
    queue_version_event(&mut tx, &[(z, x, y)], v).await?;
    // A change arrived while the job ran: re-queue, never abort.
    let requeued: Option<(i32,)> = crate::dbq::timed(
        "render_jobs",
        sqlx::query_as(
            "UPDATE render_jobs
             SET state = 'pending', rerun = false, claimed_at = NULL,
                 attempts = 0, not_before = now() + make_interval(secs => $4)
             WHERE z = $1 AND x = $2 AND y = $3 AND state = 'running' AND rerun
             RETURNING 1",
        )
        .bind(z as i16)
        .bind(x as i32)
        .bind(y as i32)
        .bind(own_delay as f64 / 1000.0)
        .fetch_optional(&mut *tx),
    )
    .await?;
    if requeued.is_none() {
        let deleted = crate::dbq::timed(
            "render_jobs",
            sqlx::query_as::<_, (i64,)>(
                "DELETE FROM render_jobs
                 WHERE z = $1 AND x = $2 AND y = $3
                   AND state = 'running' AND NOT rerun
                 RETURNING 1::bigint",
            )
            .bind(z as i16)
            .bind(x as i32)
            .bind(y as i32)
            .fetch_optional(&mut *tx),
        )
        .await?;
        if deleted.is_none() {
            // A change arrived between the claim and here: re-queue for
            // the delta instead of deleting.
            crate::dbq::timed(
                "render_jobs",
                sqlx::query(
                    "UPDATE render_jobs
                     SET state = 'pending', rerun = false, claimed_at = NULL,
                         attempts = 0, not_before = now() + make_interval(secs => $4)
                     WHERE z = $1 AND x = $2 AND y = $3
                       AND state = 'running' AND rerun",
                )
                .bind(z as i16)
                .bind(x as i32)
                .bind(y as i32)
                .bind(own_delay as f64 / 1000.0)
                .execute(&mut *tx),
            )
            .await?;
        }
    }
    if z > 0 {
        mark_dirty(
            &mut tx,
            &[(z - 1, x.div_euclid(2), y.div_euclid(2), parent_delay)],
        )
        .await?;
    }
    tx.commit().await.map_err(internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delay(low: u64, spread: u64, quiet: u64, z: i64, x: i64, y: i64) -> i64 {
        job_delay_ms(low, spread, quiet, z, x, y)
    }

    #[test]
    fn chunk_jobs_never_wait() {
        assert_eq!(delay(10_000, 20_000, 3_000, BASE_Z, 5, 9), 0);
        assert_eq!(delay(10_000, 20_000, 3_000, MAX_Z, 0, 0), 0);
    }

    #[test]
    fn low_zoom_delay_spreads_across_ten_to_thirty_seconds() {
        let mut min = i64::MAX;
        let mut max = 0i64;
        for x in 0..64i64 {
            for y in 0..64i64 {
                let d = delay(10_000, 20_000, 3_000, 7, x, y);
                min = min.min(d);
                max = max.max(d);
                assert!(
                    (10_000..=30_000).contains(&d),
                    "z<=10 delay out of doc range: {d}"
                );
            }
        }
        assert_eq!(min, 10_000);
        assert_eq!(max, 30_000);
    }

    #[test]
    fn mid_zoom_delay_is_the_quiet_window() {
        assert_eq!(delay(10_000, 20_000, 3_000, 11, 0, 0), 3_000);
        assert_eq!(delay(10_000, 20_000, 3_000, 17, 123, 45), 3_000);
    }

    #[test]
    fn zero_knobs_zero_every_parent_delay() {
        for z in 0..BASE_Z {
            assert_eq!(delay(0, 0, 0, z, 3, 7), 0);
        }
    }
}
