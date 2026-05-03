//! Terminal-session durable recording read API.
//!
//! These endpoints expose the persisted recording artefacts (metadata
//! summary, chunks, markers) to the owning user as a foundation for a
//! future replay viewer. They are read-only and owner-scoped.
//!
//! ## Privacy posture
//!
//! - **Owner-scoped.** Every route resolves the `terminal_sessions` row
//!   by id AND filters by `owner_id == caller`. A foreign-owned id and
//!   an unknown id collapse to a single byte-identical 404 — cross-user
//!   existence is never leaked.
//! - **No audit writes.** Read endpoints are intentionally NOT audited.
//!   An audit pattern for replay reads is future work and should be
//!   designed alongside any retention / export surface.
//! - **Chunk bytes are base64.** [`TerminalRecordingChunkResponse::data_b64`]
//!   is the only surface that carries chunk bytes off the backend. The
//!   payload is opaque on the wire (encryption / compression aware
//!   decoding is the caller's responsibility once those land). The
//!   bytes are NEVER logged, NEVER appear in any thrown error, and
//!   NEVER reach the audit-event payload.
//! - **Markers are metadata-only by writer contract.** The route echoes
//!   the stored JSON verbatim; any byte material in a marker payload
//!   would be a bug at the writer layer.
//!
//! ## Pagination & query parsing
//!
//! - `from_seq` is the inclusive lower bound; chunks default to `1`,
//!   markers default to `0` (a `started` marker rides at `seq = 0`).
//!   Negative values are rejected as `400 invalid_input`.
//! - `limit` clamps to `1..=MAX_LIMIT` and defaults to
//!   [`DEFAULT_LIMIT`]. The repository ALSO enforces a defence-in-depth
//!   ceiling of 1024 rows; callers cannot blow past either layer.
//! - Invalid query syntax (non-numeric, malformed) surfaces as
//!   `400 invalid_input` via axum's `Query` extractor — the offending
//!   value is NOT echoed in the wire body.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use relayterm_core::ids::TerminalSessionId;
use relayterm_core::repository::{TerminalRecordingRepository, TerminalSessionRepository};
use relayterm_protocol::output_data_encode;
use serde::Deserialize;

use crate::AppState;
use crate::auth::AuthenticatedUser;
use crate::dto::terminal_recording::{
    TerminalRecordingChunkResponse, TerminalRecordingMarkerResponse,
    TerminalRecordingMetadataResponse,
};
use crate::error::ApiError;

/// Wire entity for owner-scoping 404s. Mirrors the `terminal_sessions`
/// route — a foreign or missing recording must be byte-identical to a
/// foreign or missing session.
const ENTITY: &str = "terminal_session";

/// Default page size when `?limit` is omitted.
const DEFAULT_LIMIT: u32 = 256;
/// Wire-side ceiling on `?limit`. The repository adds its own 1024
/// ceiling underneath; this is the API-layer cap.
const MAX_LIMIT: u32 = 1024;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}/recording/metadata", get(get_metadata))
        .route("/{id}/recording/chunks", get(list_chunks))
        .route("/{id}/recording/markers", get(list_markers))
}

