//! Authenticated SSH credential check.
//!
//! This module proves only that:
//!
//! 1. The TCP+SSH transport reaches the target.
//! 2. The captured server host key matches an active, non-revoked, trusted
//!    `known_host_entries` row for the host (host-key-trust precondition).
//! 3. The decrypted private key parses as a valid OpenSSH key.
//! 4. SSH public-key authentication succeeds (or fails) against the server.
//!
//! It explicitly does NOT:
//!
//! * open a PTY,
//! * execute any command,
//! * spawn a shell,
//! * keep the connection around for any other purpose.
//!
//! The connection is torn down as soon as the auth result is known. No
//! interactive session is created and nothing on the client side is exposed
//! to the server beyond what a normal `authenticate_publickey` round-trip
//! would send.
//!
//! Host-key trust is treated as a precondition that MUST hold before any
//! authentication is attempted. The flow is:
//!
//! * If the host's `known_host_entries` does not contain at least one
//!   active, trusted, non-revoked row matching the captured key, the
//!   service short-circuits with `HostKeyUnknown` or `HostKeyChanged`
//!   per the existing classifier — never with an auth attempt.
//! * The auth checker is given the set of acceptable
//!   `(key_type, fingerprint)` pins; if the server presents any other
//!   key, the checker reports `HostKeyMismatch` instead of authenticating.
//!   This is a TOCTOU defence against the host's key flipping between
//!   the read of `known_host_entries` and the SSH connection.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use relayterm_core::ids::HostId;
use relayterm_core::known_host::KnownHostEntry;
use relayterm_core::ssh_identity::SshKeyType;

use crate::preflight::{CapturedHostKey, ProbeError, classify_host_key};

/// Hard outer time budget for one auth-check call.
///
/// Layered defence: the russh-side checker already times out connect and
/// auth individually (see `russh_auth::DEFAULT_CONNECT_TIMEOUT`). This is
/// the absolute upper bound the service enforces around `checker.run(...)`
/// regardless of which checker is plugged in — a stuck or buggy
/// implementation can never block an HTTP request indefinitely. Set a few
/// seconds above the russh budget so the inner timeout fires first under
/// normal flow and we get clean ProbeError mapping.
pub const DEFAULT_AUTH_CHECK_TIMEOUT: Duration = Duration::from_secs(25);

/// Default maximum number of concurrent auth-checks across the process.
///
/// Auth-check is the first user-driven outbound network surface: a single
/// caller with a tight POST loop would otherwise open arbitrary
/// concurrent SSH transports against a target. A small permit pool caps
/// blast radius without needing per-user rate-limit infrastructure.
/// Saturated callers see a `Saturated` error which the API layer maps to
/// 503 — the request is safe to retry.
pub const DEFAULT_MAX_CONCURRENT_AUTH_CHECKS: usize = 4;

/// Inputs to an auth-check call.
///
/// `private_key_pem` is held in a zeroizing buffer so the plaintext is wiped
/// from memory when the request struct drops. The service does not log,
/// echo, or otherwise surface these bytes — auth-check is the FIRST place in
/// the codebase where the decrypted private key is actually used to sign an
/// SSH authentication request, so the redaction discipline matters.
pub struct SshAuthCheckRequest {
    pub host_id: HostId,
    pub hostname: String,
    pub port: u16,
    pub username: String,
    /// OpenSSH PEM-encoded private key (decrypted from the vault).
    pub private_key_pem: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for SshAuthCheckRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshAuthCheckRequest")
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

/// Wire-stable status of an auth-check.
///
/// This is the operator-facing diagnostic: each variant maps to a single
/// short, static message in the API layer. Variants are deliberately small
/// in number and broad in meaning — auth-check is a "did this work?"
/// endpoint, not a structured error catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAuthCheckStatus {
    /// SSH transport completed, the host key matched a trusted pin, and
    /// public-key authentication succeeded.
    AuthenticationSucceeded,
    /// SSH transport completed and the host key matched a trusted pin, but
    /// the server rejected the key for `username`.
    AuthenticationFailed,
    /// No active, trusted, non-revoked `known_host_entries` row matches the
    /// captured host key. Auth was NOT attempted.
    HostKeyUnknown,
    /// An active, non-revoked entry exists for the same key type with a
    /// different fingerprint. Auth was NOT attempted.
    HostKeyChanged,
    /// The TCP/SSH transport itself failed (refused, timeout, malformed
    /// peer). Auth was NOT attempted.
    ConnectionFailed,
}

