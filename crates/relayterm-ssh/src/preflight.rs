//! Host-key preflight: connect, capture the server host key during KEX,
//! classify against the pinned known-host entries, and return a structured
//! status the API layer can map to a safe response.
//!
//! ## Scope of this preflight (what it proves and does NOT prove)
//!
//! A successful response from [`HostKeyPreflightService::preflight`]
//! attests **only** to the following:
//!
//! 1. The TCP+SSH transport reached the target far enough for the server
//!    to present a host key during KEX.
//! 2. That host key was captured, fingerprinted, and classified against
//!    the host's pinned `known_host_entries` rows.
//! 3. The configured SSH identity decrypts and parses as a valid OpenSSH
//!    private key (round-trip from vault → PEM).
//!
//! It does **NOT** prove:
//!
//! * SSH authentication would succeed against this server.
//! * The configured identity is installed in `authorized_keys`.
//! * A PTY can be allocated, a shell can be spawned, or a session can be
//!   opened.
//!
//! Auth-side validation belongs to a separate, later route. This service
//! disconnects immediately after KEX precisely so that no client material
//! is ever sent to a host whose key is `Unknown` or `Changed`.

use std::sync::Arc;

use async_trait::async_trait;
use zeroize::Zeroizing;

use relayterm_core::ids::HostId;
use relayterm_core::known_host::KnownHostEntry;
use relayterm_core::ssh_identity::SshKeyType;

/// Public host-key bytes captured during the SSH transport handshake.
///
/// The bytes here are entirely public material — the server's own public
/// key plus its SHA-256 fingerprint. Nothing from the client identity is
/// in this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedHostKey {
    pub key_type: SshKeyType,
    /// `SHA256:<base64>` fingerprint, exactly the form `ssh-keygen -lf`
    /// emits.
    pub fingerprint_sha256: String,
    /// OpenSSH wire-format public key bytes.
    pub public_key: Vec<u8>,
}

/// Trust state of the captured host key against the host's pinned entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// No active known-host entry matches the captured key.
    Unknown,
    /// An active known-host entry matches the captured fingerprint AND has
    /// `trusted_at` set.
    Trusted,
    /// An active known-host entry exists for the same key type with a
    /// DIFFERENT fingerprint. This is the MITM signal — never auto-trust.
    Changed,
}

impl HostKeyStatus {
    /// Lowercase wire tag (`unknown`, `trusted`, `changed`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Trusted => "trusted",
            Self::Changed => "changed",
        }
    }
}

/// Inputs to a preflight call.
///
/// `private_key_pem` is held in a zeroizing buffer so the plaintext is wiped
/// from memory once the request struct drops. The service does not log,
/// echo, or otherwise expose these bytes.
pub struct HostKeyPreflightRequest {
    pub host_id: HostId,
    pub hostname: String,
    pub port: u16,
    pub username: String,
    /// OpenSSH PEM-encoded private key (decrypted from the vault). This is
    /// validated for parseability but is NOT used to authenticate during
    /// preflight — we never proceed past the host-key handshake. Auth-side
    /// verification is a separate, later slice.
    pub private_key_pem: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for HostKeyPreflightRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostKeyPreflightRequest")
            .field("host_id", &self.host_id)
            .field("hostname", &self.hostname)
            .field("port", &self.port)
            .field("username", &self.username)
            .field(
                "private_key_pem",
                &format_args!("<redacted: {} bytes>", self.private_key_pem.len()),
            )
            .finish()
    }
}

/// Output of a preflight call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPreflightResult {
    pub captured: CapturedHostKey,
    pub status: HostKeyStatus,
}

/// Errors surfaced by the preflight service.
///
/// Variants intentionally do NOT carry secret-bearing detail. The wrapped
/// strings on `Internal` are short, generic operator-facing messages safe
/// for tracing logs but not surfaced verbatim on the wire.
#[derive(Debug, thiserror::Error)]
pub enum HostKeyPreflightError {
    /// The decrypted private-key bytes did not parse as an OpenSSH key.
    /// Treated as a data-integrity bug — a row that the vault produced
    /// should always round-trip.
    #[error("ssh identity material is malformed")]
    InvalidIdentity,

