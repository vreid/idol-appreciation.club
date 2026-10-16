//! Phase 4 upload flow (migration 0009, `ingest_jobs` consumer): the
//! 202 quarantine path, the upload-status contract, permanent decode
//! failures with cooldown refund, 202 removals with ghost-free pixel
//! assertions, claim exclusivity and the attempts ceiling — and the
//! dark-ship contract: INGEST_PIPELINE_ENABLED=false leaves the
//! synchronous 201 path byte-identical.

mod common;

use std::time::Duration;

use common::*;
use idol_appreciation_club_backend::img;
use idol_appreciation_club_backend::ingest;
use idol_appreciation_club_backend::ingest::MAX_INGEST_ATTEMPTS;
use idol_appreciation_club_backend::render_jobs;
use serde_json::{json, Value};
use sqlx::PgPool;

/// Private diagonal chunk columns (see render_jobs.rs's note: each test
/// owns (cx, cx); shared ancestors only ever GREATEST later sequence
/// values, so exact version asserts stay valid).
const CX_QUARANTINE: i64 = 131_072 - 10;
const CX_STATUS: i64 = 131_072 - 12;
const CX_DECODE: i64 = 131_072 - 14;
const CX_REMOVAL: i64 = 131_072 - 16;
const CX_EXCLUSIVE: i64 = 131_072 - 18;
const CX_DARK: i64 = 131_072 + 4;
/// Ingest-cfg preset: 202 path on, no spawned workers — tests drive
/// claims manually for determinism.
fn quarantine_cfg() -> common::PipelineCfg {
    common::PipelineCfg {
        enabled: true,
        ingest: true,
        workers: 0,
        poll_ms: 60_000,
        ..Default::default()
    }
}

fn base(col: i64) -> i64 {
    col * 1024
}

async fn pool(server: &Server) -> PgPool {
    PgPool::connect(&server.db_url).await.expect("test pool")
}

fn object(server: &Server, key: &str) -> Option<Vec<u8>> {
    std::fs::read(server._store_dir.join("test-bucket").join(key)).ok()
}

/// Decode encoded image bytes and sample pixels (vips is !Send: decode +
/// reads inside one blocking task, same pattern as the render path).
async fn decode_pixels(body: Vec<u8>, pts: &[(i32, i32)]) -> Vec<[u8; 3]> {
    let pts = pts.to_vec();
    tokio::task::spawn_blocking(move || {
        let (d, _) = img::decode(&body, 16_000_000).expect("decode raster");
        pts.iter().map(|&(x, y)| img::get_pixel(&d, x, y)).collect()
    })
    .await
    .unwrap()
}

/// Claim + execute one ingest job, asserting exactly one was due.
async fn consume_one(server: &Server) -> ingest::IngestJob {
    let p = pool(server).await;
    let job = ingest::claim_due(&p, 120).await.expect("claim ingest job");
    let job = job.expect("an ingest job was due");
    ingest::execute_job(&server.state, job.clone())
        .await
        .unwrap_or_else(|err| panic!("ingest job {} failed: {err}", job.id));
    job
}

/// Drive one render_jobs claim (the render worker is off in these
/// suites; the ingest consumer only queues).
async fn paint_one_chunk(server: &Server) {
    let p = pool(server).await;
    let job = render_jobs::claim_due(&p, 120)
        .await
        .expect("claim render job");
    let (z, x, y) = job.expect("a render job was due");
    render_jobs::execute_job(&server.state, z, x, y)
        .await
        .expect("render job executes");
}

/// Subscribe to the SSE broadcast bus (what /api/events forwards).
fn subscribe(server: &Server) -> tokio::sync::broadcast::Receiver<String> {
    server.state.events_tx.subscribe()
}

