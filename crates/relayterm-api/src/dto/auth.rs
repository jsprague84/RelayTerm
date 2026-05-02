//! Request / response DTOs for the `/api/v1/auth/*` routes.
//!
//! The DTOs are the redaction backstop on the HTTP boundary: every secret
//! input (`password`, `bootstrap_token`) is `Debug`-redacted, never serde-
//! serializable, and `Drop`-zeroized via the field type. The current-user
//! response shape is hand-rolled (NOT [`relayterm_core::user::User`]) so
//! a future column on the domain type cannot silently widen the wire DTO.
//!
//! Validation lives here: length bounds for password / display name /
//! email, plus a minimal email shape check. The bounds are intentionally
//! generous on the upper end and strict on the lower — the upper bound's
//! purpose is DoS protection (a 10MB password would melt Argon2id), not
//! aesthetic policy.

use std::fmt;

use chrono::{DateTime, Utc};
use relayterm_core::ids::{UserId, UserSessionId};
use relayterm_core::user::User;
use relayterm_core::user_session::UserSession;
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

use crate::error::ApiError;

/// Deserialize a JSON string straight into a `Zeroizing<String>` so the
/// heap copy wipes itself on drop. The raw `String` borrow that serde
/// emits is consumed before the closure returns; the wrapped value is
/// the only long-lived copy.
fn deserialize_zeroizing_string<'de, D>(de: D) -> Result<Zeroizing<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(de)?;
    Ok(Zeroizing::new(raw))
}

/// Inclusive minimum password length. Set to 12 per SPEC.md "Password
/// authentication (v1)" — a length floor + Argon2id is more effective
/// than complexity rules.
pub(crate) const PASSWORD_MIN_LEN: usize = 12;
/// Inclusive maximum password length. Bounds the work the hasher will
/// do on a single request so a malicious 10MB submission cannot hold a
/// hash thread for minutes.
pub(crate) const PASSWORD_MAX_LEN: usize = 1024;
/// Inclusive maximum email length. RFC 5321 caps the local-part at 64
/// and the full address at 254; we allow a little headroom for fringe
/// servers that ignore the cap, while still bounding the input.
pub(crate) const EMAIL_MAX_LEN: usize = 320;
pub(crate) const DISPLAY_NAME_MAX_LEN: usize = 200;
pub(crate) const BOOTSTRAP_TOKEN_MAX_LEN: usize = 4096;

/// Bootstrap request body.
///
/// `bootstrap_token` and `password` are wrapped in `Zeroizing<String>` so
/// the heap copies wipe themselves when this DTO drops. `Debug` redacts
/// both fields to length-only markers; serde re-deserialization is the
/// only legitimate writer.
#[derive(Deserialize)]
pub(crate) struct BootstrapRequest {
    #[serde(deserialize_with = "deserialize_zeroizing_string")]
    pub(crate) bootstrap_token: Zeroizing<String>,
    pub(crate) email: String,
    pub(crate) display_name: String,
    #[serde(deserialize_with = "deserialize_zeroizing_string")]
    pub(crate) password: Zeroizing<String>,
}

impl fmt::Debug for BootstrapRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootstrapRequest")
            .field(
                "bootstrap_token",
                &format_args!("<redacted: {} chars>", self.bootstrap_token.len()),
            )
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field(
                "password",
                &format_args!("<redacted: {} chars>", self.password.len()),
            )
            .finish()
    }
}

impl BootstrapRequest {
    /// Validate the bounds on every field. Failure produces a generic
    /// `invalid input` 400 — the message NEVER echoes the offered
    /// bootstrap token, password, or email value. Operator logs see the
    /// same shape; a probe cannot use the response to learn anything
    /// about the token's true length or the email's exact form.
    pub(crate) fn validated(self) -> Result<Self, ApiError> {
        validate_bootstrap_token_len(&self.bootstrap_token)?;
        validate_email(&self.email)?;
        validate_display_name(&self.display_name)?;
        validate_password(&self.password)?;
        Ok(self)
    }
}

/// Login request body. Only `password` is sensitive at this stage —
/// `email` becomes a `users.email` lookup but is not itself secret.
#[derive(Deserialize)]
pub(crate) struct LoginRequest {
    pub(crate) email: String,
    #[serde(deserialize_with = "deserialize_zeroizing_string")]
    pub(crate) password: Zeroizing<String>,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginRequest")
            .field("email", &self.email)
            .field(
                "password",
                &format_args!("<redacted: {} chars>", self.password.len()),
            )
            .finish()
    }
}

