//! SSH identity response DTOs.
//!
//! **Critical:** [`SshIdentityResponse`] does NOT carry
//! `encrypted_private_key`. The field type does not even exist on the wire
//! shape, so it cannot be serialized by accident. The mapping from the
//! domain record drops the bytes silently — the only place that field is
//! ever observed is inside an SSH session task with the vault key.

use std::borrow::Cow;
use std::fmt;

use chrono::{DateTime, Utc};
use relayterm_core::ids::SshIdentityId;
use relayterm_core::ssh_identity::{SshIdentity, SshKeyType};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::ApiError;

/// Maximum length for the user-supplied identity name.
const MAX_NAME_LEN: usize = 64;

/// Maximum size for the OpenSSH-format private key text supplied to the
/// import route, in bytes. An Ed25519 OpenSSH PEM is ~400 bytes; an
/// RSA-4096 OpenSSH PEM is ~3.3 KiB. 8 KiB is comfortably above both
/// realistic shapes and well below the default axum body cap, so a
/// malformed paste cannot chew CPU in the parser.
const MAX_PRIVATE_KEY_OPENSSH_BYTES: usize = 8 * 1024;

/// OpenSSH private-key PEM header sentinel. The DTO requires this
/// substring before handing the body to the vault — a missing header
/// short-circuits the parser. Public-key uploads (which use the
/// `ssh-<algo>` prefix and no PEM envelope) and PEM PKCS#1 / PKCS#8
/// bodies are rejected here BEFORE any `ssh-key` work runs.
const OPENSSH_PRIVATE_KEY_HEADER: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";

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
        let trimmed = validate_identity_name(&self.name)?;
        let key_type = match self.key_type.as_deref() {
            None => SshKeyType::Ed25519,
            Some(tag) => parse_supported_key_type(tag)?,
        };
        Ok(ValidatedCreateSshIdentity {
            name: trimmed,
            key_type,
        })
    }
}

/// Shared name validator for create + rename. Centralised so a rename
/// produces byte-identical errors to a create (the wire `message` text
/// is the same, the codes are the same, the per-rule order is the same).
pub(crate) fn validate_identity_name(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::Validation("name must not be empty".to_owned()));
    }
    if trimmed != name {
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
    Ok(trimmed.to_owned())
}

/// Request body for `PATCH /api/v1/ssh-identities/:id`.
///
/// Rename is the only edit surface in this slice. `key_type`,
/// `public_key`, and `encrypted_private_key` are immutable after
/// creation — adding a `key_type` field here would imply we can re-key
/// in place, which would silently break every saved server profile
/// that references this identity. Re-keying is a separate, deliberate
/// "delete + recreate + re-bind profiles" flow that doesn't exist yet.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSshIdentityRequest {
    pub name: String,
}

impl UpdateSshIdentityRequest {
    pub(crate) fn validated_name(&self) -> Result<String, ApiError> {
        validate_identity_name(&self.name)
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

/// Request body for `POST /api/v1/ssh-identities/import`.
///
/// Carries the OpenSSH-format private-key PEM. **DO NOT derive `Debug`**
/// on this struct — the manual impl below redacts `private_key_openssh`
/// to a length-only summary. A derived impl would emit the PEM bytes
/// through any `dbg!`, `format!("{:?}")`, tracing subscriber, or panic
/// formatter that touches the type.
///
/// `passphrase` is intentionally absent in v1 — passphrase-protected
/// keys are out of scope for this slice (see
/// `docs/private-key-import.md` § 1 / § 13). Adding it later widens the
/// redaction surface; it lands in v1.1.
///
/// `Deserialize` is the only auto-derive; serde does not call `Debug`,
/// so it cannot leak the field through that path.
#[derive(Deserialize)]
pub(crate) struct ImportSshIdentityRequest {
    pub name: String,
    pub private_key_openssh: String,
}

impl fmt::Debug for ImportSshIdentityRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportSshIdentityRequest")
            .field("name", &self.name)
            .field(
                "private_key_openssh",
                &format_args!("<redacted: {} bytes>", self.private_key_openssh.len()),
            )
            .finish()
    }
}

/// Validated, normalized form of [`ImportSshIdentityRequest`].
///
/// `pem` is held in a `Zeroizing<Vec<u8>>` so the bytes wipe on drop —
/// the validator consumes the request by value, the original `String`
/// allocation is dropped at the end of `validate`, and the
/// vault-bound copy here is the only durable form between validate and
/// the vault call. **DO NOT** add `Debug` / `Clone` / `Serialize` here.
pub(crate) struct ValidatedImportSshIdentity {
    pub name: String,
    pub pem: zeroize::Zeroizing<Vec<u8>>,
}

impl fmt::Debug for ValidatedImportSshIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ValidatedImportSshIdentity")
            .field("name", &self.name)
            .field("pem", &format_args!("<redacted: {} bytes>", self.pem.len()))
            .finish()
    }
}

