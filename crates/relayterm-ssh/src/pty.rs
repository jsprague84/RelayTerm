//! Live SSH PTY bridge.
//!
//! This module owns the *minimal* contract the orchestrator needs to drive
//! one interactive PTY-bearing SSH session: connect, verify the captured
//! host key against an accept-pin set, authenticate with public-key, open a
//! channel, request a PTY, start a shell, and expose async write/resize/
//! close plus an output channel.
//!
//! It does NOT own:
//! * Any axum, repository, or database type — the bridge is renderer- and
//!   route-neutral so the orchestrator can wire it without dragging
//!   transport concerns into this crate.
//! * Replay-ring or sequence-number bookkeeping — those live in the
//!   session orchestrator.
//! * Multi-attachment fanout — emitting bytes once into an mpsc and
//!   letting the manager broadcast keeps the SSH layer single-purpose.
//!
//! ## Security properties
//!
//! 1. The captured server host key is checked against `accept_pins` BEFORE
//!    any client signature reaches the wire. The russh handler returns
//!    `Ok(false)` from `check_server_key` on mismatch and the transport is
//!    torn down without auth.
//! 2. The decrypted private-key PEM lives only inside `SshPtyTarget` (and
//!    the russh internal parse) — wiped on drop via `Zeroizing`. The bridge
//!    never logs, echoes, or otherwise surfaces those bytes.
//! 3. Output bytes from the PTY are NEVER logged at any level. The driver
//!    task forwards them to the orchestrator's mpsc and that is the only
//!    consumer.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use relayterm_core::ssh_identity::SshKeyType;

use crate::preflight::ProbeError;

/// Hard outer time budget for one `start` call (connect + KEX + auth +
/// channel open + pty/shell). Layered defence: the russh-side per-call
/// timeouts already bound connect and auth; this is the absolute upper
/// limit so a stuck implementation can't block an HTTP request forever.
pub const DEFAULT_PTY_START_TIMEOUT: Duration = Duration::from_secs(20);

/// Capacity of the PTY-output mpsc returned by `start`. Bounded so a slow
/// consumer (e.g. a stalled WebSocket attachment) can backpressure the
/// SSH side rather than buffering unbounded bytes.
pub const DEFAULT_PTY_OUTPUT_CHANNEL_CAPACITY: usize = 256;

/// Capacity of the input mpsc consumed by the driver task. Single-digit
/// is fine: bytes are drained the moment the driver is awoken.
pub const DEFAULT_PTY_INPUT_CHANNEL_CAPACITY: usize = 64;

/// Capacity of the resize mpsc. Tiny on purpose — only the most recent
/// dims matter, but we don't coalesce so each request still hits the
/// remote `window_change`.
pub const DEFAULT_PTY_RESIZE_CHANNEL_CAPACITY: usize = 8;

/// Default `$TERM` advertised on `pty-req`. `xterm-256color` is the
/// modern compatibility baseline for shell apps; the renderer choice on
/// the client is independent of this string.
pub const DEFAULT_PTY_TERM: &str = "xterm-256color";

/// Static configuration for one bridge session.
#[derive(Debug, Clone)]
pub struct SshPtyConfig {
    pub hostname: String,
    pub port: u16,
    pub username: String,
    /// Active, trusted, non-revoked `(key_type, fingerprint_sha256)` pins
    /// the bridge MUST accept. Anything else aborts before auth.
    pub accept_pins: Vec<(SshKeyType, String)>,
    pub cols: u16,
    pub rows: u16,
    pub term: String,
    pub start_timeout: Duration,
}

impl SshPtyConfig {
    /// Build a config with the production defaults for `term` and
    /// `start_timeout`.
    #[must_use]
    pub fn new(
        hostname: String,
        port: u16,
        username: String,
        accept_pins: Vec<(SshKeyType, String)>,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            hostname,
            port,
            username,
            accept_pins,
            cols,
            rows,
            term: DEFAULT_PTY_TERM.to_owned(),
            start_timeout: DEFAULT_PTY_START_TIMEOUT,
        }
    }
}