impl LoginRequest {
    pub(crate) fn validated(self) -> Result<Self, ApiError> {
        validate_email(&self.email)?;
        validate_password(&self.password)?;
        Ok(self)
    }
}

/// Wire shape for a user record. Hand-rolled so a future column on
/// [`User`] cannot widen the wire surface by accident.
#[derive(Debug, Serialize)]
pub(crate) struct UserResponse {
    pub(crate) id: UserId,
    pub(crate) email: String,
    pub(crate) display_name: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) last_login_at: Option<DateTime<Utc>>,
}

impl From<User> for UserResponse {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            email: user.email,
            display_name: user.display_name,
            created_at: user.created_at,
            last_login_at: user.last_login_at,
        }
    }
}

/// Stable presentation status for a session row in the listing
/// response.
///
/// Hand-rolled (not [`UserSession`]) so the wire surface is fixed:
/// `active` / `expired` / `revoked`. The route layer collapses a
/// "revoked AND expired" row into `revoked` because revocation is the
/// deliberate-action signal and expiry is a passive timestamp; this
/// matches the `validate_session_token` priority pinned by
/// `service::validate_session_token_prefers_revoked_over_expired`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionStatus {
    Active,
    Expired,
    Revoked,
}

/// Wire shape for one session in the current-user list response.
///
/// **Token-redacted.** This DTO deliberately omits `token_hash` and
/// has no plaintext-token field — the only public reference to a
/// session is its `id`, the same one that appears on
/// [`UserSession`](relayterm_core::user_session::UserSession) and in
/// audit-event payloads. `serde_skip` on `UserSession::token_hash` is
/// the redaction backstop one layer down; this DTO is hand-rolled so
/// a future field on the domain row cannot widen the wire surface by
/// accident. Audit-redaction sentinel tests in
/// `crates/relayterm-api/tests/api.rs` pin this contract.
#[derive(Debug, Serialize)]
pub(crate) struct SessionListItem {
    pub(crate) id: UserSessionId,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) last_seen_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) revoked_at: Option<DateTime<Utc>>,
    /// `true` when this row is the session that authenticated the
    /// request producing the listing — i.e. the cookie the caller is
    /// holding right now.
    pub(crate) current: bool,
    pub(crate) status: SessionStatus,
}

impl SessionListItem {
    /// Build a listing item from a domain [`UserSession`] row.
    ///
    /// Status is derived from `(revoked_at, now, expires_at)` with
    /// `revoked` winning over `expired` when both are true. `current`
    /// is supplied by the caller — the DTO does not know which
    /// session id the request came in on.
    pub(crate) fn from_row(
        row: &UserSession,
        now: DateTime<Utc>,
        current_id: UserSessionId,
    ) -> Self {
        let status = if row.revoked_at.is_some() {
            SessionStatus::Revoked
        } else if row.is_expired_at(now) {
            SessionStatus::Expired
        } else {
            SessionStatus::Active
        };
        Self {
            id: row.id,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
            current: row.id == current_id,
            status,
        }
    }
}

/// Wire envelope for the session list. A struct (not a bare array)
/// so a future addition (page cursor, total count) does not break
/// existing clients.
#[derive(Debug, Serialize)]
pub(crate) struct SessionsListResponse {
    pub(crate) sessions: Vec<SessionListItem>,
}

/// Wire shape for the `revoke-all-except-current` response.
///
/// Carries only the count — never per-row session ids — so the audit
/// row and the wire response stay byte-aligned and a future caller
/// can't accidentally tee per-row information out of the response
/// envelope. The count is the number of rows transitioned from
/// non-revoked to revoked; an idempotent re-call returns `0`.
#[derive(Debug, Serialize)]
pub(crate) struct RevokeAllExceptCurrentResponse {
    pub(crate) revoked_count: u64,
}

fn validate_password(value: &str) -> Result<(), ApiError> {
    if value.len() < PASSWORD_MIN_LEN || value.len() > PASSWORD_MAX_LEN {
        // The wire message intentionally names the rule, not the
        // offered value. A probe cannot learn the offered length from
        // the response (`<min` and `>max` collapse to one string).
        return Err(ApiError::Validation(format!(
            "password must be between {PASSWORD_MIN_LEN} and {PASSWORD_MAX_LEN} chars"
        )));
    }
    Ok(())
}

