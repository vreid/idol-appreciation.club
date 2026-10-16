//! Shared HTTP test harness: real router against testcontainers Postgres +
//! filesystem store. Skipped (with a loud message) when Docker is
//! unavailable. Used by `integration.rs`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use idol_appreciation_club_backend::build_app;
use idol_appreciation_club_backend::config::Config;
use idol_appreciation_club_backend::state::App;
use testcontainers_modules::postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const WORLD: i64 = 268_435_456;
pub const CENTER: i64 = 134_217_728;

/// Phase 3 pipeline knobs for a test server. The api integration suite
/// runs everything dark: `fast()` would spawn worker loops, which the
/// HTTP tests never want (the worker suites own those).
#[derive(Clone, Copy)]
pub struct PipelineCfg {
    pub enabled: bool,
    pub ingest: bool,
    pub workers: u32,
    pub poll_ms: u64,
    pub lease_secs: i64,
    pub parent_quiet_ms: u64,
    pub parent_low_ms: u64,
    pub parent_spread_ms: u64,
}

impl Default for PipelineCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            ingest: false,
            workers: 0,
            poll_ms: 60_000,
            lease_secs: 120,
            parent_quiet_ms: 3_000,
            parent_low_ms: 10_000,
            parent_spread_ms: 20_000,
        }
    }
}

impl PipelineCfg {
    pub fn fast() -> Self {
        Self {
            enabled: true,
            ingest: true,
            workers: 1,
            poll_ms: 10,
            lease_secs: 5,
            parent_quiet_ms: 50,
            parent_low_ms: 100,
            parent_spread_ms: 100,
        }
    }
}

pub struct Server {
    pub addr: SocketAddr,
    pub _store_dir: PathBuf,
    /// Direct SQL access for queue/pipeline assertions.
    pub db_url: String,
    /// The app state (pool) for tests that drive the pipeline directly.
    pub state: App,
    /// None when CI presets DATABASE_URL (job service container): there
    /// is nothing to stop — the service outlives the test.
    #[allow(dead_code)] // dropped at test end; container stops with it
    pub container:
        Option<testcontainers_modules::testcontainers::ContainerAsync<postgres::Postgres>>,
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

/// With a preset `DATABASE_URL` (the CI job's postgres service — the
/// pinned image has no docker socket for testcontainers), every server
/// still needs the ISOLATION a fresh testcontainer used to provide:
/// unique usernames, fresh `placement_seq`/`chunk_seqs` (tests assert
/// etag "1"), private chunk areas. So provision a throwaway database per
/// server boot in the preset service instead of pointing every server at
/// one shared database. Databases accumulate for the job's lifetime —
/// the service is disposable.
pub async fn provision_isolated_db(preset: &str) -> String {
    let mut conn = <sqlx::postgres::PgConnection as sqlx::Connection>::connect(preset)
        .await
        .expect("connect CI service postgres");
    let db = format!("test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE DATABASE {db}"))
        .execute(&mut conn)
        .await
        .expect("create per-test database");
    let cut = preset
        .rfind('/')
        .expect("DATABASE_URL carries a db segment");
    format!("{}/{}", &preset[..cut], db)
}

pub async fn start_server_full(
    approval_required: bool,
    paint_secs: i64,
    empty_secs: i64,
    max_upload: usize,
    pipeline: PipelineCfg,
) -> Server {
    // Pin a modern postgres: the module default is 11-alpine, too old for
    // generated columns. CI presets DATABASE_URL (job service container —
    // the pinned image has no docker socket for testcontainers). The
    // service is SHARED by every suite in the job, so each test clones a
    // throwaway database off it: parallel suites would otherwise collide
    // on the same users table (the second `register("root")` gets 409)
    // and re-run migrations over each other. Local runs keep one
    // container per test instead.
    let (database_url, container) = match std::env::var("DATABASE_URL") {
        Ok(base) => (provision_isolated_db(&base).await, None),
        Err(_) => {
            let container = postgres::Postgres::default()
                .with_tag("16-alpine")
                .start()
                .await
                .expect("start postgres container");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("container port");
            (
                format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
                Some(container),
            )
        }
    };
    let store_dir =
        std::env::temp_dir().join(format!("idol-test-store-{}", uuid::Uuid::now_v7().simple()));
    let cfg = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        database_url: database_url.clone(),
        database_ro_url: None,
        database_pool_max: 30,
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
        // Production default (MAX_DECODE_PIXELS env default): 64 MPx.
        max_decode_pixels: 67_108_864,
        min_size: 16,
        max_size: 4096,
        tile_cache_entries: 256,
        registration_approval_required: approval_required,
        import_block_entries: 500,
        import_group_concurrency: 2,
        import_derivative_long_edge: 1024,
        render_pipeline_enabled: pipeline.enabled,
        ingest_pipeline_enabled: pipeline.ingest,
        render_pipeline_workers: pipeline.workers,
        render_pipeline_poll_ms: pipeline.poll_ms,
        render_pipeline_lease_secs: pipeline.lease_secs,
        render_pipeline_parent_quiet_ms: pipeline.parent_quiet_ms,
        render_pipeline_parent_low_ms: pipeline.parent_low_ms,
        render_pipeline_parent_spread_ms: pipeline.parent_spread_ms,
    };
    let (state, app): (App, _) = build_app(cfg).await.expect("build app (pool + migrations)");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    Server {
        addr,
        _store_dir: store_dir,
        db_url: database_url,
        state,
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
    request_addr(
        server.addr,
        method,
        path,
        headers,
        body,
        Duration::from_secs(20),
    )
    .await
}

