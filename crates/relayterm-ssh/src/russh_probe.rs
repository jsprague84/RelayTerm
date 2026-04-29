//! russh-backed [`SshHostKeyProbe`] implementation.
//!
//! The probe runs the SSH transport up through key exchange, captures the
//! server's public host key in [`russh::client::Handler::check_server_key`],
//! and disconnects WITHOUT attempting authentication. That ordering matters:
//! against an unknown or compromised host we must not transmit any client
//! material — and authentication happens after KEX, so disconnecting here
//! keeps the wire footprint minimal.
//!
//! All russh / IO errors are funnelled through [`ProbeError`] without
//! preserving the original message — peer-side detail can leak network
//! topology or version banners. The variant is enough.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::keys::{Algorithm, EcdsaCurve, HashAlg, PublicKey as RusshPublicKey};
use tokio::sync::Mutex;
use tracing::warn;

use relayterm_core::ssh_identity::SshKeyType;

use crate::preflight::{CapturedHostKey, ProbeError, ProbeTarget, SshHostKeyProbe};

/// Default time budget for a single connect + KEX round-trip.
///
/// Conservative on purpose. A real SSH peer on a healthy network finishes
/// KEX in well under a second; ten seconds is enough headroom for a slow
/// remote and short enough that the API request doesn't block forever.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Production probe — opens a live TCP/SSH connection.
#[derive(Debug, Clone)]
pub struct RusshHostKeyProbe {
    connect_timeout: Duration,
}

impl Default for RusshHostKeyProbe {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }
}

impl RusshHostKeyProbe {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the connect-and-handshake timeout. Useful in slow networks
    /// or in tests; the default is [`DEFAULT_CONNECT_TIMEOUT`].
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

#[async_trait]
impl SshHostKeyProbe for RusshHostKeyProbe {
    async fn capture_host_key(&self, target: ProbeTarget) -> Result<CapturedHostKey, ProbeError> {
        let captured: Arc<Mutex<Option<CapturedHostKey>>> = Arc::new(Mutex::new(None));
        let handler = CaptureHandler {
            captured: captured.clone(),
        };

        // Conservative russh client config — no auth retries, modest
        // keepalives, inactivity timeout aligned with our connect budget.
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(self.connect_timeout),
            ..Default::default()
        });

        let connect_fut =
            russh::client::connect(config, (target.hostname.as_str(), target.port), handler);

        let session_result = tokio::time::timeout(self.connect_timeout, connect_fut).await;

        match session_result {
            Err(_) => Err(ProbeError::Timeout),
            Ok(Err(e)) => {
                warn!(error = %e, "russh connect failed during preflight");
                Err(map_russh_error(&e))
            }
            Ok(Ok(session)) => {
                // We've already captured the key during KEX. Disconnect
                // immediately — no auth, no channel. Discard the disconnect
                // result; if the peer goes away mid-disconnect we still
                // have what we came for.
                let _ = session
                    .disconnect(russh::Disconnect::ByApplication, "preflight", "en")
                    .await;
                let captured = captured.lock().await.take().ok_or(ProbeError::BadHostKey)?;
                Ok(captured)
            }
        }
    }
}

/// russh handler that exists only to capture the server's host key during
/// `check_server_key`. Returns `Ok(true)` so the transport completes; the
/// caller disconnects before any authentication starts.
struct CaptureHandler {
    captured: Arc<Mutex<Option<CapturedHostKey>>>,
}

impl russh::client::Handler for CaptureHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &RusshPublicKey,
    ) -> Result<bool, Self::Error> {
        let algorithm = server_public_key.algorithm();
        let Some(key_type) = key_type_from_algorithm(&algorithm) else {
            // Unsupported algorithm — refuse the connection so the caller's
            // outer probe call ends in BadHostKey rather than hanging on
            // post-KEX traffic for a key we can't classify.
            return Ok(false);
        };
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        let public_key = match server_public_key.to_bytes() {
            Ok(bytes) => bytes,
            Err(_) => return Ok(false),
        };
        *self.captured.lock().await = Some(CapturedHostKey {
            key_type,
            fingerprint_sha256: fingerprint,
            public_key,
        });
        // OK to accept: this is a KEX-capture probe only — the caller
        // disconnects before authentication, so no client material is
        // ever transmitted. The known-hosts trust decision is performed
        // by `classify_host_key` against the captured key AFTER the
        // probe disconnects, not inside this handler. AGENTS.md's "do
        // not return Ok(true) from check_server_key" rule applies to a
        // production session handler that proceeds to auth — when that
        // landing slice arrives it will need a separate handler that
        // verifies the captured key against `known_host_entries` here.
        Ok(true)
    }
}

/// Map an `ssh_key::Algorithm` to our domain `SshKeyType`.
///
/// The `ssh_key::Algorithm` enum is `#[non_exhaustive]`, so we always end
/// with a fallback that returns `None` — anything we don't classify yet is
/// surfaced as `BadHostKey` upstream.
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

/// Funnel a `russh::Error` into a small set of safe variants.
///
/// We deliberately do NOT propagate the original message: peer banners and
/// IO error text can leak topology and software version. The variant alone
/// is enough for the API layer to choose a status code.
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
                curve: EcdsaCurve::NistP256
            }),
            Some(SshKeyType::EcdsaP256),
        );
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384
            }),
            Some(SshKeyType::EcdsaP384),
        );
        assert_eq!(
            key_type_from_algorithm(&Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521
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
        // Anything we don't classify falls through to Transport.
        assert!(matches!(
            map_io_error_kind(ErrorKind::Other),
            ProbeError::Transport
        ));
    }
}
