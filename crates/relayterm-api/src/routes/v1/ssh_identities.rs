//! SSH identity routes.
//!
//! `POST` is intentionally absent: keypair generation requires the vault
//! crate (encryption + key derivation) which is not yet implemented. This
//! slice exposes only the safe metadata surface — list and read.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use relayterm_core::ids::SshIdentityId;
use relayterm_core::repository::SshIdentityRepository;

use crate::AppState;
use crate::dev_user::DevUser;
use crate::dto::ssh_identity::SshIdentityResponse;
use crate::error::ApiError;

const ENTITY: &str = "ssh_identity";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/{id}", get(get_by_id))
}

async fn list(
    State(state): State<AppState>,
    user: DevUser,
) -> Result<Json<Vec<SshIdentityResponse>>, ApiError> {
    let identities = state.db.ssh_identities().list_for_user(user.0).await?;
    Ok(Json(
        identities
            .into_iter()
            .map(SshIdentityResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<SshIdentityId>,
) -> Result<Json<SshIdentityResponse>, ApiError> {
    let identity = state
        .db
        .ssh_identities()
        .get(id)
        .await?
        .filter(|i| i.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(identity.into()))
}