impl SshAuthCheckStatus {
    /// Lowercase wire tag used by the HTTP layer.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthenticationSucceeded => "authentication_succeeded",
            Self::AuthenticationFailed => "authentication_failed",
            Self::HostKeyUnknown => "host_key_unknown",
            Self::HostKeyChanged => "host_key_changed",
            Self::ConnectionFailed => "connection_failed",
        }
    }
}

/// Output of an auth-check call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAuthCheckResult {
    pub status: SshAuthCheckStatus,
}

/// Errors surfaced by the auth-check service that indicate a server-side
/// problem (not an auth diagnostic). These are mapped to 5xx by the API
/// layer; a normal "auth failed" / "host key changed" outcome is NOT an
/// error and lives in [`SshAuthCheckStatus`].
#[derive(Debug, thiserror::Error)]
pub enum SshAuthCheckError {
    /// The decrypted private-key bytes did not parse as an OpenSSH key.
    /// Treated as a data-integrity bug — a row that the vault produced
    /// should always round-trip.
    #[error("ssh identity material is malformed")]
    InvalidIdentity,

    /// Concurrency permit pool was full — the process is already running
    /// the configured maximum number of auth-checks. Mapped to 503 by the
    /// API layer; the wire body is the static `service unavailable`
    /// string so no operator detail leaks.
    #[error("auth-check concurrency limit reached")]
    Saturated,
}

/// What the [`SshAuthChecker`] saw on a single connect-and-attempt-auth run.
///
/// The captured host key is always present except when the network call
/// could not produce one (in which case the implementation returns
/// [`ProbeError`] and the service reports `ConnectionFailed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheckOutcome {
    pub captured: CapturedHostKey,
    pub kind: AuthAttemptKind,
}

/// Distinguishes the three "the network call completed" outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthAttemptKind {
    /// Host key matched an accepted pin AND `authenticate_publickey`
    /// returned success.
    Authenticated,
    /// Host key matched an accepted pin but the server rejected the key.
    AuthenticationFailed,
    /// Host key did NOT match any accepted pin. Auth was not attempted.
    HostKeyMismatch,
}

/// Network-level target for an auth-check. The acceptable pins are pre-
/// computed by the service from `known_host_entries`; the checker uses them
/// to short-circuit the connection if the captured key isn't on the list.
///
/// `private_key_pem` is the OpenSSH PEM the checker re-parses. We pass raw
/// PEM (rather than a typed `PrivateKey`) because russh embeds an internal
/// fork of `ssh-key` and accepts only its own `PrivateKey` type — the
/// service-level parse already validated the bytes round-trip in our
/// `ssh-key` build, and the russh impl parses again with the fork it
/// understands.
#[derive(Clone)]
pub struct AuthCheckTarget {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    /// Active, trusted, non-revoked `(key_type, fingerprint_sha256)` pins
    /// the checker MUST accept. Anything else → [`AuthAttemptKind::HostKeyMismatch`].
    pub accept_pins: Vec<(SshKeyType, String)>,
    /// OpenSSH PEM-encoded private key bytes. The buffer wipes itself when
    /// the target drops, so the plaintext lifetime matches the network
    /// call.
    pub private_key_pem: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for AuthCheckTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCheckTarget")
            .field("hostname", &self.hostname)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("accept_pins", &self.accept_pins)
            .field(
                "private_key_pem",
                &format_args!("<redacted: {} bytes>", self.private_key_pem.len()),
            )
            .finish()
    }
}

