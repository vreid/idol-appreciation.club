//! Environment-driven configuration. Exact env names wired by infra.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub database_url: String,
    /// Optional read-replica pool (the CNPG `-ro` service) for versioned
    /// tile-read lookups. Absent ⇒ the main pool serves them.
    pub database_ro_url: Option<String>,
    /// sqlx pool ceiling. Stopgap for #120: deep-zoom renders can hold
    /// connections for minutes and starve the pool; idle-held connections
    /// are free on the managed PG. DATABASE_POOL_MAX overrides the default.
    pub database_pool_max: u32,
    /// S3-compatible endpoint. `file://…` selects a local-filesystem store
    /// (dev only); anything else is an S3 endpoint (UpCloud object storage).
    pub s3_endpoint: Option<String>,
    pub s3_bucket: String,
    pub s3_access_key_id: Option<String>,
    pub s3_secret_access_key: Option<String>,
    pub session_secret: Vec<u8>,
    pub public_base_url: String,
    pub static_dir: PathBuf,
    /// Paint cooldown: placement intersecting existing art (seconds).
    pub cooldown_paint_secs: i64,
    /// Placement into empty space (seconds).
    pub cooldown_empty_secs: i64,
    /// Max accepted upload bytes for the `image` part.
    pub max_upload_bytes: usize,
    /// Max decoded pixel count accepted for a placement source image.
    /// Enforced on the image HEADER before any full decode, so tiny but
    /// hugely-dimensioned uploads (decompression bombs) are rejected
    /// without materializing their pixels. Default 64 MPx (256 MiB RGBA):
    /// 33% headroom over an 8000×6000 (48 MPx) camera source, which must
    /// keep placing.
    pub max_decode_pixels: u64,
    pub min_size: i64,
    pub max_size: i64,
    /// Entries in the hot-tile LRU (zooms > 4; zooms <= 4 are unbounded —
    /// at most 341 tiles exist).
    pub tile_cache_entries: usize,
    /// Gate new signups behind admin approval. Defaults to false so local
    /// development never touches the gate; production sets
    /// REGISTRATION_APPROVAL_REQUIRED=true to keep external signups closed.
    pub registration_approval_required: bool,
    /// Batch import: entries processed per block (validate → dedupe →
    /// chunk-groups → paint → insert → commit). Blocks are sequential;
    /// block size bounds the in-flight raster/row working set, not the
    /// manifest size (a manifest may hold millions of entries within the
    /// import body cap).
    pub import_block_entries: usize,
    /// Batch import: parallel chunk-disjoint group executors. libvips work
    /// is serialized process-wide (see img::init), so concurrency beyond
    /// ~2 only overlaps object-storage/DB round trips with another
    /// group's pixel stage while inflating raster buffers.
    pub import_group_concurrency: usize,
    /// Batch import: preupload derivatives are capped to this long edge.
    /// The canvas raster (and every tile zoom) is chunk-bounded pixel
    /// work: at the original 2-4 MP the measured wall was ~1.4 img/s of
    /// pure decode+rotate; at 1024 the same pipeline clears the ≥10 img/s
    /// SLO with headroom. Originals stay full-resolution content-addressed
    /// objects; only the canvas derivative is scaled. Default 1024.
    pub import_derivative_long_edge: i64,
    /// Phase 3 render pipeline (docs/scaling-architecture.md, "Chunk and
    /// tile pipeline"): DARK by default. When false (the default), the
    /// request path paints chunks and replays removals synchronously as
    /// always and no render_jobs worker runs. When true, place/remove only
    /// record + queue, and the render_jobs worker paints chunks, cuts
    /// their z 18–20 tiles and bubbles parent versions. Cutover is the
    /// value flip (plus migration 0008, applied at boot regardless).
    pub render_pipeline_enabled: bool,
    /// Phase 4 upload flow (docs "Upload flow", migration 0009): when
    /// true, POST /api/place and admin DELETE validate cheaply, stash
    /// the upload and return 202 with an ingest_jobs id; the in-process
    /// ingest consumer does the pixel work and records the placement.
    /// Requires the render pipeline for tiles to actually paint (the
    /// consumer queues render jobs; see the cutover order in
    /// docs/backend.md). Default false — the synchronous 201 path is the
    /// shipped behavior until this flips.
    pub ingest_pipeline_enabled: bool,
    /// Worker claim loops per process. Independent of
    /// `render_pipeline_enabled`: 0 = no worker at all (dark); > 0 lets
    /// workers burn a pre-seeded queue while the request path still
    /// composites (the staged cutover in docs/backend.md: prewarm before
    /// the enabled flip).
    pub render_pipeline_workers: u32,
    /// render_jobs worker: claim-loop poll interval when the queue is
    /// empty (claims are instant when a job is due).
    pub render_pipeline_poll_ms: u64,
    /// render_jobs worker: seconds a `running` row's claim stays valid
    /// before another worker may retake it (worker died mid-job). Must
    /// exceed the longest job (a ~40 s production rebuild) comfortably.
    pub render_pipeline_lease_secs: i64,
    /// Parent jobs (z < 18) wait out this quiet window before firing
    /// (z 11–17: "a short quiet window").
    pub render_pipeline_parent_quiet_ms: u64,
    /// Parent jobs z ≤ 10 fire at most every `low_ms..low_ms+spread_ms`
    /// (doc: 10–30 s at most) — this is the base.
    pub render_pipeline_parent_low_ms: u64,
    /// Parent jobs z ≤ 10: coordinate-spread added on top of `low_ms`.
    pub render_pipeline_parent_spread_ms: u64,
}