impl ImportSshIdentityRequest {
    /// Validate the request body. Failures map to `400 invalid_input`
    /// with stable, operator-safe messages. The offending PEM bytes are
    /// NEVER echoed in the wire `message` — only the rule that fired is
    /// named.
    ///
    /// Validation rules:
    ///  - `name` reuses [`validate_identity_name`] byte-for-byte with
    ///    the generate path.
    ///  - `private_key_openssh` must be ASCII (`< 0x80`).
    ///  - `private_key_openssh` must be ≤
    ///    [`MAX_PRIVATE_KEY_OPENSSH_BYTES`] bytes.
    ///  - `private_key_openssh` must contain the OpenSSH PEM header
    ///    sentinel.
    pub(crate) fn validate(self) -> Result<ValidatedImportSshIdentity, ApiError> {
        let name = validate_identity_name(&self.name)?;
        // Move the PEM out of the request struct as early as possible so
        // there is exactly one durable copy between here and the vault.
        let raw = self.private_key_openssh;
        if raw.is_empty() {
            return Err(ApiError::Validation(
                "private_key_openssh must not be empty".to_owned(),
            ));
        }
        if raw.len() > MAX_PRIVATE_KEY_OPENSSH_BYTES {
            return Err(ApiError::Validation(format!(
                "private_key_openssh must not exceed {MAX_PRIVATE_KEY_OPENSSH_BYTES} bytes",
            )));
        }
        if !raw.is_ascii() {
            return Err(ApiError::Validation(
                "private_key_openssh must be ASCII".to_owned(),
            ));
        }
        if !raw.contains(OPENSSH_PRIVATE_KEY_HEADER) {
            return Err(ApiError::Validation(
                "private_key_openssh is missing OpenSSH PEM header".to_owned(),
            ));
        }
        let pem = zeroize::Zeroizing::new(raw.into_bytes());
        Ok(ValidatedImportSshIdentity { name, pem })
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

    // ----------------------------------------------------------------
    // Import-request DTO tests.
    //
    // PEM bytes here are throwaway test material — minimal valid-looking
    // strings that exercise the validator branches. The real OpenSSH
    // parser only runs at the vault layer; the DTO just enforces shape
    // (size, ASCII, header sentinel) so a malformed paste is refused
    // before the parser sees it.
    // ----------------------------------------------------------------

    /// Smallest PEM body that passes the DTO validator. Not a real
    /// parseable key — `ssh-key` would reject the body in
    /// `VaultService::import_ssh_identity`. The validator only inspects
    /// shape, so this is sufficient to exercise the success branch.
    fn minimal_valid_pem_shape() -> String {
        format!(
            "{}\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
            OPENSSH_PRIVATE_KEY_HEADER,
        )
    }

    #[test]
    fn import_request_accepts_well_formed_shape() {
        let req = ImportSshIdentityRequest {
            name: "imported-identity".to_owned(),
            private_key_openssh: minimal_valid_pem_shape(),
        };
        let validated = req.validate().expect("shape-valid request must pass");
        assert_eq!(validated.name, "imported-identity");
        assert!(
            validated
                .pem
                .starts_with(OPENSSH_PRIVATE_KEY_HEADER.as_bytes())
        );
    }

    #[test]
    fn import_request_rejects_blank_name() {
        let req = ImportSshIdentityRequest {
            name: "  ".to_owned(),
            private_key_openssh: minimal_valid_pem_shape(),
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert_eq!(msg, "name must not be empty");
    }

    #[test]
    fn import_request_rejects_empty_pem() {
        let req = ImportSshIdentityRequest {
            name: "ok".to_owned(),
            private_key_openssh: String::new(),
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert_eq!(msg, "private_key_openssh must not be empty");
    }

    #[test]
    fn import_request_rejects_oversized_pem() {
        let req = ImportSshIdentityRequest {
            name: "ok".to_owned(),
            // One byte over the cap — the validator MUST refuse before any
            // parser runs so a 9 KiB paste cannot chew CPU.
            private_key_openssh: "A".repeat(MAX_PRIVATE_KEY_OPENSSH_BYTES + 1),
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert!(msg.starts_with("private_key_openssh must not exceed "));
    }

    #[test]
    fn import_request_rejects_non_ascii_pem() {
        let mut body = minimal_valid_pem_shape();
        body.push('\u{00ff}');
        let req = ImportSshIdentityRequest {
            name: "ok".to_owned(),
            private_key_openssh: body,
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert_eq!(msg, "private_key_openssh must be ASCII");
    }

    #[test]
    fn import_request_rejects_missing_pem_header() {
        let req = ImportSshIdentityRequest {
            name: "ok".to_owned(),
            // Public-key shape — no PEM envelope. Pinning that the public-
            // key path is refused before the vault parser ever sees it.
            private_key_openssh: "ssh-ed25519 AAAA-not-a-private-key".to_owned(),
        };
        let err = req.validate().unwrap_err();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got {err:?}");
        };
        assert_eq!(msg, "private_key_openssh is missing OpenSSH PEM header");
    }

    #[test]
    fn import_request_debug_redacts_pem_bytes() {
        // Sentinel bytes that would be unmistakable if the redaction
        // discipline regressed.
        let sentinel = "RT-DTO-DEBUG-LEAK-MARKER";
        let body = format!(
            "{}\n{sentinel}\n-----END OPENSSH PRIVATE KEY-----\n",
            OPENSSH_PRIVATE_KEY_HEADER,
        );
        let req = ImportSshIdentityRequest {
            name: "ok".to_owned(),
            private_key_openssh: body,
        };
        let dbg = format!("{req:?}");
        assert!(
            !dbg.contains(sentinel),
            "Debug must redact PEM bytes: {dbg}"
        );
        assert!(!dbg.contains("BEGIN OPENSSH PRIVATE KEY"));
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn validated_import_debug_redacts_pem_bytes() {
        let sentinel = "RT-DTO-VALIDATED-DEBUG-LEAK";
        let body = format!(
            "{}\n{sentinel}\n-----END OPENSSH PRIVATE KEY-----\n",
            OPENSSH_PRIVATE_KEY_HEADER,
        );
        let req = ImportSshIdentityRequest {
            name: "ok".to_owned(),
            private_key_openssh: body,
        };
        let validated = req.validate().unwrap();
        let dbg = format!("{validated:?}");
        assert!(
            !dbg.contains(sentinel),
            "Debug must redact PEM bytes: {dbg}"
        );
        assert!(dbg.contains("redacted"));
    }
}
