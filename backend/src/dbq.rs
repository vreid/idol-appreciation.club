//! Per-query-family SQL timing. Every sqlx execution site wraps its
//! executor future in [`timed`] with a static family label (the
//! `query` label values of `db_query_total` / `db_query_duration_seconds`,
//! see `metrics::DB_FAMILIES`). Instrumentation only: the future is
//! awaited exactly once and its output — success or error — passes
//! through untouched.

use std::future::Future;
use std::time::Instant;

/// Time one labeled query-family execution.
pub async fn timed<T>(family: &str, fut: impl Future<Output = T>) -> T {
    let f = crate::metrics::db_family(family);
    let t0 = Instant::now();
    let out = fut.await;
    crate::metrics::db_done(f, t0.elapsed());
    out
}
