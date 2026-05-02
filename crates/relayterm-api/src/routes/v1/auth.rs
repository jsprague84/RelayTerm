//! `/api/v1/auth/*` — bootstrap, login, logout, current-user.
//!
//! The four routes here are the first real-auth surface (SPEC.md
//! "Production authentication architecture" → "Implementation order"
//! step 4). Existing app routes still go through [`crate::DevUser`];
//! production-auth enablement still fails fast at boot. As of step 5,
//! [`AuthenticatedUser`] has landed (`crate::auth`) and `GET /me` is
//! the first consumer; broad route migration is the next slice.
//!
//! ## Cookie
//!
//! Login mints a fresh session via [`relayterm_auth::AuthService`] and
//! sets `Set-Cookie: relayterm_session=<token>; HttpOnly; SameSite=Strict;
//! Path=/; Max-Age=<ttl>` (Secure / Domain governed by
//! [`AuthRoutesConfig`]). The plaintext token crosses the boundary in
//! the cookie ONLY — never in the response body, never in audit
//! payloads, never in logs. Logout writes a `Max-Age=0` cookie with the
//! same flags so a browser deletes the prior value.
//!
//! ## CSRF
//!
//! Every state-changing route in this module runs an inline `Origin`
//! guard before any DB or auth work. A missing or non-allowlisted
//! `Origin` returns 403 `csrf_origin_mismatch` per SPEC.md "CSRF
//! posture". GETs (`/auth/me`) are exempt — they are idempotent reads
//! and (when the shared middleware lands in step 6) GETs stay exempt
//! there too. The inline guard is removed in the same commit that
//! wires the shared middleware.
//!
//! ## Audit
//!
//! - successful bootstrap → `first_user_created` (`actor_id = new user`)
//! - bad bootstrap token / already bootstrapped → `login_failed`
//!   (`actor_id = NULL`, `payload.method = "bootstrap"`,
//!   `reason = "bad_token" | "already_bootstrapped"`)
//! - successful login → `login_succeeded` (`actor_id = user_id`)
//! - failed login (bad creds OR unknown email) → `login_failed`
//!   (`actor_id = NULL`, `reason = "bad_credentials"`)
//! - successful logout → `logout_succeeded` (`actor_id = user_id`)
//!
//! Audit payloads carry public metadata only. The
//! `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel test in
//! `crates/relayterm-api/tests/api.rs` is the redaction backstop.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{Duration, Utc};
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::ids::UserId;
use relayterm_core::repository::{
    AuditEventRepository, CreateAuditEvent, CreateUser, PasswordCredentialRepository,
    UserRepository,
};
use serde_json::json;
use zeroize::Zeroizing;

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::auth::cookie::{SESSION_COOKIE_NAME, extract_session_cookie};
use crate::dto::auth::{BootstrapRequest, LoginRequest, UserResponse};
use crate::error::ApiError;

/// Session TTL. Currently a constant; promotable to `AuthRoutesConfig`
/// the moment a deploy needs to tune it. SPEC.md "Session model" pins
/// 30 days as the v1 default.
const SESSION_TTL: Duration = Duration::days(30);

/// Cookie / Origin / bootstrap-token policy for the auth routes.
///
/// Held behind `Arc` on [`AppState::auth_routes`] so secret-shaped
/// fields are not cloned on every request and so `AppState` stays
/// cheap to clone. Constructed at boot from the typed [`AuthConfig`].
///
/// `Debug` is implemented manually so the bootstrap token never
/// reaches a log line — only its presence is rendered, mirroring the
/// `_set: bool` markers on `AuthConfig` / `VaultConfig`.
///
/// [`AuthConfig`]: ../../../../../apps/backend/src/config.rs
pub struct AuthRoutesConfig {
    /// `Set-Cookie` `Secure` flag. Mirrors `auth.cookie_secure`.
    pub cookie_secure: bool,
    /// Optional `Set-Cookie` `Domain` attribute. None means a
    /// host-only cookie (the default).
    pub cookie_domain: Option<String>,
    /// Allow-listed `Origin` values for the inline CSRF guard. Empty
    /// means every `POST /auth/*` is rejected — that is the secure
    /// default; tests and dev environments must populate it
    /// explicitly.
    pub allowed_origins: Vec<String>,
    /// Bootstrap token configured at boot. `None` means the bootstrap
    /// route is disabled (returns 503). The plaintext is held in
    /// `Zeroizing<String>` so the heap copy wipes itself on drop and
    /// the `Debug` impl renders it as `_set: bool` only.
    pub bootstrap_token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for AuthRoutesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRoutesConfig")
            .field("cookie_secure", &self.cookie_secure)
            .field("cookie_domain", &self.cookie_domain)
            .field("allowed_origins", &self.allowed_origins)
            .field("bootstrap_token_set", &self.bootstrap_token.is_some())
            .finish()
    }
}

