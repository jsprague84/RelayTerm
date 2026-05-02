//! Password credential record.
//!
//! One row per user with a password set. The stored value is an Argon2id
//! PHC string (`$argon2id$...`) produced by the auth service — never the
//! plaintext password, never an HMAC, never a non-PHC encoding. PHC
//! strings carry the parameters and salt inline so a future parameter
//! upgrade can verify the old hash before re-hashing on next login.
//!
//! `Debug` is implemented manually so [`Self::password_hash`] never
//! reaches tracing logs, panic messages, or any other formatter output.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::UserId;

/// A user's stored password material.
///
/// `Debug` redacts `password_hash`. `Serialize`/`Deserialize` are
/// derived for symmetry with the other domain records, but
/// `password_hash` is `#[serde(skip)]` so a stray
/// `serde_json::to_value(&credential)` cannot leak the hash bytes onto
/// the wire. The hash is reachable only through the field directly —
/// any wire surface that needs to MOVE password material (e.g. a
/// repository test fixture) does so via [`crate::repository::CreatePasswordCredential`],
/// not via serde on this type.
///
/// The plaintext password is never represented at this layer — it is
/// hashed by the auth service before any repository call.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordCredential {
    pub user_id: UserId,
    /// Argon2id PHC string. Treated as sensitive — exposing the hash
    /// enables an offline cracking attack against the per-user salt
    /// embedded in the string.
    #[serde(skip)]
    pub password_hash: String,
    pub password_changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for PasswordCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordCredential")
            .field("user_id", &self.user_id)
            .field(
                "password_hash",
                &format_args!("<redacted: {} chars>", self.password_hash.len()),
            )
            .field("password_changed_at", &self.password_changed_at)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SENTINEL: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHk$h4$h-do-not-leak-do-not-leak";

    fn fixture() -> PasswordCredential {
        let now = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        PasswordCredential {
            user_id: UserId::new(),
            password_hash: SENTINEL.to_owned(),
            password_changed_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn debug_redacts_password_hash() {
        let cred = fixture();
        let formatted = format!("{cred:?}");
        assert!(
            !formatted.contains(SENTINEL),
            "PasswordCredential Debug must not echo the hash bytes"
        );
        assert!(
            !formatted.contains("argon2id"),
            "PasswordCredential Debug must not echo argon2id markers"
        );
        assert!(
            formatted.contains("redacted"),
            "PasswordCredential Debug must label the redaction"
        );
    }

    #[test]
    fn serde_skips_password_hash() {
        let cred = fixture();
        let json = serde_json::to_string(&cred).expect("serialize");
        assert!(
            !json.contains("password_hash"),
            "Serialize must not emit the password_hash field at all"
        );
        assert!(
            !json.contains("argon2id"),
            "Serialize must not echo argon2id markers"
        );
        assert!(
            !json.contains("DO-NOT-LEAK") && !json.contains("do-not-leak"),
            "Serialize must not echo any hash bytes"
        );
    }
}