/// Inputs to a bridge `start` call. Holds the decrypted PEM in a
/// zeroizing buffer so the plaintext is wiped from memory once the
/// target struct drops.
pub struct SshPtyTarget {
    pub config: SshPtyConfig,
    pub private_key_pem: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for SshPtyTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshPtyTarget")
            .field("config", &self.config)
            .field(
                "private_key_pem",
                &format_args!("<redacted: {} bytes>", self.private_key_pem.len()),
            )
            .finish()
    }
}

/// Events emitted on the bridge's output channel. The renderer-neutral
/// shape: raw stdout/stderr bytes, and lifecycle markers the orchestrator
/// uses to decide when to mark the session closed.
#[derive(Clone)]
pub enum SshPtyEvent {
    /// PTY stdout (and stderr — multiplexed) bytes from the remote shell.
    /// Raw bytes; the renderer is responsible for decoding/UTF-8.
    Output(Vec<u8>),
    /// Remote process delivered an explicit exit status.
    Exit { status: i32 },
    /// PTY/transport tore down. After this no more events arrive on the
    /// channel and the receiver will see `None` on the next `recv`.
    Closed { reason: ClosedReason },
}

impl std::fmt::Debug for SshPtyEvent {
    /// Output payloads are NEVER stringified at any level. Length-only is
    /// the canonical operator-facing redaction so a stray `{:?}` print in
    /// the orchestrator can't leak terminal bytes to logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Output(bytes) => f
                .debug_struct("Output")
                .field("len", &bytes.len())
                .field("data", &"<redacted pty output>")
                .finish(),
            Self::Exit { status } => f.debug_struct("Exit").field("status", status).finish(),
            Self::Closed { reason } => f.debug_struct("Closed").field("reason", reason).finish(),
        }
    }
}

/// Why the PTY/SSH transport stopped streaming. Operator-facing
/// classifier; never echoed to clients raw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedReason {
    /// Remote sent EOF / channel close cleanly.
    RemoteEof,
    /// Local close request (handle dropped or `close()` called).
    LocalClose,
    /// Transport-level error tore the channel down.
    TransportError,
}

/// Errors the bridge surfaces. Each variant maps to a small set of safe
/// HTTP statuses by the orchestrator/API layer.
#[derive(Debug, thiserror::Error)]
pub enum SshPtyError {
    /// Decrypted PEM bytes did not parse as an OpenSSH key. Vault data-
    /// integrity bug — same shape the auth-check service uses.
    #[error("ssh identity material is malformed")]
    InvalidIdentity,

    /// TCP/SSH transport failed before authentication. The `ProbeError`
    /// variant carries operator detail for tracing only — never reflected
    /// to the wire.
    #[error("ssh transport: {0}")]
    Transport(#[from] ProbeError),

    /// Captured server host key was not in `accept_pins`. Auth was NOT
    /// attempted and no client signature reached the wire.
    #[error("host key not trusted")]
    HostKeyNotTrusted,

    /// Public-key authentication completed but the server rejected the
    /// credential.
    #[error("ssh authentication failed")]
    AuthenticationFailed,

    /// PTY allocation or shell start failed at the SSH-channel layer.
    /// Maps to a typed startup-failed status at the API.
    #[error("ssh pty/shell start failed")]
    PtyStartFailed,

    /// Bridge is no longer running; tried to write input or resize on a
    /// dead handle.
    #[error("ssh pty bridge is closed")]
    BridgeClosed,
}

/// Output of a successful `start`.
///
/// `output_rx` is a single-consumer channel: the orchestrator owns it and
/// fans bytes out to attachments via its own broadcast surface. Splitting
/// fanout out of this crate keeps the SSH layer single-purpose and lets
/// tests drive the bridge without a broadcast topology.
///
/// `driver` is the [`tokio::task::JoinHandle`] for the bridge's
/// long-running driver task (russh impl: the channel multiplexer; fakes:
/// `None`). Held by the caller — typically the orchestrator — so it can
/// `abort()` the task on close instead of relying on channel-closure
/// teardown only. AGENTS.md: "use `JoinSet`/handles for dynamic
/// concurrency, never `tokio::spawn`-and-forget."
pub struct SshPtyStart {
    pub handle: Box<dyn SshPtyHandle>,
    pub output_rx: mpsc::Receiver<SshPtyEvent>,
    pub driver: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for SshPtyStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshPtyStart")
            .field("handle", &"<dyn SshPtyHandle>")
            .field("output_rx", &"<mpsc::Receiver>")
            .field(
                "driver",
                &self.driver.as_ref().map(|_| "<JoinHandle>").unwrap_or(""),
            )
            .finish()
    }
}

