//! App error type: every handler failure maps to JSON `{"error": "..."}`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(&'static str),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    /// 403: registration awaiting admin approval.
    #[error("account pending approval")]
    PendingApproval,
    #[error("not found")]
    NotFound,
    #[error("conflict")]
    Conflict,
    /// 429 with Retry-After seconds.
    #[error("cooldown active")]
    Cooldown(i64),
    #[error("payload too large")]
    TooLarge,
    #[error(transparent)]
    Vips(#[from] libvips::error::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, retry_after, msg) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, None, *m),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, None, "unauthorized"),
            AppError::Forbidden => (StatusCode::FORBIDDEN, None, "forbidden"),
            AppError::PendingApproval => {
                (StatusCode::FORBIDDEN, None, "account pending approval")
            }
            AppError::NotFound => (StatusCode::NOT_FOUND, None, "not found"),
            AppError::Conflict => (StatusCode::CONFLICT, None, "conflict"),
            AppError::Cooldown(secs) => (StatusCode::TOO_MANY_REQUESTS, Some(*secs), "cooldown"),
            AppError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, None, "payload too large"),
            AppError::Vips(e) => {
                tracing::error!(error = %e, "vips error");
                (StatusCode::INTERNAL_SERVER_ERROR, None, "internal error")
            }
            AppError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, None, "internal error")
            }
        };
        let mut resp = (status, axum::Json(json!({ "error": msg }))).into_response();
        if let Some(secs) = retry_after {
            let v = axum::http::HeaderValue::from(secs.max(1));
            resp.headers_mut().insert("retry-after", v);
        }
        resp
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => AppError::NotFound,
            sqlx::Error::Database(db) if db.kind() == sqlx::error::ErrorKind::UniqueViolation => {
                AppError::Conflict
            }
            sqlx::Error::Database(db)
                if db.kind() == sqlx::error::ErrorKind::ForeignKeyViolation =>
            {
                AppError::NotFound
            }
            _ => AppError::Internal(anyhow::anyhow!(e).context("database")),
        }
    }
}

impl From<object_store::Error> for AppError {
    fn from(e: object_store::Error) -> Self {
        AppError::Internal(anyhow::anyhow!(e).context("object storage"))
    }
}

impl From<object_store::path::Error> for AppError {
    fn from(e: object_store::path::Error) -> Self {
        AppError::Internal(anyhow::anyhow!(e).context("object storage path"))
    }
}

pub fn internal(e: impl std::fmt::Display) -> AppError {
    AppError::Internal(anyhow::anyhow!(e.to_string()))
}
