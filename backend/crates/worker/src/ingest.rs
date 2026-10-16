//! Phase 4 upload flow (docs/scaling-architecture.md, "Upload flow"):
//! the ingest consumer behind the 202 quarantine path.
//!
//! POST /api/place (and admin DELETE) validate cheaply, stash the upload
//! under `quarantine/{job_id}` and insert an `ingest_jobs` row; this
//! consumer claims rows `FOR UPDATE SKIP LOCKED` (one in-process task;
//! horizontal scale comes from more processes, mirroring render_jobs),
//! runs the pixel stage via the shared [`crate::pixel::process_original`]
//! engine and records the placement under the sequencer lock so commit
//! order equals sequence order — then advances chunk versions, queues
//! render jobs and notifies `placed`/`place_failed` on the SSE stream.
//!
//! Failure doctrine: validation-class errors (the client lied about the
//! bytes, or libvips disagrees with the API's header read) fail the job
//! permanently on the first attempt and refund the cooldown; everything
//! else retries with a poll-interval backoff up to
//! [`MAX_INGEST_ATTEMPTS`], then fails permanently.

use crate::ctx::WorkerCtx;
use iac_core::error::{internal, AppError, AppResult};
use object_store::ObjectStoreExt;
use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

/// Failed-attempt ceiling (mirrors render_jobs MAX_JOB_ATTEMPTS).
pub const MAX_INGEST_ATTEMPTS: i16 = 5;

/// The claimed job's full payload (everything the executor needs without
/// re-reading the row).
#[derive(Clone)]
pub struct IngestJob {
    pub id: Uuid,
    pub kind: String,
    pub account_id: Option<Uuid>,
    pub placement_id: Option<Uuid>,
    pub quarantine_key: Option<String>,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub rot: f32,
    pub size: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}





/// Claim the oldest due job (`FOR UPDATE SKIP LOCKED`, render_jobs
/// idiom): pending rows past `not_before`, plus `running` rows whose
/// lease expired (a crashed consumer's leftovers). The claim increments
/// `attempts`.
pub async fn claim_due(pool: &sqlx::PgPool, lease_secs: i64) -> AppResult<Option<IngestJob>> {
    let row = sqlx::query(
        "WITH next AS (
           SELECT id FROM ingest_jobs
           WHERE (state = 'pending' AND not_before <= now())
              OR (state = 'running'
                  AND claimed_at < now() - make_interval(secs => $1))
           ORDER BY created_at
           FOR UPDATE SKIP LOCKED
           LIMIT 1
         )
         UPDATE ingest_jobs j
         SET state = 'running', claimed_at = now(), attempts = j.attempts + 1
         FROM next
         WHERE j.id = next.id
         RETURNING j.id, j.kind, j.account_id, j.placement_id, j.quarantine_key,
                   j.x, j.y, j.w::bigint AS w, j.h::bigint AS h, j.rot,
                   j.size::bigint AS size, j.created_at",
    )
    .bind(lease_secs)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(IngestJob {
        id: row.get("id"),
        kind: row.get("kind"),
        account_id: row.get("account_id"),
        placement_id: row.get("placement_id"),
        quarantine_key: row.get("quarantine_key"),
        x: row.get("x"),
        y: row.get("y"),
        w: row.get("w"),
        h: row.get("h"),
        rot: row.get("rot"),
        size: row.get("size"),
        created_at: row.get("created_at"),
    }))
}

/// Validation-class failure: no retry, refund the cooldown. The pixel
/// stage's Vips errors are properties of the fixed bytes (decode,
/// strip, resize — pure CPU, no retry can succeed), so they are
/// permanent too; only transport-level failures (store, DB) retry.
pub fn is_permanent(err: &AppError) -> bool {
    matches!(
        err,
        AppError::BadRequest(_) | AppError::TooLarge | AppError::NotFound | AppError::Vips(_)
    )
}

