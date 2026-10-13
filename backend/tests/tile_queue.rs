//! Tile render admission-control behavior: server-side stale-while-
//! revalidate, the bounded PG-backed render queue (quiet window vs
//! max-delay cap), drop/shed policy, and placement-latency isolation.
//!
//! Tests here share the process-global metrics registry and the render
//! pause seam, so they serialize on one mutex (see `common::PauseGuard`);
//! they run in their own binary, isolated from `integration.rs`.

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Raw single GET used by concurrent bursts (captures the addr, not the
/// Server, so tasks stay 'static).
async fn burst_get(addr: SocketAddr, path: &str) -> (u16, HashMap<String, String>, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
        .await
        .expect("response within 20s")
        .unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
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
    (status, headers, raw[split + 4..].to_vec())
}

fn counter(map: &HashMap<String, String>, series: &str) -> u64 {
    map.get(series)
        .unwrap_or_else(|| panic!("missing series {series}"))
        .parse()
        .expect("counter value")
}

/// Counter DELTA vs a snapshot: the metrics registry is process-global
/// and shared by every test in this binary, so assertions must be relative.
async fn counter_delta(server: &Server, series: &str, snapshot: u64) -> u64 {
    counter(&metrics_map(server).await, series).saturating_sub(snapshot)
}

/// Burst of stale GETs coalesces into exactly ONE background render, and
/// the burst itself is served instantly from stored bytes.
#[tokio::test(flavor = "multi_thread")]
async fn stale_burst_coalesces_into_one_render() {
    // Quiet 2s: the render fires 2-3s after the last bump. Max-delay never
    // drives. No pause needed — the burst window is the quiet window.
    let _serial = set_render_pause_ms(0).await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        TileRenderCfg {
            quiet_secs: 2,
            max_delay_secs: 60,
            cap: 512,
        },
    )
    .await;
    let placer = make_placer(&server, "bursty").await;
    let bg0 = counter(&metrics_map(&server).await, "tile_background_renders_total");
    let ss0 = counter(
        &metrics_map(&server).await,
        "tile_requests_total{outcome=\"stale_served\"}",
    );
    let x = 1 << 20;
    let tile20 = format!("/api/tiles/20/{}/{}.webp", x / 256, x / 256);

    // First render: no stored tile → inline, fresh, no queue entry.
    assert_eq!(
        place(&server, &placer, &png(64, 64), x, x, 64).await.status,
        201
    );
    let t1 = request(&server, "GET", &tile20, &[], vec![]).await;
    assert_eq!(t1.headers["etag"], "\"1\"");
    assert_eq!(t1.headers.get("x-tile-stale"), None);

    // Burst: two overlapping placements, then 8 concurrent GETs. Every
    // response is the STORED bytes with the stored ETag + stale marker.
    assert_eq!(
        place(&server, &placer, &png(96, 96), x + 16, x + 16, 96)
            .await
            .status,
        201
    );
    assert_eq!(
        place(&server, &placer, &png(96, 96), x + 32, x + 32, 96)
            .await
            .status,
        201
    );
    let mut bursts = Vec::new();
    for _ in 0..8 {
        bursts.push(burst_get(server.addr, &tile20));
    }
    for out in futures::future::join_all(bursts).await {
        let (status, headers, body) = out;
        assert_eq!(status, 200);
        assert_eq!(headers["etag"], "\"1\"");
        assert_eq!(headers["x-tile-stale"], "true");
        assert_eq!(body, t1.body, "stale bytes are the stored ones");
    }

    // ONE background render, at the latest seq, then the tile converges.
    let fresh = tile_fresh(&server, &tile20, "\"3\"", Duration::from_secs(3)).await;
    assert_eq!(fresh.headers.get("x-tile-stale"), None);
    assert_ne!(fresh.body, t1.body);

    let m = metrics_map(&server).await;
    assert_eq!(
        counter_delta(&server, "tile_background_renders_total", bg0).await,
        1
    );
    let stale = counter(&m, "tile_requests_total{outcome=\"stale_served\"}") - ss0;
    assert!(stale >= 8, "expected >= 8 stale serves, got {stale}");
    assert_eq!(counter(&m, "tile_render_queue_depth"), 0, "queue drained");
}

