//! HTTP/WebSocket surface.
//!
//! Handlers are kept thin — they extract from axum, validate, and hand off
//! to a service in another crate. Auth and session orchestration are NOT
//! implemented at this layer.

use axum::Router;
use relayterm_db::Db;
use tower_http::trace::TraceLayer;

mod routes;

/// Shared state injected into every handler via `axum::extract::State`.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
}

/// Build the top-level router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::health::router())
        .merge(routes::ws::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
