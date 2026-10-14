//! Phase 3 render pipeline (migration 0008, `render_jobs` worker): claim
//! exclusivity, the painted_seq monotonic guard, rerun-not-abort, parent
//! bubble-up version math, removal rebuilds with pixel-decode assertions,
//! the failed-after-5-attempts ceiling, and the dark-ship contract — the
//! request path byte-identical while RENDER_PIPELINE_ENABLED=false.

mod common;

use std::time::Duration;

use common::*;
use idol_appreciation_club_backend::img;
use idol_appreciation_club_backend::render_jobs;
use idol_appreciation_club_backend::render_jobs::MAX_JOB_ATTEMPTS;
use sqlx::{PgPool, Row};

/// One postgres (CI service / sidecar) is shared across binaries and
/// across the --test-threads 2 tests of this binary. Within this binary
/// every test takes the serial lock (the shared pause seam's), and every
/// test owns a PRIVATE chunk column (even numbers, 2 apart, so each owns
/// its z 17 tile; shared z ≤ 16 ancestors only ever GREATEST with
/// strictly-later sequence values, so exact version asserts stay valid
/// regardless of test order). Across binaries each server boots its own
/// throwaway database (see common::provision_isolated_db).
const CX_GUARD: i64 = CENTER / 1024 - 8;
const CX_RERUN: i64 = CENTER / 1024 - 6;
const CX_BUBBLE: i64 = CENTER / 1024 - 4;
const CX_REMOVAL: i64 = CENTER / 1024 - 2;
const CX_DARK: i64 = CENTER / 1024 + 2;

/// World-space base of a chunk column (world coords are non-negative, so
/// `/` == floor).
fn base(col: i64) -> i64 {
    col * 1024
}

async fn pool(server: &Server) -> PgPool {
    PgPool::connect(&server.db_url).await.expect("test pool")
}

async fn count_active(server: &Server) -> i64 {
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM render_jobs WHERE state IN ('pending', 'running')")
            .fetch_one(&pool(server).await)
            .await
            .expect("count jobs");
    n
}

/// Wait until no pending/running jobs remain (finished rows are deleted),
/// stable across two polls.
async fn drain(server: &Server) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let a = count_active(server).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let b = count_active(server).await;
        if a == 0 && b == 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "render_jobs never drained (last counts {a}, {b})"
        );
    }
}

/// (max_seq, painted_seq, needs_rebuild) for a chunk.
async fn chunk_state(server: &Server, cx: i64, cy: i64) -> (i64, i64, bool) {
    let row = sqlx::query(
        "SELECT max_seq, painted_seq, needs_rebuild FROM chunk_seqs
         WHERE cx = $1 AND cy = $2",
    )
    .bind(cx)
    .bind(cy)
    .fetch_one(&pool(server).await)
    .await
    .expect("chunk_seqs row");
    (
        row.get::<i64, _>("max_seq"),
        row.get::<i64, _>("painted_seq"),
        row.get::<bool, _>("needs_rebuild"),
    )
}

async fn tile_seq(server: &Server, z: i64, x: i64, y: i64) -> Option<i64> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT seq FROM tiles WHERE z = $1 AND x = $2 AND y = $3")
            .bind(z as i16)
            .bind(x as i32)
            .bind(y as i32)
            .fetch_optional(&pool(server).await)
            .await
            .expect("tiles row");
    row.map(|(s,)| s)
}

/// One (state, rerun, attempts) row for a job coordinate.
async fn job_row(server: &Server, z: i64, x: i64, y: i64) -> Option<(String, bool, i16)> {
    let row = sqlx::query(
        "SELECT state, rerun, attempts FROM render_jobs
         WHERE z = $1 AND x = $2 AND y = $3",
    )
    .bind(z as i16)
    .bind(x as i32)
    .bind(y as i32)
    .fetch_optional(&pool(server).await)
    .await
    .expect("job row");
    row.map(|r| {
        (
            r.get::<String, _>("state"),
            r.get::<bool, _>("rerun"),
            r.get::<i16, _>("attempts"),
        )
    })
}

