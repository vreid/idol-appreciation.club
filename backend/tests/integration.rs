//! Full-flow integration tests against a real Postgres (testcontainers)
//! and a filesystem object store. Skipped (with a loud message) when
//! Docker is unavailable. Harness lives in `tests/common`.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::*;
use idol_appreciation_club_backend::build_app;
use idol_appreciation_club_backend::config::Config;
use idol_appreciation_club_backend::img;
use testcontainers_modules::postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::ImageExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn auth_flows() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    // First account is admin.
    let c1 = register(&server, "alpha").await;
    let me = request(&server, "GET", "/api/auth/me", &[("Cookie", &c1)], vec![]).await;
    assert_eq!(me.status, 200);
    let me = me.json();
    assert_eq!(me["username"], "alpha");
    assert_eq!(me["is_admin"], true);
    assert_eq!(me["cooldown_until"], serde_json::Value::Null);

    // Second account is not admin.
    let c2 = register(&server, "beta").await;
    let me2 = request(&server, "GET", "/api/auth/me", &[("Cookie", &c2)], vec![]).await;
    assert_eq!(me2.json()["is_admin"], false);

    // Bad login → 401; good login → cookie.
    let bad = request(
        &server,
        "POST",
        "/api/auth/login",
        &[("Content-Type", "application/json")],
        br#"{"username":"alpha","password":"wrong-password"}"#.to_vec(),
    )
    .await;
    assert_eq!(bad.status, 401);
    let good = request(
        &server,
        "POST",
        "/api/auth/login",
        &[("Content-Type", "application/json")],
        br#"{"username":"beta","password":"hunter2hunter2"}"#.to_vec(),
    )
    .await;
    assert_eq!(good.status, 200);
    assert!(good.cookie().is_some());

    // Username rules.
    let resp = request(
        &server,
        "POST",
        "/api/auth/register",
        &[("Content-Type", "application/json")],
        br#"{"username":"NO","password":"hunter2hunter2"}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 400);

    // Logout clears the cookie (stateless sessions: the server cannot
    // revoke a presented token, so we assert the clear directive).
    let out = request(
        &server,
        "POST",
        "/api/auth/logout",
        &[("Cookie", &c2)],
        vec![],
    )
    .await;
    assert_eq!(out.status, 204);
    let cleared = out.headers.get("set-cookie").expect("clearing cookie");
    assert!(cleared.contains("Max-Age=0"));
    assert!(cleared.contains("session="));

    // Unauthenticated authed routes → 401.
    let resp = request(&server, "GET", "/api/auth/me", &[], vec![]).await;
    assert_eq!(resp.status, 401);
}