/// Low-level network operation: connect, verify the captured host key
/// matches one of `accept_pins`, attempt public-key authentication if it
/// does, and disconnect.
///
/// Implementations MUST NOT request a PTY, execute commands, open any
/// channels, or otherwise hold the connection past the auth result.
#[async_trait]
pub trait SshAuthChecker: Send + Sync {
    async fn run(&self, target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError>;
}

/// Coordinates an auth-check: parses the identity, classifies the host's
/// trust state from `known_host_entries`, builds the acceptable-pins list,
/// and asks the checker to verify the server. Repository access stays in
/// the API layer; this service is pure-input / pure-output so the same code
/// path runs in both HTTP integration tests and unit tests with a fake.
///
/// The service owns two safety bounds:
///
/// * a hard outer timeout around `checker.run(...)` — exceeding it maps
///   to [`SshAuthCheckStatus::ConnectionFailed`].
/// * a [`Semaphore`] capping concurrent in-flight auth-checks; new
///   callers past the limit get [`SshAuthCheckError::Saturated`].
///
/// Both are configurable via [`Self::with_limits`]; the production
/// constructor uses [`DEFAULT_AUTH_CHECK_TIMEOUT`] and
/// [`DEFAULT_MAX_CONCURRENT_AUTH_CHECKS`].
pub struct SshAuthCheckService {
    checker: Arc<dyn SshAuthChecker>,
    timeout: Duration,
    semaphore: Arc<Semaphore>,
}

impl std::fmt::Debug for SshAuthCheckService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshAuthCheckService")
            .field("checker", &"<dyn SshAuthChecker>")
            .field("timeout", &self.timeout)
            .field("available_permits", &self.semaphore.available_permits())
            .finish()
    }
}

impl SshAuthCheckService {
    /// Construct a service using the default timeout and concurrency cap.
    #[must_use]
    pub fn new(checker: Arc<dyn SshAuthChecker>) -> Self {
        Self::with_limits(
            checker,
            DEFAULT_AUTH_CHECK_TIMEOUT,
            DEFAULT_MAX_CONCURRENT_AUTH_CHECKS,
        )
    }

