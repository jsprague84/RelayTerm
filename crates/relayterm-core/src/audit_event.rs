//! Security-relevant audit log entries.
//!
//! Audit events record actions that may matter for forensics: auth,
//! credential vault access, host-key mismatch, session takeover, profile
//! mutations, etc. They are append-only and intentionally distinct from
//! [`SessionEvent`](crate::session_event::SessionEvent), which is per-session
//! lifecycle telemetry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AuditEventId, UserId};

/// Categorical kind of audit event. The set is open by design — new
/// security-sensitive surfaces should add a variant rather than reusing
/// [`AuditEventKind::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventKind {
    LoginSucceeded,
    LoginFailed,
    LogoutSucceeded,
    KeyVaultAccess,
    KeyVaultDecryptFailed,
    HostKeyAccepted,
    HostKeyMismatch,
    HostKeyRevoked,
    ServerProfileCreated,
    ServerProfileUpdated,
    ServerProfileDeleted,
    SshIdentityCreated,
    SshIdentityDeleted,
    SessionOpened,
    SessionClosed,
    Other,
}

impl AuditEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LoginSucceeded => "login_succeeded",
            Self::LoginFailed => "login_failed",
            Self::LogoutSucceeded => "logout_succeeded",
            Self::KeyVaultAccess => "key_vault_access",
            Self::KeyVaultDecryptFailed => "key_vault_decrypt_failed",
            Self::HostKeyAccepted => "host_key_accepted",
            Self::HostKeyMismatch => "host_key_mismatch",
            Self::HostKeyRevoked => "host_key_revoked",
            Self::ServerProfileCreated => "server_profile_created",
            Self::ServerProfileUpdated => "server_profile_updated",
            Self::ServerProfileDeleted => "server_profile_deleted",
            Self::SshIdentityCreated => "ssh_identity_created",
            Self::SshIdentityDeleted => "ssh_identity_deleted",
            Self::SessionOpened => "session_opened",
            Self::SessionClosed => "session_closed",
            Self::Other => "other",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        Some(match value {
            "login_succeeded" => Self::LoginSucceeded,
            "login_failed" => Self::LoginFailed,
            "logout_succeeded" => Self::LogoutSucceeded,
            "key_vault_access" => Self::KeyVaultAccess,
            "key_vault_decrypt_failed" => Self::KeyVaultDecryptFailed,
            "host_key_accepted" => Self::HostKeyAccepted,
            "host_key_mismatch" => Self::HostKeyMismatch,
            "host_key_revoked" => Self::HostKeyRevoked,
            "server_profile_created" => Self::ServerProfileCreated,
            "server_profile_updated" => Self::ServerProfileUpdated,
            "server_profile_deleted" => Self::ServerProfileDeleted,
            "ssh_identity_created" => Self::SshIdentityCreated,
            "ssh_identity_deleted" => Self::SshIdentityDeleted,
            "session_opened" => Self::SessionOpened,
            "session_closed" => Self::SessionClosed,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: AuditEventId,
    /// The user the event is *about*; `None` for pre-auth events such as a
    /// failed login attempt where the actor is not yet known.
    pub actor_id: Option<UserId>,
    pub kind: AuditEventKind,
    /// Free-form details (target ids, error reasons, IP, user-agent).
    pub payload: serde_json::Value,
    pub remote_addr: Option<String>,
    pub recorded_at: DateTime<Utc>,
}