#[tokio::test]
async fn place_validation() {
    let server = start_server(300, 1, 1024).await; // 1KB upload cap
    let img = png(32, 32);
    let cookie = register(&server, "validator").await;

    // Unauthenticated → 401.
    let (b, body) = place_body(&img, 0, 0, 16);
    let resp = request(
        &server,
        "POST",
        "/api/place",
        &[(
            "Content-Type",
            &format!("multipart/form-data; boundary={b}"),
        )],
        body,
    )
    .await;
    assert_eq!(resp.status, 401);

    // Oversize upload → 413.
    let big = vec![0u8; 4096];
    let resp = place(&server, &cookie, &big, 0, 0, 16).await;
    assert_eq!(resp.status, 413);

    let server2 = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server2, "validator2").await;
    // Size below minimum → 400.
    let resp = place(&server2, &cookie, &img, 0, 0, 15).await;
    assert_eq!(resp.status, 400);
    // Out of bounds → 400.
    let resp = place(&server2, &cookie, &img, WORLD - 8, 0, 16).await;
    assert_eq!(resp.status, 400);
    let resp = place(&server2, &cookie, &img, -1, 0, 16).await;
    assert_eq!(resp.status, 400);
    // Non-image bytes → 400.
    let resp = place(&server2, &cookie, b"certainly not an image", 0, 0, 16).await;
    assert_eq!(resp.status, 400);
    // Invalid tiles → 404.
    assert_eq!(
        request(&server2, "GET", "/api/tiles/21/0/0.webp", &[], vec![])
            .await
            .status,
        404
    );
    assert_eq!(
        request(&server2, "GET", "/api/tiles/2/4/0.webp", &[], vec![])
            .await
            .status,
        404
    );
    // Health OK.
    assert_eq!(
        request(&server2, "GET", "/api/health", &[], vec![])
            .await
            .status,
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn place_and_cooldowns() {
    let server = start_server(2, 1, 10 * 1024 * 1024).await;
    let img = png(32, 32);
    let cookie = register(&server, "painter").await;
    let x = 1 << 20; // 1048576, 256- and 1024-aligned

    let first = place(&server, &cookie, &img, x, x, 32).await;
    assert_eq!(
        first.status,
        201,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    let first_json = first.json();
    assert_eq!(first_json["w"], 32);
    assert_eq!(first_json["h"], 32);
    assert_eq!(first_json["seq"], 1);
    assert_eq!(first_json["duplicate_count"], 0);

    // Overlap within the 2s paint cooldown → 429 + Retry-After.
    let second = place(&server, &cookie, &img, x + 8, x + 8, 32).await;
    assert_eq!(
        second.status,
        429,
        "BODY {}",
        String::from_utf8_lossy(&second.body)
    );
    assert!(second.headers.contains_key("retry-after"));

    // Shared edge (zero-area overlap) is NOT a paint: empty-area 1s rule
    // applies instead. Fixed sleeps are load-fragile under the full
    // parallel suite, so poll: 429 while the empty cooldown runs, 201
    // once it has expired.
    let place_until = |x: i64, y: i64| {
        let server = &server;
        let cookie = &cookie;
        let img = &img;
        async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            loop {
                let resp = place(server, cookie, img, x, y, 32).await;
                if resp.status == 201 || tokio::time::Instant::now() > deadline {
                    break resp;
                }
                assert_eq!(resp.status, 429, "{}", String::from_utf8_lossy(&resp.body));
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let edge = place_until(x + 32, x).await;
    assert_eq!(edge.status, 201, "{}", String::from_utf8_lossy(&edge.body));

    // Paint cooldown expires → overlap accepted.
    let overlap = place_until(x + 8, x + 8).await;
    assert_eq!(
        overlap.status,
        201,
        "{}",
        String::from_utf8_lossy(&overlap.body)
    );

    // Same image again (empty area → 1s rule) → duplicate_count increments,
    // single original stored.
    let again = place_until(x + 64, x).await;
    assert_eq!(
        again.status,
        201,
        "{}",
        String::from_utf8_lossy(&again.body)
    );
    assert_eq!(again.json()["duplicate_count"], 3);

    let sha = {
        let stripped = img::strip_resave(
            &img::decode(&png(32, 32), img::DEFAULT_MAX_DECODE_PIXELS)
                .unwrap()
                .0,
            img::SourceFormat::Png,
        )
        .unwrap();
        img::sha256_hex(&stripped)
    };
    let count = request(
        &server,
        "GET",
        &format!("/api/images/{sha}/count"),
        &[],
        vec![],
    )
    .await;
    assert_eq!(count.status, 200);
    assert_eq!(count.json()["count"], 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn tiles_etag_staleness() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let c1 = register(&server, "artist1").await;
    let c2 = register(&server, "artist2").await;
    let x = 1 << 20;
    let tile20 = format!("/api/tiles/20/{}/{}.webp", x / 256, x / 256);
    let tile18 = format!("/api/tiles/18/{}/{}.webp", x / 1024, x / 1024);
    let other = format!("/api/tiles/20/{}/{}.webp", (2 << 20) / 256, (2 << 20) / 256);

    // Empty world: z20 tile renders on paper with ETag "0".
    let empty = request(&server, "GET", &tile20, &[], vec![]).await;
    assert_eq!(empty.status, 200);
    assert_eq!(empty.headers["content-type"], "image/webp");
    assert_eq!(empty.headers["etag"], "\"0\"");
    assert!(!empty.body.is_empty());

    // z0 renders through the full pyramid even with no art.
    let z0 = request(&server, "GET", "/api/tiles/0/0/0.webp", &[], vec![]).await;
    assert_eq!(z0.status, 200);
    assert_eq!(z0.headers["etag"], "\"0\"");

    let first = place(&server, &c1, &png(64, 64), x, x, 64).await;
    assert_eq!(first.status, 201);

    let t1 = request(&server, "GET", &tile20, &[], vec![]).await;
    assert_eq!(t1.headers["etag"], "\"1\"");
    assert_ne!(t1.body, empty.body, "tile bytes changed after placement");
    // Parent tile (z18) is fresh too.
    assert_eq!(
        request(&server, "GET", &tile18, &[], vec![]).await.headers["etag"],
        "\"1\""
    );

    // Conditional GET → 304.
    let not_modified = request(
        &server,
        "GET",
        &tile20,
        &[("If-None-Match", "\"1\"")],
        vec![],
    )
    .await;
    assert_eq!(not_modified.status, 304);

    // Unrelated tile keeps ETag "0".
    let untouched = request(&server, "GET", &other, &[], vec![]).await;
    assert_eq!(untouched.headers["etag"], "\"0\"");
    let still = request(
        &server,
        "GET",
        &other,
        &[("If-None-Match", "\"0\"")],
        vec![],
    )
    .await;
    assert_eq!(still.status, 304);

    // Overlapping placement → the stored tile (seq 1) serves STALE
    // immediately with the stored ETag + marker, and one background
    // re-render converges the tile to seq 2.
    let second = place(&server, &c2, &png(96, 96), x + 16, x + 16, 96).await;
    assert_eq!(second.status, 201);
    let t2_stale = request(&server, "GET", &tile20, &[], vec![]).await;
    assert_eq!(t2_stale.status, 200);
    assert_eq!(t2_stale.headers["etag"], "\"1\"");
    assert_eq!(t2_stale.headers["x-tile-stale"], "true");
    assert_eq!(t2_stale.body, t1.body, "stale bytes are the stored ones");
    // A conditional GET in the stale window still 304s against the stored
    // ETag (bytes match — no waste), and the re-render is queued.
    let still_cached = request(
        &server,
        "GET",
        &tile20,
        &[("If-None-Match", "\"1\"")],
        vec![],
    )
    .await;
    assert_eq!(still_cached.status, 304);
    let t2 = tile_fresh(&server, &tile20, "\"2\"", Duration::from_secs(5)).await;
    assert_ne!(t2.body, t1.body);
    assert_eq!(t2.headers.get("x-tile-stale"), None);
    // Old ETag no longer 304s.
    assert_eq!(
        request(
            &server,
            "GET",
            &tile20,
            &[("If-None-Match", "\"1\"")],
            vec![]
        )
        .await
        .status,
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn near_center_reports() {
    let server = start_server(1, 1, 10 * 1024 * 1024).await;
    let c1 = register(&server, "holder1").await;
    let c2 = register(&server, "holder2").await;
    let cx = CENTER - 8;

    let p1 = place(&server, &c1, &png(32, 32), cx, cx, 32).await;
    assert_eq!(p1.status, 201);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let p2 = place(&server, &c2, &png(32, 32), cx + 4, cx + 4, 32).await;
    assert_eq!(p2.status, 201);

    // near: newest first, bbox intersection.
    let near = request(
        &server,
        "GET",
        &format!("/api/placements/near?x={CENTER}&y={CENTER}&r=1024"),
        &[],
        vec![],
    )
    .await;
    assert_eq!(near.status, 200);
    let list = near.json();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["username"], "holder2");
    assert_eq!(arr[1]["username"], "holder1");
    // Contract: every row carries its global seq as a positive integer,
    // descending (newest first).
    let seqs: Vec<i64> = arr
        .iter()
        .map(|row| {
            let s = row["seq"].as_i64().expect("seq is an integer");
            assert!(s > 0, "seq must be positive, got {s}");
            s
        })
        .collect();
    assert_eq!(seqs.len(), 2);
    assert!(
        seqs[0] > seqs[1],
        "seq must be monotonic newest-first: {seqs:?}"
    );
    // r capped at 65536; garbage → 400.
    assert_eq!(
        request(
            &server,
            "GET",
            "/api/placements/near?x=0&y=0&r=bogus",
            &[],
            vec![]
        )
        .await
        .status,
        400
    );

    // center: current = holder2, all_time includes holder1's segment.
    let center = request(&server, "GET", "/api/center", &[], vec![]).await;
    let c = center.json();
    assert_eq!(c["current"]["username"], "holder2");
    assert_eq!(c["current"]["placement"]["w"], 32);
    let all = c["all_time"].as_array().unwrap();
    assert!(all.iter().any(|u| u["username"] == "holder1"));
    assert!(all.iter().any(|u| u["username"] == "holder2"));

    // report + image count.
    let pid = p2.json()["id"].as_str().unwrap().to_owned();
    let report = request(
        &server,
        "POST",
        "/api/reports",
        &[("Cookie", &c1), ("Content-Type", "application/json")],
        format!(r#"{{"placement_id":"{pid}"}}"#).into_bytes(),
    )
    .await;
    assert_eq!(report.status, 204);
    let report_unknown = request(
        &server,
        "POST",
        "/api/reports",
        &[("Cookie", &c1), ("Content-Type", "application/json")],
        br#"{"placement_id":"0197ffff-ffff-7fff-ffff-ffffffffffff"}"#.to_vec(),
    )
    .await;
    assert_eq!(report_unknown.status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_removal_updates_everything() {
    let server = start_server(1, 1, 10 * 1024 * 1024).await;
    let admin = register(&server, "root").await;
    let user = register(&server, "pleb").await;
    let cx = CENTER - 8;

    place(&server, &admin, &png(32, 32), cx, cx, 32).await;
    let p2 = place(&server, &user, &png(48, 48), cx + 4, cx + 4, 48).await;
    assert_eq!(p2.status, 201);
    let pid = p2.json()["id"].as_str().unwrap().to_owned();

    // Non-admin → 403.
    let forbidden = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{pid}"),
        &[("Cookie", &user)],
        vec![],
    )
    .await;
    assert_eq!(forbidden.status, 403);

    // Subscribe to SSE BEFORE removal.
    let mut sse = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    sse.write_all(b"GET /api/events HTTP/1.1\r\nHost: test\r\nAccept: text/event-stream\r\n\r\n")
        .await
        .unwrap();

    let del = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{pid}"),
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(del.status, 204);
    // Removing again → 404.
    let del2 = request(
        &server,
        "DELETE",
        &format!("/api/admin/placements/{pid}"),
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(del2.status, 404);

    // near no longer lists it; center falls back to the admin's placement
    // (manifest replay semantics).
    let near = request(
        &server,
        "GET",
        &format!("/api/placements/near?x={CENTER}&y={CENTER}&r=1024"),
        &[],
        vec![],
    )
    .await;
    let arr = near.json().as_array().unwrap().clone();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["username"], "root");
    let center = request(&server, "GET", "/api/center", &[], vec![])
        .await
        .json();
    assert_eq!(center["current"]["username"], "root");

    // Tile freshness: the removed placement's chunk-region tile drops back
    // (first GET serves stored stale bytes and queues the re-render).
    let tile20 = format!("/api/tiles/20/{}/{}.webp", CENTER / 256, CENTER / 256);
    let tile = tile_fresh(&server, &tile20, "\"1\"", Duration::from_secs(5)).await; // only the admin's placement remains
    assert_eq!(tile.status, 200);

    // SSE delivered the remove event.
    let mut buf = vec![0u8; 8192];
    let n = tokio::time::timeout(Duration::from_secs(10), sse.read(&mut buf))
        .await
        .expect("sse event within 10s")
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(text.contains("\"type\":\"remove\""), "got: {text}");
    assert!(text.contains(pid.to_string().as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_place_event() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "streamer").await;

    let mut sse = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    sse.write_all(b"GET /api/events HTTP/1.1\r\nHost: test\r\nAccept: text/event-stream\r\n\r\n")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    let x = 3 << 20;
    let placed = place(&server, &cookie, &png(32, 32), x, x, 32).await;
    assert_eq!(placed.status, 201);
    let seq = placed.json()["seq"].as_i64().unwrap();

    let mut buf = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no SSE event in time"
        );
        let mut chunk = [0u8; 4096];
        let n = tokio::time::timeout(Duration::from_secs(9), sse.read(&mut chunk))
            .await
            .expect("sse read")
            .unwrap();
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if text.contains("\"type\":\"place\"") {
            assert!(text.contains(&format!("\"seq\":{seq}")));
            assert!(text.contains(&format!("\"bbox\":[{x},{x},32,32]")));
            break;
        }
    }
}

#[tokio::test]
async fn static_fallback_is_graceful() {
    let server = start_server(300, 1, 1024).await;
    let root = request(&server, "GET", "/", &[], vec![]).await;
    assert_eq!(root.status, 200);
    let v = root.json();
    assert_eq!(v["service"], "idol-appreciation.club");
    assert_eq!(v["static"], false);
    let api404 = request(&server, "GET", "/api/nope", &[], vec![]).await;
    assert_eq!(api404.status, 404);
    assert_eq!(api404.json()["error"], "not found");
}

// -------------------------------------------------------------------------
// Admin-approved registration.
// -------------------------------------------------------------------------

async fn raw_register(server: &Server, username: &str) -> Resp {
    request(
        server,
        "POST",
        "/api/auth/register",
        &[("Content-Type", "application/json")],
        format!(r#"{{"username":"{username}","password":"hunter2hunter2"}}"#).into_bytes(),
    )
    .await
}

async fn raw_login(server: &Server, username: &str) -> Resp {
    request(
        server,
        "POST",
        "/api/auth/login",
        &[("Content-Type", "application/json")],
        format!(r#"{{"username":"{username}","password":"hunter2hunter2"}}"#).into_bytes(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_gate_full_flow() {
    let server = start_server_gated(true, 0, 1, 10 * 1024 * 1024).await;

    // Bootstrap admin: always approved, gets a session.
    let admin = register(&server, "root").await;
    assert!(admin.starts_with("session="));

    // External signup: pending, no session cookie issued.
    let resp = raw_register(&server, "newbie").await;
    assert_eq!(resp.status, 201);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["pending"], true);
    assert_eq!(body["username"], "newbie");
    assert!(resp.cookie().is_none(), "pending accounts get no session");

    // Queue shows the pending account oldest-first; admin itself absent.
    let queue = request(
        &server,
        "GET",
        "/api/admin/accounts?pending=true",
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(queue.status, 200);
    let q: serde_json::Value = serde_json::from_slice(&queue.body).unwrap();
    let names: Vec<&str> = q
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["newbie"]);

    // Login before approval → 403 pending, no session.
    let resp = raw_login(&server, "newbie").await;
    assert_eq!(resp.status, 403);
    assert!(String::from_utf8_lossy(&resp.body).contains("account pending approval"));
    assert!(resp.cookie().is_none());

    // Non-admin approve → 403. (First approve newbie via admin so the
    // non-admin can hold a session at all.)
    let appr = request(
        &server,
        "POST",
        "/api/admin/accounts/newbie/approve",
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(appr.status, 204);

    // Idempotent: approving again is still 204.
    let appr = request(
        &server,
        "POST",
        "/api/admin/accounts/newbie/approve",
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(appr.status, 204);

    // Unknown username → 404.
    let appr = request(
        &server,
        "POST",
        "/api/admin/accounts/ghost/approve",
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    assert_eq!(appr.status, 404);

    // Approved: login works, and the account can place art.
    let resp = raw_login(&server, "newbie").await;
    assert_eq!(resp.status, 200);
    let cookie = resp.cookie().expect("approved login sets session");
    assert!(cookie.starts_with("session="));
    let place = request(
        &server,
        "POST",
        "/api/place",
        &[
            ("Cookie", &cookie),
            ("Content-Type", "multipart/form-data; boundary=X"),
        ],
        multipart("X", &[("x", "0"), ("y", "0"), ("size", "16")], &png(16, 16)),
    )
    .await;
    assert_eq!(
        place.status,
        201,
        "{}",
        String::from_utf8_lossy(&place.body)
    );

    // Second pending signup; the approved NON-admin cannot approve it.
    let resp = raw_register(&server, "second").await;
    assert_eq!(resp.status, 201);
    let forbidden = request(
        &server,
        "POST",
        "/api/admin/accounts/second/approve",
        &[("Cookie", &cookie)],
        vec![],
    )
    .await;
    assert_eq!(forbidden.status, 403);
    // ...and the second account still cannot log in.
    let resp = raw_login(&server, "second").await;
    assert_eq!(resp.status, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_flag_off_auto_approves() {
    let server = start_server(0, 1, 10 * 1024 * 1024).await;
    let resp = raw_register(&server, "openc").await;
    assert_eq!(resp.status, 201);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["pending"], false);
    assert!(resp.cookie().is_some(), "auto-approved register logs in");
    let resp = raw_login(&server, "openc").await;
    assert_eq!(resp.status, 200);
}

// -------------------------------------------------------------------------
// Cooldown exemption.
// -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cooldown_exempt_skips_both_cooldowns() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let admin = register(&server, "root").await;
    let c1 = register(&server, "speedster").await;
    let cx = CENTER - 8;

    // Overlapping paints normally 429 immediately.
    let p1 = place(&server, &c1, &png(32, 32), cx, cx, 32).await;
    assert_eq!(p1.status, 201);
    let p2 = place(&server, &c1, &png(32, 32), cx + 4, cx + 4, 32).await;
    assert_eq!(p2.status, 429);

    // Exempt → the same overlapping placement succeeds immediately.
    let on = request(
        &server,
        "POST",
        "/api/admin/accounts/speedster/cooldown-exempt",
        &[("Cookie", &admin), ("Content-Type", "application/json")],
        br#"{"exempt":true}"#.to_vec(),
    )
    .await;
    assert_eq!(on.status, 204);
    let p3 = place(&server, &c1, &png(32, 32), cx + 8, cx + 8, 32).await;
    assert_eq!(p3.status, 201, "{}", String::from_utf8_lossy(&p3.body));

    // The list surfaces the flag.
    let list = request(
        &server,
        "GET",
        "/api/admin/accounts",
        &[("Cookie", &admin)],
        vec![],
    )
    .await;
    let l = list.json();
    let row = l
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["username"] == "speedster")
        .unwrap();
    assert_eq!(row["cooldown_exempt"], true);

    // Toggle off → cooldowns reapply (overlap again → 429).
    let off = request(
        &server,
        "POST",
        "/api/admin/accounts/speedster/cooldown-exempt",
        &[("Cookie", &admin), ("Content-Type", "application/json")],
        br#"{"exempt":false}"#.to_vec(),
    )
    .await;
    assert_eq!(off.status, 204);
    let p4 = place(&server, &c1, &png(32, 32), cx + 12, cx + 12, 32).await;
    assert_eq!(p4.status, 429);

    // Idempotent 204, 404 unknown, 403 non-admin.
    let off = request(
        &server,
        "POST",
        "/api/admin/accounts/speedster/cooldown-exempt",
        &[("Cookie", &admin), ("Content-Type", "application/json")],
        br#"{"exempt":false}"#.to_vec(),
    )
    .await;
    assert_eq!(off.status, 204);
    let unknown = request(
        &server,
        "POST",
        "/api/admin/accounts/ghost/cooldown-exempt",
        &[("Cookie", &admin), ("Content-Type", "application/json")],
        br#"{"exempt":true}"#.to_vec(),
    )
    .await;
    assert_eq!(unknown.status, 404);
    let forbidden = request(
        &server,
        "POST",
        "/api/admin/accounts/root/cooldown-exempt",
        &[("Cookie", &c1), ("Content-Type", "application/json")],
        br#"{"exempt":true}"#.to_vec(),
    )
    .await;
    assert_eq!(forbidden.status, 403);
}

// -------------------------------------------------------------------------
// Rotation (free-angle placements, AABB bbox, rotated hit-test).
// -------------------------------------------------------------------------

fn place_rot_body(image: &[u8], x: i64, y: i64, size: i64, rot: Option<f32>) -> (String, Vec<u8>) {
    let boundary = format!("b{}", uuid::Uuid::now_v7().simple());
    let mut fields: Vec<(String, String)> = vec![
        ("x".into(), x.to_string()),
        ("y".into(), y.to_string()),
        ("size".into(), size.to_string()),
    ];
    if let Some(rot) = rot {
        fields.push(("rot".into(), rot.to_string()));
    }
    let fields: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let body = multipart(&boundary, &fields, image);
    (boundary, body)
}

async fn place_rot(
    server: &Server,
    cookie: &str,
    image: &[u8],
    x: i64,
    y: i64,
    size: i64,
    rot: Option<f32>,
) -> Resp {
    let (boundary, body) = place_rot_body(image, x, y, size, rot);
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

#[tokio::test]
async fn rotation_echo_near_and_normalization() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "spinner").await;
    let img = png(64, 64);

    // rot=405 normalizes to 45; omitted rot defaults to 0. (The 45° AABB
    // of a 64px image pokes ~14px past the rect corner, so keep clear of
    // the world edge.)
    let resp = place_rot(&server, &cookie, &img, 1000, 1000, 64, Some(405.0)).await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));
    let placed = resp.json();
    assert_eq!(placed["rot"], 45.0);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let resp = place_rot(&server, &cookie, &img, 1500, 1000, 64, None).await;
    assert_eq!(resp.status, 201);
    assert_eq!(resp.json()["rot"], 0.0);

    // near carries rot; the rotated placement is discoverable via its AABB.
    let near = request(
        &server,
        "GET",
        "/api/placements/near?x=1250&y=1125&r=1024",
        &[],
        vec![],
    )
    .await;
    let rows = near.json();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["rot"], 0.0); // newest first
    assert_eq!(rows[1]["rot"], 45.0);

    // NaN-ish garbage → 400 (fields precede the image part, so the parse
    // rejects before the upload matters).
    let boundary = format!("b{}", uuid::Uuid::now_v7().simple());
    let body = multipart(
        &boundary,
        &[("x", "0"), ("y", "0"), ("size", "64"), ("rot", "nan")],
        b"",
    );
    let resp = request(
        &server,
        "POST",
        "/api/place",
        &[
            ("Cookie", &cookie),
            (
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            ),
        ],
        body,
    )
    .await;
    assert_eq!(resp.status, 400);
}

#[tokio::test]
async fn rotated_center_hit_and_aabb_miss() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let holder = register(&server, "holder").await; // admin
    let other = register(&server, "corner").await;
    let img = png(100, 100);

    // Holder covers 0/0 exactly.
    let cx: i64 = CENTER;
    let cy: i64 = CENTER;
    let resp = place_rot(&server, &holder, &img, cx - 50, cy - 50, 100, Some(0.0)).await;
    assert_eq!(resp.status, 201);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    // A 45° rect whose AABB contains 0/0 but whose rotated body misses:
    // 0/0 sits 45px diagonal from the rect center — outside the 45° body
    // (local |rx| = 63.6 ≥ 50) while well inside the ~71px AABB radius.
    let resp = place_rot(
        &server,
        &other,
        &img,
        cx - 50 + 45,
        cy - 50 + 45,
        100,
        Some(45.0),
    )
    .await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));

    let center = request(&server, "GET", "/api/center", &[], vec![])
        .await
        .json();
    assert_eq!(center["current"]["username"], "holder");
    assert_eq!(center["current"]["placement"]["rot"], 0.0);

    // A FRESH account paints immediately even on overlap (paint cooldown
    // keys off the account's own last placement).
    let third = register(&server, "coverer").await;
    // Now a rotated rect that DOES cover 0/0 (centered on it) takes over.
    let resp = place_rot(&server, &third, &img, cx - 64, cy - 64, 128, Some(45.0)).await;
    assert_eq!(resp.status, 201);
    let center = request(&server, "GET", "/api/center", &[], vec![])
        .await
        .json();
    assert_eq!(center["current"]["username"], "coverer");
    assert_eq!(center["current"]["placement"]["rot"], 45.0);
    // all_time aggregates only placements that actually cover 0/0:
    // holder (rot 0) and coverer (rot 45). corner's rect misses the
    // center by construction, so it must NOT be there.
    let names: Vec<&str> = center["all_time"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"holder"));
    assert!(names.contains(&"coverer"));
    assert!(!names.contains(&"corner"));
}

#[tokio::test]
async fn rotated_tiles_render_deterministically() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "tiler").await;
    // Asymmetric image: identical place+rot → identical tile bytes;
    // different rot → different bytes.
    let img = img::asymmetric_test_png(128, 64);

    let resp = place_rot(&server, &cookie, &img, 0, 0, 128, Some(0.0)).await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let resp = place_rot(&server, &cookie, &img, 0, 4096, 128, Some(0.0)).await;
    assert_eq!(resp.status, 201);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let resp = place_rot(&server, &cookie, &img, 0, 8192, 128, Some(90.0)).await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));

    async fn tile_at(server: &Server, y: i64) -> Vec<u8> {
        request(
            server,
            "GET",
            &format!("/api/tiles/20/0/{}.webp", y / 256),
            &[],
            vec![],
        )
        .await
        .body
    }
    let a = tile_at(&server, 0).await;
    let b = tile_at(&server, 4096).await;
    let c = tile_at(&server, 8192).await;
    assert!(!a.is_empty());
    assert_eq!(a, b, "same image+rot must render byte-identical tiles");
    assert_ne!(a, c, "rot must change the rendered pixels");
}

