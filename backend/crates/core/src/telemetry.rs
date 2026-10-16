//! Telemetry: JSON logs always, OpenTelemetry spans whenever an OTLP
//! endpoint is configured (docs/scaling-architecture.md § Observability).
//!
//! Standard environment only — no app-level config knobs:
//! - `OTEL_EXPORTER_OTLP_ENDPOINT`: unset ⇒ logs only (zero behavior
//!   change; this is the prod default until a Collector exists).
//! - `OTEL_SERVICE_NAME`: defaults to `api` (worker binaries override).
//! - `OTEL_RESOURCE_ATTRIBUTES`: `k=v,k=v` forwarded into the resource
//!   (deployment version, environment, pod/node from the downward API).
//! - `OTEL_SDK_DISABLED=true`: force logs-only even with an endpoint set.
//! - `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG`: sampler selection,
//!   default `parentbased_traceidratio` at 5% (tile traffic is the bulk).
//!
//! Log/trace join: every JSON line carries `trace_id`/`span_id` via the
//! current-span block, and requests carry `request_id` (set by the
//! router's request-id middleware, echoed on the response).

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, TracerProviderBuilder};
use opentelemetry_sdk::Resource;
use std::sync::OnceLock;
use tracing_subscriber::layer::Layer as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static PROVIDER: OnceLock<Option<SdkTracerProvider>> = OnceLock::new();

/// Marker keeping the provider alive for the process lifetime.
pub struct Telemetry(());

/// Install the subscriber: JSON fmt layer always, OTel layer when an OTLP
/// endpoint is configured and the SDK is not disabled. Idempotence is not
/// attempted — binaries call this once, tests never (see no-op test).
pub fn init() -> Telemetry {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    // The filter gates the LOG layer only: `postgresql`/`vips` spans stay
    // DEBUG-quiet in stdout while still being created and exported to the
    // OTel layer (the doc's point about TraceLayer's DEBUG spans).
    let fmt = tracing_subscriber::fmt::layer()
        .json()
        .with_filter(tracing_subscriber::EnvFilter::new(filter));
    let registry = tracing_subscriber::registry().with(fmt);

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok();
    let disabled = matches!(
        std::env::var("OTEL_SDK_DISABLED").as_deref(),
        Ok("true") | Ok("1")
    );

    if let (Some(endpoint), false) = (endpoint, disabled) {
        match build_provider() {
            Ok(provider) => {
                // W3C traceparent in/out, so a Collector-fronted request
                // (or a test curl) propagates its trace into our spans.
                global::set_text_map_propagator(TraceContextPropagator::new());
                let tracer = provider.tracer("idol-appreciation-club-backend");
                registry
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .init();
                let _ = PROVIDER.set(Some(provider));
                tracing::info!(%endpoint, "telemetry: OTLP span export enabled");
            }
            Err(err) => {
                registry.init();
                let _ = PROVIDER.set(None);
                tracing::warn!(%endpoint, %err, "telemetry: OTLP init failed; logs only");
            }
        }
    } else {
        registry.init();
        let _ = PROVIDER.set(None);
    }
    Telemetry(())
}

/// Best-effort flush of pending spans on SIGTERM/SIGINT so a rolling
/// restart does not drop the batch processor's tail.
pub fn shutdown() {
    if let Some(Some(provider)) = PROVIDER.get() {
        let _ = provider.force_flush();
    }
}

