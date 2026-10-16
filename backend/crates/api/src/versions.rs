//! Versioned tile read path: the per-pod coord→version map, the bbox
//! query, and `GET /api/versions`. Pairs with webrewrite's #188 client
//! contract (`tileVersionsResponseSchema` / `versionsEventSchema` in
//! contract/zod).
//!
//! The `tiles.seq` column is the single source of truth — a tile's
//! version is the seq its stored bytes were rendered at (the same number
//! the ETag carries). This module is a lazy read-through map over that
//! column: misses load from the database (the `-ro` pool when one is
//! configured), and every render path that writes `tiles.seq` notes the
//! new version here via [`note`] — the clean seam #190's pipeline is
//! expected to call instead of touching this map directly.
//!
//! Cross-pod freshness rides the existing `events` NOTIFY channel:
//! render completions publish a `versions` payload, every pod's listener
//! notes it into its own map, and the SSE layer coalesces those payloads
//! into 1-second-per-connection `versions` events filtered to the
//! subscriber's area.

use crate::error::{AppError, AppResult};
use crate::state::App;
use crate::world::{is_valid_tile, tile_native_size, MAX_Z};
use axum::extract::State;
use axum::Json;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::LazyLock;

type Coord = (i64, i64, i64);

static MAP: LazyLock<Mutex<HashMap<Coord, i64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Record a rendered version for a tile. In-memory only: the database
/// write happens at the render's write-through; this is the notification
/// seam for in-process callers (tile GET write-through, background
/// worker, and #190's pipeline).
pub fn note(z: i64, x: i64, y: i64, version: i64) {
    if version <= 0 {
        return;
    }
    // Versions only advance; a stale late write must not regress.
    let mut map = MAP.lock();
    let cur = map.entry((z, x, y)).or_insert(0);
    if version > *cur {
        *cur = version;
    }
}

/// Note every tile in a `versions` NOTIFY payload
/// (`{"type":"versions","tiles":[{"key":"z/x/y","version":n},…]}`).
/// Malformed payloads are ignored — the map is a cache.
pub fn note_payload(payload: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("versions") {
        return;
    }
    let Some(tiles) = v.get("tiles").and_then(|t| t.as_array()) else {
        return;
    };
    for t in tiles {
        let (Some(key), Some(version)) = (
            t.get("key").and_then(|k| k.as_str()),
            t.get("version").and_then(|n| n.as_i64()),
        ) else {
            continue;
        };
        if let Some((z, x, y)) = parse_key(key) {
            note(z, x, y, version);
        }
    }
}

fn parse_key(key: &str) -> Option<Coord> {
    let mut it = key.split('/');
    let z = it.next()?.parse().ok()?;
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    (it.next().is_none() && is_valid_tile(z, x, y)).then_some((z, x, y))
}

fn read_pool(app: &App) -> &PgPool {
    app.pool_ro.as_ref().unwrap_or(&app.pool)
}

/// Current version of a tile: map hit, else read-through from the
/// database (0 = never rendered — the empty-tile shortcut). DB errors
/// degrade to 0 (never rendered): the map is a cache, reads are cheap
/// and self-correcting.
pub async fn current(app: &App, z: i64, x: i64, y: i64) -> i64 {
    {
        let map = MAP.lock();
        if let Some(v) = map.get(&(z, x, y)) {
            return *v;
        }
    }
    let row = crate::dbq::timed(
        "tile_versions",
        sqlx::query_as::<_, (i64,)>("SELECT seq FROM tiles WHERE z = $1 AND x = $2 AND y = $3")
            .bind(z as i16)
            .bind(x as i32)
            .bind(y as i32)
            .fetch_optional(read_pool(app)),
    )
    .await
    .unwrap_or(None);
    let v = row.map_or(0, |(seq,)| seq);
    note(z, x, y, v);
    v
}

