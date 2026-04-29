use chrono::{DateTime, Utc};
use relayterm_core::ids::{ServerProfileId, TerminalSessionId};
use relayterm_core::terminal_session::{TerminalSession, TerminalSessionStatus};
use serde::{Deserialize, Serialize};

/// Default PTY dimensions for `POST /api/v1/terminal-sessions` when the
/// client doesn't supply them. 80x24 matches the historical xterm default;
/// the client is expected to resize after the renderer mounts.
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

#[derive(Debug, Deserialize)]
pub(crate) struct CreateTerminalSessionRequest {
    pub server_profile_id: ServerProfileId,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
}

const fn default_cols() -> u16 {
    DEFAULT_COLS
}

const fn default_rows() -> u16 {
    DEFAULT_ROWS
}

#[derive(Debug, Serialize)]
pub(crate) struct TerminalSessionResponse {
    pub id: TerminalSessionId,
    pub server_profile_id: ServerProfileId,
    pub status: TerminalSessionStatus,
    pub cols: u16,
    pub rows: u16,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
}

impl From<TerminalSession> for TerminalSessionResponse {
    fn from(s: TerminalSession) -> Self {
        Self {
            id: s.id,
            server_profile_id: s.server_profile_id,
            status: s.status,
            cols: s.cols,
            rows: s.rows,
            created_at: s.created_at,
            last_seen_at: s.last_seen_at,
            closed_at: s.closed_at,
        }
    }
}

/// Wrapper around [`TerminalSessionResponse`] for routes that need to
/// surface the stub-PTY message inline. `flatten` keeps the wire shape
/// flat so clients see one object, not a wrapper.
#[derive(Debug, Serialize)]
pub(crate) struct CreateTerminalSessionResponse {
    #[serde(flatten)]
    pub session: TerminalSessionResponse,
    pub message: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct CloseTerminalSessionResponse {
    #[serde(flatten)]
    pub session: TerminalSessionResponse,
    pub already_closed: bool,
}