fn validate_email(value: &str) -> Result<(), ApiError> {
    let trimmed_len = value.len();
    if trimmed_len == 0 || trimmed_len > EMAIL_MAX_LEN {
        return Err(ApiError::Validation("email is invalid".to_owned()));
    }
    // Cheapest possible "looks like an email" gate. A formal RFC-5322
    // parser is beyond scope — the v1 surface is single-tenant and the
    // operator picks the email; we are bounding shape, not asserting
    // deliverability.
    let at_count = value.bytes().filter(|b| *b == b'@').count();
    if at_count != 1 || value.starts_with('@') || value.ends_with('@') {
        return Err(ApiError::Validation("email is invalid".to_owned()));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<(), ApiError> {
    if value.is_empty() || value.len() > DISPLAY_NAME_MAX_LEN {
        return Err(ApiError::Validation(format!(
            "display_name must be between 1 and {DISPLAY_NAME_MAX_LEN} chars"
        )));
    }
    Ok(())
}

fn validate_bootstrap_token_len(value: &str) -> Result<(), ApiError> {
    if value.is_empty() || value.len() > BOOTSTRAP_TOKEN_MAX_LEN {
        // Same shape as the password rule — the wire message names the
        // category, never the offered bytes.
        return Err(ApiError::Validation(
            "bootstrap_token is invalid".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_request_debug_redacts_secrets() {
        let req = BootstrapRequest {
            bootstrap_token: Zeroizing::new("AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA".to_owned()),
            email: "first@example.com".to_owned(),
            display_name: "First".to_owned(),
            password: Zeroizing::new("AAAA-PASSWORD-MARKER-AAAA".to_owned()),
        };
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA"));
        assert!(!dbg.contains("AAAA-PASSWORD-MARKER-AAAA"));
        assert!(dbg.contains("redacted"));
        // Non-secret fields remain visible.
        assert!(dbg.contains("first@example.com"));
        assert!(dbg.contains("First"));
    }

    #[test]
    fn login_request_debug_redacts_password() {
        let req = LoginRequest {
            email: "user@example.com".to_owned(),
            password: Zeroizing::new("AAAA-PASSWORD-MARKER-AAAA".to_owned()),
        };
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("AAAA-PASSWORD-MARKER-AAAA"));
        assert!(dbg.contains("redacted"));
        assert!(dbg.contains("user@example.com"));
    }

    #[test]
    fn user_response_serialization_is_safe() {
        let resp = UserResponse {
            id: UserId::new(),
            email: "first@example.com".to_owned(),
            display_name: "First".to_owned(),
            created_at: Utc::now(),
            last_login_at: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        // No secret-shaped names allowed even if a future field name
        // collision could re-introduce them.
        for forbidden in [
            "password",
            "password_hash",
            "session_token",
            "token_hash",
            "bootstrap_token",
            "argon2id",
        ] {
            assert!(
                !json.contains(forbidden),
                "UserResponse must not serialize `{forbidden}`: {json}"
            );
        }
    }

    #[test]
    fn password_below_minimum_rejected() {
        let req = LoginRequest {
            email: "u@example.com".to_owned(),
            password: Zeroizing::new("short".to_owned()),
        };
        let err = req.validated().unwrap_err();
        assert!(matches!(err, ApiError::Validation(_)));
    }

    #[test]
    fn password_above_maximum_rejected() {
        let huge = "x".repeat(PASSWORD_MAX_LEN + 1);
        let req = LoginRequest {
            email: "u@example.com".to_owned(),
            password: Zeroizing::new(huge),
        };
        let err = req.validated().unwrap_err();
        assert!(matches!(err, ApiError::Validation(_)));
    }

    #[test]
    fn malformed_email_rejected() {
        for bad in [
            "",
            "no-at-sign",
            "@no-local",
            "no-domain@",
            "two@@signs.example.com",
        ] {
            let req = LoginRequest {
                email: bad.to_owned(),
                password: Zeroizing::new("password-meets-min".to_owned()),
            };
            let err = req.validated().unwrap_err();
            assert!(
                matches!(err, ApiError::Validation(_)),
                "expected validation error for `{bad}`",
            );
        }
    }

    #[test]
    fn validation_error_message_does_not_echo_bootstrap_token() {
        let secret_token = "AAAA-BOOTSTRAP-IN-ERROR-AAAA";
        let req = BootstrapRequest {
            bootstrap_token: Zeroizing::new(secret_token.to_owned()),
            email: "bad-email".to_owned(), // forces an error path
            display_name: "OK".to_owned(),
            password: Zeroizing::new("password-meets-min".to_owned()),
        };
        let err = req.validated().unwrap_err();
        let rendered = err.to_string();
        assert!(
            !rendered.contains(secret_token),
            "validation error must not echo the bootstrap token: {rendered}",
        );
    }
}
