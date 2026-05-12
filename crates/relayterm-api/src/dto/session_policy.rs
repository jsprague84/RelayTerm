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
/// is running with right now (after env / TOML / default merge) PLUS
/// (since Phase 1B.1) the per-user live-PTY ceiling AND (since
/// Phase 1B.2a) the per-user starting-burst ceiling so the SPA can
/// render parameterised refusal copy. The frontend uses these to
/// format honest UX copy without hardcoding the legacy `~30s` literal
/// or the default caps.
///
/// What this DTO MUST NOT carry (re-asserted alongside the per-route
/// doc-comment): the deployment-wide quota (operator-only,
/// fingerprinting risk), any session id / profile id / host id, any
/// secret-shaped string, any `RELAYTERM_` env name. The sentinel sweep
/// in `crates/relayterm-api/tests/api.rs::session_policy_*` is the
/// redaction backstop.
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
    /// Per-user live PTY ceiling (Phase 1B.1 quota, see
    /// `docs/session-quotas.md` § 4.1). Mirrors
    /// `terminal_sessions.max_live_pty_sessions_per_user` (env
    /// `RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER`);
    /// bounded `1..=256` by the config validator. Used by the SPA to
    /// parameterise the "you're at the limit of N sessions" copy on a
    /// `429 too_many_sessions` refusal. NOT a probe for the caller's
    /// current count — the count never crosses the wire.
    pub max_live_pty_sessions_per_user: u32,
    /// Per-user starting-burst ceiling (Phase 1B.2a quota, see
    /// `docs/session-quotas.md` § 4.3). Mirrors
    /// `terminal_sessions.max_starting_sessions_per_user` (env
    /// `RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER`);
    /// bounded `1..=32` by the config validator. Used by the SPA to
    /// parameterise the burst-refusal copy on a
    /// `429 too_many_starting_sessions` refusal. NOT a probe for the
    /// caller's current count — the count never crosses the wire.
    pub max_starting_sessions_per_user: u32,
}