#[tokio::test]
async fn max_long_edge_is_4096() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "big").await;
    let img = png(64, 64);

    // 4096 accepted (long edge honored).
    let resp = place_rot(&server, &cookie, &img, 0, 0, 4096, None).await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(resp.json()["w"], 4096);
    assert_eq!(resp.json()["h"], 4096);

    // 4097 rejected.
    let resp = place_rot(&server, &cookie, &img, 0, 8192, 4097, None).await;
    assert_eq!(resp.status, 400);
}

// -------------------------------------------------------------------------
// Hardening: decoded-pixel budget and login throttling

/// A tiny PNG whose header declares 32768×32768 (1 Gpx, ~4 GiB of RGBA)
/// must be rejected 413 by the decoded-pixel budget BEFORE any pixel work:
/// the wire bytes are ~100 bytes, so only the header probe could have
/// made the call, and nothing materialized the pixels.
#[tokio::test]
async fn place_decode_budget_rejects_bomb() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "bomber").await;
    let bomb = png_with_declared_dims(32768, 32768);
    assert!(bomb.len() < 4096, "bomb fixture must be tiny on the wire");
    let resp = place_rot(&server, &cookie, &bomb, 0, 0, 64, None).await;
    assert_eq!(resp.status, 413, "{}", String::from_utf8_lossy(&resp.body));
}

