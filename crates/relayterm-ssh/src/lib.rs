//! russh-backed SSH client. The live `russh::Channel` lives here and never
//! leaves the backend.
//!
//! Implementation is deferred. The crate exists so the orchestrator can
//! depend on a stable surface ahead of the SSH wiring.

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("ssh is not yet implemented")]
    NotImplemented,
}
