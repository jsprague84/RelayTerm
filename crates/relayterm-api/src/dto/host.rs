use chrono::{DateTime, Utc};
use relayterm_core::host::Host;
use relayterm_core::ids::{HostId, UserId};
use relayterm_core::repository::{CreateHost, UpdateHost};
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_ssh_port, validate_ssh_username,
};
use serde::{Deserialize, Serialize};

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
    pub(crate) fn into_create(self, owner_id: UserId) -> Result<CreateHost, ApiError> {
        let display_name = validate_host_display_name(&self.display_name)?;
        let hostname = validate_hostname(&self.hostname)?;
        let port = validate_ssh_port(self.port.unwrap_or(DEFAULT_PORT))?;
        let default_username = validate_ssh_username(&self.default_username)?;
        Ok(CreateHost {
            owner_id,
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

/// Request body for `PATCH /api/v1/hosts/:id`.
///
/// Every field is optional — only the fields a client supplies are
/// updated. Omitting all fields is rejected at the route layer with a
/// 400 (`invalid_input { reason: "empty_update" }`) so a PATCH with no
/// effect doesn't bump `updated_at` for nothing.
///
/// Wire shape mirrors [`CreateHostRequest`] for the fields it shares; a
/// future addition (e.g. `tags` if hosts grow them) extends here in
/// lockstep.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateHostRequest {
    pub display_name: Option<String>,
    pub hostname: Option<String>,
    pub port: Option<u32>,
    pub default_username: Option<String>,
}

impl UpdateHostRequest {
    /// Validate the partial-update body. Returns `Err(Validation
    /// "empty update")` when no field is present — the route layer
    /// surfaces that as `400 invalid_input` so a no-op PATCH is a
    /// caller bug, not a silent no-op. Otherwise each supplied field
    /// is validated by the same newtype constructor used for creates.
    pub(crate) fn into_update(self) -> Result<UpdateHost, ApiError> {
        let display_name = self
            .display_name
            .as_deref()
            .map(validate_host_display_name)
            .transpose()?;
        let hostname = self
            .hostname
            .as_deref()
            .map(validate_hostname)
            .transpose()?;
        let port = self.port.map(validate_ssh_port).transpose()?;
        let default_username = self
            .default_username
            .as_deref()
            .map(validate_ssh_username)
            .transpose()?;

        if display_name.is_none()
            && hostname.is_none()
            && port.is_none()
            && default_username.is_none()
        {
            return Err(ApiError::Validation(
                "at least one field must be provided".to_owned(),
            ));
        }
        Ok(UpdateHost {
            display_name,
            hostname,
            port,
            default_username,
        })
    }
}