/// The placement cooldown gate runs before the pixel stage: a second
/// placement inside the empty-space cooldown is a fast 429 + Retry-After
/// without spending the serialized libvips stage on a doomed decode.
#[tokio::test]
async fn place_cooldown_precheck_rejects_before_decode() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "rapid").await;
    let img = png(32, 32);
    let resp = place_rot(&server, &cookie, &img, 0, 0, 64, None).await;
    assert_eq!(resp.status, 201, "{}", String::from_utf8_lossy(&resp.body));
    // Far from the first placement: empty-space rules apply to both.
    let resp = place_rot(&server, &cookie, &img, 9000, 9000, 64, None).await;
    assert_eq!(resp.status, 429);
    assert!(resp.header("retry-after").is_some());
}

/// Login throttling: a burst of wrong passwords trips 429 + Retry-After
/// no matter what follows — correct password, other accounts, unknown
/// usernames all stay locked out of the empty IP bucket — and the
/// limiter counts every rejection in /api/metrics.
#[tokio::test]
async fn login_rate_limited_after_burst() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    register(&server, "victim").await;
    let login = |username: &str, password: &str| {
        let body = format!(r#"{{"username":"{username}","password":"{password}"}}"#).into_bytes();
        request(
            &server,
            "POST",
            "/api/auth/login",
            &[("Content-Type", "application/json")],
            body,
        )
    };
    let mut tripped = None;
    for i in 0..16 {
        let resp = login("victim", "definitely-wrong").await;
        if resp.status == 429 {
            tripped = Some(i);
            break;
        }
        assert_eq!(resp.status, 401, "attempt {i}");
        assert!(resp.header("retry-after").is_none());
    }
    let tripped = tripped.unwrap_or_else(|| panic!("no 429 within 16 attempts"));

    // Rejection is keyed on the buckets, not the credentials: nothing
    // bypasses an empty bucket, and nothing distinguishes the victim
    // account from an unknown one while locked out.
    for (username, password) in [
        ("victim", "hunter2hunter2"),     // correct credentials
        ("nobodyhere", "hunter2hunter2"), // unknown account
    ] {
        let resp = login(username, password).await;
        assert_eq!(resp.status, 429, "{username}");
        assert!(resp.header("retry-after").is_some());
    }

    let metrics = metrics_map(&server).await;
    let limited: u64 = metrics["auth_rate_limited_total"].parse().unwrap();
    assert!(limited >= 3, "auth_rate_limited_total {limited}");

    // A lucky guess inside the first burst must not be possible either:
    // the token spend happens before verification, so attempt
    // `tripped`'s neighbors were real 401s — i.e. the lockout arrives
    // strictly after CAPACITY real attempts, not after a grace period.
    assert!(tripped >= 1);
}

