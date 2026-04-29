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
    TerminalSessionAttachmentId, TerminalSessionId, UserId,
};
use crate::known_host::KnownHostEntry;
use crate::server_profile::ServerProfile;
use crate::session_event::{SessionEvent, SessionEventKind};
use crate::ssh_identity::{SshIdentity, SshKeyType};
use crate::terminal_session::{TerminalSession, TerminalSessionAttachment, TerminalSessionStatus};
use crate::user::User;
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
    async fn get(&self, id: AuditEventId) -> Result<Option<AuditEvent>, RepositoryError>;
}
