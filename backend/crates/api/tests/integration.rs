//! Full-flow integration tests against a real Postgres (testcontainers)
//! and a filesystem object store. Skipped (with a loud message) when
//! Docker is unavailable. Harness lives in `tests/common`.

mod common;

use std::time::Duration;

use common::*;

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
    // 8s paint window: strict enough that the early 429 asserts are
    // deterministic, long enough to survive full-suite CPU load (a 2s
    // window could lapse while a 48 MPx decode runs next door).
    let server = start_server(8, 1, 10 * 1024 * 1024).await;
    let img = png(32, 32);
    let cookie = register(&server, "painter").await;
    let x = 1 << 20; // 1048576, 256- and 1024-aligned

    let first = place(&server, &cookie, &img, x, x, 32).await;
    assert_eq!(
        first.status,
        202,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    let first_json = first.json();
    assert_eq!(first_json["w"], 32);
    assert_eq!(first_json["h"], 32);
    assert_eq!(first_json["state"], "pending");
    assert_eq!(first_json["duplicate_count"], serde_json::Value::Null);

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
                if resp.status == 202 || tokio::time::Instant::now() > deadline {
                    break resp;
                }
                assert_eq!(resp.status, 429, "{}", String::from_utf8_lossy(&resp.body));
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let edge = place_until(x + 32, x).await;
    assert_eq!(edge.status, 202, "{}", String::from_utf8_lossy(&edge.body));

    // Paint cooldown expires → overlap accepted.
    let overlap = place_until(x + 8, x + 8).await;
    assert_eq!(
        overlap.status,
        202,
        "{}",
        String::from_utf8_lossy(&overlap.body)
    );

    // Same image again (empty area → 1s rule) queues too.
    let again = place_until(x + 64, x).await;
    assert_eq!(
        again.status,
        202,
        "{}",
        String::from_utf8_lossy(&again.body)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn near_center_reports() {
    let server = start_server(1, 1, 10 * 1024 * 1024).await;
    let c1 = register(&server, "holder1").await;
    // Registered only to own the second seeded row (the cookie is unused).
    let _c2 = register(&server, "holder2").await;
    let cx = CENTER - 8;

    let (_, seq1) = seed_placement(&server, "holder1", cx, cx, 32, 32, 0.0).await;
    let (p2u, seq2) = seed_placement(&server, "holder2", cx + 4, cx + 4, 32, 32, 0.0).await;
    let _ = seq1;
    let _ = seq2;

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
    let pid = p2u.to_string();
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
        202,
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
    assert_eq!(p1.status, 202);
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
    assert_eq!(p3.status, 202, "{}", String::from_utf8_lossy(&p3.body));

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
    assert_eq!(resp.status, 202, "{}", String::from_utf8_lossy(&resp.body));
    let placed = resp.json();
    assert_eq!(placed["rot"], 45.0);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let resp = place_rot(&server, &cookie, &img, 1500, 1000, 64, None).await;
    assert_eq!(resp.status, 202);
    assert_eq!(resp.json()["rot"], 0.0);

    // Rows for the read path (the API process has no ingest consumer).
    seed_placement(&server, "spinner", 1000, 1000, 64, 64, 45.0).await;
    seed_placement(&server, "spinner", 1500, 1000, 64, 64, 0.0).await;

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
    register(&server, "holder").await; // admin
    register(&server, "corner").await;
    register(&server, "coverer").await;
    let cx: i64 = CENTER;
    let cy: i64 = CENTER;

    // Holder covers 0/0 exactly.
    seed_placement(&server, "holder", cx - 50, cy - 50, 100, 100, 0.0).await;

    // A 45° rect whose AABB contains 0/0 but whose rotated body misses:
    // 0/0 sits 45px diagonal from the rect center — outside the 45° body
    // (local |rx| = 63.6 ≥ 50) while well inside the ~71px AABB radius.
    seed_placement(
        &server,
        "corner",
        cx - 50 + 45,
        cy - 50 + 45,
        100,
        100,
        45.0,
    )
    .await;

    let center = request(&server, "GET", "/api/center", &[], vec![])
        .await
        .json();
    assert_eq!(center["current"]["username"], "holder");
    assert_eq!(center["current"]["placement"]["rot"], 0.0);

    // A rotated rect that DOES cover 0/0 (centered on it) takes over as
    // the newest coverer.
    seed_placement(&server, "coverer", cx - 64, cy - 64, 128, 128, 45.0).await;
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
async fn max_long_edge_is_4096() {
    let server = start_server(300, 1, 10 * 1024 * 1024).await;
    let cookie = register(&server, "big").await;
    let img = png(64, 64);

    // 4096 accepted (long edge honored; the 202 echoes the rect).
    let resp = place_rot(&server, &cookie, &img, 0, 0, 4096, None).await;
    assert_eq!(resp.status, 202, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(resp.json()["w"], 4096);
    assert_eq!(resp.json()["h"], 4096);

    // 4097 rejected.
    let resp = place_rot(&server, &cookie, &img, 0, 8192, 4097, None).await;
    assert_eq!(resp.status, 400);
}

// -------------------------------------------------------------------------
// GET /api/dig — the wheel-dig stack

async fn dig(server: &Server, x: i64, y: i64) -> Resp {
    request(server, "GET", &format!("/api/dig?x={x}&y={y}"), &[], vec![]).await
}

/// Newest-first stack with 1-based depth: three overlapping placements at
/// a point dig as 3-of-3 with depth 1 = last placed; a rect whose AABB
/// covers the point but whose rotated pixels do not is excluded; removal
/// drops a stratum; out-of-world points are 400.
#[tokio::test]
async fn dig_returns_newest_first_stack() {
    let server = start_server(0, 0, 10 * 1024 * 1024).await;
    register(&server, "digger").await;

    let (_, first_seq) = seed_placement(&server, "digger", 1000, 1000, 32, 32, 0.0).await;
    let (_, second_seq) = seed_placement(&server, "digger", 1000, 1000, 32, 32, 0.0).await;
    // Same AABB (45° rotation of a 32x32 covers a superset region), but
    // the exact hit-test must decide: the point (1031, 1000) lies inside
    // the axis-aligned rects yet OUTSIDE both 0-rot rects (x .. x+32 ends
    // at 1031, half-open) — use a point all three truly cover instead.
    let (_, rotated_seq) = seed_placement(&server, "digger", 1000, 1000, 32, 32, 45.0).await;

    // (1016, 1016) is the center region: inside all three rects.
    let resp = dig(&server, 1016, 1016).await;
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    let stack = body["stack"].as_array().unwrap();
    assert_eq!(body["total"], 3);
    assert_eq!(body["truncated"], false);
    assert_eq!(stack.len(), 3);
    assert_eq!(stack[0]["depth"], 1);
    assert_eq!(stack[0]["seq"], rotated_seq, "topmost = newest");
    assert_eq!(stack[2]["depth"], 3);
    assert_eq!(stack[2]["seq"], first_seq, "bottom = oldest");
    let _ = second_seq;
    assert_eq!(stack[0]["username"], "digger");
    for entry in stack {
        assert!(entry["id"].is_string());
        assert!(entry["created_at"].is_string());
    }

    // AABB-covers-but-rotated-misses: (1000, 1031) is inside the 45°
    // rect's AABB corner but outside its rotated pixels; both unrotated
    // 32x32 rects still cover it, so exactly the two of them dig up.
    let corner = dig(&server, 1000, 1031).await;
    let corner_body: serde_json::Value = serde_json::from_slice(&corner.body).unwrap();
    assert_eq!(corner_body["total"], 2);

    // Off-world coordinates are rejected.
    assert_eq!(dig(&server, -1, 0).await.status, 400);
    assert_eq!(dig(&server, 0, 1 << 30).await.status, 400);
}

/// Removal drops the stratum; ETag follows the placement seq: matching
/// If-None-Match is a 304, a new placement changes it.
#[tokio::test]
async fn dig_etag_tracks_placement_seq() {
    let server = start_server(0, 0, 10 * 1024 * 1024).await;
    let _ = register(&server, "etagdig").await;
    let (first_id, _) = seed_placement(&server, "etagdig", 500, 500, 32, 32, 0.0).await;

    let resp = dig(&server, 516, 516).await;
    let etag = resp.header("etag").expect("etag").to_string();
    assert_eq!(resp.header("etag"), Some(etag.as_str()));
    let again = request(
        &server,
        "GET",
        "/api/dig?x=516&y=516",
        &[("If-None-Match", &etag)],
        vec![],
    )
    .await;
    assert_eq!(again.status, 304);

    // A new placement moves the global seq → fresh stack, new etag.
    let _ = seed_placement(&server, "etagdig", 516, 500, 32, 32, 0.0).await;
    let resp = request(
        &server,
        "GET",
        "/api/dig?x=516&y=516",
        &[("If-None-Match", &etag)],
        vec![],
    )
    .await;
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["total"], 2);

    // Removal drops the removed stratum from the stack (applied directly:
    // the API's DELETE only queues the removal for the worker).
    seed_remove(&server, first_id).await;
    let resp = dig(&server, 516, 516).await;
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(body["total"], 1);
}

// -------------------------------------------------------------------------
// Hardening: decoded-pixel budget and login throttling

/// An 8000x6000 (48 MPx) camera source keeps placing under the default
/// budget (64 MPx): the bomb cap must not reject real large photos.
#[tokio::test]
async fn place_admits_large_camera_sources() {
    let server = start_server(0, 0, 10 * 1024 * 1024).await;
    let cookie = register(&server, "photographer").await;
    // 48 MPx per the probed header, tiny on the wire (the header is what
    // the budget reads; the worker's decode re-verifies).
    let img = png_with_declared_dims(8000, 6000);
    let resp = place(&server, &cookie, &img, 0, 0, 4096).await;
    assert_eq!(resp.status, 202, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(resp.json()["w"], 4096);
}

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
    // 60s empty cooldown: the 429 is deterministic even under full-suite
    // load (a 1s window can lapse when a 48 MPx decode runs next door).
    let server = start_server(300, 60, 10 * 1024 * 1024).await;
    let cookie = register(&server, "rapid").await;
    let img = png(32, 32);
    let resp = place_rot(&server, &cookie, &img, 0, 0, 64, None).await;
    assert_eq!(resp.status, 202, "{}", String::from_utf8_lossy(&resp.body));
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

// -------------------------------------------------------------------------
// Placement tags: normalized label set, owner or admin editable

#[tokio::test]
async fn placement_tags_owner_admin_and_validation() {
    let server = start_server(0, 0, 10 * 1024 * 1024).await;
    // First account is admin; second owns the placement; third is a peer.
    let admin = register(&server, "tagadmin").await;
    let owner = register(&server, "tagowner").await;
    let other = register(&server, "tagpeer").await;
    let (placed_id, _) = seed_placement(&server, "tagowner", 500, 500, 32, 32, 0.0).await;
    let id = placed_id.to_string();
    let tags_url = format!("/api/placements/{id}/tags");

    // Unauthenticated edits are rejected.
    let resp = request(
        &server,
        "PUT",
        &tags_url,
        &[("Content-Type", "application/json")],
        br#"{"tags":["stage"]}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 401);

    // The owner's set is normalized: trim, lowercase, drop empties,
    // dedupe.
    let resp = request(
        &server,
        "PUT",
        &tags_url,
        &[("Cookie", &owner), ("Content-Type", "application/json")],
        br#"{"tags":[" Stage","stage","","ENCORE"]}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    let body = resp.json();
    assert_eq!(body["id"], id);
    let mut got = body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    got.sort();
    assert_eq!(got, vec!["encore".to_string(), "stage".to_string()]);

    // A peer who is neither owner nor admin is forbidden.
    let resp = request(
        &server,
        "PUT",
        &tags_url,
        &[("Cookie", &other), ("Content-Type", "application/json")],
        br#"{"tags":["hijack"]}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 403);

    // Validation: charset, per-tag length, and the 8-tag cap.
    for payload in [
        r#"{"tags":["no spaces here"]}"#,
        r#"{"tags":["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}"#,
        r#"{"tags":["a","b","c","d","e","f","g","h","i"]}"#,
    ] {
        let resp = request(
            &server,
            "PUT",
            &tags_url,
            &[("Cookie", &owner), ("Content-Type", "application/json")],
            payload.as_bytes().to_vec(),
        )
        .await;
        assert_eq!(
            resp.status,
            400,
            "{payload}: {}",
            String::from_utf8_lossy(&resp.body)
        );
    }

    // Reads carry the normalized set: near rows and dig strata alike.
    let near = request(
        &server,
        "GET",
        "/api/placements/near?x=516&y=516&r=1024",
        &[],
        vec![],
    )
    .await;
    assert_eq!(near.status, 200);
    let arr = near.json();
    let mine = arr
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id.as_str())
        .expect("placement row");
    assert_eq!(mine["tags"], serde_json::json!(["encore", "stage"]));

    let resp = dig(&server, 516, 516).await;
    assert_eq!(resp.status, 200);
    let body: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(
        body["stack"][0]["tags"],
        serde_json::json!(["encore", "stage"])
    );

    // An admin may edit someone else's placement.
    let resp = request(
        &server,
        "PUT",
        &tags_url,
        &[("Cookie", &admin), ("Content-Type", "application/json")],
        br#"{"tags":["moderated"]}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["tags"], serde_json::json!(["moderated"]));

    // Emptying the set is allowed; reads reflect it.
    let resp = request(
        &server,
        "PUT",
        &tags_url,
        &[("Cookie", &admin), ("Content-Type", "application/json")],
        br#"{"tags":[]}"#.to_vec(),
    )
    .await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json()["tags"], serde_json::json!([]));
    let near = request(
        &server,
        "GET",
        "/api/placements/near?x=516&y=516&r=1024",
        &[],
        vec![],
    )
    .await;
    assert_eq!(near.status, 200);
    let arr = near.json();
    let mine = arr
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id.as_str())
        .expect("placement row");
    assert_eq!(mine["tags"], serde_json::json!([]));
}

// -------------------------------------------------------------------------
// Feed: newest placements, tag filter, keyset pagination (auth-free)

#[tokio::test]
async fn feed_pagination_and_tag_filter() {
    let server = start_server(0, 0, 10 * 1024 * 1024).await;
    // First account is the admin (used for the removal at the end).
    let admin = register(&server, "feedadmin").await;
    let cookie = register(&server, "feeder").await;

    // Three placements, oldest first: p1 < p2 < p3 by seq.
    let (id1u, seq1) = seed_placement(&server, "feeder", 500, 500, 32, 32, 0.0).await;
    let (id2u, seq2) = seed_placement(&server, "feeder", 600, 600, 32, 32, 0.0).await;
    let (id3u, seq3) = seed_placement(&server, "feeder", 700, 700, 32, 32, 0.0).await;
    let id1 = id1u.to_string();
    let id3 = id3u.to_string();

    // Tag the middle and newest placements.
    for (id, body) in [
        (id3.clone(), br#"{"tags":["stage","encore"]}"#.to_vec()),
        (id2u.to_string(), br#"{"tags":["duo"]}"#.to_vec()),
    ] {
        let resp = request(
            &server,
            "PUT",
            &format!("/api/placements/{id}/tags"),
            &[("Cookie", &cookie), ("Content-Type", "application/json")],
            body,
        )
        .await;
        assert_eq!(resp.status, 200);
    }

    // Unauthenticated by design: a bare feed lists everything newest-first.
    let resp = request(&server, "GET", "/api/feed", &[], vec![]).await;
    assert_eq!(resp.status, 200);
    let body = resp.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["id"], id3.as_str());
    assert_eq!(items[0]["seq"], seq3);
    assert_eq!(items[2]["seq"], seq1);
    // Tags ride the rows.
    let mut tags = items[0]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect::<Vec<_>>();
    tags.sort();
    assert_eq!(tags, vec!["encore", "stage"]);
    // A page shorter than the limit is exhausted: no cursor.
    assert_eq!(body["next_before"], serde_json::Value::Null);

    // Tag filter (case-insensitive; the input is normalized).
    let resp = request(&server, "GET", "/api/feed?tag=STAGE", &[], vec![]).await;
    let body = resp.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], id3.as_str());
    assert_eq!(body["next_before"], serde_json::Value::Null);

    // Keyset pagination: a page of 2 stops at p2's seq...
    let resp = request(&server, "GET", "/api/feed?limit=2", &[], vec![]).await;
    let body = resp.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["seq"], seq3);
    assert_eq!(items[1]["seq"], seq2);
    assert_eq!(body["next_before"], serde_json::json!(seq2));
    // ...and `before` resumes strictly older.
    let resp = request(
        &server,
        "GET",
        &format!("/api/feed?before={seq2}"),
        &[],
        vec![],
    )
    .await;
    let body = resp.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["seq"], seq1);
    // A short page means exhausted: no cursor.
    assert_eq!(body["next_before"], serde_json::Value::Null);

    // Validation.
    for query in [
        "?tag=bad%20tag!",
        "?limit=0",
        "?limit=51",
        "?limit=bogus",
        "?before=bogus",
    ] {
        let resp = request(&server, "GET", &format!("/api/feed{query}"), &[], vec![]).await;
        assert_eq!(resp.status, 400, "{query}");
    }

    // Admin removal drops the placement from the feed (metadata follows
    // the moderation state like every other read; applied directly — the
    // API's DELETE only queues the removal for the worker).
    let _ = &admin;
    seed_remove(&server, id1u).await;
    let resp = request(&server, "GET", "/api/feed", &[], vec![]).await;
    let body = resp.json();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|row| row["id"] != id1.as_str()));
}

