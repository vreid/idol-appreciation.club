//! Tile serving: maxSeq staleness, caching, versioned read path.
//!
//! Post-pipeline (docs/scaling-architecture.md "Chunk and tile pipeline")
//! this module RENDERs nothing: pixels are cut by the worker's claim
//! loops, which also own the tiles-table write and the `versions` NOTIFY.
//! A GET resolves memory → object storage → the constant empty tile
//! (maxSeq 0) → 404. The 404s exist only inside the worker's paint quiet
//! window for a tile nobody ever fetched (or on storage loss); the
//! versioned client refetches on the SSE `versions` event. The
//! `tile_requests_total{outcome="render"}` doctrine is enforced by
//! construction — the render outcome no longer exists on this path.

use crate::error::{AppError, AppResult};
use crate::state::App;
use crate::world::{is_valid_tile, tile_bounds, tile_chunk, tile_key, BASE_Z, CHUNK_PX};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use lru::LruCache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::num::NonZeroUsize;

/// (z, x, y, seq) → encoded webp bytes.
type CacheKey = (i64, i64, i64, i64);

/// Coordinate → (newest seq, bytes). Only the newest seq per coordinate
/// is kept: keying by the full (z, x, y, seq) and never evicting (#155)
/// grows one entry per re-rendered seq — the whole pyramid's ancestors
/// re-render every quiet window while anyone is zoomed out, an unbounded
/// slow leak.
type LowZoomEntry = (i64, Bytes);

pub struct TileCache {
    /// z ≤ 4 tiles: at most 341 coordinates exist.
    low_zoom: Mutex<HashMap<(i64, i64, i64), LowZoomEntry>>,
    lru: Mutex<LruCache<CacheKey, Bytes>>,
}

impl TileCache {
    pub fn new(entries: usize) -> Self {
        Self {
            low_zoom: Mutex::new(HashMap::new()),
            lru: Mutex::new(LruCache::new(
                NonZeroUsize::new(entries.max(1)).unwrap_or(NonZeroUsize::MIN),
            )),
        }
    }

    fn get(&self, z: i64, key: CacheKey) -> Option<Bytes> {
        if z <= 4 {
            let hit = self
                .low_zoom
                .lock()
                .get(&(key.0, key.1, key.2))
                .map(|(seq, bytes)| (*seq, bytes.clone()));
            match hit {
                Some((seq, bytes)) if seq == key.3 => Some(bytes),
                _ => None,
            }
        } else {
            self.lru.lock().get(&key).cloned()
        }
    }

    fn put(&self, z: i64, key: CacheKey, bytes: Bytes) {
        if z <= 4 {
            self.low_zoom
                .lock()
                .insert((key.0, key.1, key.2), (key.3, bytes));
        } else {
            self.lru.lock().put(key, bytes);
        }
    }
}

/// Live (covering) content version for one tile: the max chunk seq over
/// its bbox — a single grouped `chunk_seqs` query at z ≥ BASE_Z, the
/// bottom-up collapse for the zoomed-out levels. This is the doctrine
/// read path (issue #120): placement rows are never consulted, so the
/// cost is pile-depth-independent.
async fn live_seq(app: &App, z: i64, x: i64, y: i64) -> AppResult<i64> {
    let rect = tile_bounds(z, x, y);
    let rows: Vec<(i64, i64, i64)> = crate::dbq::timed(
        "tile_seq",
        sqlx::query_as(
            "SELECT cx, cy, max_seq FROM chunk_seqs
             WHERE cx >= $1 AND cx <= $2 AND cy >= $3 AND cy <= $4",
        )
        .bind(rect.x.div_euclid(CHUNK_PX))
        .bind((rect.x + rect.w - 1).div_euclid(CHUNK_PX))
        .bind(rect.y.div_euclid(CHUNK_PX))
        .bind((rect.y + rect.h - 1).div_euclid(CHUNK_PX))
        .fetch_all(&app.pool),
    )
    .await?;
    if z >= BASE_Z {
        let (cx, cy) = tile_chunk(z, x, y);
        return Ok(rows
            .iter()
            .find(|(a, b, _)| (*a, *b) == (cx, cy))
            .map(|&(_, _, s)| s)
            .unwrap_or(0));
    }
    // Collapse chunk rows into per-level maxima, bottom-up, then read the
    // node's level (same math the pipeline's parent jobs use).
    let mut cur: HashMap<(i64, i64), i64> =
        rows.into_iter().map(|(cx, cy, s)| ((cx, cy), s)).collect();
    let target_level = (BASE_Z - z) as usize;
    for _ in 0..target_level {
        let mut next: HashMap<(i64, i64), i64> = HashMap::new();
        for (&(cx, cy), &seq) in &cur {
            let slot = next.entry((cx / 2, cy / 2)).or_default();
            if seq > *slot {
                *slot = seq;
            }
        }
        cur = next;
    }
    Ok(cur.get(&(x, y)).copied().unwrap_or(0))
}