/// Record the outcome of a failed attempt. Permanent (or ceiling) →
/// `failed` + `place_failed` notify + cooldown refund; otherwise back to
/// `pending` with a backoff.
pub async fn fail_job(
    ctx: &WorkerCtx,
    job: &IngestJob,
    err: &AppError,
    backoff_secs: f64,
) -> AppResult<()> {
    let permanent = is_permanent(err);
    let row = sqlx::query(
        "UPDATE ingest_jobs SET
           state = CASE WHEN $2 OR attempts >= $3
                        THEN 'failed' ELSE 'pending' END,
           not_before = CASE WHEN $2 OR attempts >= $3
                             THEN not_before
                             ELSE now() + make_interval(secs => $4) END,
           error = $5
         WHERE id = $1 AND state = 'running'
         RETURNING attempts, state",
    )
    .bind(job.id)
    .bind(permanent)
    .bind(MAX_INGEST_ATTEMPTS)
    .bind(backoff_secs)
    .bind(err.to_string())
    .fetch_one(&ctx.pool)
    .await
    .map_err(internal)?;
    let attempts: i16 = row.get("attempts");
    let state: String = row.get("state");
    if state != "failed" {
        return Ok(());
    }
    // Refund the cooldown the 202 consumed — but only if no later
    // placement has already moved it (this upload's throttle is the one
    // being refunded, not a newer placement's).
    if let Some(account_id) = job.account_id {
        if job.kind == "place" {
            sqlx::query(
                "UPDATE accounts SET last_placement_at = NULL
                 WHERE id = $1
                   AND last_placement_at IS NOT NULL
                   AND last_placement_at >= $2",
            )
            .bind(account_id)
            .bind(job.created_at)
            .execute(&ctx.pool)
            .await
            .map_err(internal)?;
        }
    }
    if job.kind == "place" {
        let event = json!({
            "type": "place_failed",
            "job": job.id,
            "attempts": attempts as i64,
            "permanent": true,
            "error": err.to_string(),
        })
        .to_string();
        sqlx::query("SELECT pg_notify('events', $1)")
            .bind(&event)
            .execute(&ctx.pool)
            .await
            .map_err(internal)?;
    }
    Ok(())
}

/// Execute one claimed job. Test seam: [`claim_due`] is exposed so tests
/// drive claims deterministically without the spawned consumer.
#[doc(hidden)]
pub async fn execute_job(ctx: &WorkerCtx, job: IngestJob) -> AppResult<()> {
    match job.kind.as_str() {
        "removal" => run_removal_job(ctx, &job).await,
        _ => run_place_job(ctx, &job).await,
    }
}