/// Long-budget variant for slow endpoints.
pub async fn request_long(
    server: &Server,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Resp {
    request_addr(
        server.addr,
        method,
        path,
        headers,
        body,
        Duration::from_secs(120),
    )
    .await
}

/// Server-less request form for use inside spawned tasks.
pub async fn request_addr(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
    deadline: Duration,
) -> Resp {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
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
    tokio::time::timeout(deadline, stream.read_to_end(&mut raw))
        .await
        .expect("response within deadline")
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
    request(
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
    .await
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

// ----------------------------------------------------- pure-Rust PNG ---
// The API no longer links libvips, so test images are encoded here: a
// minimal deterministic PNG (8-bit RGBA, solid color, zlib "stored"
// blocks) with hand-rolled CRC32/Adler32. Enough for the header probe,
// the worker's decode, and pixel sampling.

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *t = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn png_chunk(chunk_type: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(chunk_type);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// A valid small PNG (opaque red square), built without libvips.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    png_color(width, height, [220, 40, 40, 255])
}

pub fn png_color(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let row = width as usize * 4;
    let mut raw = vec![0u8; height as usize * (1 + row)];
    for line in raw.chunks_mut(1 + row) {
        // line[0] is the filter byte, already 0 (none).
        for px in line[1..].as_chunks_mut::<4>().0 {
            px.copy_from_slice(&rgba);
        }
    }
    // zlib stream: 2-byte header, stored blocks (max 65535 bytes), adler32.
    let mut zlib = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = if i == raw.chunks(65_535).count() - 1 {
            1u8
        } else {
            0
        };
        zlib.push(last);
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA

    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&png_chunk(b"IDAT", &zlib));
    out.extend_from_slice(&png_chunk(b"IEND", &[]));
    out
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
    let crc = crc32(&png[12..crc_offset]);
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

/// Seed a committed placement row directly (the api binary cannot run
/// the ingest pixel stage, so data-seeding goes through SQL): returns
/// (placement id, seq). Synthetic shas are fine — nothing on the
/// api-only paths these tests exercise decodes the bytes.
pub async fn seed_placement(
    server: &Server,
    username: &str,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    rot: f32,
) -> (String, i64) {
    let pool = sqlx::postgres::PgPool::connect(&server.db_url)
        .await
        .expect("connect test db");
    let sha = format!("{:064x}", uuid::Uuid::now_v7().as_u128());
    // bbox is a regular column since 0004 (the rotated AABB, computed in
    // Rust by the write paths) — compute it with the same world math.
    let aabb = idol_appreciation_club_backend::world::rotated_aabb(x, y, w, h, rot);
    let bbox = format!(
        "({},{}),({},{})",
        aabb.x,
        aabb.y,
        aabb.x + aabb.w - 1,
        aabb.y + aabb.h - 1
    );
    let (id, seq): (uuid::Uuid, i64) = sqlx::query_as(
        "INSERT INTO placements (id, account_id, image_key, x, y, w, h, rot, seq, original_sha, derivative_sha, bbox)          VALUES ($1, (SELECT id FROM accounts WHERE username = $2), 'seed.png', $3, $4, $5, $6, $7, nextval('placement_seq'), $8, $8, $9::box)          RETURNING id, seq",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(username)
    .bind(x)
    .bind(y)
    .bind(w as i32)
    .bind(h as i32)
    .bind(rot)
    .bind(&sha)
    .bind(&bbox)
    .fetch_one(&pool)
    .await
    .expect("seed placement row");
    pool.close().await;
    (id.to_string(), seq)
}

/// Soft-remove a seeded placement row directly (removed_at + a fresh
/// seq draw for the chunk version, exactly what the ingest consumer's
/// removal path writes).
pub async fn seed_remove(server: &Server, placement_id: impl Into<String>) {
    let pool = sqlx::postgres::PgPool::connect(&server.db_url)
        .await
        .expect("connect test db");
    sqlx::query(
        "UPDATE placements SET removed_at = now(), seq = nextval('placement_seq') WHERE id = $1",
    )
    .bind(
        placement_id
            .into()
            .parse::<uuid::Uuid>()
            .expect("placement uuid"),
    )
    .execute(&pool)
    .await
    .expect("remove placement row");
    pool.close().await;
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
