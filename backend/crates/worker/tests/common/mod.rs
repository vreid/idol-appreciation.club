//! Shared worker-suite harness: the API router (HTTP surface) PLUS a
//! `WorkerCtx` over the same database/store, so tests drive claims
//! manually or spawn worker loops — exactly how production splits the
//! two processes. Postgres comes from the CI service (preset
//! DATABASE_URL, throwaway database per boot) or a testcontainer.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use iac_api::build_app;
use iac_api::config::Config;
use idol_appreciation_club_backend as iac_api;
// (the api package is `iac-api`; its lib keeps the historical name)
use iac_worker::ctx::WorkerCtx;
use sqlx::PgPool;
use tokio::net::TcpListener;

pub const WORLD: i64 = 268_435_456;
pub const CENTER: i64 = 134_217_728;

pub struct Rig {
    pub addr: SocketAddr,
    pub db_url: String,
    pub pool: PgPool,
    pub ctx: WorkerCtx,
    pub state: iac_api::state::App,
    pub _store_dir: PathBuf,
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

/// Worker-pipeline knobs for a rig.
#[derive(Clone, Copy)]
pub struct PipelineCfg {
    pub enabled: bool,
    pub ingest: bool,
    /// Worker loops spawned for the rig (0 = tests drive claims).
    pub workers: u32,
    /// Spawn the live ingest consumer (tests that drive claims manually
    /// leave it off to avoid racing the loop).
    pub consumer: bool,
}

impl Default for PipelineCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            ingest: true,
            workers: 0,
            consumer: false,
        }
    }
}

impl PipelineCfg {
    /// Live-worker preset: loops poll fast, ingest consumer on.
    pub fn fast() -> Self {
        Self {
            enabled: true,
            ingest: true,
            workers: 1,
            consumer: true,
        }
    }
}

/// Throwaway database per rig (isolation across suites and tests).
pub async fn provision_isolated_db(preset: &str) -> String {
    let (base, query) = match preset.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (preset, None),
    };
    let origin = base
        .rsplit_once('/')
        .map(|(o, _)| o)
        .expect("DATABASE_URL with a database path");
    let db = format!("w_{}", uuid::Uuid::now_v7().simple());
    let admin = format!("{origin}/postgres");
    let admin_pool = sqlx::PgPool::connect(&admin)
        .await
        .expect("connect admin database for per-rig provisioning");
    sqlx::query(&format!(r#"CREATE DATABASE "{db}""#))
        .execute(&admin_pool)
        .await
        .expect("create per-rig database");
    admin_pool.close().await;
    let mut url = format!("{origin}/{db}");
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    url
}

/// Boot API + worker contexts over one database/store pair.
pub async fn rig(pipeline: PipelineCfg) -> Rig {
    let (database_url, _container) = match std::env::var("DATABASE_URL") {
        Ok(base) => (
            provision_isolated_db(&base).await,
            None::<
                testcontainers_modules::testcontainers::ContainerAsync<
                    testcontainers_modules::postgres::Postgres,
                >,
            >,
        ),
        Err(_) => {
            // Local runs need a postgres; the CI preset is authoritative.
            panic!("worker suites require DATABASE_URL (CI postgres service)")
        }
    };
    let store_dir =
        std::env::temp_dir().join(format!("iac-worker-test-{}", uuid::Uuid::now_v7().simple()));
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
        cooldown_paint_secs: 300,
        cooldown_empty_secs: 1,
        max_upload_bytes: 10 * 1024 * 1024,
        max_decode_pixels: 67_108_864,
        min_size: 16,
        max_size: 4096,
        tile_cache_entries: 256,
        registration_approval_required: false,
        import_block_entries: 500,
        import_group_concurrency: 2,
        import_derivative_long_edge: 1024,
        render_pipeline_enabled: pipeline.enabled,
        ingest_pipeline_enabled: pipeline.ingest,
        // The API process never spawns workers; the rig spawns the
        // requested number of worker loops over its own ctx below.
        render_pipeline_workers: pipeline.workers,
        render_pipeline_poll_ms: if pipeline.workers > 0 || pipeline.consumer {
            10
        } else {
            60_000
        },
        render_pipeline_lease_secs: 120,
        render_pipeline_parent_quiet_ms: if pipeline.workers > 0 { 0 } else { 3_000 },
        render_pipeline_parent_low_ms: if pipeline.workers > 0 { 0 } else { 10_000 },
        render_pipeline_parent_spread_ms: if pipeline.workers > 0 { 0 } else { 20_000 },
    };
    let (state, app) = build_app(cfg.clone()).await.expect("build app");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });

    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let ctx = WorkerCtx::new(cfg, pool.clone(), state.store.clone());
    for _ in 0..pipeline.workers {
        iac_worker::render_jobs::spawn_worker(&ctx);
    }
    if pipeline.ingest && pipeline.consumer {
        iac_worker::ingest::spawn_consumer(&ctx);
    }
    Rig {
        addr,
        db_url: database_url,
        pool,
        ctx,
        state,
        _store_dir: store_dir,
    }
}

