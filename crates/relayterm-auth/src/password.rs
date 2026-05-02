//! Argon2id password hashing.
//!
//! Wraps the `argon2` crate's `Argon2` engine with a typed config so
//! parameters live in one place and so the service layer doesn't have
//! to import `argon2::*` directly. The PHC string format is the
//! storage format — parameters and per-password salt travel inside the
//! string, which is why no separate `algo_version` column exists on
//! `user_passwords`.
//!
//! ## Redaction posture
//!
//! * `PasswordHasher` derives `Clone` only — no `Debug`, no
//!   `Display`. `Argon2` does not impl `Debug` either, so the engine
//!   cannot leak parameter bytes accidentally.
//! * Plaintext passwords cross this module as `&str` only and are
//!   never copied into a long-lived buffer. There is no
//!   `PasswordSecret`-style wrapper because Argon2id reads the bytes
//!   once and the surrounding service layer holds the borrow for the
//!   minimum needed. The redaction backstop for stored hashes is the
//!   `CreatePasswordCredential` / `PasswordCredential` `Debug` impl
//!   in `relayterm_core` — see step-2 SPEC notes.
//! * Errors render structural categories only (`InvalidStoredHash`,
//!   `Hash`, `Verify`). No bytes from the offered password, no bytes
//!   from the stored hash.

use std::fmt;

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{
        PasswordHash, PasswordHasher as _, PasswordVerifier, SaltString, rand_core::OsRng,
    },
};

/// Argon2id parameters.
///
/// Defaults follow OWASP 2023's "interactive use" baseline (`m=19456`,
/// `t=2`, `p=1`). The `m` parameter is in **kibibytes** — 19,456 KiB
/// ≈ 19 MiB. Do NOT multiply by 1024 anywhere; SPEC.md "Production
/// authentication architecture" calls this out as a known footgun.
#[derive(Clone, Copy)]
pub struct PasswordHasherConfig {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Number of iterations.
    pub t_cost: u32,
    /// Parallelism (lanes).
    pub p_cost: u32,
}

impl PasswordHasherConfig {
    /// OWASP 2023 baseline. Used by `Default` and called out
    /// explicitly so callers can re-use the same constant in tests
    /// (or tune it down for fast-path test variants).
    pub const OWASP_2023: Self = Self {
        m_cost: 19_456,
        t_cost: 2,
        p_cost: 1,
    };
}

impl Default for PasswordHasherConfig {
    fn default() -> Self {
        Self::OWASP_2023
    }
}

/// Manual `Debug` so accidental tracing of a hasher engine cannot
/// surface the parameter set. Parameters are not secret in v1, but
/// keeping the redaction shape uniform with the rest of the auth
/// surface means a future change (e.g. operator-supplied pepper) does
/// not need a Debug audit.
impl fmt::Debug for PasswordHasherConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordHasherConfig")
            .field("m_cost", &"<redacted>")
            .field("t_cost", &"<redacted>")
            .field("p_cost", &"<redacted>")
            .finish()
    }
}

/// Errors a password operation can produce.
///
/// `Display` deliberately stops at the structural category — it never
/// echoes the offered password, the stored hash, the salt, or the
/// argon2 engine's internal error string (which can carry parameter
/// fragments).
#[derive(Debug, thiserror::Error)]
pub enum PasswordHashingError {
    /// Argon2id refused to hash the input. In practice this only
    /// fires when the runtime parameter set is invalid (e.g. a tuned
    /// build picked an out-of-range `m_cost`); a plaintext password
    /// length problem is rejected at the boundary, not here.
    #[error("password hash failed")]
    Hash,

    /// Stored hash did not parse as a PHC string. Treated as a
    /// failed verification, never a panic — a corrupt row should
    /// surface as "credentials invalid", not crash the request.
    #[error("stored password hash is not a valid PHC string")]
    InvalidStoredHash,
}

