//! Remaining JSON API: near, center, health, reports, image counts.

use crate::auth::Authed;
use crate::error::{AppError, AppResult};
use crate::state::App;
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
        crate::metrics::render(&state.pool),
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
        "SELECT p.id, a.username, p.seq, p.x, p.y, p.w, p.h, p.rot, p.created_at
         FROM placements p
         JOIN accounts a ON a.id = p.account_id
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
