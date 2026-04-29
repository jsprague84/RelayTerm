use chrono::{DateTime, Utc};
use relayterm_core::host::Host;
use relayterm_core::ids::HostId;
use relayterm_core::repository::CreateHost;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_ssh_port, validate_ssh_username,
};
use serde::{Deserialize, Serialize};

use crate::dev_user::DevUser;
use crate::error::ApiError;

/// Default SSH port applied when the request omits `port`.
const DEFAULT_PORT: u32 = 22;

#[derive(Debug, Deserialize)]
pub(crate) struct CreateHostRequest {
    pub display_name: String,
    pub hostname: String,
    /// Optional; defaults to 22 when absent.
    pub port: Option<u32>,
    pub default_username: String,
}

impl CreateHostRequest {
    /// Validate and convert into the repository-level input.
    pub(crate) fn into_create(self, owner: DevUser) -> Result<CreateHost, ApiError> {
        let display_name = validate_host_display_name(&self.display_name)?;
        let hostname = validate_hostname(&self.hostname)?;
        let port = validate_ssh_port(self.port.unwrap_or(DEFAULT_PORT))?;
        let default_username = validate_ssh_username(&self.default_username)?;
        Ok(CreateHost {
            owner_id: owner.0,
            display_name,
            hostname,
            port,
            default_username,
        })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct HostResponse {
    pub id: HostId,
    pub display_name: String,
    pub hostname: String,
    pub port: u16,
    pub default_username: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Host> for HostResponse {
    fn from(h: Host) -> Self {
        Self {
            id: h.id,
            display_name: h.display_name.into_string(),
            hostname: h.hostname.into_string(),
            port: h.port.get(),
            default_username: h.default_username.into_string(),
            created_at: h.created_at,
            updated_at: h.updated_at,
        }
    }
}
