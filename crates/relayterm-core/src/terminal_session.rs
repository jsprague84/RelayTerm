//! Long-lived terminal session metadata.
//!
//! A `TerminalSession` is the *metadata* row that describes a session — the
//! actual `russh::Channel`, replay ring buffer, and PTY state are owned by
//! the backend orchestrator at runtime and are NOT persisted here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ServerProfileId, TerminalSessionAttachmentId, TerminalSessionId, UserId};

/// Lifecycle status of a [`TerminalSession`].
///
/// Transitions:
/// - `Active` → `Detached` when the last attached client drops.
/// - `Detached` → `Active` when a client reattaches.
/// - either → `Closed` on inactivity timeout, explicit close, or a hard error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSessionStatus {
    Active,
    Detached,
    Closed,
}

impl TerminalSessionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Detached => "detached",
            Self::Closed => "closed",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        Some(match value {
            "active" => Self::Active,
            "detached" => Self::Detached,
            "closed" => Self::Closed,
            _ => return None,
        })
    }
}

/// Persisted metadata for a long-lived SSH session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSession {
    pub id: TerminalSessionId,
    pub owner_id: UserId,
    pub server_profile_id: ServerProfileId,
    pub status: TerminalSessionStatus,
    /// Last PTY size requested by an attached client. Live PTY size is
    /// owned by the orchestrator; this column is a hint for resume.
    pub cols: u16,
    pub rows: u16,
    pub created_at: DateTime<Utc>,
    /// Most recent activity from any attached client.
    pub last_seen_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

/// One client's attachment to a [`TerminalSession`].
///
/// The session may have multiple historical attachments (detach +
/// reattach), and at runtime may have at most one currently-active
/// attachment (single-client v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionAttachment {
    pub id: TerminalSessionAttachmentId,
    pub session_id: TerminalSessionId,
    pub attached_at: DateTime<Utc>,
    pub detached_at: Option<DateTime<Utc>>,
    /// Free-form client info (`User-Agent`, Tauri build, etc.) for audit.
    pub client_info: Option<String>,
    /// Source IP at attachment time. Not used for auth, recorded for audit.
    pub remote_addr: Option<String>,
    /// Last sequence number this attachment acknowledged before detaching.
    pub last_seen_seq: Option<i64>,
}
