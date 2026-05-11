//! `/api/v1` routes.
//!
//! Handlers in this tree are intentionally thin: parse, validate, call a
//! repository, map the result. Business logic lives below this layer (in
//! services to come) — never here.

use axum::Router;

use crate::AppState;

mod audit_events;
pub(crate) mod auth;
mod hosts;
mod server_profiles;
mod session_policy;
mod ssh_identities;
mod terminal_recordings;
mod terminal_sessions;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .nest("/audit-events", audit_events::router())
        .nest("/auth", auth::router())
        .nest("/config", session_policy::router())
        .nest("/hosts", hosts::router())
        .nest("/server-profiles", server_profiles::router())
        .nest("/ssh-identities", ssh_identities::router())
        .nest(
            "/terminal-sessions",
            terminal_sessions::router().merge(terminal_recordings::router()),
        )
}
