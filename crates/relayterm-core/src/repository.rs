//! Repository contracts.
//!
//! Each entity has a small, explicit trait. Storage backends (Postgres,
//! in-memory test fakes) implement these. Callers above the persistence
//! layer depend only on the traits and the typed inputs in this module.
//!
//! The traits intentionally do NOT compose into a generic `Repository<T>` —
//! when auth, vault access, and host-key verification arrive, having
//! readable per-entity contracts is more valuable than DRY.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;

use crate::audit_event::{AuditEvent, AuditEventKind};
use crate::host::Host;
use crate::ids::{
    AuditEventId, HostId, ServerProfileId, SessionEventId, SshIdentityId,
    TerminalSessionAttachmentId, TerminalSessionId, UserId, UserSessionId,
};
use crate::known_host::KnownHostEntry;
use crate::password_credential::PasswordCredential;
use crate::server_profile::ServerProfile;
use crate::session_event::{SessionEvent, SessionEventKind};
use crate::ssh_identity::{SshIdentity, SshKeyType};
use crate::terminal_recording::{
    TerminalRecordingChunk, TerminalRecordingCompression, TerminalRecordingMarker,
    TerminalRecordingMarkerKind, TerminalRecordingMetadata, TerminalRecordingPayloadEncryption,
};
use crate::terminal_session::{
    ReconciledTerminalSession, TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use crate::user::User;
use crate::user_session::UserSession;
use crate::validation::{HostDisplayName, Hostname, ProfileName, SshPort, SshUsername, Tag};

/// Errors a repository call may return.
///
/// Public surface deliberately omits raw SQL, query parameters, and any
/// secret-bearing payloads — backends should wrap underlying driver errors
/// into [`RepositoryError::Database`] with a short, generic message.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    /// The requested row does not exist.
    #[error("{entity} not found")]
    NotFound { entity: &'static str },

    /// A unique constraint was violated (duplicate email, profile name, etc.).
    /// `constraint` is a short, human-readable identifier (the schema's
    /// constraint name is suitable). It must not contain SQL or secrets.
    #[error("{entity} conflict: {constraint}")]
    Conflict {
        entity: &'static str,
        constraint: String,
    },

    /// A value read from the database failed domain validation, or a caller
    /// supplied an out-of-range primitive (e.g. a negative cols/rows).
    #[error("invalid {field}: {message}")]
    Validation {
        field: &'static str,
        message: String,
    },

    /// Catch-all for driver/IO/integrity errors. The message is intended for
    /// operator logs, not end users; do not include secrets or raw SQL.
    #[error("database error: {0}")]
    Database(String),
}

// ----------------------------------------------------------------------
// Inputs
// ----------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CreateUser {
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone)]
pub struct CreateHost {
    pub owner_id: UserId,
    pub display_name: HostDisplayName,
    pub hostname: Hostname,
    pub port: SshPort,
    pub default_username: SshUsername,
}

/// `Debug` is implemented manually so [`Self::encrypted_private_key`]
/// never leaks into tracing logs or error messages.
#[derive(Clone)]
pub struct CreateSshIdentity {
    pub owner_id: UserId,
    pub name: String,
    pub key_type: SshKeyType,
    /// OpenSSH-format public key bytes.
    pub public_key: Vec<u8>,
    /// Encrypted private key ciphertext. Treated as opaque by the repository.
    pub encrypted_private_key: Vec<u8>,
    /// SHA-256 fingerprint of the public key, hex-encoded.
    pub fingerprint_sha256: String,
}

impl fmt::Debug for CreateSshIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateSshIdentity")
            .field("owner_id", &self.owner_id)
            .field("name", &self.name)
            .field("key_type", &self.key_type)
            .field("public_key_len", &self.public_key.len())
            .field(
                "encrypted_private_key",
                &format_args!("<redacted: {} bytes>", self.encrypted_private_key.len()),
            )
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct CreateServerProfile {
    pub owner_id: UserId,
    pub name: ProfileName,
    pub host_id: HostId,
    pub ssh_identity_id: SshIdentityId,
    pub username_override: Option<SshUsername>,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone)]
