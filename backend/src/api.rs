//! Remaining JSON API: near, center, health, reports, image counts.

use crate::auth::Authed;
use crate::error::{AppError, AppResult};
use crate::state::App;
use crate::world::{CENTER_X, CENTER_Y};
use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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
    pub x: i64,
    pub y: i64,
    pub w: i32,
    pub h: i32,
    pub created_at: DateTime<Utc>,
}

/// GET /api/placements/near?x=&y=&r= — placements whose bbox intersects the
/// query rect [x−r, x+r) × [y−r, y+r), newest first.
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
        "SELECT p.id, a.username, p.x, p.y, p.w, p.h, p.created_at
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

/// GET /api/center — current holder of the world center and the top 10
/// all-time holders. Held-seconds are derived purely from the manifest:
/// consecutive covering placements define segments; the newest covering
/// placement holds until now.
pub async fn center(State(app): State<App>) -> AppResult<Json<serde_json::Value>> {
    let current: Option<(String, DateTime<Utc>, Uuid, i64, i64, i32, i32)> = sqlx::query_as(
        "SELECT a.username, p.created_at, p.id, p.x, p.y, p.w, p.h
         FROM placements p
         JOIN accounts a ON a.id = p.account_id
         WHERE p.removed_at IS NULL
           AND p.x <= $1 AND p.x + p.w > $1
           AND p.y <= $2 AND p.y + p.h > $2
         ORDER BY p.seq DESC
         LIMIT 1",
    )
    .bind(CENTER_X)
    .bind(CENTER_Y)
    .fetch_optional(&app.pool)
    .await?;

    #[derive(sqlx::FromRow)]
    struct Holder {
        username: String,
        total: i64,
    }
    let all_time: Vec<Holder> = sqlx::query_as::<_, Holder>(
        "WITH cov AS (
             SELECT a.username, p.created_at,
                    lead(p.created_at) OVER (ORDER BY p.seq) AS next_created
             FROM placements p
             JOIN accounts a ON a.id = p.account_id
             WHERE p.removed_at IS NULL
               AND p.x <= $1 AND p.x + p.w > $1
               AND p.y <= $2 AND p.y + p.h > $2
         ),
         seg AS (
             SELECT username,
                    CASE WHEN next_created IS NULL
                         THEN EXTRACT(EPOCH FROM (now() - created_at))
                         ELSE EXTRACT(EPOCH FROM (next_created - created_at))
                    END AS secs
             FROM cov
         )
         SELECT username, COALESCE(sum(secs), 0)::bigint AS total
         FROM seg
         GROUP BY username
         ORDER BY total DESC, username
         LIMIT 10",
    )
    .bind(CENTER_X)
    .bind(CENTER_Y)
    .fetch_all(&app.pool)
    .await?;

    let current_json = current.map(
        |(username, since, id, x, y, w, h)| {
            json!({
                "username": username,
                "since": since,
                "placement": { "id": id, "x": x, "y": y, "w": w, "h": h }
            })
        },
    );
    Ok(Json(json!({
        "current": current_json,
        "all_time": all_time.iter().map(|hld| json!({
            "username": hld.username,
            "seconds": hld.total,
        })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct ReportReq {
    placement_id: Uuid,
}

/// POST /api/reports → 204.
pub async fn report(
    State(app): State<App>,
    Authed(account_id): Authed,
    Json(req): Json<ReportReq>,
) -> AppResult<Response> {
    let exists: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM placements WHERE id = $1 AND removed_at IS NULL")
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
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
