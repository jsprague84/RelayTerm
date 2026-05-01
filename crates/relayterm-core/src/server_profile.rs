//! Server profile — the user-facing binding of a host to an SSH identity.
//!
//! This is the row a user picks from a "connect to..." list. It references
//! both a [`Host`](crate::host::Host) and an
//! [`SshIdentity`](crate::ssh_identity::SshIdentity), and may override the
//! host's default username.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{HostId, ServerProfileId, SshIdentityId, UserId};
use crate::validation::{ProfileName, SshUsername, Tag};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerProfile {
    pub id: ServerProfileId,
    pub owner_id: UserId,
    pub name: ProfileName,
    pub host_id: HostId,
    pub ssh_identity_id: SshIdentityId,
    /// Override for [`Host::default_username`](crate::host::Host::default_username).
    /// `None` means "fall back to the host default."
    pub username_override: Option<SshUsername>,
    pub tags: Vec<Tag>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_connected_at: Option<DateTime<Utc>>,
    /// `Some(t)` when the operator has disabled the profile at time `t`;
    /// `None` means the profile is currently enabled. Disable blocks new
    /// terminal launches and SSH-side setup actions (auth-check, host-key
    /// preflight/trust). See SPEC.md "Inventory lifecycle and destructive-
    /// action policy" for the full contract.
    pub disabled_at: Option<DateTime<Utc>>,
}

impl ServerProfile {
    /// `true` when [`Self::disabled_at`] carries a timestamp. Convenience
    /// for guard sites that don't care WHEN the profile was disabled, only
    /// that it currently is.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }
}