/// Lenient boolean env parsing: true/1/yes (any case) are true.
fn parse_bool(input: &str) -> bool {
    matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_req(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing required env var {key}"))
}

impl Config {
    pub fn from_env() -> Self {
        let session_secret = env_req("SESSION_SECRET");
        assert!(
            session_secret.len() >= 32,
            "SESSION_SECRET must be at least 32 bytes"
        );
        Self {
            listen_addr: env_or("LISTEN_ADDR", "0.0.0.0:8080")
                .parse()
                .expect("invalid LISTEN_ADDR"),
            database_url: env_req("DATABASE_URL"),
            database_ro_url: std::env::var("DATABASE_RO_URL").ok(),
            database_pool_max: env_or("DATABASE_POOL_MAX", "15")
                .parse()
                .expect("invalid DATABASE_POOL_MAX"),
            s3_endpoint: std::env::var("S3_ENDPOINT").ok(),
            s3_bucket: env_req("S3_BUCKET"),
            s3_access_key_id: std::env::var("S3_ACCESS_KEY_ID").ok(),
            s3_secret_access_key: std::env::var("S3_SECRET_ACCESS_KEY").ok(),
            session_secret: session_secret.into_bytes(),
            public_base_url: env_or("PUBLIC_BASE_URL", "https://idol-appreciation.club"),
            static_dir: PathBuf::from(env_or("STATIC_DIR", "../web/dist")),
            cooldown_paint_secs: env_or("COOLDOWN_PAINT_SECS", "300")
                .parse()
                .expect("invalid COOLDOWN_PAINT_SECS"),
            cooldown_empty_secs: env_or("COOLDOWN_EMPTY_SECS", "1")
                .parse()
                .expect("invalid COOLDOWN_EMPTY_SECS"),
            max_upload_bytes: env_or("MAX_UPLOAD_BYTES", "10485760")
                .parse()
                .expect("invalid MAX_UPLOAD_BYTES"),
            max_decode_pixels: env_or("MAX_DECODE_PIXELS", "67108864")
                .parse()
                .expect("invalid MAX_DECODE_PIXELS"),
            min_size: env_or("MIN_SIZE", "16").parse().expect("invalid MIN_SIZE"),
            max_size: env_or("MAX_SIZE", "4096")
                .parse()
                .expect("invalid MAX_SIZE"),
            tile_cache_entries: env_or("TILE_CACHE_ENTRIES", "2048")
                .parse()
                .expect("invalid TILE_CACHE_ENTRIES"),
            registration_approval_required: parse_bool(&env_or(
                "REGISTRATION_APPROVAL_REQUIRED",
                "false",
            )),
            import_block_entries: env_or("IMPORT_BLOCK_ENTRIES", "500")
                .parse()
                .expect("invalid IMPORT_BLOCK_ENTRIES"),
            import_group_concurrency: env_or("IMPORT_GROUP_CONCURRENCY", "2")
                .parse()
                .expect("invalid IMPORT_GROUP_CONCURRENCY"),
            import_derivative_long_edge: env_or("IMPORT_DERIVATIVE_LONG_EDGE", "1024")
                .parse()
                .expect("invalid IMPORT_DERIVATIVE_LONG_EDGE"),
            render_pipeline_enabled: parse_bool(&env_or("RENDER_PIPELINE_ENABLED", "false")),
            ingest_pipeline_enabled: parse_bool(&env_or("INGEST_PIPELINE_ENABLED", "false")),
            render_pipeline_workers: env_or("RENDER_PIPELINE_WORKERS", "0")
                .parse()
                .expect("invalid RENDER_PIPELINE_WORKERS"),
            render_pipeline_poll_ms: env_or("RENDER_PIPELINE_POLL_MS", "1000")
                .parse()
                .expect("invalid RENDER_PIPELINE_POLL_MS"),
            render_pipeline_lease_secs: env_or("RENDER_PIPELINE_LEASE_SECS", "120")
                .parse()
                .expect("invalid RENDER_PIPELINE_LEASE_SECS"),
            render_pipeline_parent_quiet_ms: env_or("RENDER_PIPELINE_PARENT_QUIET_MS", "3000")
                .parse()
                .expect("invalid RENDER_PIPELINE_PARENT_QUIET_MS"),
            render_pipeline_parent_low_ms: env_or("RENDER_PIPELINE_PARENT_LOW_MS", "10000")
                .parse()
                .expect("invalid RENDER_PIPELINE_PARENT_LOW_MS"),
            render_pipeline_parent_spread_ms: env_or("RENDER_PIPELINE_PARENT_SPREAD_MS", "20000")
                .parse()
                .expect("invalid RENDER_PIPELINE_PARENT_SPREAD_MS"),
        }
    }

    /// True when PUBLIC_BASE_URL is https → cookie gets `Secure`.
    pub fn secure_cookies(&self) -> bool {
        self.public_base_url.starts_with("https://")
    }
}

#[cfg(test)]
mod tests {
    use super::parse_bool;

    /// The pipeline ships DARK (docs Phase 3): merging is a values flip,
    /// so the default MUST stay off.
    #[test]
    fn render_pipeline_defaults_off() {
        std::env::set_var("DATABASE_URL", "postgres://u:p@localhost/db");
        std::env::set_var("S3_BUCKET", "test-bucket");
        std::env::set_var("SESSION_SECRET", "x".repeat(32));
        std::env::remove_var("RENDER_PIPELINE_ENABLED");
        std::env::remove_var("RENDER_PIPELINE_WORKERS");
        let cfg = super::Config::from_env();
        assert!(!cfg.render_pipeline_enabled);
        assert_eq!(cfg.render_pipeline_workers, 0);
        // Doc defaults: z ≤ 10 at most every 10–30 s; z 11–17 quiet.
        assert_eq!(cfg.render_pipeline_parent_low_ms, 10_000);
        assert_eq!(cfg.render_pipeline_parent_spread_ms, 20_000);
        assert_eq!(cfg.render_pipeline_parent_quiet_ms, 3_000);
        assert_eq!(cfg.render_pipeline_poll_ms, 1_000);
        assert_eq!(cfg.render_pipeline_lease_secs, 120);
    }

    /// The env var is the deployment's per-replica pool lever (the chart
    /// sets it); if the pool size ever stops reading it, this fails.
    #[test]
    fn database_pool_max_env_override() {
        std::env::set_var("DATABASE_URL", "postgres://u:p@localhost/db");
        std::env::set_var("S3_BUCKET", "test-bucket");
        std::env::set_var("SESSION_SECRET", "x".repeat(32));
        std::env::set_var("DATABASE_POOL_MAX", "7");
        assert_eq!(super::Config::from_env().database_pool_max, 7);
        std::env::remove_var("DATABASE_POOL_MAX");
        // Default matches the chart: 15/pod x 2 replicas = 30 total, under
        // the managed instance's connection ceiling.
        assert_eq!(super::Config::from_env().database_pool_max, 15);
    }

    #[test]
    fn registration_flag_parsing() {
        assert!(parse_bool("true"));
        assert!(parse_bool("TRUE"));
        assert!(parse_bool("1"));
        assert!(parse_bool(" yes "));
        assert!(!parse_bool("false"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool("no"));
        assert!(!parse_bool(""));
        // Anything unparseable defaults to closed: production sets the env
        // explicitly; local dev relies on the documented default of false.
        assert!(!parse_bool("open sesame"));
    }
}
