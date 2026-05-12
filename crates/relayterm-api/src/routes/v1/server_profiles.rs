use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::host::Host;
use relayterm_core::ids::{ServerProfileId, UserId};
use relayterm_core::repository::{
    AuditEventRepository, CreateAuditEvent, CreateKnownHostEntry, HostRepository,
    KnownHostEntryRepository, ReplaceActivePin, ServerProfileRepository, SshIdentityRepository,
};
use relayterm_core::server_profile::ServerProfile;
use relayterm_core::ssh_identity::SshIdentity;
use relayterm_ssh::{HostKeyPreflightRequest, HostKeyStatus, SshAuthCheckRequest};
use serde_json::json;
use zeroize::Zeroizing;

use crate::AppState;
use crate::auth::{AuthenticatedUser, CsrfGuard};
use crate::dto::auth_check::{AuthCheckResponse, AuthCheckStatusWire};
use crate::dto::preflight::{
    HostKeyPreflightResponse, HostKeyStatusWire, ReplaceHostKeyRequest, ReplaceHostKeyResponse,
    TrustHostKeyRequest, TrustHostKeyResponse,
};
use crate::dto::server_profile::{
    CreateServerProfileRequest, ServerProfileResponse, UpdateServerProfileRequest,
};
use crate::error::ApiError;

const ENTITY: &str = "server_profile";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id).patch(update).delete(delete_by_id))
        .route("/{id}/disable", post(disable))
        .route("/{id}/enable", post(enable))
        .route("/{id}/host-key-preflight", post(host_key_preflight))
        .route("/{id}/trust-host-key", post(trust_host_key))
        .route("/{id}/replace-host-key", post(replace_host_key))
        .route("/{id}/auth-check", post(auth_check))
}

async fn create(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(req): Json<CreateServerProfileRequest>,
) -> Result<(StatusCode, Json<ServerProfileResponse>), ApiError> {
    let user_id = user.user_id();
    let input = req.into_create(user_id)?;

    // Pre-flight checks so a missing reference returns a 404 the caller can
    // act on rather than the generic 500 a raw FK violation would yield.
    // Both lookups are scoped to the authenticated user — referencing
    // another user's host or identity is treated the same as
    // "does not exist."
    let host = state
        .db
        .hosts()
        .get(input.host_id)
        .await?
        .filter(|h| h.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: "host" })?;
    let identity = state
        .db
        .ssh_identities()
        .get(input.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user_id)
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

    // Lifecycle audit. Public metadata only — no key material, no host
    // banner, no DB error text. Failure policy: fail-closed. The row has
    // already been written; surfacing the audit failure to the caller
    // mirrors the partial-success shape of `create_session` (see the
    // 2026-04-29 lesson in AGENTS.md). The orphan row is operator-visible
    // and can be reconciled, the audit gap cannot.
    write_lifecycle_audit(
        &state,
        user_id,
        AuditEventKind::ServerProfileCreated,
        &profile,
    )
    .await?;

    Ok((StatusCode::CREATED, Json(profile.into())))
}

