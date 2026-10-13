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
}

pub type App = Arc<AppState>;
