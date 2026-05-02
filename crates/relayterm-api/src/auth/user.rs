//! Cookie-backed `AuthenticatedUser` axum extractor.
//!
//! Use it on a handler to require a valid, non-revoked, non-expired
//! session cookie. The extractor:
//!
//! 1. Reads the `Cookie:` header via [`extract_session_cookie`].
//! 2. Hashes the plaintext token and validates it through
//!    [`AuthService::validate_session_token`].
//! 3. Loads the [`User`] row referenced by the session.
//!
//! Any failure on any step collapses to a single
//! [`ApiError::Unauthorized`]. The wire body is the static
//! `unauthorized` envelope — operator-side detail (`missing cookie` vs
//! `session invalid` vs `session expired` vs `session revoked` vs
//! `session references missing user`) survives in the `warn!` line in
//! [`crate::error::ApiError::IntoResponse`] and never reaches the
//! caller.
//!
//! ## What the extractor does
//!
//! * It DOES stamp `user_sessions.last_seen_at` on every successful
//!   extraction, best-effort and inline. The touch runs AFTER the
//!   session is validated AND the user row is loaded — so a missing /
//!   invalid / expired / revoked session, or a session whose user row
//!   is gone, never produces a `last_seen_at` write. A failed touch is
//!   logged at `warn!` (with the session id only — never the cookie,
//!   token hash, or repository internals) and the request still
//!   succeeds. `AuthService::validate_session_token` is kept a pure
//!   read so future routes that want a non-touching validation can
//!   call it directly. SPEC.md "Auth extractor and route migration"
//!   pins the best-effort posture.
//!
//! ## What the extractor does NOT do
//!
//! * It does NOT enforce CSRF. The shared CSRF middleware is a
//!   separate slice (SPEC step 6); the auth-route inline `Origin`
//!   guard remains until then.
//! * It does NOT expose the session token, the token hash, or the
//!   session row to the handler. The handler receives the
//!   [`UserId`] and the [`User`] only.
//! * It does NOT spawn a background task to stamp `last_seen_at`. The
//!   touch is awaited inline so a single failing repository call
//!   cannot accumulate orphaned futures, and so observability tooling
//!   that only sees the request span still captures the latency.
//!
//! ## Redaction posture
//!
//! The extractor never logs, embeds, or returns the cookie value or
//! the token hash. The only external sink is the `warn!` in
//! [`ApiError::IntoResponse`] which formats `ApiError::Unauthorized`'s
//! wrapped detail string — that string is written here, never derived
//! from the cookie bytes.

use axum::{
    extract::{FromRef, FromRequestParts},
    http::request::Parts,
};
use chrono::Utc;
use relayterm_core::ids::UserId;
use relayterm_core::repository::{UserRepository, UserSessionRepository};
use relayterm_core::user::User;

use crate::AppState;
use crate::error::ApiError;

use super::cookie::extract_session_cookie;

/// Authenticated identity for a request that must carry a valid
/// session cookie.
///
/// Obtain via the axum extractor:
///
/// ```ignore
/// async fn handler(user: AuthenticatedUser) -> Result<..., ApiError> {
///     let _: UserId = user.user_id();
///     let _: &User = user.user();
///     ...
/// }
/// ```
///
/// The wrapped [`User`] is loaded once at extraction time. A handler
/// that needs the freshest copy (e.g. after a self-edit) re-reads via
/// the repository — this struct deliberately does NOT auto-refresh.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    user: User,
}

impl AuthenticatedUser {
    /// The authenticated user's id. Equivalent to `self.user().id`,
    /// kept as a sugar so handlers that only need the id don't have to
    /// destructure the inner `User`.
    #[must_use]
    pub fn user_id(&self) -> UserId {
        self.user.id
    }

    /// Borrow the loaded [`User`] row.
    #[must_use]
    pub fn user(&self) -> &User {
        &self.user
    }

    /// Consume the extractor and return the owned [`User`] row. Used
    /// where the handler wants to map straight into a response DTO
    /// without an extra clone.
    #[must_use]
    pub fn into_user(self) -> User {
        self.user
    }
}

impl<S> FromRequestParts<S> for AuthenticatedUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        let token = extract_session_cookie(&parts.headers)
            .ok_or_else(|| ApiError::Unauthorized("missing session cookie".to_owned()))?;

        let now = Utc::now();
        let session = app_state.auth.validate_session_token(token, now).await?;

        let user = app_state
            .db
            .users()
            .get(session.user_id)
            .await?
            .ok_or_else(|| {
                // The session row references a user that no longer
                // exists. The DB FK should make this unreachable
                // (CASCADE drops the session when the user is
                // deleted), but if it ever happens we surface as 401
                // — the cookie is no longer authoritative.
                ApiError::Unauthorized("session references missing user".to_owned())
            })?;

        // Best-effort `last_seen_at` stamp. The touch runs only after
        // the session AND user row are confirmed valid — failed /
        // expired / revoked / missing-user paths above already
        // returned, so a row will never be touched outside the
        // happy-path. A repository failure here is logged at `warn!`
        // and the request still succeeds; SPEC.md "Auth extractor and
        // route migration" pins this posture. Logging the session id
        // is safe (it is the audit-event reference) — never the
        // cookie value, the token hash, or the repository internals.
        if let Err(err) = app_state
            .db
            .user_sessions()
            .touch_last_seen(session.id, now)
            .await
        {
            tracing::warn!(
                session_id = %session.id,
                error = %err,
                "touch_last_seen failed; ignoring",
            );
        }

        Ok(Self { user })
    }
}
