//! Known-hosts pinning.
//!
//! A `KnownHostEntry` is the backend's record of "we have seen this host
//! present this public key before, and (optionally) we trust it." All
//! `check_server_key` decisions in the SSH layer must consult this table.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{HostId, KnownHostEntryId, UserId};
use crate::ssh_identity::SshKeyType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHostEntry {
    pub id: KnownHostEntryId,
    pub host_id: HostId,
    pub key_type: SshKeyType,
    /// SHA-256 fingerprint of the host key, hex-encoded.
    pub fingerprint_sha256: String,
    /// Raw public key bytes, OpenSSH wire format.
    pub public_key: Vec<u8>,
    pub first_seen_at: DateTime<Utc>,
    pub trusted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// Operator-of-record for the revoke. Set together with
    /// `revoked_at` and `revoked_reason_code`; the schema CHECK
    /// `known_host_entries_revoked_columns_set_together` is the
    /// defence-in-depth backstop against partial revoke writes.
    pub revoked_by: Option<UserId>,
    /// Fixed-enum reason for the revoke. Values are constrained at the
    /// route boundary by [`KnownHostRevocationReason`] and at the schema
    /// boundary by `known_host_entries_revoked_reason_chk`. Free-text
    /// operator notes are deliberately not persisted (see
    /// `docs/spec/host-key-replace.md` § R3).
    pub revoked_reason_code: Option<KnownHostRevocationReason>,
    /// When this row was revoked as part of an explicit replace, the
    /// fresh `known_host_entries.id` that took over as the active pin.
    /// `None` for never-revoked rows AND for revokes that were not part
    /// of a replace (a future admin "revoke without replace" surface).
    pub replaced_by_id: Option<KnownHostEntryId>,
}

/// Operator-supplied reason for revoking an active known-host pin via the
/// host-key replace flow.
///
/// The accept-list is short by design: a stable enum keeps audit-feed UIs
/// rendering predictable labels and removes any chance of free-text
/// operator prose smuggling secrets into `audit_events.payload`. New
/// reasons require a schema migration AND an enum variant — there is no
/// catch-all variant the route layer can fall back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownHostRevocationReason {
    /// The remote host was reinstalled or rebuilt; the new install
    /// generated a fresh host key.
    ServerReinstalled,
    /// The operator deliberately rotated the host key on the remote.
    HostKeyRotated,
    /// A staging / lab target was recreated. The recurring shape that
    /// surfaced this gap on the VPS staging smoke.
    LabTargetRecreated,
    /// Acknowledged "other"; the operator explicitly accepted the
    /// fingerprint change without picking one of the other codes.
    OperatorOther,
}

impl KnownHostRevocationReason {
    /// Canonical lowercase tag persisted in
    /// `known_host_entries.revoked_reason_code` and validated by the
    /// `known_host_entries_revoked_reason_chk` CHECK.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServerReinstalled => "server_reinstalled",
            Self::HostKeyRotated => "host_key_rotated",
            Self::LabTargetRecreated => "lab_target_recreated",
            Self::OperatorOther => "operator_other",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        Some(match value {
            "server_reinstalled" => Self::ServerReinstalled,
            "host_key_rotated" => Self::HostKeyRotated,
            "lab_target_recreated" => Self::LabTargetRecreated,
            "operator_other" => Self::OperatorOther,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::KnownHostRevocationReason;

    /// The wire tags are persisted in `known_host_entries.revoked_reason_code`
    /// and constrained by `known_host_entries_revoked_reason_chk` in the
    /// migrations. Renaming any of them is a schema break.
    #[test]
    fn revocation_reason_tags_are_stable() {
        assert_eq!(
            KnownHostRevocationReason::ServerReinstalled.as_str(),
            "server_reinstalled",
        );
        assert_eq!(
            KnownHostRevocationReason::HostKeyRotated.as_str(),
            "host_key_rotated",
        );
        assert_eq!(
            KnownHostRevocationReason::LabTargetRecreated.as_str(),
            "lab_target_recreated",
        );
        assert_eq!(
            KnownHostRevocationReason::OperatorOther.as_str(),
            "operator_other",
        );
    }

    #[test]
    fn revocation_reason_round_trip_through_str_tag() {
        for reason in [
            KnownHostRevocationReason::ServerReinstalled,
            KnownHostRevocationReason::HostKeyRotated,
            KnownHostRevocationReason::LabTargetRecreated,
            KnownHostRevocationReason::OperatorOther,
        ] {
            let tag = reason.as_str();
            let parsed = KnownHostRevocationReason::from_str_tag(tag);
            assert_eq!(parsed, Some(reason), "tag {tag} did not round-trip");
        }
    }

    #[test]
    fn revocation_reason_rejects_unknown_tag() {
        assert!(KnownHostRevocationReason::from_str_tag("operator_freeform").is_none());
        assert!(KnownHostRevocationReason::from_str_tag("").is_none());
        assert!(KnownHostRevocationReason::from_str_tag("SERVER_REINSTALLED").is_none());
    }
}
