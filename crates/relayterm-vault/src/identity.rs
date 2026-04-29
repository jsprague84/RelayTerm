//! High-level identity-creation API.
//!
//! `VaultService` is what the API layer talks to: hand it an owner-supplied
//! key type, get back a public-key-shaped record plus an opaque ciphertext
//! suitable for `INSERT INTO ssh_identities`. The plaintext private key is
//! never returned to callers.

use std::fmt;

use rand::rngs::OsRng;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};
use zeroize::Zeroizing;

use relayterm_core::ssh_identity::SshKeyType;

use crate::VaultError;
use crate::cipher::{self, EncryptedBlob};
use crate::master_key::VaultMasterKey;

/// Result of generating a new SSH identity.
///
/// The struct exposes only data that is safe to persist, return to the
/// caller, or surface in API responses. The plaintext private key never
/// leaves [`VaultService::generate_ssh_identity`] — only its encrypted form.
#[derive(Clone)]
pub struct GeneratedSshIdentity {
    /// Algorithm used for the keypair.
    pub key_type: SshKeyType,
    /// OpenSSH-format public key bytes (the `authorized_keys` line as
    /// ASCII, without trailing newline). Safe to expose.
    pub public_key_openssh: Vec<u8>,
    /// SHA-256 fingerprint of the public key, matching the
    /// `SHA256:<base64>` format `ssh-keygen -lf` emits.
    pub fingerprint_sha256: String,
    /// Encrypted private-key blob. Treat as opaque; persist as-is.
    pub encrypted_private_key: EncryptedBlob,
}

impl fmt::Debug for GeneratedSshIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedSshIdentity")
            .field("key_type", &self.key_type)
            .field("public_key_len", &self.public_key_openssh.len())
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("encrypted_private_key", &self.encrypted_private_key)
            .finish()
    }
}

/// Vault service: holds the master key, generates and decrypts SSH
/// identities. Cheap to clone — the inner key is `Clone` and zeroizing.
/// Axum clones `AppState` per request, so multiple copies of the master
/// key live in memory at runtime; every copy wipes itself on drop.
#[derive(Clone)]
pub struct VaultService {
    master_key: VaultMasterKey,
}

impl fmt::Debug for VaultService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultService")
            .field("master_key", &self.master_key)
            .finish()
    }
}

impl VaultService {
    /// Construct a vault service bound to the given master key.
    #[must_use]
    pub fn new(master_key: VaultMasterKey) -> Self {
        Self { master_key }
    }

    /// Generate a new keypair of the requested type and return its public
    /// material plus the encrypted private blob.
    ///
    /// `comment` is baked into the OpenSSH key as its trailing comment, so
    /// the `authorized_keys` line a user installs is self-identifying
    /// (`ssh-ed25519 AAAA... <comment>`). Pass an empty string to omit.
    ///
    /// Currently only [`SshKeyType::Ed25519`] is supported; other variants
    /// return [`VaultError::UnsupportedKeyType`] so callers can surface a
    /// clean 400 without having to enumerate vault internals.
    pub fn generate_ssh_identity(
        &self,
        key_type: SshKeyType,
        comment: &str,
    ) -> Result<GeneratedSshIdentity, VaultError> {
        let algorithm = match key_type {
            SshKeyType::Ed25519 => Algorithm::Ed25519,
            other => return Err(VaultError::UnsupportedKeyType(other.as_str())),
        };

        let mut private =
            PrivateKey::random(&mut OsRng, algorithm).map_err(|_| VaultError::KeyGeneration)?;
        if !comment.is_empty() {
            private.set_comment(comment);
        }

        // OpenSSH `authorized_keys` line, ASCII text, no trailing newline.
        let public_key_openssh = private
            .public_key()
            .to_openssh()
            .map_err(|_| VaultError::PublicKeySerialization)?
            .into_bytes();

        // `SHA256:<base64>` fingerprint, exactly what `ssh-keygen -lf` prints.
        let fingerprint_sha256 = private
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();

        // OpenSSH PEM private key — held in a zeroizing buffer so the
        // intermediate plaintext is wiped before this function returns.
        let pem = private
            .to_openssh(LineEnding::LF)
            .map_err(|_| VaultError::PrivateKeySerialization)?;
        let plaintext: Zeroizing<Vec<u8>> = Zeroizing::new(pem.as_bytes().to_vec());

        let encrypted_private_key = cipher::encrypt(&self.master_key, &plaintext)?;

        Ok(GeneratedSshIdentity {
            key_type,
            public_key_openssh,
            fingerprint_sha256,
            encrypted_private_key,
        })
    }

