//! Cookie-backed `AuthenticatedUser` axum extractor.
//!
//! This is the foundation of the route-migration arc that replaces
//! [`crate::DevUser`]. Use it on a handler to require a valid,
//! non-revoked, non-expired session cookie. The extractor:
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
//! ## Scope of this slice
//!
//! Only the `/api/v1/auth/me` route consumes the extractor today. The
//! rest of the protected app routes (`hosts`, `server-profiles`,
//! `ssh-identities`, `terminal-sessions`, `audit-events`) keep using
//! [`crate::DevUser`] until the route-migration slice. SPEC.md
//! "Production authentication architecture → Implementation order"
//! step 7 owns that migration (this slice is step 5 — the extractor
//! itself, Phase A coexistence).
//!
//! ## What the extractor does NOT do
//!
//! * It does NOT touch `last_seen_at`. Stamping `last_seen_at` on every
//!   authenticated request is best-effort, error-tolerant, and a
//!   future slice — see SPEC.md "Auth extractor and route migration"
//!   for the deferred behaviour. `AuthService::validate_session_token`
//!   is a pure read for that reason.
//! * It does NOT enforce CSRF. The shared CSRF middleware is a
//!   separate slice (SPEC step 6); the auth-route inline `Origin`
//!   guard remains until then.
//! * It does NOT expose the session token, the token hash, or the
//!   session row to the handler. The handler receives the
//!   [`UserId`] and the [`User`] only.
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
use relayterm_core::repository::UserRepository;
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

        Ok(Self { user })
    }
}
