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
    FirstUserCreated,
    /// One specific browser session was explicitly revoked through the
    /// current-user `POST /api/v1/auth/sessions/:id/revoke` route. Fires
    /// only on a non-revoked → revoked transition; idempotent re-revoke
    /// is a no-op and writes no audit row. The payload carries the
    /// revoked session id and a `current_session: bool` marker.
    SessionRevoked,
    /// `POST /api/v1/auth/sessions/revoke-all-except-current` transitioned
    /// one or more sessions for the caller from non-revoked to revoked.
    /// Fires at most once per call, and only when `revoked_count > 0`.
    /// The payload carries the count — never per-row session ids — so a
    /// future audit search by session id stays scoped to the
    /// `session_revoked` kind.
    SessionsRevoked,
    KeyVaultAccess,
    KeyVaultDecryptFailed,
    HostKeyAccepted,
    HostKeyMismatch,
    HostKeyRevoked,
    ServerProfileCreated,
    ServerProfileUpdated,
    ServerProfileDisabled,
    ServerProfileEnabled,
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
            Self::FirstUserCreated => "first_user_created",
            Self::SessionRevoked => "session_revoked",
            Self::SessionsRevoked => "sessions_revoked",
            Self::KeyVaultAccess => "key_vault_access",
            Self::KeyVaultDecryptFailed => "key_vault_decrypt_failed",
            Self::HostKeyAccepted => "host_key_accepted",
            Self::HostKeyMismatch => "host_key_mismatch",
            Self::HostKeyRevoked => "host_key_revoked",
            Self::ServerProfileCreated => "server_profile_created",
            Self::ServerProfileUpdated => "server_profile_updated",
            Self::ServerProfileDisabled => "server_profile_disabled",
            Self::ServerProfileEnabled => "server_profile_enabled",
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
            "first_user_created" => Self::FirstUserCreated,
            "session_revoked" => Self::SessionRevoked,
            "sessions_revoked" => Self::SessionsRevoked,
            "key_vault_access" => Self::KeyVaultAccess,
            "key_vault_decrypt_failed" => Self::KeyVaultDecryptFailed,
            "host_key_accepted" => Self::HostKeyAccepted,
            "host_key_mismatch" => Self::HostKeyMismatch,
            "host_key_revoked" => Self::HostKeyRevoked,
            "server_profile_created" => Self::ServerProfileCreated,
            "server_profile_updated" => Self::ServerProfileUpdated,
            "server_profile_disabled" => Self::ServerProfileDisabled,
            "server_profile_enabled" => Self::ServerProfileEnabled,
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

#[cfg(test)]
mod tests {
    use super::AuditEventKind;

    /// The wire tags are persisted in `audit_events.kind` and constrained by
    /// the `audit_events_kind_chk` CHECK in the migrations. Renaming any of
    /// them is a schema break that requires a follow-up migration. Pin them.
    #[test]
    fn server_profile_lifecycle_wire_tags_are_stable() {
        assert_eq!(
            AuditEventKind::ServerProfileCreated.as_str(),
            "server_profile_created",
        );
        assert_eq!(
            AuditEventKind::ServerProfileDisabled.as_str(),
            "server_profile_disabled",
        );
        assert_eq!(
            AuditEventKind::ServerProfileEnabled.as_str(),
            "server_profile_enabled",
        );
    }

    #[test]
    fn server_profile_lifecycle_round_trips_through_from_str_tag() {
        for kind in [
            AuditEventKind::ServerProfileCreated,
            AuditEventKind::ServerProfileDisabled,
            AuditEventKind::ServerProfileEnabled,
        ] {
            assert_eq!(AuditEventKind::from_str_tag(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn unknown_tag_remains_none() {
        assert_eq!(
            AuditEventKind::from_str_tag("server_profile_obliterated"),
            None,
        );
    }
}