// Ignored by default: cold-pyramid render timing for the parallel-children
// change (`-- --ignored --nocapture`). Places one image near the center,
// then times the first GET of the z0 tile — the spine render resolves 4
// children per level through the cache/lock/s3 path.
#[tokio::test]
#[ignore = "timing probe: run explicitly to compare child fan-out latency"]
async fn z0_cold_render_timing() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let img = png(256, 256);
    // One placement per z1 quadrant, ~1M px from the center split: the z0
    // tile's four children are all non-empty, so the parent must resolve
    // four real spines. Sequential: sum of spines. Parallel: max of spines.
    let m = 1_000_000i64;
    let spots = [
        (CENTER - m - 128, CENTER - m - 128),
        (CENTER + m, CENTER - m - 128),
        (CENTER - m - 128, CENTER + m),
        (CENTER + m, CENTER + m),
    ];
    for (i, (x, y)) in spots.into_iter().enumerate() {
        let cookie = register(&server, &format!("timer{i}")).await;
        let resp = place_rot(&server, &cookie, &img, x, y, 256, Some(0.0)).await;
        assert_eq!(resp.status, 201);
    }
    let t0 = std::time::Instant::now();
    let resp = request(&server, "GET", "/api/tiles/0/0/0.webp", &[], vec![]).await;
    let elapsed = t0.elapsed();
    assert_eq!(resp.status, 200);
    assert!(!resp.body.is_empty());
    println!("cold z0 four-spine render: {elapsed:?}");
}