async fn set_chunk_state(server: &Server, cx: i64, cy: i64, painted: i64, rebuild: bool) {
    sqlx::query(
        "UPDATE chunk_seqs SET painted_seq = $3, needs_rebuild = $4
         WHERE cx = $1 AND cy = $2",
    )
    .bind(cx)
    .bind(cy)
    .bind(painted)
    .bind(rebuild)
    .execute(&pool(server).await)
    .await
    .expect("set chunk state");
}

async fn plant_job(server: &Server, z: i64, x: i64, y: i64) {
    sqlx::query("INSERT INTO render_jobs (z, x, y) VALUES ($1, $2, $3)")
        .bind(z as i16)
        .bind(x as i32)
        .bind(y as i32)
        .execute(&pool(server).await)
        .await
        .expect("plant job");
}

/// Read an object straight from the file-backed store bucket (the dev
/// store nests the bucket under the configured endpoint directory).
fn object(server: &Server, key: &str) -> Option<Vec<u8>> {
    std::fs::read(server._store_dir.join("test-bucket").join(key)).ok()
}

/// Object-storage key of a pyramid tile (same layout as `world::tile_key`).
fn tile_object_key(z: i64, x: i64, y: i64) -> String {
    format!("tiles/{z}/{x}/{y}.webp")
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

async fn wait_job_state(server: &Server, z: i64, x: i64, y: i64, state: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some((s, rerun, _)) = job_row(server, z, x, y).await {
            if s == state {
                return;
            }
            assert!(!rerun, "unexpected rerun while waiting for {state}");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job ({z},{x},{y}) never reached {state}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ----------------------------------------------- claim exclusivity ----

#[tokio::test]
async fn claim_is_exclusive_between_two_claimers() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        PipelineCfg {
            enabled: false, // no worker: claims are driven below
            poll_ms: 60_000,
            ..Default::default()
        },
    )
    .await;
    let pool = pool(&server).await;
    plant_job(&server, 18, 4, 4).await;

    // Two concurrent claimers race for the one job: SKIP LOCKED must give
    // it to exactly one.
    let (a, b) = tokio::join!(
        render_jobs::claim_due(&pool, 120),
        render_jobs::claim_due(&pool, 120),
    );
    let winners = [a.expect("claim a").is_some(), b.expect("claim b").is_some()]
        .into_iter()
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1, "exactly one claimer wins the job");

    // A claimed (running) row is not re-claimable.
    assert_eq!(render_jobs::claim_due(&pool, 120).await.unwrap(), None);

    // …until the crash lease expires: the claim IS the retry, and the
    // attempt counter advanced with each claim.
    sqlx::query("UPDATE render_jobs SET claimed_at = now() - interval '200 seconds'")
        .execute(&pool)
        .await
        .unwrap();
    let reclaimed = render_jobs::claim_due(&pool, 120).await.unwrap();
    assert_eq!(reclaimed, Some((18, 4, 4)));
    let (attempts,): (i16,) = sqlx::query_as("SELECT attempts FROM render_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 2, "lease reclaim counts as an attempt");

    // Leave nothing behind for the shared database.
    sqlx::query("DELETE FROM render_jobs")
        .execute(&pool)
        .await
        .unwrap();
}

// --------------------------------------------- painted_seq guard ----

#[tokio::test]
async fn painted_seq_guard_never_moves_backwards() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        PipelineCfg::fast(),
    )
    .await;
    let admin = make_placer(&server, "guarder").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_GUARD;
    let b = base(cx);
    let a = place(&server, &admin, &red, b + 100, b + 100, 64).await;
    assert_eq!(a.status, 201);
    drain(&server).await;
    let (max_seq, painted, rebuild) = chunk_state(&server, cx, cx).await;
    assert_eq!(painted, max_seq, "drained chunk is fully painted");
    assert!(!rebuild);

    // Simulate a SLOWER job finishing after a newer one: painted_seq is
    // already far ahead and a rebuild is (re-)flagged. The guard
    // (GREATEST, never below the stored version) must refuse the
    // backwards write while still consuming the rebuild flag — otherwise
    // the job could hot-loop on a flag it can never clear.
    let ahead = painted + 10_000;
    set_chunk_state(&server, cx, cx, ahead, true).await;
    plant_job(&server, 18, cx, cx).await;
    drain(&server).await;

    let (max_seq2, painted2, rebuild2) = chunk_state(&server, cx, cx).await;
    assert_eq!(painted2, ahead, "painted_seq never moves backwards");
    assert_eq!(max_seq2, max_seq);
    assert!(!rebuild2, "the rebuild consumed its flag");
    assert!(
        job_row(&server, 18, cx, cx).await.is_none(),
        "finished rows are deleted, no guard hot-loop"
    );
}

