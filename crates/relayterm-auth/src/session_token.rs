//! Browser session token: high-entropy random bytes + SHA-256 digest.
//!
//! The cookie value is the URL-safe base64 of 32 random bytes from
//! `OsRng`. The database stores ONLY the SHA-256 of those bytes
//! (`token_hash`), so a database dump is not enough to authenticate
//! as any user; you also need the original cookie. Lookup is by
//! `token_hash` exactly — no prefix matching, no second-source-of-truth.
//!
//! ## Surface (what crosses the service boundary)
//!
//! * [`SessionToken`] — plaintext token returned EXACTLY ONCE from
//!   `AuthService::create_session`. The HTTP layer immediately puts
//!   the bytes in the `Set-Cookie` header and drops the wrapper.
//! * [`SessionTokenHash`] — SHA-256 digest. Persisted, looked up,
//!   and otherwise treated as sensitive (a leaked digest plus a
//!   captured plaintext cookie still equals session takeover).
//!
//! Plaintext tokens never reach the database, never appear in audit
//! payloads, never reach `Display` / `serde` / `Debug` output.

use std::fmt;

use base64::Engine;
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Number of random bytes per session token. 32 bytes = 256 bits of
/// entropy — well above the threshold for resistance to online
/// guessing.
pub const SESSION_TOKEN_BYTES: usize = 32;

/// Length in bytes of a SHA-256 digest. Repeated here as a constant
/// so dependent code (route layer, repository, audit redaction tests)
/// can pin it without re-importing `sha2`.
pub const SESSION_TOKEN_HASH_BYTES: usize = 32;

/// A freshly-minted session token.
///
/// The wrapper holds the URL-safe base64 encoding of the random
/// bytes — that is the exact value the HTTP layer puts in the
/// `Set-Cookie` header. The raw bytes are not retained: once the
/// token has been encoded for the wire AND hashed for storage there
/// is no production path that needs them again.
///
/// `Debug` redacts. There is no `Display`. There is no `serde`. The
/// wrapper exposes the encoded value through [`Self::expose`] for the
/// single legitimate caller (the HTTP layer setting the cookie).
/// The underlying string is zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SessionToken {
    encoded: String,
}

impl SessionToken {
    /// Generate a new random token.
    ///
    /// Reads `SESSION_TOKEN_BYTES` bytes from `OsRng` and encodes
    /// them as URL-safe base64 *without* trailing `=` padding. The
    /// resulting cookie string is 43 ASCII characters — short enough
    /// for `Set-Cookie` headers, long enough to encode 256 bits of
    /// entropy.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; SESSION_TOKEN_BYTES];
        OsRng.fill_bytes(&mut bytes);
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        // Wipe the local stack copy of the bytes — `bytes.zeroize()`
        // is best-effort defense in depth; once `encoded` is built
        // the raw entropy is no longer needed.
        bytes.zeroize();
        Self { encoded }
    }

    /// Borrow the encoded token. Use ONLY at the HTTP-boundary site
    /// that sets `Set-Cookie`. Do not log, do not pass through
    /// `tracing::field::Empty::record`, do not include in any
    /// formatted string that reaches the wire body or audit payload.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.encoded
    }

    /// Compute the SHA-256 digest. The digest is what gets stored
    /// in `user_sessions.token_hash`; the plaintext is dropped after
    /// the cookie is set.
    #[must_use]
    pub fn hash(&self) -> SessionTokenHash {
        hash_session_token(&self.encoded)
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionToken")
            .field(
                "encoded",
                &format_args!("<redacted: {} chars>", self.encoded.len()),
            )
            .finish()
    }
}

/// SHA-256 digest of a session token.
///
/// Stored verbatim as `user_sessions.token_hash` and used as the only
/// lookup key in the auth extractor. `Debug` redacts; there is no
/// `Display`; there is no `serde`. Conversion into `Vec<u8>` is
/// deliberate and one-shot via [`Self::into_bytes`] so the repository
/// can take ownership when constructing `CreateUserSession`.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionTokenHash {
    bytes: [u8; SESSION_TOKEN_HASH_BYTES],
}

impl SessionTokenHash {
    /// Borrow the digest as a byte slice for a repository lookup
    /// call (`get_by_token_hash(&[u8])`).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Move the digest into an owned `Vec<u8>` for a repository
    /// insert call (`CreateUserSession::token_hash`).
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.to_vec()
    }

    /// Build a digest from a previously-computed 32-byte array.
    ///
    /// Test-only: production code paths must go through
    /// [`hash_session_token`] / [`SessionToken::hash`] so a digest
    /// always traces back to a known source. The constructor exists
    /// only so the redaction tests in this module can assemble a
    /// fixture digest without first generating a token. A future
    /// admin tool that takes a hex digest on the command line would
    /// graduate this to a public, parsing-aware constructor.
    #[cfg(test)]
    #[must_use]
    const fn from_bytes(bytes: [u8; SESSION_TOKEN_HASH_BYTES]) -> Self {
        Self { bytes }
    }
}

