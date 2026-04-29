//! russh-backed [`SshPtyBridge`] implementation.
//!
//! On `start`: open the SSH transport, capture the server's host key in
//! [`russh::client::Handler::check_server_key`], compare it against the
//! caller-supplied accept-pin set, attempt public-key authentication,
//! open a session channel, request a PTY, and start a shell. A driver
//! task then owns the channel for the life of the bridge — forwarding
//! `Data` and stderr-extended-data into the output mpsc, accepting
//! `Input` writes via an inbound mpsc, and applying `window_change` for
//! resize requests.
//!
//! Two security properties are maintained:
//!
//! 1. The host-key check happens BEFORE any auth signature is sent. If
//!    the captured key isn't in `accept_pins`, [`PtyHandler::check_server_key`]
//!    returns `Ok(false)` and russh tears the transport down without
//!    reaching `authenticate_publickey`.
//! 2. PTY input/output bytes never reach a tracing log at any level.
//!    The driver task forwards them through mpsc and that's the only
//!    side-effect.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::keys::{
    Algorithm, EcdsaCurve, HashAlg, PrivateKey as RusshPrivateKey, PrivateKeyWithHashAlg,
    PublicKey as RusshPublicKey,
};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

use relayterm_core::ssh_identity::SshKeyType;

use crate::preflight::{CapturedHostKey, ProbeError};
use crate::pty::{
    ClosedReason, DEFAULT_PTY_INPUT_CHANNEL_CAPACITY, DEFAULT_PTY_OUTPUT_CHANNEL_CAPACITY,
    DEFAULT_PTY_RESIZE_CHANNEL_CAPACITY, SshPtyBridge, SshPtyError, SshPtyEvent, SshPtyHandle,
    SshPtyStart, SshPtyTarget,
};

/// Default time budget for the per-step russh calls (connect, auth).
/// The outer [`crate::pty::DEFAULT_PTY_START_TIMEOUT`] is the absolute
/// upper bound on `start`; this is the per-step inner timeout so a stuck
/// individual call surfaces as `Timeout` instead of dragging out the
/// outer budget.
const DEFAULT_INNER_TIMEOUT: Duration = Duration::from_secs(10);

/// Production bridge — opens a live TCP/SSH connection.
#[derive(Debug, Clone)]
pub struct RusshPtyBridge {
    inner_timeout: Duration,
}

impl Default for RusshPtyBridge {
    fn default() -> Self {
        Self {
            inner_timeout: DEFAULT_INNER_TIMEOUT,
        }
    }
}

impl RusshPtyBridge {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the per-step inner timeout. Tests use this to shrink the
    /// budget; production callers should prefer [`Self::new`].
    #[must_use]
    pub fn with_inner_timeout(mut self, timeout: Duration) -> Self {
        self.inner_timeout = timeout;
        self
    }
}

