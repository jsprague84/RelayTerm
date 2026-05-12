use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::ids::HostId;
use relayterm_core::repository::HostRepository;

use crate::AppState;
use crate::auth::{AuthenticatedUser, CsrfGuard};
use crate::dto::host::{CreateHostRequest, HostResponse, UpdateHostRequest};
use crate::error::ApiError;

const ENTITY: &str = "host";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id).patch(update).delete(delete_by_id))
}

async fn create(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(req): Json<CreateHostRequest>,
) -> Result<(StatusCode, Json<HostResponse>), ApiError> {
    let input = req.into_create(user.user_id())?;
    let host = state.db.hosts().create(input).await?;
    Ok((StatusCode::CREATED, Json(host.into())))
}

async fn list(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<HostResponse>>, ApiError> {
    let hosts = state.db.hosts().list_for_user(user.user_id()).await?;
    Ok(Json(hosts.into_iter().map(HostResponse::from).collect()))
}

async fn get_by_id(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<HostId>,
) -> Result<Json<HostResponse>, ApiError> {
    // Cross-user reads must be indistinguishable from a missing row — the
    // ownership mismatch and the genuinely-absent case both produce the
    // same `NotFound` response so we don't leak existence by id.
    let host = state
        .db
        .hosts()
        .get(id)
        .await?
        .filter(|h| h.owner_id == user.user_id())
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(host.into()))
}

/// `PATCH /api/v1/hosts/:id`.
///
/// Owner-scoped partial update. Validation mirrors `create`: each
/// supplied field passes through the same domain validator (length,
/// charset, port range). An empty body — no fields present — is a
/// `400 invalid_input` so a caller bug surfaces immediately rather
/// than as a silent no-op that bumps `updated_at` for nothing.
///
/// No audit event is emitted: hosts do not currently audit lifecycle
/// (create / update / delete) and adding a new `host_*` audit kind is
/// out of scope for this slice (would require a CHECK migration). The
/// route is owner-scoped and re-validates every input at the boundary;
/// the host row's `updated_at` and the row content itself are the
/// authoritative record.
async fn update(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<HostId>,
    Json(req): Json<UpdateHostRequest>,
) -> Result<Json<HostResponse>, ApiError> {
    let user_id = user.user_id();
    // Validate the body BEFORE touching the DB — a garbage PATCH must
    // not even reach the owner-scoped lookup, mirroring the
    // create-route pattern.
    let input = req.into_update()?;
    // Resolve under owner scope first so a foreign-owned id collapses
    // to 404 with the same wire shape as `get_by_id`. The repository's
    // UPDATE also enforces owner scoping; this lookup is the leak-prevention
    // first line.
    state
        .db
        .hosts()
        .get(id)
        .await?
        .filter(|h| h.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let updated = state.db.hosts().update(id, user_id, input).await?;
    Ok(Json(updated.into()))
}

/// `DELETE /api/v1/hosts/:id`.
///
/// Refuses (409 `conflict { entity: "host", reason: "referenced" }`)
/// when at least one dependent row exists for this host — today that
/// means a `server_profiles` row owned by the caller AND/OR any
/// `known_host_entries` row. Both are pre-checked with one round trip
/// via `HostRepository::any_dependents_for_user`. The schema FK
/// `server_profiles.host_id ON DELETE RESTRICT` is the race-safe
/// backstop: a concurrent profile-create that slips between the
/// pre-check and the DELETE surfaces as the same 409 via the FK branch
/// of `map_sqlx_error`.
///
/// `known_host_entries.host_id ON DELETE CASCADE` would otherwise let
/// the host delete silently delete pinned-trust history. AGENTS.md
/// "Do not hard-delete known_host_entries" — refusing host delete when
/// any known-host row exists is the route-layer enforcement.
///
/// Foreign-owned or missing ids return the same 404 as the read path.
/// No audit event is emitted (no `host_deleted` kind exists; out of
/// scope for this slice).
async fn delete_by_id(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<HostId>,
) -> Result<StatusCode, ApiError> {
    let user_id = user.user_id();
    state
        .db
        .hosts()
        .get(id)
        .await?
        .filter(|h| h.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    if state
        .db
        .hosts()
        .any_dependents_for_user(id, user_id)
        .await?
    {
        return Err(ApiError::Conflict {
            entity: ENTITY,
            reason: Some("referenced"),
        });
    }
    state.db.hosts().delete(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
