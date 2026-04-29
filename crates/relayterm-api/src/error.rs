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
    InternalError,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
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