/// Continuous placement bumps keep resetting the quiet window; the
/// max-delay cap fires renders anyway, at roughly the cap rate.
#[tokio::test(flavor = "multi_thread")]
async fn max_delay_fires_under_continuous_bumps() {
    let _serial = set_render_pause_ms(150).await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        TileRenderCfg {
            quiet_secs: 60, // quiet never fires; the cap must
            max_delay_secs: 1,
            cap: 512,
        },
    )
    .await;
    let placer = make_placer(&server, "soaker").await;
    let bg0 = counter(&metrics_map(&server).await, "tile_background_renders_total");
    let x = 1 << 20;
    let tile20 = format!("/api/tiles/20/{}/{}.webp", x / 256, x / 256);

    assert_eq!(
        place(&server, &placer, &png(64, 64), x, x, 64).await.status,
        201
    );
    assert_eq!(
        request(&server, "GET", &tile20, &[], vec![]).await.headers["etag"],
        "\"1\""
    );

    // ~3s of continuous placement+GET traffic, each GET re-bumping the
    // queued entry.
    let mut stale_serves = 0u64;
    for i in 1..=30 {
        assert_eq!(
            place(&server, &placer, &png(48, 48), x + i * 4, x + i * 4, 48)
                .await
                .status,
            201
        );
        let resp = request(&server, "GET", &tile20, &[], vec![]).await;
        if resp.headers.contains_key("x-tile-stale") {
            stale_serves += 1;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    assert!(
        stale_serves >= 20,
        "expected mostly-stale serves, got {stale_serves}"
    );

    // Cap-rate renders happened DESPITE the never-quiet window.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if counter_delta(&server, "tile_background_renders_total", bg0).await >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "cap-rate renders never fired");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // And the tile converges to the final seq.
    tile_fresh(&server, &tile20, "\"31\"", Duration::from_secs(1)).await;
}

/// A seq bump landing mid-render does not double-render or wedge the tile:
/// the in-flight render re-reads the CURRENT seq, so the tile converges to
/// the newest state with a single render; the redundant re-enqueue during
/// 'rendering' is a counted no-op.
#[tokio::test(flavor = "multi_thread")]
async fn seq_bump_during_render_converges_once() {
    let _serial = set_render_pause_ms(400).await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        TileRenderCfg {
            quiet_secs: 1,
            max_delay_secs: 60,
            cap: 512,
        },
    )
    .await;
    let placer = make_placer(&server, "racer").await;
    let bg0 = counter(&metrics_map(&server).await, "tile_background_renders_total");
    let x = 1 << 20;
    let tile20 = format!("/api/tiles/20/{}/{}.webp", x / 256, x / 256);

    assert_eq!(
        place(&server, &placer, &png(64, 64), x, x, 64).await.status,
        201
    );
    assert_eq!(
        request(&server, "GET", &tile20, &[], vec![]).await.headers["etag"],
        "\"1\""
    );

    // Enqueue (stored seq 1, live seq 2), wait for the worker to claim it
    // (depth 1 → 0) and enter the paused render.
    assert_eq!(
        place(&server, &placer, &png(64, 64), x + 8, x + 8, 64)
            .await
            .status,
        201
    );
    let stale = request(&server, "GET", &tile20, &[], vec![]).await;
    assert_eq!(stale.headers["etag"], "\"1\"");
    assert_eq!(stale.headers["x-tile-stale"], "true");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let depth = counter(&metrics_map(&server).await, "tile_render_queue_depth");
        if depth == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "render was never claimed");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Mid-render: another placement bumps the seq; the GET during
    // 'rendering' serves stale and its enqueue attempt is a no-op.
    assert_eq!(
        place(&server, &placer, &png(64, 64), x + 16, x + 16, 64)
            .await
            .status,
        201
    );
    let mid = request(&server, "GET", &tile20, &[], vec![]).await;
    assert_eq!(mid.headers["x-tile-stale"], "true");

    // The render completes at the CURRENT seq (3) — no second render.
    let fresh = tile_fresh(&server, &tile20, "\"3\"", Duration::from_secs(2)).await;
    assert_ne!(fresh.body, stale.body);
    let m = metrics_map(&server).await;
    assert_eq!(
        counter_delta(&server, "tile_background_renders_total", bg0).await,
        1
    );
    assert!(
        counter(&m, "tile_render_queue_dropped_total") >= 1,
        "enqueue attempts during 'rendering' must be recorded drops"
    );
}

