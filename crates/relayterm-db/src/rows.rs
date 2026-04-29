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
    TerminalSessionAttachmentId, TerminalSessionId, UserId,
};
use relayterm_core::known_host::KnownHostEntry;
use relayterm_core::repository::RepositoryError;
use relayterm_core::server_profile::ServerProfile;
use relayterm_core::session_event::{SessionEvent, SessionEventKind};
use relayterm_core::ssh_identity::{SshIdentity, SshKeyType};
use relayterm_core::terminal_session::{
    TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use relayterm_core::user::User;
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
}

impl KnownHostEntryRow {
    pub(crate) fn try_into_domain(self) -> Result<KnownHostEntry, RepositoryError> {
        let key_type = SshKeyType::from_str_tag(&self.key_type).ok_or_else(|| {
            RepositoryError::Database(format!(
                "row integrity: known_host_entry.key_type unknown ({})",
                self.key_type
            ))
        })?;
        Ok(KnownHostEntry {
            id: KnownHostEntryId::from_uuid(self.id),
            host_id: HostId::from_uuid(self.host_id),
            key_type,
            fingerprint_sha256: self.fingerprint_sha256,
            public_key: self.public_key,
            first_seen_at: self.first_seen_at,
            trusted_at: self.trusted_at,
            revoked_at: self.revoked_at,
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
