//! In-process cache-invalidation hook for the versioned tile read path.
//! The API registers its `versions::note` at boot; the worker binary
//! leaves it unset (a no-op — a separate process has no API caches to
//! invalidate). Keeps `iac-worker` free of API types while the loops
//! run in-process during the transitional window.

use parking_lot::Mutex;

type NoteFn = fn(i64, i64, i64, i64);

static NOTE_HOOK: Mutex<Option<NoteFn>> = Mutex::new(None);

/// Register the API's note fn (called once from `build_app`).
pub fn register(hook: NoteFn) {
    *NOTE_HOOK.lock() = Some(hook);
}

/// Fire the hook if one is registered; no-op otherwise.
pub fn notify(z: i64, x: i64, y: i64, version: i64) {
    if let Some(hook) = *NOTE_HOOK.lock() {
        hook(z, x, y, version);
    }
}