// -------------------------------------------------------------------------
// Hot regions: z6-cell density over a recency window (auth-free)

#[tokio::test]
async fn hot_regions_window_and_bounded_response() {
    let server = start_server(0, 0, 10 * 1024 * 1024).await;
    let _ = register(&server, "hotter").await;

    // Cluster A: two placements in the same z6 cell around the centre
    // (native 134217728 -> cell 33 on each axis: 134217728 / 2097152 = 64? no:
    // cell px = WORLD_PX/64 = 4194304, so cell = 134217728 / 4194304 = 32).
    let (a1, _) = seed_placement(&server, "hotter", 134217728, 134217728, 32, 32, 0.0).await;
    let _ = seed_placement(&server, "hotter", 134217760, 134217760, 32, 32, 0.0).await;
    // Cluster B: far away in a different cell.
    let _ = seed_placement(
        &server,
        "hotter",
        134217728 + 4 * 4194304,
        134217728 + 4 * 4194304,
        32,
        32,
        0.0,
    )
    .await;

    // Unauthenticated by design.
    let resp = request(&server, "GET", "/api/hot", &[], vec![]).await;
    assert_eq!(resp.status, 200);
    let body = resp.json();
    assert_eq!(body["window_hours"], 24);
    assert_eq!(body["total"], 3);
    let cells = body["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 2, "two distinct cells, hottest first");
    // The 2-placement cell leads.
    assert_eq!(cells[0]["count"], 2);
    assert_eq!(cells[0]["x"], 32);
    assert_eq!(cells[0]["y"], 32);
    assert_eq!(cells[1]["count"], 1);
    assert_eq!(cells[1]["x"], 36);
    assert_eq!(cells[1]["y"], 36);
    // Score is recency-weighted and bounded above by the raw count, and
    // sorted descending.
    let score0 = cells[0]["score"].as_f64().unwrap();
    let score1 = cells[1]["score"].as_f64().unwrap();
    assert!(score0 > score1, "{score0} must beat {score1}");
    // Two same-day placements weigh at most their raw count (the
    // weighting decays with age), and rounded to 2 decimals can hit it.
    assert!(
        score0 <= 2.0 && score0 > 1.0,
        "score out of range: {score0}"
    );

    // Bounded response: limit=1 keeps only the hottest cell.
    let resp = request(&server, "GET", "/api/hot?limit=1", &[], vec![]).await;
    let body = resp.json();
    assert_eq!(body["cells"].as_array().unwrap().len(), 1);
    assert_eq!(body["total"], 3, "total is window-wide, not per page");

    // Validation.
    for query in [
        "?window=0",
        "?window=169",
        "?window=bogus",
        "?limit=0",
        "?limit=25",
        "?limit=bogus",
    ] {
        let resp = request(&server, "GET", &format!("/api/hot{query}"), &[], vec![]).await;
        assert_eq!(resp.status, 400, "{query}");
    }

    // Admin removal shrinks both the cell count and the total (applied
    // directly — the API's DELETE only queues the removal).
    seed_remove(&server, a1).await;
    let resp = request(&server, "GET", "/api/hot", &[], vec![]).await;
    let body = resp.json();
    assert_eq!(body["total"], 2);
    let cells = body["cells"].as_array().unwrap();
    assert_eq!(cells[0]["count"], 1);
}
