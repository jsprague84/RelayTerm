//! Server-issued opaque sessions and the audit-log facade.
//!
//! No implementation yet. Per AGENTS.md: sessions are server-side and
//! cookie-bound; JWTs are not used for the browser surface.

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("auth is not yet implemented")]
    NotImplemented,
}