/// Receive the next event matching `kind` (with a deadline).
async fn next_event(rx: &mut tokio::sync::broadcast::Receiver<String>, kind: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no {kind} event arrived"
        );
        let raw = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("event wait")
            .expect("bus open");
        let v: Value = serde_json::from_str(&raw).expect("event json");
        if v["type"] == kind {
            return v;
        }
    }
}

async fn ingest_row(server: &Server, id: &str) -> Option<(String, i16, Option<String>)> {
    let p = pool(server).await;
    sqlx::query_as("SELECT state, attempts, error FROM ingest_jobs WHERE id = $1::uuid")
        .bind(id.parse::<uuid::Uuid>().expect("uuid"))
        .fetch_optional(&p)
        .await
        .expect("ingest row")
}

// --------------------------------------------------- the dark doctrine ----

/// INGEST_PIPELINE_ENABLED=false (the default): the 201 synchronous path
/// is byte-identical — same body shape, chunk painted inline, nothing
/// quarantined, no job rows.
#[tokio::test]
async fn dark_path_stays_synchronous_201() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        Default::default(),
    )
    .await;
    let placer = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_DARK;
    let b = base(cx);

    let resp = place(&server, &placer, &red, b + 100, b + 100, 64).await;
    assert_eq!(resp.status, 201, "dark path must stay 201");
    let body = resp.json();
    // The exact #95-era body shape (201 contract unchanged).
    for key in ["id", "x", "y", "w", "h", "rot", "seq", "duplicate_count"] {
        assert!(body.get(key).is_some(), "201 body missing {key}");
    }
    assert_eq!(body["state"], Value::Null, "no state field on the 201");

    // Nothing queued, nothing quarantined, pixels already composited.
    let p = pool(&server).await;
    let (jobs,): (i64,) = sqlx::query_as("SELECT count(*) FROM ingest_jobs")
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "dark path must not enqueue ingest jobs");
    assert!(
        object(
            &server,
            &format!("quarantine/{}", body["id"].as_str().unwrap())
        )
        .is_none(),
        "dark path must not quarantine uploads"
    );
    let chunk = object(&server, &format!("chunks/{cx}/{cx}.webp")).expect("chunk raster");
    let px = decode_pixels(chunk, &[(110, 110)]).await;
    let red = [220, 40, 40];
    assert!(
        px[0].iter().zip(red).all(|(g, e)| g.abs_diff(e) <= 3),
        "dark path paints synchronously: {:?}",
        px[0]
    );
}

// -------------------------------------------------- the 202 happy path ----

