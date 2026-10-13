//! Environment-driven configuration. Exact env names wired by infra.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub database_url: String,
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
    /// Background re-render queue: seconds of request-quiet on a tile
    /// before its queued re-render fires (see `tiles::RenderQueue`).
    pub tile_render_quiet_secs: u64,
    /// Background re-render queue: hard cap on how long a queued tile can
    /// wait past its first enqueue, even under continuous bumps.
    pub tile_render_max_delay_secs: u64,
    /// Hard bound on queued tile coords. Overflow sheds first-ever tile
    /// renders (202) and skips stale re-render enqueues; stored tiles keep
    /// serving stale regardless.
    pub tile_render_queue_cap: usize,
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
            tile_render_quiet_secs: env_or("TILE_RENDER_QUIET_SECS", "4")
                .parse()
                .expect("invalid TILE_RENDER_QUIET_SECS"),
            tile_render_max_delay_secs: env_or("TILE_RENDER_MAX_DELAY_SECS", "12")
                .parse()
                .expect("invalid TILE_RENDER_MAX_DELAY_SECS"),
            tile_render_queue_cap: env_or("TILE_RENDER_QUEUE_CAP", "512")
                .parse()
                .expect("invalid TILE_RENDER_QUEUE_CAP"),
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