/// Quarantine placement: pixel stage → content-addressed originals and
/// derivative → record under the sequencer lock → advance chunks →
/// queue render jobs → notify `placed`.
async fn run_place_job(ctx: &WorkerCtx, job: &IngestJob) -> AppResult<()> {
    let quarantine_key = job
        .quarantine_key
        .clone()
        .ok_or_else(|| AppError::BadRequest("job has no quarantine object"))?;
    let size = job
        .size
        .ok_or_else(|| AppError::BadRequest("job has no derivative size"))?;
    let bytes = iac_core::store::get_bytes(&ctx.store, &quarantine_key)
        .await?
        .ok_or(AppError::NotFound)?;

    // Shared pixel engine (decode, strip, re-save, hash, derivative).
    // The stage is pure CPU over fixed bytes, so its failures (the Vips
    // variant below) are properties of the upload: retrying cannot
    // succeed. The API's header read fixed the accepted rect; if libvips
    // disagrees with it, the job fails (docs "Upload flow").
    let max_pixels = ctx.cfg.max_decode_pixels;
    let pipeline = crate::pixel::process_original(
        bytes,
        Some(size),
        ctx.cfg.min_size,
        ctx.cfg.max_size,
        ctx.cfg.max_size,
        max_pixels,
    )
    .await?;
    if pipeline.w != job.w || pipeline.h != job.h {
        return Err(AppError::BadRequest(
            "header and decoded dimensions disagree",
        ));
    }

    let rect = iac_core::world::Rect::new(job.x, job.y, pipeline.w, pipeline.h);
    let aabb = iac_core::world::rotated_aabb(rect.x, rect.y, rect.w, rect.h, job.rot);
    if aabb.x < 0
        || aabb.y < 0
        || aabb.x + aabb.w > iac_core::world::WORLD_PX
        || aabb.y + aabb.h > iac_core::world::WORLD_PX
    {
        return Err(AppError::BadRequest("rect out of world bounds"));
    }

    // Content-addressed objects (dedupe across identical uploads).
    let original_key = format!(
        "originals/{}.{}",
        pipeline.original_sha,
        pipeline.format.ext()
    );
    let image_key = format!("derivatives/{}.webp", pipeline.derivative_sha);
    iac_core::store::put_if_absent(&ctx.store, &image_key, pipeline.derivative.clone()).await?;
    iac_core::store::put_if_absent(&ctx.store, &original_key, pipeline.stripped.clone()).await?;

    let account_id = job
        .account_id
        .ok_or_else(|| AppError::BadRequest("place job has no account"))?;
    let id = Uuid::now_v7();
    let chunk_keys = aabb.chunk_keys();
    let t0 = std::time::Instant::now();

    let mut tx = ctx.pool.begin().await?;
    // Ordering doctrine (docs "Ordering"): the sequencer row lock is held
    // from the seq draw to commit, so commit order equals sequence order
    // and the render workers' incremental painting sees a consistent
    // world.
    iac_core::jobs::lock_sequencer(&mut tx).await?;
    let seq: (i64,) = iac_core::dbq::timed(
        "ingest",
        sqlx::query_as("SELECT nextval('placement_seq')").fetch_one(&mut *tx),
    )
    .await?;
    let duplicate_count: (i64,) = iac_core::dbq::timed(
        "ingest",
        sqlx::query_as(
            "SELECT count(*) FROM placements
             WHERE original_sha = $1 AND removed_at IS NULL",
        )
        .bind(&pipeline.original_sha)
        .fetch_one(&mut *tx),
    )
    .await?;
    iac_core::dbq::timed(
        "ingest",
        sqlx::query(
            "INSERT INTO placements
             (id, account_id, image_key, x, y, w, h, seq, rot,
              bbox, original_sha, derivative_sha)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                     box(point($10::float8, $11::float8),
                         point($12::float8, $13::float8)), $14, $15)",
        )
        .bind(id)
        .bind(account_id)
        .bind(&image_key)
        .bind(rect.x)
        .bind(rect.y)
        .bind(rect.w as i32)
        .bind(rect.h as i32)
        .bind(seq.0)
        .bind(job.rot)
        .bind(aabb.x as f64)
        .bind(aabb.y as f64)
        .bind((aabb.x + aabb.w - 1) as f64)
        .bind((aabb.y + aabb.h - 1) as f64)
        .bind(&pipeline.original_sha)
        .bind(&pipeline.derivative_sha)
        .execute(&mut *tx),
    )
    .await?;
    iac_core::chunk_seqs::bump_chunk_seqs_pipeline(
        &mut tx,
        &[(seq.0, aabb.x, aabb.y, aabb.w as i32, aabb.h as i32)],
    )
    .await?;
    crate::render_jobs::enqueue_chunks(&mut tx, &chunk_keys).await?;
    sqlx::query(
        "UPDATE ingest_jobs
         SET state = 'placed', placement_id = $2, seq = $3,
             aabb_x = $4, aabb_y = $5, aabb_w = $6, aabb_h = $7
         WHERE id = $1 AND state = 'running'",
    )
    .bind(job.id)
    .bind(id)
    .bind(seq.0)
    .bind(aabb.x)
    .bind(aabb.y)
    .bind(aabb.w)
    .bind(aabb.h)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    // Delete the quarantine object only after the placement commits
    // (the lifecycle rule backs the cleanup up, doc step 3).
    sqlx::query("SELECT pg_notify('events', $1)")
        .bind(
            json!({
                "type": "placed",
                "id": id,
                "job": job.id,
                "seq": seq.0,
                "bbox": [aabb.x, aabb.y, aabb.w, aabb.h],
                "duplicate_count": duplicate_count.0,
            })
            .to_string(),
        )
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await?;
    // Happy-path quarantine cleanup (doc step 3; the lifecycle rule only
    // backs this up for crashed consumers). Best-effort: the placement is
    // committed, so a failed delete must not fail the job.
    if let Err(err) = ctx
        .store
        .delete(&iac_core::store::key(&quarantine_key))
        .await
    {
        tracing::warn!(job = %job.id, %err, "quarantine cleanup failed");
    }
    iac_core::metrics::ingest_done(t0.elapsed());
    Ok(())
}