/// 202 + upload id; the consumer records the placement under the
/// sequencer lock, queues render jobs; the status poll rides the frozen
/// contract (pending → placed) and the `placed` event hits the bus.
#[tokio::test]
async fn quarantine_202_polls_to_placed_and_paints() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let placer = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_QUARANTINE;
    let b = base(cx);
    let mut rx = subscribe(&server);

    let resp = place(&server, &placer, &red, b + 100, b + 100, 64).await;
    assert_eq!(resp.status, 202, "quarantine path accepts with 202");
    let accepted = resp.json();
    assert_eq!(accepted["state"], "pending");
    assert_eq!(accepted["x"], b + 100);
    assert_eq!(accepted["rot"].as_f64(), Some(0.0));
    let job_id = accepted["id"].as_str().unwrap().to_owned();

    // Quarantined bytes exist; no placement row yet.
    assert!(
        object(&server, &format!("quarantine/{job_id}")).is_some(),
        "upload stashed under quarantine/"
    );
    let p = pool(&server).await;
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM placements")
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(rows, 0, "no placement row before the consumer runs");

    // Status contract: pending.
    let status = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[("Cookie", &placer)],
        Vec::new(),
    )
    .await;
    assert_eq!(status.status, 200);
    assert_eq!(status.json(), json!({ "state": "pending" }));

    // The consumer runs the pixel stage and records.
    let job = consume_one(&server).await;
    assert_eq!(job.id.to_string(), job_id);

    // Status contract: placed (id = the placement's uuid).
    let status = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[("Cookie", &placer)],
        Vec::new(),
    )
    .await;
    assert_eq!(status.status, 200);
    let placed = status.json();
    assert_eq!(placed["state"], "placed");
    assert!(placed["seq"].as_i64().unwrap() > 0);
    assert_eq!(
        placed["bbox"],
        json!([b + 100, b + 100, 64, 64]),
        "bbox is the AABB of the accepted rect"
    );
    let placement_id = placed["id"].as_str().unwrap().to_owned();
    let (live,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM placements WHERE id = $1::uuid AND removed_at IS NULL",
    )
    .bind(&placement_id)
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(live, 1, "placement recorded");

    // The `placed` event on the bus (frozen schema).
    let event = next_event(&mut rx, "placed").await;
    assert_eq!(event["job"], job_id);
    assert_eq!(event["id"], placement_id);
    assert_eq!(event["bbox"], json!([b + 100, b + 100, 64, 64]));
    assert!(event["duplicate_count"].is_number());

    // Quarantine object cleaned up after the commit.
    assert!(object(&server, &format!("quarantine/{job_id}")).is_none());

    // The ingest consumer queued render jobs; painting them puts real
    // pixels on the tiles. Leaf tiles are 256 px: the z 20 tile holding
    // the art samples it 1:1 (chunk-local (110,110) is inside the first
    // 256 px quadrant).
    paint_one_chunk(&server).await;
    let bx = cx * 4;
    let tile = request(
        &server,
        "GET",
        &format!("/api/tiles/20/{bx}/{bx}.webp"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(tile.status, 200);
    let px = decode_pixels(tile.body, &[(110, 110)]).await;
    let red = [220, 40, 40];
    assert!(
        px[0].iter().zip(red).all(|(g, e)| g.abs_diff(e) <= 3),
        "quarantined art must reach the tile pixels: {:?}",
        px[0]
    );
}

/// Unknown upload ids are 404; other users' jobs are 404 too (no
/// existence leak), while the owner sees theirs.
#[tokio::test]
async fn unknown_and_foreign_uploads_are_404() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let placer = make_placer(&server, "root").await;
    let outsider = register(&server, "outsider").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_STATUS;
    let b = base(cx);

    // Unknown id.
    let missing = uuid::Uuid::now_v7();
    let resp = request(
        &server,
        "GET",
        &format!("/api/uploads/{missing}"),
        &[("Cookie", &placer)],
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status, 404);

    // A real pending job: owner ok, outsider 404.
    let resp = place(&server, &placer, &red, b + 110, b + 110, 64).await;
    assert_eq!(resp.status, 202);
    let job_id = resp.json()["id"].as_str().unwrap().to_owned();
    let resp = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[("Cookie", &outsider)],
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status, 404, "foreign jobs must not leak existence");
    let resp = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[("Cookie", &placer)],
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status, 200);
    // No cookie at all: 401-class, not 200.
    let resp = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert!((400..=404).contains(&resp.status), "unauthenticated is 4xx");
}

// ------------------------------------------------- permanent failures ----

