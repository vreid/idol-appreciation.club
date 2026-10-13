//! Shared HTTP test harness: real router against testcontainers Postgres +
//! filesystem store. Skipped (with a loud message) when Docker is
//! unavailable. Used by `integration.rs` and `tile_queue.rs`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use idol_appreciation_club_backend::build_app;
use idol_appreciation_club_backend::config::Config;
use idol_appreciation_club_backend::img;
use idol_appreciation_club_backend::state::App;
use idol_appreciation_club_backend::tiles;
use testcontainers_modules::postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const WORLD: i64 = 268_435_456;
pub const CENTER: i64 = 134_217_728;

/// Background render queue knobs for a test server. Defaults match the
/// production spec values; queue-behavior tests shrink them for speed.
#[derive(Clone, Copy)]
pub struct TileRenderCfg {
    pub quiet_secs: u64,
    pub max_delay_secs: u64,
    pub cap: usize,
}

impl Default for TileRenderCfg {
    fn default() -> Self {
        Self {
            quiet_secs: 4,
            max_delay_secs: 12,
            cap: 512,
        }
    }
}

pub struct Server {
    pub addr: SocketAddr,
    pub _store_dir: PathBuf,
    #[allow(dead_code)] // dropped at test end; container stops with it
    pub container: testcontainers_modules::testcontainers::ContainerAsync<postgres::Postgres>,
}

pub struct Resp {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }
    pub fn cookie(&self) -> Option<String> {
        self.headers
            .get("set-cookie")
            .map(|v| v.split(';').next().unwrap_or_default().to_string())
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.as_str())
    }
}

pub async fn start_server(paint_secs: i64, empty_secs: i64, max_upload: usize) -> Server {
    start_server_gated(false, paint_secs, empty_secs, max_upload).await
}

/// `approval_required = true` gates new signups behind admin approval.
pub async fn start_server_gated(
    approval_required: bool,
    paint_secs: i64,
    empty_secs: i64,
    max_upload: usize,
) -> Server {
    start_server_full(
        approval_required,
        paint_secs,
        empty_secs,
        max_upload,
        Default::default(),
    )
    .await
}

/// Full-knob boot: queue behavior tests override the render-queue config.
pub async fn start_server_full(
    approval_required: bool,
    paint_secs: i64,
    empty_secs: i64,
    max_upload: usize,
    tiles_cfg: TileRenderCfg,
) -> Server {
    // Pin a modern postgres: the module default is 11-alpine, too old for
    // generated columns.
    let container = postgres::Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");
    let store_dir =
        std::env::temp_dir().join(format!("idol-test-store-{}", uuid::Uuid::now_v7().simple()));
    let cfg = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        database_url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        s3_endpoint: Some(format!("file://{}", store_dir.display())),
        s3_bucket: "test-bucket".to_string(),
        s3_access_key_id: None,
        s3_secret_access_key: None,
        session_secret: vec![b'x'; 32],
        public_base_url: "http://localhost:8080".to_string(),
        static_dir: PathBuf::from("/nonexistent-web-dist-for-tests"),
        cooldown_paint_secs: paint_secs,
        cooldown_empty_secs: empty_secs,
        max_upload_bytes: max_upload,
        max_decode_pixels: idol_appreciation_club_backend::img::DEFAULT_MAX_DECODE_PIXELS,
        min_size: 16,
        max_size: 4096,
        tile_cache_entries: 256,
        tile_render_quiet_secs: tiles_cfg.quiet_secs,
        tile_render_max_delay_secs: tiles_cfg.max_delay_secs,
        tile_render_queue_cap: tiles_cfg.cap,
        registration_approval_required: approval_required,
    };
    let (state, app): (App, _) = build_app(cfg).await.expect("build app (pool + migrations)");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    let _ = state; // kept alive by the spawned server
    Server {
        addr,
        _store_dir: store_dir,
        container,
    }
}