pub struct CreateKnownHostEntry {
    pub host_id: HostId,
    pub key_type: SshKeyType,
    pub fingerprint_sha256: String,
    pub public_key: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CreateTerminalSession {
    pub owner_id: UserId,
    pub server_profile_id: ServerProfileId,
    pub status: TerminalSessionStatus,
    pub cols: u16,
    pub rows: u16,
}

/// Input for opening a new attachment row against an existing
/// [`TerminalSession`].
///
/// `attached_at` is set by the database default. `detached_at` and
/// `last_seen_seq` are `NULL` until the WebSocket handler closes the
/// attachment and writes the resume bookkeeping.
#[derive(Debug, Clone)]
pub struct CreateTerminalSessionAttachment {
    pub session_id: TerminalSessionId,
    /// Free-form client info (`User-Agent`, Tauri build, etc.) for audit.
    pub client_info: Option<String>,
    /// Source IP at attachment time. Recorded for audit; not used for auth.
    pub remote_addr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateSessionEvent {
    pub session_id: TerminalSessionId,
    pub kind: SessionEventKind,
    pub payload: JsonValue,
}

#[derive(Debug, Clone)]
pub struct CreateAuditEvent {
    pub actor_id: Option<UserId>,
    pub kind: AuditEventKind,
    pub payload: JsonValue,
    pub remote_addr: Option<String>,
}

/// Repository input for upserting a password credential.
///
/// `Debug` is implemented manually so [`Self::password_hash`] never
/// leaks into tracing logs or error messages. The auth service is the
/// only caller; it hashes the plaintext password (Argon2id, PHC string)
/// before constructing this input.
#[derive(Clone)]
pub struct CreatePasswordCredential {
    pub user_id: UserId,
    /// Argon2id PHC string (`$argon2id$...`). Sensitive — never log.
    pub password_hash: String,
}

impl fmt::Debug for CreatePasswordCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatePasswordCredential")
            .field("user_id", &self.user_id)
            .field(
                "password_hash",
                &format_args!("<redacted: {} chars>", self.password_hash.len()),
            )
            .finish()
    }
}

/// Repository input for issuing a new browser session row.
///
/// `Debug` is implemented manually so [`Self::token_hash`] never leaks
/// into tracing logs or error messages. The auth service generates the
/// random cookie token, SHA-256-hashes it, and passes only the digest
/// here.
#[derive(Clone)]
pub struct CreateUserSession {
    pub user_id: UserId,
    /// SHA-256 digest of the random cookie token. Sensitive — paired
    /// with a captured plaintext token it permits session takeover.
    pub token_hash: Vec<u8>,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for CreateUserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateUserSession")
            .field("user_id", &self.user_id)
            .field(
                "token_hash",
                &format_args!("<redacted: {} bytes>", self.token_hash.len()),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Repository input for appending one chunk of recorded PTY OUTPUT bytes.
///
/// `Debug` is implemented manually so [`Self::payload`] never leaks into
/// tracing logs or error messages — the bytes are sensitive PTY output
/// (see `crate::terminal_recording` module docs).
///
/// `seq_start`/`seq_end` are `i64` so the binding to Postgres `BIGINT` is
/// trivial; the schema CHECKs (`seq_start >= 1`, `seq_end >= seq_start`,
/// `byte_len > 0 AND byte_len <= 2 MiB`, `octet_length(payload) =
/// byte_len`) are the load-bearing validations. `byte_len` is `i32` for
/// the same reason `BYTEA` length stays an `INTEGER` column.
#[derive(Clone)]
pub struct CreateTerminalRecordingChunk {
    pub terminal_session_id: TerminalSessionId,
    pub seq_start: i64,
    pub seq_end: i64,
    pub byte_len: i32,
    pub payload: Vec<u8>,
    pub encryption: TerminalRecordingPayloadEncryption,
    pub compression: TerminalRecordingCompression,
}

impl fmt::Debug for CreateTerminalRecordingChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateTerminalRecordingChunk")
            .field("terminal_session_id", &self.terminal_session_id)
            .field("seq_start", &self.seq_start)
            .field("seq_end", &self.seq_end)
            .field("byte_len", &self.byte_len)
            .field(
                "payload",
                &format_args!("<redacted: {} bytes>", self.payload.len()),
            )
            .field("encryption", &self.encryption)
            .field("compression", &self.compression)
            .finish()
    }
}

