//! Durable terminal recording domain types.
//!
//! These types describe the persisted chunk and marker rows that the
//! recording subsystem will eventually own. This module is the domain
//! contract; the schema lives under `apps/backend/migrations/`, the
//! repository contracts in `crate::repository`, and the Postgres
//! implementations in `relayterm-db`.
//!
//! Load-bearing privacy rules — see `docs/terminal-recording.md`
//! Section 7 ("Privacy and security posture") and SPEC.md "Durable
//! terminal recording and replay architecture":
//!
//! * [`TerminalRecordingChunk::payload`] holds opaque PTY OUTPUT bytes.
//!   The bytes are sensitive — they may include anything the operator's
//!   shell printed (env-var dumps, decrypted file contents, API tokens
//!   echoed by tooling). They MUST NEVER appear in `audit_events.payload`,
//!   `tracing::*` lines, panic messages, HTTP error response bodies, or
//!   any frontend storage. The `Debug` impls on this module redact the
//!   bytes to length-only.
//! * [`TerminalRecordingMarker::payload`] is metadata-only JSON
//!   (resize dims, reason codes, gap ranges) — never PTY bytes.
//! * Neither domain struct derives `Serialize`. A future REST surface
//!   that needs to expose chunks to a caller MUST build its own DTO
//!   from explicit fields, never `serde_json::to_value(&chunk)`.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::ids::{TerminalRecordingChunkId, TerminalRecordingMarkerId, TerminalSessionId};

/// Payload-encryption scheme stored in `terminal_recording_chunks.encryption`.
///
/// v1 only writes [`Self::None`]. Future slices add a `recording_v1`
/// variant via a CHECK-extending migration; existing rows stay `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRecordingPayloadEncryption {
    /// Plaintext-at-rest. The operator has accepted the documented
    /// at-rest risk in their config.
    None,
}

impl TerminalRecordingPayloadEncryption {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Payload-compression scheme stored in `terminal_recording_chunks.compression`.
///
/// v1 only writes [`Self::None`]. A future zstd variant lands behind its
/// own migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRecordingCompression {
    None,
}

impl TerminalRecordingCompression {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }

    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Categorical kind of a recording marker row.
///
/// Mirrors the `terminal_recording_markers_kind_chk` CHECK in the migration;
/// new kinds add a variant here AND extend the CHECK in a follow-up
/// migration (never replace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRecordingMarkerKind {
    /// Recording began for this session. The only kind allowed at
    /// `seq = 0`; written before the forwarder has stamped any
    /// `Output` frame.
    Started,
    Attached,
    Detached,
    Reattached,
    Resized,
    Closed,
    /// The chunk writer dropped frames under backpressure / cap pressure;
    /// the marker brackets the lost seq range so the replay surface
    /// surfaces a clean `ReplayWindowLost` instead of faking continuity.
    ReplayGap,
}

impl TerminalRecordingMarkerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Attached => "attached",
            Self::Detached => "detached",
            Self::Reattached => "reattached",
            Self::Resized => "resized",
            Self::Closed => "closed",
            Self::ReplayGap => "replay_gap",
        }
    }

    /// Parse the canonical tag; returns `None` for unknown values.
    #[must_use]
    pub fn from_str_tag(value: &str) -> Option<Self> {
        Some(match value {
            "started" => Self::Started,
            "attached" => Self::Attached,
            "detached" => Self::Detached,
            "reattached" => Self::Reattached,
            "resized" => Self::Resized,
            "closed" => Self::Closed,
            "replay_gap" => Self::ReplayGap,
            _ => return None,
        })
    }

    /// Returns true iff this marker kind tolerates `seq = 0`.
    ///
    /// Mirrors the schema's `terminal_recording_markers_started_seq_chk`
    /// CHECK so callers can validate before they hit the DB.
    #[must_use]
    pub const fn allows_seq_zero(self) -> bool {
        matches!(self, Self::Started)
    }
}