/// Stored staleness row for the tile (the seq its stored bytes were cut
/// at), straight from the tiles table.
async fn stored_seq(app: &App, z: i64, x: i64, y: i64) -> AppResult<Option<i64>> {
    let row: Option<(i64,)> = crate::dbq::timed(
        "tiles_meta",
        sqlx::query_as("SELECT seq FROM tiles WHERE z = $1 AND x = $2 AND y = $3")
            .bind(z as i16)
            .bind(x as i32)
            .bind(y as i32)
            .fetch_optional(&app.pool),
    )
    .await?;
    Ok(row.map(|(seq,)| seq))
}

pub async fn get_tile(
    State(app): State<App>,
    axum::extract::Path((z, x, y)): axum::extract::Path<(String, String, String)>,
    axum::extract::Query(q): axum::extract::Query<TileQuery>,
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
    let t0 = std::time::Instant::now();
    let seq = live_seq(&app, z, x, y).await?;

    // Empty region: the constant paper tile — no storage, no recursion.
    if seq == 0 {
        let bytes = Bytes::from(iac_core::empty_tile::decode());
        crate::metrics::tile_done(crate::metrics::tile_outcome("empty"), t0.elapsed());
        let inm = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok());
        return Ok(tile_response(bytes, 0, false, inm, q.v));
    }

    let key: CacheKey = (z, x, y, seq);
    if let Some(bytes) = app.tile_cache.get(z, key) {
        crate::metrics::tile_done(crate::metrics::tile_outcome("hit_mem"), t0.elapsed());
        let inm = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok());
        return Ok(tile_response(bytes, seq, false, inm, q.v));
    }

    let object_key = tile_key(z, x, y);
    let stored = stored_seq(&app, z, x, y).await?;
    if stored == Some(seq) {
        if let Some(bytes) = crate::store::get_bytes(&app.store, &object_key).await? {
            let bytes = Bytes::from(bytes);
            app.tile_cache.put(z, key, bytes.clone());
            crate::metrics::tile_done(crate::metrics::tile_outcome("hit_s3"), t0.elapsed());
            let inm = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok());
            return Ok(tile_response(bytes, seq, false, inm, q.v));
        }
    }

    // Stored bytes at an older seq: serve them NOW (server-side SWR) —
    // the repaint belongs to the worker's quiet window, and the client
    // refetches on the SSE `versions` event. The ETag carries the STORED
    // seq so revalidations stay cheap 304s while the paint is in flight.
    if let Some(stored_seq) = stored {
        if stored_seq > 0 {
            if let Some(bytes) = crate::store::get_bytes(&app.store, &object_key).await? {
                crate::metrics::tile_done(
                    crate::metrics::tile_outcome("stale_served"),
                    t0.elapsed(),
                );
                let inm = headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok());
                return Ok(tile_response(
                    Bytes::from(bytes),
                    stored_seq,
                    true,
                    inm,
                    q.v,
                ));
            }
        }
    }

    // Nothing stored (first-ever request inside the paint window, or
    // storage loss): 404 — the worker is the only writer and the client
    // refetches on the version event.
    crate::metrics::tile_done(crate::metrics::tile_outcome("miss"), t0.elapsed());
    Err(AppError::NotFound)
}