/// Repository input for appending one recording marker row.
///
/// `payload` is metadata-only by contract — see
/// `crate::terminal_recording` module docs. The repository implementation
/// does NOT inspect the JSON for sentinels; the writer above is
/// responsible for building objects field-by-field.
#[derive(Debug, Clone)]
pub struct CreateTerminalRecordingMarker {
    pub terminal_session_id: TerminalSessionId,
    pub kind: TerminalRecordingMarkerKind,
    pub seq: i64,
    pub payload: JsonValue,
}

// ----------------------------------------------------------------------
// Traits
// ----------------------------------------------------------------------

#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn create(&self, input: CreateUser) -> Result<User, RepositoryError>;
    async fn get(&self, id: UserId) -> Result<Option<User>, RepositoryError>;
    async fn get_by_email(&self, email: &str) -> Result<Option<User>, RepositoryError>;
    async fn touch_last_login(&self, id: UserId, at: DateTime<Utc>) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait HostRepository: Send + Sync {
    async fn create(&self, input: CreateHost) -> Result<Host, RepositoryError>;
    async fn get(&self, id: HostId) -> Result<Option<Host>, RepositoryError>;
    async fn list_for_user(&self, owner_id: UserId) -> Result<Vec<Host>, RepositoryError>;
}

#[async_trait]
pub trait SshIdentityRepository: Send + Sync {
    async fn create(&self, input: CreateSshIdentity) -> Result<SshIdentity, RepositoryError>;
    async fn get(&self, id: SshIdentityId) -> Result<Option<SshIdentity>, RepositoryError>;
    async fn list_for_user(&self, owner_id: UserId) -> Result<Vec<SshIdentity>, RepositoryError>;
}

#[async_trait]
pub trait ServerProfileRepository: Send + Sync {
    async fn create(&self, input: CreateServerProfile) -> Result<ServerProfile, RepositoryError>;
    async fn get(&self, id: ServerProfileId) -> Result<Option<ServerProfile>, RepositoryError>;
    async fn list_for_user(&self, owner_id: UserId) -> Result<Vec<ServerProfile>, RepositoryError>;
    /// Set or clear `disabled_at` on a profile owned by `owner_id`.
    ///
    /// Behavior:
    /// - `disabled_at = Some(t)` writes the timestamp; `None` clears it.
    /// - The update is scoped to `(id, owner_id)`. A row not owned by the
    ///   caller, or absent, returns [`RepositoryError::NotFound`] — the
    ///   route layer maps that to a single 404 so cross-user existence
    ///   isn't leaked.
    /// - The implementation writes the column unconditionally and bumps
    ///   `updated_at = NOW()`. Idempotency is enforced one layer up:
    ///   the disable / enable routes early-return when the requested
    ///   state already holds, so a redundant operator action does not
    ///   reach this method at all. Callers that genuinely want to write
    ///   the same state again (e.g. an admin "re-stamp" workflow that
    ///   doesn't exist today) get the bump.
    /// - Returns the post-update [`ServerProfile`] row.
    async fn set_disabled_at(
        &self,
        id: ServerProfileId,
        owner_id: UserId,
        disabled_at: Option<DateTime<Utc>>,
    ) -> Result<ServerProfile, RepositoryError>;
}

