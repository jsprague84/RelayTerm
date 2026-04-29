use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::ids::HostId;
use relayterm_core::repository::HostRepository;

use crate::AppState;
use crate::dev_user::DevUser;
use crate::dto::host::{CreateHostRequest, HostResponse};
use crate::error::ApiError;

const ENTITY: &str = "host";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id))
}

async fn create(
    State(state): State<AppState>,
    user: DevUser,
    Json(req): Json<CreateHostRequest>,
) -> Result<(StatusCode, Json<HostResponse>), ApiError> {
    let input = req.into_create(user)?;
    let host = state.db.hosts().create(input).await?;
    Ok((StatusCode::CREATED, Json(host.into())))
}

async fn list(
    State(state): State<AppState>,
    user: DevUser,
) -> Result<Json<Vec<HostResponse>>, ApiError> {
    let hosts = state.db.hosts().list_for_user(user.0).await?;
    Ok(Json(hosts.into_iter().map(HostResponse::from).collect()))
}

async fn get_by_id(
    State(state): State<AppState>,
    _user: DevUser,
    Path(id): Path<HostId>,
) -> Result<Json<HostResponse>, ApiError> {
    let host = state
        .db
        .hosts()
        .get(id)
        .await?
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(host.into()))
}
