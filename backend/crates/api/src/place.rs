//! POST /api/place — the placement pipeline, and DELETE admin removal.

use crate::auth::Authed;
use crate::error::{internal, AppError, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{derive_w_h, Rect, WORLD_PX};
use axum::extract::{Multipart, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

/// Pure cooldown rule (seconds, fractional for exact boundary tests):
/// returns Err(retry_after_seconds) when the placement must be rejected.
pub fn cooldown_check(elapsed_secs: f64, threshold_secs: i64) -> Result<(), i64> {
    let threshold = f64::from(threshold_secs as i32);
    if elapsed_secs >= threshold {
        return Ok(());
    }
    let retry = (threshold - elapsed_secs).ceil() as i64;
    Err(retry.max(1))
}

/// The transactional cooldown gate, consumed inside the caller's
/// transaction (the synchronous path holds chunk locks around it; the
/// 202 quarantine path runs it standalone). Admin-exempt accounts skip
/// both cooldowns entirely — no 429 path. Everyone else: paints
/// (overlapping existing art) wait 300 s, empty space 1 s. On rejection
/// the transaction is rolled back and the retry-after mirrors the
/// read-only precheck's decision.
async fn cooldown_gate(
    app: &App,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    aabb: &Rect,
) -> AppResult<()> {
    let acct: (bool,) = crate::dbq::timed(
        "place",
        sqlx::query_as("SELECT cooldown_exempt FROM accounts WHERE id = $1 FOR UPDATE")
            .bind(account_id)
            .fetch_one(&mut **tx),
    )
    .await?;
    if !acct.0 {
        let intersects: (bool,) = crate::dbq::timed(
            "place",
            sqlx::query_as(
                "SELECT EXISTS(
                 SELECT 1 FROM placements
                 WHERE removed_at IS NULL
                   AND bbox && box(point($1::float8, $2::float8),
                                   point($3::float8, $4::float8)))",
            )
            .bind(aabb.x)
            .bind(aabb.y)
            .bind(aabb.x + aabb.w - 1)
            .bind(aabb.y + aabb.h - 1)
            .fetch_one(&mut **tx),
        )
        .await?;
        let threshold = if intersects.0 {
            app.cfg.cooldown_paint_secs
        } else {
            app.cfg.cooldown_empty_secs
        };
        let updated: Option<(chrono::DateTime<Utc>,)> = crate::dbq::timed(
            "place",
            sqlx::query_as(
                "UPDATE accounts SET last_placement_at = now()
             WHERE id = $1
               AND (last_placement_at IS NULL
                    OR now() - last_placement_at >= make_interval(secs => $2))
             RETURNING last_placement_at",
            )
            .bind(account_id)
            .bind(threshold)
            .fetch_optional(&mut **tx),
        )
        .await?;
        if updated.is_none() {
            // No rollback here: the helper sees the transaction through a
            // &mut, and returning Err drops the caller's owned
            // transaction, which sqlx rolls back — the same prompt
            // release of the account row lock as an explicit rollback.
            let last: Option<(chrono::DateTime<Utc>,)> = crate::dbq::timed(
                "place",
                sqlx::query_as("SELECT last_placement_at FROM accounts WHERE id = $1")
                    .bind(account_id)
                    .fetch_optional(&app.pool),
            )
            .await?;
            let elapsed = last
                .map(|(t,)| (Utc::now() - t).num_milliseconds() as f64 / 1000.0)
                .unwrap_or(0.0);
            return match cooldown_check(elapsed, threshold) {
                Ok(()) => Err(AppError::Cooldown(threshold.max(1))),
                Err(retry) => Err(AppError::Cooldown(retry)),
            };
        }
    } else {
        // Keep last_placement_at fresh so a later un-exempt state starts
        // from the real last placement, not an ancient one.
        crate::dbq::timed(
            "place",
            sqlx::query("UPDATE accounts SET last_placement_at = now() WHERE id = $1")
                .bind(account_id)
                .execute(&mut **tx),
        )
        .await?;
    }
    Ok(())
}

/// Read-only early cooldown gate, run BEFORE the pixel stage so a
/// cooled-down account cannot spend the serialized libvips stage on a
/// request the placement transaction would reject anyway. Mirrors the
/// transaction's threshold decision (paint vs empty from the true-AABB
/// intersection); the transactional UPDATE stays the authoritative gate
/// under races.
async fn cooldown_precheck(app: &App, account_id: Uuid, aabb: &Rect) -> AppResult<()> {
    let acct: (bool, Option<chrono::DateTime<Utc>>) = crate::dbq::timed(
        "place",
        sqlx::query_as("SELECT cooldown_exempt, last_placement_at FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&app.pool),
    )
    .await?;
    if acct.0 {
        return Ok(());
    }
    let intersects: (bool,) = crate::dbq::timed(
        "place",
        sqlx::query_as(
            "SELECT EXISTS(
             SELECT 1 FROM placements
             WHERE removed_at IS NULL
               AND bbox && box(point($1::float8, $2::float8),
                               point($3::float8, $4::float8)))",
        )
        .bind(aabb.x)
        .bind(aabb.y)
        .bind(aabb.x + aabb.w - 1)
        .bind(aabb.y + aabb.h - 1)
        .fetch_one(&app.pool),
    )
    .await?;
    let threshold = if intersects.0 {
        app.cfg.cooldown_paint_secs
    } else {
        app.cfg.cooldown_empty_secs
    };
    let elapsed = match acct.1 {
        // Never placed: the transaction's `last_placement_at IS NULL OR`
        // arm admits unconditionally.
        None => return Ok(()),
        Some(t) => (Utc::now() - t).num_milliseconds() as f64 / 1000.0,
    };
    match cooldown_check(elapsed, threshold) {
        Ok(()) => Ok(()),
        Err(retry) => Err(AppError::Cooldown(retry)),
    }
}

async fn read_multipart(app: &App, mut mp: Multipart) -> AppResult<(Vec<u8>, i64, i64, i64, f32)> {
    let mut image: Option<Vec<u8>> = None;
    let mut x: Option<i64> = None;
    let mut y: Option<i64> = None;
    let mut size: Option<i64> = None;
    let mut rot: Option<f32> = None;
    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("malformed multipart"))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "image" => {
                let mut buf: Vec<u8> = Vec::new();
                let mut stream = field;
                while let Some(chunk) = stream
                    .chunk()
                    .await
                    .map_err(|_| AppError::BadRequest("bad image part"))?
                {
                    if buf.len() + chunk.len() > app.cfg.max_upload_bytes {
                        return Err(AppError::TooLarge);
                    }
                    buf.extend_from_slice(&chunk);
                }
                if image.is_some() {
                    return Err(AppError::BadRequest("duplicate image field"));
                }
                image = Some(buf);
            }
            "x" | "y" | "size" => {
                let value = field
                    .text()
                    .await
                    .map_err(|_| AppError::BadRequest("bad form field"))?;
                let parsed: i64 = value
                    .trim()
                    .parse()
                    .map_err(|_| AppError::BadRequest("x, y and size must be integers"))?;
                match name.as_str() {
                    "x" => x = Some(parsed),
                    "y" => y = Some(parsed),
                    _ => size = Some(parsed),
                }
            }
            "rot" => {
                let value = field
                    .text()
                    .await
                    .map_err(|_| AppError::BadRequest("bad form field"))?;
                let parsed: f32 = value
                    .trim()
                    .parse()
                    .map_err(|_| AppError::BadRequest("rot must be a number of degrees"))?;
                if !parsed.is_finite() {
                    return Err(AppError::BadRequest("rot must be finite"));
                }
                if rot.is_some() {
                    return Err(AppError::BadRequest("duplicate rot field"));
                }
                rot = Some(parsed);
            }
            _ => {} // ignore unknown fields
        }
    }
    match (image, x, y, size) {
        (Some(image), Some(x), Some(y), Some(size)) => Ok((
            image,
            x,
            y,
            size,
            crate::world::normalize_rot(rot.unwrap_or(0.0)),
        )),
        _ => Err(AppError::BadRequest(
            "fields image, x, y and size are required",
        )),
    }
}

