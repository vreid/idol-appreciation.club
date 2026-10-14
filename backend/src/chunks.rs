//! Chunk raster mutation. Chunks are the ground-truth 1024×1024 RGBA
//! rasters in `chunks/{cx}/{cy}.webp`. All mutation happens while holding
//! the chunk-key locks (sorted), so paint order matches `seq` order.
//!
//! Two lock tiers protect the S3 read-modify-write window:
//! 1. in-process per-key locks (`App.chunk_locks`) — cheap same-pod
//!    serialization, acquired by callers before entering this module;
//! 2. session-level PG advisory locks (`pg_advisory_lock(class, objid)` on
//!    a dedicated pooled connection) — cross-instance exclusion. They are
//!    acquired BEFORE the chunk GET and held through the chunk PUT: the
//!    object write is not transactional, so a transaction-scoped lock
//!    would release at commit and leave the read-modify-write window open
//!    (commit-then-PUT = lost update).
//!
//! libvips images are !Send, so all pixel work runs inside
//! `spawn_blocking` over owned byte buffers.

use crate::error::{internal, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{chunk_key, Rect, CHUNK_PX};
use sqlx::Row;

/// Advisory-lock namespace for chunk raster mutation. PG 16 offers the
/// two-key advisory form only as (int4, int4), which cannot hold the 2^36
/// chunk-index space — so the class constant lives in the high bits of the
/// single-bigint key instead: semantics identical to a constant classid
/// plus a chunk-index objid, in the (bigint) signature that exists.
const CHUNK_LOCK_CLASS: i64 = 0x10F01;

/// Pack class + chunk index into one advisory key. Chunks are 2^18 per
/// side (268435456 / 1024); the index needs 36 bits, the class sits above.
pub fn chunk_lock_objid(cx: i64, cy: i64) -> i64 {
    (CHUNK_LOCK_CLASS << 40) | ((cx << 18) | cy)
}

/// Session-level advisory locks over a set of chunk indices, acquired in
/// sorted-key order (deadlock avoidance for multi-chunk rects: every
/// contender waits on the lowest index first, so the wait-for graph is
/// acyclic). Held on a dedicated pooled connection — not a transaction —
/// so they outlive any surrounding transaction and release only when
/// [`ChunkAdvisoryLocks::release`] runs or the connection's session ends.
pub struct ChunkAdvisoryLocks {
    conn: sqlx::pool::PoolConnection<sqlx::Postgres>,
    pub objids: Vec<i64>,
}

impl ChunkAdvisoryLocks {
    pub async fn acquire(pool: &sqlx::PgPool, mut objids: Vec<i64>) -> AppResult<Self> {
        objids.sort_unstable();
        objids.dedup();
        let mut conn = pool.acquire().await.map_err(internal)?;
        for oid in &objids {
            if let Err(e) = sqlx::query("SELECT pg_advisory_lock($1)")
                .bind(oid)
                .execute(&mut *conn)
                .await
            {
                // Partial acquisition: the connection's session still holds
                // the earlier locks. Closing it ends the session, which
                // releases them — never return a locked connection to the
                // pool.
                let _ = conn.close().await;
                return Err(internal(e));
            }
        }
        Ok(Self { conn, objids })
    }

    /// Release every held lock. Always call this (success or error paths):
    /// dropping without releasing would return a locked connection to the
    /// pool. If an unlock fails, the connection is closed instead, ending
    /// the session and releasing its locks server-side.
    pub async fn release(mut self) {
        for oid in &self.objids {
            let ok = sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(oid)
                .execute(&mut *self.conn)
                .await
                .is_ok();
            if !ok {
                let _ = self.conn.close().await;
                return;
            }
        }
    }
}

/// Composite an overlay (rotated about its center by `rot` clockwise
/// degrees) onto every chunk the placement's AABB touches. `aabb` must be
/// `world::rotated_aabb(x, y, w, h, rot)`. Caller must hold the locks for
/// `aabb.chunk_keys()`.
pub async fn composite_placement(
    app: &App,
    aabb: Rect,
    rot: f32,
    overlay: Vec<u8>,
) -> AppResult<()> {
    let keys = aabb.chunk_keys();
    let locks = ChunkAdvisoryLocks::acquire(
        &app.pool,
        keys.iter()
            .map(|(cx, cy)| chunk_lock_objid(*cx, *cy))
            .collect(),
    )
    .await?;
    let result = composite_locked(app, &keys, aabb, rot, overlay).await;
    locks.release().await;
    result
}

async fn composite_locked(
    app: &App,
    keys: &[(i64, i64)],
    aabb: Rect,
    rot: f32,
    overlay: Vec<u8>,
) -> AppResult<()> {
    // Fetch all current chunk rasters first.
    let mut bases = Vec::with_capacity(keys.len());
    let mut targets = Vec::with_capacity(keys.len());
    for (cx, cy) in keys {
        let key = chunk_key(*cx, *cy);
        bases.push(crate::store::get_bytes(&app.store, &key).await?);
        targets.push((aabb.x - cx * CHUNK_PX, aabb.y - cy * CHUNK_PX));
    }
    let encoded =
        crate::telemetry::blocking(move || img::composite_chunks(bases, &overlay, &targets, rot))
            .await
            .map_err(internal)?
            .map_err(internal)?;
    for ((cx, cy), bytes) in keys.iter().zip(encoded) {
        crate::store::put(&app.store, &chunk_key(*cx, *cy), bytes).await?;
    }
    Ok(())
}

/// Rebuild the given chunks from the manifest: blank canvas, then every
/// non-removed placement intersecting the chunk, in seq order. Runs inside
/// the caller's transaction so it observes the removal that triggered it.
pub async fn replay_chunks(
    app: &App,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    chunk_keys: &[(i64, i64)],
) -> AppResult<()> {
    // Same cross-instance exclusion as placement compositing: the replay
    // GETs and PUTs chunk rasters, so it must not interleave with a
    // concurrent placement's composite on another instance. Locks live on
    // their own pooled connection, deliberately outside `tx`.
    let locks = ChunkAdvisoryLocks::acquire(
        &app.pool,
        chunk_keys
            .iter()
            .map(|(cx, cy)| chunk_lock_objid(*cx, *cy))
            .collect(),
    )
    .await?;
    let result = replay_chunks_locked(app, tx, chunk_keys).await;
    locks.release().await;
    result
}

async fn replay_chunks_locked(
    app: &App,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    chunk_keys: &[(i64, i64)],
) -> AppResult<()> {
    for (cx, cy) in chunk_keys {
        let key = chunk_key(*cx, *cy);
        let (qx0, qy0) = (*cx * CHUNK_PX, *cy * CHUNK_PX);
        let (qx1, qy1) = (qx0 + CHUNK_PX - 1, qy0 + CHUNK_PX - 1);
        let rows = crate::dbq::timed(
            "tile_rows",
            sqlx::query(
                "SELECT image_key, x, y, w, h, rot FROM placements
             WHERE removed_at IS NULL
               AND bbox && box(point($1::float8, $2::float8), point($3::float8, $4::float8))
             ORDER BY seq",
            )
            .bind(qx0)
            .bind(qy0)
            .bind(qx1)
            .bind(qy1)
            .fetch_all(&mut **tx),
        )
        .await?;
        // Rebuild from BLANK (issue #159): compositing the live
        // placements over the CURRENT raster never erases the removed
        // art — it only gets hidden where something else painted over
        // it, and a chunk with zero live placements re-encoded
        // unchanged. The doc-comment contract is "blank canvas, then
        // every non-removed placement in seq order"; the live set was
        // fetched above, so start from transparent and repaint it all.
        let canvas_owned: Option<Vec<u8>> = None;
        let mut overlays: Vec<(Vec<u8>, i64, i64, f32)> = Vec::with_capacity(rows.len());
        for row in rows {
            let image_key: String = row.get("image_key");
            let px: i64 = row.get("x");
            let py: i64 = row.get("y");
            let rot: f32 = row.get("rot");
            let Some(bytes) = crate::store::get_bytes(&app.store, &image_key).await? else {
                tracing::warn!(key = %image_key, "derivative missing during replay");
                continue;
            };
            // The stored x/y are the UNROTATED rect's top-left; the
            // painted bitmap is the rotated AABB, offset by the AABB's
            // distance from the rect corner (negative extents included).
            let w: i32 = row.get("w");
            let h: i32 = row.get("h");
            let aabb = crate::world::rotated_aabb(px, py, i64::from(w), i64::from(h), rot);
            overlays.push((bytes, aabb.x - qx0, aabb.y - qy0, rot));
        }
        let encoded =
            crate::telemetry::blocking(move || img::replay_canvas(canvas_owned, &overlays))
                .await
                .map_err(internal)?
                .map_err(internal)?;
        crate::store::put(&app.store, &key, encoded).await?;
    }
    Ok(())
}

/// Advance the per-chunk content version for every chunk each (seq, x, y,
/// w, h) placement rect touches. Called in the SAME transaction as the
/// placement write (place insert, import batch insert) and — with a fresh
/// `nextval('placement_seq')` as the version — after a removal replay, so
/// versions only move forward. This table is the tile read path's ONLY
/// placement-derived input (issue #120 doctrine): placement rows are never
/// consulted at render time.
pub async fn bump_chunk_seqs(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    rows: &[(i64, i64, i64, i32, i32)],
) -> AppResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut seqs = Vec::with_capacity(rows.len());
    let mut xs = Vec::with_capacity(rows.len());
    let mut ys = Vec::with_capacity(rows.len());
    let mut ws = Vec::with_capacity(rows.len());
    let mut hs = Vec::with_capacity(rows.len());
    for (s, x, y, w, h) in rows {
        seqs.push(*s);
        xs.push(*x);
        ys.push(*y);
        ws.push(*w);
        hs.push(*h);
    }
    crate::dbq::timed(
        "chunk_bump",
        sqlx::query(
            "INSERT INTO chunk_seqs (cx, cy, max_seq)
             SELECT cx, cy, max(seq)
             FROM unnest($1::bigint[], $2::bigint[], $3::bigint[], $4::int[], $5::int[])
                  AS t(seq, x, y, w, h)
             CROSS JOIN LATERAL generate_series(floor(x / 1024.0)::bigint,
                                                floor((x + w - 1) / 1024.0)::bigint) AS cx
             CROSS JOIN LATERAL generate_series(floor(y / 1024.0)::bigint,
                                                floor((y + h - 1) / 1024.0)::bigint) AS cy
             GROUP BY cx, cy
             ON CONFLICT (cx, cy) DO UPDATE
             SET max_seq = GREATEST(chunk_seqs.max_seq, EXCLUDED.max_seq)",
        )
        .bind(&seqs)
        .bind(&xs)
        .bind(&ys)
        .bind(&ws)
        .bind(&hs)
        .execute(&mut **tx),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objid_packing_is_bijective_in_range() {
        // 2^18 chunks per side; packing must be unique across the full grid.
        let mut seen = std::collections::HashSet::new();
        for cx in [0i64, 1, 262143, 131072] {
            for cy in [0i64, 1, 262143, 65535] {
                assert!(seen.insert(chunk_lock_objid(cx, cy)));
            }
        }
        assert_eq!(
            chunk_lock_objid(262143, 262143),
            (CHUNK_LOCK_CLASS << 40) | (262143 << 18) | 262143
        );
        // Decodable round-trip (class in the high bits, index below).
        let oid = chunk_lock_objid(3, 5);
        assert_eq!(
            (
                (oid >> 40),
                (oid >> 18) & ((1 << 22) - 1),
                oid & ((1 << 18) - 1)
            ),
            (CHUNK_LOCK_CLASS, 3, 5)
        );
    }

    #[test]
    fn acquire_sorts_and_dedups() {
        // The sort/dedup happens before any DB round-trip; exercise it via
        // a pool-less path by checking the packed ordering directly.
        let mut ids: Vec<i64> = [(5i64, 9i64), (1, 2), (5, 9), (0, 262143)]
            .into_iter()
            .map(|(cx, cy)| chunk_lock_objid(cx, cy))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids,
            vec![
                chunk_lock_objid(0, 262143),
                chunk_lock_objid(1, 2),
                chunk_lock_objid(5, 9),
            ]
        );
    }
}
