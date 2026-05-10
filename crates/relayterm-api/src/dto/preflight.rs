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
use relayterm_core::known_host::KnownHostRevocationReason;
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
    /// Public fingerprint of the active pinned entry on this host, when
    /// status is [`HostKeyStatusWire::Changed`]. `None` for `Unknown`
    /// (no active pin OR revoked-and-reappearing) and for `Trusted` (the
    /// captured fingerprint already matches the active pin, so there is
    /// nothing to "replace"). Carries ONLY the public SHA-256 fingerprint
    /// string — no public-key bytes, no key-type override, no
    /// `known_host_entries` row internals. Wired solely to enable the
    /// SPA's host-key replace flow without a separate known-host listing
    /// endpoint (see `docs/spec/host-key-replace.md` § R6 / Phase 4).
    pub active_pin_fingerprint: Option<String>,
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

/// Request body for `POST /server-profiles/:id/replace-host-key`.
///
/// The caller must echo BOTH fingerprints (the active pin they consent to
/// revoke AND the captured fingerprint they confirmed in the preceding
/// `changed` preflight) and pick one of the canonical
/// [`KnownHostRevocationReason`] codes. Free-text reason notes are
/// deliberately not part of the schema (see
/// `docs/spec/host-key-replace.md` § R3 — the enum is the only operator
/// input persisted, removing the free-text channel that could smuggle
/// secrets into `audit_events.payload`).
#[derive(Debug, Deserialize)]
pub(crate) struct ReplaceHostKeyRequest {
    pub expected_old_fingerprint: String,
    pub expected_new_fingerprint: String,
    /// Wire tag (`server_reinstalled`, `host_key_rotated`,
    /// `lab_target_recreated`, `operator_other`). Validated by
    /// [`Self::validated`].
    pub reason_code: String,
}

/// Validated, type-safe shape derived from a [`ReplaceHostKeyRequest`].
/// Constructed by [`ReplaceHostKeyRequest::validated`] AFTER the
/// fingerprint shape and reason-code accept-list checks pass — every
/// field is safe to forward to the repository / SSH layers without
/// re-validation.
pub(crate) struct ReplaceHostKeyValidatedRequest {
    pub expected_old_fingerprint: String,
    pub expected_new_fingerprint: String,
    pub reason_code: KnownHostRevocationReason,
}

impl ReplaceHostKeyRequest {
    /// Validate every input field BEFORE any DB or network work.
    ///
    /// - Both fingerprints go through the same shape rules as the
    ///   `trust-host-key` route's `validated_expected_fingerprint` — the
    ///   error messages name which field tripped so a CLI client gets a
    ///   precise diagnostic, but the wire envelope still collapses to
    ///   `invalid_input`.
    /// - `reason_code` is matched against the
    ///   [`KnownHostRevocationReason::from_str_tag`] accept-list. Any
    ///   value outside the four canonical tags returns 400 — this is
    ///   the API-layer mirror of the `known_host_entries_revoked_reason_chk`
    ///   schema CHECK.
    pub(crate) fn validated(&self) -> Result<ReplaceHostKeyValidatedRequest, ApiError> {
        let expected_old_fingerprint =
            validate_fingerprint(&self.expected_old_fingerprint, "expected_old_fingerprint")?
                .to_owned();
        let expected_new_fingerprint =
            validate_fingerprint(&self.expected_new_fingerprint, "expected_new_fingerprint")?
                .to_owned();
        let reason_code = KnownHostRevocationReason::from_str_tag(self.reason_code.as_str())
            .ok_or_else(|| {
                ApiError::Validation("reason_code is not a recognised value".to_owned())
            })?;
        Ok(ReplaceHostKeyValidatedRequest {
            expected_old_fingerprint,
            expected_new_fingerprint,
            reason_code,
        })
    }
}

/// Successful response from `POST /server-profiles/:id/replace-host-key`.
///
/// Carries only public-side identifiers and the public fingerprints. No
/// `public_key` byte blob; no vault payloads; no host banner; no raw
/// error text. The reason code is intentionally NOT echoed — it lives in
/// the audit row, where its visibility is bounded by the audit-feed UI.
#[derive(Debug, Serialize)]
pub(crate) struct ReplaceHostKeyResponse {
    pub profile_id: ServerProfileId,
    pub host_id: HostId,
    pub revoked_known_host_entry_id: KnownHostEntryId,
    pub revoked_fingerprint: String,
    pub trusted_known_host_entry_id: KnownHostEntryId,
    pub trusted_fingerprint: String,
    pub host_key_type: SshKeyType,
    pub trusted_at: DateTime<Utc>,
}

