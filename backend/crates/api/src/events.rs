//! SSE event stream fed by Postgres LISTEN/NOTIFY. `versions` payloads
//! are coalesced into 1-second-per-connection events, filtered to the
//! subscriber's area; `place`/`remove` pass through as they arrive.

use crate::state::App;
use crate::world::{is_valid_tile, tile_native_size, MAX_Z};
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Deserialize;
use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

/// Background task: forward `events` channel payloads to the broadcast bus.
/// Postgres delivers each NOTIFY exactly once per connection at commit time.
/// `versions` payloads additionally refresh this pod's version map so a
/// render completed on one pod updates every pod's /api/versions.
pub async fn spawn_listener(pool: sqlx::PgPool, tx: broadcast::Sender<String>) {
    loop {
        match run_listener(&pool, &tx).await {
            Ok(never) => match never {},
            Err(e) => {
                tracing::error!(error = ?e, "events listener failed; retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn run_listener(
    pool: &sqlx::PgPool,
    tx: &broadcast::Sender<String>,
) -> Result<std::convert::Infallible, anyhow::Error> {
    let mut listener = sqlx::postgres::PgListener::connect_with(pool).await?;
    listener.listen("events").await?;
    tracing::info!("listening on pg channel events");
    loop {
        let notification = listener.recv().await?;
        let payload = notification.payload().to_string();
        // Cross-pod version-map refresh: a render noted on another pod.
        if payload.contains("\"type\":\"versions\"") {
            crate::versions::note_payload(&payload);
        }
        // No subscribers → lagged/empty is fine; NOTIFY was still delivered.
        let _ = tx.send(payload);
    }
}

/// Optional subscription area for /api/events (`?bbox=x,y,w,h&z=<zoom>`,
/// the same shape as /api/versions). Absent = all areas (the behavior of
/// every backend deployed before the versions lane — old clients pass
/// nothing and see everything, as today).
#[derive(Deserialize, Default)]
pub struct AreaQuery {
    bbox: Option<String>,
    z: Option<i64>,
}

fn area_tile_set(area: &AreaQuery) -> Option<HashSet<(i64, i64, i64)>> {
    let bbox = area.bbox.as_deref()?;
    let z = area.z?;
    if !(0..=MAX_Z).contains(&z) {
        return None;
    }
    let parts: Vec<&str> = bbox.split(',').collect();
    let [x, y, w, h] = parts.as_slice() else {
        return None;
    };
    let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
        x.trim().parse::<i64>(),
        y.trim().parse::<i64>(),
        w.trim().parse::<i64>(),
        h.trim().parse::<i64>(),
    ) else {
        return None;
    };
    let native = tile_native_size(z);
    let n = 1i64 << z;
    let tx0 = (x.max(0) / native).clamp(0, n - 1);
    let tx1 = (((x.saturating_add(w.max(0)) - 1).max(0)) / native).clamp(0, n - 1);
    let ty0 = (y.max(0) / native).clamp(0, n - 1);
    let ty1 = (((y.saturating_add(h.max(0)) - 1).max(0)) / native).clamp(0, n - 1);
    let mut set = HashSet::new();
    for tx in tx0..=tx1 {
        for ty in ty0..=ty1 {
            if is_valid_tile(z, tx, ty) {
                set.insert((z, tx, ty));
            }
        }
    }
    Some(set)
}

/// One SSE connection's pump: forwards place/remove as they arrive and
/// coalesces `versions` payloads into one event per second, trimmed to
/// the connection's area (latest version wins per tile).
async fn connection_pump(
    mut rx: broadcast::Receiver<String>,
    area: Option<HashSet<(i64, i64, i64)>>,
    out: mpsc::Sender<Result<Event, Infallible>>,
) {
    let mut pending: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut flush = std::pin::pin!(tokio::time::sleep(Duration::from_secs(3600)));
    loop {
        tokio::select! {
            item = rx.recv() => {
                let payload = match item {
                    Ok(p) => p,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if payload.contains("\"type\":\"versions\"") {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
                        if let Some(tiles) = v.get("tiles").and_then(|t| t.as_array()) {
                            // An empty→non-empty transition opens the 1s
                            // coalescing window; later payloads within it
                            // just fold in.
                            let was_empty = pending.is_empty();
                            for t in tiles {
                                if let (Some(key), Some(version)) = (
                                    t.get("key").and_then(|k| k.as_str()),
                                    t.get("version").and_then(|n| n.as_i64()),
                                ) {
                                    pending.insert(key.to_string(), version);
                                }
                            }
                            if was_empty && !pending.is_empty() {
                                flush.as_mut().reset(
                                    tokio::time::Instant::now() + Duration::from_secs(1),
                                );
                            }
                        }
                    }
                } else {
                    // Pass-through event (place/remove); keep its JSON as data.
                    let _ = out.send(Ok(Event::default().data(payload))).await;
                }
            }
            _ = &mut flush => {
                if pending.is_empty() {
                    // Idle: rearm long so empty wakes are rare.
                    flush.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3600));
                    continue;
                }
                let tiles: Vec<serde_json::Value> = pending
                    .drain()
                    .filter(|(key, _)| match &area {
                        Some(set) => {
                            let mut it = key.split('/');
                            match (it.next(), it.next(), it.next(), it.next()) {
                                (Some(z), Some(x), Some(y), None) => set.contains(&(
                                    z.parse().unwrap_or(i64::MAX),
                                    x.parse().unwrap_or(i64::MAX),
                                    y.parse().unwrap_or(i64::MAX),
                                )),
                                _ => false,
                            }
                        }
                        None => true,
                    })
                    .map(|(key, version)| {
                        serde_json::json!({ "key": key, "version": version })
                    })
                    .collect();
                pending.shrink_to_fit();
                if !tiles.is_empty() {
                    let payload = serde_json::json!({ "type": "versions", "tiles": tiles });
                    let _ = out
                        .send(Ok(Event::default().data(payload.to_string())))
                        .await;
                }
                flush.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3600));
            }
        }
    }
}

/// GET /api/events — SSE stream of place/remove events plus 1s-coalesced
/// `versions` events for the subscribed area (`?bbox&z`, optional).
pub async fn sse(
    State(app): State<App>,
    axum::extract::Query(area): axum::extract::Query<AreaQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.events_tx.subscribe();
    let area_set = area_tile_set(&area);
    let (tx_out, rx_out) = mpsc::channel::<Result<Event, Infallible>>(256);
    tokio::spawn(connection_pump(rx, area_set, tx_out));
    let stream = ReceiverStream::new(rx_out);
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}