    /// The host-key probe failed.
    #[error("ssh probe failed: {0}")]
    Probe(#[from] ProbeError),
}

/// Errors a host-key probe may surface. Each variant is mapped to a small
/// set of safe HTTP statuses by the API layer.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// TCP connection refused, DNS resolution failed, or the network was
    /// unreachable. Maps to 502.
    #[error("ssh peer unreachable")]
    Unreachable,

    /// The TCP connect or SSH handshake exceeded the configured timeout.
    /// Maps to 502.
    #[error("ssh handshake timed out")]
    Timeout,

    /// The server presented a host key the probe could not interpret
    /// (unsupported algorithm, wire-format error, or the handler refused).
    /// Maps to 502.
    #[error("ssh server presented invalid host key")]
    BadHostKey,

    /// Generic SSH transport failure (KEX rejected, malformed packet, peer
    /// closed early). Maps to 502.
    #[error("ssh transport error")]
    Transport,
}

/// Low-level capture trait: connect, run the SSH transport up through host
/// key exchange, capture the public key, disconnect.
///
/// Implementations MUST NOT proceed past KEX — no auth, no channels — so a
/// preflight against an untrusted host never transmits client material.
#[async_trait]
pub trait SshHostKeyProbe: Send + Sync {
    async fn capture_host_key(&self, target: ProbeTarget) -> Result<CapturedHostKey, ProbeError>;
}

/// Network-level target for a probe.
#[derive(Debug, Clone)]
pub struct ProbeTarget {
    pub hostname: String,
    pub port: u16,
}

/// Coordinates a preflight: validates the identity material, asks the probe
/// for the host key, and classifies it against the supplied known-host
/// entries. Repository access stays in the API layer; this service is
/// pure-input / pure-output so the same code path is exercised in both
/// HTTP integration tests and unit tests with a fake probe.
pub struct HostKeyPreflightService {
    probe: Arc<dyn SshHostKeyProbe>,
}

impl std::fmt::Debug for HostKeyPreflightService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostKeyPreflightService")
            .field("probe", &"<dyn SshHostKeyProbe>")
            .finish()
    }
}

impl HostKeyPreflightService {
    #[must_use]
    pub fn new(probe: Arc<dyn SshHostKeyProbe>) -> Self {
        Self { probe }
    }

    /// Run a full preflight: parse the identity, probe the host key, and
    /// classify it against `known`.
    pub async fn preflight(
        &self,
        req: HostKeyPreflightRequest,
        known: &[KnownHostEntry],
    ) -> Result<HostKeyPreflightResult, HostKeyPreflightError> {
        // Validate that the decrypted blob is a real OpenSSH PEM. We don't
        // use the parsed key here — auth is a later slice — but a bad parse
        // means the vault row is corrupt and we want to surface that as
        // `InvalidIdentity` instead of silently probing without it.
        ssh_key::PrivateKey::from_openssh(req.private_key_pem.as_slice())
            .map_err(|_| HostKeyPreflightError::InvalidIdentity)?;

        let captured = self
            .probe
            .capture_host_key(ProbeTarget {
                hostname: req.hostname,
                port: req.port,
            })
            .await?;

        let status = classify_host_key(&captured, known);
        Ok(HostKeyPreflightResult { captured, status })
    }
}