/// One persisted chunk of recorded PTY OUTPUT bytes for a session.
///
/// `Debug` redacts [`Self::payload`] to length-only so a stray
/// `tracing::debug!(?chunk)` cannot leak terminal bytes. The struct
/// deliberately does NOT derive `Serialize` / `Deserialize`; any future
/// REST DTO must opt in field-by-field.
#[derive(Clone, PartialEq, Eq)]
pub struct TerminalRecordingChunk {
    pub id: TerminalRecordingChunkId,
    pub terminal_session_id: TerminalSessionId,
    /// Inclusive lowest output seq covered by this chunk. `>= 1`.
    pub seq_start: i64,
    /// Inclusive highest output seq covered by this chunk. `>= seq_start`.
    pub seq_end: i64,
    /// Length in bytes of [`Self::payload`] AFTER any encryption /
    /// compression. Schema CHECK pins this against `octet_length(payload)`.
    pub byte_len: i32,
    /// Opaque chunk bytes. Sensitive — see module docs.
    pub payload: Vec<u8>,
    pub encryption: TerminalRecordingPayloadEncryption,
    pub compression: TerminalRecordingCompression,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for TerminalRecordingChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalRecordingChunk")
            .field("id", &self.id)
            .field("terminal_session_id", &self.terminal_session_id)
            .field("seq_start", &self.seq_start)
            .field("seq_end", &self.seq_end)
            .field("byte_len", &self.byte_len)
            .field(
                "payload",
                &format_args!("<redacted: {} bytes>", self.payload.len()),
            )
            .field("encryption", &self.encryption)
            .field("compression", &self.compression)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// One persisted recording marker row.
///
/// `Debug` is derived because [`Self::payload`] is metadata-only by
/// contract (Section 5.5 of the design doc). Callers writing a marker
/// payload MUST use the helper builders / explicit object construction
/// — never `serde_json::to_value` against a bag of arbitrary types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRecordingMarker {
    pub id: TerminalRecordingMarkerId,
    pub terminal_session_id: TerminalSessionId,
    pub kind: TerminalRecordingMarkerKind,
    /// Output seq at which the marker was observed. `0` is allowed only
    /// for [`TerminalRecordingMarkerKind::Started`]; every other kind
    /// requires `seq >= 1`. Schema CHECK pins this.
    pub seq: i64,
    /// Public-safe metadata only — counts, dimensions, reason codes.
    /// Never PTY bytes, never `client_info`, never error text.
    pub payload: JsonValue,
    pub created_at: DateTime<Utc>,
}

/// Aggregate read-side metadata for a session's durable recording.
///
/// Contains counts and the seq / time bounds the read API surfaces to a
/// caller. NEVER carries chunk payload bytes — by contract this is a
/// metadata-only summary. A session with no chunks AND no markers
/// produces a struct with zero counts and `None` bounds (i.e.
/// `has_recording == false` at the API layer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRecordingMetadata {
    pub terminal_session_id: TerminalSessionId,
    /// Number of chunk rows for the session.
    pub chunk_count: i64,
    /// Number of marker rows for the session.
    pub marker_count: i64,
    /// Lowest `seq_start` across chunks; `None` when no chunks exist.
    pub first_seq: Option<i64>,
    /// Highest `seq_end` across chunks; `None` when no chunks exist.
    pub last_seq: Option<i64>,
    /// Earliest `created_at` across chunk OR marker rows.
    pub first_recorded_at: Option<DateTime<Utc>>,
    /// Latest `created_at` across chunk OR marker rows.
    pub last_recorded_at: Option<DateTime<Utc>>,
}

impl TerminalRecordingMetadata {
    /// Empty / "no recording" instance for a session id.
    #[must_use]
    pub fn empty(terminal_session_id: TerminalSessionId) -> Self {
        Self {
            terminal_session_id,
            chunk_count: 0,
            marker_count: 0,
            first_seq: None,
            last_seq: None,
            first_recorded_at: None,
            last_recorded_at: None,
        }
    }

    /// `true` iff at least one chunk OR marker row exists.
    #[must_use]
    pub const fn has_recording(&self) -> bool {
        self.chunk_count > 0 || self.marker_count > 0
    }
}

