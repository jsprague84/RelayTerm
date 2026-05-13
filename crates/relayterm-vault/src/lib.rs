//! Encrypted key vault.
//!
//! Stores backend-issued SSH private keys at rest. Decrypted bytes are
//! produced inside the vault on demand and dropped immediately afterwards.
//! No plaintext private key bytes ever appear in `Debug` output, log lines,
//! or API responses — every secret-bearing type in this crate has a manual
//! redacted `Debug` impl and zeroizes its memory on drop.
//!
//! # Crypto design (v1)
//!
//! * Keypair: Ed25519, generated via [`ssh-key`].
//! * Public key: emitted in OpenSSH `authorized_keys` text form.
//! * Private key: serialized as OpenSSH PEM-encoded text inside the
//!   ciphertext blob.
//! * Encryption: XChaCha20-Poly1305 AEAD with a 32-byte master key supplied
//!   by the operator. The master key is *used directly* as the AEAD key in
//!   v1 — there is no per-record subkey derivation. v2 may add HKDF-SHA256
//!   sub-derivation; the on-disk envelope already carries a version byte so
//!   readers can dispatch.
//! * Blob layout (opaque from the caller's perspective):
//!
//!   ```text
//!   [magic: b"RTV1" | 4]
//!   [version: u8    | 1]    // 0x01 = XChaCha20Poly1305 master-key direct
//!   [nonce: 24]              // 24-byte XChaCha20 nonce, sourced from OsRng
//!   [ciphertext + 16-byte Poly1305 tag]
//!   ```
//!
//! v1 is intentionally small and local. OpenBao / external KMS integration
//! is out of scope for this slice; when added, it slots in as a new version
//! byte without breaking blobs already at rest.
//!
//! # Operator setup
//!
//! `vault.master_key_b64` must decode to **exactly 32 random bytes** — it
//! is the AEAD key, not a human password. There is no key-stretching
//! step (no Argon2/scrypt) on this path, so a low-entropy passphrase
//! would weaken the ciphertext directly. Generate one with:
//!
//! ```sh
//! openssl rand -base64 32
//! ```
//!
//! `vault.master_key_file` is the same value, in a file. Either source
//! must resolve at boot or the backend refuses to start; there is no
//! random-key fallback because that would orphan every previously stored
//! ciphertext after a restart.

mod cipher;
mod identity;
mod master_key;

pub use cipher::EncryptedBlob;
pub use identity::{GeneratedSshIdentity, VaultService};
pub use master_key::{MasterKeyError, VaultMasterKey};

/// Errors surfaced by the vault.
///
/// Variants intentionally do NOT carry secret-bearing values — operator
/// logs render the discriminant only. The wrapped strings are short,
/// generic descriptions safe to surface in error responses.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The configured master key is missing, malformed, or the wrong size.
    /// The wrapped detail must NEVER include the key value or any prefix
    /// of it; only the structural reason (length / encoding) is recorded.
    #[error("vault master key invalid: {0}")]
    MasterKey(#[from] MasterKeyError),

    /// Keypair generation failed inside the SSH-key library.
    #[error("ssh keypair generation failed")]
    KeyGeneration,

    /// Public-key serialization to OpenSSH text form failed.
    #[error("ssh public key serialization failed")]
    PublicKeySerialization,

    /// Private-key serialization to OpenSSH text form failed.
    #[error("ssh private key serialization failed")]
    PrivateKeySerialization,

    /// AEAD encryption returned an error. This should be effectively
    /// unreachable for valid inputs; treat as an internal bug.
    #[error("vault encrypt failed")]
    Encrypt,

    /// AEAD decryption rejected the ciphertext — wrong key, tampered blob,
    /// or wrong version byte.
    #[error("vault decrypt failed")]
    Decrypt,

    /// Stored blob did not match the expected envelope shape (bad magic,
    /// truncated header, unknown version).
    #[error("vault blob format invalid")]
    BlobFormat,

    /// Caller asked for a key algorithm the vault does not generate yet.
    /// Currently only Ed25519 is supported.
    #[error("ssh key type not supported by vault: {0}")]
    UnsupportedKeyType(&'static str),

    /// Imported PEM bytes parsed but the format is not one the vault accepts.
    /// `reason` is a closed, short, operator-safe discriminant — never raw
    /// parser text. Today's reasons: `"encrypted"` (passphrase-protected),
    /// `"malformed"` (PEM envelope was valid but the openssh-key-v1 body
    /// failed to parse). The third reason `"not_a_private_key"` is reserved
    /// for a future surface that detects public-key-only material; the v1
    /// DTO rejects missing `BEGIN OPENSSH PRIVATE KEY` headers before the
    /// vault is ever called, so the variant is unreachable today.
    #[error("ssh key format not supported by vault: {reason}")]
    UnsupportedFormat { reason: &'static str },
}
