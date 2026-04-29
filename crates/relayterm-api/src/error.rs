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
use relayterm_ssh::{HostKeyPreflightError, ProbeError, SshAuthCheckError};
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
    BadGateway,
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
            Self::BadGateway => "bad_gateway",
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

    /// 502 — an upstream system the request depends on (e.g. an SSH peer
    /// during preflight) failed in a way that's not the client's fault.
    /// The wrapped detail is operator-facing only; the wire body collapses
    /// to the static `bad gateway` message so peer-side topology and
    /// version banners never leak through.
    #[error("bad gateway: {0}")]
    BadGateway(String),

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
            Self::BadGateway(_) => (
                StatusCode::BAD_GATEWAY,
                ErrorCode::BadGateway,
                "bad gateway".to_owned(),
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
            Self::BadGateway(detail) => warn!(detail = %detail, "bad gateway"),
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

impl From<HostKeyPreflightError> for ApiError {
    fn from(err: HostKeyPreflightError) -> Self {
        match err {
            // The decrypted private blob did not parse as an OpenSSH PEM —
            // a vault-issued row should always round-trip, so a failure
            // here is a data-integrity bug rather than a client problem.
            HostKeyPreflightError::InvalidIdentity => {
                Self::Internal("ssh identity material is malformed".to_owned())
            }
            // All probe failures collapse to `bad gateway`. The variant is
            // logged operator-side via the warn! in IntoResponse; the wire
            // body is static.
            HostKeyPreflightError::Probe(probe) => Self::BadGateway(format!("ssh probe: {probe}")),
        }
    }
}

impl From<ProbeError> for ApiError {
    fn from(err: ProbeError) -> Self {
        Self::BadGateway(format!("ssh probe: {err}"))
    }
}

impl From<SshAuthCheckError> for ApiError {
    fn from(err: SshAuthCheckError) -> Self {
        match err {
            // The decrypted blob did not parse — vault row is corrupt.
            // Same shape as the preflight equivalent: a generic 500 with
            // no operator detail on the wire.
            SshAuthCheckError::InvalidIdentity => {
                Self::Internal("ssh identity material is malformed".to_owned())
            }
            // Outbound-network safety guard fired: the process is already
            // running its configured maximum number of auth-checks. Surface
            // as 503 (`service_unavailable`) so the operator UI knows the
            // request is safe to retry. The wire body is the static
            // `service unavailable` string per the ApiError contract.
            SshAuthCheckError::Saturated => {
                Self::ServiceUnavailable("auth-check concurrency limit reached".to_owned())
            }
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
