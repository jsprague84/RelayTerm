//! Server-side opaque browser session record.
//!
//! Each row represents one issued session. The cookie value is a 32-byte
//! random token produced by the auth service; only its SHA-256 digest is
//! persisted as `token_hash`. Plaintext tokens MUST NEVER be modeled at
//! this layer — the auth service hashes the token before any repository
//! call.
//!
//! `Debug` is implemented manually so [`Self::token_hash`] never appears
//! in tracing logs, panic messages, or any other formatter output. Even
//! though the hash is one-way, the project treats it as sensitive: a
//! database dump that leaks the hash plus a network capture of the
//! plaintext token would still be a session-takeover.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{UserId, UserSessionId};

/// A persisted browser session.
///
/// `id` is the stable session identifier referenced by audit-event
/// payloads — it is NOT the cookie value.
///
/// `Serialize`/`Deserialize` are derived for symmetry with the other
/// domain records, but `token_hash` is `#[serde(skip)]` so a stray
/// `serde_json::to_value(&session)` cannot leak the digest onto the
/// wire. The hash is reachable only through the field directly — any
/// wire surface that needs the digest does so via
/// [`crate::repository::CreateUserSession`] (and even that lookup
/// happens by digest, not by structural serde).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSession {
    pub id: UserSessionId,
    pub user_id: UserId,
    /// SHA-256 digest of the random cookie token. Treated as sensitive.
    #[serde(skip)]
    pub token_hash: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// Short, free-form reason recorded on revocation (e.g. `"logout"`,
    /// `"admin_revoke"`). Display metadata only — never used as an auth
    /// input.
    pub revoked_reason: Option<String>,
}

impl UserSession {
    /// True when the session is past its hard expiry.
    #[must_use]
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }

    /// True when the session has been explicitly revoked.
    #[must_use]
    pub const fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// True when the session is usable: not expired AND not revoked.
    #[must_use]
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        !self.is_expired_at(now) && !self.is_revoked()
    }
}

impl fmt::Debug for UserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserSession")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field(
                "token_hash",
                &format_args!("<redacted: {} bytes>", self.token_hash.len()),
            )
            .field("created_at", &self.created_at)
            .field("last_seen_at", &self.last_seen_at)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .field("revoked_reason", &self.revoked_reason)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // 32 distinctive bytes — looks nothing like a normal int sequence so a
    // formatter that printed `Vec<u8>` element-wise would still be caught.
    const SENTINEL_HASH: [u8; 32] = [
        0x53, 0x55, 0x50, 0x45, 0x52, 0x53, 0x45, 0x43, 0x52, 0x45, 0x54, 0x53, 0x45, 0x53, 0x53,
        0x49, 0x4f, 0x4e, 0x54, 0x4f, 0x4b, 0x45, 0x4e, 0x44, 0x49, 0x47, 0x45, 0x53, 0x54, 0x21,
        0x21, 0x21,
    ];

    fn fixture() -> UserSession {
        let now = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        UserSession {
            id: UserSessionId::new(),
            user_id: UserId::new(),
            token_hash: SENTINEL_HASH.to_vec(),
            created_at: now,
            last_seen_at: now,
            expires_at: now + chrono::Duration::days(30),
            revoked_at: None,
            revoked_reason: None,
        }
    }

    #[test]
    fn debug_redacts_token_hash() {
        let session = fixture();
        let formatted = format!("{session:?}");
        // The first byte (`0x53`) repeated as a decimal element ("83")
        // would appear in any naive Vec<u8> Debug. Likewise the printable
        // prefix "SUPERSECRET..." bytes if reinterpreted as ASCII.
        assert!(
            !formatted.contains("83, 85, 80"),
            "UserSession Debug must not echo token_hash bytes"
        );
        assert!(
            !formatted.contains("SUPERSECRET"),
            "UserSession Debug must not echo token_hash interpreted as ASCII"
        );
        assert!(
            formatted.contains("redacted"),
            "UserSession Debug must label the redaction"
        );
        // Every other field is allowed to render normally.
        assert!(formatted.contains("expires_at"));
    }

    #[test]
    fn user_session_id_round_trips_uuid() {
        let raw = uuid::Uuid::new_v4();
        let id = UserSessionId::from_uuid(raw);
        assert_eq!(id.into_uuid(), raw);
        assert_eq!(*id.as_uuid(), raw);
    }

    #[test]
    fn serde_skips_token_hash() {
        let session = fixture();
        let json = serde_json::to_string(&session).expect("serialize");
        assert!(
            !json.contains("token_hash"),
            "Serialize must not emit the token_hash field at all"
        );
        // Reject any rendering of the bytes we used in the fixture, in
        // both raw-byte-array form and ASCII reinterpretation.
        assert!(
            !json.contains("83,85,80") && !json.contains("83, 85, 80"),
            "Serialize must not echo token_hash bytes"
        );
        assert!(
            !json.contains("SUPERSECRET"),
            "Serialize must not echo token_hash bytes interpreted as ASCII"
        );
    }

    #[test]
    fn lifecycle_helpers_match_fields() {
        let now = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let mut session = fixture();
        session.expires_at = now + chrono::Duration::seconds(60);

        assert!(session.is_active_at(now));
        assert!(!session.is_expired_at(now));
        assert!(!session.is_revoked());

        // Past expiry → inactive.
        let later = now + chrono::Duration::seconds(120);
        assert!(session.is_expired_at(later));
        assert!(!session.is_active_at(later));

        // Revoked → inactive even before expiry.
        session.revoked_at = Some(now);
        session.revoked_reason = Some("logout".to_owned());
        assert!(session.is_revoked());
        assert!(!session.is_active_at(now));
    }
}
