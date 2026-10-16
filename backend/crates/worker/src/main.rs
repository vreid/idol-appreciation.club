//! The dedicated worker binary: render_jobs claim loops + the ingest
//! consumer, WITHOUT the HTTP server. One process, N loops (the claim
//! query is the only coordination — horizontal scale is more replicas).
//! Deployed by the chart's worker Deployment, values-gated dark
//! (`backend.workerReplicas: 0` default): pods exist only when the
//! cutover scales them (docs/backend.md "The ingest pipeline",
//! "Cutover order matters").

use iac_core::store;
use iac_worker::ctx::WorkerCtx;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = iac_core::telemetry::init();

    let cfg = iac_core::config::Config::from_env();
    iac_core::telemetry::set_db_endpoint(&cfg.database_url);
    iac_worker::img::init()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(cfg.database_pool_max)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&cfg.database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    let store = store::from_config(&cfg)?;
    let ctx = WorkerCtx::new(cfg, pool, store);

    let loops = ctx.cfg.render_pipeline_workers;
    let ingest_on = ctx.cfg.ingest_pipeline_enabled;
    if loops == 0 && !ingest_on {
        tracing::warn!(
            "worker binary started with RENDER_PIPELINE_WORKERS=0 and \
             INGEST_PIPELINE_ENABLED=false: nothing to do; exiting"
        );
        return Ok(());
    }
    for _ in 0..loops {
        iac_worker::render_jobs::spawn_worker(&ctx);
    }
    if ingest_on {
        iac_worker::ingest::spawn_consumer(&ctx);
    }
    tracing::info!(
        render_loops = loops,
        ingest_consumer = ingest_on,
        "worker loops running"
    );

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("signal handler");
    let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("signal handler");
    tokio::select! {
        _ = term.recv() => {},
        _ = int.recv() => {},
    }
    tracing::info!("shutdown signal received");
    iac_core::telemetry::shutdown();
    Ok(())
}