impl fmt::Debug for SessionTokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionTokenHash")
            .field(
                "bytes",
                &format_args!("<redacted: {} bytes>", self.bytes.len()),
            )
            .finish()
    }
}

/// Hash a session-token string into a [`SessionTokenHash`].
///
/// Free function form so the auth extractor can call it on the
/// cookie value without instantiating a service. Deterministic — the
/// same input always produces the same digest, which is what makes
/// the index lookup work.
#[must_use]
pub fn hash_session_token(token: &str) -> SessionTokenHash {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; SESSION_TOKEN_HASH_BYTES];
    bytes.copy_from_slice(&digest);
    SessionTokenHash { bytes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_tokens_are_unique() {
        // 1024 tokens × 256 bits of entropy means a collision is
        // astronomically unlikely; this test exists to catch a
        // regression where someone seeds a non-cryptographic RNG.
        let mut seen: HashSet<String> = HashSet::new();
        for _ in 0..1024 {
            let token = SessionToken::generate();
            assert!(
                seen.insert(token.expose().to_owned()),
                "duplicate token across 1024 generations"
            );
        }
    }

    #[test]
    fn generated_tokens_have_expected_length() {
        let token = SessionToken::generate();
        // 32 bytes URL-safe base64 (no padding) = 43 chars.
        assert_eq!(token.expose().len(), 43, "URL-safe no-pad length");
        // ASCII only — no whitespace, no `=`, no `+`, no `/`.
        assert!(
            token
                .expose()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "token must be URL-safe base64 alphabet"
        );
    }

    #[test]
    fn hash_is_deterministic_for_same_input() {
        let token = SessionToken::generate();
        let h1 = token.hash();
        let h2 = token.hash();
        assert_eq!(h1, h2, "same token must hash to the same digest");
        // Free-function form must agree with the method.
        let h3 = hash_session_token(token.expose());
        assert_eq!(h1, h3, "hash_session_token must match SessionToken::hash");
    }

    #[test]
    fn hash_differs_for_different_inputs() {
        let a = SessionToken::generate();
        let b = SessionToken::generate();
        assert_ne!(a.expose(), b.expose());
        assert_ne!(a.hash(), b.hash(), "different tokens must hash differently");
    }

    #[test]
    fn hash_has_expected_length() {
        let h = hash_session_token("anything");
        assert_eq!(h.as_bytes().len(), SESSION_TOKEN_HASH_BYTES);
        assert_eq!(h.into_bytes().len(), SESSION_TOKEN_HASH_BYTES);
    }

    #[test]
    fn debug_redacts_token() {
        let token = SessionToken::generate();
        let exposed = token.expose().to_owned();
        let dbg = format!("{token:?}");
        assert!(
            !dbg.contains(&exposed),
            "SessionToken Debug must not echo the encoded token"
        );
        assert!(
            dbg.contains("redacted"),
            "SessionToken Debug must label the redaction"
        );
    }

    #[test]
    fn debug_redacts_token_hash() {
        // Sentinel bytes that would be visible if any formatter
        // printed the array element-wise.
        let h = SessionTokenHash::from_bytes([
            0x53, 0x55, 0x50, 0x45, 0x52, 0x53, 0x45, 0x43, 0x52, 0x45, 0x54, 0x44, 0x49, 0x47,
            0x45, 0x53, 0x54, 0x53, 0x55, 0x50, 0x45, 0x52, 0x53, 0x45, 0x43, 0x52, 0x45, 0x54,
            0x21, 0x21, 0x21, 0x21,
        ]);
        let dbg = format!("{h:?}");
        // First-byte decimal "83" repeated would appear in any naive
        // Vec/array Debug. ASCII reinterpretation would say
        // "SUPERSECRET".
        assert!(
            !dbg.contains("83, 85, 80"),
            "SessionTokenHash Debug must not echo bytes"
        );
        assert!(
            !dbg.contains("SUPERSECRET"),
            "SessionTokenHash Debug must not echo bytes interpreted as ASCII"
        );
        assert!(
            dbg.contains("redacted"),
            "SessionTokenHash Debug must label the redaction"
        );
    }

    #[test]
    fn into_bytes_round_trips() {
        let token = SessionToken::generate();
        let h = token.hash();
        let from_method = h.as_bytes().to_vec();
        let from_owned = token.hash().into_bytes();
        assert_eq!(from_method, from_owned, "as_bytes and into_bytes agree");
    }
}
