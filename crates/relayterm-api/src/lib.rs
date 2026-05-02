//! HTTP/WebSocket surface.
//!
//! Handlers are kept thin — they extract from axum, validate, and hand off
//! to a service in another crate. Auth and session orchestration are NOT
//! implemented at this layer; `dev_user` injects a stopgap [`UserId`] until
//! they are.

use std::sync::Arc;

use axum::{Router, extract::FromRef};
use relayterm_auth::AuthService;
use relayterm_core::ids::UserId;
use relayterm_db::Db;
use relayterm_ssh::{HostKeyPreflightService, SshAuthCheckService, SshPtyBridge};
use relayterm_terminal::TerminalSessionManager;
use relayterm_vault::VaultService;
use tower_http::trace::TraceLayer;

mod auth;
mod dev_user;
mod dto;
mod error;
mod routes;

pub use auth::{AuthenticatedUser, CsrfGuard};
pub use dev_user::DevUser;
pub use error::ApiError;
pub use routes::v1::auth::AuthRoutesConfig;

/// Shared state injected into every handler via `axum::extract::State`.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    /// Vault service used to generate and decrypt SSH identities. `None`
    /// means vault-backed identity creation is disabled — the
    /// `POST /api/v1/ssh-identities` route returns `503` in that mode.
    pub vault: Option<VaultService>,
    /// Host-key preflight service. Captures the server's host key during
    /// KEX and classifies it against the host's pinned `known_host_entries`
    /// rows. Wraps a probe (production: russh; tests: a fake) plus the
    /// pure classification logic. Held behind `Arc` so `AppState` stays
    /// `Clone` and the same probe instance is shared across handlers.
    ///
    /// **Scope**: this service does NOT validate SSH authentication or
    /// PTY readiness — see `HostKeyPreflightService` docs for the full
    /// "what it proves vs does not prove" list.
    pub preflight: Arc<HostKeyPreflightService>,
    /// Authenticated SSH credential check service. Verifies the host key
    /// is pinned and trusted, then attempts public-key auth and tears the
    /// connection down — no PTY, no shell, no commands. Held behind `Arc`
    /// for the same reason `preflight` is.
    ///
    /// **Scope**: this attests to host-key trust + public-key auth only.
    /// It does NOT validate that a PTY can be allocated, a shell can be
    /// spawned, or a session can be opened. See
    /// [`SshAuthCheckService`](relayterm_ssh::SshAuthCheckService) docs
    /// for the full "proves vs does not prove" contract.
    pub auth_check: Arc<SshAuthCheckService>,
    /// Live SSH PTY bridge. Production: the russh-backed implementation.
    /// Tests inject a fake bridge directly into `AppState` so the
    /// terminal-session create + WebSocket attach paths can be exercised
    /// without an SSH peer.
    ///
    /// **Scope**: this attests to the minimal interactive PTY path —
    /// connect, host-key trust, public-key auth, PTY/shell allocation,
    /// and `Input`/`Resize`/`Output` plumbing. It does NOT yet provide
    /// replay-buffer recovery across reconnects.
    pub pty_bridge: Arc<dyn SshPtyBridge>,
    /// Backend-owned terminal-session orchestrator. Owns the in-memory
    /// runtime registry and writes session metadata + lifecycle events
    /// to Postgres. Held behind `Arc` so `AppState` stays `Clone`.
    ///
    /// **Scope**: this slice only manages session lifecycle metadata —
    /// it does NOT open SSH channels, allocate PTYs, or stream terminal
    /// data. See [`TerminalSessionManager`] docs for the full contract.
    pub terminal_sessions: Arc<TerminalSessionManager>,
    /// Dev-only owner id stamped onto every created row until auth lands.
    /// `None` when `dev_auth.enabled = false` (the shim is off but real
    /// auth has not yet been wired up); in that mode `DevUser` extractors
    /// return `401`. See [`dev_user`](crate::dev_user) for the full
    /// transition story.
    ///
    /// **Scope today.** SPEC step 7 migrated every protected app route
    /// off `DevUser` onto [`AuthenticatedUser`], so this field no longer
    /// gates any production handler. It survives ONLY as a placeholder
    /// during the dev-auth retirement window — the field can be removed
    /// in a follow-up slice once the dev-auth shim itself is deleted.
    pub dev_user_id: Option<UserId>,
    /// Server-issued opaque session + password primitives. Used by every
    /// protected app route through the [`AuthenticatedUser`] extractor
    /// (`crate::auth`) and by the `/api/v1/auth/*` routes. Held behind
    /// `Arc` so `AppState` stays `Clone`.
    ///
    /// **Scope**: SPEC step 7 (this slice) wired the cookie-backed
    /// extractor onto every protected route — `hosts`,
    /// `ssh-identities`, `server-profiles`, `terminal-sessions`,
    /// `audit-events`, and the WebSocket attach route. Production-auth
    /// enablement still fails fast at boot until the frontend auth UI
    /// and DevUser retirement slices land.
    pub auth: Arc<AuthService>,
    /// Cookie / Origin / bootstrap-token policy for the auth routes.
    /// Shared via `Arc` so secret-shaped fields are not cloned on every
    /// request and so `AppState` stays cheap to clone.
    pub auth_routes: Arc<AuthRoutesConfig>,
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
        .nest("/api/v1", routes::v1::router())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
