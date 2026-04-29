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
use tracing::error;

/// Stable error code strings emitted on the wire. The enum keeps the set
/// closed so handlers can't invent ad-hoc codes that clients then depend on.
#[derive(Debug, Clone, Copy)]
#[allow(unreachable_pub)]
pub enum ErrorCode {
    InvalidInput,
    NotFound,
    Conflict,
    InternalError,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
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
        if let Self::Internal(detail) = &self {
            error!(detail = %detail, "internal API error");
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
