//! AEAD encryption envelope for vault-stored secrets.
//!
//! The on-disk blob layout is fully described at the crate root. This
//! module owns the byte-level encode / decode and the AEAD calls. The
//! envelope is intentionally self-describing (magic + version) so future
//! schemes (HKDF subkeys, KMS-wrapped keys, OpenBao references) can be
//! introduced without breaking blobs already at rest.

use std::fmt;

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit},
};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::VaultError;
use crate::master_key::VaultMasterKey;

/// 4-byte magic prefix on every envelope. "RelayTerm Vault, format 1".
const MAGIC: &[u8; 4] = b"RTV1";

/// Version byte describing how the rest of the envelope is interpreted.
/// `0x01` = XChaCha20-Poly1305 with the master key used directly (no KDF).
const VERSION_XCHACHA20POLY1305_DIRECT: u8 = 0x01;

/// XChaCha20 nonce length, in bytes.
const NONCE_LEN: usize = 24;

/// Header overhead before the ciphertext.
const HEADER_LEN: usize = MAGIC.len() + 1 /* version */ + NONCE_LEN;

/// AEAD tag length appended by `chacha20poly1305`.
const TAG_LEN: usize = 16;

/// Opaque ciphertext blob with redacted `Debug`.
///
/// Storage layer treats this as `BYTEA`. Callers obtain the inner `Vec<u8>`
/// via [`Self::into_bytes`] only when persisting — the redacted `Debug`
/// impl prevents accidental log lines from echoing ciphertext.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedBlob(Vec<u8>);

impl EncryptedBlob {
    /// Wrap pre-encoded bytes (e.g. read from the database) without copying
    /// out of the layer they came from.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the encoded bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Consume into the encoded byte vector for persistence.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Length of the encoded envelope in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when the blob is empty (only meaningful pre-encryption).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for EncryptedBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptedBlob")
            .field("len", &self.0.len())
            .field("bytes", &format_args!("<redacted ciphertext>"))
            .finish()
    }
}

/// Encrypt `plaintext` with `master_key` and return the encoded envelope.
///
/// Uses a fresh random 24-byte nonce per call (XChaCha20). Errors map to
/// [`VaultError::Encrypt`] without echoing inputs.
pub(crate) fn encrypt(
    master_key: &VaultMasterKey,
    plaintext: &[u8],
) -> Result<EncryptedBlob, VaultError> {
    let cipher = XChaCha20Poly1305::new(master_key.bytes().into());

    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| VaultError::Encrypt)?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION_XCHACHA20POLY1305_DIRECT);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(EncryptedBlob(out))
}

/// Decrypt a vault envelope, returning a zeroizing buffer holding the
/// plaintext bytes. The buffer is wiped when dropped.
pub(crate) fn decrypt(
    master_key: &VaultMasterKey,
    blob: &[u8],
) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    if blob.len() < HEADER_LEN + TAG_LEN {
        return Err(VaultError::BlobFormat);
    }
    if &blob[..MAGIC.len()] != MAGIC {
        return Err(VaultError::BlobFormat);
    }
    let version = blob[MAGIC.len()];
    if version != VERSION_XCHACHA20POLY1305_DIRECT {
        return Err(VaultError::BlobFormat);
    }
    let nonce = XNonce::from_slice(&blob[MAGIC.len() + 1..HEADER_LEN]);
    let ciphertext = &blob[HEADER_LEN..];

    let cipher = XChaCha20Poly1305::new(master_key.bytes().into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| VaultError::Decrypt)?;
    Ok(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_key(seed: u8) -> VaultMasterKey {
        VaultMasterKey::from_bytes([seed; 32])
    }

    #[test]
    fn round_trip_matches_plaintext() {
        let key = fixture_key(0x11);
        let plaintext = b"super secret private key bytes";
        let blob = encrypt(&key, plaintext).unwrap();
        let recovered = decrypt(&key, blob.as_bytes()).unwrap();
        assert_eq!(&recovered[..], plaintext);
    }

    #[test]
    fn ciphertext_differs_from_plaintext() {
        let key = fixture_key(0x22);
        let plaintext = b"-----BEGIN OPENSSH PRIVATE KEY-----";
        let blob = encrypt(&key, plaintext).unwrap();
        assert!(blob.len() > plaintext.len());
        assert!(
            !blob
                .as_bytes()
                .windows(plaintext.len())
                .any(|w| w == plaintext)
        );
    }

    #[test]
    fn wrong_master_key_fails_decrypt() {
        let alice = fixture_key(0xAA);
        let mallory = fixture_key(0xBB);
        let blob = encrypt(&alice, b"secret").unwrap();
        let err = decrypt(&mallory, blob.as_bytes()).unwrap_err();
        assert!(matches!(err, VaultError::Decrypt));
    }

    #[test]
    fn rejects_truncated_blob() {
        let key = fixture_key(0x33);
        let blob = encrypt(&key, b"secret").unwrap();
        let bytes = blob.into_bytes();
        let truncated = &bytes[..HEADER_LEN]; // header only, no tag.
        let err = decrypt(&key, truncated).unwrap_err();
        assert!(matches!(err, VaultError::BlobFormat));
    }

    #[test]
    fn rejects_bad_magic() {
        let key = fixture_key(0x44);
        let blob = encrypt(&key, b"secret").unwrap();
        let mut bytes = blob.into_bytes();
        bytes[0] ^= 0xFF;
        let err = decrypt(&key, &bytes).unwrap_err();
        assert!(matches!(err, VaultError::BlobFormat));
    }

    #[test]
    fn rejects_unknown_version() {
        let key = fixture_key(0x55);
        let blob = encrypt(&key, b"secret").unwrap();
        let mut bytes = blob.into_bytes();
        bytes[MAGIC.len()] = 0xFE;
        let err = decrypt(&key, &bytes).unwrap_err();
        assert!(matches!(err, VaultError::BlobFormat));
    }

    #[test]
    fn rejects_tampered_ciphertext() {
        let key = fixture_key(0x66);
        let blob = encrypt(&key, b"secret content").unwrap();
        let mut bytes = blob.into_bytes();
        // Flip a byte inside the ciphertext.
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let err = decrypt(&key, &bytes).unwrap_err();
        assert!(matches!(err, VaultError::Decrypt));
    }

    #[test]
    fn nonce_is_random_per_call() {
        let key = fixture_key(0x77);
        let a = encrypt(&key, b"same plaintext").unwrap().into_bytes();
        let b = encrypt(&key, b"same plaintext").unwrap().into_bytes();
        // Header differs (random nonce), so envelopes differ.
        assert_ne!(a, b);
    }

    #[test]
    fn debug_does_not_echo_ciphertext_bytes() {
        let key = fixture_key(0x88);
        let blob = encrypt(&key, b"plaintext").unwrap();
        let s = format!("{blob:?}");
        assert!(
            s.contains("redacted"),
            "debug should announce redaction: {s}"
        );
        // Hex/byte-array forms must not appear.
        assert!(!s.contains(", 0x"));
    }
}