/// Validate a single SHA-256 fingerprint string against the same shape
/// rules `trust-host-key` enforces. Lifted into a free function so both
/// the `expected_old_fingerprint` and `expected_new_fingerprint` paths
/// share one body.
///
/// `field` is included in the validation message so a CLI client gets a
/// precise diagnostic even though the wire `code` is the same
/// `invalid_input` either way.
fn validate_fingerprint<'a>(value: &'a str, field: &'static str) -> Result<&'a str, ApiError> {
    // 7 prefix chars + at least one byte of digest material.
    const MIN_LEN: usize = 8;
    // SHA256 base64 (43) + prefix (7) plus generous slack for whatever
    // exotic encoding a future caller might invent.
    const MAX_LEN: usize = 128;
    if !value.starts_with("SHA256:") {
        return Err(ApiError::Validation(format!(
            "{field} must start with 'SHA256:'"
        )));
    }
    if value.len() < MIN_LEN || value.len() > MAX_LEN {
        return Err(ApiError::Validation(format!(
            "{field} length is out of range"
        )));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::Validation(format!(
            "{field} must not contain whitespace or control characters"
        )));
    }
    Ok(value)
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

    #[test]
    fn replace_request_validates_fingerprints_and_reason_code() {
        let v = ReplaceHostKeyRequest {
            expected_old_fingerprint: "SHA256:OLD-fp".to_owned(),
            expected_new_fingerprint: "SHA256:NEW-fp".to_owned(),
            reason_code: "server_reinstalled".to_owned(),
        }
        .validated()
        .expect("valid replace request");
        assert_eq!(v.expected_old_fingerprint, "SHA256:OLD-fp");
        assert_eq!(v.expected_new_fingerprint, "SHA256:NEW-fp");
        assert_eq!(v.reason_code, KnownHostRevocationReason::ServerReinstalled);
    }

    #[test]
    fn replace_request_rejects_each_canonical_failure_mode() {
        // Bad old fingerprint shape.
        let r = ReplaceHostKeyRequest {
            expected_old_fingerprint: "MD5:nope".to_owned(),
            expected_new_fingerprint: "SHA256:NEW".to_owned(),
            reason_code: "host_key_rotated".to_owned(),
        };
        let Err(ApiError::Validation(msg)) = r.validated() else {
            panic!("expected Validation error");
        };
        assert!(
            msg.contains("expected_old_fingerprint"),
            "field name must be in message: {msg}",
        );

        // Bad new fingerprint shape.
        let r = ReplaceHostKeyRequest {
            expected_old_fingerprint: "SHA256:OLD".to_owned(),
            expected_new_fingerprint: "garbage".to_owned(),
            reason_code: "host_key_rotated".to_owned(),
        };
        let Err(ApiError::Validation(msg)) = r.validated() else {
            panic!("expected Validation error");
        };
        assert!(msg.contains("expected_new_fingerprint"));

        // Reason code outside the four-tag accept-list.
        let r = ReplaceHostKeyRequest {
            expected_old_fingerprint: "SHA256:OLD".to_owned(),
            expected_new_fingerprint: "SHA256:NEW".to_owned(),
            reason_code: "operator_freeform".to_owned(),
        };
        let Err(ApiError::Validation(msg)) = r.validated() else {
            panic!("expected Validation error");
        };
        assert!(msg.contains("reason_code"));

        // Empty reason code.
        let r = ReplaceHostKeyRequest {
            expected_old_fingerprint: "SHA256:OLD".to_owned(),
            expected_new_fingerprint: "SHA256:NEW".to_owned(),
            reason_code: String::new(),
        };
        assert!(matches!(r.validated(), Err(ApiError::Validation(_))));

        // Whitespace inside fingerprint.
        let r = ReplaceHostKeyRequest {
            expected_old_fingerprint: "SHA256:OLD".to_owned(),
            expected_new_fingerprint: "SHA256:NEW \nfp".to_owned(),
            reason_code: "operator_other".to_owned(),
        };
        assert!(matches!(r.validated(), Err(ApiError::Validation(_))));
    }

    #[test]
    fn replace_request_accepts_all_four_canonical_reason_codes() {
        for (tag, expected) in [
            (
                "server_reinstalled",
                KnownHostRevocationReason::ServerReinstalled,
            ),
            (
                "host_key_rotated",
                KnownHostRevocationReason::HostKeyRotated,
            ),
            (
                "lab_target_recreated",
                KnownHostRevocationReason::LabTargetRecreated,
            ),
            ("operator_other", KnownHostRevocationReason::OperatorOther),
        ] {
            let r = ReplaceHostKeyRequest {
                expected_old_fingerprint: "SHA256:OLD".to_owned(),
                expected_new_fingerprint: "SHA256:NEW".to_owned(),
                reason_code: tag.to_owned(),
            };
            let v = r.validated().unwrap();
            assert_eq!(v.reason_code, expected, "tag {tag} did not parse");
        }
    }
}
