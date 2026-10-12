//! Chunk raster mutation. Chunks are the ground-truth 1024×1024 RGBA
//! rasters in `chunks/{cx}/{cy}.webp`. All mutation happens while holding
//! the chunk-key locks (sorted), so paint order matches `seq` order.

use crate::error::AppResult;
use crate::img;
use crate::state::App;
use crate::world::{chunk_key, Rect, CHUNK_PX};
use sqlx::Row;

/// Composite an overlay onto every chunk the rect touches. Caller must
/// hold the locks for `rect.chunk_keys()`.
pub async fn composite_placement(
    app: &App,
    rect: Rect,
    overlay: &libvips::VipsImage,
) -> AppResult<()> {
    for (cx, cy) in rect.chunk_keys() {
        let key = chunk_key(cx, cy);
        let base = match crate::store::get_bytes(&app.store, &key).await? {
            Some(bytes) => libvips::VipsImage::new_from_buffer(&bytes, "")
                .map_err(crate::error::AppError::from)?,
            None => img::blank_chunk(CHUNK_PX as i32)?,
        };
        let local_x = rect.x - cx * CHUNK_PX;
        let local_y = rect.y - cy * CHUNK_PX;
        let out = img::composite(&base, overlay, local_x, local_y)?;
        let bytes = img::webp_bytes(&out, 90)?;
        crate::store::put(&app.store, &key, bytes).await?;
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
            "SELECT image_key, x, y FROM placements
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
        let mut canvas = img::blank_chunk(CHUNK_PX as i32)?;
        for row in rows {
            let image_key: String = row.get("image_key");
            let px: i64 = row.get("x");
            let py: i64 = row.get("y");
            let Some(bytes) = crate::store::get_bytes(&app.store, &image_key).await? else {
                tracing::warn!(key = %image_key, "derivative missing during replay");
                continue;
            };
            let (overlay, _) = img::decode(&bytes)?;
            canvas = img::composite(&canvas, &overlay, px - qx0, py - qy0)?;
        }
        let out = img::webp_bytes(&canvas, 90)?;
        crate::store::put(&app.store, &key, out).await?;
    }
    Ok(())
}