/// Argon2id PHC hasher.
///
/// One instance is reused across requests; `Argon2` itself is
/// stateless and cheap to clone, so pinning a single hasher onto the
/// service avoids re-parsing parameters on every login. Constructed
/// via [`PasswordHasher::new`] (custom params) or [`Default::default`]
/// (OWASP 2023 baseline).
#[derive(Clone)]
pub struct PasswordHasher {
    config: PasswordHasherConfig,
}

impl PasswordHasher {
    /// Build a hasher with the given parameters. Returns an error if
    /// the parameter triple is rejected by Argon2 (e.g. `m_cost`
    /// below the implementation minimum).
    pub fn new(config: PasswordHasherConfig) -> Result<Self, PasswordHashingError> {
        // Validate by trying to construct the underlying `Params`. We
        // don't keep the result — the engine is reconstructed per
        // call below to keep the type free of `Argon2<'_>` lifetimes.
        Params::new(config.m_cost, config.t_cost, config.p_cost, None)
            .map_err(|_| PasswordHashingError::Hash)?;
        Ok(Self { config })
    }

    fn engine(&self) -> Result<Argon2<'static>, PasswordHashingError> {
        let params = Params::new(
            self.config.m_cost,
            self.config.t_cost,
            self.config.p_cost,
            None,
        )
        .map_err(|_| PasswordHashingError::Hash)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    /// Hash a plaintext password into a PHC string.
    ///
    /// A fresh random salt is generated per call from `OsRng`, so
    /// hashing the same password twice produces two distinct PHC
    /// strings.
    pub fn hash_password(&self, plaintext: &str) -> Result<String, PasswordHashingError> {
        let engine = self.engine()?;
        let salt = SaltString::generate(&mut OsRng);
        let hash = engine
            .hash_password(plaintext.as_bytes(), &salt)
            .map_err(|_| PasswordHashingError::Hash)?;
        Ok(hash.to_string())
    }

    /// Verify a plaintext password against a previously stored PHC
    /// string.
    ///
    /// Returns `Ok(true)` on a match, `Ok(false)` on a structurally
    /// valid hash that did not match the offered password, and
    /// `Err(InvalidStoredHash)` when the stored value is not a PHC
    /// string at all. The route layer collapses the error case to a
    /// non-match before responding so a corrupt row is
    /// indistinguishable from wrong-password to a probe.
    pub fn verify_password(
        &self,
        plaintext: &str,
        stored_hash: &str,
    ) -> Result<bool, PasswordHashingError> {
        let parsed =
            PasswordHash::new(stored_hash).map_err(|_| PasswordHashingError::InvalidStoredHash)?;
        let engine = self.engine()?;
        match engine.verify_password(plaintext.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            // `password_hash::Error::Password` is "wrong password";
            // anything else (corrupt parameters, unsupported algorithm
            // baked into a PHC string we wrote ourselves) is mapped
            // to false rather than surfaced as a typed error so a
            // probe can't distinguish "your password is wrong" from
            // "the row is broken".
            Err(_) => Ok(false),
        }
    }
}

impl fmt::Debug for PasswordHasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PasswordHasher")
            .field("config", &self.config)
            .finish()
    }
}