impl AuthRoutesConfig {
    /// Inline CSRF Origin guard. Called before any DB or auth work on
    /// every state-changing auth route.
    ///
    /// Policy (matches SPEC.md "CSRF posture"):
    /// * Missing `Origin` → 403 `csrf_origin_mismatch`.
    /// * `Origin` not in `allowed_origins` → 403 `csrf_origin_mismatch`.
    /// * Match → continue.
    ///
    /// Empty `allowed_origins` rejects every write. That is the secure
    /// default; tests / dev set the allow-list explicitly.
    pub(crate) fn check_origin(&self, headers: &HeaderMap) -> Result<(), ApiError> {
        let Some(value) = headers.get(header::ORIGIN) else {
            return Err(ApiError::CsrfOriginMismatch(
                "missing Origin header".to_owned(),
            ));
        };
        let Ok(origin) = value.to_str() else {
            return Err(ApiError::CsrfOriginMismatch(
                "Origin header is not valid UTF-8".to_owned(),
            ));
        };
        if !self.allowed_origins.iter().any(|allowed| allowed == origin) {
            return Err(ApiError::CsrfOriginMismatch(
                "Origin not in allowed_origins".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/bootstrap", post(bootstrap))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
}

// ---------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------

/// `POST /api/v1/auth/bootstrap`.
///
/// First-user creation. Refuses every call after the first user with a
/// password row exists (the dev fixture user has no password row, so
/// bootstrap is unaffected by it — SPEC.md "User model and first-user
/// bootstrap"). Does NOT mint a session; the SPA calls `/auth/login`
/// next.
async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BootstrapRequest>,
) -> Result<(StatusCode, Json<UserResponse>), ApiError> {
    state.auth_routes.check_origin(&headers)?;
    let req = req.validated()?;

    // Per SPEC: failure modes (bad token, already-bootstrapped) emit
    // `login_failed` with a NULL actor and method = "bootstrap". Audit
    // failures on these probe paths are best-effort — the operator
    // signal is the structural 401/409 we return, and a transient DB
    // failure on the audit append should not turn a 401 into a 500.

    let configured_token = state.auth_routes.bootstrap_token.as_ref().ok_or_else(|| {
        // Disabled / unconfigured: treat as a plain service-unavailable.
        // We deliberately do NOT write a `login_failed` row here —
        // there is no token to compare against, so there is nothing
        // to log as misuse.
        ApiError::ServiceUnavailable(
            "bootstrap is disabled (no first_user_bootstrap_token configured)".to_owned(),
        )
    })?;

    if !constant_time_eq(req.bootstrap_token.as_bytes(), configured_token.as_bytes()) {
        write_audit_best_effort(
            &state,
            None,
            AuditEventKind::LoginFailed,
            json!({"method": "bootstrap", "reason": "bad_token"}),
        )
        .await;
        return Err(ApiError::Unauthorized("bad bootstrap token".to_owned()));
    }

    if state.db.password_credentials().any_exists().await? {
        write_audit_best_effort(
            &state,
            None,
            AuditEventKind::LoginFailed,
            json!({"method": "bootstrap", "reason": "already_bootstrapped"}),
        )
        .await;
        return Err(ApiError::Conflict {
            entity: "user",
            reason: Some("already_bootstrapped"),
        });
    }

    let user = state
        .db
        .users()
        .create(CreateUser {
            email: req.email.clone(),
            display_name: req.display_name.clone(),
        })
        .await?;
    state.auth.set_password(user.id, &req.password).await?;

    // Successful bootstrap audit. Failure here mirrors the partial-
    // success shape `create_session` keeps: the user row + password
    // row are already written; surfacing the audit failure to the
    // caller matches the documented policy in
    // `routes/v1/server_profiles.rs::write_lifecycle_audit`. Audit
    // gaps are worse than orphan rows because they cannot be
    // reconstructed.
    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(user.id),
            kind: AuditEventKind::FirstUserCreated,
            payload: json!({
                "user_id": user.id,
                "created_at": user.created_at,
            }),
            remote_addr: None,
        })
        .await?;

    Ok((StatusCode::CREATED, Json(user.into())))
}

// ---------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------

/// `POST /api/v1/auth/login`.
///
/// Verifies the password and mints a fresh session cookie. Wrong-
/// password / unknown-email collapse to the same 401 + the same
/// `login_failed` audit row so a probe cannot distinguish the two.
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    state.auth_routes.check_origin(&headers)?;
    let req = req.validated()?;

