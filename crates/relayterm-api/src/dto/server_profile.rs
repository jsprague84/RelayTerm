use chrono::{DateTime, Utc};
use relayterm_core::ids::{HostId, ServerProfileId, SshIdentityId, UserId};
use relayterm_core::repository::{CreateServerProfile, SetOptional, UpdateServerProfile};
use relayterm_core::server_profile::ServerProfile;
use relayterm_core::validation::{validate_profile_name, validate_ssh_username, validate_tags};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ApiError;

/// Distinguish "field absent" from "field explicitly null" during
/// deserialization.
///
/// The default `Option<Option<T>>` deserializer collapses BOTH absent
/// AND `null` to the outer `None`. The PATCH route needs to distinguish:
/// absent means "leave the column alone", `null` means "clear it back
/// to the default". `#[serde(default, deserialize_with =
/// "deserialize_some_present")]` makes a present `null` produce
/// `Some(None)` and a present value produce `Some(Some(value))`, while
/// `#[serde(default)]` keeps an absent field at `None`.
fn deserialize_some_present<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

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

/// Request body for `PATCH /api/v1/server-profiles/:id`.
///
/// Every field is optional. `username_override` uses a double-`Option`
/// to distinguish three caller intents on the wire:
///  - field omitted → "leave the column unchanged"
///  - `null` → "clear the override; fall back to the host default"
///  - a string value → "set the override"
///
/// `serde(default, deserialize_with = ...)` is not needed — `Option<Option<String>>`
/// already deserializes correctly with the omitted-vs-null distinction
/// because serde does not visit the deserializer for absent fields.
#[derive(Debug, Deserialize)]
pub(crate) struct UpdateServerProfileRequest {
    pub name: Option<String>,
    pub host_id: Option<HostId>,
    pub ssh_identity_id: Option<SshIdentityId>,
    /// See struct docs for the omitted-vs-null-vs-string semantics.
    /// The custom `deserialize_some_present` is what lets a JSON `null`
    /// arrive as `Some(None)` instead of being collapsed to outer
    /// `None` by serde's default `Option<Option<T>>` deserializer.
    #[serde(default, deserialize_with = "deserialize_some_present")]
    pub username_override: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
}

impl UpdateServerProfileRequest {
    /// Validate the partial-update body. Returns `Err(Validation
    /// "empty update")` when no field is present so a no-op PATCH is a
    /// caller bug, not a silent no-op. `host_id` / `ssh_identity_id`
    /// ownership is NOT checked here — the route layer re-resolves
    /// them under the caller's `owner_id` before issuing the UPDATE
    /// (collapsing foreign / missing to 404 just like create).
    pub(crate) fn into_update(self) -> Result<UpdateServerProfile, ApiError> {
        let name = self
            .name
            .as_deref()
            .map(validate_profile_name)
            .transpose()?;
        let host_id = self.host_id;
        let ssh_identity_id = self.ssh_identity_id;
        let username_override = match self.username_override {
            None => SetOptional::Unchanged,
            Some(None) => SetOptional::Set(None),
            Some(Some(raw)) => SetOptional::Set(Some(validate_ssh_username(&raw)?)),
        };
        let tags = match self.tags {
            None => None,
            Some(raw) => {
                let tag_refs: Vec<&str> = raw.iter().map(String::as_str).collect();
                Some(validate_tags(&tag_refs)?)
            }
        };

        let empty = name.is_none()
            && host_id.is_none()
            && ssh_identity_id.is_none()
            && matches!(username_override, SetOptional::Unchanged)
            && tags.is_none();
        if empty {
            return Err(ApiError::Validation(
                "at least one field must be provided".to_owned(),
            ));
        }
        Ok(UpdateServerProfile {
            name,
            host_id,
            ssh_identity_id,
            username_override,
            tags,
        })
    }
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