// -------------------------------------------------------------------------
// Cross-instance advisory locks (chunk raster read-modify-write)

use idol_appreciation_club_backend::chunks;
use idol_appreciation_club_backend::chunks::ChunkAdvisoryLocks;
use idol_appreciation_club_backend::store;
use idol_appreciation_club_backend::world;

/// Two "instances": separate pools over the same DB + shared object store
/// directory, compositing into the SAME chunk concurrently. The session-
/// level advisory locks must close the S3 read-modify-write window so no
/// composite is lost, across 30 interleaved rounds.
#[tokio::test]
async fn two_instance_composite_no_lost_update() {
    let container = postgres::Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container");
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let store_dir =
        std::env::temp_dir().join(format!("idol-advisory-{}", uuid::Uuid::now_v7().simple()));
    let mk_cfg = || Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        database_url: db_url.clone(),
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
        max_decode_pixels: idol_appreciation_club_backend::img::DEFAULT_MAX_DECODE_PIXELS,
        min_size: 16,
        max_size: 4096,
        tile_cache_entries: 256,
        tile_render_quiet_secs: 4,
        tile_render_max_delay_secs: 12,
        tile_render_queue_cap: 512,
        registration_approval_required: false,
    };
    let (app1, _) = build_app(mk_cfg()).await.expect("instance 1");
    let (app2, _) = build_app(mk_cfg()).await.expect("instance 2");

    let red = img::solid_png(256, 256, [220, 40, 40]);
    let blue = img::solid_png(256, 256, [40, 40, 220]);
    // Same chunk (0,0), disjoint probe points.
    let rect_red = crate_rect(64, 64, 256, 256);
    let rect_blue = crate_rect(600, 600, 256, 256);

    for _ in 0..30 {
        let a = chunks::composite_placement(&app1, rect_red, 0.0, red.clone());
        let b = chunks::composite_placement(&app2, rect_blue, 0.0, blue.clone());
        let (ra, rb) = tokio::join!(a, b);
        ra.expect("instance 1 composite");
        rb.expect("instance 2 composite");
    }

    // Both overlays must be present in the final raster.
    let chunk = store::get_bytes(&app1.store, &world::chunk_key(0, 0))
        .await
        .unwrap()
        .expect("chunk 0/0 exists");
    let probes = tokio::task::spawn_blocking(move || {
        let (img, _) = img::decode(&chunk, img::DEFAULT_MAX_DECODE_PIXELS).unwrap();
        (pixel_at(&img, 100, 100), pixel_at(&img, 700, 700))
    })
    .await
    .unwrap();
    // Chunk rasters re-encode lossy webp (Q90) on every composite, so 30
    // serialized rounds drift chroma — assert channel dominance, not exact
    // equality. A lost update would leave one overlay FULLY absent
    // (transparent), which dominance still catches.
    let (r, g, b) = (probes.0[0], probes.0[1], probes.0[2]);
    assert!(
        r > 140 && r > g + 40 && r > b + 40,
        "red overlay lost — update race: {probes:?}"
    );
    let (r, g, b) = (probes.1[0], probes.1[1], probes.1[2]);
    assert!(
        b > 140 && b > r + 40 && b > g + 40,
        "blue overlay lost — update race: {probes:?}"
    );
}

