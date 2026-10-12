//! Router assembly.

use crate::state::App;
use crate::{api, events, handlers, place, statics, tiles};
use axum::routing::{delete, get, post};
use axum::Router;

pub fn router(state: App) -> Router {
    let max_body = state.cfg.max_upload_bytes + (1 << 20); // multipart slack
    let api = Router::new()
        .route("/api/health", get(api::health))
        .route("/api/auth/register", post(handlers::register))
        .route("/api/auth/login", post(handlers::login))
        .route("/api/auth/logout", post(handlers::logout))
        .route("/api/auth/me", get(handlers::me))
        .route("/api/place", post(place::place))
        .route("/api/tiles/{z}/{x}/{*y}", get(tiles::get_tile))
        .route("/api/placements/near", get(api::near))
        .route("/api/center", get(api::center))
        .route("/api/events", get(events::sse))
        .route("/api/reports", post(api::report))
        .route("/api/images/{sha}/count", get(api::image_count))
        .route("/api/admin/placements/{id}", delete(place::admin_delete))
        .layer(axum::extract::DefaultBodyLimit::max(max_body))
        .with_state(state.clone());
    api.fallback(|req: axum::http::Request<axum::body::Body>| async move {
        let uri = req.uri().clone();
        statics::fallback(state.clone(), uri, req).await
    })
    .layer(tower_http::trace::TraceLayer::new_for_http())
}