/// Summary of one session's purge — the durable counts and bytes that
/// the retention worker writes into the `recording_purged` audit row.
///
/// All fields are aggregate primitives (counts, byte totals, ids,
/// timestamps). This struct deliberately does NOT carry per-chunk seq
/// ranges, per-chunk byte counts, per-chunk ids, per-marker kinds, or
/// per-marker payloads — the audit row's redaction policy
/// (`docs/terminal-recording.md` Section 12.5) collapses to aggregates,
/// and the repository-side type enforces the same shape so a future
/// caller cannot accidentally widen the payload.
///
/// `Debug` is derived because every field is a public-safe primitive.
/// The struct is NOT `Serialize` — the API / audit DTO builds its
/// payload field-by-field rather than `serde_json::to_value`-ing a
/// domain struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgedRecordingSummary {
    pub terminal_session_id: TerminalSessionId,
    /// Number of chunk rows that were deleted. `>= 0`.
    pub chunk_count: i64,
    /// Number of marker rows that were deleted. `>= 0`.
    pub marker_count: i64,
    /// `SUM(byte_len)` across deleted chunks. Markers are
    /// metadata-only by contract and contribute zero bytes here.
    /// `>= 0`.
    pub bytes_purged: i64,
    /// The session's `closed_at` value at the moment of the purge —
    /// the field the retention eligibility predicate measured against
    /// (`docs/terminal-recording.md` Section 12.2). Pinned in the
    /// summary so the audit row can record it without re-querying the
    /// (preserved) `terminal_sessions` row.
    pub closed_at: DateTime<Utc>,
    /// Worker-supplied UTC timestamp captured at the START of the
    /// per-session purge transaction (the same value the worker passed
    /// in as `PurgeRecordingForRetention::now`). The repository writes
    /// this same value into the `audit_events.recorded_at` column of
    /// the `recording_purged` row, so the summary's `purged_at` and
    /// the audit row's `recorded_at` are byte-identical. The actual
    /// `COMMIT` happens slightly later — typically sub-millisecond,
    /// but a slow transaction can widen the gap; callers that need
    /// COMMIT-precise timing should not rely on this field.
    pub purged_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{TerminalRecordingChunkId, TerminalRecordingMarkerId, TerminalSessionId};

    const SENTINEL_BYTES: &[u8] = b"RECORDING-SENTINEL-AB12";

    #[test]
    fn marker_kind_round_trips() {
        for kind in [
            TerminalRecordingMarkerKind::Started,
            TerminalRecordingMarkerKind::Attached,
            TerminalRecordingMarkerKind::Detached,
            TerminalRecordingMarkerKind::Reattached,
            TerminalRecordingMarkerKind::Resized,
            TerminalRecordingMarkerKind::Closed,
            TerminalRecordingMarkerKind::ReplayGap,
        ] {
            let tag = kind.as_str();
            assert_eq!(TerminalRecordingMarkerKind::from_str_tag(tag), Some(kind));
        }
    }

    #[test]
    fn unknown_marker_kind_is_rejected() {
        assert_eq!(TerminalRecordingMarkerKind::from_str_tag("unknown"), None);
        assert_eq!(TerminalRecordingMarkerKind::from_str_tag(""), None);
        assert_eq!(TerminalRecordingMarkerKind::from_str_tag("STARTED"), None);
    }

    #[test]
    fn allows_seq_zero_only_for_started() {
        assert!(TerminalRecordingMarkerKind::Started.allows_seq_zero());
        for kind in [
            TerminalRecordingMarkerKind::Attached,
            TerminalRecordingMarkerKind::Detached,
            TerminalRecordingMarkerKind::Reattached,
            TerminalRecordingMarkerKind::Resized,
            TerminalRecordingMarkerKind::Closed,
            TerminalRecordingMarkerKind::ReplayGap,
        ] {
            assert!(!kind.allows_seq_zero(), "{kind:?} must not allow seq=0");
        }
    }

    #[test]
    fn encryption_round_trips() {
        let tag = TerminalRecordingPayloadEncryption::None.as_str();
        assert_eq!(tag, "none");
        assert_eq!(
            TerminalRecordingPayloadEncryption::from_str_tag(tag),
            Some(TerminalRecordingPayloadEncryption::None),
        );
        assert_eq!(
            TerminalRecordingPayloadEncryption::from_str_tag("recording_v1"),
            None,
        );
    }

    #[test]
    fn compression_round_trips() {
        let tag = TerminalRecordingCompression::None.as_str();
        assert_eq!(tag, "none");
        assert_eq!(
            TerminalRecordingCompression::from_str_tag(tag),
            Some(TerminalRecordingCompression::None),
        );
        assert_eq!(TerminalRecordingCompression::from_str_tag("zstd"), None);
    }

    #[test]
    fn chunk_debug_redacts_payload_bytes() {
        let chunk = TerminalRecordingChunk {
            id: TerminalRecordingChunkId::new(),
            terminal_session_id: TerminalSessionId::new(),
            seq_start: 1,
            seq_end: 4,
            byte_len: SENTINEL_BYTES.len() as i32,
            payload: SENTINEL_BYTES.to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
            created_at: Utc::now(),
        };
        let dbg = format!("{chunk:?}");
        assert!(
            !dbg.contains("RECORDING-SENTINEL-AB12"),
            "payload sentinel leaked into TerminalRecordingChunk Debug: {dbg}",
        );
        assert!(
            dbg.contains("redacted"),
            "Debug output should mention redaction: {dbg}",
        );
    }

    #[test]
    fn marker_payload_debug_does_not_leak_bytes() {
        // Markers are metadata-only by contract — a sentinel-shaped string
        // smuggled into the payload would be a bug at the writer layer.
        // This test pins that the Debug impl on the marker faithfully
        // formats whatever JSON it was given (so a bug-driven sentinel is
        // visible in tests) AND that nothing in the wrapper struct hides
        // it. The redaction backstop for byte material is on the chunk.
        let payload = serde_json::json!({ "cols": 80, "rows": 24 });
        let marker = TerminalRecordingMarker {
            id: TerminalRecordingMarkerId::new(),
            terminal_session_id: TerminalSessionId::new(),
            kind: TerminalRecordingMarkerKind::Resized,
            seq: 17,
            payload,
            created_at: Utc::now(),
        };
        let dbg = format!("{marker:?}");
        assert!(dbg.contains("Resized"));
        assert!(dbg.contains("80"));
        assert!(dbg.contains("24"));
    }
}