/// Bytes that pass the header probe but fail the decode fail the job
/// permanently on the first attempt, emit `place_failed`, refund the
/// cooldown the 202 consumed, and leave the quarantine object for the
/// lifecycle rule.
#[tokio::test]
async fn decode_failure_fails_permanently_and_refunds_cooldown() {
    let _serial = pipeline_serial().await;
    // Non-exempt account: the 300 s paint cooldown is the thing being
    // refunded. Empty-space cooldown 1 s would also throttle, so use the
    // paint path (place #2 overlaps place #1's area? No — place #1 never
    // lands; the second placement lands in EMPTY space with a 1 s
    // cooldown, which is useless as a refund proof). Instead: assert the
    // refund directly on accounts.last_placement_at, then show a real
    // placement goes through.
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let user = register(&server, "cooled").await;
    // Passes the header probe (IHDR intact), fails the decode (truncated
    // body): the pixel stage must reject it.
    let mut hostile = img::solid_png(64, 64, [9, 9, 9]);
    hostile.truncate(hostile.len() - 24);
    let cx = CX_DECODE;
    let b = base(cx);
    let mut rx = subscribe(&server);

    // First: a VALID placement to anchor last_placement_at in the past…
    let anchor = place(
        &server,
        &user,
        &img::solid_png(64, 64, [10, 10, 10]),
        b + 10,
        b + 10,
        64,
    )
    .await;
    assert_eq!(anchor.status, 202);
    consume_one(&server).await;

    // …then wait out the 1 s empty-space cooldown, and submit the hostile
    // upload: the 202 consumed a cooldown (last_placement_at = now()).
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let resp = place(&server, &user, &hostile, b + 200, b + 200, 64).await;
    assert_eq!(resp.status, 202);
    let job_id = resp.json()["id"].as_str().unwrap().to_owned();
    let p = pool(&server).await;
    let (fresh,): (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT last_placement_at FROM accounts WHERE username = 'cooled'")
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(fresh.is_some(), "the 202 consumed a cooldown");

    // The consumer fails it permanently (probe ok, decode impossible).
    let job = ingest::claim_due(&p, 120).await.unwrap().expect("job");
    assert_eq!(job.id.to_string(), job_id);
    let err = ingest::execute_job(&server.state, job.clone())
        .await
        .expect_err("hostile bytes must fail the pixel stage");
    ingest::fail_job(&server.state, &job, &err, 1.0)
        .await
        .unwrap();

    let (state, attempts, _) = ingest_row(&server, &job_id).await.expect("row");
    assert_eq!(state, "failed");
    assert_eq!(attempts, 1, "permanent on the first attempt");
    let status = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[("Cookie", &user)],
        Vec::new(),
    )
    .await;
    let failed = status.json();
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["attempts"], 1);
    assert_eq!(failed["permanent"], true);
    assert!(!failed["error"].as_str().unwrap().is_empty());

    // The event names the job.
    let event = next_event(&mut rx, "place_failed").await;
    assert_eq!(event["job"], job_id);
    assert_eq!(event["permanent"], true);

    // The cooldown is refunded: last_placement_at is cleared (no later
    // placement moved it), so an immediate placement is accepted.
    let (refunded,): (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT last_placement_at FROM accounts WHERE username = 'cooled'")
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(refunded.is_none(), "cooldown must be refunded");
    let retry = place(
        &server,
        &user,
        &img::solid_png(64, 64, [10, 10, 10]),
        b + 200,
        b + 200,
        64,
    )
    .await;
    assert_eq!(retry.status, 202, "refunded cooldown accepts the retry");
}

// --------------------------------------------------------- removal 202 ----

