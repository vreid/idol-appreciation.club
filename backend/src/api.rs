//! Remaining JSON API: near, center, health, reports, image counts.

use crate::auth::Authed;
use crate::error::{AppError, AppResult};
use crate::state::App;
use crate::world;
use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

/// GET /api/health
pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

/// Prometheus text exposition (format 0.0.4). Public: aggregates only.
pub async fn metrics(State(state): State<App>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        crate::metrics::render(
            &state.pool,
            state.render_queue.depth().await,
            state.render_queue.dropped(),
        ),
    )
}

fn parse_param<T: std::str::FromStr>(params: &HashMap<String, String>, name: &str) -> AppResult<T> {
    params
        .get(name)
        .and_then(|v| v.parse().ok())
        .ok_or(AppError::BadRequest("missing or invalid query parameter"))
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct NearPlacement {
    pub id: Uuid,
    pub username: String,
    /// Global manifest sequence (monotonic; topmost-at-a-point = max seq).
    pub seq: i64,
    pub x: i64,
    pub y: i64,
    pub w: i32,
    pub h: i32,
    /// Rotation in degrees, clockwise (y-down), normalized to [0, 360).
    pub rot: f32,
    pub created_at: DateTime<Utc>,
    /// Normalized label set (sorted); empty when untagged. Tags are pure
    /// metadata — they ride every placement read but never affect tiles,
    /// seq, or hit-testing.
    #[sqlx(default)]
    pub tags: Vec<String>,
}

/// Tag rules: 1..=32 chars of `[a-z0-9_-]` after trim+lowercase, at most
/// 8 per placement, duplicates dropped. Returns the message of the first
/// violation so both the PUT handler and the import manifest map it to a
/// 400.
/// One normalized label: 1..=32 chars of [a-z0-9_-] (already lowercased).
fn valid_tag(t: &str) -> bool {
    t.len() <= 32
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub(crate) fn normalize_tags(raw: Vec<String>) -> Result<Vec<String>, &'static str> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len().min(8));
    for t in raw {
        let t = t.trim().to_ascii_lowercase();
        if t.is_empty() {
            continue;
        }
        if !valid_tag(&t) {
            return Err("tags must be 1..=32 chars of [a-z0-9_-]");
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > 8 {
        return Err("at most 8 tags per placement");
    }
    Ok(out)
}

/// Max entries a dig stack returns; a deeper stack reports `truncated`.
pub const DIG_STACK_CAP: usize = 64;

/// One stratum in a dig stack: the placement plus its 1-based depth
/// (1 = topmost / newest at the point).
#[derive(serde::Serialize)]
pub struct DigEntry {
    pub depth: u32,
    #[serde(flatten)]
    pub placement: NearPlacement,
}

/// GET /api/placements/near?x=&y=&r= — placements whose bbox (the AABB of
/// the rotated rect) intersects the query rect [x−r, x+r) × [y−r, y+r),
/// newest first.
pub async fn near(
    State(app): State<App>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<Vec<NearPlacement>>> {
    let x: i64 = parse_param(&params, "x")?;
    let y: i64 = parse_param(&params, "y")?;
    let r: i64 = parse_param(&params, "r")?;
    if r <= 0 {
        return Err(AppError::BadRequest("r must be positive"));
    }
    let r = r.min(65536);
    let rows = sqlx::query_as::<_, NearPlacement>(
        "SELECT p.id, a.username, p.seq, p.x, p.y, p.w, p.h, p.rot, p.created_at,
                COALESCE(t.tags, '{}') AS tags
         FROM placements p
         JOIN accounts a ON a.id = p.account_id
         LEFT JOIN LATERAL (SELECT array_agg(pt.tag ORDER BY pt.tag) AS tags
                            FROM placement_tags pt
                            WHERE pt.placement_id = p.id) t ON true
         WHERE p.removed_at IS NULL
           AND p.bbox && box(point($1::float8, $2::float8),
                             point($3::float8, $4::float8))
         ORDER BY p.seq DESC
         LIMIT 50",
    )
    .bind(x - r)
    .bind(y - r)
    .bind(x + r - 1)
    .bind(y + r - 1)
    .fetch_all(&app.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(serde::Serialize)]
pub struct FeedRes {
    /// Newest-first placements (most recent first, global `seq` order).
    pub items: Vec<NearPlacement>,
    /// `seq` cursor for the next-older page (pass as `?before=`); null
    /// when the feed is exhausted.
    pub next_before: Option<i64>,
}

/// GET /api/feed — the newest placements, newest-first, optionally
/// filtered to one normalized tag. Auth-free read with keyset
/// pagination: pass `before=<next_before>` from the previous page.
pub async fn feed(
    State(app): State<App>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<FeedRes>> {
    let tag = match params.get("tag").map(|t| t.trim()) {
        None | Some("") => None,
        Some(raw) => {
            let tag = raw.to_ascii_lowercase();
            if !valid_tag(&tag) {
                return Err(AppError::BadRequest(
                    "tags must be 1..=32 chars of [a-z0-9_-]",
                ));
            }
            Some(tag)
        }
    };
    let limit: i64 = match params.get("limit") {
        None => 20,
        Some(raw) => raw
            .parse()
            .map_err(|_| AppError::BadRequest("missing or invalid query parameter"))?,
    };
    if !(1..=50).contains(&limit) {
        return Err(AppError::BadRequest("limit must be 1..=50"));
    }
    let before: Option<i64> = match params.get("before") {
        None => None,
        Some(raw) => Some(
            raw.parse()
                .map_err(|_| AppError::BadRequest("missing or invalid query parameter"))?,
        ),
    };
    let items = sqlx::query_as::<_, NearPlacement>(
        "SELECT p.id, a.username, p.seq, p.x, p.y, p.w, p.h, p.rot, p.created_at,
                COALESCE(t.tags, '{}') AS tags
         FROM placements p
         JOIN accounts a ON a.id = p.account_id
         LEFT JOIN LATERAL (SELECT array_agg(pt.tag ORDER BY pt.tag) AS tags
                            FROM placement_tags pt
                            WHERE pt.placement_id = p.id) t ON true
         WHERE p.removed_at IS NULL
           AND ($1::bigint IS NULL OR p.seq < $1)
           AND ($2::text IS NULL OR EXISTS (SELECT 1 FROM placement_tags ft
                                            WHERE ft.placement_id = p.id
                                              AND ft.tag = $2))
         ORDER BY p.seq DESC
         LIMIT $3",
    )
    .bind(before)
    .bind(tag)
    .bind(limit)
    .fetch_all(&app.pool)
    .await?;
    let next_before = if items.len() as i64 == limit {
        items.last().map(|item| item.seq)
    } else {
        None
    };
    Ok(Json(FeedRes { items, next_before }))
}

// -------------------------------------------------------------------------
// Hot regions: placement density per z6 cell over a recent window

/// Grid granularity for /api/hot: the z6 tile grid (64x64 cells, each
/// 4 Mi native px) — coarse enough to summarize, aligned with the tile
/// pyramid so clients can address a cell as a map region.
const HOT_Z: i64 = 6;

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct HotCell {
    /// z6 grid index, 0..64, native (tile) coordinates.
    pub x: i64,
    pub y: i64,
    /// Placements in the cell inside the window.
    pub count: i64,
    /// Recency-weighted score: each placement contributes
    /// 1 / (1 + age_days), so a fresh placement is worth ~1 and a
    /// week-old one ~0.13. Sorted descending.
    pub score: f64,
}

#[derive(serde::Serialize)]
pub struct HotRes {
    /// The window actually applied (hours).
    pub window_hours: i64,
    /// Placements inside the window across the whole world.
    pub total: i64,
    /// Hottest cells first, at most `limit` entries.
    pub cells: Vec<HotCell>,
}

/// GET /api/hot — the busiest regions over a recent window. Auth-free
/// read; bounded response (at most `limit` cells, 1..=24, default 8).
pub async fn hot(
    State(app): State<App>,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Json<HotRes>> {
    let window_hours: i64 = match params.get("window") {
        None => 24,
        Some(raw) => raw
            .parse()
            .map_err(|_| AppError::BadRequest("missing or invalid query parameter"))?,
    };
    if !(1..=168).contains(&window_hours) {
        return Err(AppError::BadRequest("window must be 1..=168 hours"));
    }
    let limit: i64 = match params.get("limit") {
        None => 8,
        Some(raw) => raw
            .parse()
            .map_err(|_| AppError::BadRequest("missing or invalid query parameter"))?,
    };
    if !(1..=24).contains(&limit) {
        return Err(AppError::BadRequest("limit must be 1..=24"));
    }
    // Placement x/y are native px, so the z6 cell is a plain division
    // by WORLD_PX / 64; the grid matches the tile pyramid's z6 indices.
    let cell = world::WORLD_PX / (1 << HOT_Z);
    let rows = sqlx::query_as::<_, HotCell>(
        "SELECT (p.x / $3)::bigint AS x,
                (p.y / $3)::bigint AS y,
                count(*)::bigint AS count,
                round(sum(1.0 / (1.0 +
                    EXTRACT(EPOCH FROM (now() - p.created_at)) / 86400.0))
                    ::numeric, 2)::float8 AS score
         FROM placements p
         WHERE p.removed_at IS NULL
           AND p.created_at >= now() - make_interval(hours => $1::int)
         GROUP BY 1, 2
         ORDER BY score DESC, count DESC
         LIMIT $2",
    )
    .bind(window_hours as i32)
    .bind(limit)
    .bind(cell)
    .fetch_all(&app.pool)
    .await?;
    let total: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM placements
         WHERE removed_at IS NULL
           AND created_at >= now() - make_interval(hours => $1::int)",
    )
    .bind(window_hours as i32)
    .fetch_one(&app.pool)
    .await?;
    Ok(Json(HotRes {
        window_hours,
        total: total.0,
        cells: rows,
    }))
}

/// A placement that covers the world center (AABB candidate — membership
/// in the rotated rect is refined in Rust).
#[derive(sqlx::FromRow)]
#[allow(dead_code)] // seq documents the ordering contract; kept for clarity
struct CenterCandidate {
    username: String,
    created_at: DateTime<Utc>,
    id: Uuid,
    x: i64,
    y: i64,
    w: i32,
    h: i32,
    rot: f32,
    seq: i64,
}

impl CenterCandidate {
    /// Point-in-rotated-rect hit-test against the world center.
    fn covers_center(&self) -> bool {
        crate::world::rotated_contains(
            self.x,
            self.y,
            i64::from(self.w),
            i64::from(self.h),
            self.rot,
            crate::world::CENTER_X,
            crate::world::CENTER_Y,
        )
    }
}

/// GET /api/dig?x=&y= — the wheel-dig stack: every placement whose
/// ROTATED rect contains the point, newest-first (depth 1 = topmost).
/// Candidates come from the GiST bbox (`@>` on the AABB), membership is
/// refined with the exact rotated hit-test. Bounded: at most
/// [`DIG_STACK_CAP`] entries; `truncated` reports a cut stack. Auth-free
/// read. ETag = the global placement-seq counter — the same generator the
/// tile reads' staleness marker derives from — so a revalidation with a
/// matching If-None-Match is a cheap 304, and any new placement
/// invalidates every outstanding stack at once.
pub async fn dig(
    State(app): State<App>,
    headers: axum::http::HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> AppResult<Response> {
    let x: i64 = parse_param(&params, "x")?;
    let y: i64 = parse_param(&params, "y")?;
    if x < 0 || y < 0 || x >= crate::world::WORLD_PX || y >= crate::world::WORLD_PX {
        return Err(AppError::BadRequest("x and y must be within world bounds"));
    }
    let rows = sqlx::query_as::<_, NearPlacement>(
        "SELECT p.id, a.username, p.seq, p.x, p.y, p.w, p.h, p.rot, p.created_at,
                COALESCE(t.tags, '{}') AS tags
         FROM placements p
         JOIN accounts a ON a.id = p.account_id
         LEFT JOIN LATERAL (SELECT array_agg(pt.tag ORDER BY pt.tag) AS tags
                            FROM placement_tags pt
                            WHERE pt.placement_id = p.id) t ON true
         WHERE p.removed_at IS NULL
           AND p.bbox @> point($1::float8, $2::float8)
         ORDER BY p.seq DESC",
    )
    .bind(x as f64)
    .bind(y as f64)
    .fetch_all(&app.pool)
    .await?;
    let covering: Vec<NearPlacement> = rows
        .into_iter()
        .filter(|p| {
            crate::world::rotated_contains(p.x, p.y, i64::from(p.w), i64::from(p.h), p.rot, x, y)
        })
        .collect();
    let total = covering.len();
    let truncated = total > DIG_STACK_CAP;
    let stack: Vec<DigEntry> = covering
        .into_iter()
        .take(DIG_STACK_CAP)
        .enumerate()
        .map(|(i, placement)| DigEntry {
            depth: (i + 1) as u32,
            placement,
        })
        .collect();
    let (seq,): (i64,) = sqlx::query_as("SELECT last_value FROM placement_seq")
        .fetch_one(&app.pool)
        .await?;
    let etag = format!("\"{seq}\"");
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(&etag)
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let body = json!({ "stack": stack, "total": total, "truncated": truncated });
    let mut resp = (StatusCode::OK, Json(body)).into_response();
    resp.headers_mut()
        .insert(header::ETAG, etag.parse().expect("etag header"));
    Ok(resp)
}

/// GET /api/center — current holder of the world center and the top 10
/// all-time holders. Covering = the point lies inside the ROTATED rect
/// (candidates come from the GiST bbox, then a rotated hit-test).
/// Held-seconds are derived purely from the manifest: consecutive
/// covering placements define segments; the newest covering placement
/// holds until now.
pub async fn center(State(app): State<App>) -> AppResult<Json<serde_json::Value>> {
    let candidates: Vec<CenterCandidate> = sqlx::query_as(
        "SELECT a.username, p.created_at, p.id, p.x, p.y, p.w, p.h, p.rot, p.seq
         FROM placements p
         JOIN accounts a ON a.id = p.account_id
         WHERE p.removed_at IS NULL
           AND p.bbox @> point($1::float8, $2::float8)
         ORDER BY p.seq",
    )
    .bind(crate::world::CENTER_X as f64)
    .bind(crate::world::CENTER_Y as f64)
    .fetch_all(&app.pool)
    .await?;
    let covering: Vec<&CenterCandidate> = candidates.iter().filter(|c| c.covers_center()).collect();

    // Current = newest covering placement (max seq, candidates are
    // seq-ascending).
    let current_json = covering.last().map(|c| {
        json!({
            "username": c.username,
            "since": c.created_at,
            "placement": {
                "id": c.id,
                "x": c.x,
                "y": c.y,
                "w": c.w,
                "h": c.h,
                "rot": c.rot,
            }
        })
    });

    // Held-seconds: segments between consecutive covering seqs; the last
    // segment runs until now. The covering set is small (a handful of
    // rects contain the point), so linear aggregation is fine.
    let mut totals: Vec<(String, f64)> = Vec::new();
    for (i, c) in covering.iter().enumerate() {
        let end = covering
            .get(i + 1)
            .map(|next| next.created_at)
            .unwrap_or_else(Utc::now);
        let secs = (end - c.created_at).num_milliseconds() as f64 / 1000.0;
        if let Some(entry) = totals.iter_mut().find(|(u, _)| *u == c.username) {
            entry.1 += secs;
        } else {
            totals.push((c.username.clone(), secs));
        }
    }
    let mut all_time: Vec<(String, i64)> = totals
        .into_iter()
        .map(|(username, secs)| (username, secs.round() as i64))
        .collect();
    all_time.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    all_time.truncate(10);

    Ok(Json(json!({
        "current": current_json,
        "all_time": all_time.iter().map(|(username, seconds)| json!({
            "username": username,
            "seconds": seconds,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct ReportReq {
    placement_id: Uuid,
}

/// GET /api/admin/accounts?pending=true → pending queue, oldest first.
/// Without the flag: all accounts, same shape.
pub async fn admin_list_accounts(
    State(app): State<App>,
    Authed(admin_id): Authed,
    axum::extract::Query(q): axum::extract::Query<PendingQuery>,
) -> AppResult<Response> {
    crate::auth::require_admin(&app, admin_id).await?;
    let sql = if q.pending == Some(true) {
        "SELECT username, created_at, cooldown_exempt FROM accounts WHERE NOT approved ORDER BY created_at ASC, id ASC"
    } else {
        "SELECT username, created_at, cooldown_exempt FROM accounts ORDER BY created_at ASC, id ASC"
    };
    let rows: Vec<(String, DateTime<Utc>, bool)> = sqlx::query_as(sql).fetch_all(&app.pool).await?;
    Ok(Json(json!(rows
        .into_iter()
        .map(|(username, created_at, cooldown_exempt)| json!({
            "username": username,
            "created_at": created_at,
            "cooldown_exempt": cooldown_exempt,
        }))
        .collect::<Vec<_>>()))
    .into_response())
}

#[derive(Deserialize)]
pub struct PendingQuery {
    pending: Option<bool>,
}

/// POST /api/admin/accounts/{username}/approve → 204 (idempotent; 404 for
/// unknown usernames).
pub async fn admin_approve(
    State(app): State<App>,
    Authed(admin_id): Authed,
    axum::extract::Path(username): axum::extract::Path<String>,
) -> AppResult<Response> {
    crate::auth::require_admin(&app, admin_id).await?;
    let result = sqlx::query("UPDATE accounts SET approved = true WHERE username = $1")
        .bind(&username)
        .execute(&app.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

/// POST /api/admin/accounts/{username}/cooldown-exempt with
/// {"exempt": true|false} → 204 (idempotent; 404 unknown user). Exempt
/// accounts skip both placement cooldowns.
pub async fn admin_cooldown_exempt(
    State(app): State<App>,
    Authed(admin_id): Authed,
    axum::extract::Path(username): axum::extract::Path<String>,
    Json(req): Json<ExemptReq>,
) -> AppResult<Response> {
    crate::auth::require_admin(&app, admin_id).await?;
    let result = sqlx::query("UPDATE accounts SET cooldown_exempt = $1 WHERE username = $2")
        .bind(req.exempt)
        .bind(&username)
        .execute(&app.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(axum::http::StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct ExemptReq {
    pub exempt: bool,
}

/// POST /api/reports → 204.
pub async fn report(
    State(app): State<App>,
    Authed(account_id): Authed,
    Json(req): Json<ReportReq>,
) -> AppResult<Response> {
    let exists: Option<(i64,)> =
        sqlx::query_as("SELECT 1::bigint FROM placements WHERE id = $1 AND removed_at IS NULL")
            .bind(req.placement_id)
            .fetch_optional(&app.pool)
            .await?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }
    sqlx::query("INSERT INTO reports (id, placement_id, account_id) VALUES ($1, $2, $3)")
        .bind(Uuid::now_v7())
        .bind(req.placement_id)
        .bind(account_id)
        .execute(&app.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct TagsReq {
    pub tags: Vec<String>,
}

#[derive(serde::Serialize)]
pub struct TagsRes {
    pub id: Uuid,
    pub tags: Vec<String>,
}

/// PUT /api/placements/{id}/tags → the placement's normalized tag set.
/// Owner or admin; 404 for unknown/removed placements.
pub async fn tags_put(
    State(app): State<App>,
    Authed(account_id): Authed,
    Path(id): Path<Uuid>,
    Json(req): Json<TagsReq>,
) -> AppResult<Json<TagsRes>> {
    let tags = normalize_tags(req.tags).map_err(AppError::BadRequest)?;
    let owner: Option<(Uuid,)> =
        sqlx::query_as("SELECT account_id FROM placements WHERE id = $1 AND removed_at IS NULL")
            .bind(id)
            .fetch_optional(&app.pool)
            .await?;
    let Some((owner,)) = owner else {
        return Err(AppError::NotFound);
    };
    if owner != account_id {
        crate::auth::require_admin(&app, account_id).await?;
    }
    let mut tx = app.pool.begin().await?;
    sqlx::query("DELETE FROM placement_tags WHERE placement_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    for tag in &tags {
        sqlx::query(
            "INSERT INTO placement_tags (placement_id, tag) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(tag)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(TagsRes { id, tags }))
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// GET /api/images/{sha256}/count → non-removed placements using that
/// content-addressed original.
pub async fn image_count(
    State(app): State<App>,
    Path(sha): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    if !valid_sha(&sha) {
        return Err(AppError::BadRequest("not a sha256 hex digest"));
    }
    let row: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM placements
         WHERE original_sha = $1 AND removed_at IS NULL",
    )
    .bind(sha)
    .fetch_one(&app.pool)
    .await?;
    Ok(Json(json!({ "count": row.0 })))
}