#[derive(Debug, Deserialize, Default)]
struct ChunksQuery {
    /// Inclusive lower bound on `seq_start`. Defaults to `1` (the
    /// lowest legal chunk seq).
    from_seq: Option<i64>,
    /// Page size. Clamped to `1..=MAX_LIMIT`.
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
struct MarkersQuery {
    /// Inclusive lower bound on `seq`. Defaults to `0` (the
    /// `started` marker rides at seq=0).
    from_seq: Option<i64>,
    /// Page size. Clamped to `1..=MAX_LIMIT`.
    limit: Option<u32>,
}

/// `GET /api/v1/terminal-sessions/:id/recording/metadata`.
async fn get_metadata(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<TerminalRecordingMetadataResponse>, ApiError> {
    resolve_owned_session(&state, user, id).await?;
    let metadata = state.db.terminal_recordings().get_metadata(id).await?;
    Ok(Json(metadata.into()))
}

/// `GET /api/v1/terminal-sessions/:id/recording/chunks?from_seq=..&limit=..`.
async fn list_chunks(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<TerminalSessionId>,
    Query(q): Query<ChunksQuery>,
) -> Result<Json<Vec<TerminalRecordingChunkResponse>>, ApiError> {
    resolve_owned_session(&state, user, id).await?;
    let from_seq = parse_from_seq(q.from_seq, 1)?;
    let limit = clamp_limit(q.limit);

    let chunks = state
        .db
        .terminal_recordings()
        .list_chunks(id, from_seq, limit)
        .await?;
    let body: Vec<TerminalRecordingChunkResponse> = chunks
        .into_iter()
        .map(|c| TerminalRecordingChunkResponse {
            seq_start: c.seq_start,
            seq_end: c.seq_end,
            byte_len: c.byte_len,
            // Encoded here, never anywhere upstream — keeps raw bytes
            // off any error path / log line. The encode helper is the
            // same RFC-4648 standard alphabet the wire protocol uses.
            data_b64: output_data_encode(&c.payload),
            encryption: c.encryption.as_str(),
            compression: c.compression.as_str(),
            created_at: c.created_at,
        })
        .collect();
    Ok(Json(body))
}

/// `GET /api/v1/terminal-sessions/:id/recording/markers?from_seq=..&limit=..`.
async fn list_markers(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<TerminalSessionId>,
    Query(q): Query<MarkersQuery>,
) -> Result<Json<Vec<TerminalRecordingMarkerResponse>>, ApiError> {
    resolve_owned_session(&state, user, id).await?;
    let from_seq = parse_from_seq(q.from_seq, 0)?;
    let limit = clamp_limit(q.limit);

    let markers = state
        .db
        .terminal_recordings()
        .list_markers(id, from_seq, limit)
        .await?;
    let body: Vec<TerminalRecordingMarkerResponse> = markers
        .into_iter()
        .map(|m| TerminalRecordingMarkerResponse {
            kind: m.kind,
            seq: m.seq,
            payload: m.payload,
            created_at: m.created_at,
        })
        .collect();
    Ok(Json(body))
}

/// Resolve the addressed `terminal_session` AND prove the caller owns
/// it. Foreign-owned and missing collapse to the same 404 entity so
/// cross-user existence is never leaked — same shape the
/// `terminal_sessions` route uses.
async fn resolve_owned_session(
    state: &AppState,
    user: AuthenticatedUser,
    id: TerminalSessionId,
) -> Result<(), ApiError> {
    state
        .db
        .terminal_sessions()
        .get(id)
        .await?
        .filter(|s| s.owner_id == user.user_id())
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(())
}

fn clamp_limit(raw: Option<u32>) -> u32 {
    raw.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Parse an optional `from_seq` query parameter. Negative values are
/// rejected as 400 — the wire body uses a static classifier so the
/// offending value is NOT echoed back. `None` returns `default`. The
/// upper bound is deliberately unconstrained: a pathological
/// `from_seq = i64::MAX` is harmless because the SQL `WHERE seq >=
/// $from_seq` filter simply returns an empty result, and the chunk /
/// marker `seq` columns are themselves `BIGINT` so no overflow path
/// exists.
fn parse_from_seq(raw: Option<i64>, default: i64) -> Result<i64, ApiError> {
    let v = raw.unwrap_or(default);
    if v < 0 {
        return Err(ApiError::Validation("from_seq: must be >= 0".to_owned()));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_clamping_table() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(500)), 500);
        assert_eq!(clamp_limit(Some(MAX_LIMIT)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(MAX_LIMIT + 1)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(u32::MAX)), MAX_LIMIT);
    }

    #[test]
    fn from_seq_default_when_none() {
        assert_eq!(parse_from_seq(None, 1).unwrap(), 1);
        assert_eq!(parse_from_seq(None, 0).unwrap(), 0);
    }

    #[test]
    fn from_seq_negative_rejected() {
        let err = parse_from_seq(Some(-1), 1).unwrap_err();
        match err {
            ApiError::Validation(msg) => {
                assert!(msg.contains("from_seq"), "unexpected message: {msg}");
                assert!(msg.contains(">= 0"), "unexpected message: {msg}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn from_seq_zero_and_positive_pass_through() {
        assert_eq!(parse_from_seq(Some(0), 1).unwrap(), 0);
        assert_eq!(parse_from_seq(Some(42), 1).unwrap(), 42);
        assert_eq!(parse_from_seq(Some(i64::MAX), 1).unwrap(), i64::MAX);
    }
}