#[async_trait]
pub trait KnownHostEntryRepository: Send + Sync {
    async fn create(&self, input: CreateKnownHostEntry) -> Result<KnownHostEntry, RepositoryError>;
    async fn list_for_host(&self, host_id: HostId) -> Result<Vec<KnownHostEntry>, RepositoryError>;
    async fn find_by_fingerprint(
        &self,
        host_id: HostId,
        fingerprint_sha256: &str,
    ) -> Result<Option<KnownHostEntry>, RepositoryError>;
    /// Idempotently record a trusted known-host entry.
    ///
    /// Behavior:
    /// 1. If no row exists for `(host_id, fingerprint_sha256)`, insert
    ///    one stamped with `trusted_at = NOW()`.
    /// 2. If a row exists AND `revoked_at IS NULL`, stamp `trusted_at`
    ///    only if it was previously unset (preserves audit history).
    /// 3. If a row exists AND `revoked_at` is set, return
    ///    [`RepositoryError::Conflict`] with constraint `"revoked"`.
    ///    A revoked fingerprint must NEVER be silently re-trusted —
    ///    recovery is an explicit operator action that does not have an
    ///    implementation in this slice.
    ///
    /// Used only by the explicit trust-host-key route. Never called by
    /// the preflight path: preflight is read-only against this table.
    async fn record_trusted(
        &self,
        input: CreateKnownHostEntry,
    ) -> Result<KnownHostEntry, RepositoryError>;
}