/// Versions of every rendered tile intersecting the bbox at zoom `z`
/// (contract: `GET /api/versions?bbox=x,y,w,h&z=<zoom>`). One query;
/// also warms the map.
pub async fn in_bbox(
    app: &App,
    z: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
) -> AppResult<Vec<(Coord, i64)>> {
    if !(0..=MAX_Z).contains(&z) {
        return Err(AppError::BadRequest("z out of range"));
    }
    let native = tile_native_size(z);
    let n = 1i64 << z;
    let (bx0, by0) = (x.max(0), y.max(0));
    let (bx1, by1) = (x.saturating_add(w.max(0)), y.saturating_add(h.max(0)));
    let tx0 = (bx0 / native).clamp(0, n - 1);
    let tx1 = (((bx1 - 1).max(0)) / native).clamp(0, n - 1);
    let ty0 = (by0 / native).clamp(0, n - 1);
    let ty1 = (((by1 - 1).max(0)) / native).clamp(0, n - 1);

    let rows = crate::dbq::timed(
        "tile_versions",
        sqlx::query_as::<_, (i16, i32, i32, i64)>(
            "SELECT z, x, y, seq FROM tiles
             WHERE z = $1 AND x BETWEEN $2 AND $3 AND y BETWEEN $4 AND $5 AND seq > 0",
        )
        .bind(z as i16)
        .bind(tx0 as i32)
        .bind(tx1 as i32)
        .bind(ty0 as i32)
        .bind(ty1 as i32)
        .fetch_all(read_pool(app)),
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    let mut map = MAP.lock();
    for (tz, tx, ty, seq) in rows {
        let coord = (i64::from(tz), i64::from(tx), i64::from(ty));
        map.insert(coord, seq);
        out.push((coord, seq));
    }
    drop(map);
    Ok(out)
}

/// `GET /api/versions?bbox=x,y,w,h&z=<zoom>` —
/// `{tiles: [{key: "z/x/y", version}]}` per `tileVersionsResponseSchema`.
#[derive(Deserialize)]
pub struct VersionsQuery {
    bbox: String,
    z: i64,
}

pub async fn versions(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<VersionsQuery>,
) -> AppResult<Json<serde_json::Value>> {
    let parts: Vec<&str> = q.bbox.split(',').collect();
    let [x, y, w, h] = parts.as_slice() else {
        return Err(AppError::BadRequest("bbox must be x,y,w,h"));
    };
    let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
        x.trim().parse::<i64>(),
        y.trim().parse::<i64>(),
        w.trim().parse::<i64>(),
        h.trim().parse::<i64>(),
    ) else {
        return Err(AppError::BadRequest("bbox must be integers"));
    };
    let tiles = in_bbox(&app, q.z, x, y, w, h).await?;
    let tiles: Vec<_> = tiles
        .into_iter()
        .map(|((z, x, y), version)| json!({ "key": format!("{z}/{x}/{y}"), "version": version }))
        .collect();
    Ok(Json(json!({ "tiles": tiles })))
}

/// The `versions` NOTIFY payload for one rendered tile (published by the
/// write-through so every pod's map and every area subscriber learn of
/// the new version).
pub fn payload_for(z: i64, x: i64, y: i64, version: i64) -> String {
    json!({
        "type": "versions",
        "tiles": [{ "key": format!("{z}/{x}/{y}"), "version": version }]
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_keys_only() {
        assert_eq!(parse_key("16/2/3"), Some((16, 2, 3)));
        assert_eq!(parse_key("0/0/0"), Some((0, 0, 0)));
        assert_eq!(parse_key("16/2"), None);
        assert_eq!(parse_key("16/2/3/4"), None);
        assert_eq!(parse_key("16/x/3"), None);
        // Out-of-world coords are not valid tiles.
        assert_eq!(parse_key("21/0/0"), None);
        assert_eq!(parse_key("4/999999/999999"), None);
    }

    #[test]
    fn note_advances_only() {
        note(19, 7, 7, 5);
        note(19, 7, 7, 3); // stale late write must not regress
        let v = MAP.lock().get(&(19, 7, 7)).copied();
        assert_eq!(v, Some(5));
        note(19, 7, 7, 0); // zero = never rendered; ignored
        let v = MAP.lock().get(&(19, 7, 7)).copied();
        assert_eq!(v, Some(5));
    }

    #[test]
    fn notes_payload_tiles() {
        note_payload(r#"{"type":"versions","tiles":[{"key":"18/1/1","version":9}]}"#);
        assert_eq!(MAP.lock().get(&(18, 1, 1)).copied(), Some(9));
        // Malformed shapes are ignored, not fatal.
        note_payload("not json");
        note_payload(r#"{"type":"place","seq":3}"#);
        note_payload(r#"{"type":"versions","tiles":{}"#);
    }

    #[test]
    fn payload_round_trips_through_note() {
        let p = payload_for(20, 3, 4, 12);
        note_payload(&p);
        assert_eq!(MAP.lock().get(&(20, 3, 4)).copied(), Some(12));
    }
}