/// Live handle to a running PTY bridge. Cheap to share; the manager
/// keeps it behind `Arc` so multiple attachments route input through the
/// same handle.
///
/// Implementors MUST honour the redaction discipline: input bytes are
/// never logged or echoed, and `close` is idempotent.
#[async_trait]
pub trait SshPtyHandle: Send + Sync {
    /// Forward user input bytes to the remote PTY's stdin.
    async fn write_input(&self, bytes: Vec<u8>) -> Result<(), SshPtyError>;

    /// Apply a window-size change to the remote PTY.
    async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshPtyError>;

    /// Initiate a clean shutdown. Idempotent. Subsequent writes return
    /// [`SshPtyError::BridgeClosed`] and the output channel will see
    /// [`SshPtyEvent::Closed`] then `None` on the next `recv`.
    async fn close(&self);
}

/// Low-level operation: connect, verify host key, authenticate, open a
/// PTY, start a shell, and return a handle plus an output channel.
///
/// Implementations MUST NOT execute commands or spawn anything other
/// than the user's default login shell.
#[async_trait]
pub trait SshPtyBridge: Send + Sync {
    async fn start(&self, target: SshPtyTarget) -> Result<SshPtyStart, SshPtyError>;
}

#[async_trait]
impl<T: SshPtyBridge + ?Sized> SshPtyBridge for Arc<T> {
    async fn start(&self, target: SshPtyTarget) -> Result<SshPtyStart, SshPtyError> {
        (**self).start(target).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REDACT_OUTPUT_MARKER: &[u8] = b"REDACT-MARKER-PTY-OUTPUT-A91F";
    const REDACT_PEM_MARKER: &[u8] = b"REDACT-MARKER-PTY-PEM-A91F";

    #[test]
    fn target_debug_redacts_pem() {
        let config = SshPtyConfig::new(
            "h.example.com".to_owned(),
            22,
            "deploy".to_owned(),
            Vec::new(),
            80,
            24,
        );
        let mut pem = b"-----BEGIN OPENSSH PRIVATE KEY-----\n".to_vec();
        pem.extend_from_slice(REDACT_PEM_MARKER);
        pem.extend_from_slice(b"\n-----END OPENSSH PRIVATE KEY-----\n");
        let target = SshPtyTarget {
            config,
            private_key_pem: Zeroizing::new(pem),
        };
        let dbg = format!("{target:?}");
        assert!(
            !dbg.contains(std::str::from_utf8(REDACT_PEM_MARKER).unwrap()),
            "Debug must not echo PEM bytes: {dbg}",
        );
        assert!(
            dbg.contains("redacted"),
            "Debug must redact private_key_pem: {dbg}",
        );
    }

    #[test]
    fn output_event_debug_redacts_bytes() {
        let evt = SshPtyEvent::Output(REDACT_OUTPUT_MARKER.to_vec());
        let dbg = format!("{evt:?}");
        assert!(
            !dbg.contains(std::str::from_utf8(REDACT_OUTPUT_MARKER).unwrap()),
            "Debug must not echo PTY output: {dbg}",
        );
        assert!(
            dbg.contains("redacted"),
            "Debug must redact pty output: {dbg}",
        );
        assert!(
            dbg.contains("len"),
            "Debug should still surface length so logs are useful: {dbg}",
        );
    }
}