/// Pure classification of a captured host key against the host's pinned
/// entries. Extracted as a free function so the decision logic is unit
/// testable without an SSH stack.
///
/// Rules:
/// 1. If any active (non-revoked) entry matches both `key_type` and
///    `fingerprint_sha256`, return `Trusted` if `trusted_at` is set,
///    otherwise `Unknown` (a row exists but the user has not confirmed it).
/// 2. Otherwise, if any active entry exists for the same `key_type` with a
///    different fingerprint, return `Changed` — that is the MITM signal.
/// 3. Otherwise, return `Unknown`.
///
/// Revoked entries are excluded from all checks: a revoked match must NOT
/// resurrect itself by reappearing on the wire.
#[must_use]
pub fn classify_host_key(captured: &CapturedHostKey, known: &[KnownHostEntry]) -> HostKeyStatus {
    let active = || known.iter().filter(|e| e.revoked_at.is_none());

    if let Some(matching) = active().find(|e| {
        e.key_type == captured.key_type && e.fingerprint_sha256 == captured.fingerprint_sha256
    }) {
        return if matching.trusted_at.is_some() {
            HostKeyStatus::Trusted
        } else {
            HostKeyStatus::Unknown
        };
    }

    if active().any(|e| {
        e.key_type == captured.key_type && e.fingerprint_sha256 != captured.fingerprint_sha256
    }) {
        return HostKeyStatus::Changed;
    }

    HostKeyStatus::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use relayterm_core::ids::{HostId, KnownHostEntryId};

    fn captured_ed25519(fp: &str) -> CapturedHostKey {
        CapturedHostKey {
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-ed25519 AAAA-host".to_vec(),
        }
    }

    fn entry(
        host_id: HostId,
        key_type: SshKeyType,
        fp: &str,
        trusted: bool,
        revoked: bool,
    ) -> KnownHostEntry {
        KnownHostEntry {
            id: KnownHostEntryId::new(),
            host_id,
            key_type,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-* PUB".to_vec(),
            first_seen_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            trusted_at: trusted.then(|| Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap()),
            revoked_at: revoked.then(|| Utc.with_ymd_and_hms(2026, 1, 3, 0, 0, 0).unwrap()),
        }
    }

    #[test]
    fn classify_unknown_when_table_empty() {
        let captured = captured_ed25519("SHA256:abc");
        assert_eq!(
            classify_host_key(&captured, &[]),
            HostKeyStatus::Unknown,
            "an empty table is the canonical first-time-seen state",
        );
    }

    #[test]
    fn classify_trusted_when_matching_fp_is_trusted() {
        let host = HostId::new();
        let captured = captured_ed25519("SHA256:abc");
        let known = vec![entry(host, SshKeyType::Ed25519, "SHA256:abc", true, false)];
        assert_eq!(classify_host_key(&captured, &known), HostKeyStatus::Trusted);
    }

    #[test]
    fn classify_unknown_when_matching_fp_not_trusted() {
        // A row exists but the user never confirmed it; treat as Unknown so
        // the trust flow re-runs.
        let host = HostId::new();
        let captured = captured_ed25519("SHA256:abc");
        let known = vec![entry(host, SshKeyType::Ed25519, "SHA256:abc", false, false)];
        assert_eq!(classify_host_key(&captured, &known), HostKeyStatus::Unknown);
    }

    #[test]
    fn classify_changed_when_same_key_type_different_fp() {
        let host = HostId::new();
        let captured = captured_ed25519("SHA256:NEW");
        let known = vec![entry(host, SshKeyType::Ed25519, "SHA256:OLD", true, false)];
        assert_eq!(classify_host_key(&captured, &known), HostKeyStatus::Changed);
    }

    #[test]
    fn classify_unknown_when_only_other_key_types_pinned() {
        // Server presents an ed25519 key; the host has only an RSA pin
        // recorded (e.g., for a different listener). Not a "Changed" event.
        let host = HostId::new();
        let captured = captured_ed25519("SHA256:abc");
        let known = vec![entry(host, SshKeyType::Rsa, "SHA256:rsa-fp", true, false)];
        assert_eq!(classify_host_key(&captured, &known), HostKeyStatus::Unknown);
    }

    #[test]
    fn classify_ignores_revoked_entries() {
        // A revoked-and-matching entry must NOT resurrect to Trusted, and a
        // revoked-and-different entry must NOT count as Changed.
        let host = HostId::new();
        let captured = captured_ed25519("SHA256:abc");
        let revoked_match = entry(host, SshKeyType::Ed25519, "SHA256:abc", true, true);
        let revoked_other = entry(host, SshKeyType::Ed25519, "SHA256:OLD", true, true);
        assert_eq!(
            classify_host_key(&captured, std::slice::from_ref(&revoked_match)),
            HostKeyStatus::Unknown,
        );
        assert_eq!(
            classify_host_key(&captured, &[revoked_other]),
            HostKeyStatus::Unknown,
        );
    }

    #[test]
    fn classify_changed_takes_priority_over_unknown_for_other_key_types() {
        let host = HostId::new();
        let captured = captured_ed25519("SHA256:NEW");
        let known = vec![
            entry(host, SshKeyType::Rsa, "SHA256:rsa", true, false),
            entry(host, SshKeyType::Ed25519, "SHA256:OLD", true, false),
        ];
        assert_eq!(classify_host_key(&captured, &known), HostKeyStatus::Changed);
    }

    /// Fake probe used in the preflight service's own unit tests.
    struct FakeProbe(CapturedHostKey);

    #[async_trait]
    impl SshHostKeyProbe for FakeProbe {
        async fn capture_host_key(
            &self,
            _target: ProbeTarget,
        ) -> Result<CapturedHostKey, ProbeError> {
            Ok(self.0.clone())
        }
    }

    /// Probe that always errors — exercises the error path.
    struct ErrorProbe(ProbeError);

    #[async_trait]
    impl SshHostKeyProbe for ErrorProbe {
        async fn capture_host_key(
            &self,
            _target: ProbeTarget,
        ) -> Result<CapturedHostKey, ProbeError> {
            Err(match &self.0 {
                ProbeError::Unreachable => ProbeError::Unreachable,
                ProbeError::Timeout => ProbeError::Timeout,
                ProbeError::BadHostKey => ProbeError::BadHostKey,
                ProbeError::Transport => ProbeError::Transport,
            })
        }
    }

    fn well_formed_pem() -> Zeroizing<Vec<u8>> {
        // Generate a real OpenSSH PEM via ssh-key directly so the parse
        // step inside `preflight()` succeeds.
        use rand::rngs::OsRng;
        use ssh_key::{Algorithm, LineEnding, PrivateKey};
        let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
        Zeroizing::new(key.to_openssh(LineEnding::LF).unwrap().as_bytes().to_vec())
    }

    fn req(host_id: HostId, pem: Zeroizing<Vec<u8>>) -> HostKeyPreflightRequest {
        HostKeyPreflightRequest {
            host_id,
            hostname: "example.com".to_owned(),
            port: 22,
            username: "deploy".to_owned(),
            private_key_pem: pem,
        }
    }

    #[tokio::test]
    async fn service_returns_unknown_against_empty_table() {
        let host_id = HostId::new();
        let captured = captured_ed25519("SHA256:abc");
        let svc = HostKeyPreflightService::new(Arc::new(FakeProbe(captured.clone())));
        let result = svc
            .preflight(req(host_id, well_formed_pem()), &[])
            .await
            .unwrap();
        assert_eq!(result.captured, captured);
        assert_eq!(result.status, HostKeyStatus::Unknown);
    }

    #[tokio::test]
    async fn service_classifies_against_known_entries() {
        let host_id = HostId::new();
        let captured = captured_ed25519("SHA256:abc");
        let svc = HostKeyPreflightService::new(Arc::new(FakeProbe(captured.clone())));
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            "SHA256:abc",
            true,
            false,
        )];
        let result = svc
            .preflight(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(result.status, HostKeyStatus::Trusted);
    }

    #[tokio::test]
    async fn service_rejects_malformed_identity_before_probing() {
        // A garbage PEM must short-circuit with InvalidIdentity. The probe
        // must not be called — verified by passing an ErrorProbe whose
        // failure would otherwise dominate.
        let host_id = HostId::new();
        let svc = HostKeyPreflightService::new(Arc::new(ErrorProbe(ProbeError::Unreachable)));
        let bad = Zeroizing::new(
            b"-----BEGIN OPENSSH PRIVATE KEY-----\ngarbage\n-----END OPENSSH PRIVATE KEY-----\n"
                .to_vec(),
        );
        let err = svc.preflight(req(host_id, bad), &[]).await.unwrap_err();
        assert!(matches!(err, HostKeyPreflightError::InvalidIdentity));
    }

    #[tokio::test]
    async fn service_propagates_probe_errors() {
        let host_id = HostId::new();
        let svc = HostKeyPreflightService::new(Arc::new(ErrorProbe(ProbeError::Timeout)));
        let err = svc
            .preflight(req(host_id, well_formed_pem()), &[])
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            HostKeyPreflightError::Probe(ProbeError::Timeout)
        ));
    }

    #[test]
    fn host_key_status_wire_tags_are_stable() {
        // Wire tags are part of the public API contract — guard them.
        assert_eq!(HostKeyStatus::Unknown.as_str(), "unknown");
        assert_eq!(HostKeyStatus::Trusted.as_str(), "trusted");
        assert_eq!(HostKeyStatus::Changed.as_str(), "changed");
    }
}