#[async_trait]
impl SshPtyBridge for RusshPtyBridge {
    async fn start(&self, target: SshPtyTarget) -> Result<SshPtyStart, SshPtyError> {
        let SshPtyTarget {
            config,
            private_key_pem,
        } = target;

        // Parse the decrypted PEM with russh's internally-forked ssh-key
        // crate. A failure here means our `ssh_key` and russh's disagree
        // on shape — surface as InvalidIdentity so the API maps it to a
        // generic 500. The auth-check service follows the same shape.
        let private_key = RusshPrivateKey::from_openssh(private_key_pem.as_slice())
            .map_err(|_| SshPtyError::InvalidIdentity)?;
        // Drop the plaintext PEM as soon as russh has parsed it. The
        // Zeroizing buffer wipes the bytes on drop.
        drop(private_key_pem);
        let private_key = Arc::new(private_key);

        let captured: Arc<Mutex<Option<CapturedHostKey>>> = Arc::new(Mutex::new(None));
        let pin_match: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let handler = PtyHandler {
            captured: captured.clone(),
            pin_match: pin_match.clone(),
            accept_pins: config.accept_pins.clone(),
        };

        let russh_config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(self.inner_timeout),
            ..Default::default()
        });

        let connect_fut = russh::client::connect(
            russh_config,
            (config.hostname.as_str(), config.port),
            handler,
        );
        let connect_outcome = tokio::time::timeout(self.inner_timeout, connect_fut).await;

        let host_key_pinned = *pin_match.lock().await;

        let mut session = match connect_outcome {
            Err(_) => return Err(SshPtyError::Transport(ProbeError::Timeout)),
            Ok(Err(e)) => {
                // KEX may have failed because the handler refused the host
                // key. If `check_server_key` ran AND set `pin_match=false`,
                // surface that precisely; otherwise it's a genuine transport
                // failure.
                if !host_key_pinned && captured.lock().await.is_some() {
                    return Err(SshPtyError::HostKeyNotTrusted);
                }
                warn!(error = %e, "russh connect failed during pty start");
                return Err(SshPtyError::Transport(map_russh_error(&e)));
            }
            Ok(Ok(s)) => s,
        };

        if !host_key_pinned {
            // Defensive: connect succeeded but the handler reported no
            // pin match (only happens if russh returned the session
            // without honouring `Ok(false)` from check_server_key —
            // shouldn't happen, but we never auth against an untrusted
            // peer regardless). Disconnect and surface mismatch.
            let _ = session
                .disconnect(Disconnect::ByApplication, "host key not trusted", "en")
                .await;
            return Err(SshPtyError::HostKeyNotTrusted);
        }

        // Public-key auth. The PEM has been parsed and the bytes are
        // gone; from here on, only the russh-parsed `PrivateKey` is in
        // play, scoped to this scope.
        let auth_fut = session.authenticate_publickey(
            config.username.clone(),
            PrivateKeyWithHashAlg::new(private_key.clone(), None),
        );
        let auth_outcome = match tokio::time::timeout(self.inner_timeout, auth_fut).await {
            Err(_) => {
                let _ = session
                    .disconnect(Disconnect::ByApplication, "auth timeout", "en")
                    .await;
                return Err(SshPtyError::Transport(ProbeError::Timeout));
            }
            Ok(Err(e)) => {
                warn!(error = %e, "russh auth failed during pty start");
                let _ = session
                    .disconnect(Disconnect::ByApplication, "auth failed", "en")
                    .await;
                return Err(SshPtyError::Transport(map_russh_error(&e)));
            }
            Ok(Ok(result)) => result,
        };
        if !auth_outcome.success() {
            let _ = session
                .disconnect(Disconnect::ByApplication, "auth rejected", "en")
                .await;
            return Err(SshPtyError::AuthenticationFailed);
        }

        // Open a session channel, request a PTY, and start the user's
        // login shell. Any failure here tears the transport down before
        // we hand the handle to the caller.
        let channel = session.channel_open_session().await.map_err(|e| {
            warn!(error = %e, "channel_open_session failed");
            SshPtyError::PtyStartFailed
        })?;

        channel
            .request_pty(
                false,
                &config.term,
                u32::from(config.cols),
                u32::from(config.rows),
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| {
                warn!(error = %e, "request_pty failed");
                SshPtyError::PtyStartFailed
            })?;

        channel.request_shell(false).await.map_err(|e| {
            warn!(error = %e, "request_shell failed");
            SshPtyError::PtyStartFailed
        })?;

        // Build the in/out channels and spawn the driver task that owns
        // the SSH session + channel for the life of the bridge.
        let (output_tx, output_rx) = mpsc::channel(DEFAULT_PTY_OUTPUT_CHANNEL_CAPACITY);
        let (input_tx, input_rx) = mpsc::channel(DEFAULT_PTY_INPUT_CHANNEL_CAPACITY);
        let (resize_tx, resize_rx) = mpsc::channel(DEFAULT_PTY_RESIZE_CHANNEL_CAPACITY);

        let driver = tokio::spawn(drive_channel(
            session, channel, output_tx, input_rx, resize_rx,
        ));

        let handle = RusshHandle {
            input_tx: std::sync::Mutex::new(Some(input_tx)),
            resize_tx: std::sync::Mutex::new(Some(resize_tx)),
        };

        Ok(SshPtyStart {
            handle: Box::new(handle),
            output_rx,
            driver: Some(driver),
        })
    }
}

/// Live handle returned by [`RusshPtyBridge::start`].
///
/// Senders live behind a `std::sync::Mutex<Option<...>>` so `close()` can
/// drop them — that drops the receiver-side `None` in the driver task and
/// triggers a clean shutdown. Subsequent writes return `BridgeClosed`.
struct RusshHandle {
    input_tx: std::sync::Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    resize_tx: std::sync::Mutex<Option<mpsc::Sender<(u16, u16)>>>,
}

