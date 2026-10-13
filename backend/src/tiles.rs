//! Tile pyramid: lazy rendering from chunks, maxSeq staleness, caching.

use crate::error::{internal, AppError, AppResult};
use crate::img;
use crate::state::App;
use crate::world::{
    is_valid_tile, tile_bounds, tile_chunk, tile_key, tile_native_size, tile_offset_in_chunk, Rect,
    BASE_Z, CHUNK_PX, TILE_PX,
};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Mutex as StdMutex;

/// (z, x, y, seq) → encoded webp bytes.
type CacheKey = (i64, i64, i64, i64);

pub struct TileCache {
    /// z ≤ 4 tiles never evict (at most 341 exist).
    low_zoom: StdMutex<HashMap<CacheKey, Bytes>>,
    lru: StdMutex<LruCache<CacheKey, Bytes>>,
}

impl TileCache {
    pub fn new(entries: usize) -> Self {
        Self {
            low_zoom: StdMutex::new(HashMap::new()),
            lru: StdMutex::new(LruCache::new(
                NonZeroUsize::new(entries.max(1)).unwrap_or(NonZeroUsize::MIN),
            )),
        }
    }

    fn get(&self, z: i64, key: CacheKey) -> Option<Bytes> {
        if z <= 4 {
            self.low_zoom
                .lock()
                .expect("low zoom cache")
                .get(&key)
                .cloned()
        } else {
            self.lru.lock().expect("tile lru").get(&key).cloned()
        }
    }

    fn put(&self, z: i64, key: CacheKey, bytes: Bytes) {
        if z <= 4 {
            self.low_zoom
                .lock()
                .expect("low zoom cache")
                .insert(key, bytes);
        } else {
            self.lru.lock().expect("tile lru").put(key, bytes);
        }
    }
}

/// Max non-removed placement seq whose bbox intersects the rect (0 = none).
pub async fn max_seq_covering(app: &App, rect: Rect) -> AppResult<i64> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COALESCE(max(seq), 0) FROM placements
         WHERE removed_at IS NULL
           AND bbox && box(point($1::float8, $2::float8),
                           point($3::float8, $4::float8))",
    )
    .bind(rect.x)
    .bind(rect.y)
    .bind(rect.x + rect.w - 1)
    .bind(rect.y + rect.h - 1)
    .fetch_one(&app.pool)
    .await?;
    Ok(row.0)
}

/// GET /api/tiles/{z}/{x}/{y}.webp
pub async fn get_tile(
    State(app): State<App>,
    axum::extract::Path((z, x, y)): axum::extract::Path<(String, String, String)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    // axum 0.8 has no suffix captures, so the last segment is a wildcard
    // that must be exactly "<n>.webp".
    let parse = |s: &str| {
        if s.contains('/') {
            return None;
        }
        s.strip_suffix(".webp").unwrap_or(s).parse::<i64>().ok()
    };
    let (Some(z), Some(x), Some(y)) = (parse(&z), parse(&x), parse(&y)) else {
        return Err(AppError::NotFound);
    };
    if !is_valid_tile(z, x, y) {
        return Err(AppError::NotFound);
    }
    let (bytes, seq) = tile_bytes(&app, z, x, y).await?;
    let etag = format!("\"{seq}\"");
    if let Some(inm) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        if inm.split(',').any(|t| t.trim() == etag) {
            let mut resp = StatusCode::NOT_MODIFIED.into_response();
            resp.headers_mut()
                .insert(header::ETAG, etag.parse().expect("etag header"));
            return Ok(resp);
        }
    }
    let mut resp = (StatusCode::OK, bytes).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, "image/webp".parse().expect("mime"));
    h.insert(header::CACHE_CONTROL, "no-cache".parse().expect("cc"));
    h.insert(header::ETAG, etag.parse().expect("etag header"));
    Ok(resp)
}

/// The constant empty (pure paper) tile, encoded once per process. Tiles
/// with maxSeq 0 are exactly this — which is what keeps cold pyramid
/// renders O(spine) instead of 4^z.
static EMPTY_TILE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

fn empty_tile_bytes() -> AppResult<Vec<u8>> {
    let bytes = EMPTY_TILE.get_or_init(|| img::empty_tile_256().expect("empty tile encode"));
    Ok(bytes.clone())
}

/// Resolve tile bytes through: memory cache → object storage (if fresh) →
/// render. Returns the bytes and the seq they were rendered at. Boxed to
/// permit recursion (children of z < BASE_Z render through this same fn).
fn tile_bytes<'a>(
    app: &'a App,
    z: i64,
    x: i64,
    y: i64,
) -> futures::future::BoxFuture<'a, AppResult<(Bytes, i64)>> {
    Box::pin(tile_bytes_inner(app, z, x, y))
}

