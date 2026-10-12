//! Stateless HMAC-signed session cookies. No server-side session store.

use crate::error::{AppError, AppResult};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

const COOKIE_NAME: &str = "session";
/// 30 days.
const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;

#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    /// Unix expiry.
    pub exp: i64,
}

fn sign(secret: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac key");
    mac.update(payload);
    mac.finalize().into_bytes().to_vec()
}

pub fn issue(secret: &[u8], account_id: Uuid, now: i64) -> String {
    let claims = Claims { sub: account_id, exp: now + SESSION_TTL_SECS };
    let payload = serde_json::to_vec(&claims).expect("serialize claims");
    let body = URL_SAFE_NO_PAD.encode(payload);
    let sig = URL_SAFE_NO_PAD.encode(sign(secret, body.as_bytes()));
    format!("{body}.{sig}")
}

pub fn verify(secret: &[u8], token: &str, now: i64) -> Option<Claims> {
    let (body, sig) = token.split_once('.')?;
    let expected = URL_SAFE_NO_PAD.decode(sig).ok()?;
    if expected.len() != 32 {
        return None;
    }
    // Constant-time comparison via hmac::verify semantics.
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).ok()?;
    mac.update(body.as_bytes());
    mac.verify_slice(&expected).ok()?;
    let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body).ok()?).ok()?;
    if claims.exp <= now {
        return None;
    }
    Some(claims)
}

/// Extract the session cookie from request headers.
pub fn claims_from_headers(secret: &[u8], headers: &HeaderMap, now: i64) -> Option<Claims> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for pair in cookies.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(COOKIE_NAME)?.strip_prefix('=') {
            return verify(secret, value, now);
        }
    }
    None
}

/// Build a Set-Cookie header value for a fresh session.
pub fn cookie_value(token: &str, secure: bool) -> String {
    let mut c = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}"
    );
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Cookie that clears the session (logout).
pub fn clear_cookie(secure: bool) -> String {
    let mut c = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        c.push_str("; Secure");
    }
    c
}

/// Axum extractor: authenticated account id. 401 when absent/invalid.
#[derive(Debug, Clone)]
pub struct Authed(pub Uuid);

impl<S> axum::extract::FromRequestParts<S> for Authed
where
    App: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let app = App::from_ref(state);
        let now = chrono::Utc::now().timestamp();
        match claims_from_headers(&app.cfg.session_secret, &parts.headers, now) {
            Some(c) => Ok(Authed(c.sub)),
            None => Err(AppError::Unauthorized.into_response()),
        }
    }
}

/// Admin guard: 401 unauthenticated, 403 authenticated non-admin.
pub async fn require_admin(app: &App, id: Uuid) -> AppResult<()> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT is_admin FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(&app.pool)
            .await?;
    match row {
        Some((true,)) => Ok(()),
        Some((false,)) => Err(AppError::Forbidden),
        None => Err(AppError::Unauthorized),
    }
}

/// Set-Cookie helper.
pub fn set_cookie(resp: &mut Response, value: String) {
    if let Ok(v) = axum::http::HeaderValue::from_str(&value) {
        resp.headers_mut().append(SET_COOKIE, v);
    }
}

pub fn status_no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