/// Removal: wait-free apply once the placement exists. Already-removed
/// targets resolve idempotently; the job's `placed` state mirrors the
/// removal bump so GET /api/uploads/{id} reports completion either way.
async fn run_removal_job(ctx: &WorkerCtx, job: &IngestJob) -> AppResult<()> {
    let placement_id = job
        .placement_id
        .ok_or_else(|| AppError::BadRequest("removal job has no placement"))?;
    let mut tx = ctx.pool.begin().await?;
    let peeked: Option<(i64, i64, i64, i64, f32)> = iac_core::dbq::timed(
        "ingest",
        sqlx::query_as(
            "SELECT x, y, w::bigint, h::bigint, rot FROM placements
             WHERE id = $1 AND removed_at IS NULL FOR UPDATE",
        )
        .bind(placement_id)
        .fetch_optional(&mut *tx),
    )
    .await?;
    let (aabb_x, aabb_y, aabb_w, aabb_h, bump) = match peeked {
        Some((x, y, w, h, rot)) => {
            let aabb = iac_core::world::rotated_aabb(x, y, w, h, rot);
            let chunk_keys = aabb.chunk_keys();
            let bump: (i64,) = iac_core::dbq::timed(
                "ingest",
                sqlx::query_as("SELECT nextval('placement_seq')").fetch_one(&mut *tx),
            )
            .await?;
            iac_core::dbq::timed(
                "ingest",
                sqlx::query("UPDATE placements SET removed_at = now() WHERE id = $1")
                    .bind(placement_id)
                    .execute(&mut *tx),
            )
            .await?;
            iac_core::chunk_seqs::bump_chunk_seqs_removal(&mut tx, bump.0, &chunk_keys).await?;
            crate::render_jobs::enqueue_chunks(&mut tx, &chunk_keys).await?;
            sqlx::query("SELECT pg_notify('events', $1)")
                .bind(
                    json!({
                        "type": "remove",
                        "id": placement_id,
                        "bbox": [aabb.x, aabb.y, aabb.w, aabb.h],
                    })
                    .to_string(),
                )
                .execute(&mut *tx)
                .await
                .map_err(internal)?;
            (aabb.x, aabb.y, aabb.w, aabb.h, bump.0)
        }
        // Nothing live to remove (already removed, or the placement never
        // landed because its place job failed): the removal is complete.
        None => {
            let bump: (i64,) = iac_core::dbq::timed(
                "ingest",
                sqlx::query_as("SELECT nextval('placement_seq')").fetch_one(&mut *tx),
            )
            .await?;
            let (ax, ay, aw, ah) = (job.x, job.y, job.w, job.h);
            (ax, ay, aw, ah, bump.0)
        }
    };
    sqlx::query(
        "UPDATE ingest_jobs
         SET state = 'placed', seq = $2,
             aabb_x = $3, aabb_y = $4, aabb_w = $5, aabb_h = $6
         WHERE id = $1 AND state = 'running'",
    )
    .bind(job.id)
    .bind(bump)
    .bind(aabb_x)
    .bind(aabb_y)
    .bind(aabb_w)
    .bind(aabb_h)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await?;
    Ok(())
}

/// GET /api/uploads/{id} body: exactly contract/zod's
/// `uploadStatusSchema`. `Some(json)` → 200, `None` → 404.
pub async fn status_json(pool: &sqlx::PgPool, id: Uuid) -> AppResult<Option<serde_json::Value>> {
    let row = sqlx::query(
        "SELECT state, attempts, placement_id, seq,
                aabb_x, aabb_y, aabb_w, aabb_h, error
         FROM ingest_jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;
    let Some(row) = row else { return Ok(None) };
    let state: String = row.get("state");
    let body = match state.as_str() {
        "placed" => {
            let placement_id: Uuid = row.get("placement_id");
            let seq: i64 = row.get("seq");
            json!({
                "state": "placed",
                "id": placement_id,
                "seq": seq,
                "bbox": [
                    row.get::<i64, _>("aabb_x"),
                    row.get::<i64, _>("aabb_y"),
                    row.get::<i64, _>("aabb_w"),
                    row.get::<i64, _>("aabb_h"),
                ],
            })
        }
        "failed" => json!({
            "state": "failed",
            "attempts": i64::from(row.get::<i16, _>("attempts")),
            "permanent": true,
            "error": row.get::<Option<String>, _>("error").unwrap_or_default(),
        }),
        _ => json!({ "state": "pending" }),
    };
    Ok(Some(body))
}

/// Spawn the in-process ingest consumer. Called once from `build_app`,
/// only when the ingest pipeline is enabled. One task per process, at
/// most one pixel stage at a time (mirrors the request path's libvips
/// serialization); claims are SKIP LOCKED for horizontal scale.
pub fn spawn_consumer(ctx: &WorkerCtx) {
    let app = std::sync::Arc::new(ctx.clone());
    tokio::spawn(async move {
        loop {
            if let Ok(rows) = sqlx::query_as::<_, (String, i64)>(
                "SELECT state, count(*)::bigint FROM ingest_jobs GROUP BY state",
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
                iac_core::metrics::ingest_jobs_queue(
                    get("pending"),
                    get("running"),
                    get("placed"),
                    get("failed"),
                );
            }
            let claimed = match claim_due(&app.pool, app.cfg.render_pipeline_lease_secs).await {
                Ok(Some(job)) => {
                    tracing::info!(job = %job.id, kind = %job.kind, "ingest job claimed");
                    let t0 = std::time::Instant::now();
                    match execute_job(&app, job.clone()).await {
                        Ok(()) => tracing::info!(
                            job = %job.id,
                            dur_ms = t0.elapsed().as_millis() as u64,
                            outcome = "done",
                            "ingest job finished"
                        ),
                        Err(err) => {
                            tracing::warn!(job = %job.id, %err, "ingest job attempt failed");
                            let _ = fail_job(
                                &app,
                                &job,
                                &err,
                                app.cfg.render_pipeline_poll_ms as f64 / 1000.0,
                            )
                            .await;
                        }
                    }
                    continue;
                }
                Ok(None) => None,
                Err(err) => {
                    tracing::error!(%err, "ingest job claim failed");
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
