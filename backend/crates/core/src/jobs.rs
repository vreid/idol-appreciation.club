//! Pure-SQL job-queue writes shared by the API's request path (the
//! queue-only place/remove branches) and the worker's consumers. The
//! EXECUTION loops live in `iac-worker`; these helpers only enqueue and
//! lock, so the API can depend on them without touching libvips.

use crate::error::{internal, AppResult};
use uuid::Uuid;
use sqlx::{Postgres, Transaction};

/// Advisory key the place path holds across the seq draw (the worker
/// inherits the constant so the lock identity matches).
pub const SEQUENCER_LOCK_KEY: i64 = 0x10F02 << 40;

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

/// Queue chunk jobs (z = 18) for `chunks` — the place/remove write paths'
/// single entry point. Chunk jobs never wait.
pub async fn enqueue_chunks(
    tx: &mut Transaction<'_, Postgres>,
    chunks: &[(i64, i64)],
) -> AppResult<()> {
    let jobs = chunks
        .iter()
        .map(|&(cx, cy)| (crate::world::BASE_Z, cx, cy, 0i64))
        .collect::<Vec<_>>();
    mark_dirty(tx, &jobs).await
}

/// Everything the 202 path records about an accepted upload.
pub struct PlaceJobSpec {
    pub job_id: Uuid,
    pub account_id: Uuid,
    pub quarantine_key: String,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub rot: f32,
    pub size: i64,
}

/// Queue a quarantine placement job (the 202 path's tx).
pub async fn enqueue_place(
    tx: &mut Transaction<'_, Postgres>,
    spec: &PlaceJobSpec,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO ingest_jobs
         (id, kind, account_id, quarantine_key, x, y, w, h, rot, size)
         VALUES ($1, 'place', $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(spec.job_id)
    .bind(spec.account_id)
    .bind(&spec.quarantine_key)
    .bind(spec.x)
    .bind(spec.y)
    .bind(spec.w as i32)
    .bind(spec.h as i32)
    .bind(spec.rot)
    .bind(spec.size as i32)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    Ok(())
}

/// Everything the 202 removal path records about the target.
pub struct RemovalJobSpec {
    pub job_id: Uuid,
    pub account_id: Uuid,
    pub placement_id: Uuid,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub rot: f32,
}

/// Queue an admin removal job (the 202 path's tx). The consumer locks
/// the placement row before applying, so a delete racing the placement's
/// own ingest applies strictly after it lands.
pub async fn enqueue_removal(
    tx: &mut Transaction<'_, Postgres>,
    spec: &RemovalJobSpec,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO ingest_jobs
         (id, kind, account_id, placement_id, x, y, w, h, rot)
         VALUES ($1, 'removal', $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(spec.job_id)
    .bind(spec.account_id)
    .bind(spec.placement_id)
    .bind(spec.x)
    .bind(spec.y)
    .bind(spec.w as i32)
    .bind(spec.h as i32)
    .bind(spec.rot)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    Ok(())
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
