//! idol-appreciation.club backend: HTTP API + tile renderer + static host.
//!
//! Library crate so integration tests can boot the real router.
//! (Image-digest drift source for the images-bot exercise, 2026-10-14.)

pub mod api;
pub mod auth;
pub mod events;
pub mod handlers;
pub mod place;
pub mod rate;
pub mod router;

pub mod state;
pub mod statics;
pub mod tiles;
pub mod versions;

// The shared foundation lives in `iac-core` and the pixel work in
// `iac-worker` (workspace split); re-export them at the crate root so
// `crate::config`, `crate::img` and every existing
// `idol_appreciation_club_backend::*` path keeps resolving unchanged.
pub use iac_core::{config, dbq, error, metrics, store, telemetry, world};

use crate::state::AppState;
use std::sync::Arc;

/// Bootstrap everything (pool + migrations, object store, events bus) and
/// return the app state plus the fully wired router.
pub async fn build_app(cfg: config::Config) -> anyhow::Result<(state::App, axum::Router)> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(cfg.database_pool_max)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&cfg.database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    let store = store::from_config(&cfg)?;
    let (events_tx, _) = tokio::sync::broadcast::channel(1024);
    // Read-replica pool for the versioned read path (small: point lookups
    // only). Absent DATABASE_RO_URL ⇒ the main pool serves them.
    let pool_ro = match &cfg.database_ro_url {
        Some(url) => Some(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(3)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect(url)
                .await?,
        ),
        None => None,
    };

    let state = Arc::new(AppState {
        tile_cache: tiles::TileCache::new(cfg.tile_cache_entries),
        pool,
        pool_ro,
        store,
        events_tx: events_tx.clone(),
        login_limiter: rate::LoginLimiter::new(),
        cfg,
    });

    tokio::spawn(events::spawn_listener(
        state.pool.clone(),
        events_tx.clone(),
    ));

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