    // Look up the user by email. A miss is collapsed to the same
    // `InvalidCredentials` path a wrong-password verify would take —
    // a probe can't distinguish "no such user" from "wrong password"
    // via the wire response OR the audit row.
    let user = state.db.users().get_by_email(&req.email).await?;

    // Call the hasher on BOTH branches so an attacker cannot time-
    // side-channel which emails exist. Without this, the unknown-email
    // path skips Argon2id entirely and returns ~150 ms faster than the
    // wrong-password path, which leaks the membership of the `users`
    // table. See SPEC.md "Password authentication (v1)" probe-
    // resistance contract.
    let verify_result = match user.as_ref() {
        Some(u) => state.auth.verify_password(u.id, &req.password).await,
        None => {
            state.auth.anti_timing_verify(&req.password);
            Err(relayterm_auth::AuthServiceError::InvalidCredentials)
        }
    };

    if let Err(err) = verify_result {
        // Any auth-service failure on this path is treated as
        // bad-credentials at the wire layer. We deliberately drop
        // structural detail (Repository / Crypto) to keep the probe-
        // resistance contract; a non-bad-credentials shape is logged
        // operator-side via the existing warn!/error! in
        // `IntoResponse`, but the wire body is the static
        // `unauthorized`.
        let api_err = match err {
            relayterm_auth::AuthServiceError::InvalidCredentials => {
                ApiError::Unauthorized("invalid credentials".to_owned())
            }
            other => ApiError::from(other),
        };
        // login_failed audit. Failure to record the audit row here is
        // best-effort — a transient DB failure on the audit append
        // should not turn a 401 into a 500.
        write_audit_best_effort(
            &state,
            None,
            AuditEventKind::LoginFailed,
            json!({"method": "password", "reason": "bad_credentials"}),
        )
        .await;
        return Err(api_err);
    }

    let user = user.expect("user present after verify_password ok");

    let now = Utc::now();
    let created = state.auth.create_session(user.id, SESSION_TTL, now).await?;

    // Best-effort `last_login_at` update. A failure here is logged but
    // does NOT fail the request — the session is already minted, the
    // audit row will land below, and the column is purely display
    // metadata. Mirrors the SPEC posture for `last_seen_at` on the
    // future auth extractor.
    if let Err(err) = state.db.users().touch_last_login(user.id, now).await {
        tracing::warn!(error = %err, "touch_last_login failed; ignoring");
    }

    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(user.id),
            kind: AuditEventKind::LoginSucceeded,
            payload: json!({
                "user_id": user.id,
                "method": "password",
                "login_at": now,
            }),
            remote_addr: None,
        })
        .await?;

    let cookie = build_session_cookie(created.token.expose(), &state.auth_routes);
    // Drop the wrapper before building the response — the cookie
    // string holds the only remaining copy and the `Set-Cookie`
    // header writer is the single legitimate sink. `created.token`
    // zeroizes on drop.
    drop(created);

    let body = Json(UserResponse::from(user));
    let response = (StatusCode::OK, [(header::SET_COOKIE, cookie)], body).into_response();
    Ok(response)
}

// ---------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------

