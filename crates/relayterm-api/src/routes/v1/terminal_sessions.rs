//! Terminal-session lifecycle routes.
//!
//! These endpoints manage the *metadata* surface of a terminal session.
//! The orchestrator behind them (`relayterm_terminal::TerminalSessionManager`)
//! deliberately does NOT open SSH channels, allocate PTYs, or stream
//! terminal data in this slice — see the doc-comments on the manager
//! and on `STUB_PTY_NOT_IMPLEMENTED_MESSAGE` for the full contract.
//!
//! Ownership rules mirror the rest of the v1 API:
//! - The caller's user is taken from the `DevUser` extractor.
//! - `create` verifies the referenced server_profile, host, and identity
//!   all belong to the caller; foreign-owned references collapse to the
//!   same 404 the route would return for a missing resource.
//! - `get_by_id`, `close`, and the `list` filter all scope to the
//!   caller's user, so cross-user existence is never leaked by id.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::ids::TerminalSessionId;
use relayterm_core::repository::{
    HostRepository, KnownHostEntryRepository, ServerProfileRepository, SshIdentityRepository,
    TerminalSessionRepository,
};
use relayterm_terminal::CreateTerminalSessionRequest as ManagerCreateRequest;

use crate::AppState;
use crate::dev_user::DevUser;
use crate::dto::terminal_session::{
    CloseTerminalSessionResponse, CreateTerminalSessionRequest, CreateTerminalSessionResponse,
    TerminalSessionResponse,
};
use crate::error::ApiError;

const ENTITY: &str = "terminal_session";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id))
        .route("/{id}/close", post(close))
}

/// `POST /api/v1/terminal-sessions`.
///
/// Creates terminal-session metadata and an in-memory runtime placeholder.
/// PTY startup and SSH channel allocation are NOT implemented in this
/// slice — the response carries a static `message` that names the stub
/// scope explicitly so the client cannot mistake "row created" for
/// "shell ready."
async fn create(
    State(state): State<AppState>,
    user: DevUser,
    Json(req): Json<CreateTerminalSessionRequest>,
) -> Result<(StatusCode, Json<CreateTerminalSessionResponse>), ApiError> {
    // Resolve the (profile, host, identity) trio scoped to the caller.
    // Any miss — by id OR by ownership — collapses to a single 404 entity
    // ("terminal_session") so cross-user existence is never leaked.
    let profile = state
        .db
        .server_profiles()
        .get(req.server_profile_id)
        .await?
        .filter(|p| p.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let host = state
        .db
        .hosts()
        .get(profile.host_id)
        .await?
        .filter(|h| h.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let _identity = state
        .db
        .ssh_identities()
        .get(profile.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    // Precondition: host key MUST already be pinned and trusted (and not
    // revoked). We do NOT perform a live preflight here — that's the
    // caller's responsibility via `POST /trust-host-key`. Refusing to
    // create a session without a trusted pin keeps the future PTY-bearing
    // implementation from accidentally connecting to an unverified peer.
    let known = state.db.known_host_entries().list_for_host(host.id).await?;
    let any_trusted = known
        .iter()
        .any(|e| e.trusted_at.is_some() && e.revoked_at.is_none());
    if !any_trusted {
        return Err(ApiError::Conflict { entity: "host_key" });
    }

    let outcome = state
        .terminal_sessions
        .create_session(ManagerCreateRequest {
            owner_id: user.0,
            server_profile_id: profile.id,
            cols: req.cols,
            rows: req.rows,
        })
        .await?;

    let body = CreateTerminalSessionResponse {
        session: outcome.session.into(),
        message: outcome.message,
    };
    Ok((StatusCode::CREATED, Json(body)))
}

async fn list(
    State(state): State<AppState>,
    user: DevUser,
) -> Result<Json<Vec<TerminalSessionResponse>>, ApiError> {
    let sessions = state.db.terminal_sessions().list_for_user(user.0).await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(TerminalSessionResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<TerminalSessionResponse>, ApiError> {
    let session = state
        .db
        .terminal_sessions()
        .get(id)
        .await?
        .filter(|s| s.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(session.into()))
}

/// `POST /api/v1/terminal-sessions/:id/close`.
///
/// Idempotent: closing an already-closed session returns 200 with
/// `already_closed = true`. The manager handles ownership filtering —
/// foreign-owned ids surface as the same 404 the route would emit for a
/// missing id.
async fn close(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<CloseTerminalSessionResponse>, ApiError> {
    let outcome = state.terminal_sessions.close_session(id, user.0).await?;
    Ok(Json(CloseTerminalSessionResponse {
        session: outcome.session.into(),
        already_closed: outcome.already_closed,
    }))
}
