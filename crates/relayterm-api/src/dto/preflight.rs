//! Host-key preflight + trust-host-key DTOs.
//!
//! These wire shapes are the public contract for
//! `POST /server-profiles/:id/host-key-preflight` and
//! `POST /server-profiles/:id/trust-host-key`. The response carries ONLY
//! public host-side data — the captured fingerprint, key type, and trust
//! status. Nothing about the client identity (vault blob, decrypted PEM,
//! internal russh errors) is reachable from this struct.
//!
//! **Scope (do not overclaim).** A successful preflight response means the
//! KEX-stage host-key probe completed and was classified — it does NOT
//! attest to SSH authentication, PTY allocation, or session readiness.
//! The response wording is deliberately conservative; do not loosen it
//! without revisiting the security posture.

use chrono::{DateTime, Utc};
use relayterm_core::ids::{HostId, KnownHostEntryId, ServerProfileId};
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_ssh::HostKeyStatus;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// Wire enum for [`HostKeyStatus`]. Snake-case tags are part of the
/// contract — clients depend on them.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostKeyStatusWire {
    Unknown,
    Trusted,
    Changed,
}

impl From<HostKeyStatus> for HostKeyStatusWire {
    fn from(s: HostKeyStatus) -> Self {
        match s {
            HostKeyStatus::Unknown => Self::Unknown,
            HostKeyStatus::Trusted => Self::Trusted,
            HostKeyStatus::Changed => Self::Changed,
        }
    }
}

/// Successful response from `POST /server-profiles/:id/host-key-preflight`.
///
/// **Wire-contract note**: this response describes ONLY the host-key
/// reachability classification. It does NOT mean SSH authentication
/// succeeded, that the configured identity is installed in
/// `authorized_keys` on the target, or that a PTY/shell can be opened.
/// Auth and session readiness are separate, later concerns.
#[derive(Debug, Serialize)]
pub(crate) struct HostKeyPreflightResponse {
    pub profile_id: ServerProfileId,
    pub host_id: HostId,
    /// Hostname the probe connected to. Echoed so a client showing a UI
    /// summary doesn't have to re-fetch the host row.
    pub hostname: String,
    pub port: u16,
    /// Classification of the captured host key against the host's pinned
    /// `known_host_entries`. See [`HostKeyStatusWire`].
    pub host_key_status: HostKeyStatusWire,
    pub host_key_type: SshKeyType,
    pub host_key_fingerprint: String,
    /// Short, user-facing message explaining the status. Static per
    /// status — no operator detail leaks here. Phrasing is deliberate:
    /// the messages name *only* what the host-key probe proved (KEX
    /// reached, key captured) and never imply anything about SSH auth or
    /// PTY readiness.
    pub message: &'static str,
}

impl HostKeyPreflightResponse {
    pub(crate) fn message_for(status: HostKeyStatusWire) -> &'static str {
        match status {
            HostKeyStatusWire::Unknown => {
                "host key not yet pinned; KEX-stage probe only — \
                 SSH authentication and session readiness were not validated"
            }
            HostKeyStatusWire::Trusted => {
                "host key matches a trusted pinned entry; KEX-stage probe only — \
                 SSH authentication and session readiness were not validated"
            }
            HostKeyStatusWire::Changed => {
                "host key differs from the pinned entry; \
                 refusing to trust automatically — pin was NOT updated"
            }
        }
    }
}

/// Request body for `POST /server-profiles/:id/trust-host-key`.
///
/// The caller must echo the fingerprint they observed from the most
/// recent preflight. The route compares it against a fresh capture — if
/// the server's key has changed in between, the trust is rejected.
#[derive(Debug, Deserialize)]
pub(crate) struct TrustHostKeyRequest {
    pub expected_fingerprint: String,
}

impl TrustHostKeyRequest {
    /// Sanity-check the fingerprint format before any DB or network work.
    /// Rejects empty input, missing prefix, embedded whitespace, control
    /// characters, and absurd lengths. The strict format is the
    /// `SHA256:<base64>` shape that `ssh-keygen -lf` (and our vault) emit.
    pub(crate) fn validated_expected_fingerprint(&self) -> Result<&str, ApiError> {
        // 7 prefix chars + at least one byte of digest material.
        const MIN_LEN: usize = 8;
        // SHA256 base64 (43) + prefix (7) plus generous slack for
        // whatever exotic encoding a future caller might invent.
        const MAX_LEN: usize = 128;
        let v = self.expected_fingerprint.as_str();
        if !v.starts_with("SHA256:") {
            return Err(ApiError::Validation(
                "expected_fingerprint must start with 'SHA256:'".to_owned(),
            ));
        }
        if v.len() < MIN_LEN || v.len() > MAX_LEN {
            return Err(ApiError::Validation(
                "expected_fingerprint length is out of range".to_owned(),
            ));
        }
        if v.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(ApiError::Validation(
                "expected_fingerprint must not contain whitespace or control characters".to_owned(),
            ));
        }
        Ok(v)
    }
}

/// Successful response from `POST /server-profiles/:id/trust-host-key`.
#[derive(Debug, Serialize)]
pub(crate) struct TrustHostKeyResponse {
    pub known_host_entry_id: KnownHostEntryId,
    pub host_id: HostId,
    pub host_key_type: SshKeyType,
    pub host_key_fingerprint: String,
    pub trusted_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_status_wire_uses_snake_case_tags() {
        // The wire tags are part of the API contract; serialize them
        // explicitly so the test fails on accidental rename.
        let unknown = serde_json::to_value(HostKeyStatusWire::Unknown).unwrap();
        let trusted = serde_json::to_value(HostKeyStatusWire::Trusted).unwrap();
        let changed = serde_json::to_value(HostKeyStatusWire::Changed).unwrap();
        assert_eq!(unknown, serde_json::Value::String("unknown".into()));
        assert_eq!(trusted, serde_json::Value::String("trusted".into()));
        assert_eq!(changed, serde_json::Value::String("changed".into()));
    }

    #[test]
    fn trust_request_accepts_well_formed_fingerprint() {
        let req = TrustHostKeyRequest {
            expected_fingerprint: "SHA256:abcdefGHIJKLmnopqr0123456789".to_owned(),
        };
        let v = req.validated_expected_fingerprint().unwrap();
        assert!(v.starts_with("SHA256:"));
    }

    #[test]
    fn trust_request_rejects_missing_prefix() {
        let req = TrustHostKeyRequest {
            expected_fingerprint: "MD5:abcd".to_owned(),
        };
        assert!(matches!(
            req.validated_expected_fingerprint(),
            Err(ApiError::Validation(_))
        ));
    }

    #[test]
    fn trust_request_rejects_whitespace() {
        let req = TrustHostKeyRequest {
            expected_fingerprint: "SHA256:ab cd".to_owned(),
        };
        assert!(matches!(
            req.validated_expected_fingerprint(),
            Err(ApiError::Validation(_))
        ));
    }

    #[test]
    fn trust_request_rejects_too_short() {
        let req = TrustHostKeyRequest {
            expected_fingerprint: "SHA256:".to_owned(),
        };
        assert!(matches!(
            req.validated_expected_fingerprint(),
            Err(ApiError::Validation(_))
        ));
    }

    #[test]
    fn trust_request_rejects_control_chars() {
        let req = TrustHostKeyRequest {
            expected_fingerprint: "SHA256:ab\ncd".to_owned(),
        };
        assert!(matches!(
            req.validated_expected_fingerprint(),
            Err(ApiError::Validation(_))
        ));
    }
}
