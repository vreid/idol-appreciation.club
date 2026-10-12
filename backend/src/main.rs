//! idol-appreciation.club backend: HTTP API + tile renderer + static host.

mod api;
mod auth;
mod chunks;
mod config;
mod error;
mod events;
mod handlers;
mod img;
mod place;
mod state;
mod statics;
mod store;
mod tiles;
mod world;

use crate::state::{AppState, KeyLocks};
use axum::routing::{delete, get, post};
use axum::Router;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .init();
    img::init()?;

    let cfg = config::Config::from_env();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&cfg.database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let store = store::from_config(&cfg)?;
    let (events_tx, _) = tokio::sync::broadcast::channel(1024);

    let state = Arc::new(AppState {
        tile_cache: tiles::TileCache::new(cfg.tile_cache_entries),
        cfg,
        pool,
        store,
        chunk_locks: KeyLocks::new(),
        events_tx: events_tx.clone(),
        tile_locks: KeyLocks::new(),
    });

    tokio::spawn(events::spawn_listener(
        state.pool.clone(),
        events_tx.clone(),
    ));

    let app = router(state.clone());
    let addr = state.cfg.listen_addr;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "backend listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

pub fn router(state: state::App) -> Router {
    let max_body = state.cfg.max_upload_bytes + (1 << 20); // multipart slack
    let api = Router::new()
        .route("/api/health", get(api::health))
        .route("/api/auth/register", post(handlers::register))
        .route("/api/auth/login", post(handlers::login))
        .route("/api/auth/logout", post(handlers::logout))
        .route("/api/auth/me", get(handlers::me))
        .route("/api/place", post(place::place))
        .route("/api/tiles/{z}/{x}/{y}.webp", get(tiles::get_tile))
        .route("/api/placements/near", get(api::near))
        .route("/api/center", get(api::center))
        .route("/api/events", get(events::sse))
        .route("/api/reports", post(api::report))
        .route("/api/images/{sha}/count", get(api::image_count))
        .route("/api/admin/placements/{id}", delete(place::admin_delete))
        .layer(axum::extract::DefaultBodyLimit::max(max_body))
        .with_state(state.clone());
    api.fallback(|req: axum::http::Request<axum::body::Body>| async move {
        let uri = req.uri().clone();
        statics::fallback(state.clone(), uri, req).await
    })
    .layer(tower_http::trace::TraceLayer::new_for_http())
}

async fn shutdown_signal() {
    let mut term = signal(SignalKind::terminate()).expect("signal handler");
    let mut int = signal(SignalKind::interrupt()).expect("signal handler");
    tokio::select! {
        _ = term.recv() => {},
        _ = int.recv() => {},
    }
    tracing::info!("shutdown signal received");
}
