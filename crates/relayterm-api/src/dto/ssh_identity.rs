//! SSH identity response DTOs.
//!
//! **Critical:** [`SshIdentityResponse`] does NOT carry
//! `encrypted_private_key`. The field type does not even exist on the wire
//! shape, so it cannot be serialized by accident. The mapping from the
//! domain record drops the bytes silently — the only place that field is
//! ever observed is inside an SSH session task with the vault key.

use chrono::{DateTime, Utc};
use relayterm_core::ids::SshIdentityId;
use relayterm_core::ssh_identity::{SshIdentity, SshKeyType};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct SshIdentityResponse {
    pub id: SshIdentityId,
    pub name: String,
    pub key_type: SshKeyType,
    /// OpenSSH-format public key as text. Stored as bytes in the domain
    /// because the column is `BYTEA`, but the on-the-wire convention for
    /// OpenSSH public keys is ASCII; non-UTF-8 bytes (which would indicate
    /// a corrupted row) are replaced with U+FFFD rather than failing.
    pub public_key: String,
    pub fingerprint_sha256: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<SshIdentity> for SshIdentityResponse {
    fn from(id: SshIdentity) -> Self {
        Self {
            id: id.id,
            name: id.name,
            key_type: id.key_type,
            public_key: String::from_utf8_lossy(&id.public_key).into_owned(),
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
}