    /// Construct a service with explicit safety bounds. Tests use this to
    /// exercise the timeout and saturation paths without waiting for the
    /// production defaults; production callers should prefer [`Self::new`].
    #[must_use]
    pub fn with_limits(
        checker: Arc<dyn SshAuthChecker>,
        timeout: Duration,
        max_concurrent: usize,
    ) -> Self {
        // `Semaphore::new(0)` is technically valid but would deadlock every
        // call. Treat zero as a configuration bug and clamp to one — the
        // service still rejects every concurrent call past the first, which
        // is the spirit of "max_concurrent = 0" without the deadlock.
        let permits = max_concurrent.max(1);
        Self {
            checker,
            timeout,
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Run a full auth-check against the given request and the host's
    /// pinned `known_host_entries`.
    pub async fn auth_check(
        &self,
        req: SshAuthCheckRequest,
        known: &[KnownHostEntry],
    ) -> Result<SshAuthCheckResult, SshAuthCheckError> {
        // 1. Round-trip parse the decrypted blob. A bad parse means the
        //    vault row is corrupt; surface it as `InvalidIdentity` so the
        //    API layer maps it to a generic 500. The russh-side checker
        //    re-parses the same bytes using its own (internally-forked)
        //    `ssh-key`; the parse here is the authoritative shape check
        //    for the rest of the codebase.
        ssh_key::PrivateKey::from_openssh(req.private_key_pem.as_slice())
            .map_err(|_| SshAuthCheckError::InvalidIdentity)?;

        // 2. Build the active-trusted pin set. `classify_host_key` filters
        //    revoked rows; we mirror the same predicate here so the two
        //    halves of "what counts as trusted" can never disagree.
        let accept_pins: Vec<(SshKeyType, String)> = known
            .iter()
            .filter(|e| e.revoked_at.is_none() && e.trusted_at.is_some())
            .map(|e| (e.key_type, e.fingerprint_sha256.clone()))
            .collect();

        // 3. Acquire the concurrency permit BEFORE spending any network
        //    budget. `try_acquire` is non-blocking — if every slot is in
        //    use we surface `Saturated` immediately and the caller (the
        //    operator UI) can choose to retry. The permit drops when this
        //    function returns, releasing the slot regardless of outcome.
        let _permit = self
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| SshAuthCheckError::Saturated)?;

        // 4. Run the network call under a hard outer timeout. The russh
        //    checker has its own connect/auth timeouts; this one is the
        //    last line of defence so a stuck checker (real or fake) can
        //    never block an HTTP request indefinitely. Both timeout and
        //    inner ProbeError collapse to `ConnectionFailed` on the wire —
        //    they're indistinguishable to the operator (no auth happened,
        //    retry later) and a typed status lets the operator UI render
        //    a meaningful diagnostic without a 5xx.
        let target = AuthCheckTarget {
            hostname: req.hostname,
            port: req.port,
            username: req.username,
            accept_pins,
            private_key_pem: req.private_key_pem,
        };
        let outcome = match tokio::time::timeout(self.timeout, self.checker.run(target)).await {
            Err(_elapsed) => {
                // Outer timeout fired. The checker is still running in the
                // background but `_permit` drops here, releasing its slot.
                // No auth was attempted from this caller's perspective.
                return Ok(SshAuthCheckResult {
                    status: SshAuthCheckStatus::ConnectionFailed,
                });
            }
            Ok(Err(_probe_err)) => {
                // ProbeError variants are deliberately not surfaced here —
                // they are diagnostic operator detail, but the wire signal
                // a caller acts on is "connection failed." Variants are
                // logged operator-side via the checker's own tracing.
                return Ok(SshAuthCheckResult {
                    status: SshAuthCheckStatus::ConnectionFailed,
                });
            }
            Ok(Ok(o)) => o,
        };

        // 4. Map the network outcome into the wire status. `HostKeyMismatch`
        //    needs further classification (`Unknown` vs `Changed`) so the
        //    operator-facing status is precise. We do that with the same
        //    classifier the preflight surface uses, against the captured
        //    key the checker handed back.
        let status = match outcome.kind {
            AuthAttemptKind::Authenticated => SshAuthCheckStatus::AuthenticationSucceeded,
            AuthAttemptKind::AuthenticationFailed => SshAuthCheckStatus::AuthenticationFailed,
            AuthAttemptKind::HostKeyMismatch => match classify_host_key(&outcome.captured, known) {
                crate::preflight::HostKeyStatus::Changed => SshAuthCheckStatus::HostKeyChanged,
                // `Trusted` here would mean the classifier disagrees with
                // the accept-pins predicate — they share the same filter
                // (`active && trusted && !revoked`), so this branch is
                // unreachable in practice. Map to `Unknown` defensively
                // rather than panic: the auth attempt didn't happen and
                // no client material went on the wire.
                crate::preflight::HostKeyStatus::Trusted
                | crate::preflight::HostKeyStatus::Unknown => SshAuthCheckStatus::HostKeyUnknown,
            },
        };

        Ok(SshAuthCheckResult { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rand::rngs::OsRng;
    use relayterm_core::ids::{HostId, KnownHostEntryId};
    use ssh_key::{Algorithm, LineEnding, PrivateKey};
    use std::sync::Mutex;

    const REDACT_MARKER: &[u8] = b"REDACT-MARKER-SSH-3C7E";

    fn well_formed_pem() -> Zeroizing<Vec<u8>> {
        let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).unwrap();
        Zeroizing::new(key.to_openssh(LineEnding::LF).unwrap().as_bytes().to_vec())
    }

    fn captured(fp: &str) -> CapturedHostKey {
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
            revoked_by: None,
            revoked_reason_code: None,
            replaced_by_id: None,
        }
    }

    fn req(host_id: HostId, pem: Zeroizing<Vec<u8>>) -> SshAuthCheckRequest {
        SshAuthCheckRequest {
            host_id,
            hostname: "host.example.com".to_owned(),
            port: 22,
            username: "deploy".to_owned(),
            private_key_pem: pem,
        }
    }

    /// Fake checker driven by a configured outcome. Records every call so
    /// tests can prove the network call did or did not happen.
    struct FakeChecker {
        outcome: AuthCheckOutcome,
        calls: Arc<Mutex<Vec<AuthCheckTarget>>>,
    }

    impl FakeChecker {
        fn new(outcome: AuthCheckOutcome) -> (Arc<Self>, Arc<Mutex<Vec<AuthCheckTarget>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Arc::new(Self {
                    outcome,
                    calls: calls.clone(),
                }),
                calls,
            )
        }
    }

    #[async_trait]
    impl SshAuthChecker for FakeChecker {
        async fn run(&self, target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
            self.calls.lock().unwrap().push(target);
            Ok(self.outcome.clone())
        }
    }

    /// Checker that always errors — exercises the `ConnectionFailed` path.
    struct ErroringChecker(ProbeError);

    #[async_trait]
    impl SshAuthChecker for ErroringChecker {
        async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
            Err(match &self.0 {
                ProbeError::Unreachable => ProbeError::Unreachable,
                ProbeError::Timeout => ProbeError::Timeout,
                ProbeError::BadHostKey => ProbeError::BadHostKey,
                ProbeError::Transport => ProbeError::Transport,
            })
        }
    }