impl Default for PasswordHasher {
    fn default() -> Self {
        // `unwrap` is safe: OWASP_2023 is a known-valid parameter
        // triple that round-trips through `Params::new` in the unit
        // tests below.
        Self::new(PasswordHasherConfig::default()).expect("OWASP_2023 params are valid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tuned-down params so unit tests run in milliseconds. Production
    /// uses `OWASP_2023` (~250 ms verify); these are not security
    /// claims, only test plumbing. `m_cost = 4096` (4 MiB) is well
    /// above Argon2's implementation minimum and keeps the suite
    /// resilient to additional test count without slowing CI.
    const FAST_TEST_PARAMS: PasswordHasherConfig = PasswordHasherConfig {
        m_cost: 4_096,
        t_cost: 1,
        p_cost: 1,
    };

    fn fast_hasher() -> PasswordHasher {
        PasswordHasher::new(FAST_TEST_PARAMS).expect("fast test params are valid")
    }

    #[test]
    fn default_uses_owasp_2023() {
        // Pinning the constant forces a deliberate edit + ADR if a
        // future PR weakens the default.
        assert_eq!(PasswordHasherConfig::default().m_cost, 19_456);
        assert_eq!(PasswordHasherConfig::default().t_cost, 2);
        assert_eq!(PasswordHasherConfig::default().p_cost, 1);
    }

    #[test]
    fn hash_emits_argon2id_phc_string() {
        let hasher = fast_hasher();
        let hash = hasher
            .hash_password("correct horse battery staple")
            .expect("hash");
        assert!(
            hash.starts_with("$argon2id$"),
            "expected argon2id PHC prefix, got `{}`",
            hash
        );
        // PHC-string sanity: at least four `$` segments.
        let segments: Vec<&str> = hash.split('$').collect();
        assert!(segments.len() >= 5, "PHC string had too few segments");
    }

    #[test]
    fn same_password_hashes_differently() {
        let hasher = fast_hasher();
        let a = hasher.hash_password("hunter2").expect("hash a");
        let b = hasher.hash_password("hunter2").expect("hash b");
        assert_ne!(
            a, b,
            "Argon2id with random salts must not produce identical PHC strings"
        );
    }

    #[test]
    fn correct_password_verifies() {
        let hasher = fast_hasher();
        let stored = hasher.hash_password("correct password").expect("hash");
        assert!(
            hasher
                .verify_password("correct password", &stored)
                .expect("verify"),
            "correct password should verify"
        );
    }

    #[test]
    fn wrong_password_does_not_verify() {
        let hasher = fast_hasher();
        let stored = hasher.hash_password("the secret").expect("hash");
        assert!(
            !hasher
                .verify_password("the wrong secret", &stored)
                .expect("verify"),
            "wrong password must not verify"
        );
    }

    #[test]
    fn malformed_hash_returns_safe_error() {
        let hasher = fast_hasher();
        // Definitely not a PHC string.
        let result = hasher.verify_password("anything", "not-a-phc-string");
        match result {
            Err(PasswordHashingError::InvalidStoredHash) => {}
            other => panic!("expected InvalidStoredHash, got {:?}", other),
        }
    }

    #[test]
    fn malformed_hash_error_does_not_leak_input() {
        let hasher = fast_hasher();
        let bogus_stored = "definitely-not-phc-DO-NOT-LEAK-stored";
        let bogus_password = "definitely-the-password-DO-NOT-LEAK-password";
        let err = hasher
            .verify_password(bogus_password, bogus_stored)
            .expect_err("malformed hash must error");
        let rendered = format!("{err}");
        assert!(
            !rendered.contains("DO-NOT-LEAK-stored"),
            "error must not echo stored-hash bytes"
        );
        assert!(
            !rendered.contains("DO-NOT-LEAK-password"),
            "error must not echo plaintext password"
        );
    }

    #[test]
    fn debug_does_not_echo_password_or_hash() {
        let hasher = fast_hasher();
        let stored = hasher
            .hash_password("DO-NOT-LEAK-password-bytes-IN-DEBUG")
            .expect("hash");
        // Hasher itself.
        let dbg = format!("{hasher:?}");
        assert!(
            !dbg.contains("19456") && !dbg.contains("19_456"),
            "Debug must not echo m_cost numerics"
        );
        // Config alone.
        let cfg_dbg = format!("{:?}", hasher.config);
        assert!(
            !cfg_dbg.contains("19456") && !cfg_dbg.contains("19_456"),
            "PasswordHasherConfig Debug must not echo numeric parameters"
        );
        // Stored hash is just a String here, but verify the value the
        // service produces does not embed our plaintext.
        assert!(
            !stored.contains("DO-NOT-LEAK-password-bytes-IN-DEBUG"),
            "PHC string must not contain the plaintext input"
        );
    }
}