fn build_provider() -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    // HTTP/protobuf with the default blocking client; endpoint, headers
    // and timeout come from the standard OTEL_EXPORTER_OTLP_* variables.
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()?;

    let sampler = match (
        std::env::var("OTEL_TRACES_SAMPLER").as_deref(),
        std::env::var("OTEL_TRACES_SAMPLER_ARG")
            .ok()
            .and_then(|v| v.parse::<f64>().ok()),
    ) {
        (Ok("always_on"), _) => Sampler::AlwaysOn,
        (Ok("always_off"), _) => Sampler::AlwaysOff,
        (Ok("traceidratio"), Some(ratio)) => Sampler::TraceIdRatioBased(ratio),
        (Ok("parentbased_always_on"), _) => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        (Ok("parentbased_always_off"), _) => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        (Ok("parentbased_traceidratio"), Some(ratio)) => {
            Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
        }
        _ => Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(0.05))),
    };

    let service_name = std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "api".to_string());
    let mut resource = Resource::builder_empty().with_service_name(service_name);
    if let Ok(attrs) = std::env::var("OTEL_RESOURCE_ATTRIBUTES") {
        for kv in attrs.split(',') {
            if let Some((k, v)) = kv.split_once('=') {
                if !k.is_empty() && !v.is_empty() {
                    resource = resource.with_attribute(opentelemetry::KeyValue::new(
                        k.trim().to_string(),
                        v.trim().to_string(),
                    ));
                }
            }
        }
    }

    Ok(TracerProviderBuilder::default()
        .with_batch_exporter(exporter)
        .with_sampler(sampler)
        .with_resource(resource.build())
        .build())
}

/// `spawn_blocking` with span context carried into the blocking thread:
/// libvips work otherwise runs in a disconnected trace (doc rule: carry
/// context into blocking threads).
pub fn blocking<T, F>(f: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
}

/// Which database endpoint the pool points at (`-rw`, `-ro`, pooler, or
/// bare primary) — parsed once from the pool URL host and stamped onto
/// every `postgresql` span so slow-query traces name the topology node.
pub fn set_db_endpoint(database_url: &str) {
    let label = database_url
        .split('@')
        .nth(1)
        .and_then(|hostport| hostport.split(['/', ':']).next())
        .unwrap_or("unknown");
    let label = if label.contains("-rw") {
        "rw"
    } else if label.contains("-ro") {
        "ro"
    } else if label.contains("pooler") || label.contains("-pgbouncer") {
        "pooler"
    } else {
        "primary"
    };
    let _ = DB_ENDPOINT.set(label);
}

static DB_ENDPOINT: OnceLock<&'static str> = OnceLock::new();
/// The parsed pool endpoint label ("" until `set_db_endpoint` runs; every
/// `postgresql` span carries it so traces name the topology node).
pub fn db_endpoint() -> &'static str {
    DB_ENDPOINT.get().copied().unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    /// Env vars are process-global; serialize the tests that set them.
    static ENV_LOCK: LazyLock<std::sync::Mutex<()>> = LazyLock::new(|| std::sync::Mutex::new(()));

    /// End-to-end: with an OTLP endpoint pointing at a local TCP sink,
    /// a span emitted and flushed must reach it as OTLP/HTTP bytes.
    #[test]
    fn spans_export_over_otlp_http() {
        let _env = ENV_LOCK.lock().unwrap();
        let sink = std::net::TcpListener::bind("127.0.0.1:0").expect("bind sink");
        let addr = sink.local_addr().unwrap();
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{addr}"));
        std::env::set_var("OTEL_SERVICE_NAME", "telemetry-test");
        // Root spans are sampled at the default 5% ratio; make this test
        // deterministic.
        std::env::set_var("OTEL_TRACES_SAMPLER", "always_on");
        std::env::remove_var("OTEL_SDK_DISABLED");

        let _t = crate::telemetry::init();
        tracing::debug_span!("postgresql", "db.system.name" = "postgresql").in_scope(|| {
            tracing::debug!("span body");
        });
        crate::telemetry::shutdown();

        sink.set_nonblocking(true).expect("nonblocking");
        let mut got = 0usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while got == 0 && std::time::Instant::now() < deadline {
            if let Ok((stream, _)) = sink.accept() {
                use std::io::Read;
                let mut s = stream;
                let mut buf = [0u8; 4096];
                got += s.read(&mut buf).unwrap_or(0);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        std::env::remove_var("OTEL_SERVICE_NAME");
        std::env::remove_var("OTEL_TRACES_SAMPLER");
        assert!(got > 0, "no OTLP bytes reached the sink");
    }
}
