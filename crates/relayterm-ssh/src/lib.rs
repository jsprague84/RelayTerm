//! SSH client surface.
//!
//! This crate is split into pure decision modules and network-side
//! implementations behind traits, so every higher-level service can be
//! unit-tested without spinning up an SSH server:
//!
//! * [`preflight`] — KEX-only host-key probe and classification.
//! * [`russh_probe`] — russh-backed implementation of [`SshHostKeyProbe`].
//! * [`auth_check`] — authenticated SSH credential check (no PTY, no shell).
//! * [`russh_auth`] — russh-backed implementation of [`SshAuthChecker`].
//! * [`pty`] — minimal SSH PTY bridge: connect, host-key pin verify,
//!   public-key auth, request PTY, start shell, and async write/resize/
//!   close + an output channel of PTY bytes.
//! * [`russh_pty`] — russh-backed implementation of [`SshPtyBridge`].
//!
//! Replay-ring / sequence-number bookkeeping and the multi-attachment
//! fanout topology live in the orchestrator; this crate's job is the
//! single PTY-bearing transport.

pub mod auth_check;
pub mod preflight;
pub mod pty;
pub mod russh_auth;
pub mod russh_probe;
pub mod russh_pty;

pub use auth_check::{
    AuthAttemptKind, AuthCheckOutcome, AuthCheckTarget, SshAuthCheckError, SshAuthCheckRequest,
    SshAuthCheckResult, SshAuthCheckService, SshAuthCheckStatus, SshAuthChecker,
};
pub use preflight::{
    CapturedHostKey, HostKeyPreflightError, HostKeyPreflightRequest, HostKeyPreflightResult,
    HostKeyPreflightService, HostKeyStatus, ProbeError, ProbeTarget, SshHostKeyProbe,
    classify_host_key,
};
pub use pty::{
    ClosedReason, DEFAULT_PTY_INPUT_CHANNEL_CAPACITY, DEFAULT_PTY_OUTPUT_CHANNEL_CAPACITY,
    DEFAULT_PTY_RESIZE_CHANNEL_CAPACITY, DEFAULT_PTY_START_TIMEOUT, DEFAULT_PTY_TERM, SshPtyBridge,
    SshPtyConfig, SshPtyError, SshPtyEvent, SshPtyHandle, SshPtyStart, SshPtyTarget,
};
pub use russh_auth::RusshAuthChecker;
pub use russh_probe::RusshHostKeyProbe;
pub use russh_pty::RusshPtyBridge;