/// Minimal HTTP/1.1 client over a raw socket (Connection: close).
pub async fn request(
    rig: &Rig,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Resp {
    let mut buf = format!("{method} {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\ncontent-length: {}\r\n", body.len());
    for (k, v) in headers {
        buf.push_str(&format!("{k}: {v}\r\n"));
    }
    buf.push_str("\r\n");
    let mut sock = tokio::net::TcpStream::connect(rig.addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    sock.write_all(buf.as_bytes()).await.unwrap();
    sock.write_all(&body).await.unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    parse_resp(raw)
}

fn parse_resp(raw: Vec<u8>) -> Resp {
    let sep = b"\r\n\r\n";
    let cut = raw
        .windows(sep.len())
        .position(|w| w == sep)
        .map(|p| p + sep.len())
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..cut.min(raw.len())]).to_string();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Resp {
        status,
        headers,
        body: raw[cut.min(raw.len())..].to_vec(),
    }
}

pub fn multipart(boundary: &str, fields: &[(&str, &str)], file: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in fields {
        out.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    out.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"img.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(file);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

pub fn place_body(image: &[u8], x: i64, y: i64, size: i64) -> (String, Vec<u8>) {
    let boundary = format!("testb{}", uuid::Uuid::now_v7().simple());
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

/// POST /api/place; the branch's contract is 202 + a job id.
pub async fn place(rig: &Rig, cookie: &str, image: &[u8], x: i64, y: i64, size: i64) -> Resp {
    let (boundary, body) = place_body(image, x, y, size);
    request(
        rig,
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

/// 202 + poll GET /api/uploads/{id} until the placement is live.
pub async fn place_wait(rig: &Rig, cookie: &str, image: &[u8], x: i64, y: i64, size: i64) -> Resp {
    let resp = place(rig, cookie, image, x, y, size).await;
    assert_eq!(resp.status, 202, "{}", String::from_utf8_lossy(&resp.body));
    let id = resp.json()["id"].as_str().expect("job id").to_owned();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let status = request(
            rig,
            "GET",
            &format!("/api/uploads/{id}"),
            &[("Cookie", cookie)],
            vec![],
        )
        .await
        .json();
        match status["state"].as_str() {
            Some("placed") => return resp,
            Some("failed") => panic!("placement {id} failed: {status}"),
            _ => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "placement {id} never went live: {status}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn register(rig: &Rig, username: &str) -> String {
    let body = serde_json::json!({ "username": username, "password": "hunter2hunter2" });
    let resp = request(
        rig,
        "POST",
        "/api/auth/register",
        &[("Content-Type", "application/json")],
        body.to_string().into_bytes(),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));
    resp.cookie().expect("session cookie")
}

/// Register + grant the admin cooldown exemption (burst-friendly
/// placements; the first account is admin, so it exempts itself).
pub async fn make_placer(rig: &Rig, username: &str) -> String {
    let cookie = register(rig, username).await;
    let resp = request(
        rig,
        "POST",
        &format!("/api/admin/accounts/{username}/cooldown-exempt"),
        &[("Cookie", &cookie), ("Content-Type", "application/json")],
        br#"{"exempt":true}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 204);
    cookie
}

/// First registered account is admin.
pub async fn admin_cookie(rig: &Rig) -> String {
    register(rig, "root").await
}

pub async fn drain_ingest(rig: &Rig) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM ingest_jobs WHERE state IN ('pending', 'running')",
        )
        .fetch_one(&rig.pool)
        .await
        .expect("count ingest jobs");
        if n == 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ingest_jobs never drained"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn png(width: u32, height: u32) -> Vec<u8> {
    iac_worker::img::test_png(width, height)
}
