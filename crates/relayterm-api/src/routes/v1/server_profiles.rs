use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::host::Host;
use relayterm_core::ids::ServerProfileId;
use relayterm_core::repository::{
    CreateKnownHostEntry, HostRepository, KnownHostEntryRepository, ServerProfileRepository,
    SshIdentityRepository,
};
use relayterm_core::server_profile::ServerProfile;
use relayterm_core::ssh_identity::SshIdentity;
use relayterm_ssh::{HostKeyPreflightRequest, HostKeyStatus, SshAuthCheckRequest};
use zeroize::Zeroizing;

use crate::AppState;
use crate::dev_user::DevUser;
use crate::dto::auth_check::{AuthCheckResponse, AuthCheckStatusWire};
use crate::dto::preflight::{
    HostKeyPreflightResponse, HostKeyStatusWire, TrustHostKeyRequest, TrustHostKeyResponse,
};
use crate::dto::server_profile::{CreateServerProfileRequest, ServerProfileResponse};
use crate::error::ApiError;

const ENTITY: &str = "server_profile";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id))
        .route("/{id}/host-key-preflight", post(host_key_preflight))
        .route("/{id}/trust-host-key", post(trust_host_key))
        .route("/{id}/auth-check", post(auth_check))
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

/// Resolve the (profile, host, identity) trio for a preflight-style request.
///
/// All three lookups are scoped to the authenticated user; any miss — by
/// id OR by ownership — collapses to the same `server_profile not found`
/// response so cross-user existence is never leaked. See the lesson in
/// AGENTS.md (`API get_by_id ownership`).
async fn resolve_owned_profile(
    state: &AppState,
    user: DevUser,
    profile_id: ServerProfileId,
) -> Result<(ServerProfile, Host, SshIdentity), ApiError> {
    let profile = state
        .db
        .server_profiles()
        .get(profile_id)
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
    let identity = state
        .db
        .ssh_identities()
        .get(profile.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok((profile, host, identity))
}

/// Decrypt the identity's private key into a zeroizing buffer for the
/// preflight call. The bytes never cross another `await` boundary.
fn decrypt_identity(
    state: &AppState,
    identity: &SshIdentity,
) -> Result<Zeroizing<Vec<u8>>, ApiError> {
    let vault = state.vault.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "vault is disabled; preflight requires a master key".to_owned(),
        )
    })?;
    Ok(vault.decrypt_private_key(&identity.encrypted_private_key)?)
}

