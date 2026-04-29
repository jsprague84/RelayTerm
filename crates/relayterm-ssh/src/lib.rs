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
//!
//! A live `russh::Channel`, PTY orchestration, and the reconnect/replay
//! buffer all belong to a later slice — they are deliberately NOT part of
//! this surface.

pub mod auth_check;
pub mod preflight;
pub mod russh_auth;
pub mod russh_probe;

pub use auth_check::{
    AuthAttemptKind, AuthCheckOutcome, AuthCheckTarget, SshAuthCheckError, SshAuthCheckRequest,
    SshAuthCheckResult, SshAuthCheckService, SshAuthCheckStatus, SshAuthChecker,
};
pub use preflight::{
    CapturedHostKey, HostKeyPreflightError, HostKeyPreflightRequest, HostKeyPreflightResult,
    HostKeyPreflightService, HostKeyStatus, ProbeError, ProbeTarget, SshHostKeyProbe,
    classify_host_key,
};
pub use russh_auth::RusshAuthChecker;
pub use russh_probe::RusshHostKeyProbe;