/// `POST /api/v1/auth/logout`.
///
/// Idempotent from the user's perspective: missing / unknown / already-
/// revoked cookies all return 204 with a clear-cookie header. The
/// `logout_succeeded` audit row is appended only on a real revocation
/// transition so the audit feed reflects intent, not noise.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    state.auth_routes.check_origin(&headers)?;

    // Always-clear cookie. Built first so the success / no-op paths
    // share the same exit shape.
    let clear_cookie = build_clear_cookie(&state.auth_routes);

    let Some(token) = extract_session_cookie(&headers) else {
        return Ok(no_content_with_cookie(clear_cookie));
    };

    let now = Utc::now();
    let session = match state.auth.validate_session_token(token, now).await {
        Ok(s) => s,
        Err(_) => {
            // Unknown / expired / revoked all collapse to the same
            // "clear cookie, return 204" path. No audit row — the
            // user did not just log out anything that wasn't already
            // gone, and a "logout of a revoked session" event would
            // be operator noise.
            return Ok(no_content_with_cookie(clear_cookie));
        }
    };

    // Revoke. A re-revoke against an already-revoked row is a no-op
    // at the repository (the original `revoked_at` is preserved).
    // SessionInvalid would mean the row vanished between validate and
    // revoke — extremely unlikely, treat as a real no-op rather than
    // a 500.
    if let Err(err) = state
        .auth
        .revoke_session(session.id, now, Some("logout"))
        .await
    {
        match err {
            relayterm_auth::AuthServiceError::SessionInvalid => {
                return Ok(no_content_with_cookie(clear_cookie));
            }
            other => return Err(other.into()),
        }
    }

    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(session.user_id),
            kind: AuditEventKind::LogoutSucceeded,
            payload: json!({
                "user_id": session.user_id,
                "session_id": session.id,
                "logout_at": now,
            }),
            remote_addr: None,
        })
        .await?;

    Ok(no_content_with_cookie(clear_cookie))
}

// ---------------------------------------------------------------------
// /me
// ---------------------------------------------------------------------

/// `GET /api/v1/auth/me`.
///
/// Returns the safe DTO of the user that owns the session cookie. No
/// cookie / expired / revoked / unknown all collapse to the same 401
/// `unauthorized` body — the operator-side detail (`session invalid`
/// vs `session expired` vs `missing cookie`) lives in the existing
/// `warn!` line in `error.rs::IntoResponse`.
///
/// First production consumer of [`AuthenticatedUser`]. The remaining
/// protected app routes (`hosts`, `server-profiles`, `ssh-identities`,
/// `terminal-sessions`, `audit-events`) still go through
/// [`crate::DevUser`] until the route-migration slice — SPEC.md
/// "Production authentication architecture → Implementation order"
/// step 7.
async fn me(user: AuthenticatedUser) -> Json<UserResponse> {
    Json(user.into_user().into())
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// Best-effort audit append. A transient DB failure here is logged at
/// `warn!` but does NOT fail the request — the structural 401/409
/// already conveyed the auth outcome. Used only on the failure paths
/// of bootstrap / login.
async fn write_audit_best_effort(
    state: &AppState,
    actor_id: Option<UserId>,
    kind: AuditEventKind,
    payload: serde_json::Value,
) {
    if let Err(err) = state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id,
            kind,
            payload,
            remote_addr: None,
        })
        .await
    {
        tracing::warn!(error = %err, kind = ?kind, "best-effort auth audit append failed");
    }
}

