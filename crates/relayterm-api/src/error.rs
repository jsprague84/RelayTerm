//! HTTP-level error type and response mapping.
//!
//! Handlers return `Result<T, ApiError>`. The mapping below decides the
//! status code and the JSON body shape. Internal details — SQL fragments,
//! `RepositoryError::Database` payloads, secret-bearing values — never leak:
//! every response goes through a small set of canonical shapes and any
//! "unexpected" outcome collapses to a generic `internal_error`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use relayterm_core::repository::RepositoryError;
use relayterm_core::validation::ValidationError;
use relayterm_vault::VaultError;
use serde::Serialize;
use tracing::{error, warn};

/// Stable error code strings emitted on the wire. The enum keeps the set
/// closed so handlers can't invent ad-hoc codes that clients then depend on.
#[derive(Debug, Clone, Copy)]
#[allow(unreachable_pub)]
pub enum ErrorCode {
    InvalidInput,
    Unauthorized,
    NotFound,
    Conflict,
    ServiceUnavailable,
    InternalError,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::ServiceUnavailable => "service_unavailable",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[allow(unreachable_pub)]
pub enum ApiError {
    /// 400 — input failed validation at the API boundary.
    #[error("invalid input: {0}")]
    Validation(String),

    /// 401 — no authenticated identity available.
    ///
    /// **The wrapped detail is operator-facing only and is NEVER echoed to
    /// the client.** It is logged at `warn!` when the response is built;
    /// the wire body collapses to the static `"unauthorized"` message.
    /// Do not put credential hints, token fragments, or transient session
    /// state in this string — even though it is currently redacted at the
    /// boundary, treat it as a server-side log line.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// 404 — the addressed entity does not exist (or is not visible to the caller).
    #[error("{entity} not found")]
    NotFound { entity: &'static str },

    /// 409 — uniqueness or referential constraint violated.
    #[error("{entity} conflict")]
    Conflict { entity: &'static str },

    /// 503 — a backend dependency required for the request is intentionally
    /// not configured (e.g. vault disabled). The wrapped detail is logged
    /// at warn but the wire body is the static `service unavailable`
    /// message.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// 500 — anything unexpected. The wrapped string is logged but never echoed.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, ErrorCode, String) {
        match self {
            Self::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidInput,
                msg.clone(),
            ),
            Self::Unauthorized(_) => (
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "unauthorized".to_owned(),
            ),
            Self::NotFound { entity } => (
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                format!("{entity} not found"),
            ),
            Self::Conflict { entity } => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                format!("{entity} conflict"),
            ),
            Self::ServiceUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::ServiceUnavailable,
                "service unavailable".to_owned(),
            ),
            Self::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::InternalError,
                "internal error".to_owned(),
            ),
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Both branches keep the wrapped detail server-side only. The wire
        // body comes from `parts()` and is always one of a small set of
        // static-or-derived-from-static strings.
        match &self {
            Self::Internal(detail) => error!(detail = %detail, "internal API error"),
            Self::Unauthorized(detail) => warn!(detail = %detail, "unauthorized request"),
            Self::ServiceUnavailable(detail) => {
                warn!(detail = %detail, "service unavailable");
            }
            _ => {}
        }
        let (status, code, message) = self.parts();
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: code.as_str(),
                message: &message,
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<ValidationError> for ApiError {
    fn from(err: ValidationError) -> Self {
        Self::Validation(err.to_string())
    }
}

impl From<RepositoryError> for ApiError {
    fn from(err: RepositoryError) -> Self {
        match err {
            RepositoryError::NotFound { entity } => Self::NotFound { entity },
            RepositoryError::Conflict { entity, .. } => Self::Conflict { entity },
            // A row read/written by the persistence layer that failed domain
            // validation is a data-integrity bug; treat it as internal.
            RepositoryError::Validation { field, message } => {
                Self::Internal(format!("row integrity {field}: {message}"))
            }
            RepositoryError::Database(msg) => Self::Internal(msg),
        }
    }
}

impl From<VaultError> for ApiError {
    fn from(err: VaultError) -> Self {
        match err {
            // Defense-in-depth: the DTO's `parse_supported_key_type` already
            // 400s every non-Ed25519 tag before the vault is called, so this
            // arm is unreachable in normal flow. Kept so a future
            // `SshKeyType` variant added to the DTO allowlist before the
            // vault grows a generator falls through as a clean 400 instead
            // of a 500. Format mirrors the DTO (`{tag:?}`) so the wire
            // message stays identical regardless of which gate fires.
            VaultError::UnsupportedKeyType(tag) => {
                Self::Validation(format!("unsupported key_type {tag:?}"))
            }
            // Master key issues are an operator/deploy problem, not a
            // client problem. Crash with 503 rather than leaking why.
            VaultError::MasterKey(_) => {
                Self::ServiceUnavailable("vault master key invalid".to_owned())
            }
            // Anything else inside the vault is an internal bug — encrypt
            // failures, serialization failures, etc.
            other => Self::Internal(format!("vault: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_unsupported_key_type_message_matches_dto_format() {
        // The DTO's `parse_supported_key_type` emits
        // `unsupported key_type "<tag>"`. The vault fallback must produce
        // the same shape so a future change that exposes this arm doesn't
        // break clients matching on the wire message.
        let err: ApiError = VaultError::UnsupportedKeyType("rsa").into();
        let ApiError::Validation(msg) = err else {
            panic!("expected Validation, got: {err:?}");
        };
        assert_eq!(msg, "unsupported key_type \"rsa\"");
    }
}
