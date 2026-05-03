//! Read-side DTOs for the durable terminal recording API.
//!
//! Privacy contract:
//! - `TerminalRecordingChunkResponse::data_b64` is the ONLY surface that
//!   carries chunk bytes off the backend, and it MUST always be base64.
//!   The struct does NOT carry a raw `Vec<u8>` field. The encoding step
//!   lives in the route handler so the DTO can never be constructed
//!   from raw bytes by accident.
//! - `TerminalRecordingMarkerResponse::payload` is metadata-only by
//!   contract — the writer constructs marker payloads field-by-field
//!   (see `crates/relayterm-terminal/src/recording.rs`). Echoing the
//!   stored JSON value back to the caller is safe.
//! - `TerminalRecordingMetadataResponse` carries counts, seq bounds, and
//!   timestamps only. NEVER chunk bytes, NEVER marker bytes, NEVER any
//!   ownership / internals fields.
//!
//! These types deliberately do NOT derive `Deserialize`. The read API is
//! GET-only; a future write surface (export, retention) gets its own
//! DTOs scoped to that route.

use std::fmt;

use chrono::{DateTime, Utc};
use relayterm_core::ids::TerminalSessionId;
use relayterm_core::terminal_recording::{TerminalRecordingMarkerKind, TerminalRecordingMetadata};
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Aggregate read-side metadata for a session's recording.
#[derive(Debug, Serialize)]
pub(crate) struct TerminalRecordingMetadataResponse {
    pub terminal_session_id: TerminalSessionId,
    /// `true` iff at least one chunk OR marker row exists for the
    /// session. The other fields can still be populated independently
    /// (e.g. a session with only a `started` marker has
    /// `has_recording == true`, `chunk_count == 0`).
    pub has_recording: bool,
    pub chunk_count: i64,
    pub marker_count: i64,
    /// Lowest `seq_start` across chunks, `None` when no chunks exist.
    pub first_seq: Option<i64>,
    /// Highest `seq_end` across chunks, `None` when no chunks exist.
    pub last_seq: Option<i64>,
    /// Earliest `created_at` across chunk OR marker rows.
    pub first_recorded_at: Option<DateTime<Utc>>,
    /// Latest `created_at` across chunk OR marker rows.
    pub last_recorded_at: Option<DateTime<Utc>>,
}

impl From<TerminalRecordingMetadata> for TerminalRecordingMetadataResponse {
    fn from(m: TerminalRecordingMetadata) -> Self {
        Self {
            terminal_session_id: m.terminal_session_id,
            has_recording: m.has_recording(),
            chunk_count: m.chunk_count,
            marker_count: m.marker_count,
            first_seq: m.first_seq,
            last_seq: m.last_seq,
            first_recorded_at: m.first_recorded_at,
            last_recorded_at: m.last_recorded_at,
        }
    }
}

/// One recording chunk on the read API.
///
/// `data_b64` is RFC-4648 standard-alphabet base64 of the chunk bytes
/// AS-PERSISTED (post-encryption / post-compression). Decoding is the
/// caller's responsibility; the bytes are opaque to the wire today and
/// will become a typed envelope when the encryption / compression
/// implementation lands. NEVER log this field, NEVER include it in any
/// error path. The `Debug` impl below redacts `data_b64` to length-only
/// so a stray `tracing::debug!(?response)` cannot leak the payload —
/// base64 is a wire-shape, NOT a redaction layer.
#[derive(Serialize)]
pub(crate) struct TerminalRecordingChunkResponse {
    pub seq_start: i64,
    pub seq_end: i64,
    pub byte_len: i32,
    /// base64-encoded chunk payload — see struct docs.
    pub data_b64: String,
    pub encryption: &'static str,
    pub compression: &'static str,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for TerminalRecordingChunkResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalRecordingChunkResponse")
            .field("seq_start", &self.seq_start)
            .field("seq_end", &self.seq_end)
            .field("byte_len", &self.byte_len)
            .field(
                "data_b64",
                &format_args!("<redacted: {} chars>", self.data_b64.len()),
            )
            .field("encryption", &self.encryption)
            .field("compression", &self.compression)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// One recording marker on the read API.
///
/// `payload` is the stored metadata JSON, echoed verbatim. The writer
/// builds marker payloads field-by-field (counts, dims, reason codes);
/// any byte material would be a bug at the writer layer, not here.
#[derive(Debug, Serialize)]
pub(crate) struct TerminalRecordingMarkerResponse {
    pub kind: TerminalRecordingMarkerKind,
    pub seq: i64,
    pub payload: JsonValue,
    pub created_at: DateTime<Utc>,
}
