//! POST /api/auth/* handlers.

use crate::auth::{claims_from_headers, cookie_value, issue, set_cookie, Authed};
use crate::error::{AppError, AppResult};
use crate::state::App;
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, SaltString};
use argon2::{Argon2, PasswordVerifier};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

fn valid_username(u: &str) -> bool {
    let n = u.len();
    (3..=32).contains(&n)
        && u.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn valid_password(p: &str) -> bool {
    // Long passwords are pointless and slow argon2 down; cap them.
    (8..=128).contains(&p.len())
}

fn hash_password(p: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(p.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(crate::error::internal)
}

fn verify_password(hash: &str, p: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(p.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

#[derive(Deserialize)]
pub struct RegisterReq {
    username: String,
    password: String,
}

/// POST /api/auth/register → 200, sets session cookie. First account ever
/// becomes admin (single-admission enforced by a partial unique index).
pub async fn register(State(app): State<App>, Json(req): Json<RegisterReq>) -> AppResult<Response> {
    if !valid_username(&req.username) {
        return Err(AppError::BadRequest(
            "username must be 3-32 chars of a-z, 0-9, _ or -",
        ));
    }
    if !valid_password(&req.password) {
        return Err(AppError::BadRequest("password must be at least 8 chars"));
    }
    let hash = hash_password(&req.password)?;
    let id = Uuid::now_v7();
    let mut resp = match try_insert(&app, id, &req.username, &hash, true).await {
        Ok(r) => r,
        Err(e) => {
            // Lost the single-admin race → retry as a normal account.
            if constraint_of(&e) == Some("accounts_single_admin") {
                try_insert(&app, id, &req.username, &hash, false)
                    .await
                    .map_err(|e| {
                        if constraint_of(&e) == Some("accounts_username_key") {
                            AppError::Conflict
                        } else {
                            AppError::from(e)
                        }
                    })?
            } else if constraint_of(&e) == Some("accounts_username_key") {
                return Err(AppError::Conflict);
            } else {
                return Err(AppError::from(e));
            }
        }
    };
    let token = issue(&app.cfg.session_secret, id, chrono::Utc::now().timestamp());
    set_cookie(&mut resp, cookie_value(&token, app.cfg.secure_cookies()));
    Ok(resp)
}

fn constraint_of(e: &sqlx::Error) -> Option<&str> {
    match e {
        sqlx::Error::Database(db) => db.constraint(),
        _ => None,
    }
}

async fn try_insert(
    app: &App,
    id: Uuid,
    username: &str,
    hash: &str,
    as_admin: bool,
) -> Result<Response, sqlx::Error> {
    let result: (bool,) = sqlx::query_as(
        "INSERT INTO accounts (id, username, password_hash, is_admin)
         VALUES ($1, $2, $3, $4)
         RETURNING is_admin",
    )
    .bind(id)
    .bind(username)
    .bind(hash)
    .bind(as_admin)
    .fetch_one(&app.pool)
    .await?;
    let resp = (
        axum::http::StatusCode::CREATED,
        Json(json!({ "id": id, "username": username, "is_admin": result.0 })),
    )
        .into_response();
    Ok(resp)
}

#[derive(Deserialize)]
pub struct LoginReq {
    username: String,
    password: String,
}

/// POST /api/auth/login → 200, sets session cookie.
pub async fn login(State(app): State<App>, Json(req): Json<LoginReq>) -> AppResult<Response> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, password_hash FROM accounts WHERE username = $1")
            .bind(&req.username)
            .fetch_optional(&app.pool)
            .await?;
    let Some((id, hash)) = row else {
        return Err(AppError::Unauthorized);
    };
    if !verify_password(&hash, &req.password) {
        return Err(AppError::Unauthorized);
    }
    let mut resp = (axum::http::StatusCode::OK, Json(json!({ "id": id }))).into_response();
    let token = issue(&app.cfg.session_secret, id, chrono::Utc::now().timestamp());
    set_cookie(&mut resp, cookie_value(&token, app.cfg.secure_cookies()));
    Ok(resp)
}

/// POST /api/auth/logout → 204, clears cookie.
pub async fn logout(State(app): State<App>, headers: HeaderMap) -> AppResult<Response> {
    let _ = claims_from_headers(
        &app.cfg.session_secret,
        &headers,
        chrono::Utc::now().timestamp(),
    );
    let mut resp = crate::auth::status_no_content();
    set_cookie(
        &mut resp,
        crate::auth::clear_cookie(app.cfg.secure_cookies()),
    );
    Ok(resp)
}

/// GET /api/auth/me → current account + active cooldown end.
pub async fn me(State(app): State<App>, Authed(id): Authed) -> AppResult<Json<serde_json::Value>> {
    let row: Option<(String, bool, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT username, is_admin, last_placement_at FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(&app.pool)
            .await?;
    let Some((username, is_admin, last)) = row else {
        return Err(AppError::Unauthorized);
    };
    let now = Utc::now();
    let cooldown_until = last
        .map(|t| t + chrono::Duration::seconds(app.cfg.cooldown_paint_secs))
        .filter(|t| *t > now);
    Ok(Json(json!({
        "id": id,
        "username": username,
        "is_admin": is_admin,
        "cooldown_until": cooldown_until,
    })))
}