/// POST /api/place.
pub async fn place(
    State(app): State<App>,
    Authed(account_id): Authed,
    mp: Multipart,
) -> AppResult<Response> {
    let t0 = std::time::Instant::now();
    let (image_bytes, x, y, size, rot) = read_multipart(&app, mp).await?;
    if image_bytes.is_empty() {
        return Err(AppError::BadRequest("empty image"));
    }
    if !(app.cfg.min_size..=app.cfg.max_size).contains(&size) {
        return Err(AppError::BadRequest(
            "size must be within the configured bounds",
        ));
    }
    // Everything cheap is decided BEFORE any pixel work: header probe,
    // decoded-pixel budget, world bounds, cooldown. An oversizing or
    // cooled-down request never reaches the (process-wide serialized)
    // libvips stage, so neither a decompression bomb nor a cooldown retry
    // loop can monopolize it.
    let (probe_w, probe_h) = img::probe(&image_bytes)?;
    let declared = i64::from(probe_w) * i64::from(probe_h);
    if declared > 0 && u64::try_from(declared).unwrap_or(u64::MAX) > app.cfg.max_decode_pixels {
        return Err(AppError::TooLarge);
    }
    // The strip/resave stage preserves dimensions, so the probed header
    // determines the placement rect exactly.
    let (w, h) = derive_w_h(i64::from(probe_w), i64::from(probe_h), size);
    // Bounds are checked on the AABB of the rotated rect: it contains the
    // unrotated rect, and the painted bitmap is the AABB.
    let aabb = crate::world::rotated_aabb(x, y, w, h, rot);
    if aabb.x < 0 || aabb.y < 0 || aabb.x + aabb.w > WORLD_PX || aabb.y + aabb.h > WORLD_PX {
        return Err(AppError::BadRequest("rect out of world bounds"));
    }
    cooldown_precheck(&app, account_id, &aabb).await?;

    // Phase 4 quarantine path (docs "Upload flow", INGEST_PIPELINE_ENABLED):
    // the cheap validation above is all the request pays. Stash the bytes
    // and queue the ingest job; the pixel stage, content-addressed S3
    // writes, the placement row, chunk versions and render jobs all move
    // to the consumer. The client follows the `placed`/`place_failed`
    // events or polls GET /api/uploads/{id}.
    if app.cfg.ingest_pipeline_enabled {
        let job_id = Uuid::now_v7();
        let quarantine_key = format!("quarantine/{job_id}");
        // The object write is not transactional; a crash between PUT and
        // the job insert orphans a quarantine object, which the
        // lifecycle rule cleans up (docs step 3).
        crate::store::put(&app.store, &quarantine_key, image_bytes).await?;
        let mut tx = app.pool.begin().await?;
        cooldown_gate(&app, &mut tx, account_id, &aabb).await?;
        crate::ingest::enqueue_place(
            &mut tx,
            &crate::ingest::PlaceJobSpec {
                job_id,
                account_id,
                quarantine_key,
                x,
                y,
                w,
                h,
                rot,
                size,
            },
        )
        .await?;
        tx.commit().await?;
        return Ok((
            axum::http::StatusCode::ACCEPTED,
            Json(json!({
                "id": job_id,
                "state": "pending",
                "x": x,
                "y": y,
                "w": w,
                "h": h,
                "rot": rot,
            })),
        )
            .into_response());
    }

    // Sync pixel stage (libvips is !Send): decode, strip metadata,
    // re-save in the source codec, hash; then build the deterministic
    // webp derivative from the stripped raster.
    let max_pixels = app.cfg.max_decode_pixels;
    let pipeline = crate::telemetry::blocking(move || -> AppResult<_> {
        let (src, format) = img::decode(&image_bytes, max_pixels)?;
        let (w, h) = derive_w_h(
            i64::from(src.get_width()),
            i64::from(src.get_height()),
            size,
        );
        // Strip ALL metadata (EXIF/XMP/ICC) and re-save in the source
        // codec; sha256 of these bytes is the content address. No GPS or
        // EXIF data ever leaves the process.
        let stripped = img::strip_resave(&src, format)?;
        let (derivative_src, _) = img::decode(&stripped, max_pixels)?;
        let dw = i32::try_from(w).map_err(|_| internal("width overflow"))?;
        let dh = i32::try_from(h).map_err(|_| internal("height overflow"))?;
        let derivative = img::resize_exact(&derivative_src, dw, dh)?;
        let derivative_bytes = img::webp_bytes(&derivative, 85)?;
        Ok((stripped, format, (w, h), derivative_bytes))
    })
    .await
    .map_err(internal)??;
    let (stripped, format, (w, h), derivative_bytes) = pipeline;
    let rect = Rect::new(x, y, w, h);
    let original_sha = img::sha256_hex(&stripped);
    let original_key = format!("originals/{original_sha}.{}", format.ext());
    let derivative_sha = img::sha256_hex(&derivative_bytes);
    let image_key = format!("derivatives/{derivative_sha}.webp");

    // Upload derivative first (dedup); original second.
    crate::store::put_if_absent(&app.store, &image_key, derivative_bytes.clone()).await?;
    crate::store::put_if_absent(&app.store, &original_key, stripped).await?;

    let id = Uuid::now_v7();
    let chunk_keys = aabb.chunk_keys();
    let lock_keys: Vec<String> = chunk_keys
        .iter()
        .map(|(cx, cy)| crate::world::chunk_key(*cx, *cy))
        .collect();
    let _guards = app.chunk_locks.lock_sorted(&lock_keys).await;

    let mut tx = app.pool.begin().await?;
    // Cooldown exemption (admin-granted): skip both cooldowns entirely —
    // no 429 path. Everyone else: paints (overlapping existing art) wait
    // 300 s, empty space 1 s.
    cooldown_gate(&app, &mut tx, account_id, &aabb).await?;

    // Pipeline-on ordering doctrine (docs/scaling-architecture.md
    // "Ordering"): hold the single sequencer row lock from the seq draw
    // to commit so commit order equals sequence order — the worker's
    // incremental painting depends on it. Off (the default): today's
    // path, unchanged.
    if app.cfg.render_pipeline_enabled {
        crate::render_jobs::lock_sequencer(&mut tx).await?;
    }
    let seq: (i64,) = crate::dbq::timed(
        "place",
        sqlx::query_as("SELECT nextval('placement_seq')").fetch_one(&mut *tx),
    )
    .await?;
    let seq = seq.0;

    // Paint chunks before the row commits: tiles/ETags advance only when
    // the rasters already show the placement. Pipeline-on: the worker
    // paints — this transaction only records and queues.
    if !app.cfg.render_pipeline_enabled {
        crate::chunks::composite_placement(&app, aabb, rot, derivative_bytes).await?;
        crate::metrics::chunk_composited();
    }

    let duplicate_count: (i64,) = crate::dbq::timed(
        "place",
        sqlx::query_as(
            "SELECT count(*) FROM placements
         WHERE original_sha = $1 AND removed_at IS NULL",
        )
        .bind(&original_sha)
        .fetch_one(&mut *tx),
    )
    .await?;

    // bbox column = AABB of the rotated rect (GiST-indexed); x/y/w/h stay
    // the unrotated rect top-left and size.
    crate::dbq::timed(
        "place",
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
        .bind(seq)
        .bind(rot)
        .bind(aabb.x as f64)
        .bind(aabb.y as f64)
        .bind((aabb.x + aabb.w - 1) as f64)
        .bind((aabb.y + aabb.h - 1) as f64)
        .bind(&original_sha)
        .bind(&derivative_sha)
        .execute(&mut *tx),
    )
    .await?;

    // The chunk version rollup advances with the placement (same tx): the
    // tile read path reads chunk_seqs, never placement rows. Pipeline-on:
    // the ordering-aware variant flags out-of-order commits, and the
    // chunk jobs are queued here (the worker paints).
    if app.cfg.render_pipeline_enabled {
        crate::chunks::bump_chunk_seqs_pipeline(
            &mut tx,
            &[(seq, aabb.x, aabb.y, aabb.w as i32, aabb.h as i32)],
        )
        .await?;
        crate::render_jobs::enqueue_chunks(&mut tx, &chunk_keys).await?;
    } else {
        crate::chunks::bump_chunk_seqs(
            &mut tx,
            &[(seq, aabb.x, aabb.y, aabb.w as i32, aabb.h as i32)],
        )
        .await?;
    }

    let event = json!({
        "type": "place",
        "seq": seq,
        "bbox": [aabb.x, aabb.y, aabb.w, aabb.h]
    })
    .to_string();
    let _: ((),) = crate::dbq::timed(
        "place",
        sqlx::query_as("SELECT pg_notify('events', $1)")
            .bind(&event)
            .fetch_one(&mut *tx),
    )
    .await?;
    tx.commit().await?;

    crate::metrics::place_done(t0.elapsed());
    let resp = (
        axum::http::StatusCode::CREATED,
        Json(json!({
            "id": id,
            "x": rect.x,
            "y": rect.y,
            "w": rect.w,
            "h": rect.h,
            "rot": rot,
            "seq": seq,
            "duplicate_count": duplicate_count.0,
        })),
    )
        .into_response();
    Ok(resp)
}

