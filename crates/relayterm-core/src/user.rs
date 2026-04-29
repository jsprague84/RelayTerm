//! User account record.
//!
//! Auth wiring (passkeys, password hashes, sessions) is intentionally NOT
//! modeled here yet. This is the minimum identity record other domain
//! entities reference as their owner.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::UserId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    /// Stable login email. The auth layer is responsible for normalization.
    pub email: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}