/// `POST /api/v1/server-profiles/:id/host-key-preflight`.
///
/// Captures the server's host key during KEX, classifies it against the
/// host's pinned `known_host_entries`, and returns a structured status.
/// Disconnects before SSH authentication — see the wire-contract note on
/// [`HostKeyPreflightResponse`] for what this attests to and what it
/// deliberately does NOT.
async fn host_key_preflight(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<HostKeyPreflightResponse>, ApiError> {
    let (profile, host, identity) = resolve_owned_profile(&state, user, id).await?;
    let pem = decrypt_identity(&state, &identity)?;

    // Username override falls back to the host default. Validation already
    // guarantees both are well-formed SSH usernames.
    let username = profile
        .username_override
        .as_ref()
        .map_or_else(|| host.default_username.as_str(), |u| u.as_str())
        .to_owned();
    let hostname = host.hostname.as_str().to_owned();
    let port = host.port.get();

    let known = state.db.known_host_entries().list_for_host(host.id).await?;
    let req = HostKeyPreflightRequest {
        host_id: host.id,
        hostname: hostname.clone(),
        port,
        username,
        private_key_pem: pem,
    };
    let result = state.preflight.preflight(req, &known).await?;
    let host_key_status: HostKeyStatusWire = result.status.into();

    Ok(Json(HostKeyPreflightResponse {
        profile_id: profile.id,
        host_id: host.id,
        hostname,
        port,
        host_key_status,
        host_key_type: result.captured.key_type,
        host_key_fingerprint: result.captured.fingerprint_sha256,
        message: HostKeyPreflightResponse::message_for(host_key_status),
    }))
}

async fn trust_host_key(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<ServerProfileId>,
    Json(req): Json<TrustHostKeyRequest>,
) -> Result<Json<TrustHostKeyResponse>, ApiError> {
    // Validate the fingerprint shape BEFORE doing any DB or SSH work — a
    // garbage body shouldn't open a network connection.
    let expected = req.validated_expected_fingerprint()?.to_owned();

    let (profile, host, identity) = resolve_owned_profile(&state, user, id).await?;
    let pem = decrypt_identity(&state, &identity)?;

    // Mirror the username fallback used in `host_key_preflight` so both
    // routes probe the same target. Username is not consulted during KEX
    // today, but if a future slice ever binds a host key to a username
    // (e.g. multi-listener detection), the two probes must agree or one
    // could classify as `Trusted` while the other re-flags `Unknown`.
    let username = profile
        .username_override
        .as_ref()
        .map_or_else(|| host.default_username.as_str(), |u| u.as_str())
        .to_owned();
    let hostname = host.hostname.as_str().to_owned();

    let known = state.db.known_host_entries().list_for_host(host.id).await?;
    let preflight_req = HostKeyPreflightRequest {
        host_id: host.id,
        hostname,
        port: host.port.get(),
        username,
        private_key_pem: pem,
    };
    let result = state.preflight.preflight(preflight_req, &known).await?;

    // Defence-in-depth, in this order:
    // 1. The classifier flags `Changed` first — that means an active pin
    //    exists with a different fingerprint, which we never auto-overwrite.
    // 2. Even if the table is empty, the caller's expected fingerprint
    //    must match the captured one. Otherwise the host's key changed
    //    between preflight and trust, or the caller posted a stale value.
    if result.status == HostKeyStatus::Changed || result.captured.fingerprint_sha256 != expected {
        return Err(ApiError::Conflict { entity: "host_key" });
    }

    // Refuse to re-trust a fingerprint that was explicitly revoked. The
    // classifier filters revoked rows out of `Trusted`/`Changed`, so the
    // captured-vs-expected check above doesn't catch this on its own —
    // a revoked-and-reappearing key would otherwise be treated as
    // first-time-seen `Unknown` and pinned. The `record_trusted` SQL
    // also enforces this via `WHERE revoked_at IS NULL` on the conflict
    // branch; this guard is the user-facing layer and produces a clean
    // 409 before any write is attempted. Recovery from a revoked entry
    // is a separate, deliberate operator action that does not exist yet.
    if known.iter().any(|e| {
        e.key_type == result.captured.key_type
            && e.fingerprint_sha256 == result.captured.fingerprint_sha256
            && e.revoked_at.is_some()
    }) {
        return Err(ApiError::Conflict { entity: "host_key" });
    }

    let entry = state
        .db
        .known_host_entries()
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: result.captured.key_type,
            fingerprint_sha256: result.captured.fingerprint_sha256.clone(),
            public_key: result.captured.public_key.clone(),
        })
        .await?;

    // The repository upsert always returns a row with `trusted_at` set —
    // the SQL stamps NOW() on insert and COALESCEs on conflict. If the
    // column is somehow NULL we treat it as a data-integrity bug.
    let trusted_at = entry.trusted_at.ok_or_else(|| {
        ApiError::Internal("known_host_entry.trusted_at NULL after record_trusted".to_owned())
    })?;

    Ok(Json(TrustHostKeyResponse {
        known_host_entry_id: entry.id,
        host_id: entry.host_id,
        host_key_type: entry.key_type,
        host_key_fingerprint: entry.fingerprint_sha256,
        trusted_at,
    }))
}

/// `POST /api/v1/server-profiles/:id/auth-check`.
///
/// Authenticated SSH credential check for a saved server profile. Connects
/// to the host, verifies the host key matches an active, trusted, non-
/// revoked pin, attempts public-key authentication, and disconnects. Does
/// NOT open a PTY, run a shell, execute a command, or persist any session.
///
/// Host-key trust is a precondition. If the host key isn't already pinned
/// and trusted, the route returns a typed `host_key_unknown` /
/// `host_key_changed` status WITHOUT attempting authentication, so no
/// client signature is ever sent to an unverified peer.
async fn auth_check(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<AuthCheckResponse>, ApiError> {
    let (profile, host, identity) = resolve_owned_profile(&state, user, id).await?;
    let pem = decrypt_identity(&state, &identity)?;

    let username = profile
        .username_override
        .as_ref()
        .map_or_else(|| host.default_username.as_str(), |u| u.as_str())
        .to_owned();
    let port = host.port.get();

    let known = state.db.known_host_entries().list_for_host(host.id).await?;
    let req = SshAuthCheckRequest {
        host_id: host.id,
        hostname: host.hostname.as_str().to_owned(),
        port,
        username,
        private_key_pem: pem,
    };
    // `SshAuthCheckError` → `ApiError` mapping lives in `crate::error`;
    // a stuck checker, an oversubscribed semaphore, and a corrupt vault
    // row each get the right HTTP status without operator detail leaking.
    let result = state.auth_check.auth_check(req, &known).await?;

    let status: AuthCheckStatusWire = result.status.into();
    Ok(Json(AuthCheckResponse {
        profile_id: profile.id,
        host_id: host.id,
        ssh_identity_id: identity.id,
        status,
        message: AuthCheckResponse::message_for(status),
        checked_at: chrono::Utc::now(),
    }))
}
