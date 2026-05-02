//! Typed UUID newtypes for domain entities.
//!
//! Each ID wraps a `Uuid` so the type system can distinguish, for example, a
//! `UserId` from a `HostId` even though both are 128-bit values.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

define_id!(
    /// Identifies a [`User`](crate::user::User).
    UserId
);
define_id!(
    /// Identifies a [`Host`](crate::host::Host) — a reachable SSH endpoint.
    HostId
);
define_id!(
    /// Identifies an [`SshIdentity`](crate::ssh_identity::SshIdentity) — a credential record.
    SshIdentityId
);
define_id!(
    /// Identifies a [`ServerProfile`](crate::server_profile::ServerProfile) — a host + identity binding.
    ServerProfileId
);
define_id!(
    /// Identifies a [`KnownHostEntry`](crate::known_host::KnownHostEntry).
    KnownHostEntryId
);
define_id!(
    /// Identifies a [`TerminalSession`](crate::terminal_session::TerminalSession).
    TerminalSessionId
);
define_id!(
    /// Identifies a [`TerminalSessionAttachment`](crate::terminal_session::TerminalSessionAttachment).
    TerminalSessionAttachmentId
);
define_id!(
    /// Identifies a [`SessionEvent`](crate::session_event::SessionEvent).
    SessionEventId
);
define_id!(
    /// Identifies an [`AuditEvent`](crate::audit_event::AuditEvent).
    AuditEventId
);
define_id!(
    /// Identifies a [`UserSession`](crate::user_session::UserSession) — one
    /// issued browser session row. NOT the cookie token; the cookie value
    /// is a separate 32-byte random secret hashed into `token_hash`.
    UserSessionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_ids_round_trip_through_uuid() {
        let raw = Uuid::new_v4();
        let id = UserId::from_uuid(raw);
        assert_eq!(*id.as_uuid(), raw);
        assert_eq!(id.into_uuid(), raw);
    }

    #[test]
    fn typed_ids_distinguish_kinds() {
        let raw = Uuid::new_v4();
        let user: UserId = raw.into();
        let host: HostId = raw.into();
        // Different types — assignment between them must fail to compile.
        assert_eq!(user.as_uuid(), host.as_uuid());
    }
}