#[async_trait]
pub trait TerminalSessionRepository: Send + Sync {
    async fn create(
        &self,
        input: CreateTerminalSession,
    ) -> Result<TerminalSession, RepositoryError>;
    async fn get(&self, id: TerminalSessionId) -> Result<Option<TerminalSession>, RepositoryError>;
    async fn list_for_user(
        &self,
        owner_id: UserId,
    ) -> Result<Vec<TerminalSession>, RepositoryError>;
    async fn set_status(
        &self,
        id: TerminalSessionId,
        status: TerminalSessionStatus,
        closed_at: Option<DateTime<Utc>>,
    ) -> Result<(), RepositoryError>;
    /// Open a new attachment row. Each `(client connect → client drop)`
    /// pair gets its own row; the WebSocket handler closes the row by
    /// writing `detached_at` + `last_seen_seq` once the dedicated update
    /// methods land.
    async fn create_attachment(
        &self,
        input: CreateTerminalSessionAttachment,
    ) -> Result<TerminalSessionAttachment, RepositoryError>;
    async fn list_attachments(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Vec<TerminalSessionAttachment>, RepositoryError>;
    /// Used by audit/test code to look up an attachment row directly.
    async fn get_attachment(
        &self,
        id: TerminalSessionAttachmentId,
    ) -> Result<Option<TerminalSessionAttachment>, RepositoryError>;
    /// Stamp `detached_at` and `last_seen_seq` on an attachment row.
    ///
    /// Idempotent: a second call against an already-detached row is a
    /// no-op and returns `Ok(())`. Distinguishing "first detach" from
    /// "redundant detach" is the manager's responsibility (the runtime
    /// registry tracks live attachments) — repositories don't fight the
    /// caller about this.
    ///
    /// Returns [`RepositoryError::NotFound`] if the attachment id does
    /// not exist; the manager treats that as an internal bug since it
    /// only calls this for attachments it just registered.
    async fn mark_attachment_detached(
        &self,
        id: TerminalSessionAttachmentId,
        detached_at: DateTime<Utc>,
        last_seen_seq: Option<i64>,
    ) -> Result<(), RepositoryError>;

    /// Sweep terminal sessions whose runtime entry was lost across a
    /// backend restart and transition them to `closed`.
    ///
    /// A session whose `status` is `starting`, `active`, or `detached`
    /// only has meaning while the backend's in-memory
    /// `TerminalSessionManager` still owns its `russh::Channel`,
    /// broadcast fanout, and replay ring buffer. Once the process
    /// exits, those resources are unrecoverable (see
    /// `docs/terminal-recording.md` Section 9.1) and the row would
    /// otherwise remain operator-visible as a stale "live" session
    /// the UI cannot ever resume.
    ///
    /// Behavior:
    /// - In a single transaction, locks candidate rows
    ///   (`status IN ('starting','active','detached')` `FOR UPDATE`),
    ///   transitions each to `closed` with `closed_at = at` and
    ///   `last_seen_at = NOW()`, AND inserts one matching
    ///   `session_events` row per reconciled session with `kind =
    ///   closed` and a payload of `{ "reason":
    ///   "startup_reconciliation", "previous_status": <old>,
    ///   "reconciled_at": at }`. The status transition AND its
    ///   matching session_event are committed together — a partial
    ///   reconciliation that closes a row without an audit trail is
    ///   not possible (`docs/terminal-recording.md` Section 9.3).
    /// - Idempotent. A second call finds no candidates and returns an
    ///   empty `Vec` without writing anything.
    /// - Does NOT touch `terminal_recording_chunks`. Chunks are
    ///   append-only and survive the restart unchanged so the existing
    ///   closed-session replay path keeps working.
    /// - Appends one `terminal_recording_markers` row with `kind =
    ///   closed`, `seq = MAX(seq_end)` across the session's chunks,
    ///   and `payload = { "reason": "startup_reconciliation",
    ///   "previous_status": <prior>, "reconciled_at": at }` for any
    ///   reconciled session that has at least one chunk row.
    ///   Idempotency is enforced at the schema layer by the partial
    ///   unique index `terminal_recording_markers_session_closed_seq_uidx`
    ///   on `(terminal_session_id, seq) WHERE kind = 'closed'`; the
    ///   INSERT uses `ON CONFLICT DO NOTHING` so a partial earlier
    ///   run, an operator-written marker at the same seq, or a racing
    ///   writer all collapse to a single row at the database. This
    ///   gives the replay viewer a clean terminator instead of a
    ///   trailing chunk with no end marker (see
    ///   `docs/terminal-recording.md` Section 9.3). Sessions with no
    ///   chunks get no marker. The marker insert is committed in the
    ///   same transaction as the status transition + `session_events`
    ///   row, so a partial reconciliation that closes a row without
    ///   the matching marker is not possible.
    /// - Does NOT write `audit_events`. Reconciliation is operational
    ///   bookkeeping and matches the existing close-path audit shape
    ///   (lifecycle close writes a `session_events` row only). The new
    ///   recording-marker write follows the same rule.
    /// - Does NOT delete any row.
    /// - Returns the reconciled (id, previous-status) pairs in a
    ///   stable per-row shape so the caller (the backend startup
    ///   path) can log the count without reaching the wire payload.
    ///
    /// Implementations MUST keep the SQL boundary tight: no terminal
    /// output, no `client_info`, no peer banners, no recording bytes
    /// (including chunk `payload`) can appear in any returned error or
    /// constructed payload. The closed-marker `payload` is built
    /// field-by-field from public metadata only, mirroring the
    /// `session_events` payload.
    async fn reconcile_orphaned_on_startup(
        &self,
        at: DateTime<Utc>,
    ) -> Result<Vec<ReconciledTerminalSession>, RepositoryError>;
}

#[async_trait]
pub trait SessionEventRepository: Send + Sync {
    async fn create(&self, input: CreateSessionEvent) -> Result<SessionEvent, RepositoryError>;
    async fn list_for_session(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Vec<SessionEvent>, RepositoryError>;
    async fn get(&self, id: SessionEventId) -> Result<Option<SessionEvent>, RepositoryError>;
}

#[async_trait]
pub trait AuditEventRepository: Send + Sync {
    async fn create(&self, input: CreateAuditEvent) -> Result<AuditEvent, RepositoryError>;
    async fn recent(&self, limit: u32) -> Result<Vec<AuditEvent>, RepositoryError>;
    /// Most-recent-first audit events scoped to a single actor.
    ///
    /// The current-user audit read route uses this so a request from
    /// user A never observes user B's events. Rows with `actor_id IS
    /// NULL` (pre-auth events, e.g. failed login attempts where the
    /// actor isn't known) are NOT returned by this method; an admin
    /// surface that wants those uses [`Self::recent`] directly.
    async fn recent_for_actor(
        &self,
        actor_id: UserId,
        limit: u32,
    ) -> Result<Vec<AuditEvent>, RepositoryError>;
    async fn get(&self, id: AuditEventId) -> Result<Option<AuditEvent>, RepositoryError>;
}

/// Password credential persistence.
///
/// One row per user with a password set. The hash is an Argon2id PHC
/// string produced by the auth service; this layer is opaque to the
/// hashing scheme. Plaintext passwords are never represented here.
///
/// Inputs and outputs both redact the hash via `Debug` so a stray
/// `tracing::debug!(?credential, ?input)` cannot leak the bytes.
#[async_trait]
pub trait PasswordCredentialRepository: Send + Sync {
    /// Insert or replace the password row for a user.
    ///
    /// Behavior:
    /// - First call for a user inserts the row with
    ///   `created_at = updated_at = password_changed_at = NOW()`.
    /// - Subsequent calls overwrite `password_hash`, bump
    ///   `updated_at = password_changed_at = NOW()`, and leave
    ///   `created_at` unchanged.
    /// - Returns the post-write [`PasswordCredential`].
    /// - A foreign-key failure (no matching `users.id`) is mapped to
    ///   [`RepositoryError::Database`] via the `users` constraint name —
    ///   the auth service is expected to ensure the user row exists
    ///   before calling this.
    async fn upsert_for_user(
        &self,
        input: CreatePasswordCredential,
    ) -> Result<PasswordCredential, RepositoryError>;

