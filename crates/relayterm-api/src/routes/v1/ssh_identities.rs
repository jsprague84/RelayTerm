//! SSH identity routes.
//!
//! `POST` generates a fresh keypair inside the vault, encrypts the
//! private material, persists the row, and returns *only* the public
//! metadata. The plaintext private key never leaves the vault call;
//! the encrypted blob never leaves the persistence layer.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::ids::SshIdentityId;
use relayterm_core::repository::{CreateSshIdentity, SshIdentityRepository};

use crate::AppState;
use crate::auth::{AuthenticatedUser, CsrfGuard};
use crate::dto::ssh_identity::{CreateSshIdentityRequest, SshIdentityResponse};
use crate::error::ApiError;

const ENTITY: &str = "ssh_identity";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id))
}

async fn create(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(req): Json<CreateSshIdentityRequest>,
) -> Result<(StatusCode, Json<SshIdentityResponse>), ApiError> {
    let validated = req.validate()?;

    // The vault is the single point that owns the master key and the
    // generated private bytes. If it isn't configured we 503 — refusing
    // to silently degrade to an unencrypted or no-op path.
    let vault = state.vault.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "vault is disabled; backend-generated SSH identities require a master key".to_owned(),
        )
    })?;

    // Generate inside the vault. The plaintext PEM exists only inside this
    // call and is wiped before `encrypted_private_key` is handed back.
    let generated = vault.generate_ssh_identity(validated.key_type, &validated.name)?;

    let identity = state
        .db
        .ssh_identities()
        .create(CreateSshIdentity {
            owner_id: user.user_id(),
            name: validated.name,
            key_type: generated.key_type,
            public_key: generated.public_key_openssh,
            encrypted_private_key: generated.encrypted_private_key.into_bytes(),
            fingerprint_sha256: generated.fingerprint_sha256,
        })
        .await?;

    Ok((StatusCode::CREATED, Json(identity.into())))
}

async fn list(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<SshIdentityResponse>>, ApiError> {
    let identities = state
        .db
        .ssh_identities()
        .list_for_user(user.user_id())
        .await?;
    Ok(Json(
        identities
            .into_iter()
            .map(SshIdentityResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<SshIdentityId>,
) -> Result<Json<SshIdentityResponse>, ApiError> {
    let identity = state
        .db
        .ssh_identities()
        .get(id)
        .await?
        .filter(|i| i.owner_id == user.user_id())
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(identity.into()))
}