/// The point of the whole design: a saturated render queue leaves the
/// placement path at baseline latency (renders never queue a placement
/// behind them, and only one render ever runs at a time).
#[tokio::test(flavor = "multi_thread")]
async fn placement_latency_unaffected_by_saturated_queue() {
    // Each "render" takes 5s: any placement queued behind one would be
    // instantly visible.
    let _serial = set_render_pause_ms(5000).await;
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        TileRenderCfg {
            quiet_secs: 1,
            max_delay_secs: 60,
            cap: 512,
        },
    )
    .await;
    let placer = make_placer(&server, "poster").await;
    let base = 1 << 21;

    // Baseline: 4 placements into one dense pile.
    let img = png(64, 64);
    let mut baseline = Vec::new();
    for i in 0..4i64 {
        let t0 = Instant::now();
        let resp = place(&server, &placer, &img, base + i * 16, base + i * 16, 64).await;
        assert_eq!(resp.status, 201);
        baseline.push(t0.elapsed());
    }
    let baseline_max = baseline.iter().copied().max().unwrap();
    assert!(
        baseline_max < Duration::from_millis(1500),
        "baseline placements must be fast, max was {baseline_max:?}"
    );

    // Fill the queue: 20 distinct tiles, each made stale after a first
    // (row-creating) fetch. quiet=1s ⇒ the worker claims and grinds
    // (5s per entry) while we measure.
    for i in 0..20i64 {
        let jx = base + 1024 + i * 512;
        assert_eq!(place(&server, &placer, &img, jx, jx, 64).await.status, 201);
        let path = format!("/api/tiles/20/{}/{}.webp", jx / 256, jx / 256);
        // ETags carry the placement's GLOBAL seq (baseline already consumed
        // 1-4) — just require a real, nonzero etag; the row is what matters.
        let etag = request(&server, "GET", &path, &[], vec![]).await.headers["etag"].clone();
        assert_ne!(etag, "\"0\"", "job tile must have art");
        let _ = etag;
        assert_eq!(
            place(&server, &placer, &img, jx + 8, jx + 8, 64)
                .await
                .status,
            201
        );
        let stale = request(&server, "GET", &path, &[], vec![]).await;
        assert_eq!(stale.headers["x-tile-stale"], "true");
    }

    // Give the worker a moment to claim entry #1 and enter its 5s pause.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let depth_mid = counter(&metrics_map(&server).await, "tile_render_queue_depth");
    assert!(
        depth_mid >= 1,
        "queue must still be saturated, depth {depth_mid}"
    );

    // Placements during saturation stay at baseline scale — they never
    // wait behind a render (a queued-behind placement would cost ≥ 5s).
    for i in 0..5i64 {
        let t0 = Instant::now();
        let resp = place(
            &server,
            &placer,
            &img,
            base + 128 + i * 16,
            base + 128 + i * 16,
            64,
        )
        .await;
        assert_eq!(resp.status, 201);
        let took = t0.elapsed();
        assert!(
            took < Duration::from_millis(1500),
            "placement during saturation took {took:?} (baseline max {baseline_max:?})"
        );
    }
    assert!(
        counter(&metrics_map(&server).await, "tile_render_queue_depth") >= 1,
        "queue still has backlog after the placements"
    );
}

