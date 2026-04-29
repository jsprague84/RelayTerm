//! Encrypted key vault.
//!
//! Stores backend-issued SSH private keys at rest. Decrypted bytes never
//! cross a boundary — they are produced inside the SSH session task and
//! dropped immediately after the SSH handshake.
//!
//! Implementation is deferred; this crate currently exposes only the trait
//! shape so callers can compile against it.

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault is not yet implemented")]
    NotImplemented,
}
