//! HTTP/WebSocket surface.
//!
//! Handlers are kept thin — they extract from axum, validate, and hand off
//! to a service in another crate. Auth and session orchestration are NOT
//! implemented at this layer; `dev_user` injects a stopgap [`UserId`] until
//! they are.

use axum::{Router, extract::FromRef};
use relayterm_core::ids::UserId;
use relayterm_db::Db;
use relayterm_vault::VaultService;
use tower_http::trace::TraceLayer;

mod dev_user;
mod dto;
mod error;
mod routes;

pub use dev_user::DevUser;
pub use error::ApiError;

/// Shared state injected into every handler via `axum::extract::State`.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    /// Vault service used to generate and decrypt SSH identities. `None`
    /// means vault-backed identity creation is disabled — the
    /// `POST /api/v1/ssh-identities` route returns `503` in that mode.
    pub vault: Option<VaultService>,
    /// Dev-only owner id stamped onto every created row until auth lands.
    /// `None` when `dev_auth.enabled = false` (the shim is off but real
    /// auth has not yet been wired up); in that mode `DevUser` extractors
    /// return `401`. See [`dev_user`](crate::dev_user) for the full
    /// transition story.
    pub dev_user_id: Option<UserId>,
}

impl FromRef<AppState> for Option<UserId> {
    fn from_ref(state: &AppState) -> Self {
        state.dev_user_id
    }
}

/// Build the top-level router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(routes::health::router())
        .merge(routes::ws::router())
        .nest("/api/v1", routes::v1::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
