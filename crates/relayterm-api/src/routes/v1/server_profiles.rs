use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::ids::ServerProfileId;
use relayterm_core::repository::{HostRepository, ServerProfileRepository, SshIdentityRepository};

use crate::AppState;
use crate::dev_user::DevUser;
use crate::dto::server_profile::{CreateServerProfileRequest, ServerProfileResponse};
use crate::error::ApiError;

const ENTITY: &str = "server_profile";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id))
}

async fn create(
    State(state): State<AppState>,
    user: DevUser,
    Json(req): Json<CreateServerProfileRequest>,
) -> Result<(StatusCode, Json<ServerProfileResponse>), ApiError> {
    let input = req.into_create(user)?;

    // Pre-flight checks so a missing reference returns a 404 the caller can
    // act on rather than the generic 500 a raw FK violation would yield.
    // Both lookups are scoped to the dev user — referencing another user's
    // host or identity is treated the same as "does not exist."
    let host = state
        .db
        .hosts()
        .get(input.host_id)
        .await?
        .filter(|h| h.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: "host" })?;
    let identity = state
        .db
        .ssh_identities()
        .get(input.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user.0)
        .ok_or(ApiError::NotFound {
            entity: "ssh_identity",
        })?;
    // Tripwires for refactors that move the prefetch elsewhere — the
    // `.filter()`s above already guarantee the id match in release, so a
    // mismatch here would mean `repository.get(id)` started returning a
    // different row than asked for. Cheap to keep, free at runtime.
    debug_assert_eq!(host.id, input.host_id);
    debug_assert_eq!(identity.id, input.ssh_identity_id);

    let profile = state.db.server_profiles().create(input).await?;
    Ok((StatusCode::CREATED, Json(profile.into())))
}

async fn list(
    State(state): State<AppState>,
    user: DevUser,
) -> Result<Json<Vec<ServerProfileResponse>>, ApiError> {
    let profiles = state.db.server_profiles().list_for_user(user.0).await?;
    Ok(Json(
        profiles
            .into_iter()
            .map(ServerProfileResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<ServerProfileResponse>, ApiError> {
    let profile = state
        .db
        .server_profiles()
        .get(id)
        .await?
        .filter(|p| p.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(profile.into()))
}
