//! Public session-policy DTO for the authenticated frontend.
//!
//! Exposes the SMALLEST useful subset of `terminal_sessions` deployment
//! config so the SPA can tell honest stories about how long a detached
//! session survives. The wire shape is intentionally minimal — adding a
//! field here is the same level of decision as adding a new public
//! `/api/v1` route.
//!
//! **What this DTO MUST NOT carry.** Vault internals, master keys,
//! cookie/session/CSRF policy, env names, secret-shaped strings,
//! deployment paths, database URLs, or anything sourced from
//! `relayterm-auth` / `relayterm-vault`. The redaction backstop is the
//! `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel sweep in
//! `crates/relayterm-api/tests/api.rs::session_policy_*` tests; any
//! widening of this shape must extend that sweep too.

use serde::Serialize;

/// Wire body of `GET /api/v1/config/session-policy`.
///
/// Carries only the effective detached-live-PTY TTL the orchestrator
/// is running with right now (after env / TOML / default merge). The
/// frontend uses it to format honest UX copy without hardcoding the
/// legacy `~30s` literal.
#[derive(Debug, Serialize)]
pub(crate) struct SessionPolicyResponse {
    /// Effective TTL (seconds) for the bounded detached-live-PTY
    /// reconnect window. Mirrors `terminal_sessions.detached_live_pty_ttl_seconds`
    /// (env `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`);
    /// bounded `5..=86_400` by the config validator. Persistence
    /// disclaimer (in-memory replay, no backend-restart survival)
    /// lives in the UI copy that consumes this value, not on the
    /// wire.
    pub detached_live_pty_ttl_seconds: u64,
}