/// Render-storm flood: 1000 unique cold tiles against a full queue — every
/// cold request is shed with 202 (no queue growth, no inline render
/// stampede) while tiles WITH stored bytes keep being served stale.
#[tokio::test(flavor = "multi_thread")]
async fn cold_flood_sheds_202_while_hot_stays_stale() {
    let _serial = set_render_pause_ms(0).await;
    // quiet/max_delay huge: nothing ever fires, the queue stays at cap.
    let server = start_server_full(
        false,
        300,
        1,
        10 * 1024 * 1024,
        TileRenderCfg {
            quiet_secs: 3600,
            max_delay_secs: 3600,
            cap: 64,
        },
    )
    .await;
    let placer = make_placer(&server, "flooder").await;
    let base = 1 << 22;
    let img = png(48, 48);

    // 36 spots on a 32768px grid (each its own z16 tile).
    let spots: Vec<(i64, i64)> = (0..6i64)
        .flat_map(|i| (0..6i64).map(move |j| (base + i * 32768, base + j * 32768)))
        .collect();

    // Phase 1: place everything (no tile rows yet).
    for (x, y) in &spots {
        assert_eq!(place(&server, &placer, &img, *x, *y, 48).await.status, 201);
    }
    // Phase 2: row-creating fresh fetches — 36 z16 tiles + 28 z17 tiles
    // (each 48px spot touches exactly one tile per zoom).
    let z16 = |x: i64, y: i64| format!("/api/tiles/16/{}/{}.webp", x / 4096, y / 4096);
    let z17 = |x: i64, y: i64| format!("/api/tiles/17/{}/{}.webp", x / 2048, y / 2048);
    for (x, y) in &spots {
        let r = request(&server, "GET", &z16(*x, *y), &[], vec![]).await;
        assert_eq!(r.headers.get("x-tile-stale"), None);
    }
    for (x, y) in spots.iter().take(28) {
        let r = request(&server, "GET", &z17(*x, *y), &[], vec![]).await;
        assert_eq!(r.headers.get("x-tile-stale"), None);
    }
    // Phase 3: overlapping placements bump every touched tile.
    for (x, y) in &spots {
        assert_eq!(
            place(&server, &placer, &img, *x + 8, *y + 8, 48)
                .await
                .status,
            201
        );
    }
    // Phase 4: stale fetches enqueue — exactly the cap (64) entries.
    for (x, y) in &spots {
        let r = request(&server, "GET", &z16(*x, *y), &[], vec![]).await;
        assert_eq!(r.headers["x-tile-stale"], "true");
    }
    for (x, y) in spots.iter().take(28) {
        let r = request(&server, "GET", &z17(*x, *y), &[], vec![]).await;
        assert_eq!(r.headers["x-tile-stale"], "true");
    }
    assert_eq!(
        counter(&metrics_map(&server).await, "tile_render_queue_depth"),
        64,
        "queue pinned at cap"
    );

    // The flood: 1000 unique COLD tiles (art present ⇒ seq > 0; never
    // fetched ⇒ no stored bytes). One far-away art block: four 4096px
    // placements forming an 8192px square, aligned, gives 32×32 z20 art
    // tiles plus every parent zoom ≈ 1367 unique coords.
    let bx = 1 << 23;
    for (dx, dy) in [(0i64, 0i64), (4096, 0), (0, 4096), (4096, 4096)] {
        assert_eq!(
            place(&server, &placer, &img, bx + dx, bx + dy, 4096)
                .await
                .status,
            201
        );
    }
    let mut cold: Vec<String> = Vec::new();
    for z in 13..=20i64 {
        let side = 256 * (1 << (20 - z));
        let n = 8192 / side;
        for tx in (bx / side)..(bx / side + n) {
            for ty in (bx / side)..(bx / side + n) {
                cold.push(format!("/api/tiles/{z}/{tx}/{ty}.webp"));
            }
        }
    }
    assert!(
        cold.len() >= 1000,
        "need 1000 unique cold coords, got {}",
        cold.len()
    );
    cold.truncate(1000);

    let t_flood = Instant::now();
    let mut shed = 0;
    let mut other = Vec::new();
    for path in &cold {
        let (status, _, _) = burst_get(server.addr, path).await;
        if status == 202 {
            shed += 1;
        } else {
            other.push((path.clone(), status));
        }
    }
    let flood_secs = t_flood.elapsed().as_secs_f64();
    assert!(
        other.is_empty(),
        "expected all cold GETs shed with 202, got {} non-202: {other:?}",
        other.len()
    );
    assert_eq!(shed, 1000, "flood of 1000 completed in {flood_secs:.2}s");

    // Bounded: depth never exceeded the cap, and the process still serves.
    let depth = counter(&metrics_map(&server).await, "tile_render_queue_depth");
    assert!(depth <= 64, "queue exceeded cap: {depth}");
    assert_eq!(
        request(&server, "GET", "/api/health", &[], vec![])
            .await
            .status,
        200
    );

    // Hot tile (stored bytes, queued): still served stale, instantly.
    let t0 = Instant::now();
    let hot = request(&server, "GET", &z16(spots[0].0, spots[0].1), &[], vec![]).await;
    let hot_latency = t0.elapsed();
    assert_eq!(hot.status, 200);
    assert_eq!(hot.headers["x-tile-stale"], "true");
    assert!(
        hot_latency < Duration::from_secs(2),
        "hot stale serve took {hot_latency:?}"
    );
}
