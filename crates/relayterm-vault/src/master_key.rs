//! Vault master key.
//!
//! 32 raw bytes used directly as the AEAD key. Wrapped in a zeroizing
//! buffer so the value is wiped from memory on drop, with a `Debug` impl
//! that never reveals byte contents and never echoes any user-supplied
//! prefix.
//!
//! Construction is deliberately strict: the only safe inputs are
//! [`VaultMasterKey::from_bytes`] (typed) or [`VaultMasterKey::from_base64`]
//! (operator config). There is no `Default` and no fallback-generation path
//! at runtime — booting without a key configured is a deployment error,
//! not a soft-failure case.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Length in bytes of the master key. Matches the AEAD key size.
pub(crate) const MASTER_KEY_LEN: usize = 32;

/// Errors specific to master-key construction.
///
/// Variants describe *why* a value is unusable without echoing the value
/// itself — the offending bytes never enter the error path.
#[derive(Debug, thiserror::Error)]
pub enum MasterKeyError {
    /// Empty configuration — no key provided at all.
    #[error("master key is empty")]
    Empty,

    /// Provided key was the wrong number of decoded bytes.
    /// Renders length only; never the bytes.
    #[error("master key has wrong length (expected {expected} bytes, got {actual})")]
    WrongLength { expected: usize, actual: usize },

    /// Base64 decoding failed.
    /// The wrapped message is the decoder's structural complaint (e.g.
    /// "invalid byte at offset N"); it does not include the input.
    #[error("master key base64 decode failed: {0}")]
    Base64(String),

    /// Reading the configured key file failed.
    #[error("master key file read failed: {0}")]
    File(String),
}

/// 32-byte key used as the AEAD key for vault ciphertext.
///
/// `Zeroize`/`ZeroizeOnDrop` ensure the bytes are wiped on drop. The
/// manual `Debug` impl renders only the length and a redaction marker.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VaultMasterKey {
    bytes: [u8; MASTER_KEY_LEN],
}

impl VaultMasterKey {
    /// Build a master key from raw bytes. Used by typed callers (tests,
    /// generated keys, KMS integrations).
    #[must_use]
    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self { bytes }
    }

    /// Decode a base64-encoded master key. Whitespace around the value is
    /// trimmed before decoding so operators can paste from a config file
    /// or env var without thinking about trailing newlines.
    pub fn from_base64(input: &str) -> Result<Self, MasterKeyError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(MasterKeyError::Empty);
        }
        let raw = BASE64_STANDARD
            .decode(trimmed)
            .map_err(|e| MasterKeyError::Base64(e.to_string()))?;
        if raw.len() != MASTER_KEY_LEN {
            // Manually zero the partial buffer before dropping so a
            // wrong-length input still does not linger.
            let mut raw = raw;
            let actual = raw.len();
            // `Vec::zeroize` zeroes the bytes in place; it does not change
            // `len()`, so reading the length captured above stays correct.
            raw.zeroize();
            return Err(MasterKeyError::WrongLength {
                expected: MASTER_KEY_LEN,
                actual,
            });
        }
        let mut bytes = [0u8; MASTER_KEY_LEN];
        bytes.copy_from_slice(&raw);
        let mut raw = raw;
        raw.zeroize();
        Ok(Self { bytes })
    }

    /// Read a master key from a file containing a single base64 value.
    /// Errors are mapped without the path or contents being echoed.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, MasterKeyError> {
        let raw = std::fs::read_to_string(path).map_err(|e| MasterKeyError::File(e.to_string()))?;
        Self::from_base64(&raw)
    }

    /// Borrow the raw bytes for AEAD construction. Internal only.
    #[must_use]
    pub(crate) fn bytes(&self) -> &[u8; MASTER_KEY_LEN] {
        &self.bytes
    }
}

impl fmt::Debug for VaultMasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultMasterKey")
            .field("bytes", &format_args!("<redacted: {MASTER_KEY_LEN} bytes>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_b64() -> String {
        // 32 bytes of 0x42; never deployed.
        BASE64_STANDARD.encode([0x42u8; MASTER_KEY_LEN])
    }

    #[test]
    fn from_base64_round_trips_valid_key() {
        let key = VaultMasterKey::from_base64(&fixture_b64()).unwrap();
        assert_eq!(key.bytes()[0], 0x42);
        assert_eq!(key.bytes().len(), MASTER_KEY_LEN);
    }

    #[test]
    fn from_base64_trims_whitespace() {
        let mut padded = String::from("  ");
        padded.push_str(&fixture_b64());
        padded.push('\n');
        VaultMasterKey::from_base64(&padded).unwrap();
    }

    #[test]
    fn from_base64_rejects_empty() {
        assert!(matches!(
            VaultMasterKey::from_base64("").unwrap_err(),
            MasterKeyError::Empty
        ));
        assert!(matches!(
            VaultMasterKey::from_base64("   \n").unwrap_err(),
            MasterKeyError::Empty
        ));
    }

    #[test]
    fn from_base64_rejects_wrong_length() {
        // 16 bytes encoded.
        let short = BASE64_STANDARD.encode([0u8; 16]);
        assert!(matches!(
            VaultMasterKey::from_base64(&short).unwrap_err(),
            MasterKeyError::WrongLength { .. }
        ));
    }

    #[test]
    fn from_base64_rejects_non_base64() {
        assert!(matches!(
            VaultMasterKey::from_base64("@@@@@@@@@@").unwrap_err(),
            MasterKeyError::Base64(_)
        ));
    }

    #[test]
    fn debug_does_not_leak_bytes() {
        let key = VaultMasterKey::from_bytes([0xABu8; MASTER_KEY_LEN]);
        let s = format!("{key:?}");
        assert!(!s.contains("AB"), "debug should not contain hex bytes: {s}");
        assert!(
            !s.contains("0xab"),
            "debug should not contain hex bytes: {s}"
        );
        assert!(
            s.contains("redacted"),
            "debug should announce redaction: {s}"
        );
    }

    #[test]
    fn error_does_not_echo_input() {
        // Provide a wrong-length, base64-decodable input. The error string
        // must mention length only, not the input bytes.
        let short_b64 = BASE64_STANDARD.encode(b"too short");
        let err = VaultMasterKey::from_base64(&short_b64).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("too short"));
        assert!(!msg.contains(&short_b64));
    }
}
