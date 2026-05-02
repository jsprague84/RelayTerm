//! Server-issued opaque sessions, password hashing, and the
//! [`AuthService`] that composes them on top of the core repository
//! traits.
//!
//! No HTTP, cookie, CSRF, or extractor surface lives here yet — those
//! land in a later slice. This crate stops at the service boundary so
//! the route layer can call into one place without learning about
//! Argon2id parameters, salt formats, or token-hashing rules.
//!
//! ## Surface
//!
//! * [`PasswordHasher`] — Argon2id PHC string producer / verifier.
//! * [`SessionToken`] / [`SessionTokenHash`] — high-entropy opaque
//!   token + its SHA-256 digest. Plaintext crosses the service
//!   boundary exactly once (as the return of [`AuthService::create_session`]).
//! * [`AuthService`] — composes [`PasswordCredentialRepository`] and
//!   [`UserSessionRepository`] for set-password / verify-password /
//!   create-session / validate-session-token / revoke-session. Time
//!   is passed in as `DateTime<Utc>` so tests do not need a clock
//!   trait.
//!
//! [`PasswordCredentialRepository`]: relayterm_core::repository::PasswordCredentialRepository
//! [`UserSessionRepository`]: relayterm_core::repository::UserSessionRepository
//!
//! ## Redaction posture (load-bearing — sentinel-tested)
//!
//! Every wrapper that carries password material, session-token bytes,
//! or a token digest implements `Debug` manually so a stray
//! `tracing::debug!(?wrapper)` cannot leak the bytes. `Display` either
//! is not implemented or renders the same redacted shape. `serde` is
//! deliberately NOT derived on any plaintext-bearing type — there is
//! no "send the token over the wire" surface at this layer beyond the
//! one-shot return from [`AuthService::create_session`].
//!
//! Errors render only structural categories (`InvalidCredentials`,
//! `SessionInvalid`, `SessionExpired`, `SessionRevoked`,
//! `Repository`, `Crypto`) — never the offered password, the stored
//! hash, the offered token, or the stored digest.

pub mod password;
pub mod service;
pub mod session_token;

pub use password::{PasswordHasher, PasswordHasherConfig, PasswordHashingError};
pub use service::{AuthService, AuthServiceError};
pub use session_token::{SessionToken, SessionTokenHash, hash_session_token};

/// Legacy placeholder kept so external consumers that only matched on
/// `AuthError::NotImplemented` continue to compile. New code should
/// match on [`AuthServiceError`] instead.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("auth is not yet implemented")]
    NotImplemented,
}
