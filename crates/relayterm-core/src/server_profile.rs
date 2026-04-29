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
}
