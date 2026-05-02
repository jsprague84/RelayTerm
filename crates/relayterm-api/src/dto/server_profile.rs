use chrono::{DateTime, Utc};
use relayterm_core::ids::{HostId, ServerProfileId, SshIdentityId, UserId};
use relayterm_core::repository::CreateServerProfile;
use relayterm_core::server_profile::ServerProfile;
use relayterm_core::validation::{validate_profile_name, validate_ssh_username, validate_tags};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub(crate) struct CreateServerProfileRequest {
    pub name: String,
    pub host_id: HostId,
    pub ssh_identity_id: SshIdentityId,
    pub username_override: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl CreateServerProfileRequest {
    pub(crate) fn into_create(self, owner_id: UserId) -> Result<CreateServerProfile, ApiError> {
        let name = validate_profile_name(&self.name)?;
        let username_override = self
            .username_override
            .as_deref()
            .map(validate_ssh_username)
            .transpose()?;
        let tag_refs: Vec<&str> = self.tags.iter().map(String::as_str).collect();
        let tags = validate_tags(&tag_refs)?;
        Ok(CreateServerProfile {
            owner_id,
            name,
            host_id: self.host_id,
            ssh_identity_id: self.ssh_identity_id,
            username_override,
            tags,
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ServerProfileResponse {
    pub id: ServerProfileId,
    pub name: String,
    pub host_id: HostId,
    pub ssh_identity_id: SshIdentityId,
    pub username_override: Option<String>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_connected_at: Option<DateTime<Utc>>,
    /// `Some(t)` when the operator has disabled the profile; `None` when
    /// it is currently enabled. Always serialised (as `null` when absent)
    /// so clients can rely on the field's presence.
    pub disabled_at: Option<DateTime<Utc>>,
}

impl From<ServerProfile> for ServerProfileResponse {
    fn from(p: ServerProfile) -> Self {
        Self {
            id: p.id,
            name: p.name.into_string(),
            host_id: p.host_id,
            ssh_identity_id: p.ssh_identity_id,
            username_override: p.username_override.map(|u| u.into_string()),
            tags: p.tags.into_iter().map(|t| t.into_string()).collect(),
            created_at: p.created_at,
            updated_at: p.updated_at,
            last_connected_at: p.last_connected_at,
            disabled_at: p.disabled_at,
        }
    }
}
