//! Session lifecycle events.
//!
//! These are append-only rows describing what happened to a
//! [`TerminalSession`](crate::terminal_session::TerminalSession). They are
//! NOT the per-output replay events — those live only in the orchestrator's
//! in-memory ring buffer and never touch Postgres.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{SessionEventId, TerminalSessionId};

/// Categorical kind of session event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    Created,
    Attached,
    Detached,
    Reattached,
    Resized,
    ReplayStarted,
    ReplayCompleted,
    Closed,
}

impl SessionEventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Attached => "attached",
            Self::Detached => "detached",
            Self::Reattached => "reattached",
            Self::Resized => "resized",
            Self::ReplayStarted => "replay_started",
            Self::ReplayCompleted => "replay_completed",
            Self::Closed => "closed",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        Some(match value {
            "created" => Self::Created,
            "attached" => Self::Attached,
            "detached" => Self::Detached,
            "reattached" => Self::Reattached,
            "resized" => Self::Resized,
            "replay_started" => Self::ReplayStarted,
            "replay_completed" => Self::ReplayCompleted,
            "closed" => Self::Closed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: SessionEventId,
    pub session_id: TerminalSessionId,
    pub kind: SessionEventKind,
    /// Free-form details (resize dimensions, replay range, error message).
    /// Stored as JSON so the schema can evolve without a migration.
    pub payload: serde_json::Value,
    pub recorded_at: DateTime<Utc>,
}