// -------------------------------------------------- rerun, no abort ----

#[tokio::test]
async fn rerun_requeues_running_job_instead_of_aborting() {
    let pause = set_paint_pause_ms(5_000).await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        PipelineCfg::fast(),
    )
    .await;
    let admin = make_placer(&server, "runner").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let green = img::solid_png(48, 48, [30, 180, 60]);
    let cx = CX_RERUN;
    let b = base(cx);

    let a = place(&server, &admin, &red, b + 100, b + 100, 64).await;
    assert_eq!(a.status, 201);
    // The worker claims while paused: the job is RUNNING, mid-paint.
    wait_job_state(&server, 18, cx, cx, "running").await;

    // A change arrives while the job runs: never aborted — rerun.
    let b2 = place(&server, &admin, &green, b + 140, b + 140, 48).await;
    assert_eq!(b2.status, 201);
    let (state, rerun, _) = job_row(&server, 18, cx, cx).await.expect("row");
    assert_eq!(state, "running");
    assert!(rerun, "a change during a running job sets rerun");

    // The running pass finishes, re-queues itself, and the second pass
    // catches up to both placements; then the row is deleted.
    drop(pause);
    drain(&server).await;
    assert!(
        job_row(&server, 18, cx, cx).await.is_none(),
        "finished rows are deleted"
    );
    let (max_seq, painted, rebuild) = chunk_state(&server, cx, cx).await;
    assert_eq!(painted, max_seq);
    assert!(!rebuild);

    let bytes = object(&server, &format!("chunks/{cx}/{cx}.webp")).expect("chunk object");
    // Chunk rasters are RGBA: sample painted areas only (unpainted is
    // transparent, not paper — flattening happens at tile render).
    let px = decode_pixels(bytes, &[(110, 110), (150, 150)]).await;
    let red = [220, 40, 40];
    let green = [30, 180, 60];
    assert!(
        px[0].iter().zip(red).all(|(g, e)| g.abs_diff(e) <= 3),
        "red survives: {:?}",
        px[0]
    );
    assert!(
        px[1].iter().zip(green).all(|(g, e)| g.abs_diff(e) <= 3),
        "green painted by rerun: {:?}",
        px[1]
    );
}

// --------------------------------------------- parent bubble-up math ----

