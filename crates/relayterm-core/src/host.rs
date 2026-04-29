//! Host record — a reachable SSH endpoint.
//!
//! A host is "where to connect." It does NOT carry credentials; that's
//! [`SshIdentity`](crate::ssh_identity::SshIdentity). The binding of a host
//! to an identity (plus user-facing label, default user override, tags) is
//! [`ServerProfile`](crate::server_profile::ServerProfile).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{HostId, UserId};
use crate::validation::{HostDisplayName, Hostname, SshPort, SshUsername};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub id: HostId,
    pub owner_id: UserId,
    pub display_name: HostDisplayName,
    pub hostname: Hostname,
    pub port: SshPort,
    /// Default SSH username when a [`ServerProfile`](crate::server_profile::ServerProfile)
    /// does not override it.
    pub default_username: SshUsername,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
