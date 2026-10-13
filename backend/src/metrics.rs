//! Minimal in-process metrics with Prometheus text exposition (format
//! version 0.0.4). Hand-rolled instead of pulling the `metrics` +
//! `prometheus` stacks: the series set is small and fixed, so everything is
//! pre-created atomics (no locks, no registry strings) on the hot path and
//! `render` is a pure formatting walk.
//!
//! Aggregates only — no usernames, keys, or request payloads.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

/// Log-ish buckets in seconds, spanning sub-ms S3 hits to multi-second
/// cold pyramid renders.
const WIDE_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub struct Histogram<const N: usize> {
    buckets: &'static [f64],
    /// Bucket i counts observations <= buckets[i].
    counts: [AtomicU64; N],
    /// Cumulative nanoseconds.
    sum_nanos: AtomicU64,
    count: AtomicU64,
}

impl<const N: usize> Histogram<N> {
    const fn new(buckets: &'static [f64]) -> Self {
        Self {
            buckets,
            counts: [const { AtomicU64::new(0) }; N],
            sum_nanos: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, d: Duration) {
        let secs = d.as_secs_f64();
        let i = self
            .buckets
            .partition_point(|&b| b < secs)
            .min(self.counts.len() - 1);
        self.counts[i].fetch_add(1, Ordering::Relaxed);
        self.sum_nanos
            .fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn render(&self, name: &str, labels: &str) -> String {
        let mut out = String::with_capacity(64 * (N + 4));
        let count = self.count.load(Ordering::Relaxed);
        let mut cumulative = 0u64;
        for (i, b) in self.buckets.iter().enumerate() {
            cumulative += self.counts[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "{name}_bucket{{{labels}le=\"{b:?}\"}} {cumulative}\n"
            ));
        }
        out.push_str(&format!("{name}_bucket{{{labels}le=\"+Inf\"}} {count}\n"));
        let sum_secs = self.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9;
        out.push_str(&format!("{name}_sum{{{labels}}} {sum_secs}\n"));
        out.push_str(&format!("{name}_count{{{labels}}} {count}\n"));
        out
    }
}

pub struct Metrics {
    /// Successful POST /api/place completions (201 only).
    place_requests: AtomicU64,
    place_duration: Histogram<12>,
    /// GET /api/tiles by resolution path.
    tile_requests: [AtomicU64; 6],
    tile_duration: [Histogram<12>; 6],
    /// Background re-render queue entries the worker completed (actual
    /// renders and cheap already-fresh rechecks alike).
    background_renders: AtomicU64,
    background_render_duration: Histogram<12>,
    /// Object storage calls: [op=get, put][ok=false, true].
    s3_ops: [[AtomicU64; 2]; 2],
    s3_duration: [Histogram<12>; 2],
    /// Alpha-composited chunk writes during placements.
    chunk_composites: AtomicU64,
}

pub const TILE_OUTCOMES: [&str; 6] = [
    "hit_mem",
    "hit_s3",
    "render",
    "empty",
    "stale_served",
    "shed",
];

pub static METRICS: LazyLock<Metrics> = LazyLock::new(|| Metrics {
    place_requests: AtomicU64::new(0),
    place_duration: Histogram::new(WIDE_BUCKETS),
    tile_requests: std::array::from_fn(|_| AtomicU64::new(0)),
    tile_duration: std::array::from_fn(|_| Histogram::new(WIDE_BUCKETS)),
    background_renders: AtomicU64::new(0),
    background_render_duration: Histogram::new(WIDE_BUCKETS),
    s3_ops: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
    s3_duration: std::array::from_fn(|_| Histogram::new(WIDE_BUCKETS)),
    chunk_composites: AtomicU64::new(0),
});

pub fn place_done(elapsed: Duration) {
    let m = &*METRICS;
    m.place_requests.fetch_add(1, Ordering::Relaxed);
    m.place_duration.observe(elapsed);
}

pub fn tile_done(outcome: usize, elapsed: Duration) {
    let m = &*METRICS;
    m.tile_requests[outcome].fetch_add(1, Ordering::Relaxed);
    m.tile_duration[outcome].observe(elapsed);
}

pub fn tile_outcome(outcome: &str) -> usize {
    TILE_OUTCOMES
        .iter()
        .position(|&o| o == outcome)
        .expect("known tile outcome")
}

/// One background render-queue entry processed to completion.
pub fn background_render_done(elapsed: Duration) {
    let m = &*METRICS;
    m.background_renders.fetch_add(1, Ordering::Relaxed);
    m.background_render_duration.observe(elapsed);
}

pub fn s3_done(op: usize, ok: bool, elapsed: Duration) {
    let m = &*METRICS;
    m.s3_ops[op][usize::from(ok)].fetch_add(1, Ordering::Relaxed);
    m.s3_duration[op].observe(elapsed);
}

pub const S3_OPS: [&str; 2] = ["get", "put"];

pub fn chunk_composited() {
    METRICS.chunk_composites.fetch_add(1, Ordering::Relaxed);
}

/// Exposition of all series. `pool` supplies the sqlx connection gauges;
/// the render-queue stats come straight from the live queue.
pub fn render(pool: &sqlx::PgPool, queue_depth: u64, queue_dropped: u64) -> String {
    let m = &*METRICS;
    let mut out = String::with_capacity(4096);

    out.push_str("# HELP place_requests_total Completed placements.\n");
    out.push_str("# TYPE place_requests_total counter\n");
    out.push_str(&format!(
        "place_requests_total {}\n",
        m.place_requests.load(Ordering::Relaxed)
    ));

    out.push_str(
        "# HELP place_duration_seconds Placement pipeline latency (multipart through NOTIFY).\n# TYPE place_duration_seconds histogram\n",
    );
    out.push_str(&m.place_duration.render("place_duration_seconds", ""));

    out.push_str(
        "# HELP tile_requests_total Tile GETs by resolution path.\n# TYPE tile_requests_total counter\n",
    );
    for (i, name) in TILE_OUTCOMES.iter().enumerate() {
        out.push_str(&format!(
            "tile_requests_total{{outcome=\"{name}\"}} {}\n",
            m.tile_requests[i].load(Ordering::Relaxed)
        ));
    }

    out.push_str(
        "# HELP tile_duration_seconds Tile GET latency by resolution path.\n# TYPE tile_duration_seconds histogram\n",
    );
    for (i, name) in TILE_OUTCOMES.iter().enumerate() {
        out.push_str(
            &m.tile_duration[i].render("tile_duration_seconds", &format!("outcome=\"{name}\",")),
        );
    }

    out.push_str(
        "# HELP s3_ops_total Object storage calls by operation and result.\n# TYPE s3_ops_total counter\n",
    );
    for (op, name) in S3_OPS.iter().enumerate() {
        for (ok, res) in [(0usize, "false"), (1, "true")] {
            out.push_str(&format!(
                "s3_ops_total{{op=\"{name}\",ok=\"{res}\"}} {}\n",
                m.s3_ops[op][ok].load(Ordering::Relaxed)
            ));
        }
    }

    out.push_str(
        "# HELP s3_duration_seconds Object storage call latency by operation.\n# TYPE s3_duration_seconds histogram\n",
    );
    for (op, name) in S3_OPS.iter().enumerate() {
        out.push_str(&m.s3_duration[op].render("s3_duration_seconds", &format!("op=\"{name}\",")));
    }

    out.push_str(
        "# HELP chunk_composites_total Chunk alpha-composites performed while placing.\n# TYPE chunk_composites_total counter\n",
    );
    out.push_str(&format!(
        "chunk_composites_total {}\n",
        m.chunk_composites.load(Ordering::Relaxed)
    ));

    out.push_str(
        "# HELP tile_background_renders_total Background render-queue entries completed (renders and cheap already-fresh rechecks).\n# TYPE tile_background_renders_total counter\n",
    );
    out.push_str(&format!(
        "tile_background_renders_total {}\n",
        m.background_renders.load(Ordering::Relaxed)
    ));
    out.push_str(
        "# HELP tile_background_render_duration_seconds Background render-queue entry processing time.\n# TYPE tile_background_render_duration_seconds histogram\n",
    );
    out.push_str(
        &m.background_render_duration
            .render("tile_background_render_duration_seconds", ""),
    );

    out.push_str(
        "# HELP tile_render_queue_depth Background re-render queue length (coords waiting).\n# TYPE tile_render_queue_depth gauge\n",
    );
    out.push_str(&format!("tile_render_queue_depth {queue_depth}\n"));
    out.push_str(
        "# HELP tile_render_queue_dropped_total Enqueue attempts shed by the queue cap (retried by the next stale GET).\n# TYPE tile_render_queue_dropped_total counter\n",
    );
    out.push_str(&format!(
        "tile_render_queue_dropped_total {queue_dropped}\n"
    ));

    out.push_str(
        "# HELP db_pool_connections sqlx connection pool state.\n# TYPE db_pool_connections gauge\n",
    );
    let idle = pool.num_idle();
    let size = pool.size();
    out.push_str(&format!("db_pool_connections{{state=\"idle\"}} {idle}\n"));
    out.push_str(&format!(
        "db_pool_connections{{state=\"in_use\"}} {}\n",
        size.saturating_sub(u32::try_from(idle).unwrap_or(u32::MAX))
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the exposition back and validate the structural guarantees the
    /// format depends on: TYPE declarations, label syntax, ascending bucket
    /// `le` values ending in +Inf, and bucket Inf == _count.
    #[tokio::test]
    async fn exposition_round_trips() {
        let m = &*METRICS;
        m.place_requests.store(7, Ordering::Relaxed);
        m.tile_requests[0].store(3, Ordering::Relaxed);
        m.tile_duration[0].observe(Duration::from_millis(12));
        m.tile_duration[0].observe(Duration::from_millis(480));
        m.s3_ops[1][1].store(9, Ordering::Relaxed);

        let text = render(&render_pool_stub(), 3, 1);

        let mut histogram_inf = std::collections::HashMap::new();
        let mut histogram_count = std::collections::HashMap::new();
        let mut last_le: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for line in text.lines() {
            if let Some(series) = line.strip_prefix("tile_duration_seconds_bucket") {
                let (labels, value) = split_series(series);
                assert!(labels.contains("outcome="), "{line}");
                let outcome = labels
                    .split("outcome=\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .expect("outcome label")
                    .to_string();
                let le = labels
                    .split("le=\"")
                    .nth(1)
                    .and_then(|s| s.trim_end_matches(['"', '}']).parse::<f64>().ok())
                    .expect("le parse");
                let v: u64 = value.parse().expect("bucket value");
                if le == f64::INFINITY {
                    histogram_inf.insert(labels.to_string(), v);
                } else {
                    let prev = last_le.insert(outcome.clone(), le);
                    assert!(prev.is_none_or(|p| le > p), "buckets ascending: {line}");
                }
            }
            if let Some(series) = line.strip_prefix("tile_duration_seconds_count") {
                let (labels, value) = split_series(series);
                let outcome = labels
                    .split("outcome=\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .expect("outcome label")
                    .to_string();
                histogram_count.insert(outcome, value.parse::<u64>().unwrap());
            }
            if line.starts_with("place_requests_total ") {
                assert_eq!(split_series(line).1, "7");
            }
            if line.starts_with("s3_ops_total{op=\"put\",ok=\"true\"}") {
                assert_eq!(split_series(line).1, "9");
            }
        }
        assert!(text.contains("# TYPE place_duration_seconds histogram"));
        assert!(text.contains("# TYPE tile_requests_total counter"));
        assert!(text.contains("tile_requests_total{outcome=\"empty\"} 0"));
        assert!(text.contains("tile_requests_total{outcome=\"stale_served\"} 0"));
        assert!(text.contains("tile_requests_total{outcome=\"shed\"} 0"));
        assert!(text.contains("tile_render_queue_depth 3"));
        assert!(text.contains("tile_render_queue_dropped_total 1"));
        assert!(text.contains("# TYPE tile_background_renders_total counter"));
        // The two observed samples must appear in the +Inf bucket, which
        // must equal _count for every series.
        let hit_mem_inf = histogram_inf
            .iter()
            .find(|(l, _)| l.contains("hit_mem"))
            .map(|(_, v)| *v);
        assert_eq!(hit_mem_inf, Some(2));
        for (labels, inf) in &histogram_inf {
            let outcome = labels
                .split("outcome=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .expect("outcome label");
            assert_eq!(
                Some(inf),
                histogram_count.get(outcome),
                "Inf bucket must equal count for {labels}"
            );
        }
    }

    fn split_series(line: &str) -> (&str, &str) {
        match line.rsplit_once(' ') {
            Some((series, value)) => (series, value),
            None => panic!("unparseable line: {line}"),
        }
    }

    /// A pool stub for rendering — metrics rendering only reads pool gauge
    /// numbers, and this test constructs no connections. Uses a real closed
    /// pool via sqlx's pool options (zero connections).
    fn render_pool_stub() -> sqlx::PgPool {
        sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
            .idle_timeout(Duration::from_millis(1))
            .connect_lazy("postgres://localhost/metrics_render_stub")
            .expect("lazy pool")
    }
}