#[async_trait]
impl SshPtyHandle for RusshHandle {
    async fn write_input(&self, bytes: Vec<u8>) -> Result<(), SshPtyError> {
        let tx = self
            .input_tx
            .lock()
            .expect("input_tx mutex poisoned")
            .clone();
        let Some(tx) = tx else {
            return Err(SshPtyError::BridgeClosed);
        };
        tx.send(bytes).await.map_err(|_| SshPtyError::BridgeClosed)
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshPtyError> {
        let tx = self
            .resize_tx
            .lock()
            .expect("resize_tx mutex poisoned")
            .clone();
        let Some(tx) = tx else {
            return Err(SshPtyError::BridgeClosed);
        };
        tx.send((cols, rows))
            .await
            .map_err(|_| SshPtyError::BridgeClosed)
    }

    async fn close(&self) {
        // Dropping the senders signals the driver task that the handle
        // is gone (input_rx / resize_rx will return None). Idempotent:
        // a second take returns None and we no-op.
        self.input_tx
            .lock()
            .expect("input_tx mutex poisoned")
            .take();
        self.resize_tx
            .lock()
            .expect("resize_tx mutex poisoned")
            .take();
    }
}

/// Driver task: owns the russh session and channel for the life of the
/// bridge. Multiplexes:
/// * channel events → `output_tx` (Output / Exit / Closed)
/// * `input_rx` recv → `channel.data(...)`
/// * `resize_rx` recv → `channel.window_change(...)`
///
/// Exits on (a) channel EOF / close, (b) both senders dropped (handle
/// gone), (c) a transport error.
async fn drive_channel(
    session: russh::client::Handle<PtyHandler>,
    mut channel: russh::Channel<russh::client::Msg>,
    output_tx: mpsc::Sender<SshPtyEvent>,
    mut input_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<(u16, u16)>,
) {
    let close_reason: ClosedReason = loop {
        tokio::select! {
            // Inbound channel events (PTY output, exit status, EOF).
            maybe_msg = channel.wait() => {
                match maybe_msg {
                    Some(ChannelMsg::Data { data }) => {
                        if output_tx
                            .send(SshPtyEvent::Output(data.to_vec()))
                            .await
                            .is_err()
                        {
                            break ClosedReason::LocalClose;
                        }
                    }
                    // ext == 1 is stderr in the SSH connection protocol.
                    // Multiplex with stdout — RelayTerm's protocol is
                    // single-channel and the renderer doesn't distinguish.
                    Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                        if output_tx
                            .send(SshPtyEvent::Output(data.to_vec()))
                            .await
                            .is_err()
                        {
                            break ClosedReason::LocalClose;
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        // i32 cast: russh emits u32 for parity with the
                        // wire field; shells use small non-negative codes
                        // and even a sign-flipping cast loses no info
                        // operators care about for diagnostics.
                        #[allow(clippy::cast_possible_wrap)]
                        let _ = output_tx
                            .send(SshPtyEvent::Exit {
                                status: exit_status as i32,
                            })
                            .await;
                        // Keep the loop running until EOF / close so we
                        // don't drop final output bytes that may follow
                        // the exit-status message.
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) => {
                        break ClosedReason::RemoteEof;
                    }
                    Some(_) => { /* request-success / open-failure / ignore */ }
                    None => {
                        break ClosedReason::TransportError;
                    }
                }
            }
            // Outbound input bytes from the handle.
            maybe_input = input_rx.recv() => {
                match maybe_input {
                    Some(bytes) => {
                        if let Err(e) = channel.data(&bytes[..]).await {
                            warn!(error = %e, "channel.data failed; tearing down pty");
                            break ClosedReason::TransportError;
                        }
                    }
                    None => {
                        // Senders gone. Handle was dropped (or close()
                        // was called). Tear down cleanly.
                        break ClosedReason::LocalClose;
                    }
                }
            }
            // Resize requests.
            maybe_resize = resize_rx.recv() => {
                match maybe_resize {
                    Some((cols, rows)) => {
                        if let Err(e) = channel
                            .window_change(u32::from(cols), u32::from(rows), 0, 0)
                            .await
                        {
                            // Resize is best-effort; a failure here doesn't
                            // tear down the shell. The renderer is expected
                            // to refit on its own.
                            warn!(error = %e, cols = cols, rows = rows, "window_change failed");
                        }
                    }
                    None => {
                        // Resize half closed; input is also closing as
                        // part of the same shutdown sequence. Exit so we
                        // don't spin-loop on a permanently-None branch.
                        break ClosedReason::LocalClose;
                    }
                }
            }
        }
    };

    // Best-effort teardown. Failures here are diagnostic only — the
    // session is already going away. Send the final Closed event so the
    // orchestrator knows to mark the row.
    let _ = output_tx
        .send(SshPtyEvent::Closed {
            reason: close_reason,
        })
        .await;
    let _ = channel.eof().await;
    let _ = channel.close().await;
    let _ = session
        .disconnect(Disconnect::ByApplication, "relayterm pty close", "en")
        .await;
}

/// russh handler. Captures the server host key, compares against
/// `accept_pins`, and signals the outer call via shared flags whether
/// the check passed.
struct PtyHandler {
    captured: Arc<Mutex<Option<CapturedHostKey>>>,
    pin_match: Arc<Mutex<bool>>,
    accept_pins: Vec<(SshKeyType, String)>,
}

impl russh::client::Handler for PtyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &RusshPublicKey,
    ) -> Result<bool, Self::Error> {
        let algorithm = server_public_key.algorithm();
        let Some(key_type) = key_type_from_algorithm(&algorithm) else {
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
