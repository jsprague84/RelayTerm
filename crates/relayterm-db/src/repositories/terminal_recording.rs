//! Postgres implementation of [`TerminalRecordingRepository`].
//!
//! Privacy contract:
//! - Chunk `payload` bytes never reach a `tracing::*` line, never appear
//!   in any error path, and never round-trip through a `Debug` impl.
//!   `map_sqlx_error` already strips driver text down to the entity name
//!   plus the constraint name; this layer keeps it that way.
//! - Marker `payload` is metadata-only by contract — the writer above is
//!   responsible for object construction discipline. The repository
//!   stores whatever JSON it is given.
//!
//! Bound discipline:
//! - `LIST_LIMIT_CEILING` is the defence-in-depth cap on `limit` for
//!   chunk and marker reads. The API layer adds its own pagination cap
//!   on top; this is the floor that no caller can blow past.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::TerminalSessionId;
use relayterm_core::repository::{
    CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, RepositoryError,
    TerminalRecordingRepository,
};
use relayterm_core::terminal_recording::{
    TerminalRecordingChunk, TerminalRecordingMarker, TerminalRecordingMetadata,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::{TerminalRecordingChunkRow, TerminalRecordingMarkerRow};

const ENTITY_CHUNK: &str = "terminal_recording_chunk";
const ENTITY_MARKER: &str = "terminal_recording_marker";

/// Defence-in-depth ceiling on the number of rows a single
/// `list_chunks` / `list_markers` call may return. The API layer adds
/// its own pagination cap above this.
const LIST_LIMIT_CEILING: u32 = 1024;

#[derive(Debug, Clone)]
pub struct PgTerminalRecordingRepository {
    pool: PgPool,
}

impl PgTerminalRecordingRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn clamp_limit(limit: u32) -> i64 {
    let bounded = limit.clamp(1, LIST_LIMIT_CEILING);
    i64::from(bounded)
}

#[async_trait]
impl TerminalRecordingRepository for PgTerminalRecordingRepository {
    async fn append_chunk(
        &self,
        input: CreateTerminalRecordingChunk,
    ) -> Result<TerminalRecordingChunk, RepositoryError> {
        let id = Uuid::new_v4();
        let row: TerminalRecordingChunkRow = sqlx::query_as(
            r#"
            INSERT INTO terminal_recording_chunks (
                id, terminal_session_id, seq_start, seq_end, byte_len,
                payload, encryption, compression
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id, terminal_session_id, seq_start, seq_end, byte_len,
                      payload, encryption, compression, created_at
            "#,
        )
        .bind(id)
        .bind(input.terminal_session_id.into_uuid())
        .bind(input.seq_start)
        .bind(input.seq_end)
        .bind(input.byte_len)
        .bind(&input.payload)
        .bind(input.encryption.as_str())
        .bind(input.compression.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_CHUNK, e))?;

        row.try_into_domain()
    }

    async fn append_marker(
        &self,
        input: CreateTerminalRecordingMarker,
    ) -> Result<TerminalRecordingMarker, RepositoryError> {
        let id = Uuid::new_v4();
        let row: TerminalRecordingMarkerRow = sqlx::query_as(
            r#"
            INSERT INTO terminal_recording_markers (
                id, terminal_session_id, kind, seq, payload
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, terminal_session_id, kind, seq, payload, created_at
            "#,
        )
        .bind(id)
        .bind(input.terminal_session_id.into_uuid())
        .bind(input.kind.as_str())
        .bind(input.seq)
        .bind(&input.payload)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_MARKER, e))?;

        row.try_into_domain()
    }

    async fn list_chunks(
        &self,
        terminal_session_id: TerminalSessionId,
        from_seq: i64,
        limit: u32,
    ) -> Result<Vec<TerminalRecordingChunk>, RepositoryError> {
        let bounded_limit = clamp_limit(limit);
        let rows: Vec<TerminalRecordingChunkRow> = sqlx::query_as(
            r#"
            SELECT id, terminal_session_id, seq_start, seq_end, byte_len,
                   payload, encryption, compression, created_at
            FROM terminal_recording_chunks
            WHERE terminal_session_id = $1
              AND seq_start >= $2
            ORDER BY seq_start ASC
            LIMIT $3
            "#,
        )
        .bind(terminal_session_id.into_uuid())
        .bind(from_seq)
        .bind(bounded_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_CHUNK, e))?;

        rows.into_iter()
            .map(TerminalRecordingChunkRow::try_into_domain)
            .collect()
    }

    async fn list_markers(
        &self,
        terminal_session_id: TerminalSessionId,
        from_seq: i64,
        limit: u32,
    ) -> Result<Vec<TerminalRecordingMarker>, RepositoryError> {
        let bounded_limit = clamp_limit(limit);
        let rows: Vec<TerminalRecordingMarkerRow> = sqlx::query_as(
            r#"
            SELECT id, terminal_session_id, kind, seq, payload, created_at
            FROM terminal_recording_markers
            WHERE terminal_session_id = $1
              AND seq >= $2
            ORDER BY seq ASC, created_at ASC
            LIMIT $3
            "#,
        )
        .bind(terminal_session_id.into_uuid())
        .bind(from_seq)
        .bind(bounded_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_MARKER, e))?;

        rows.into_iter()
            .map(TerminalRecordingMarkerRow::try_into_domain)
            .collect()
    }

    async fn get_metadata(
        &self,
        terminal_session_id: TerminalSessionId,
    ) -> Result<TerminalRecordingMetadata, RepositoryError> {
        // Aggregate over chunks: count, seq bounds, time bounds.
        // `payload` is intentionally NOT in the projection — metadata
        // queries must never read chunk bytes.
        let chunks: (
            i64,
            Option<i64>,
            Option<i64>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
        ) = sqlx::query_as(
            r#"
                SELECT
                    COUNT(*)           AS chunk_count,
                    MIN(seq_start)     AS first_seq,
                    MAX(seq_end)       AS last_seq,
                    MIN(created_at)    AS first_recorded_at,
                    MAX(created_at)    AS last_recorded_at
                FROM terminal_recording_chunks
                WHERE terminal_session_id = $1
                "#,
        )
        .bind(terminal_session_id.into_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_CHUNK, e))?;

        let markers: (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
            r#"
            SELECT
                COUNT(*)           AS marker_count,
                MIN(created_at)    AS first_marker_at,
                MAX(created_at)    AS last_marker_at
            FROM terminal_recording_markers
            WHERE terminal_session_id = $1
            "#,
        )
        .bind(terminal_session_id.into_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_MARKER, e))?;

        let (chunk_count, first_seq, last_seq, chunk_first_at, chunk_last_at) = chunks;
        let (marker_count, marker_first_at, marker_last_at) = markers;

        Ok(TerminalRecordingMetadata {
            terminal_session_id,
            chunk_count,
            marker_count,
            first_seq,
            last_seq,
            first_recorded_at: min_opt(chunk_first_at, marker_first_at),
            last_recorded_at: max_opt(chunk_last_at, marker_last_at),
        })
    }
}

fn min_opt<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

fn max_opt<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_floor_is_one() {
        assert_eq!(clamp_limit(0), 1);
    }

    #[test]
    fn clamp_limit_ceiling() {
        assert_eq!(
            clamp_limit(LIST_LIMIT_CEILING + 1),
            i64::from(LIST_LIMIT_CEILING)
        );
        assert_eq!(clamp_limit(u32::MAX), i64::from(LIST_LIMIT_CEILING));
    }

    #[test]
    fn clamp_limit_pass_through() {
        assert_eq!(clamp_limit(32), 32);
        assert_eq!(
            clamp_limit(LIST_LIMIT_CEILING),
            i64::from(LIST_LIMIT_CEILING)
        );
    }
}
