//! Shared application state.

use crate::config::Config;
use crate::store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{broadcast, Mutex as TokioMutex};

/// Per-string-key async mutexes. Placement compositing and moderation
/// replay acquire the full sorted set of chunk keys they touch before
/// opening their DB transaction, so paint order always matches `seq`
/// order and lock ordering is deadlock-free. Tile rendering uses the same
/// mechanism keyed by "z/x/y" (strictly descending zoom ⇒ no cycles).
pub struct KeyLocks {
    map: StdMutex<HashMap<String, Arc<TokioMutex<()>>>>,
}

impl AppState {
    /// The narrow execution context the worker loops run against (the
    /// worker binary constructs `WorkerCtx` standalone; the API derives
    /// it from the HTTP state).
    pub fn worker_ctx(&self) -> iac_worker::ctx::WorkerCtx {
        iac_worker::ctx::WorkerCtx::new(self.cfg.clone(), self.pool.clone(), self.store.clone())
    }
}

impl KeyLocks {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for KeyLocks {
    fn default() -> Self {
        Self {
            map: StdMutex::new(HashMap::new()),
        }
    }
}

impl KeyLocks {
    /// Acquire all keys in sorted order; the returned guards release them.
    pub async fn lock_sorted(&self, keys: &[String]) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
        let mut raw: Vec<(String, Arc<TokioMutex<()>>)> = {
            let mut map = self.map.lock().expect("key lock map");
            keys.iter()
                .map(|k| (k.clone(), map.entry(k.clone()).or_default().clone()))
                .collect()
        };
        raw.sort_by(|a, b| a.0.cmp(&b.0));
        raw.dedup_by(|a, b| a.0 == b.0);
        let mut guards = Vec::with_capacity(raw.len());
        for (_, m) in &raw {
            guards.push(m.clone().lock_owned().await);
        }
        // Drop map entries nobody references to keep memory bounded.
        self.map
            .lock()
            .expect("key lock map")
            .retain(|_, v| Arc::strong_count(v) > 1);
        guards
    }
}

pub struct AppState {
    pub cfg: Config,
    pub pool: sqlx::PgPool,
    /// Optional read-replica pool (DATABASE_RO_URL): versioned tile-read
    /// lookups prefer it when present. None ⇒ `pool` serves everything.
    pub pool_ro: Option<sqlx::PgPool>,
    pub store: Store,
    pub chunk_locks: KeyLocks,
    /// NOTIFY "events" payloads fan out to SSE subscribers here.
    pub events_tx: broadcast::Sender<String>,
    /// Tile render serialization per "z/x/y".
    pub tile_locks: KeyLocks,
    /// Encoded tile bytes keyed by (z, x, y, seq).
    pub tile_cache: crate::tiles::TileCache,
    /// Bounded background re-render queue (single worker; see
    /// `tiles::RenderQueue`).
    pub render_queue: crate::tiles::RenderQueue,
    /// Cap on concurrent full-pyramid (z < BASE_Z) renders (issue #120).
    /// Entry GETs beyond the cap are shed (202 + Retry-After) instead of
    /// stacking another minutes-long walk onto the pool.
    pub pyramid_gate: crate::tiles::PyramidGate,
    /// Per-IP and per-(username, IP) token buckets for POST /api/auth/login.
    pub login_limiter: crate::rate::LoginLimiter,
}

pub type App = Arc<AppState>;
