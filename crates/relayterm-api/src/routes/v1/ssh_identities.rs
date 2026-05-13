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
    AuditEventRepository, CreateAuditEvent, CreateSshIdentity, RepositoryError,
    SshIdentityRepository,
};
use relayterm_core::ssh_identity::SshIdentity;
use serde_json::json;

use crate::AppState;
use crate::auth::{AuthenticatedUser, CsrfGuard};
use crate::dto::ssh_identity::{
    CreateSshIdentityRequest, ImportSshIdentityRequest, SshIdentityResponse,
    UpdateSshIdentityRequest,
};
use crate::error::ApiError;

const ENTITY: &str = "ssh_identity";

/// Discriminator carried in the `ssh_identity_created` audit payload so a
/// reader can tell `POST /ssh-identities` (`generated`) from
/// `POST /ssh-identities/import` (`imported`) without inspecting the
/// route. Public metadata only — see `write_ssh_identity_create_audit`.
#[derive(Debug, Clone, Copy)]
enum AuditSource {
    Generated,
    Imported,
}

impl AuditSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Imported => "imported",
        }
    }
}

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/import", post(import))
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
    let vault = vault_or_503(&state)?;

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
        .await
        .map_err(map_create_repository_error)?;

    write_ssh_identity_create_audit(&state, user.user_id(), &identity, AuditSource::Generated)
        .await?;

    Ok((StatusCode::CREATED, Json(identity.into())))
}

/// `POST /api/v1/ssh-identities/import`.
///
/// Imports an existing OpenSSH-format Ed25519 private key into the
/// vault. v1 scope (see `docs/private-key-import.md` § 1):
///
/// * Unencrypted OpenSSH-format Ed25519 only.
/// * Paste-into-textarea ingress (no file picker, no passphrase
///   channel, no RSA / ECDSA / DSA, no Putty `.ppk`, no PEM PKCS#1 /
///   PKCS#8). Each rejection collapses to a typed `400 invalid_input`
///   with a stable `unsupported_key_format <reason>` /
///   `unsupported key_type "<tag>"` message — never the parser's text,
///   never the supplied PEM bytes.
/// * Reuses the existing `EncryptedBlob` envelope; an imported row is
///   indistinguishable at rest from a generated row.
/// * Duplicate fingerprint (per `(owner_id, fingerprint_sha256)` unique
///   constraint) collapses to a typed `409 conflict { entity:
///   "ssh_identity", reason: "duplicate_fingerprint" }`.
/// * Audits via `ssh_identity_created` with `source: "imported"`.
///
/// Extractor order matches every other browser-write route in the
/// crate: `_csrf` runs FIRST so a bad `Origin` returns 403 BEFORE the
/// JSON body extractor parses anything (mirroring
/// `bad_origin_rejects_before_body_parsing` from
/// `crates/relayterm-api/tests/api.rs`).
async fn import(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(req): Json<ImportSshIdentityRequest>,
) -> Result<(StatusCode, Json<SshIdentityResponse>), ApiError> {
    let validated = req.validate()?;

    let vault = vault_or_503(&state)?;

    // The vault parses, validates, re-serializes to canonical PEM, and
    // encrypts. Parser/encryption failures collapse to typed
    // `VaultError` variants; the `From<VaultError> for ApiError` impl in
    // `crate::error` maps them to safe wire shapes — the original
    // parser text never crosses the API boundary.
    let imported = vault.import_ssh_identity(&validated.pem, &validated.name)?;
    // Drop the `Zeroizing<Vec<u8>>` PEM buffer as soon as the vault is
    // done with it — explicitly, BEFORE the DB-create await — so the
    // plaintext bytes are wiped right now rather than at function
    // return (which would be after `db.create(...)` AND
    // `write_ssh_identity_create_audit(...)`). The only durable form
    // from this point on is `imported.encrypted_private_key`.
    drop(validated.pem);

    let identity = state
        .db
        .ssh_identities()
        .create(CreateSshIdentity {
            owner_id: user.user_id(),
            name: validated.name,
            key_type: imported.key_type,
            public_key: imported.public_key_openssh,
            encrypted_private_key: imported.encrypted_private_key.into_bytes(),
            fingerprint_sha256: imported.fingerprint_sha256,
        })
        .await
        .map_err(map_create_repository_error)?;

    write_ssh_identity_create_audit(&state, user.user_id(), &identity, AuditSource::Imported)
        .await?;

    Ok((StatusCode::CREATED, Json(identity.into())))
}

/// Resolve `state.vault` or return the static 503 the create + import
/// routes share. Centralised so both creation paths produce
/// byte-identical wire bodies when the vault is disabled.
fn vault_or_503(state: &AppState) -> Result<&relayterm_vault::VaultService, ApiError> {
    state.vault.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "vault is disabled; backend-generated SSH identities require a master key".to_owned(),
        )
    })
}

/// Map a [`RepositoryError`] from `ssh_identities().create(...)` to a
/// typed [`ApiError`]. The unique index on
/// `(owner_id, fingerprint_sha256)` (`ssh_identities_owner_fingerprint_key`)
/// is the source of truth for "this caller already owns a key with this
/// fingerprint" — both the generate and the import routes hit it; on
/// import the 409 is the routine duplicate-paste case, on generate it
/// is essentially unreachable (a fresh keypair colliding with an
/// existing fingerprint is astronomically unlikely) but the mapping is
/// shared so the wire shape is uniform.
///
/// Anything else flows through the generic `RepositoryError → ApiError`
/// path (e.g. a Database driver failure turns into a 500).
fn map_create_repository_error(err: RepositoryError) -> ApiError {
    if let RepositoryError::Conflict {
        entity: ENTITY,
        constraint,
    } = &err
    {
        if constraint == "ssh_identities_owner_fingerprint_key" {
            return ApiError::Conflict {
                entity: ENTITY,
                reason: Some("duplicate_fingerprint"),
            };
        }
    }
    err.into()
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

/// Build the public-metadata-only payload for an `ssh_identity_created`
/// audit event and append it to `audit_events`.
///
/// **Payload contract (security-critical):** the JSON object MUST
/// contain only public metadata. It MUST NOT contain
/// `encrypted_private_key`, `public_key` bytes, plaintext PEM, vault
/// internals, raw russh / parser / DB error text, peer banners, or the
/// `client_info` blob from `terminal_session_attachments`. The
/// sentinel-based redaction tests in the API test crate
/// (`AUDIT_FORBIDDEN_SUBSTRINGS`) guard this invariant for both the
/// generate-route emission (`source: "generated"`) and the
/// import-route emission (`source: "imported"`).
///
/// **Failure policy:** fail-closed (mirror of
/// `write_ssh_identity_delete_audit`). A failed audit insert surfaces
/// as `RepositoryError → ApiError::Internal`. The audit append happens
/// AFTER the DB row is created (mirroring the generate-path
/// precedent); on a route retry the unique-fingerprint constraint
/// would refuse the second insert with a clean 409, so a duplicate
/// audit row from a routine retry cannot accumulate.
async fn write_ssh_identity_create_audit(
    state: &AppState,
    actor_id: UserId,
    identity: &SshIdentity,
    source: AuditSource,
) -> Result<(), ApiError> {
    let payload = json!({
        "ssh_identity_id": identity.id,
        "name": identity.name.as_str(),
        "key_type": identity.key_type,
        "fingerprint_sha256": identity.fingerprint_sha256,
        "created_at": identity.created_at,
        "source": source.as_str(),
    });
    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(actor_id),
            kind: AuditEventKind::SshIdentityCreated,
            payload,
            remote_addr: None,
        })
        .await?;
    Ok(())
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