#[tokio::test]
async fn parent_bubble_up_versions_are_max_of_children() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        PipelineCfg::fast(),
    )
    .await;
    let admin = make_placer(&server, "bubbler").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let green = img::solid_png(48, 48, [30, 180, 60]);
    let cx = CX_BUBBLE;
    let b = base(cx);

    let a = place(&server, &admin, &red, b + 100, b + 100, 64).await;
    assert_eq!(a.status, 201);
    drain(&server).await;
    let (s1, painted, _) = chunk_state(&server, cx, cx).await;
    assert_eq!(painted, s1);

    // The whole spine chunk → z 0 carries the chunk's version.
    for z in 0..=18i64 {
        let x = cx >> (18 - z);
        let y = cx >> (18 - z);
        assert_eq!(
            tile_seq(&server, z, x, y).await,
            Some(s1),
            "z{z} spine version after first placement"
        );
    }

    // Second placement in the NEIGHBOR chunk (shares every parent above
    // z 17). Parent versions are the max over their children, which only
    // grows; the unchanged sibling keeps its own version.
    let b2 = place(&server, &admin, &green, b + 1024 + 100, b + 100, 48).await;
    assert_eq!(b2.status, 201);
    drain(&server).await;
    let (s2, painted2, _) = chunk_state(&server, cx + 1, cx).await;
    let (_, painted_a, _) = chunk_state(&server, cx, cx).await;
    assert_eq!(painted2, s2);
    assert!(s2 > s1, "versions only move forward");
    assert_eq!(tile_seq(&server, 18, cx, cx).await, Some(painted_a));
    assert_eq!(tile_seq(&server, 18, cx + 1, cx).await, Some(s2));
    for z in 0..=17i64 {
        let x = (cx + 1) >> (18 - z);
        let y = cx >> (18 - z);
        assert_eq!(
            tile_seq(&server, z, x, y).await,
            Some(s2),
            "z{z} parent version must be the max child version"
        );
    }
    assert_eq!(
        tile_seq(&server, 0, 0, 0).await,
        Some(s2),
        "z0 never decreases"
    );
}

// ------------------------------------------------ removal → rebuild ----

#[tokio::test]
async fn removal_rebuild_renders_ghost_free_pixels() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        PipelineCfg::fast(),
    )
    .await;
    let admin = make_placer(&server, "root").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_REMOVAL;
    let b = base(cx);

    let a = place(&server, &admin, &red, b + 100, b + 100, 64).await;
    assert_eq!(a.status, 201);
    let aid = a.json()["id"].as_str().unwrap().to_owned();
    drain(&server).await;
    let (_, painted_before, _) = chunk_state(&server, cx, cx).await;
    let chunk = object(&server, &format!("chunks/{cx}/{cx}.webp")).expect("chunk");
    // Chunk rasters are RGBA (unpainted = transparent): assert the red
    // art is present; paper assertions belong to flattened tiles below.
    let px = decode_pixels(chunk, &[(110, 110)]).await;
    let red = [220, 40, 40];
    let paper = [243, 240, 232];
    assert!(
        px[0].iter().zip(red).all(|(g, e)| g.abs_diff(e) <= 3),
        "red painted: {:?}",
        px[0]
    );

    // Removal: the request returns immediately (no replay inside the
    // request); the chunk is flagged and rebuilt in the worker.
    let del = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{aid}"),
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(del.status, 204);
    let (_, _, flagged) = chunk_state(&server, cx, cx).await;
    assert!(flagged, "removal sets needs_rebuild");
    drain(&server).await;

    let (max_seq, painted_after, rebuild) = chunk_state(&server, cx, cx).await;
    assert!(!rebuild, "rebuild consumed the flag");
    assert!(
        painted_after > painted_before,
        "removal advanced the version"
    );
    assert_eq!(painted_after, max_seq);
    assert!(job_row(&server, 18, cx, cx).await.is_none());

    // Pixel-decode assertion: the ghost is GONE from the chunk raster and
    // from the rebuilt parent tiles.
    let chunk = object(&server, &format!("chunks/{cx}/{cx}.webp")).expect("chunk after");
    let px = decode_pixels(chunk, &[(110, 110)]).await;
    assert_ne!(
        px[0], red,
        "removed image's pixels survived the rebuild (ghost)"
    );
    // z 17 parent tile: flattened to paper; our chunk is its top-left
    // quadrant (110/8 = 13).
    let parent = object(&server, &tile_object_key(17, cx / 2, cx / 2)).expect("z17 tile");
    let ppx = decode_pixels(parent, &[(13, 13), (200, 200)]).await;
    assert_ne!(ppx[0], red, "removed pixels must not survive in parents");
    assert!(
        ppx[1].iter().zip(paper).all(|(g, e)| g.abs_diff(e) <= 3),
        "untouched parent area must stay paper (got {:?})",
        ppx[1]
    );
}

// -------------------------------------------- failed after 5 attempts ----

