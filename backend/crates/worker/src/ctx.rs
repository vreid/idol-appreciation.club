//! The narrow execution context the worker loops run against: config,
//! pool and object store. Deliberately NOT the API's `AppState` — the
//! worker binary constructs this standalone (no axum, no HTTP state),
//! and the API builds it from its own state for the in-process loops
//! and the dark path's shared seams.

use iac_core::{config::Config, store::Store};
use std::sync::Arc;

#[derive(Clone)]
pub struct WorkerCtx {
    pub cfg: Config,
    pub pool: sqlx::PgPool,
    pub store: Store,
}

impl WorkerCtx {
    pub fn new(cfg: Config, pool: sqlx::PgPool, store: Store) -> Self {
        Self { cfg, pool, store }
    }
}

/// Type alias so call sites read the same as they did against `Arc<AppState>`.
pub type SharedWorkerCtx = Arc<WorkerCtx>;