    /// Fetch the password row for a user, or `None` if the user has no
    /// password set yet.
    async fn get_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Option<PasswordCredential>, RepositoryError>;

    /// Cheap "is anyone already bootstrapped?" probe for the auth
    /// bootstrap route.
    ///
    /// Returns `true` iff at least one row exists in `user_passwords`.
    /// The bootstrap route uses this to decide whether the first-user
    /// path is already closed (a `users` row with no password is the
    /// dev fixture and MUST NOT block bootstrap, per SPEC.md "User
    /// model and first-user bootstrap"). Implementations SHOULD scan at
    /// most one row (`SELECT 1 ... LIMIT 1`); this is hot-path-cheap.
    async fn any_exists(&self) -> Result<bool, RepositoryError>;
}

/// Browser session persistence.
///
/// Plaintext cookie tokens never reach this layer. The auth service
/// generates the token, SHA-256-hashes it, and passes only the digest
/// here. Lookup is by digest. The repository does NOT filter on
/// `revoked_at` / `expires_at` — those are returned on the row and the
/// service decides whether to honor them. Keeping the SQL trivial and
/// pushing the policy to one place (the auth service) avoids two
/// sources of truth drifting.
#[async_trait]
pub trait UserSessionRepository: Send + Sync {
    /// Insert a fresh session row.
    ///
    /// A duplicate `token_hash` (astronomically unlikely with 32 random
    /// bytes, but possible if a caller mishashes) is mapped to
    /// [`RepositoryError::Conflict`] with constraint name
    /// `"user_sessions_token_hash_key"` so the route can surface a safe
    /// generic 500 without echoing any digest material.
    async fn create(&self, input: CreateUserSession) -> Result<UserSession, RepositoryError>;

