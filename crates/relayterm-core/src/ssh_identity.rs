//! SSH credential record.
//!
//! An identity is the *credential*: a keypair plus algorithm metadata. It is
//! intentionally NOT bound to a host — a single key may be reused across
//! many hosts, with the host binding done via
//! [`ServerProfile`](crate::server_profile::ServerProfile).
//!
//! Encryption of the private key material is out of scope for this module —
//! the field type carries the encrypted bytes, but no encryption is
//! performed here yet.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{SshIdentityId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshKeyType {
    Ed25519,
    Rsa,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
}

impl SshKeyType {
    /// Canonical lowercase tag used in DB rows and protocol messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ed25519 => "ed25519",
            Self::Rsa => "rsa",
            Self::EcdsaP256 => "ecdsa_p256",
            Self::EcdsaP384 => "ecdsa_p384",
            Self::EcdsaP521 => "ecdsa_p521",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        Some(match value {
            "ed25519" => Self::Ed25519,
            "rsa" => Self::Rsa,
            "ecdsa_p256" => Self::EcdsaP256,
            "ecdsa_p384" => Self::EcdsaP384,
            "ecdsa_p521" => Self::EcdsaP521,
            _ => return None,
        })
    }
}

/// An SSH credential record managed by the backend's vault.
///
/// `Debug` is implemented manually so [`Self::encrypted_private_key`]
/// never leaks into tracing logs, panic messages, or any other formatter
/// output. The bytes are reachable only through the field itself.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshIdentity {
    pub id: SshIdentityId,
    pub owner_id: UserId,
    /// Human-friendly label shown in the UI.
    pub name: String,
    pub key_type: SshKeyType,
    /// OpenSSH-format public key bytes — safe to expose.
    pub public_key: Vec<u8>,
    /// Encrypted private key bytes. The encryption scheme is owned by the
    /// vault crate; this module treats the field as opaque ciphertext and
    /// the manual `Debug` impl redacts it.
    pub encrypted_private_key: Vec<u8>,
    /// SHA-256 fingerprint of the public key, hex-encoded.
    pub fingerprint_sha256: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for SshIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshIdentity")
            .field("id", &self.id)
            .field("owner_id", &self.owner_id)
            .field("name", &self.name)
            .field("key_type", &self.key_type)
            .field("public_key_len", &self.public_key.len())
            .field(
                "encrypted_private_key",
                &format_args!("<redacted: {} bytes>", self.encrypted_private_key.len()),
            )
            .field("fingerprint_sha256", &self.fingerprint_sha256)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .finish()
    }
}
