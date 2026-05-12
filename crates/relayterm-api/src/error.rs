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
use relayterm_auth::AuthServiceError;
use relayterm_core::repository::RepositoryError;
use relayterm_core::validation::ValidationError;
use relayterm_ssh::{HostKeyPreflightError, ProbeError, SshAuthCheckError};
use relayterm_terminal::TerminalSessionManagerError;
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
    CsrfOriginMismatch,
    NotFound,
    Conflict,
    TooManyRequests,
    /// Phase 1B.1 quota refusal — per-user live PTY ceiling reached.
    /// Distinct from [`Self::TooManyRequests`] (the login throttler's
    /// rate-limit code) so the SPA can map the two wires to different
    /// copy without parsing the static `message` string. See
    /// `docs/session-quotas.md` § 7.1.
    TooManySessions,
    /// Phase 1B.2a quota refusal — per-user starting-burst ceiling
    /// reached (see `docs/session-quotas.md` § 4.3 / § 7.1). Distinct
    /// from [`Self::TooManySessions`] so the SPA can map the
    /// "in-flight burst" cause to different copy than the live-cap
    /// cause without parsing the static `message` string.
    TooManyStartingSessions,
    BadGateway,
    ServiceUnavailable,
    InternalError,
}

impl ErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::Unauthorized => "unauthorized",
            Self::CsrfOriginMismatch => "csrf_origin_mismatch",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::TooManyRequests => "too_many_requests",
            Self::TooManySessions => "too_many_sessions",
            Self::TooManyStartingSessions => "too_many_starting_sessions",
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

    /// 403 — request rejected by the inline CSRF Origin guard. Emitted
    /// by the auth routes (login / logout / bootstrap) before any auth
    /// or DB work runs. The wrapped detail is operator-facing only;
    /// the wire body collapses to the static `forbidden` message and
    /// the wire `code` is `csrf_origin_mismatch` per SPEC.md "CSRF
    /// posture". When the shared CSRF middleware lands (SPEC step 6)
    /// this variant moves there in the same commit.
    #[error("forbidden: {0}")]
    CsrfOriginMismatch(String),

    /// 404 — the addressed entity does not exist (or is not visible to the caller).
    #[error("{entity} not found")]
    NotFound { entity: &'static str },

    /// 409 — uniqueness or referential constraint violated.
    ///
    /// `reason` is an optional short stable discriminator (`"disabled"`,
    /// `"closed"`, etc.) included when the route wants the wire message to
    /// distinguish *why* the conflict fired — e.g. a disabled
    /// `server_profile` vs. a profile-name-uniqueness clash. The wire
    /// envelope still uses the static `code = "conflict"`; the variant
    /// rides in the message string so existing clients keep parsing.
    #[error("{entity} {}", reason.unwrap_or("conflict"))]
    Conflict {
        entity: &'static str,
        reason: Option<&'static str>,
    },

    /// 429 — request rejected because the caller is rate-limited.
    /// Currently emitted only by the login throttler at
    /// `POST /api/v1/auth/login` (SPEC.md "Password authentication
    /// (v1)" → "Throttling"). The wrapped detail is operator-facing
    /// only; the wire body collapses to the static `too many requests`
    /// message so the throttle key and timing telemetry stay
    /// server-side. The route layer is responsible for any
    /// `Retry-After` header — it is not derived from this variant.
    #[error("too many requests: {0}")]
    TooManyRequests(String),

    /// 429 — request rejected because the caller has reached their
    /// per-user live-PTY ceiling (Phase 1B.1 quota — see
    /// `docs/session-quotas.md` § 7.1). Emitted by
    /// `POST /api/v1/terminal-sessions` AFTER ownership + host-key
    /// gating but BEFORE any vault decrypt or SSH side effect.
    ///
    /// Wire envelope is `429 { code: "too_many_sessions", message:
    /// "too many terminal sessions" }` — distinct from
    /// [`Self::TooManyRequests`] (login throttle) so the SPA can map
    /// the two wires to different copy. NO `Retry-After` header
    /// (the user must act, not wait on a wall clock). NO operator
    /// detail on the wire — no count, no cap, no session ids, no
    /// hostnames. The operator-side `warn!` line (built at the call
    /// site BEFORE returning this variant) carries the safe public
    /// metadata (`user_id`, `current_count`, `cap`).
    #[error("too many terminal sessions")]
    TooManySessions,

    /// 429 — request rejected because the caller has reached their
    /// per-user starting-burst ceiling (Phase 1B.2a quota — see
    /// `docs/session-quotas.md` § 4.3 / § 7.1). Emitted by
    /// `POST /api/v1/terminal-sessions` AFTER ownership + host-key
    /// gating but BEFORE any vault decrypt or SSH side effect.
    ///
    /// Wire envelope is `429 { code: "too_many_starting_sessions",
    /// message: "too many starting terminal sessions" }` — distinct
    /// from [`Self::TooManySessions`] so the SPA can map the
    /// in-flight-burst cause to different copy than the live-cap
    /// cause. Same redaction posture as the live variant: NO
    /// `Retry-After` header, NO operator detail on the wire (no
    /// count, no cap, no session ids, no hostnames). The
    /// operator-side `warn!` line built at the call site carries the
    /// safe public metadata (`user_id`, `scope = "per_user_starting"`,
    /// `current_count`, `cap`).
    #[error("too many starting terminal sessions")]
    TooManyStartingSessions,

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
            Self::CsrfOriginMismatch(_) => (
                StatusCode::FORBIDDEN,
                ErrorCode::CsrfOriginMismatch,
                "forbidden".to_owned(),
            ),
            Self::NotFound { entity } => (
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                format!("{entity} not found"),
            ),
            Self::Conflict { entity, reason } => (
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                match reason {
                    Some(r) => format!("{entity} {r}"),
                    None => format!("{entity} conflict"),
                },
            ),
            Self::TooManyRequests(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::TooManyRequests,
                "too many requests".to_owned(),
            ),
            Self::TooManySessions => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::TooManySessions,
                "too many terminal sessions".to_owned(),
            ),
            Self::TooManyStartingSessions => (
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::TooManyStartingSessions,
                "too many starting terminal sessions".to_owned(),
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
            Self::CsrfOriginMismatch(detail) => warn!(detail = %detail, "csrf origin mismatch"),
            Self::TooManyRequests(detail) => warn!(detail = %detail, "too many requests"),
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
            RepositoryError::Conflict { entity, .. } => Self::Conflict {
                entity,
                reason: None,
            },
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

impl From<TerminalSessionManagerError> for ApiError {
    fn from(err: TerminalSessionManagerError) -> Self {
        match err {
            TerminalSessionManagerError::InvalidDimensions { field, message } => {
                Self::Validation(format!("{field}: {message}"))
            }
            TerminalSessionManagerError::NotFound => Self::NotFound {
                entity: "terminal_session",
            },
            // Closed-session attach/resize → 409. Distinct from 404 so the
            // operator UI can tell "no such session" from "session is gone."
            TerminalSessionManagerError::SessionClosed => Self::Conflict {
                entity: "terminal_session",
                reason: None,
            },
            // The session row exists but its live PTY runtime is gone
            // (start failed, shell exited, or never bound). Surface as
            // 409 conflict so the operator UI can tell "row missing"
            // (404) from "row present, runtime gone" (409).
            TerminalSessionManagerError::PtyNotLive => Self::Conflict {
                entity: "pty_runtime",
                reason: None,
            },
            // PTY-startup errors only reach the API layer if a route
            // forwards one without prior translation. The terminal-
            // sessions create route handles them via `map_pty_start_error`
            // for precise typed mapping; this fallback is the safe net.
            TerminalSessionManagerError::PtyStart(inner) => {
                Self::Internal(format!("ssh pty: {inner}"))
            }
            TerminalSessionManagerError::Repository(e) => e.into(),
        }
    }
}

impl From<AuthServiceError> for ApiError {
    fn from(err: AuthServiceError) -> Self {
        match err {
            // Every "you are not (or no longer) authenticated" shape
            // collapses to the same 401 on the wire. The structural
            // distinction (`InvalidCredentials` vs `SessionInvalid` vs
            // `SessionExpired` vs `SessionRevoked`) survives in the
            // operator-side `warn!` line via the wrapped detail string;
            // the client sees the static `unauthorized` body either
            // way.
            AuthServiceError::InvalidCredentials => {
                Self::Unauthorized("invalid credentials".to_owned())
            }
            AuthServiceError::SessionInvalid => Self::Unauthorized("session invalid".to_owned()),
            AuthServiceError::SessionExpired => Self::Unauthorized("session expired".to_owned()),
            AuthServiceError::SessionRevoked => Self::Unauthorized("session revoked".to_owned()),
            AuthServiceError::Repository(detail) => Self::Internal(format!("auth repo: {detail}")),
            AuthServiceError::Crypto => Self::Internal("auth crypto failure".to_owned()),
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