async fn list(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<ServerProfileResponse>>, ApiError> {
    let profiles = state
        .db
        .server_profiles()
        .list_for_user(user.user_id())
        .await?;
    Ok(Json(
        profiles
            .into_iter()
            .map(ServerProfileResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<ServerProfileResponse>, ApiError> {
    let profile = state
        .db
        .server_profiles()
        .get(id)
        .await?
        .filter(|p| p.owner_id == user.user_id())
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(profile.into()))
}

/// `PATCH /api/v1/server-profiles/:id`.
///
/// Owner-scoped partial update of name / host / identity / username
/// override / tags. Each newly-supplied `host_id` and `ssh_identity_id`
/// is re-resolved under the caller's `owner_id` BEFORE the UPDATE
/// fires — referencing another user's host or identity collapses to
/// the same 404 as "host not found" / "ssh_identity not found", so
/// cross-user existence is never leaked through a PATCH any more than
/// through a create.
///
/// Emits `server_profile_updated` audit on success. Same fail-closed
/// policy as create / disable / enable: a failed audit insert surfaces
/// as 500 with the row mutation already committed.
async fn update(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
    Json(req): Json<UpdateServerProfileRequest>,
) -> Result<Json<ServerProfileResponse>, ApiError> {
    let user_id = user.user_id();
    // Validate body shape BEFORE any DB work, mirroring create.
    let input = req.into_update()?;

    // Owner-scoped resolve for the profile itself. Foreign / missing
    // collapses to 404 — the wire shape matches `get_by_id`.
    state
        .db
        .server_profiles()
        .get(id)
        .await?
        .filter(|p| p.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    // Ownership pre-check on any newly-referenced host / identity.
    // The repository's COALESCE-shaped UPDATE doesn't enforce
    // ownership on the new FK target — the FK alone only checks that
    // the row EXISTS, not who owns it. Pre-checking here means a PATCH
    // that tries to bind to another user's host / identity returns the
    // same `host not found` / `ssh_identity not found` 404 a create
    // would, instead of silently binding to a foreign row.
    if let Some(new_host_id) = input.host_id {
        state
            .db
            .hosts()
            .get(new_host_id)
            .await?
            .filter(|h| h.owner_id == user_id)
            .ok_or(ApiError::NotFound { entity: "host" })?;
    }
    if let Some(new_identity_id) = input.ssh_identity_id {
        state
            .db
            .ssh_identities()
            .get(new_identity_id)
            .await?
            .filter(|i| i.owner_id == user_id)
            .ok_or(ApiError::NotFound {
                entity: "ssh_identity",
            })?;
    }

    let updated = state
        .db
        .server_profiles()
        .update(id, user_id, input)
        .await?;
    write_lifecycle_audit(
        &state,
        user_id,
        AuditEventKind::ServerProfileUpdated,
        &updated,
    )
    .await?;
    Ok(Json(updated.into()))
}

/// `DELETE /api/v1/server-profiles/:id`.
///
/// Refuses (409 `conflict { entity: "server_profile", reason:
/// "referenced" }`) when any `terminal_sessions` row references the
/// profile — live OR closed. `terminal_sessions` rows are NEVER
/// deleted from the user UI (AGENTS.md "Things to avoid"); the schema
/// FK `terminal_sessions.server_profile_id ON DELETE RESTRICT` is the
/// race-safe backstop.
///
/// The recommended alternative for profiles that already have session
/// history is the existing disable flow (`POST :id/disable`) — that
/// preserves history and lifecycle audit AND blocks future launches.
/// The UI surfaces this distinction.
///
/// Owner-scoped: foreign / missing collapses to 404.
///
/// Emits `server_profile_deleted` audit BEFORE the DELETE so the audit
/// row exists even if the delete later fails. The audit row carries
/// public metadata only (id, name, host_id, ssh_identity_id) — same
/// shape as the other lifecycle audits.
async fn delete_by_id(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
) -> Result<StatusCode, ApiError> {
    let user_id = user.user_id();
    let profile = state
        .db
        .server_profiles()
        .get(id)
        .await?
        .filter(|p| p.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    if state
        .db
        .server_profiles()
        .any_dependents_for_user(id, user_id)
        .await?
    {
        return Err(ApiError::Conflict {
            entity: ENTITY,
            reason: Some("referenced"),
        });
    }

    // Audit before the delete: the row content (`profile`) is still
    // available to build the public-metadata payload. If the delete
    // then fails (FK race) the audit row is harmless — it records
    // "operator intent to delete" with the row's last-known snapshot.
    // The 409 from the FK conflict is the user-facing answer.
    write_lifecycle_audit(
        &state,
        user_id,
        AuditEventKind::ServerProfileDeleted,
        &profile,
    )
    .await?;
    state.db.server_profiles().delete(id, user_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/server-profiles/:id/disable`.
///
/// Stamps `disabled_at` on a profile owned by the caller. Idempotent: a
/// second disable preserves the original timestamp (the SQL only writes
/// when `disabled_at IS NULL`). Foreign-owned or missing ids collapse to
/// the same 404 a regular get_by_id would return — cross-user existence
/// is never leaked.
///
/// Disabled profiles are blocked from new launches, auth-check, host-key
/// preflight, and host-key trust. Existing live `terminal_sessions` are
/// unaffected — disable is a launch-time gate, not a runtime kill switch.
/// See SPEC.md "Inventory lifecycle and destructive-action policy".
async fn disable(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<ServerProfileResponse>, ApiError> {
    let user_id = user.user_id();
    // Owner-scoped read first so the conflict / current-state check
    // collapses cross-user existence to 404 BEFORE the UPDATE runs.
    let current = state
        .db
        .server_profiles()
        .get(id)
        .await?
        .filter(|p| p.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    // Idempotency: if already disabled, return the existing row unchanged
    // — preserving the original `disabled_at` is the audit-correct shape.
    // Bumping `updated_at` on a redundant disable would be misleading.
    //
    // TOCTOU: two concurrent callers can both pass this guard and both
    // reach `set_disabled_at`. The outcome is benign — the SQL writes
    // unconditionally, the second writer overwrites with a near-identical
    // timestamp, and both callers return a consistent post-update row.
    // No data is lost. In the concurrent case both callers ALSO append
    // a `server_profile_disabled` audit row, which technically violates
    // the "idempotent calls write zero rows" contract — duplicate audit
    // rows in this race are harmless (operator-visible, near-identical
    // recorded_at), so the slice ships without serialising. If
    // timestamp-preservation OR strict zero-duplicate audit ever becomes
    // load-bearing, push the guard into SQL via `WHERE disabled_at IS
    // NULL` and treat zero affected rows as the "already disabled" case
    // (mirrors `record_trusted`).
    if current.is_disabled() {
        return Ok(Json(current.into()));
    }

    let updated = state
        .db
        .server_profiles()
        .set_disabled_at(id, user_id, Some(chrono::Utc::now()))
        .await?;

    // Audit only on the enabled -> disabled transition. The early-return
    // above means we only reach here when `current.is_disabled()` was
    // false, so a redundant disable does NOT produce a duplicate row.
    // Fail-closed: see `create` for the rationale.
    write_lifecycle_audit(
        &state,
        user_id,
        AuditEventKind::ServerProfileDisabled,
        &updated,
    )
    .await?;

    Ok(Json(updated.into()))
}

/// `POST /api/v1/server-profiles/:id/enable`.
///
/// Clears `disabled_at` on a profile owned by the caller. Idempotent: a
/// second enable on an already-enabled row is a no-op and returns the
/// row unchanged. Foreign-owned or missing ids collapse to the same 404
/// as the rest of the route surface.
async fn enable(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<ServerProfileResponse>, ApiError> {
    let user_id = user.user_id();
    let current = state
        .db
        .server_profiles()
        .get(id)
        .await?
        .filter(|p| p.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    if !current.is_disabled() {
        return Ok(Json(current.into()));
    }

    let updated = state
        .db
        .server_profiles()
        .set_disabled_at(id, user_id, None)
        .await?;

    // Audit only on the disabled -> enabled transition. Same fail-closed
    // policy as `create` / `disable`.
    write_lifecycle_audit(
        &state,
        user_id,
        AuditEventKind::ServerProfileEnabled,
        &updated,
    )
    .await?;

    Ok(Json(updated.into()))
}

/// Build the public-metadata-only payload for a server-profile lifecycle
/// audit event and append it to `audit_events`.
///
/// **Payload contract (security-critical):** the JSON object MUST contain
/// only public metadata: ids, the profile name, and the `disabled_at`
/// timestamp. It MUST NOT contain `private_key`, `encrypted_private_key`,
/// PEM bytes, public-key bytes, terminal I/O, replay frames, raw russh
/// errors, vault internals, or DB error text. Sentinel-style redaction
/// tests in the API test crate guard this invariant on every lifecycle
/// path.
///
/// **Failure policy:** fail-closed. A failed audit insert surfaces as
/// `RepositoryError` → `ApiError::Internal` to the caller. The lifecycle
/// row state is already committed; the orphan-without-audit shape mirrors
/// the partial-success pattern documented for `create_session` (see the
/// 2026-04-29 lesson in AGENTS.md). Audit gaps are worse than orphan rows
/// because they can't be reconstructed after the fact.
async fn write_lifecycle_audit(
    state: &AppState,
    actor_id: UserId,
    kind: AuditEventKind,
    profile: &ServerProfile,
) -> Result<(), ApiError> {
    let payload = json!({
        "server_profile_id": profile.id,
        "name": profile.name.as_str(),
        "host_id": profile.host_id,
        "ssh_identity_id": profile.ssh_identity_id,
        "disabled_at": profile.disabled_at,
    });
    state
        .db
        .audit_events()
        .create(CreateAuditEvent {
            actor_id: Some(actor_id),
            kind,
            payload,
            // Client IP / user-agent capture is deferred — see SPEC.md.
            // Recording `None` here is intentional, not a bug to fix in a
            // drive-by edit; the column is nullable for exactly this case.
            remote_addr: None,
        })
        .await?;
    Ok(())
}

/// 409 conflict shape returned by every route that refuses to operate on
/// a disabled profile (auth-check, host-key preflight/trust, terminal
/// launch). The message reads `"server_profile disabled"`; the wire
/// `code` stays `conflict` so clients keep parsing.
fn server_profile_disabled_conflict() -> ApiError {
    ApiError::Conflict {
        entity: ENTITY,
        reason: Some("disabled"),
    }
}

/// Resolve the (profile, host, identity) trio for a preflight-style request.
///
/// All three lookups are scoped to the authenticated user; any miss — by
/// id OR by ownership — collapses to the same `server_profile not found`
/// response so cross-user existence is never leaked. See the lesson in
/// AGENTS.md (`API get_by_id ownership`).
async fn resolve_owned_profile(
    state: &AppState,
    user_id: UserId,
    profile_id: ServerProfileId,
) -> Result<(ServerProfile, Host, SshIdentity), ApiError> {
    let profile = state
        .db
        .server_profiles()
        .get(profile_id)
        .await?
        .filter(|p| p.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let host = state
        .db
        .hosts()
        .get(profile.host_id)
        .await?
        .filter(|h| h.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let identity = state
        .db
        .ssh_identities()
        .get(profile.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user_id)
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
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<HostKeyPreflightResponse>, ApiError> {
    let (profile, host, identity) = resolve_owned_profile(&state, user.user_id(), id).await?;
    // Disabled profiles cannot be probed. Re-enabling is the documented
    // path back to a live setup surface — keeping preflight refuse-only
    // for now matches the launch / trust / auth-check guards and keeps
    // semantics simple. See SPEC.md "Inventory lifecycle..." for why.
    if profile.is_disabled() {
        return Err(server_profile_disabled_conflict());
    }
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

    // When status is `Changed`, expose the active pin's fingerprint so
    // the SPA can offer the host-key replace flow without a separate
    // known-host-entries fetch. The active pin is the row that flagged
    // the change: same key_type as the captured key, non-revoked,
    // already trusted, with a fingerprint that differs from the captured
    // one. We surface ONLY the public fingerprint string — no public-key
    // bytes, no row id, no audit data. For `Unknown` and `Trusted` the
    // field stays `None`: an unknown host has no pin to replace, and a
    // trusted host has nothing to consent to revoke.
    //
    // `find()` returns the first matching row. Today the
    // `replace_active_pin` repository invariant + the trust route's
    // refusal to overwrite a `changed` pin guarantee at most one
    // active, trusted, non-revoked row per `(host_id, key_type)` —
    // so there is exactly one candidate to surface. The
    // `host_key_replace` design (`docs/spec/host-key-replace.md` § R5)
    // and the `replace_active_pin`'s `FOR UPDATE` lock keep that
    // invariant on the write path; if a future code path were to
    // intentionally allow multiple active rows of the same key type,
    // this projection would need to revisit how it resolves the
    // displayed "old" fingerprint for the SPA.
    let active_pin_fingerprint = if matches!(host_key_status, HostKeyStatusWire::Changed) {
        known
            .iter()
            .find(|e| {
                e.revoked_at.is_none()
                    && e.trusted_at.is_some()
                    && e.key_type == result.captured.key_type
                    && e.fingerprint_sha256 != result.captured.fingerprint_sha256
            })
            .map(|e| e.fingerprint_sha256.clone())
    } else {
        None
    };

    Ok(Json(HostKeyPreflightResponse {
        profile_id: profile.id,
        host_id: host.id,
        hostname,
        port,
        host_key_status,
        host_key_type: result.captured.key_type,
        host_key_fingerprint: result.captured.fingerprint_sha256,
        active_pin_fingerprint,
        message: HostKeyPreflightResponse::message_for(host_key_status),
    }))
}

async fn trust_host_key(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
    Json(req): Json<TrustHostKeyRequest>,
) -> Result<Json<TrustHostKeyResponse>, ApiError> {
    // Validate the fingerprint shape BEFORE doing any DB or SSH work — a
    // garbage body shouldn't open a network connection.
    let expected = req.validated_expected_fingerprint()?.to_owned();

    let (profile, host, identity) = resolve_owned_profile(&state, user.user_id(), id).await?;
    // Disabled profiles cannot be trusted. The enable route is the
    // explicit return path; trust must not be a sneaky bypass.
    if profile.is_disabled() {
        return Err(server_profile_disabled_conflict());
    }
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
        return Err(ApiError::Conflict {
            entity: "host_key",
            reason: None,
        });
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
        return Err(ApiError::Conflict {
            entity: "host_key",
            reason: None,
        });
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

/// `POST /api/v1/server-profiles/:id/replace-host-key`.
///
/// Atomically revokes the active pinned host key and trusts a new one.
/// The replace flow is the operator-sanctioned recovery path from the
/// `changed` outcome the regular `trust-host-key` route refuses — it
/// preserves the TOFU posture (no auto-overwrite) by requiring BOTH the
/// active pin's fingerprint AND the freshly-captured fingerprint AND a
/// canonical reason code.
///
/// Order of operations (see `docs/spec/host-key-replace.md` § R5):
/// 1. `CsrfGuard` first — reject bad-Origin requests before any DB or
///    body work.
/// 2. Validate request shape (fingerprint format, reason-code accept-list).
/// 3. Resolve `(profile, host, identity)` scoped to the caller — foreign
///    or missing collapses to byte-identical 404.
/// 4. Refuse if the profile is disabled (mirrors trust / preflight /
///    auth-check / launch guards).
/// 5. Decrypt the identity (vault must be configured).
/// 6. Run a fresh probe to capture the current host key.
/// 7. Initial-shape checks against the in-memory known-host list:
///    - `captured_unchanged` if probe matches the active pin (no-op).
///    - `captured_mismatch` if probe differs from `expected_new_fingerprint`.
///    - `captured_revoked` if a revoked row already exists for the
///      captured fingerprint.
/// 8. Call `replace_active_pin` — repository serialises around the active
///    row's `FOR UPDATE` lock and emits the paired audit rows in the
///    same transaction. An audit-insert failure rolls the row mutations
///    back; the route surfaces it as a 500.
async fn replace_host_key(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
    Json(req): Json<ReplaceHostKeyRequest>,
) -> Result<Json<ReplaceHostKeyResponse>, ApiError> {
    // 1. Validate input BEFORE any DB or network work — a garbage body
    //    must not reach the probe / vault / repository.
    let validated = req.validated()?;

    // 2. Owner-scoped resolve. Cross-user existence collapses to 404.
    let user_id = user.user_id();
    let (profile, host, identity) = resolve_owned_profile(&state, user_id, id).await?;

    // 3. Disabled profiles cannot be replaced — re-enable first. Mirrors
    //    the launch / trust / auth-check / preflight guards. Replace must
    //    not be a sneaky bypass.
    if profile.is_disabled() {
        return Err(server_profile_disabled_conflict());
    }

    // 4. Decrypt the identity for the probe. The plaintext lives in a
    //    `Zeroizing<Vec<u8>>` and never crosses another `await` boundary
    //    after the `preflight.preflight` call returns.
    let pem = decrypt_identity(&state, &identity)?;

    // 5. Fresh capture. Mirror the username / port / hostname computation
    //    used by `host_key_preflight` and `trust_host_key` so all three
    //    routes probe the same target.
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

    // 6. Initial-shape checks (§ R5 step 6). The order matters: each
    //    branch produces a precise SPA copy keyed off the typed reason.
    //
    //    - `captured_unchanged` first: if the probe matches the active
    //      pin the host has not actually changed; replace is a no-op.
    //      Surfacing this distinct from a stale `expected_new_fingerprint`
    //      lets the SPA say "the host key didn't change — your preflight
    //      result is stale" without the operator second-guessing the
    //      request body.
    //    - `captured_mismatch` next: the probe captured a different key
    //      than the operator just confirmed in their preflight modal.
    //      The host could have rotated again mid-flow, OR a BGP-shaped
    //      MITM is in progress — either way, do not write.
    //    - `captured_revoked` last: a revoked row already exists for
    //      the captured fingerprint. A revoked-and-reappearing key MUST
    //      refuse re-trust through this path; recovery is a deliberate
    //      admin-only future surface (mirrors the symmetric guard in
    //      `trust_host_key`).
    if result.captured.fingerprint_sha256 == validated.expected_old_fingerprint {
        return Err(ApiError::Conflict {
            entity: "host_key",
            reason: Some("captured_unchanged"),
        });
    }
    if result.captured.fingerprint_sha256 != validated.expected_new_fingerprint {
        return Err(ApiError::Conflict {
            entity: "host_key",
            reason: Some("captured_mismatch"),
        });
    }
    if known.iter().any(|e| {
        e.key_type == result.captured.key_type
            && e.fingerprint_sha256 == result.captured.fingerprint_sha256
            && e.revoked_at.is_some()
    }) {
        return Err(ApiError::Conflict {
            entity: "host_key",
            reason: Some("captured_revoked"),
        });
    }

    // 7. Atomic replace + paired audit. The repository's `replace_active_pin`
    //    locks the active row, refuses if `expected_old_fingerprint` does
    //    not match (collapses no-active-pin AND active-pin-mismatch into
    //    the same typed conflict), inserts the new trusted row, updates
    //    the old row's revoke metadata, AND appends both audit rows —
    //    all in one transaction. The repository's TOCTOU close re-checks
    //    the captured fingerprint inside the open transaction; an audit
    //    failure rolls the row mutations back.
    let replaced = state
        .db
        .known_host_entries()
        .replace_active_pin(ReplaceActivePin {
            host_id: host.id,
            expected_old_fingerprint: validated.expected_old_fingerprint.clone(),
            new_key_type: result.captured.key_type,
            new_fingerprint_sha256: result.captured.fingerprint_sha256.clone(),
            new_public_key: result.captured.public_key.clone(),
            revoked_by: user_id,
            reason_code: validated.reason_code,
        })
        .await
        .map_err(map_replace_repository_error)?;

    let trusted_at = replaced.trusted_new.trusted_at.ok_or_else(|| {
        ApiError::Internal("known_host_entry.trusted_at NULL after replace_active_pin".to_owned())
    })?;

    Ok(Json(ReplaceHostKeyResponse {
        profile_id: profile.id,
        host_id: host.id,
        revoked_known_host_entry_id: replaced.revoked_old.id,
        revoked_fingerprint: replaced.revoked_old.fingerprint_sha256.clone(),
        trusted_known_host_entry_id: replaced.trusted_new.id,
        trusted_fingerprint: replaced.trusted_new.fingerprint_sha256.clone(),
        host_key_type: replaced.trusted_new.key_type,
        trusted_at,
    }))
}

/// Map a [`RepositoryError`] returned by `replace_active_pin` to a typed
/// [`ApiError`]. The repository emits three relevant `Conflict`
/// constraints — `active_pin_mismatch`, `new_fingerprint_revoked`, and
/// `new_fingerprint_already_active` — each surfaced through a stable
/// `host_key` 409 reason so the SPA can render precise copy without
/// scraping the wire `message`. Anything else flows through the generic
/// `RepositoryError → ApiError` path (e.g. a Database driver failure
/// turns into a 500).
///
/// The wire reasons match the spec table (§ R4); the third reason
/// (`new_fingerprint_already_active`) is impossible in normal flow today
/// — the `captured_unchanged` / `captured_mismatch` checks above already
/// ensure the captured fingerprint is fresh AND distinct from the active
/// pin — but the repository contract is the load-bearing source of
/// truth, so we surface it explicitly for forward compatibility.
fn map_replace_repository_error(err: relayterm_core::repository::RepositoryError) -> ApiError {
    use relayterm_core::repository::RepositoryError;
    if let RepositoryError::Conflict {
        entity: "known_host_entry",
        constraint,
    } = &err
    {
        let reason = match constraint.as_str() {
            "active_pin_mismatch" => Some("active_pin_mismatch"),
            "new_fingerprint_revoked" => Some("captured_revoked"),
            "new_fingerprint_already_active" => Some("new_fingerprint_already_active"),
            _ => None,
        };
        if let Some(reason) = reason {
            return ApiError::Conflict {
                entity: "host_key",
                reason: Some(reason),
            };
        }
    }
    err.into()
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
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<ServerProfileId>,
) -> Result<Json<AuthCheckResponse>, ApiError> {
    let (profile, host, identity) = resolve_owned_profile(&state, user.user_id(), id).await?;
    // Disabled profiles cannot be auth-checked. Re-enable first.
    if profile.is_disabled() {
        return Err(server_profile_disabled_conflict());
    }
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