    /// Look up a session by the SHA-256 digest of its cookie token.
    ///
    /// Returns the row regardless of `revoked_at` / `expires_at` — the
    /// auth extractor is the single place that validates those fields.
    /// `None` is returned for an unknown digest.
    async fn get_by_token_hash(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<UserSession>, RepositoryError>;

    /// Look up a session by its primary key.
    ///
    /// Used by audit / management surfaces that already know the row's
    /// `id`. NOT used by the auth extractor — the extractor goes through
    /// [`Self::get_by_token_hash`] only.
    async fn get(&self, id: UserSessionId) -> Result<Option<UserSession>, RepositoryError>;

    /// Stamp `last_seen_at` on an existing session.
    ///
    /// Best-effort by the auth extractor — a failure is logged at
    /// `warn!` and does NOT fail the request. The extractor MAY skip
    /// the call entirely on hot paths if the row's `last_seen_at` is
    /// already within a small window. Returns
    /// [`RepositoryError::NotFound`] for an unknown id so callers can
    /// distinguish "row was deleted under us" from a write failure.
    async fn touch_last_seen(
        &self,
        id: UserSessionId,
        at: DateTime<Utc>,
    ) -> Result<(), RepositoryError>;

    /// Stamp `revoked_at` and an optional short reason on an existing
    /// session. Idempotent: a second call against an already-revoked
    /// row is a no-op and returns `Ok(())` — the original `revoked_at`
    /// and `revoked_reason` are preserved so the audit trail stays
    /// honest. Returns [`RepositoryError::NotFound`] for an unknown id.
    async fn revoke(
        &self,
        id: UserSessionId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<(), RepositoryError>;

    /// Stamp `revoked_at` on every non-revoked session for a user.
    ///
    /// Idempotent across already-revoked rows (those rows are skipped).
    /// Returns the number of rows transitioned from non-revoked to
    /// revoked so the caller can decide whether to write any audit
    /// events. An unknown `user_id` simply returns `0`.
    async fn revoke_all_for_user(
        &self,
        user_id: UserId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<u64, RepositoryError>;

    /// List every session row owned by `user_id`, newest first by
    /// `created_at`. Includes revoked AND expired rows — the route
    /// layer decides which states to surface and how to label them so
    /// the user-facing UI can show "active / revoked / expired" without
    /// the repository encoding presentation policy. An unknown
    /// `user_id` simply returns an empty Vec.
    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<UserSession>, RepositoryError>;

    /// Revoke a single session by primary key, scoped to `user_id` in
    /// SQL.
    ///
    /// The `(id, user_id)` filter makes ownership a database-level
    /// guarantee, not a route-level one — a route that forgot to
    /// re-check the owner cannot leak a cross-user revoke. A row
    /// addressed by id but owned by a different user, OR a row that
    /// does not exist at all, both surface as
    /// [`RepositoryError::NotFound`] so a probe cannot distinguish the
    /// two cases through the wire response.
    ///
    /// Idempotent: a second call against an already-revoked row owned
    /// by `user_id` is a no-op (Ok(`false`)) and the original
    /// `revoked_at` / `revoked_reason` are preserved. Returns
    /// `Ok(true)` when the row transitioned from non-revoked to
    /// revoked, so the caller can decide whether to write an audit
    /// event.
    async fn revoke_for_user(
        &self,
        user_id: UserId,
        session_id: UserSessionId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<bool, RepositoryError>;

    /// Stamp `revoked_at` on every non-revoked session for a user
    /// EXCEPT the one identified by `except_id`.
    ///
    /// `except_id` is typically the caller's current session — this is
    /// the "log out everywhere else" surface. The except row is
    /// untouched whether it is revoked or not; only OTHER rows for the
    /// user can transition. Returns the count of rows transitioned
    /// from non-revoked to revoked. An unknown `user_id` returns `0`.
    async fn revoke_all_except(
        &self,
        user_id: UserId,
        except_id: UserSessionId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<u64, RepositoryError>;
}

/// Durable terminal-recording persistence.
///
/// Methods are session-scoped, NOT owner-scoped. Owner-scoping happens at
/// the API layer (an `AuthenticatedUser` route resolves the
/// `terminal_sessions.owner_id` BEFORE calling any method here). This
/// matches the existing pattern for `SessionEventRepository` /
/// `TerminalSessionRepository::list_attachments` — the repository takes a
/// session id, the route is responsible for proving the caller owns it.
///
/// Privacy contract:
/// - Implementations MUST NOT echo chunk `payload` bytes back through any
///   error path, log line, panic, or `Debug` impl.
/// - Marker `payload` is metadata-only by contract; implementations do
///   not inspect / sanitise it. Construction discipline lives at the
///   writer layer.
/// - `limit` is bounded by the caller. Implementations MAY clamp at a
///   sane upper bound; this slice ships with a fixed 1024 ceiling
///   enforced inside the Postgres impl so no caller can accidentally
///   pull a whole session's worth of chunks in one query. Bound is
///   defence-in-depth; the API layer adds its own pagination cap.
#[async_trait]
pub trait TerminalRecordingRepository: Send + Sync {
    /// Insert one chunk row.
    ///
    /// Maps schema constraint failures into typed errors:
    /// - duplicate `(terminal_session_id, seq_start)` →
    ///   [`RepositoryError::Conflict`] with constraint
    ///   `terminal_recording_chunks_session_seq_start_uq`.
    /// - missing `terminal_sessions(id)` (FK violation) →
    ///   [`RepositoryError::Database`] (no such session — the writer
    ///   above is expected to ensure the row exists).
    /// - any CHECK violation (seq, byte_len, payload-length, encryption,
    ///   compression) → [`RepositoryError::Database`] tagged with the
    ///   constraint name. The error MUST NOT echo `payload` bytes.
    async fn append_chunk(
        &self,
        input: CreateTerminalRecordingChunk,
    ) -> Result<TerminalRecordingChunk, RepositoryError>;

    /// Insert one marker row. Constraint mapping mirrors
    /// [`Self::append_chunk`].
    async fn append_marker(
        &self,
        input: CreateTerminalRecordingMarker,
    ) -> Result<TerminalRecordingMarker, RepositoryError>;

    /// List chunks for a session ordered by `seq_start ASC`, starting at
    /// `from_seq` (inclusive against `seq_start`) and capped at `limit`
    /// rows. Returns an empty Vec for an unknown session id (a foreign
    /// session is the route layer's concern).
    ///
    /// `limit` is clamped to the implementation's configured ceiling
    /// (currently 1024).
    async fn list_chunks(
        &self,
        terminal_session_id: TerminalSessionId,
        from_seq: i64,
        limit: u32,
    ) -> Result<Vec<TerminalRecordingChunk>, RepositoryError>;

    /// List markers for a session ordered by `(seq ASC, created_at ASC)`,
    /// starting at `from_seq` (inclusive) and capped at `limit` rows.
    /// Same clamping rules as [`Self::list_chunks`].
    async fn list_markers(
        &self,
        terminal_session_id: TerminalSessionId,
        from_seq: i64,
        limit: u32,
    ) -> Result<Vec<TerminalRecordingMarker>, RepositoryError>;

    /// Aggregate read-side metadata for a session's recording (counts
    /// and seq/time bounds across chunks AND markers). Returns
    /// [`TerminalRecordingMetadata::empty`] when the session has no
    /// chunks AND no markers — never errors with `NotFound`. Unknown
    /// session ids also return the empty shape; the caller is
    /// responsible for proving the session exists / is owner-scoped
    /// BEFORE calling this method.
    ///
    /// Implementations MUST NOT echo chunk payload bytes through any
    /// error path; the metadata aggregate touches `BYTEA` rows but
    /// never reads `payload`.
    async fn get_metadata(
        &self,
        terminal_session_id: TerminalSessionId,
    ) -> Result<TerminalRecordingMetadata, RepositoryError>;
}

#[cfg(test)]
mod recording_input_tests {
    use super::*;
    use crate::ids::TerminalSessionId;
    use crate::terminal_recording::{
        TerminalRecordingCompression, TerminalRecordingMarkerKind,
        TerminalRecordingPayloadEncryption,
    };

    const SENTINEL: &[u8] = b"CREATE-CHUNK-SENTINEL-7E";

    #[test]
    fn create_chunk_input_redacts_payload_in_debug() {
        let input = CreateTerminalRecordingChunk {
            terminal_session_id: TerminalSessionId::new(),
            seq_start: 1,
            seq_end: 1,
            byte_len: SENTINEL.len() as i32,
            payload: SENTINEL.to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        };
        let dbg = format!("{input:?}");
        assert!(
            !dbg.contains("CREATE-CHUNK-SENTINEL-7E"),
            "payload sentinel leaked into CreateTerminalRecordingChunk Debug: {dbg}",
        );
        assert!(
            dbg.contains("redacted"),
            "Debug output should mention redaction: {dbg}",
        );
    }

    #[test]
    fn create_marker_input_debug_renders_metadata() {
        // Markers are metadata-only by contract — Debug should faithfully
        // reflect the JSON the caller supplied (no redaction needed).
        let input = CreateTerminalRecordingMarker {
            terminal_session_id: TerminalSessionId::new(),
            kind: TerminalRecordingMarkerKind::Resized,
            seq: 7,
            payload: serde_json::json!({ "cols": 132, "rows": 40 }),
        };
        let dbg = format!("{input:?}");
        assert!(dbg.contains("Resized"));
        assert!(dbg.contains("132"));
        assert!(dbg.contains("40"));
    }
}
