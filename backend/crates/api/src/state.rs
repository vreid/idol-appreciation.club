//! Shared application state.

use crate::config::Config;
use crate::store::Store;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct AppState {
    pub cfg: Config,
    pub pool: sqlx::PgPool,
    /// Optional read-replica pool (DATABASE_RO_URL): versioned tile-read
    /// lookups prefer it when present. None ⇒ `pool` serves everything.
    pub pool_ro: Option<sqlx::PgPool>,
    pub store: Store,
    /// NOTIFY "events" payloads fan out to SSE subscribers here.
    pub events_tx: broadcast::Sender<String>,
    /// Encoded tile bytes keyed by (z, x, y, seq).
    pub tile_cache: crate::tiles::TileCache,
    /// Per-IP and per-(username, IP) token buckets for POST /api/auth/login.
    pub login_limiter: crate::rate::LoginLimiter,
}

pub type App = Arc<AppState>;
