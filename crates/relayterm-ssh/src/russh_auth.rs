//! russh-backed [`SshAuthChecker`] implementation.
//!
//! On each call: open the SSH transport, capture the server's host key in
//! [`russh::client::Handler::check_server_key`], compare it against the
//! caller-supplied accept-pin set, and EITHER attempt public-key
//! authentication OR refuse the connection. Either way the session is torn
//! down before this method returns — no PTY, no channel, no shell.
//!
//! This is the FIRST place in the codebase where the decrypted client
//! private key is actually presented to a network peer. The two security
//! properties we maintain here:
//!
//! 1. The host-key check happens BEFORE the auth signature is sent. If the
//!    captured key isn't in `accept_pins`, [`Handler::check_server_key`]
//!    returns `Ok(false)` and russh tears the transport down without ever
//!    reaching `authenticate_publickey`.
//! 2. All russh / IO errors are funnelled through [`ProbeError`] without
//!    preserving the original message — peer-side detail can leak network
//!    topology or version banners. Variant alone is enough.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::keys::{
    Algorithm, EcdsaCurve, HashAlg, PrivateKey as RusshPrivateKey, PrivateKeyWithHashAlg,
    PublicKey as RusshPublicKey,
};
use tokio::sync::Mutex;
use tracing::warn;

use relayterm_core::ssh_identity::SshKeyType;

use crate::auth_check::{AuthAttemptKind, AuthCheckOutcome, AuthCheckTarget, SshAuthChecker};
use crate::preflight::{CapturedHostKey, ProbeError};

/// Default time budget for connect + KEX + auth.
///
/// Conservative on purpose. Auth completes well under a second on a healthy
/// network; ten seconds is enough headroom for a slow remote and short
/// enough that the API request doesn't block forever.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Production checker — opens a live TCP/SSH connection.
#[derive(Debug, Clone)]
pub struct RusshAuthChecker {
    connect_timeout: Duration,
}

impl Default for RusshAuthChecker {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }
}

impl RusshAuthChecker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the connect-and-auth timeout. Useful in slow networks or in
    /// tests; the default is [`DEFAULT_CONNECT_TIMEOUT`].
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

#[async_trait]
impl SshAuthChecker for RusshAuthChecker {
    async fn run(&self, target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
        // russh embeds an internal fork of ssh-key, so we parse with russh's
        // crate here even though the service already round-tripped the bytes
        // through our own `ssh_key`. A failure here would mean the two
        // crates disagree on the format; surface as BadHostKey-adjacent
        // Transport rather than a panic.
        let private_key = RusshPrivateKey::from_openssh(target.private_key_pem.as_slice())
            .map_err(|_| ProbeError::Transport)?;
        let private_key = Arc::new(private_key);

        let captured: Arc<Mutex<Option<CapturedHostKey>>> = Arc::new(Mutex::new(None));
        let pin_match: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let handler = AuthHandler {
            captured: captured.clone(),
            pin_match: pin_match.clone(),
            accept_pins: target.accept_pins.clone(),
        };

        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(self.connect_timeout),
            ..Default::default()
        });

        let connect_fut =
            russh::client::connect(config, (target.hostname.as_str(), target.port), handler);
        let connect_result = tokio::time::timeout(self.connect_timeout, connect_fut).await;

        let mut session = match connect_result {
            Err(_) => return Err(ProbeError::Timeout),
            Ok(Err(e)) => {
                warn!(error = %e, "russh connect failed during auth-check");
                return Err(map_russh_error(&e));
            }
            Ok(Ok(s)) => s,
        };

        // If `check_server_key` rejected the host key, russh closes the
        // transport before this point — but russh::client::connect still
        // returns the session structure; the auth call below would then
        // produce a transport error. Detect the rejection up front via the
        // captured-pin flag the handler set.
        let captured_key = captured.lock().await.take();
        let pin_matched = *pin_match.lock().await;

        if !pin_matched {
            // The handler rejected the connection or never saw a key it
            // could classify. Disconnect best-effort and report mismatch.
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "auth-check", "en")
                .await;
            let captured_key = captured_key.ok_or(ProbeError::BadHostKey)?;
            return Ok(AuthCheckOutcome {
                captured: captured_key,
                kind: AuthAttemptKind::HostKeyMismatch,
            });
        }
        let captured_key = captured_key.ok_or(ProbeError::BadHostKey)?;

        // Host key matched. Attempt public-key auth.
        //
        // `PrivateKeyWithHashAlg::new(_, None)` defers to the server's
        // advertised hash-algorithm preference. For Ed25519 — the only
        // type the vault generates today — the field is ignored entirely.
        // If the vault grows an RSA generator, revisit this and pin to
        // `best_supported_rsa_hash()` so an old server doesn't silently
        // pick SHA-1.
        let auth_fut = session.authenticate_publickey(
            target.username.clone(),
            PrivateKeyWithHashAlg::new(private_key.clone(), None),
        );
        let auth_outcome = tokio::time::timeout(self.connect_timeout, auth_fut).await;

        // Always tear down the connection — best-effort, ignore errors.
        // The auth result is already in our hands.
        let kind = match auth_outcome {
            Err(_) => {
                // Auth call timed out; treat as a transport failure rather
                // than auth-failed since we can't say which.
                let _ = session
                    .disconnect(russh::Disconnect::ByApplication, "auth-check", "en")
                    .await;
                return Err(ProbeError::Timeout);
            }
            Ok(Err(e)) => {
                warn!(error = %e, "russh auth call failed during auth-check");
                let _ = session
                    .disconnect(russh::Disconnect::ByApplication, "auth-check", "en")
                    .await;
                return Err(map_russh_error(&e));
            }
            Ok(Ok(result)) => {
                if result.success() {
                    AuthAttemptKind::Authenticated
                } else {
                    AuthAttemptKind::AuthenticationFailed
                }
            }
        };

        let _ = session
            .disconnect(russh::Disconnect::ByApplication, "auth-check", "en")
            .await;

        Ok(AuthCheckOutcome {
            captured: captured_key,
            kind,
        })
    }
}