/// Minimal HTTP/1.1 client over a raw socket (Connection: close).
pub async fn request(
    server: &Server,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Resp {
    let mut stream = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    if !body.is_empty() || method == "POST" || method == "DELETE" {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.write_all(&body).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
        .await
        .expect("response within 20s")
        .unwrap();
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> Resp {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw[split + 4..].to_vec();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Resp {
        status,
        headers,
        body,
    }
}

pub fn multipart(boundary: &str, fields: &[(&str, &str)], file: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"i.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

pub fn place_body(image: &[u8], x: i64, y: i64, size: i64) -> (String, Vec<u8>) {
    let boundary = format!("b{}", uuid::Uuid::now_v7().simple());
    let body = multipart(
        &boundary,
        &[
            ("x", &x.to_string()),
            ("y", &y.to_string()),
            ("size", &size.to_string()),
        ],
        image,
    );
    (boundary, body)
}

pub async fn place(server: &Server, cookie: &str, image: &[u8], x: i64, y: i64, size: i64) -> Resp {
    let (boundary, body) = place_body(image, x, y, size);
    let resp = request(
        server,
        "POST",
        "/api/place",
        &[
            ("Cookie", cookie),
            (
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
        ],
        body,
    )
    .await;
    resp
}

pub async fn register(server: &Server, username: &str) -> String {
    let resp = request(
        server,
        "POST",
        "/api/auth/register",
        &[("Content-Type", "application/json")],
        format!(r#"{{"username":"{username}","password":"hunter2hunter2"}}"#).into_bytes(),
    )
    .await;
    assert_eq!(resp.status, 201, "register {username}: {}", resp.body.len());
    resp.cookie().expect("session cookie")
}

/// A valid small PNG via the image pipeline (opaque red square).
pub fn png(width: u32, height: u32) -> Vec<u8> {
    img::test_png(width, height)
}

/// A PNG whose IHDR DECLARES (width, height) but whose pixel data encodes
/// a 1×1 image: header-probing sees the huge dimensions while a full
/// decode would fail. Exercise for the decoded-pixel budget, which must
/// reject on the header before attempting any pixel work.
pub fn png_with_declared_dims(width: u32, height: u32) -> Vec<u8> {
    let mut png = png(1, 1);
    // Layout: 8-byte signature, then chunks of (4-byte BE len, 4-byte
    // type, data, 4-byte CRC). IHDR is the first chunk and its data
    // starts with width then height as u32 BE.
    const IHDR_DATA: usize = 8 + 4 + 4;
    png[IHDR_DATA..IHDR_DATA + 4].copy_from_slice(&width.to_be_bytes());
    png[IHDR_DATA + 4..IHDR_DATA + 8].copy_from_slice(&height.to_be_bytes());
    let chunk_len = u32::from_be_bytes(png[8..12].try_into().expect("chunk len")) as usize;
    let crc_offset = 12 + 4 + chunk_len;
    let crc = img::crc32(&png[12..crc_offset]);
    png[crc_offset..crc_offset + 4].copy_from_slice(&crc.to_be_bytes());
    png
}

/// GET /api/metrics parsed into `name{labels} value` lines (HELP/TYPE
/// dropped) for counter/gauge assertions.
pub async fn metrics_map(server: &Server) -> HashMap<String, String> {
    let resp = request(server, "GET", "/api/metrics", &[], vec![]).await;
    assert_eq!(resp.status, 200);
    let mut map = HashMap::new();
    for line in String::from_utf8_lossy(&resp.body).lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some((series, value)) = line.rsplit_once(' ') {
            map.insert(series.to_string(), value.to_string());
        }
    }
    map
}

/// Poll a tile until it is FRESH at `etag` (no X-Tile-Stale). Every
/// intermediate GET also re-bumps the queued render's quiet window — which
/// is exactly what a polling client does — so `interval` MUST exceed the
/// server's quiet window, or the poller itself starves the re-render (the
/// max-delay cap is the only thing that would eventually fire).
pub async fn tile_fresh(server: &Server, path: &str, etag: &str, interval: Duration) -> Resp {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let resp = request(server, "GET", path, &[], vec![]).await;
        if resp.header("etag") == Some(etag) && resp.header("x-tile-stale").is_none() {
            return resp;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tile {path} never became fresh at etag {etag} (last etag {:?}, stale {:?})",
            resp.header("etag"),
            resp.header("x-tile-stale")
        );
        tokio::time::sleep(interval).await;
    }
}

/// First-account-is-admin: grant the admin cooldown exemption so tests can
/// place in bursts without 429s.
pub async fn make_placer(server: &Server, username: &str) -> String {
    let cookie = register(server, username).await;
    let resp = request(
        server,
        "POST",
        &format!("/api/admin/accounts/{username}/cooldown-exempt"),
        &[("Cookie", &cookie), ("Content-Type", "application/json")],
        br#"{"exempt":true}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 204);
    cookie
}

/// Render-worker pause seam for deterministic queue-behavior tests. The
/// guard resets to 0 on drop; tests in one binary also take this mutex so
/// the global seam never overlaps.
pub struct PauseGuard {
    _perm: tokio::sync::OwnedMutexGuard<()>,
}

static PAUSE_LOCK: std::sync::LazyLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Mutex::new(())));

pub async fn set_render_pause_ms(ms: u64) -> PauseGuard {
    let perm = std::sync::Arc::clone(&*PAUSE_LOCK).lock_owned().await;
    tiles::set_render_pause_for_tests(ms);
    PauseGuard { _perm: perm }
}

impl Drop for PauseGuard {
    fn drop(&mut self) {
        tiles::set_render_pause_for_tests(0);
    }
}
