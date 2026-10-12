//! SSE event stream fed by Postgres LISTEN/NOTIFY.

use crate::state::App;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// Background task: forward `events` channel payloads to the broadcast bus.
/// Postgres delivers each NOTIFY exactly once per connection at commit time.
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
        // No subscribers → lagged/empty is fine; NOTIFY was still delivered.
        let _ = tx.send(payload);
    }
}

/// GET /api/events — SSE stream of place/remove events.
pub async fn sse(State(app): State<App>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(payload) => Some(Ok(Event::default().data(payload))),
        // Lagged subscribers just skip missed events.
        Err(_lagged) => None,
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}
