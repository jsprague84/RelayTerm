//! Known-hosts pinning.
//!
//! A `KnownHostEntry` is the backend's record of "we have seen this host
//! present this public key before, and (optionally) we trust it." All
//! `check_server_key` decisions in the SSH layer must consult this table.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{HostId, KnownHostEntryId};
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
}
