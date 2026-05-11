//! `/api/v1/config/session-policy` — read-only public session policy.
//!
//! ## Scope
//!
//! Exposes the deployment's effective detached-live-PTY TTL to the
//! authenticated frontend so production UX copy can stop hardcoding the
//! legacy `~30s` literal. The endpoint is intentionally
//! minimal — one field, one numeric value, no nested objects.
//!
//! ## Security posture
//!
//! - **Authenticated.** Takes [`AuthenticatedUser`] like every other
//!   protected `/api/v1` route.
//! - **GET only.** Pure read; no body extractor, no CSRF guard needed
//!   per the SPEC.md "CSRF posture" exemption for idempotent reads.
//! - **No secret-shaped fields.** The wire shape is constructed
//!   field-by-field from a single `u64` accessor on the orchestrator
//!   ([`TerminalSessionManager::detach_ttl`]); a future regression that
//!   tried to widen this surface with vault/cookie/CSRF/db config would
//!   have to type those fields into [`SessionPolicyResponse`] first,
//!   which the redaction sweep in
//!   `crates/relayterm-api/tests/api.rs::session_policy_*` pins.
//! - **Owner-scope is N/A.** Session policy is a deployment property,
//!   not a per-user resource — every authenticated caller sees the
//!   same value. This is consistent with how the SPA already treats
//!   `terminal_sessions.detached_live_pty_ttl_seconds` (operator-wide
//!   knob, not per-user).
//!
//! ## What this endpoint does NOT do
//!
//! - It does NOT carry any persistence claim. The fact that the
//!   detached PTY survives the TTL window is in-memory only; that
//!   disclaimer lives in the UI copy on top of this value, not on the
//!   wire. See `docs/persistent-sessions.md` § 11.1.
//! - It does NOT enable per-user quotas, 429 rejection, or operator
//!   dashboards. Those are Phase 1B/C deliverables, deliberately out
//!   of scope here.

use axum::{Json, Router, extract::State, routing::get};

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::dto::session_policy::SessionPolicyResponse;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/session-policy", get(session_policy))
}

/// `GET /api/v1/config/session-policy`.
///
/// Returns the deployment's effective detached-live-PTY TTL. The value
/// is read off the live orchestrator
/// ([`TerminalSessionManager::detach_ttl`]) rather than re-reading
/// config, so a future per-instance override or runtime adjustment
/// surface here without changing the route.
async fn session_policy(
    _user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Json<SessionPolicyResponse> {
    let detached_live_pty_ttl_seconds = state.terminal_sessions.detach_ttl().as_secs();
    Json(SessionPolicyResponse {
        detached_live_pty_ttl_seconds,
    })
}
