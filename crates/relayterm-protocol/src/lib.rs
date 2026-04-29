//! Wire protocol shared between the backend and web/Tauri clients.
//!
//! The protocol is JSON-over-WebSocket. Messages defined here are the
//! canonical schema; the web client mirrors these shapes.
//!
//! No transport behavior lives here — only the shape of payloads.

use relayterm_core::{SeqNo, SessionId};
use serde::{Deserialize, Serialize};

/// Messages the client sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Open a new SSH session or resume an existing one.
    Attach {
        session_id: Option<SessionId>,
        /// If resuming, the last sequence number the client received.
        last_seen_seq: Option<SeqNo>,
    },
    /// User keystroke / paste / etc. from the renderer.
    Input { data: String },
    /// Renderer was resized.
    Resize { cols: u16, rows: u16 },
    /// Client is detaching cleanly (server keeps session alive).
    Detach,
}

/// Messages the backend sends to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Acknowledgement of `Attach`. Includes the assigned session id and the
    /// sequence number from which live output will resume.
    Attached {
        session_id: SessionId,
        next_seq: SeqNo,
    },
    /// PTY output bytes.
    Output { seq: SeqNo, data: String },
    /// Replay window has expired; the client must reset.
    ReplayWindowLost,
    /// Backend-side error surfaced to the client.
    Error { message: String },
}