/// russh handler. Captures the server host key, compares against
/// `accept_pins`, and signals the outer call via a shared flag whether the
/// check passed.
struct AuthHandler {
    captured: Arc<Mutex<Option<CapturedHostKey>>>,
    pin_match: Arc<Mutex<bool>>,
    accept_pins: Vec<(SshKeyType, String)>,
}

impl russh::client::Handler for AuthHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &RusshPublicKey,
    ) -> Result<bool, Self::Error> {
        let algorithm = server_public_key.algorithm();
        let Some(key_type) = key_type_from_algorithm(&algorithm) else {
            // Unsupported algorithm — refuse the connection so the outer
            // call ends in BadHostKey rather than hanging on traffic for a
            // key we can't classify.
            return Ok(false);
        };
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        let public_key = match server_public_key.to_bytes() {
            Ok(bytes) => bytes,
            Err(_) => return Ok(false),
        };

        let matched = self
            .accept_pins
            .iter()
            .any(|(kt, fp)| *kt == key_type && fp == &fingerprint);

        *self.captured.lock().await = Some(CapturedHostKey {
            key_type,
            fingerprint_sha256: fingerprint,
            public_key,
        });
        *self.pin_match.lock().await = matched;

        // Returning Ok(false) terminates the SSH transport BEFORE auth, so
        // no client signature ever goes on the wire to a host whose key we
        // don't trust. That's the security-critical property of this path.
        Ok(matched)
    }
}

fn key_type_from_algorithm(alg: &Algorithm) -> Option<SshKeyType> {
    match alg {
        Algorithm::Ed25519 => Some(SshKeyType::Ed25519),
        Algorithm::Rsa { .. } => Some(SshKeyType::Rsa),
        Algorithm::Ecdsa { curve } => match curve {
            EcdsaCurve::NistP256 => Some(SshKeyType::EcdsaP256),
            EcdsaCurve::NistP384 => Some(SshKeyType::EcdsaP384),
            EcdsaCurve::NistP521 => Some(SshKeyType::EcdsaP521),
        },
        _ => None,
    }
}

fn map_russh_error(err: &russh::Error) -> ProbeError {
    if let russh::Error::IO(io) = err {
        return map_io_error_kind(io.kind());
    }
    ProbeError::Transport
}

fn map_io_error_kind(kind: std::io::ErrorKind) -> ProbeError {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::TimedOut => ProbeError::Timeout,
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionReset
        | ErrorKind::ConnectionAborted
        | ErrorKind::NotConnected
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable
        | ErrorKind::AddrNotAvailable => ProbeError::Unreachable,
        _ => ProbeError::Transport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algorithm_mapping_covers_supported_set() {
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Ed25519),
            Some(SshKeyType::Ed25519),
        );
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Rsa { hash: None }),
            Some(SshKeyType::Rsa),
        );
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            }),
            Some(SshKeyType::EcdsaP256),
        );
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            }),
            Some(SshKeyType::EcdsaP384),
        );
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521,
            }),
            Some(SshKeyType::EcdsaP521),
        );
    }

    #[test]
    fn io_error_kind_mapping() {
        use std::io::ErrorKind;
        assert!(matches!(
            map_io_error_kind(ErrorKind::TimedOut),
            ProbeError::Timeout
        ));
        assert!(matches!(
            map_io_error_kind(ErrorKind::ConnectionRefused),
            ProbeError::Unreachable
        ));
        assert!(matches!(
            map_io_error_kind(ErrorKind::ConnectionReset),
            ProbeError::Unreachable
        ));
        assert!(matches!(
            map_io_error_kind(ErrorKind::Other),
            ProbeError::Transport
        ));
    }
}
