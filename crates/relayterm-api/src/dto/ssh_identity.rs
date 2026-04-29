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
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::ApiError;

/// Maximum length for the user-supplied identity name.
const MAX_NAME_LEN: usize = 64;

/// Request body for `POST /api/v1/ssh-identities`.
///
/// Deliberately minimal: the user picks a label and (optionally) a key
/// type. Everything else — the keypair itself, fingerprint, encrypted
/// blob — is generated server-side by the vault. There is no field for
/// supplying private-key material; private-key import is a separate,
/// not-yet-implemented surface.
///
/// `key_type` is taken as a free-form string and parsed in
/// [`Self::validate`] so an unknown algorithm tag yields a clean 400
/// `invalid_input` rather than a serde 422 rejection.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateSshIdentityRequest {
    pub name: String,
    /// Optional; defaults to `ed25519` when omitted.
    pub key_type: Option<String>,
}

/// Validated, normalized form of [`CreateSshIdentityRequest`].
///
/// `Debug` is fine here — the struct only carries public, user-supplied
/// metadata. The vault, not this struct, owns the secret material.
#[derive(Debug, Clone)]
pub(crate) struct ValidatedCreateSshIdentity {
    pub name: String,
    pub key_type: SshKeyType,
}

impl CreateSshIdentityRequest {
    /// Validate the request body. Failures map to `400 invalid_input`
    /// without echoing the offending value beyond what the validator
    /// already names.
    pub(crate) fn validate(self) -> Result<ValidatedCreateSshIdentity, ApiError> {
        let trimmed = self.name.trim();
        if trimmed.is_empty() {
            return Err(ApiError::Validation("name must not be empty".to_owned()));
        }
        if trimmed != self.name {
            return Err(ApiError::Validation(
                "name must not start or end with whitespace".to_owned(),
            ));
        }
        if trimmed.chars().count() > MAX_NAME_LEN {
            return Err(ApiError::Validation(format!(
                "name must be at most {MAX_NAME_LEN} characters",
            )));
        }
        if trimmed.chars().any(char::is_control) {
            return Err(ApiError::Validation(
                "name must not contain control characters".to_owned(),
            ));
        }
        let key_type = match self.key_type.as_deref() {
            None => SshKeyType::Ed25519,
            Some(tag) => parse_supported_key_type(tag)?,
        };
        Ok(ValidatedCreateSshIdentity {
            name: trimmed.to_owned(),
            key_type,
        })
    }
}

/// Parse and gate a `key_type` tag against the algorithms the vault can
/// currently generate.
///
/// Funnels both "unknown algorithm" and "known but not yet supported by
/// the vault generator" through one error shape: 400 `invalid_input` with
/// the message `unsupported key_type "<tag>"`. Without this gate, an
/// unknown tag would 400 from the DTO and a known-unsupported tag (e.g.
/// `"rsa"`) would 400 from the vault's `UnsupportedKeyType` mapping with
/// a slightly different phrasing — surprising clients that match on
/// message text.
fn parse_supported_key_type(tag: &str) -> Result<SshKeyType, ApiError> {
    let parsed = SshKeyType::from_str_tag(tag)
        .ok_or_else(|| ApiError::Validation(format!("unsupported key_type {tag:?}")))?;
    match parsed {
        SshKeyType::Ed25519 => Ok(parsed),
        SshKeyType::Rsa | SshKeyType::EcdsaP256 | SshKeyType::EcdsaP384 | SshKeyType::EcdsaP521 => {
            Err(ApiError::Validation(format!(
                "unsupported key_type {tag:?}"
            )))
        }
    }
}

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

    #[test]
    fn create_request_defaults_to_ed25519() {
        let req = CreateSshIdentityRequest {
            name: "primary".to_owned(),
            key_type: None,
        };
        let v = req.validate().unwrap();
        assert_eq!(v.name, "primary");
        assert_eq!(v.key_type, SshKeyType::Ed25519);
    }

    #[test]
    fn create_request_accepts_explicit_key_type() {
        let req = CreateSshIdentityRequest {
            name: "primary".to_owned(),
            key_type: Some("ed25519".to_owned()),
        };
        let v = req.validate().unwrap();
        assert_eq!(v.key_type, SshKeyType::Ed25519);
    }

    #[test]
    fn create_request_rejects_unknown_key_type() {
        let req = CreateSshIdentityRequest {
            name: "primary".to_owned(),
            key_type: Some("invalid-algo".to_owned()),
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert_eq!(msg, "unsupported key_type \"invalid-algo\"");
    }

    #[test]
    fn create_request_rejects_known_but_unsupported_key_type() {
        // RSA parses to a known SshKeyType variant but the vault has no
        // generator for it — the DTO must produce the same error shape as
        // a totally-unknown tag so clients see one canonical 400.
        let req = CreateSshIdentityRequest {
            name: "primary".to_owned(),
            key_type: Some("rsa".to_owned()),
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert_eq!(msg, "unsupported key_type \"rsa\"");
    }

    #[test]
    fn create_request_unknown_and_unsupported_share_message_shape() {
        // Same prefix, just a different tag value — what the test above
        // proves implicitly, restated as a one-line invariant.
        let unknown = CreateSshIdentityRequest {
            name: "p".to_owned(),
            key_type: Some("foo".to_owned()),
        };
        let unsupported = CreateSshIdentityRequest {
            name: "p".to_owned(),
            key_type: Some("ecdsa_p256".to_owned()),
        };
        let m1 = match unknown.validate() {
            Err(ApiError::Validation(m)) => m,
            other => panic!("unexpected: {other:?}"),
        };
        let m2 = match unsupported.validate() {
            Err(ApiError::Validation(m)) => m,
            other => panic!("unexpected: {other:?}"),
        };
        assert!(m1.starts_with("unsupported key_type "));
        assert!(m2.starts_with("unsupported key_type "));
    }

    #[test]
    fn create_request_rejects_empty_name() {
        let req = CreateSshIdentityRequest {
            name: "   ".to_owned(),
            key_type: None,
        };
        assert!(matches!(req.validate(), Err(ApiError::Validation(_))));
    }

    #[test]
    fn create_request_rejects_surrounding_whitespace() {
        let req = CreateSshIdentityRequest {
            name: " primary".to_owned(),
            key_type: None,
        };
        assert!(matches!(req.validate(), Err(ApiError::Validation(_))));
    }

    #[test]
    fn create_request_rejects_control_chars() {
        let req = CreateSshIdentityRequest {
            name: "bad\nname".to_owned(),
            key_type: None,
        };
        assert!(matches!(req.validate(), Err(ApiError::Validation(_))));
    }

    #[test]
    fn create_request_rejects_too_long() {
        let req = CreateSshIdentityRequest {
            name: "a".repeat(65),
            key_type: None,
        };
        assert!(matches!(req.validate(), Err(ApiError::Validation(_))));
    }
}
