//! Static frontend serving with SPA fallback; graceful JSON when the web
//! build is absent.

use crate::state::App;
use axum::body::Body;
use axum::http::{Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

fn json_404() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

/// Router fallback: /api/* always JSON 404; otherwise ServeDir with
/// index.html fallback (SPA). When STATIC_DIR is missing, `/` serves a tiny
/// JSON banner so the deployment stays graceful.
pub async fn fallback(app: App, uri: Uri, req: Request<Body>) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") || path == "/api" {
        return json_404();
    }
    let dir = app.cfg.static_dir.clone();
    if dir.is_dir() {
        let index = dir.join("index.html");
        let svc = ServeDir::new(&dir)
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(index));
        match svc.oneshot(req).await {
            Ok(resp) => resp.map(Body::new),
            Err(_) => json_404(),
        }
    } else if path == "/" {
        Json(json!({
            "service": "idol-appreciation.club",
            "static": false
        }))
        .into_response()
    } else {
        json_404()
    }
}
