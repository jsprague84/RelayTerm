//! Auth-check DTO.
//!
//! `POST /api/v1/server-profiles/:id/auth-check` returns this shape on
//! 200. The status field is a typed enum — auth failure and host-key
//! mismatch are NOT HTTP errors at this surface, they are diagnostic
//! outcomes the operator-facing UI surfaces directly. HTTP errors are
//! reserved for "the request couldn't be processed" cases (missing
//! profile, vault disabled, internal bug).
//!
//! **Wire-contract note**: the response carries ONLY public, non-secret
//! diagnostic data. No host key, fingerprint, peer banner, decrypted PEM,
//! encrypted blob, vault internal, or russh error text leaks here. The
//! `message` is a static, status-keyed string set by the handler.

use chrono::{DateTime, Utc};
use relayterm_core::ids::{HostId, ServerProfileId, SshIdentityId};
use relayterm_ssh::SshAuthCheckStatus;
use serde::Serialize;

/// Wire enum for [`SshAuthCheckStatus`]. Snake-case tags are part of the
/// contract — clients depend on them.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthCheckStatusWire {
    AuthenticationSucceeded,
    AuthenticationFailed,
    HostKeyUnknown,
    HostKeyChanged,
    ConnectionFailed,
}

impl From<SshAuthCheckStatus> for AuthCheckStatusWire {
    fn from(s: SshAuthCheckStatus) -> Self {
        match s {
            SshAuthCheckStatus::AuthenticationSucceeded => Self::AuthenticationSucceeded,
            SshAuthCheckStatus::AuthenticationFailed => Self::AuthenticationFailed,
            SshAuthCheckStatus::HostKeyUnknown => Self::HostKeyUnknown,
            SshAuthCheckStatus::HostKeyChanged => Self::HostKeyChanged,
            SshAuthCheckStatus::ConnectionFailed => Self::ConnectionFailed,
        }
    }
}

/// Successful response from `POST /server-profiles/:id/auth-check`.
#[derive(Debug, Serialize)]
pub(crate) struct AuthCheckResponse {
    pub profile_id: ServerProfileId,
    pub host_id: HostId,
    pub ssh_identity_id: SshIdentityId,
    pub status: AuthCheckStatusWire,
    /// Short, user-facing message keyed off `status`. Static per status —
    /// no operator detail leaks here. Phrasing is deliberate: the
    /// messages name only what the auth-check proves, and never imply
    /// PTY allocation, command execution, or session readiness.
    pub message: &'static str,
    pub checked_at: DateTime<Utc>,
}

impl AuthCheckResponse {
    /// Static message per status. The shape and exact wording are part of
    /// the wire contract — guarded by an integration test against the
    /// "no overclaim" rule.
    pub(crate) fn message_for(status: AuthCheckStatusWire) -> &'static str {
        match status {
            AuthCheckStatusWire::AuthenticationSucceeded => {
                "ssh public-key authentication succeeded; \
                 no PTY was allocated and no command was executed"
            }
            AuthCheckStatusWire::AuthenticationFailed => {
                "ssh public-key authentication was rejected by the server"
            }
            AuthCheckStatusWire::HostKeyUnknown => {
                "host key is not pinned and trusted; \
                 trust the host key first via the trust-host-key endpoint"
            }
            AuthCheckStatusWire::HostKeyChanged => {
                "host key differs from the pinned entry; \
                 refusing to authenticate against an unverified peer"
            }
            AuthCheckStatusWire::ConnectionFailed => {
                "ssh transport failed before authentication could complete"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_check_status_wire_uses_snake_case_tags() {
        let succeeded = serde_json::to_value(AuthCheckStatusWire::AuthenticationSucceeded).unwrap();
        let failed = serde_json::to_value(AuthCheckStatusWire::AuthenticationFailed).unwrap();
        let unknown = serde_json::to_value(AuthCheckStatusWire::HostKeyUnknown).unwrap();
        let changed = serde_json::to_value(AuthCheckStatusWire::HostKeyChanged).unwrap();
        let conn = serde_json::to_value(AuthCheckStatusWire::ConnectionFailed).unwrap();
        assert_eq!(
            succeeded,
            serde_json::Value::String("authentication_succeeded".into())
        );
        assert_eq!(
            failed,
            serde_json::Value::String("authentication_failed".into())
        );
        assert_eq!(
            unknown,
            serde_json::Value::String("host_key_unknown".into())
        );
        assert_eq!(
            changed,
            serde_json::Value::String("host_key_changed".into())
        );
        assert_eq!(conn, serde_json::Value::String("connection_failed".into()));
    }

    #[test]
    fn message_does_not_overclaim_session_or_command_execution() {
        // The success message must NOT imply that a PTY was allocated, a
        // shell was spawned, or a command ran. Pin the wording so an
        // accidental rewording trips the test.
        let msg = AuthCheckResponse::message_for(AuthCheckStatusWire::AuthenticationSucceeded);
        let lower = msg.to_lowercase();
        assert!(
            lower.contains("no pty") && lower.contains("no command"),
            "success message must explicitly disclaim PTY/command, got: {msg}"
        );
        assert!(
            !lower.contains("session opened") && !lower.contains("shell"),
            "success message must not imply a shell or session: {msg}"
        );
    }
}