    /// Decrypt a stored ciphertext into the plaintext OpenSSH private-key
    /// PEM. The buffer wipes itself on drop.
    ///
    /// Reachable from tests and the (future) SSH session task; never
    /// surfaced through an HTTP handler.
    pub fn decrypt_private_key(&self, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        cipher::decrypt(&self.master_key, blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use sha2::{Digest, Sha256};

    fn sha256(input: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(input);
        hasher.finalize().into()
    }

    fn vault() -> VaultService {
        VaultService::new(VaultMasterKey::from_bytes([0x91u8; 32]))
    }

    #[test]
    fn generates_ed25519_with_openssh_public_key() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        assert!(
            public.starts_with("ssh-ed25519 "),
            "expected OpenSSH ed25519 public key prefix, got: {public}",
        );
        // No trailing newline — `authorized_keys` lines are appended with
        // their own newline by callers if needed.
        assert!(!public.ends_with('\n'));
    }

    #[test]
    fn fingerprint_uses_sha256_format() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        assert!(
            identity.fingerprint_sha256.starts_with("SHA256:"),
            "expected SHA256: prefix, got: {}",
            identity.fingerprint_sha256
        );
        // Trailing portion is base64 of the 32-byte digest, so 43 chars
        // (no padding) plus the prefix.
        assert!(identity.fingerprint_sha256.len() >= "SHA256:".len() + 32);
    }

    #[test]
    fn fingerprint_matches_independent_sha256() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        // OpenSSH fingerprint is over the wire-format public-key blob, not
        // over the textual form. Round-tripping through `ssh-key` lets us
        // reconstruct that blob without re-implementing the wire codec.
        let txt = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        let pk: ssh_key::PublicKey = txt.parse().unwrap();
        let blob = pk.to_bytes().unwrap();
        let digest = sha256(&blob);
        let expected = format!(
            "SHA256:{}",
            BASE64_STANDARD.encode(digest).trim_end_matches('=')
        );
        assert_eq!(identity.fingerprint_sha256, expected);
    }

    #[test]
    fn round_trip_decrypt_recovers_openssh_pem() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();
        let pem = v
            .decrypt_private_key(identity.encrypted_private_key.as_bytes())
            .unwrap();
        let pem_str = std::str::from_utf8(&pem).unwrap();
        assert!(pem_str.contains("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(pem_str.contains("-----END OPENSSH PRIVATE KEY-----"));
    }

    #[test]
    fn ciphertext_differs_from_pem() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();
        let ciphertext = identity.encrypted_private_key.as_bytes();
        let needle = b"-----BEGIN OPENSSH PRIVATE KEY-----";
        assert!(
            !ciphertext.windows(needle.len()).any(|w| w == needle),
            "ciphertext must not contain the plaintext PEM header",
        );
    }

    #[test]
    fn decrypt_with_wrong_master_key_fails() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();
        let other = VaultService::new(VaultMasterKey::from_bytes([0x00u8; 32]));
        let err = other
            .decrypt_private_key(identity.encrypted_private_key.as_bytes())
            .unwrap_err();
        assert!(matches!(err, VaultError::Decrypt));
    }

    #[test]
    fn unsupported_key_types_are_rejected() {
        let err = vault()
            .generate_ssh_identity(SshKeyType::Rsa, "")
            .unwrap_err();
        assert!(matches!(err, VaultError::UnsupportedKeyType("rsa")));
    }

    #[test]
    fn comment_is_baked_into_public_key() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "homelab-admin")
            .unwrap();
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        assert!(
            public.ends_with(" homelab-admin"),
            "expected comment in public key, got: {public}",
        );
    }

    #[test]
    fn empty_comment_omits_trailing_text() {
        let identity = vault()
            .generate_ssh_identity(SshKeyType::Ed25519, "")
            .unwrap();
        let public = std::str::from_utf8(&identity.public_key_openssh).unwrap();
        let parts: Vec<&str> = public.split_whitespace().collect();
        assert_eq!(
            parts.len(),
            2,
            "no comment should leave exactly algo + base64, got: {public}",
        );
    }

    #[test]
    fn debug_does_not_leak_master_key_or_ciphertext() {
        let v = vault();
        let identity = v.generate_ssh_identity(SshKeyType::Ed25519, "").unwrap();

        let svc = format!("{v:?}");
        assert!(
            svc.contains("redacted"),
            "service debug should redact key: {svc}"
        );

        let id = format!("{identity:?}");
        assert!(
            id.contains("redacted ciphertext"),
            "identity debug should redact ciphertext: {id}"
        );
        // Public key bytes are not redacted (they're public), but plaintext
        // PEM markers must never appear — every reachable byte is ciphertext.
        assert!(!id.contains("BEGIN OPENSSH PRIVATE KEY"));
    }
}
