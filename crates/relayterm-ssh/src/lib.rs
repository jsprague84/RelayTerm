//! SSH client surface.
//!
//! This crate is intentionally split into two layers:
//!
//! * [`preflight`] — pure decision logic: validate the identity, classify
//!   a captured host key against the host's pinned entries. No network.
//! * [`russh_probe`] — network-side implementation of the
//!   [`SshHostKeyProbe`] trait, backed by `russh`. Gated behind the trait
//!   so the preflight service can be exercised in unit tests without
//!   spinning up an SSH server.
//!
//! A live `russh::Channel`, PTY orchestration, and the reconnect/replay
//! buffer all belong to a later slice — they are deliberately NOT part of
//! this surface.

pub mod preflight;
pub mod russh_probe;

pub use preflight::{
    CapturedHostKey, HostKeyPreflightError, HostKeyPreflightRequest, HostKeyPreflightResult,
    HostKeyPreflightService, HostKeyStatus, ProbeError, ProbeTarget, SshHostKeyProbe,
    classify_host_key,
};
pub use russh_probe::RusshHostKeyProbe;