// Records its resolution path (hit_mem / hit_s3 / render / empty) into
// metrics on every success return, including recursive child resolutions.
async fn tile_bytes_inner(app: &App, z: i64, x: i64, y: i64) -> AppResult<(Bytes, i64)> {
    let t0 = std::time::Instant::now();
    let seq = max_seq_covering(app, tile_bounds(z, x, y)).await?;
    // Empty region: constant paper tile, no recursion, no storage writes.
    if seq == 0 {
        let bytes = tokio::task::spawn_blocking(empty_tile_bytes)
            .await
            .map_err(internal)??;
        crate::metrics::tile_done(crate::metrics::tile_outcome("empty"), t0.elapsed());
        return Ok((Bytes::from(bytes), 0));
    }
    let key: CacheKey = (z, x, y, seq);
    if let Some(bytes) = app.tile_cache.get(z, key) {
        crate::metrics::tile_done(crate::metrics::tile_outcome("hit_mem"), t0.elapsed());
        return Ok((bytes, seq));
    }
    // Serialize renders per tile so concurrent misses don't duplicate work.
    // In-process only, by design: cross-instance duplicate renders are
    // benign — every instance computes the same seq for the tile, so both
    // produce byte-identical output and the last PUT wins with identical
    // content (place compositing is the only nondeterministic raster
    // writer, and it takes the chunk advisory locks). No PG advisory locks
    // on this path.
    let lock_key = format!("{z}/{x}/{y}");
    let _guard = app.tile_locks.lock_sorted(&[lock_key]).await;
    // Another request may have rendered this exact seq while we waited.
    if let Some(bytes) = app.tile_cache.get(z, key) {
        crate::metrics::tile_done(crate::metrics::tile_outcome("hit_mem"), t0.elapsed());
        return Ok((bytes, seq));
    }
    let object_key = tile_key(z, x, y);
    let stored: Option<(i64,)> =
        sqlx::query_as("SELECT seq FROM tiles WHERE z = $1 AND x = $2 AND y = $3")
            .bind(z as i16)
            .bind(x as i32)
            .bind(y as i32)
            .fetch_optional(&app.pool)
            .await?;
    if stored.map(|(s,)| s) == Some(seq) {
        if let Some(bytes) = crate::store::get_bytes(&app.store, &object_key).await? {
            let bytes = Bytes::from(bytes);
            app.tile_cache.put(z, key, bytes.clone());
            crate::metrics::tile_done(crate::metrics::tile_outcome("hit_s3"), t0.elapsed());
            return Ok((bytes, seq));
        }
    }
    let encoded = render_tile(app, z, x, y).await?;
    // Write-through: object storage + staleness row + memory cache.
    crate::store::put(&app.store, &object_key, encoded.to_vec()).await?;
    sqlx::query(
        "INSERT INTO tiles (z, x, y, seq) VALUES ($1, $2, $3, $4)
         ON CONFLICT (z, x, y) DO UPDATE
         SET seq = EXCLUDED.seq, rendered_at = now()",
    )
    .bind(z as i16)
    .bind(x as i32)
    .bind(y as i32)
    .bind(seq)
    .execute(&app.pool)
    .await?;
    app.tile_cache.put(z, key, encoded.clone());
    Ok((encoded, seq))
}

/// Render + encode the tile from its sources (chunk for z ≥ BASE_Z, the
/// four child tiles below). Pixel work is boxed into spawn_blocking.
async fn render_tile(app: &App, z: i64, x: i64, y: i64) -> AppResult<Bytes> {
    let bytes = if z >= BASE_Z {
        let (cx, cy) = tile_chunk(z, x, y);
        let key = crate::world::chunk_key(cx, cy);
        let chunk = crate::store::get_bytes(&app.store, &key).await?;
        let (ox, oy) = tile_offset_in_chunk(z, x, y);
        let native = tile_native_size(z);
        tokio::task::spawn_blocking(move || img::render_leaf_tile(chunk, ox, oy, native))
            .await
            .map_err(internal)?
            .map_err(internal)?
    } else {
        // Resolve the four children CONCURRENTLY: each child goes through
        // its own tile_bytes (mem cache / s3 / render), serialized only by
        // its own per-tile lock. Deadlock-free: the parent holds "z/x/y"
        // while children take "z+1/x'/y'" keys — the wait-for graph only
        // ever points DOWN the pyramid (a child's own recursion goes
        // deeper, never back up), so no cycle is possible regardless of
        // scheduling; lock_sorted orders within a single acquisition and
        // every concurrent child here acquires a different key. First
        // child error aborts the join (same ? semantics as the old
        // sequential loop).
        let child_futures = [(0i64, 0i64), (1, 0), (0, 1), (1, 1)]
            .map(|(dx, dy)| tile_bytes(app, z + 1, 2 * x + dx, 2 * y + dy));
        let resolved = futures::future::try_join_all(child_futures).await?;
        let kids: [Vec<u8>; 4] = resolved
            .into_iter()
            .map(|(bytes, _)| bytes.to_vec())
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| internal("children count"))?;
        tokio::task::spawn_blocking(move || img::render_parent_tile(&kids))
            .await
            .map_err(internal)?
            .map_err(internal)?
    };
    Ok(Bytes::from(bytes))
}

/// TILE_PX must divide a chunk exactly at BASE_Z for the recursion to line
/// up (compile-time sanity of the world constants).
const _: () = assert!(TILE_PX * (1 << (20 - BASE_Z)) == CHUNK_PX);
