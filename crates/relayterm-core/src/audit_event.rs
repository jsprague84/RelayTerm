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
    /// `POST /api/v1/auth/change-password` succeeded for the caller.
    /// Fires only on a real password rotation — a wrong-current-password
    /// attempt writes no audit row at this kind. The payload carries
    /// `revoked_other_sessions: u64` only (the count of other sessions
    /// revoked as part of the rotation); never the offered current or
    /// new password, never any password hash, never session token bytes
    /// or token-hash bytes, never per-session ids.
    PasswordChanged,
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
    /// The retention cleanup worker purged one session's durable
    /// recording (chunks + markers). System-authored: `actor_id` is
    /// `NULL` because the cleanup worker is not a user. The payload
    /// carries `target_id`, `target_kind = "terminal_session"`,
    /// `chunk_count`, `marker_count`, `bytes_purged`, `retention_days`,
    /// `closed_at`, `purged_at`, and `reason = "retention_expired"` —
    /// public metadata only. NEVER chunk `payload` bytes (or any
    /// base64 form), marker payload contents, `client_info`, hostnames,
    /// peer banners, raw russh / DB error text, vault internals,
    /// session-token bytes, token hashes, password hashes, or bootstrap
    /// tokens (see `docs/terminal-recording.md` Section 12.5 for the
    /// full redaction list and `crates/relayterm-api/tests/api.rs`'s
    /// `AUDIT_FORBIDDEN_SUBSTRINGS` for the sentinel backstop).
    RecordingPurged,
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
            Self::PasswordChanged => "password_changed",
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
            Self::RecordingPurged => "recording_purged",
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
            "password_changed" => Self::PasswordChanged,
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
            "recording_purged" => Self::RecordingPurged,
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

    /// `recording_purged` is the wire tag the retention cleanup worker
    /// writes (`audit_events_kind_chk` migration
    /// `20260503000021_audit_events_recording_purged_kind.sql`).
    /// Renaming it is a schema break; pin both directions.
    #[test]
    fn recording_purged_wire_tag_is_stable() {
        assert_eq!(AuditEventKind::RecordingPurged.as_str(), "recording_purged",);
        assert_eq!(
            AuditEventKind::from_str_tag("recording_purged"),
            Some(AuditEventKind::RecordingPurged),
        );
    }
}