/// DELETE /api/admin/placements/{id} — soft delete + chunk replay + NOTIFY.
pub async fn admin_delete(
    State(app): State<App>,
    Authed(admin_id): Authed,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Response> {
    crate::auth::require_admin(&app, admin_id).await?;
    // Peek the rect first so chunk locks can be acquired before any row
    // locks, matching the placement pipeline's lock order.
    let peeked: Option<(i64, i64, i64, i64, f32)> = crate::dbq::timed(
        "place",
        sqlx::query_as(
        "SELECT x, y, w::bigint, h::bigint, rot FROM placements WHERE id = $1 AND removed_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&app.pool),
    )
    .await?;
    let Some((x, y, w, h, rot)) = peeked else {
        return Err(AppError::NotFound);
    };
    // Replay/notify scope = the AABB of the rotated rect (the painted
    // bitmap), matching the placement pipeline.
    let aabb = crate::world::rotated_aabb(x, y, w, h, rot);
    // Phase 4 quarantine path: queue the removal instead of applying it
    // inline. The consumer locks the placement row — which also orders a
    // delete of a still-quarantined upload after its placement lands —
    // then applies removed_at + the version bump + render job enqueue.
    if app.cfg.ingest_pipeline_enabled {
        let job_id = Uuid::now_v7();
        let mut tx = app.pool.begin().await?;
        crate::ingest::enqueue_removal(
            &mut tx,
            &crate::ingest::RemovalJobSpec {
                job_id,
                account_id: admin_id,
                placement_id: id,
                x,
                y,
                w,
                h,
                rot,
            },
        )
        .await?;
        tx.commit().await?;
        return Ok((
            axum::http::StatusCode::ACCEPTED,
            Json(json!({
                "id": job_id,
                "state": "pending",
                "x": x,
                "y": y,
                "w": w,
                "h": h,
                "rot": rot,
            })),
        )
            .into_response());
    }
    let chunk_keys = aabb.chunk_keys();
    let lock_keys: Vec<String> = chunk_keys
        .iter()
        .map(|(cx, cy)| crate::world::chunk_key(*cx, *cy))
        .collect();
    let _guards = app.chunk_locks.lock_sorted(&lock_keys).await;

    let mut tx = app.pool.begin().await?;
    let still_there: Option<(i64,)> = crate::dbq::timed(
        "place",
        sqlx::query_as(
            "SELECT 1::bigint FROM placements WHERE id = $1 AND removed_at IS NULL FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx),
    )
    .await?;
    if still_there.is_none() {
        tx.rollback().await.map_err(internal)?;
        return Err(AppError::NotFound);
    }

    crate::dbq::timed(
        "place",
        sqlx::query("UPDATE placements SET removed_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx),
    )
    .await?;
    if app.cfg.render_pipeline_enabled {
        // Phase 3 removal (docs "Removal"): set removed_at, advance the
        // chunk's version, set needs_rebuild, queue — and return. The
        // from-blank rebuild (~80 ms per remaining image) runs in the
        // worker.
        let bump: (i64,) = crate::dbq::timed(
            "place",
            sqlx::query_as("SELECT nextval('placement_seq')").fetch_one(&mut *tx),
        )
        .await?;
        crate::chunks::bump_chunk_seqs_removal(&mut tx, bump.0, &chunk_keys).await?;
        crate::render_jobs::enqueue_chunks(&mut tx, &chunk_keys).await?;
    } else {
        crate::chunks::replay_chunks(&app, &mut tx, &chunk_keys).await?;
        // The replay rewrites the rasters without the removed art; give the
        // chunks a FRESH version (a raw sequence draw, no row inserted) so the
        // rollup moves forward and stale tiles re-render exactly once.
        let bump: (i64,) = crate::dbq::timed(
            "place",
            sqlx::query_as("SELECT nextval('placement_seq')").fetch_one(&mut *tx),
        )
        .await?;
        crate::chunks::bump_chunk_seqs(
            &mut tx,
            &chunk_keys
                .iter()
                .map(|&(cx, cy)| {
                    (
                        bump.0,
                        cx * crate::world::CHUNK_PX,
                        cy * crate::world::CHUNK_PX,
                        1,
                        1,
                    )
                })
                .collect::<Vec<_>>(),
        )
        .await?;
    }
    let event = json!({
        "type": "remove",
        "id": id,
        "bbox": [aabb.x, aabb.y, aabb.w, aabb.h]
    })
    .to_string();
    let _: ((),) = crate::dbq::timed(
        "place",
        sqlx::query_as("SELECT pg_notify('events', $1)")
            .bind(&event)
            .fetch_one(&mut *tx),
    )
    .await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

/// GET /api/uploads/{id} — the upload-status contract (docs "Upload
/// flow"): the body is exactly contract/zod's `uploadStatusSchema`
/// (pending / placed / failed), 404 for unknown ids. The web client's
/// poll treats 404 as "keep waiting on events", so pre-pipeline
/// backends never break it. Owner or admin only; anyone else gets the
/// same 404 (no existence leak).
pub async fn upload_status(
    State(app): State<App>,
    Authed(account_id): Authed,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> AppResult<Response> {
    let owner: Option<(Option<Uuid>,)> = crate::dbq::timed(
        "place",
        sqlx::query_as("SELECT account_id FROM ingest_jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(&app.pool),
    )
    .await?;
    match owner {
        // The uploading account polls its own job.
        Some((account,)) if account == Some(account_id) => {}
        // Everyone else (incl. admin-issued removal jobs): admin role or
        // the same 404.
        Some(_) => {
            crate::auth::require_admin(&app, account_id)
                .await
                .map_err(|_| AppError::NotFound)?;
        }
        None => return Err(AppError::NotFound),
    }
    match crate::ingest::status_json(&app.pool, id).await? {
        Some(body) => Ok(Json(body).into_response()),
        None => Err(AppError::NotFound),
    }
}

#[cfg(test)]
mod tests {
    use super::cooldown_check;

    /// Exact cooldown boundaries; fractional elapsed seconds model
    /// "just under" vs "at" the threshold (299.99s vs 300s).
    #[test]
    fn paint_cooldown_boundary() {
        assert!(cooldown_check(299.99, 300).is_err());
        assert_eq!(cooldown_check(299.99, 300), Err(1));
        assert_eq!(cooldown_check(299.5, 300), Err(1));
        assert_eq!(cooldown_check(299.0, 300), Err(1));
        assert_eq!(cooldown_check(300.0, 300), Ok(()));
        assert_eq!(cooldown_check(300.001, 300), Ok(()));
        assert_eq!(cooldown_check(100_000.0, 300), Ok(()));
    }

    #[test]
    fn empty_space_cooldown_boundary() {
        assert_eq!(cooldown_check(0.0, 1), Err(1));
        assert_eq!(cooldown_check(0.99, 1), Err(1));
        assert_eq!(cooldown_check(1.0, 1), Ok(()));
        assert_eq!(cooldown_check(2.0, 1), Ok(()));
    }

    #[test]
    fn retry_after_always_positive() {
        // Never less than 1 second, even for a 1ms gap.
        assert_eq!(cooldown_check(0.001, 1), Err(1));
        assert_eq!(cooldown_check(0.0, 300), Err(300));
    }
}
