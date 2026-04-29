//! SSH identity response DTOs.
//!
//! **Critical:** [`SshIdentityResponse`] does NOT carry
//! `encrypted_private_key`. The field type does not even exist on the wire
//! shape, so it cannot be serialized by accident. The mapping from the
//! domain record drops the bytes silently — the only place that field is
//! ever observed is inside an SSH session task with the vault key.

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use relayterm_core::ids::SshIdentityId;
use relayterm_core::ssh_identity::{SshIdentity, SshKeyType};
use serde::Serialize;
use tracing::warn;

#[derive(Debug, Serialize)]
pub(crate) struct SshIdentityResponse {
    pub id: SshIdentityId,
    pub name: String,
    pub key_type: SshKeyType,
    /// OpenSSH-format public key as text. Stored as bytes in the domain
    /// because the column is `BYTEA`, but the on-the-wire convention for
    /// OpenSSH public keys is ASCII; non-UTF-8 bytes (which would indicate
    /// a corrupted row) are replaced with U+FFFD rather than failing, and
    /// a `warn!` is emitted so the data-integrity issue surfaces in logs.
    pub public_key: String,
    pub fingerprint_sha256: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Decode a public-key byte buffer to a UTF-8 string, replacing invalid
/// sequences with U+FFFD and `warn!`-logging the (non-secret) identity id
/// when replacement happens. Public-key bytes are not secret, but the raw
/// buffer is still left out of the log line — only the id and the fact of
/// the replacement are recorded.
fn public_key_to_lossy_string(identity_id: SshIdentityId, bytes: &[u8]) -> String {
    match String::from_utf8_lossy(bytes) {
        Cow::Borrowed(s) => s.to_owned(),
        Cow::Owned(s) => {
            warn!(
                %identity_id,
                "ssh_identity.public_key contains invalid UTF-8; serialized via lossy replacement",
            );
            s
        }
    }
}

impl From<SshIdentity> for SshIdentityResponse {
    fn from(id: SshIdentity) -> Self {
        let public_key = public_key_to_lossy_string(id.id, &id.public_key);
        Self {
            id: id.id,
            name: id.name,
            key_type: id.key_type,
            public_key,
            fingerprint_sha256: id.fingerprint_sha256,
            created_at: id.created_at,
            last_used_at: id.last_used_at,
            // encrypted_private_key intentionally dropped — see module docs.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use relayterm_core::ids::{SshIdentityId, UserId};

    fn fixture() -> SshIdentity {
        SshIdentity {
            id: SshIdentityId::new(),
            owner_id: UserId::new(),
            name: "primary".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 AAAA-public".to_vec(),
            encrypted_private_key: b"REDACT-MARKER-9F2B".to_vec(),
            fingerprint_sha256: "SHA256:abcd".to_owned(),
            created_at: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            last_used_at: None,
        }
    }

    #[test]
    fn response_drops_encrypted_private_key() {
        let resp = SshIdentityResponse::from(fixture());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("encrypted_private_key"),
            "ssh identity DTO must not expose encrypted_private_key field: {json}",
        );
        assert!(
            !json.contains("REDACT-MARKER-9F2B"),
            "ssh identity DTO must not leak private key bytes: {json}",
        );
        assert!(
            !json.contains("owner_id"),
            "ssh identity DTO should not expose owner_id at the wire boundary: {json}",
        );
    }

    #[test]
    fn response_keeps_public_metadata() {
        let resp = SshIdentityResponse::from(fixture());
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "primary");
        assert_eq!(json["public_key"], "ssh-ed25519 AAAA-public");
        assert_eq!(json["fingerprint_sha256"], "SHA256:abcd");
    }

    #[test]
    fn invalid_utf8_public_key_falls_back_to_replacement() {
        let mut bad = fixture();
        // 0xFF is never valid UTF-8.
        bad.public_key = vec![b's', b's', b'h', b'-', 0xFF, 0xFE];
        let resp = SshIdentityResponse::from(bad);
        // Replacement char appears, no panic.
        assert!(resp.public_key.starts_with("ssh-"));
        assert!(resp.public_key.contains('\u{FFFD}'));
    }
}
