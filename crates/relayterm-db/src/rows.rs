//! Private row structs and `Row → DomainType` conversions.
//!
//! Row structs hold only primitive types so SQLx can derive `FromRow`
//! without leaking back into the domain crate. Each row carries an
//! `into_domain` (or `try_into_domain`) that re-validates anything the
//! database constraints don't fully encode and returns the typed domain
//! record.
//!
//! When a value read from the database fails domain validation that's a
//! data-integrity bug, but we surface it as a generic
//! `RepositoryError::Database` rather than panicking — operators get a log
//! line, callers get a sane error.

use chrono::{DateTime, Utc};
use relayterm_core::audit_event::{AuditEvent, AuditEventKind};
use relayterm_core::host::Host;
use relayterm_core::ids::{
    AuditEventId, HostId, KnownHostEntryId, ServerProfileId, SessionEventId, SshIdentityId,
    TerminalRecordingChunkId, TerminalRecordingMarkerId, TerminalSessionAttachmentId,
    TerminalSessionId, UserId, UserSessionId,
};
use relayterm_core::known_host::{KnownHostEntry, KnownHostRevocationReason};
use relayterm_core::password_credential::PasswordCredential;
use relayterm_core::repository::RepositoryError;
use relayterm_core::server_profile::ServerProfile;
use relayterm_core::session_event::{SessionEvent, SessionEventKind};
use relayterm_core::ssh_identity::{SshIdentity, SshKeyType};
use relayterm_core::terminal_recording::{
    TerminalRecordingChunk, TerminalRecordingCompression, TerminalRecordingMarker,
    TerminalRecordingMarkerKind, TerminalRecordingPayloadEncryption,
};
use relayterm_core::terminal_session::{
    TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use relayterm_core::user::User;
use relayterm_core::user_session::UserSession;
use relayterm_core::validation::{
    HostDisplayName, Hostname, ProfileName, SshPort, SshUsername, Tag,
};
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

fn invalid<T>(field: &'static str, msg: impl Into<String>) -> Result<T, RepositoryError> {
    Err(RepositoryError::Database(format!(
        "row integrity: {field} ({})",
        msg.into()
    )))
}

#[derive(Debug, FromRow)]
pub(crate) struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl UserRow {
    pub(crate) fn into_domain(self) -> User {
        User {
            id: UserId::from_uuid(self.id),
            email: self.email,
            display_name: self.display_name,
            created_at: self.created_at,
            last_login_at: self.last_login_at,
        }
    }
}

/// SQLx row for `user_passwords`.
///
/// `Debug` is intentionally NOT derived on this type — the row carries
/// the password hash and is private to this module. If a future caller
/// needs to log it, the conversion to [`PasswordCredential`] (which has
/// a redacting `Debug`) is the correct intermediate.
#[derive(FromRow)]
pub(crate) struct PasswordCredentialRow {
    pub user_id: Uuid,
    pub password_hash: String,
    pub password_changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PasswordCredentialRow {
    pub(crate) fn into_domain(self) -> PasswordCredential {
        PasswordCredential {
            user_id: UserId::from_uuid(self.user_id),
            password_hash: self.password_hash,
            password_changed_at: self.password_changed_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// SQLx row for `user_sessions`.
///
/// `Debug` is intentionally NOT derived — the row carries the
/// `token_hash` digest. Convert to [`UserSession`] (redacting `Debug`)
/// before any formatter exposure.
#[derive(FromRow)]
pub(crate) struct UserSessionRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_reason: Option<String>,
}

impl UserSessionRow {
    pub(crate) fn into_domain(self) -> UserSession {
        UserSession {
            id: UserSessionId::from_uuid(self.id),
            user_id: UserId::from_uuid(self.user_id),
            token_hash: self.token_hash,
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
            expires_at: self.expires_at,
            revoked_at: self.revoked_at,
            revoked_reason: self.revoked_reason,
        }
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct HostRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub display_name: String,
    pub hostname: String,
    pub port: i32,
    pub default_username: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HostRow {
    pub(crate) fn try_into_domain(self) -> Result<Host, RepositoryError> {
        let port = u16::try_from(self.port).map_err(|_| {
            RepositoryError::Database(format!(
                "row integrity: host.port out of u16 range ({})",
                self.port
            ))
        })?;
        if port == 0 {
            return invalid("host.port", "zero is not a valid SSH port");
        }
        Ok(Host {
            id: HostId::from_uuid(self.id),
            owner_id: UserId::from_uuid(self.owner_id),
            display_name: HostDisplayName::from_validated(self.display_name),
            hostname: Hostname::from_validated(self.hostname),
            port: SshPort::from_validated(port),
            default_username: SshUsername::from_validated(self.default_username),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct SshIdentityRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub key_type: String,
    pub public_key: Vec<u8>,
    pub encrypted_private_key: Vec<u8>,
    pub fingerprint_sha256: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl SshIdentityRow {
    pub(crate) fn try_into_domain(self) -> Result<SshIdentity, RepositoryError> {
        let key_type = SshKeyType::from_str_tag(&self.key_type).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: ssh_identity.key_type unknown ({})",
                self.key_type
            ))
        })?;
        Ok(SshIdentity {
            id: SshIdentityId::from_uuid(self.id),
            owner_id: UserId::from_uuid(self.owner_id),
            name: self.name,
            key_type,
            public_key: self.public_key,
            encrypted_private_key: self.encrypted_private_key,
            fingerprint_sha256: self.fingerprint_sha256,
            created_at: self.created_at,
            last_used_at: self.last_used_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct ServerProfileRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub host_id: Uuid,
    pub ssh_identity_id: Uuid,
    pub username_override: Option<String>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_connected_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl ServerProfileRow {
    pub(crate) fn into_domain(self) -> ServerProfile {
        ServerProfile {
            id: ServerProfileId::from_uuid(self.id),
            owner_id: UserId::from_uuid(self.owner_id),
            name: ProfileName::from_validated(self.name),
            host_id: HostId::from_uuid(self.host_id),
            ssh_identity_id: SshIdentityId::from_uuid(self.ssh_identity_id),
            username_override: self.username_override.map(SshUsername::from_validated),
            tags: self.tags.into_iter().map(Tag::from_validated).collect(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_connected_at: self.last_connected_at,
            disabled_at: self.disabled_at,
        }
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct KnownHostEntryRow {
    pub id: Uuid,
    pub host_id: Uuid,
    pub key_type: String,
    pub fingerprint_sha256: String,
    pub public_key: Vec<u8>,
    pub first_seen_at: DateTime<Utc>,
    pub trusted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<Uuid>,
    pub revoked_reason_code: Option<String>,
    pub replaced_by_id: Option<Uuid>,
}

impl KnownHostEntryRow {
    pub(crate) fn try_into_domain(self) -> Result<KnownHostEntry, RepositoryError> {
        let key_type = SshKeyType::from_str_tag(&self.key_type).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: known_host_entry.key_type unknown ({})",
                self.key_type
            ))
        })?;
        let revoked_reason_code = match self.revoked_reason_code.as_deref() {
            None => None,
            Some(tag) => Some(KnownHostRevocationReason::from_str_tag(tag).ok_or_else(|| {
                RepositoryError::Database(format!(
                    "row integrity: known_host_entry.revoked_reason_code unknown ({tag})",
                ))
            })?),
        };
        Ok(KnownHostEntry {
            id: KnownHostEntryId::from_uuid(self.id),
            host_id: HostId::from_uuid(self.host_id),
            key_type,
            fingerprint_sha256: self.fingerprint_sha256,
            public_key: self.public_key,
            first_seen_at: self.first_seen_at,
            trusted_at: self.trusted_at,
            revoked_at: self.revoked_at,
            revoked_by: self.revoked_by.map(UserId::from_uuid),
            revoked_reason_code,
            replaced_by_id: self.replaced_by_id.map(KnownHostEntryId::from_uuid),
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct TerminalSessionRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub server_profile_id: Uuid,
    pub status: String,
    pub cols: i32,
    pub rows: i32,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

impl TerminalSessionRow {
    pub(crate) fn try_into_domain(self) -> Result<TerminalSession, RepositoryError> {
        let status = TerminalSessionStatus::from_str_tag(&self.status).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: terminal_session.status unknown ({})",
                self.status
            ))
        })?;
        let cols = u16::try_from(self.cols).map_err(|_| {
            RepositoryError::Database(format!(
                "row integrity: terminal_session.cols out of u16 range ({})",
                self.cols
            ))
        })?;
        let rows = u16::try_from(self.rows).map_err(|_| {
            RepositoryError::Database(format!(
                "row integrity: terminal_session.rows out of u16 range ({})",
                self.rows
            ))
        })?;
        Ok(TerminalSession {
            id: TerminalSessionId::from_uuid(self.id),
            owner_id: UserId::from_uuid(self.owner_id),
            server_profile_id: ServerProfileId::from_uuid(self.server_profile_id),
            status,
            cols,
            rows,
            created_at: self.created_at,
            last_seen_at: self.last_seen_at,
            closed_at: self.closed_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct TerminalSessionAttachmentRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub attached_at: DateTime<Utc>,
    pub detached_at: Option<DateTime<Utc>>,
    pub client_info: Option<String>,
    pub remote_addr: Option<String>,
    pub last_seen_seq: Option<i64>,
}

impl TerminalSessionAttachmentRow {
    pub(crate) fn into_domain(self) -> TerminalSessionAttachment {
        TerminalSessionAttachment {
            id: TerminalSessionAttachmentId::from_uuid(self.id),
            session_id: TerminalSessionId::from_uuid(self.session_id),
            attached_at: self.attached_at,
            detached_at: self.detached_at,
            client_info: self.client_info,
            remote_addr: self.remote_addr,
            last_seen_seq: self.last_seen_seq,
        }
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct SessionEventRow {
    pub id: Uuid,
    pub session_id: Uuid,
    pub kind: String,
    pub payload: JsonValue,
    pub recorded_at: DateTime<Utc>,
}

impl SessionEventRow {
    pub(crate) fn try_into_domain(self) -> Result<SessionEvent, RepositoryError> {
        let kind = SessionEventKind::from_str_tag(&self.kind).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: session_event.kind unknown ({})",
                self.kind
            ))
        })?;
        Ok(SessionEvent {
            id: SessionEventId::from_uuid(self.id),
            session_id: TerminalSessionId::from_uuid(self.session_id),
            kind,
            payload: self.payload,
            recorded_at: self.recorded_at,
        })
    }
}

#[derive(Debug, FromRow)]
pub(crate) struct AuditEventRow {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub kind: String,
    pub payload: JsonValue,
    pub remote_addr: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

impl AuditEventRow {
    pub(crate) fn try_into_domain(self) -> Result<AuditEvent, RepositoryError> {
        let kind = AuditEventKind::from_str_tag(&self.kind).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: audit_event.kind unknown ({})",
                self.kind
            ))
        })?;
        Ok(AuditEvent {
            id: AuditEventId::from_uuid(self.id),
            actor_id: self.actor_id.map(UserId::from_uuid),
            kind,
            payload: self.payload,
            remote_addr: self.remote_addr,
            recorded_at: self.recorded_at,
        })
    }
}

/// SQLx row for `terminal_recording_chunks`.
///
/// `Debug` is intentionally NOT derived — the row carries opaque PTY
/// output bytes. Convert to [`TerminalRecordingChunk`] (redacting
/// `Debug`) before any formatter exposure.
#[derive(FromRow)]
pub(crate) struct TerminalRecordingChunkRow {
    pub id: Uuid,
    pub terminal_session_id: Uuid,
    pub seq_start: i64,
    pub seq_end: i64,
    pub byte_len: i32,
    pub payload: Vec<u8>,
    pub encryption: String,
    pub compression: String,
    pub created_at: DateTime<Utc>,
}

impl TerminalRecordingChunkRow {
    pub(crate) fn try_into_domain(self) -> Result<TerminalRecordingChunk, RepositoryError> {
        let encryption = TerminalRecordingPayloadEncryption::from_str_tag(&self.encryption)
            .ok_or_else(|| {
                RepositoryError::Database(format!(
                    "row integrity: terminal_recording_chunk.encryption unknown ({})",
                    self.encryption
                ))
            })?;
        let compression = TerminalRecordingCompression::from_str_tag(&self.compression)
            .ok_or_else(|| {
                RepositoryError::Database(format!(
                    "row integrity: terminal_recording_chunk.compression unknown ({})",
                    self.compression
                ))
            })?;
        Ok(TerminalRecordingChunk {
            id: TerminalRecordingChunkId::from_uuid(self.id),
            terminal_session_id: TerminalSessionId::from_uuid(self.terminal_session_id),
            seq_start: self.seq_start,
            seq_end: self.seq_end,
            byte_len: self.byte_len,
            payload: self.payload,
            encryption,
            compression,
            created_at: self.created_at,
        })
    }
}

/// SQLx row for `terminal_recording_markers`.
///
/// Markers are metadata-only by contract; `Debug` is safe to derive on
/// the row, but we keep symmetry with the chunk row by routing through
/// the typed conversion before any formatter exposure.
#[derive(Debug, FromRow)]
pub(crate) struct TerminalRecordingMarkerRow {
    pub id: Uuid,
    pub terminal_session_id: Uuid,
    pub kind: String,
    pub seq: i64,
    pub payload: JsonValue,
    pub created_at: DateTime<Utc>,
}

impl TerminalRecordingMarkerRow {
    pub(crate) fn try_into_domain(self) -> Result<TerminalRecordingMarker, RepositoryError> {
        let kind = TerminalRecordingMarkerKind::from_str_tag(&self.kind).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: terminal_recording_marker.kind unknown ({})",
                self.kind
            ))
        })?;
        Ok(TerminalRecordingMarker {
            id: TerminalRecordingMarkerId::from_uuid(self.id),
            terminal_session_id: TerminalSessionId::from_uuid(self.terminal_session_id),
            kind,
            seq: self.seq,
            payload: self.payload,
            created_at: self.created_at,
        })
    }
}
