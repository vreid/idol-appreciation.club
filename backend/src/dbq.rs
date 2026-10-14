//! Per-query-family SQL timing. Every sqlx execution site wraps its
//! executor future in [`timed`] with a static family label (the
//! `query` label values of `db_query_total` / `db_query_duration_seconds`,
//! see `metrics::DB_FAMILIES`). Instrumentation only: the future is
//! awaited exactly once and its output — success or error — passes
//! through untouched.

use std::future::Future;
use std::time::Instant;
use tracing::Instrument as _;

/// Time one labeled query-family execution and, when running under an
/// existing span, emit a `postgresql <family>` span with the standard
/// db.* attributes plus the pool's endpoint label.
///
/// No-orphan rule (docs § Observability): the span is created only under
/// an existing span — the workers' once-a-second queue polls run spanless
/// and stay spanless instead of becoming root traces of their own.
pub async fn timed<T>(family: &str, fut: impl Future<Output = T>) -> T {
    let f = crate::metrics::db_family(family);
    let endpoint = crate::telemetry::db_endpoint();
    let span = if tracing::Span::current().is_none() {
        tracing::Span::none()
    } else {
        tracing::debug_span!(
            "postgresql",
            "db.system.name" = "postgresql",
            "db.query.summary" = family,
            "db.operation.name" = family,
            db.endpoint = endpoint,
        )
    };
    let t0 = Instant::now();
    let out = fut.instrument(span).await;
    crate::metrics::db_done(f, t0.elapsed());
    out
}
