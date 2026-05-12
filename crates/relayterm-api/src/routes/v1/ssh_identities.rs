//! SSH identity routes.
//!
//! `POST` generates a fresh keypair inside the vault, encrypts the
//! private material, persists the row, and returns *only* the public
//! metadata. The plaintext private key never leaves the vault call;
//! the encrypted blob never leaves the persistence layer.
//!
//! `PATCH` (rename only) and `DELETE` are the inventory-management
//! surfaces. Both are owner-scoped, CSRF-guarded, and never touch the
//! `encrypted_private_key` column except by deleting the row outright.
//! Private-key material is never on the wire.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::ids::{SshIdentityId, UserId};
use relayterm_core::repository::{
    AuditEventRepository, CreateAuditEvent, CreateSshIdentity, SshIdentityRepository,
};
use relayterm_core::ssh_identity::SshIdentity;
use serde_json::json;

use crate::AppState;
use crate::auth::{AuthenticatedUser, CsrfGuard};
use crate::dto::ssh_identity::{
    CreateSshIdentityRequest, SshIdentityResponse, UpdateSshIdentityRequest,
};
use crate::error::ApiError;

const ENTITY: &str = "ssh_identity";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id).patch(update).delete(delete_by_id))
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

/// `PATCH /api/v1/ssh-identities/:id`.
///
/// Renames an SSH identity owned by the caller. `key_type`,
/// `public_key`, and `encrypted_private_key` are immutable — the route
/// has no field to mutate them, the DTO has no field for them, and the
/// repository's `rename` SQL writes only `name`.
///
/// Foreign / missing collapses to 404 (matching `get_by_id`). No audit
/// event is emitted: there is no `ssh_identity_updated` audit kind
/// today and adding one is out of scope for this slice (would require
/// a CHECK-constraint migration). Identity renames are inventory
/// metadata, not security-critical — the row's `name` column is the
/// authoritative record.
async fn update(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<SshIdentityId>,
    Json(req): Json<UpdateSshIdentityRequest>,
) -> Result<Json<SshIdentityResponse>, ApiError> {
    let user_id = user.user_id();
    // Validate first so a garbage name never reaches the DB.
    let name = req.validated_name()?;
    state
        .db
        .ssh_identities()
        .get(id)
        .await?
        .filter(|i| i.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let updated = state.db.ssh_identities().rename(id, user_id, name).await?;
    Ok(Json(updated.into()))
}

/// `DELETE /api/v1/ssh-identities/:id`.
///
/// Refuses (409 `conflict { entity: "ssh_identity", reason:
/// "referenced" }`) when any `server_profiles` row owned by the
/// caller references this identity. The schema FK
/// `server_profiles.ssh_identity_id ON DELETE RESTRICT` is the
/// race-safe backstop.
///
/// On success, the row (including the `encrypted_private_key` column
/// value) is hard-deleted. This is the ONLY allowed path to removing
/// vault-encrypted private-key bytes from durable storage. The
/// pre-check ensures no profile still expects to authenticate with
/// this key.
///
/// Emits `ssh_identity_deleted` audit BEFORE the DELETE so the audit
/// row exists even if the delete later fails. The audit payload
/// carries public metadata only — id, name, key_type,
/// fingerprint_sha256, created_at. NEVER the encrypted blob or
/// public-key bytes (the `public_key` column is OpenSSH ASCII and
/// public, but treating it conservatively avoids any future
/// re-classification surprise; the fingerprint is the durable public
/// identifier).
async fn delete_by_id(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<SshIdentityId>,
) -> Result<StatusCode, ApiError> {
    let user_id = user.user_id();
    let identity = state
        .db
        .ssh_identities()
        .get(id)
        .await?
        .filter(|i| i.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    if state
        .db
        .ssh_identities()
        .any_dependents_for_user(id, user_id)
        .await?
    {
        return Err(ApiError::Conflict {
            entity: ENTITY,
            reason: Some("referenced"),
        });
    }

    write_ssh_identity_delete_audit(&state, user_id, &identity).await?;
    state.db.ssh_identities().delete(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Build the public-metadata-only payload for an `ssh_identity_deleted`
/// audit event and append it to `audit_events`.
///
/// **Payload contract (security-critical):** the JSON object MUST
/// contain only public metadata. It MUST NOT contain
/// `encrypted_private_key`, `public_key` bytes, plaintext PEM,
/// fingerprint of plaintext material, vault internals, or DB error
/// text. The sentinel-based redaction tests in the API test crate
/// guard this invariant.
///
/// **Failure policy:** fail-closed (matches the server-profile
/// lifecycle audit). A failed audit insert surfaces as
/// `RepositoryError` → `ApiError::Internal`; the route returns BEFORE
/// the row is deleted, so the operator can retry.
async fn write_ssh_identity_delete_audit(
    state: &AppState,
    actor_id: UserId,
    identity: &SshIdentity,
) -> Result<(), ApiError> {
    let payload = json!({
        "ssh_identity_id": identity.id,
        "name": identity.name.as_str(),
        "key_type": identity.key_type,
        "fingerprint_sha256": identity.fingerprint_sha256,
        "created_at": identity.created_at,
    });
    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(actor_id),
            kind: AuditEventKind::SshIdentityDeleted,
            payload,
            remote_addr: None,
        })
        .await?;
    Ok(())
}
