//! idol-appreciation.club backend: HTTP API + tile renderer + static host.
//!
//! Library crate so integration tests can boot the real router.
//! (Image-digest drift source for the images-bot exercise, 2026-10-14.)

pub mod api;
pub mod auth;
pub mod chunks;
pub mod config;
pub mod dbq;
pub mod error;
pub mod events;
pub mod handlers;
pub mod img;
pub mod import;
pub mod metrics;
pub mod place;
pub mod rate;
pub mod render_jobs;
pub mod router;

pub mod state;
pub mod statics;
pub mod store;
pub mod telemetry;
pub mod tiles;
pub mod world;

use crate::state::{AppState, KeyLocks};
use std::sync::Arc;

/// Bootstrap everything (libvips, pool + migrations, object store, events
/// bus) and return the app state plus the fully wired router.
pub async fn build_app(cfg: config::Config) -> anyhow::Result<(state::App, axum::Router)> {
    img::init()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(cfg.database_pool_max)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&cfg.database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let store = store::from_config(&cfg)?;
    let (events_tx, _) = tokio::sync::broadcast::channel(1024);

    let render_queue = tiles::RenderQueue::new(pool.clone(), cfg.tile_render_queue_cap);
    // Two concurrent full-pyramid renders max: each is minutes of work;
    // with the connection-free walk (#120) they hold no pool connections,
    // but CPU/S3 bandwidth still argues for a bound.
    let pyramid_gate = tiles::PyramidGate::new(2);
    let state = Arc::new(AppState {
        tile_cache: tiles::TileCache::new(cfg.tile_cache_entries),
        pool,
        store,
        chunk_locks: KeyLocks::new(),
        events_tx: events_tx.clone(),
        tile_locks: KeyLocks::new(),
        render_queue,
        pyramid_gate,
        login_limiter: rate::LoginLimiter::new(),
        cfg,
    });

    tokio::spawn(events::spawn_listener(
        state.pool.clone(),
        events_tx.clone(),
    ));
    tiles::spawn_render_worker(&state);
    // Phase 3 pipeline (docs/scaling-architecture.md): DARK by default —
    // the worker (and the queue-only place/remove paths) only run when
    // RENDER_PIPELINE_ENABLED=true.
    if state.cfg.render_pipeline_enabled {
        render_jobs::spawn_worker(&state);
    }

    Ok((state.clone(), router::router(state)))
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("signal handler");
    tokio::select! {
        _ = term.recv() => {},
        _ = int.recv() => {},
    }
    tracing::info!("shutdown signal received");
    telemetry::shutdown();
}

/// Main entry point used by the binary.
pub async fn run() -> anyhow::Result<()> {
    let _telemetry = telemetry::init();

    let cfg = config::Config::from_env();
    telemetry::set_db_endpoint(&cfg.database_url);
    let addr = cfg.listen_addr;
    let (_state, app) = build_app(cfg).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "backend listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    telemetry::shutdown();
    Ok(())
}