fn no_content_with_cookie(cookie: String) -> Response {
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

/// Build the `Set-Cookie` header value for a fresh session.
///
/// Only the `Set-Cookie` writer (this function) consumes the plaintext
/// token. Any other caller of [`SessionToken::expose`] is a redaction
/// regression — push the requirement up to [`SessionTokenHash`]
/// instead. See AGENTS.md "Don't ... stash, log, or pass-around the
/// plaintext value of a `SessionToken`".
fn build_session_cookie(token: &str, cfg: &Arc<AuthRoutesConfig>) -> String {
    let mut s = format!(
        "{SESSION_COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL.num_seconds(),
    );
    if cfg.cookie_secure {
        s.push_str("; Secure");
    }
    if let Some(d) = &cfg.cookie_domain {
        s.push_str("; Domain=");
        s.push_str(d);
    }
    s
}

fn build_clear_cookie(cfg: &Arc<AuthRoutesConfig>) -> String {
    let mut s = format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0",);
    if cfg.cookie_secure {
        s.push_str("; Secure");
    }
    if let Some(d) = &cfg.cookie_domain {
        s.push_str("; Domain=");
        s.push_str(d);
    }
    s
}

/// Constant-time byte-compare. Prevents a timing-side-channel on the
/// bootstrap-token check: `==` short-circuits at the first mismatch,
/// which is enough signal for an attacker to learn one byte at a time.
///
/// `core::hint::black_box` wraps the accumulator after the loop so a
/// future compiler optimisation cannot silently introduce a short-
/// circuit on `diff != 0`. The bootstrap route runs at most once in
/// production and the configured token is an operator secret, so the
/// length-mismatch leak (a stranger token of a different length
/// returns slightly faster) is acceptable; if it ever needs to close,
/// pad both sides to a fixed maximum and run the loop unconditionally.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    core::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn cfg(allowed: &[&str], bootstrap_token: Option<&str>) -> Arc<AuthRoutesConfig> {
        Arc::new(AuthRoutesConfig {
            cookie_secure: true,
            cookie_domain: None,
            allowed_origins: allowed.iter().map(|s| (*s).to_owned()).collect(),
            bootstrap_token: bootstrap_token.map(|t| Zeroizing::new(t.to_owned())),
        })
    }

    fn headers_with_origin(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn origin_guard_allows_match() {
        let c = cfg(&["https://relay.example.com"], None);
        c.check_origin(&headers_with_origin("https://relay.example.com"))
            .unwrap();
    }

    #[test]
    fn origin_guard_rejects_mismatch() {
        let c = cfg(&["https://relay.example.com"], None);
        let err = c
            .check_origin(&headers_with_origin("https://evil.example.com"))
            .unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn origin_guard_rejects_missing_origin() {
        let c = cfg(&["https://relay.example.com"], None);
        let err = c.check_origin(&HeaderMap::new()).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn origin_guard_rejects_when_allowlist_empty() {
        let c = cfg(&[], None);
        let err = c
            .check_origin(&headers_with_origin("https://relay.example.com"))
            .unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn origin_guard_rejects_non_utf8_origin() {
        let c = cfg(&["https://relay.example.com"], None);
        let mut h = HeaderMap::new();
        // Non-UTF-8 byte triggers `to_str().is_err()`.
        h.insert(
            header::ORIGIN,
            HeaderValue::from_bytes(&[0xff, 0xfe, 0xfd]).unwrap(),
        );
        let err = c.check_origin(&h).unwrap_err();
        assert!(matches!(err, ApiError::CsrfOriginMismatch(_)));
    }

    #[test]
    fn debug_redacts_bootstrap_token() {
        let secret = "AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA";
        let c = cfg(&[], Some(secret));
        let dbg = format!("{c:?}");
        assert!(!dbg.contains(secret));
        assert!(dbg.contains("bootstrap_token_set: true"));
    }

    #[test]
    fn session_cookie_is_strict_httponly_and_path_root() {
        let c = cfg(&[], None);
        let cookie = build_session_cookie("token-marker", &c);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=2592000"));
        // Secure flag mirrors cfg.cookie_secure (true above).
        assert!(cookie.contains("Secure"));
        // Token value crosses the boundary intact.
        assert!(cookie.starts_with("relayterm_session=token-marker;"));
    }

    #[test]
    fn session_cookie_omits_secure_when_dev_insecure() {
        let mut inner = AuthRoutesConfig {
            cookie_secure: false,
            cookie_domain: None,
            allowed_origins: Vec::new(),
            bootstrap_token: None,
        };
        inner.cookie_domain = Some("relay.example.com".to_owned());
        let c = Arc::new(inner);
        let cookie = build_session_cookie("token-marker", &c);
        assert!(!cookie.contains("Secure"));
        assert!(cookie.contains("Domain=relay.example.com"));
    }

    #[test]
    fn clear_cookie_zeros_max_age() {
        let c = cfg(&[], None);
        let cookie = build_clear_cookie(&c);
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.starts_with("relayterm_session=;"));
    }

    #[test]
    fn constant_time_eq_matches_for_equal_inputs() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_rejects_unequal_lengths() {
        assert!(!constant_time_eq(b"hello", b"hello!"));
    }

    #[test]
    fn constant_time_eq_rejects_different_bytes() {
        assert!(!constant_time_eq(b"hello", b"hellp"));
    }
}