fn crate_rect(x: i64, y: i64, w: i64, h: i64) -> world::Rect {
    world::Rect { x, y, w, h }
}

fn pixel_at(img: &img::VipsImage, x: i32, y: i32) -> [u8; 3] {
    img::get_pixel(img, x, y)
}

/// Deadlock-ordering probe: two instances acquire the SAME two chunk locks
/// in opposite given orders. Sorted acquisition inside `acquire` must make
/// both take the lower index first; a regression back to given-order
/// acquisition deadlocks and trips the timeout.
#[tokio::test]
async fn advisory_lock_opposite_orders_no_deadlock() {
    let container = postgres::Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container");
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let store_dir = std::env::temp_dir().join(format!(
        "idol-advisory-deadlock-{}",
        uuid::Uuid::now_v7().simple()
    ));
    let mk_cfg = || Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        database_url: db_url.clone(),
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
        max_decode_pixels: idol_appreciation_club_backend::img::DEFAULT_MAX_DECODE_PIXELS,
        min_size: 16,
        max_size: 4096,
        tile_cache_entries: 256,
        tile_render_quiet_secs: 4,
        tile_render_max_delay_secs: 12,
        tile_render_queue_cap: 512,
        registration_approval_required: false,
    };
    let (app1, _) = build_app(mk_cfg()).await.expect("instance 1");
    let (app2, _) = build_app(mk_cfg()).await.expect("instance 2");

    let x = chunks::chunk_lock_objid(1, 0);
    let y = chunks::chunk_lock_objid(2, 0);
    // Instance A takes both locks (given order Y,X — stored sorted).
    let la = ChunkAdvisoryLocks::acquire(&app1.pool, vec![y, x])
        .await
        .expect("A acquire");
    // Instance B requests the same locks in the opposite given order while
    // A holds them: it must stay EXCLUDED (not interleave), then resolve
    // promptly once A releases — no deadlock in either phase.
    let pool2 = app2.pool.clone();
    let waiter = tokio::spawn(async move {
        ChunkAdvisoryLocks::acquire(&pool2, vec![x, y])
            .await
            .expect("B acquire")
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !waiter.is_finished(),
        "B acquired while A still holds — exclusion broken"
    );
    la.release().await;
    let lb = tokio::time::timeout(std::time::Duration::from_secs(10), waiter)
        .await
        .expect("B never resolved after A released — deadlock")
        .expect("B acquire");
    lb.release().await;

    // Re-acquiring the same locks must succeed immediately (proves the
    // release path actually let them go).
    let again = ChunkAdvisoryLocks::acquire(&app1.pool, vec![x, y]);
    tokio::time::timeout(std::time::Duration::from_secs(10), again)
        .await
        .expect("locks not released")
        .unwrap()
        .release()
        .await;
}