/// 304 when If-None-Match carries the served `"maxSeq"` tag, else 200 with
/// the encoded tile. Caching is four cooperating parts: `max-age=1` stops
/// per-tile revalidation storms during placement bursts,
/// `stale-while-revalidate` serves tiles instantly while refreshing them in
/// the background, the SSE-driven client refetch bounds visible staleness
/// to ~1s past each place/remove commit, and the server-side stale-serve
/// (`stale` ⇒ `X-Tile-Stale: true` + the STORED seq as ETag) keeps a GET
/// with stale bytes available entirely off the paint path — the ETag
/// still makes every revalidation a cheap 304 while the seq is unchanged.
fn tile_response(
    bytes: Bytes,
    seq: i64,
    stale: bool,
    if_none_match: Option<&str>,
    version: Option<i64>,
) -> Response {
    let etag = format!("\"{seq}\"");
    let matched = if_none_match.is_some_and(|inm| inm.split(',').any(|t| t.trim() == etag));
    if matched {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        let h = resp.headers_mut();
        h.insert(header::ETAG, etag.parse().expect("etag header"));
        if stale {
            h.insert(
                HeaderName::from_static("x-tile-stale"),
                HeaderValue::from_static("true"),
            );
        }
        return resp;
    }
    // Versioned read path (contract #188): ?v=<held version> selects the
    // cache policy; absent v keeps today's max-age=1 + SWR. v == current
    // pins the bytes immutable; older is servable for a minute; newer
    // (client ahead of this pod — flip/rollback window) is not cacheable.
    let cache_control = match version {
        Some(v) if v > 0 && v == seq => "public, max-age=31536000, immutable",
        Some(v) if v > 0 && v < seq => "public, max-age=60",
        Some(v) if v > 0 && v > seq => "no-store",
        _ => "public, max-age=1, stale-while-revalidate=30",
    };
    let mut resp = (StatusCode::OK, bytes).into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, "image/webp".parse().expect("mime"));
    h.insert(
        header::CACHE_CONTROL,
        cache_control.parse().expect("cache-control header"),
    );
    h.insert(header::ETAG, etag.parse().expect("etag header"));
    if stale {
        h.insert(
            HeaderName::from_static("x-tile-stale"),
            HeaderValue::from_static("true"),
        );
    }
    resp
}

#[derive(serde::Deserialize)]
pub struct TileQuery {
    /// The version the client already holds (contract #188).
    pub v: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::{tile_response, TileCache};

    #[test]
    fn low_zoom_keeps_only_newest_seq_per_tile() {
        // Issue #155: the map must stay bounded by coordinates, not by
        // the number of distinct seqs ever rendered.
        let cache = TileCache::new(16);
        for seq in 0..1000i64 {
            cache.put(0, (0, 0, 0, seq), Bytes::from_static(b"new"));
        }
        assert_eq!(cache.low_zoom.lock().len(), 1);
        assert!(cache.get(0, (0, 0, 0, 999)).is_some());
        assert!(cache.get(0, (0, 0, 0, 0)).is_none());
        // A second coordinate is kept alongside.
        cache.put(2, (2, 1, 1, 7), Bytes::from_static(b"x"));
        assert_eq!(cache.low_zoom.lock().len(), 2);
    }

    use axum::http::header;
    use axum::http::StatusCode;
    use bytes::Bytes;

    const BYTES: Bytes = Bytes::from_static(b"tile-bytes");

    #[test]
    fn fresh_get_is_200_with_cache_headers() {
        let resp = tile_response(BYTES.clone(), 7, false, None, None);
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_TYPE], "image/webp");
        assert_eq!(
            h[header::CACHE_CONTROL],
            "public, max-age=1, stale-while-revalidate=30"
        );
        assert_eq!(h[header::ETAG], "\"7\"");
    }

    #[test]
    fn matching_if_none_match_is_304_with_etag() {
        let resp = tile_response(BYTES.clone(), 7, false, Some("\"7\""), None);
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(resp.headers()[header::ETAG], "\"7\"");
    }

    #[test]
    fn if_none_match_list_matches_after_trim() {
        let resp = tile_response(BYTES.clone(), 7, false, Some("\"3\", \"7\""), None);
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
    }

    #[test]
    fn stale_if_none_match_still_serves_200() {
        let resp = tile_response(BYTES.clone(), 7, false, Some("\"6\""), None);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::ETAG], "\"7\"");
    }

    #[test]
    fn stale_serve_flags_both_success_kinds() {
        // Server-side SWR: stored seq served with X-Tile-Stale on 200...
        let resp = tile_response(BYTES.clone(), 6, true, None, None);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::ETAG], "\"6\"");
        assert_eq!(resp.headers()["x-tile-stale"], "true");
        // ...and on the 304 when the client already holds those bytes.
        let resp = tile_response(BYTES.clone(), 6, true, Some("\"6\""), None);
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(resp.headers()["x-tile-stale"], "true");
    }
}