#[tokio::test]
async fn job_fails_after_five_attempts() {
    let _serial = pipeline_serial().await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        Default::default(),
        PipelineCfg {
            enabled: false,
            poll_ms: 60_000,
            ..Default::default()
        },
    )
    .await;
    let pool = pool(&server).await;
    // The attempts ceiling is bookkeeping on the job row: drive claim →
    // fail (as a failing execution would) five times and watch the row
    // park as `failed` instead of retrying forever.
    plant_job(&server, 17, 2, 3).await;

    for attempt in 1..=MAX_JOB_ATTEMPTS {
        let claimed = render_jobs::claim_due(&pool, 120).await.unwrap();
        assert_eq!(claimed, Some((17, 2, 3)));
        render_jobs::fail_job(&pool, 17, 2, 3, 0.0).await.unwrap();
        let (state, _, attempts) = job_row(&server, 17, 2, 3).await.expect("row kept");
        assert_eq!(attempts, attempt);
        if attempt < MAX_JOB_ATTEMPTS {
            assert_eq!(state, "pending", "retry before the ceiling");
        } else {
            assert_eq!(state, "failed", "5 attempts park the job as failed");
        }
    }
    // A failed row is not claimable: no retrying forever.
    assert_eq!(render_jobs::claim_due(&pool, 120).await.unwrap(), None);

    // A fresh change revives it (a new change is a new job).
    sqlx::query(
        "INSERT INTO render_jobs (z, x, y) VALUES (17, 2, 3)
         ON CONFLICT (z, x, y) DO UPDATE SET state = 'pending'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (state, _, attempts) = job_row(&server, 17, 2, 3).await.expect("row");
    assert_eq!(state, "pending");
    assert_eq!(attempts, 5, "revival keeps history");
}

// ------------------------------------------- dark-ship contract ----

#[tokio::test]
async fn pipeline_disabled_keeps_request_path_unchanged() {
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
    let admin = make_placer(&server, "dark").await;
    let red = img::solid_png(64, 64, [220, 40, 40]);
    let cx = CX_DARK;
    let b = base(cx);

    // Placement paints SYNCHRONOUSLY (no worker): pixels are on the chunk
    // the instant the 201 returns, and the pipeline tables are untouched.
    let a = place(&server, &admin, &red, b + 100, b + 100, 64).await;
    assert_eq!(a.status, 201);
    assert_eq!(count_active(&server).await, 0, "no render_jobs while dark");
    let (max_seq, painted, rebuild) = chunk_state(&server, cx, cx).await;
    assert_eq!(painted, 0, "dark path never advances painted_seq");
    assert!(!rebuild);
    assert!(max_seq > 0);
    let chunk = object(&server, &format!("chunks/{cx}/{cx}.webp")).expect("chunk painted inline");
    let px = decode_pixels(chunk, &[(110, 110)]).await;
    let red = [220, 40, 40];
    assert!(px[0].iter().zip(red).all(|(g, e)| g.abs_diff(e) <= 3));

    // Serving path unchanged: fresh tile with today's cache semantics.
    let bx = (b + 100) / 256;
    let tile = request(
        &server,
        "GET",
        &format!("/api/tiles/20/{bx}/{bx}.webp"),
        &[],
        vec![],
    )
    .await;
    assert_eq!(tile.status, 200);
    assert_eq!(tile.header("x-tile-stale"), None);

    // Removal replays synchronously: ghost gone the instant 204 returns,
    // still zero pipeline rows.
    let aid = a.json()["id"].as_str().unwrap().to_owned();
    let del = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{aid}"),
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(del.status, 204);
    let chunk = object(&server, &format!("chunks/{cx}/{cx}.webp")).expect("chunk rebuilt inline");
    let px = decode_pixels(chunk, &[(110, 110), (250, 250)]).await;
    assert_ne!(px[0], red, "ghost must be gone (synchronous replay)");
    assert_eq!(
        count_active(&server).await,
        0,
        "still no render_jobs while dark"
    );
}