/// DELETE under the ingest flag returns 202 and the consumer applies the
/// removal: removed_at, version bump, render jobs — and the pixels are
/// GONE from the tile (ghost-free decode).
#[tokio::test]
async fn removal_202_completes_ghost_free() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let admin = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_REMOVAL;
    let b = base(cx);

    let resp = place(&server, &admin, &red, b + 100, b + 100, 64).await;
    assert_eq!(resp.status, 202);
    let upload_id = resp.json()["id"].as_str().unwrap().to_owned();
    consume_one(&server).await;
    paint_one_chunk(&server).await;

    // DELETE → 202 with the pending job id and the placement rect.
    let resp = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{upload_id}"),
        &[("Cookie", &admin)],
        Vec::new(),
    )
    .await;
    assert_eq!(
        resp.status, 404,
        "delete keys on the PLACEMENT id, not the upload id"
    );
    let p = pool(&server).await;
    let (placement_id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM placements LIMIT 1")
        .fetch_one(&p)
        .await
        .unwrap();

    let mut rx2 = subscribe(&server);
    let resp = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{placement_id}"),
        &[("Cookie", &admin)],
        Vec::new(),
    )
    .await;
    assert_eq!(resp.status, 202);
    let removal = resp.json();
    assert_eq!(removal["state"], "pending");
    assert_eq!(removal["x"], b + 100);
    let removal_id = removal["id"].as_str().unwrap().to_owned();

    consume_one(&server).await;

    // Job status: placed; the remove event rode the bus.
    let status = request(
        &server,
        "GET",
        &format!("/api/uploads/{removal_id}"),
        &[("Cookie", &admin)],
        Vec::new(),
    )
    .await;
    assert_eq!(status.json()["state"], "placed");
    let event = next_event(&mut rx2, "remove").await;
    assert_eq!(event["id"], placement_id.to_string());

    let (gone,): (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT removed_at FROM placements WHERE id = $1")
            .bind(placement_id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(gone.is_some(), "consumer must set removed_at");

    // Ghost-free pixels: repaint the chunk and decode the z 20 leaf that
    // held the art (1:1 sampling).
    paint_one_chunk(&server).await;
    let bx = cx * 4;
    let tile = request(
        &server,
        "GET",
        &format!("/api/tiles/20/{bx}/{bx}.webp"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(tile.status, 200);
    let px = decode_pixels(tile.body, &[(110, 110)]).await;
    let red = [220, 40, 40];
    assert!(
        !px[0].iter().zip(red).all(|(g, e)| g.abs_diff(e) <= 3),
        "removed art must not survive in the tile (ghost): {:?}",
        px[0]
    );
    let _ = ingest_row(&server, &upload_id).await;
}

/// Two rapid DELETEs both 202; the consumer applies the removal once and
/// completes both jobs idempotently.
#[tokio::test]
async fn double_removal_is_idempotent() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let admin = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_EXCLUSIVE + 2; // private neighbor column
    let b = base(cx);

    let resp = place(&server, &admin, &red, b + 110, b + 110, 64).await;
    assert_eq!(resp.status, 202);
    consume_one(&server).await;
    let p = pool(&server).await;
    let (placement_id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM placements LIMIT 1")
        .fetch_one(&p)
        .await
        .unwrap();

    let resp1 = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{placement_id}"),
        &[("Cookie", &admin)],
        Vec::new(),
    )
    .await;
    let resp2 = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{placement_id}"),
        &[("Cookie", &admin)],
        Vec::new(),
    )
    .await;
    assert_eq!(resp1.status, 202);
    assert_eq!(resp2.status, 202);

    consume_one(&server).await;
    consume_one(&server).await;

    let (states,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM ingest_jobs WHERE kind = 'removal' AND state = 'placed'",
    )
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(states, 2, "both removal jobs complete");
    let (removed,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM placements WHERE id = $1 AND removed_at IS NOT NULL")
            .bind(placement_id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert_eq!(removed, 1);
}

// ------------------------------------------------ claim loop machinery ----

/// Two concurrent claimers race for one due job: SKIP LOCKED gives it to
/// exactly one; the lease expiry re-claims and bumps attempts.
#[tokio::test]
async fn ingest_claim_is_exclusive_between_two_claimers() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let placer = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let b = base(CX_EXCLUSIVE);
    let resp = place(&server, &placer, &red, b + 110, b + 110, 64).await;
    assert_eq!(resp.status, 202);

    let p = pool(&server).await;
    let (a, b2) = tokio::join!(ingest::claim_due(&p, 120), ingest::claim_due(&p, 120),);
    let winners = [
        a.expect("claim a").is_some(),
        b2.expect("claim b").is_some(),
    ]
    .into_iter()
    .filter(|won| *won)
    .count();
    assert_eq!(winners, 1, "exactly one claimer wins the ingest job");

    // A running job is not re-claimable until the lease expires; the
    // claim is the retry and attempts advanced.
    assert!(
        ingest::claim_due(&p, 120).await.unwrap().is_none(),
        "running jobs are not re-claimable"
    );
    sqlx::query("UPDATE ingest_jobs SET claimed_at = now() - interval '200 seconds'")
        .execute(&p)
        .await
        .unwrap();
    let reclaimed = ingest::claim_due(&p, 120).await.unwrap();
    assert!(reclaimed.is_some(), "expired lease re-claims");
    let (attempts,): (i16,) = sqlx::query_as("SELECT attempts FROM ingest_jobs")
        .fetch_one(&p)
        .await
        .unwrap();
    assert_eq!(attempts, 2, "each claim increments attempts");
}

/// Transient failures retry with backoff; the ceiling marks the job
/// failed after MAX_INGEST_ATTEMPTS and the status contract reports it.
#[tokio::test]
async fn transient_failures_exhaust_to_failed() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        quarantine_cfg(),
    )
    .await;
    let placer = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let b = base(CX_DECODE - 2);
    let resp = place(&server, &placer, &red, b + 110, b + 110, 64).await;
    assert_eq!(resp.status, 202);
    let job_id = resp.json()["id"].as_str().unwrap().to_owned();

    let p = pool(&server).await;
    let mut rx = subscribe(&server);
    // A transient error class (internal): retry, never permanent-until-ceiling.
    for attempt in 1..=MAX_INGEST_ATTEMPTS {
        let job = ingest::claim_due(&p, 120).await.unwrap().expect("job due");
        assert_eq!(job.id.to_string(), job_id);
        let err = idol_appreciation_club_backend::error::internal("simulated transient");
        ingest::fail_job(&server.state, &job, &err, 0.0)
            .await
            .unwrap();
        let (state, attempts, _) = ingest_row(&server, &job_id).await.unwrap();
        if attempt < MAX_INGEST_ATTEMPTS {
            assert_eq!(state, "pending", "transient failures retry");
        } else {
            assert_eq!(state, "failed", "the ceiling marks the job failed");
        }
        assert_eq!(attempts, attempt);
    }

    // Only ONE place_failed event: the terminal one.
    let event = next_event(&mut rx, "place_failed").await;
    assert_eq!(event["job"], job_id);
    assert_eq!(event["attempts"], i64::from(MAX_INGEST_ATTEMPTS));
    assert_eq!(event["permanent"], true);
    let status = request(
        &server,
        "GET",
        &format!("/api/uploads/{job_id}"),
        &[("Cookie", &placer)],
        Vec::new(),
    )
    .await;
    assert_eq!(status.json()["attempts"], i64::from(MAX_INGEST_ATTEMPTS));
}

/// The real spawned consumer drains the queue end-to-end (poll loop
/// wiring, not just the seam).
#[tokio::test]
async fn spawned_consumer_drains_end_to_end() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        common::PipelineCfg {
            enabled: true,
            ingest: true,
            workers: 1,
            poll_ms: 15,
            ..Default::default()
        },
    )
    .await;
    let placer = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let b = base(CX_QUARANTINE - 2);
    let resp = place(&server, &placer, &red, b + 110, b + 110, 64).await;
    assert_eq!(resp.status, 202);
    let job_id = resp.json()["id"].as_str().unwrap().to_owned();

    // Poll the status endpoint until placed (the consumer's claim loop
    // does the work; nothing here is manual).
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let resp = request(
            &server,
            "GET",
            &format!("/api/uploads/{job_id}"),
            &[("Cookie", &placer)],
            Vec::new(),
        )
        .await;
        assert_eq!(resp.status, 200);
        if resp.json()["state"] == "placed" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "consumer never placed the upload"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (live,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM placements WHERE removed_at IS NULL")
            .fetch_one(&pool(&server).await)
            .await
            .unwrap();
    assert_eq!(live, 1);
}