    #[tokio::test]
    async fn succeeds_when_host_key_trusted_and_auth_ok() {
        let host_id = HostId::new();
        let captured_fp = "SHA256:trusted-fp";
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            captured_fp,
            true,
            false,
        )];

        let (checker, calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured(captured_fp),
            kind: AuthAttemptKind::Authenticated,
        });
        let svc = SshAuthCheckService::new(checker);

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::AuthenticationSucceeded);

        // The checker received the trusted pin.
        let target = calls.lock().unwrap()[0].clone();
        assert_eq!(target.accept_pins.len(), 1);
        assert_eq!(target.accept_pins[0].0, SshKeyType::Ed25519);
        assert_eq!(target.accept_pins[0].1, captured_fp);
        assert_eq!(target.username, "deploy");
    }

    #[tokio::test]
    async fn host_key_unknown_blocks_auth_attempt() {
        // No trusted entry at all → the checker is still called (so we can
        // tell unknown apart from changed via the captured key), but the
        // checker reports HostKeyMismatch and the classifier maps to Unknown.
        let host_id = HostId::new();
        let known: Vec<KnownHostEntry> = vec![];

        let (checker, _calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured("SHA256:fresh-fp"),
            kind: AuthAttemptKind::HostKeyMismatch,
        });
        let svc = SshAuthCheckService::new(checker);

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::HostKeyUnknown);
    }

    #[tokio::test]
    async fn host_key_changed_blocks_auth_attempt() {
        // OLD entry pinned, server presents NEW. Checker says mismatch
        // because NEW isn't in accept_pins. The classifier sees an active
        // entry with different fp for the same key_type → Changed.
        let host_id = HostId::new();
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            "SHA256:OLD-fp",
            true,
            false,
        )];

        let (checker, calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured("SHA256:NEW-fp"),
            kind: AuthAttemptKind::HostKeyMismatch,
        });
        let svc = SshAuthCheckService::new(checker);

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::HostKeyChanged);

        // The checker received the OLD trusted pin (NOT the captured NEW fp)
        // — which is precisely how it knew to reject the connection.
        let target = calls.lock().unwrap()[0].clone();
        assert_eq!(target.accept_pins[0].1, "SHA256:OLD-fp");
    }

    #[tokio::test]
    async fn revoked_match_blocks_auth_attempt() {
        // The matching entry is revoked. accept_pins must be empty (revoked
        // is excluded), so even though the captured fp matches the revoked
        // row, the checker can never authenticate.
        let host_id = HostId::new();
        let captured_fp = "SHA256:revoked-fp";
        let known = vec![entry(host_id, SshKeyType::Ed25519, captured_fp, true, true)];

        let (checker, calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured(captured_fp),
            kind: AuthAttemptKind::HostKeyMismatch,
        });
        let svc = SshAuthCheckService::new(checker);

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::HostKeyUnknown);

        let target = calls.lock().unwrap()[0].clone();
        assert!(
            target.accept_pins.is_empty(),
            "revoked-only entries must not enter accept_pins, got: {:?}",
            target.accept_pins,
        );
    }

    #[tokio::test]
    async fn untrusted_match_blocks_auth_attempt() {
        // Row exists with the captured fp but `trusted_at` was never
        // stamped — accept_pins must be empty, so the checker reports
        // mismatch. Status: Unknown (not authenticated).
        let host_id = HostId::new();
        let captured_fp = "SHA256:untrusted-fp";
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            captured_fp,
            false,
            false,
        )];

        let (checker, calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured(captured_fp),
            kind: AuthAttemptKind::HostKeyMismatch,
        });
        let svc = SshAuthCheckService::new(checker);

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::HostKeyUnknown);
        assert!(calls.lock().unwrap()[0].accept_pins.is_empty());
    }

    #[tokio::test]
    async fn authentication_failure_returns_typed_status() {
        let host_id = HostId::new();
        let captured_fp = "SHA256:trusted-fp";
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            captured_fp,
            true,
            false,
        )];

        let (checker, _) = FakeChecker::new(AuthCheckOutcome {
            captured: captured(captured_fp),
            kind: AuthAttemptKind::AuthenticationFailed,
        });
        let svc = SshAuthCheckService::new(checker);

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::AuthenticationFailed);
    }

    #[tokio::test]
    async fn connection_failure_maps_to_typed_status() {
        let host_id = HostId::new();
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            "SHA256:any",
            true,
            false,
        )];

        let svc = SshAuthCheckService::new(Arc::new(ErroringChecker(ProbeError::Unreachable)));
        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::ConnectionFailed);
    }

    #[tokio::test]
    async fn malformed_private_key_returns_invalid_identity_without_calling_checker() {
        // The checker would error out if it ran — so reaching it would
        // mask the InvalidIdentity verdict.
        let host_id = HostId::new();
        let svc = SshAuthCheckService::new(Arc::new(ErroringChecker(ProbeError::Unreachable)));

        let bad = Zeroizing::new(
            b"-----BEGIN OPENSSH PRIVATE KEY-----\ngarbage\n-----END OPENSSH PRIVATE KEY-----\n"
                .to_vec(),
        );
        let err = svc.auth_check(req(host_id, bad), &[]).await.unwrap_err();
        assert!(matches!(err, SshAuthCheckError::InvalidIdentity));
    }

    #[tokio::test]
    async fn auth_check_request_debug_redacts_private_key_pem() {
        // Holds a sentinel inside the PEM so an accidental {:?} print could
        // be detected. The Debug impl must not echo it.
        let host_id = HostId::new();
        let mut pem_bytes = b"-----BEGIN OPENSSH PRIVATE KEY-----\n".to_vec();
        pem_bytes.extend_from_slice(REDACT_MARKER);
        pem_bytes.extend_from_slice(b"\n-----END OPENSSH PRIVATE KEY-----\n");
        let request = SshAuthCheckRequest {
            host_id,
            hostname: "h.example.com".to_owned(),
            port: 22,
            username: "deploy".to_owned(),
            private_key_pem: Zeroizing::new(pem_bytes),
        };
        let dbg = format!("{request:?}");
        assert!(
            !dbg.contains(std::str::from_utf8(REDACT_MARKER).unwrap()),
            "Debug must not echo PEM bytes: {dbg}",
        );
        assert!(
            dbg.contains("redacted"),
            "Debug must redact private_key_pem: {dbg}",
        );
    }

    #[test]
    fn status_wire_tags_are_stable() {
        // Wire tags are part of the public API contract — guard them.
        assert_eq!(
            SshAuthCheckStatus::AuthenticationSucceeded.as_str(),
            "authentication_succeeded",
        );
        assert_eq!(
            SshAuthCheckStatus::AuthenticationFailed.as_str(),
            "authentication_failed",
        );
        assert_eq!(
            SshAuthCheckStatus::HostKeyUnknown.as_str(),
            "host_key_unknown",
        );
        assert_eq!(
            SshAuthCheckStatus::HostKeyChanged.as_str(),
            "host_key_changed",
        );
        assert_eq!(
            SshAuthCheckStatus::ConnectionFailed.as_str(),
            "connection_failed",
        );
    }

    /// Checker that sleeps for a configured duration before producing an
    /// outcome. Used to exercise the outer timeout.
    struct SlowChecker {
        delay: Duration,
    }

    #[async_trait]
    impl SshAuthChecker for SlowChecker {
        async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
            tokio::time::sleep(self.delay).await;
            Ok(AuthCheckOutcome {
                captured: captured("SHA256:should-not-reach"),
                kind: AuthAttemptKind::Authenticated,
            })
        }
    }

    /// Checker that blocks on a `Notify` until the test releases it. Lets
    /// a saturation test pin one slot in flight while a second call
    /// races for the next permit.
    struct BlockingChecker {
        gate: Arc<tokio::sync::Notify>,
        captured: CapturedHostKey,
        kind: AuthAttemptKind,
    }

    #[async_trait]
    impl SshAuthChecker for BlockingChecker {
        async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
            self.gate.notified().await;
            Ok(AuthCheckOutcome {
                captured: self.captured.clone(),
                kind: self.kind,
            })
        }
    }

    #[tokio::test]
    async fn outer_timeout_maps_to_connection_failed() {
        // Checker sleeps for 500ms; service timeout is 50ms. Real wall
        // clock — but the assertion fires the moment the outer timeout
        // does, so the test bounds itself to ~50ms regardless of the
        // checker's sleep budget.
        let host_id = HostId::new();
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            "SHA256:any",
            true,
            false,
        )];

        let svc = SshAuthCheckService::with_limits(
            Arc::new(SlowChecker {
                delay: Duration::from_millis(500),
            }),
            Duration::from_millis(50),
            DEFAULT_MAX_CONCURRENT_AUTH_CHECKS,
        );

        let res = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap();
        assert_eq!(res.status, SshAuthCheckStatus::ConnectionFailed);
    }

    #[tokio::test]
    async fn saturated_when_no_permit_is_available() {
        // max_concurrent = 1; the first call holds the permit until we
        // notify it; the second call must error with `Saturated` rather
        // than block. We drive both through the same service instance.
        let host_id = HostId::new();
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            "SHA256:any",
            true,
            false,
        )];

        let gate = Arc::new(tokio::sync::Notify::new());
        let checker = Arc::new(BlockingChecker {
            gate: gate.clone(),
            captured: captured("SHA256:any"),
            kind: AuthAttemptKind::Authenticated,
        });
        let svc = Arc::new(SshAuthCheckService::with_limits(
            checker,
            Duration::from_secs(60),
            1,
        ));

        // Spawn the first call; it will park on the gate.
        let svc_first = svc.clone();
        let known_first = known.clone();
        let first = tokio::spawn(async move {
            svc_first
                .auth_check(req(host_id, well_formed_pem()), &known_first)
                .await
        });

        // Wait until the first call is past `try_acquire_owned`. Looping
        // on `available_permits` is more robust than a fixed sleep — the
        // task scheduler decides when the spawn actually runs.
        for _ in 0..100 {
            if svc.semaphore.available_permits() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            svc.semaphore.available_permits(),
            0,
            "first call should have grabbed the only permit",
        );

        // Second call: the semaphore is empty → Saturated.
        let err = svc
            .auth_check(req(host_id, well_formed_pem()), &known)
            .await
            .unwrap_err();
        assert!(
            matches!(err, SshAuthCheckError::Saturated),
            "expected Saturated, got: {err:?}",
        );

        // Release the first call so the test exits cleanly.
        gate.notify_one();
        let first_res = first.await.unwrap().unwrap();
        assert_eq!(
            first_res.status,
            SshAuthCheckStatus::AuthenticationSucceeded
        );
    }

    #[tokio::test]
    async fn permit_is_released_after_call_completes() {
        // After a normal call returns, the slot is back in the pool so a
        // subsequent call against the same service does not see Saturated.
        let host_id = HostId::new();
        let captured_fp = "SHA256:permit-recycled";
        let known = vec![entry(
            host_id,
            SshKeyType::Ed25519,
            captured_fp,
            true,
            false,
        )];

        let (checker, _calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured(captured_fp),
            kind: AuthAttemptKind::Authenticated,
        });
        let svc = SshAuthCheckService::with_limits(checker, Duration::from_secs(60), 1);

        for _ in 0..3 {
            let res = svc
                .auth_check(req(host_id, well_formed_pem()), &known)
                .await
                .unwrap();
            assert_eq!(res.status, SshAuthCheckStatus::AuthenticationSucceeded);
        }
        assert_eq!(
            svc.semaphore.available_permits(),
            1,
            "the single permit should be back after every successful call",
        );
    }

    #[test]
    fn with_limits_clamps_zero_concurrency_to_one() {
        // `max_concurrent = 0` would deadlock every caller. The
        // constructor clamps to one so the service still behaves like a
        // strict serial queue without the deadlock.
        let (checker, _calls) = FakeChecker::new(AuthCheckOutcome {
            captured: captured("SHA256:any"),
            kind: AuthAttemptKind::Authenticated,
        });
        let svc = SshAuthCheckService::with_limits(checker, Duration::from_secs(1), 0);
        assert_eq!(svc.semaphore.available_permits(), 1);
    }
}
