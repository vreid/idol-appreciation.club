//! Chunk raster mutation. Chunks are the ground-truth 1024×1024 RGBA
//! rasters in `chunks/{cx}/{cy}.webp`. All mutation happens while holding
//! the chunk-key locks (sorted), so paint order matches `seq` order.
//!
//! libvips images are !Send, so all pixel work runs inside
//! `spawn_blocking` over owned byte buffers.

use crate::error::{internal, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{chunk_key, Rect, CHUNK_PX};
use sqlx::Row;

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
    // Fetch all current chunk rasters first.
    let mut bases = Vec::with_capacity(keys.len());
    let mut targets = Vec::with_capacity(keys.len());
    for (cx, cy) in &keys {
        let key = chunk_key(*cx, *cy);
        bases.push(crate::store::get_bytes(&app.store, &key).await?);
        targets.push((aabb.x - cx * CHUNK_PX, aabb.y - cy * CHUNK_PX));
    }
    let encoded =
        tokio::task::spawn_blocking(move || img::composite_chunks(bases, &overlay, &targets, rot))
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
    for (cx, cy) in chunk_keys {
        let key = chunk_key(*cx, *cy);
        let (qx0, qy0) = (*cx * CHUNK_PX, *cy * CHUNK_PX);
        let (qx1, qy1) = (qx0 + CHUNK_PX - 1, qy0 + CHUNK_PX - 1);
        let rows = sqlx::query(
            "SELECT image_key, x, y, w, h, rot FROM placements
             WHERE removed_at IS NULL
               AND bbox && box(point($1::float8, $2::float8), point($3::float8, $4::float8))
             ORDER BY seq",
        )
        .bind(qx0)
        .bind(qy0)
        .bind(qx1)
        .bind(qy1)
        .fetch_all(&mut **tx)
        .await?;
        let canvas = crate::store::get_bytes(&app.store, &key).await?;
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
        let canvas_owned = canvas.clone();
        let encoded =
            tokio::task::spawn_blocking(move || img::replay_canvas(canvas_owned, &overlays))
                .await
                .map_err(internal)?
                .map_err(internal)?;
        crate::store::put(&app.store, &key, encoded).await?;
    }
    Ok(())
}