// Ignored perf probe (run with `-- --ignored --nocapture`): 200 placements
// against a fresh server, then the place_duration_seconds percentiles read
// back from /api/metrics. Run on both refs for the advisory-lock A/B.
#[tokio::test]
#[ignore = "perf probe: run explicitly for the advisory-lock A/B numbers"]
async fn place_load_metrics_200() {
    let server = start_server(300, 0, 10 * 1024 * 1024).await;
    let cookie = register(&server, "loader").await;
    let img = png(384, 384);
    // Straddle chunk boundaries every other placement (multi-chunk lock
    // cost) while keeping placements disjoint (empty-area pacing).
    for i in 0..200i64 {
        let x = (i % 20) * 1536;
        let y = (i / 20) * 1024 + 268_435_456 / 2 - 20 * 1024;
        let resp = place_rot(&server, &cookie, &img, x, y, 384, Some(0.0)).await;
        assert_eq!(resp.status, 201, "placement {i} failed");
    }
    let metrics = request(&server, "GET", "/api/metrics", &[], vec![])
        .await
        .body;
    let text = String::from_utf8(metrics).unwrap();
    let mut buckets: Vec<(String, u64)> = vec![];
    let mut count = 0u64;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("place_duration_seconds_bucket") {
            let le = rest
                .split("le=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("?")
                .to_string();
            let v: u64 = rest.rsplit(' ').next().unwrap().parse().unwrap();
            buckets.push((le, v));
        }
        if let Some(rest) = line
            .strip_prefix("place_duration_seconds_count")
            .map(|r| r.trim_start_matches("{}").trim())
        {
            count = rest.parse().unwrap();
        }
    }
    assert_eq!(count, 200, "histogram must have observed every placement");
    println!("place_duration_seconds cumulative buckets (n={count}):");
    for (le, v) in &buckets {
        println!("  le={le}: {v}");
    }
}
