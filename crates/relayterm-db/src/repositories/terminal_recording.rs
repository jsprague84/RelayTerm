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
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::ids::TerminalSessionId;
use relayterm_core::repository::{
    CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, PurgeRecordingForRetention,
    RepositoryError, TerminalRecordingRepository,
};
use relayterm_core::terminal_recording::{
    PurgedRecordingSummary, TerminalRecordingChunk, TerminalRecordingMarker,
    TerminalRecordingMetadata,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::{TerminalRecordingChunkRow, TerminalRecordingMarkerRow};

const ENTITY_CHUNK: &str = "terminal_recording_chunk";
const ENTITY_MARKER: &str = "terminal_recording_marker";
const ENTITY_AUDIT: &str = "audit_event";
const ENTITY_SESSION: &str = "terminal_session";

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

    async fn purge_for_retention(
        &self,
        input: PurgeRecordingForRetention,
    ) -> Result<Option<PurgedRecordingSummary>, RepositoryError> {
        // Single transaction. Audit failure ROLLBACK reverts the
        // deletes — the purge is fail-closed (Section 12.4 of
        // `docs/terminal-recording.md`). Either both writes land OR
        // neither does and the next sweep retries.
        //
        // Locking discipline:
        //  - The session row is locked `FOR UPDATE` against
        //    `terminal_sessions`. This serialises against any future
        //    writer that mutates `closed_at` (none today, but cheap
        //    future-proofing) AND against a concurrent retention
        //    sweep that addresses the same session id (a multi-node
        //    deployment with a slow advisory-lock dance, or a
        //    test-spawned race).
        //  - Chunk + marker rows do NOT need explicit locking: the FK
        //    is `ON DELETE RESTRICT` to `terminal_sessions(id)` and
        //    nothing else mutates these tables once the session is
        //    closed. The session-row lock is sufficient.
        //
        // Privacy discipline (mirrors the trait contract):
        //  - The aggregate query reads `byte_len` only — never
        //    `payload`. `bytes_purged` comes from `SUM(byte_len)`.
        //  - The audit payload is built field-by-field from primitive
        //    aggregates. NEVER `serde_json::to_value` of a domain
        //    struct, NEVER per-chunk seq ranges, NEVER per-chunk
        //    byte counts.
        //  - All errors flow through `map_sqlx_error` which strips
        //    driver text down to the entity name plus the constraint.
        let session_uuid = input.terminal_session_id.into_uuid();
        // Postgres rejects negative arguments to `make_interval`; the
        // domain type is `u32`, but cast through `i32` defensively.
        // `u32::MAX` is well past any plausible retention policy
        // (`retention_days <= 3650` per the SPEC.md envelope), so a
        // wraparound here would be a programmer error elsewhere.
        let retention_days_i32 =
            i32::try_from(input.retention_days).map_err(|_| RepositoryError::Validation {
                field: "retention_days",
                message: format!("retention_days {} exceeds i32::MAX", input.retention_days),
            })?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_sqlx_error(ENTITY_SESSION, e))?;

        // Eligibility predicate (1) + (2): closed AND past threshold.
        // The session row is locked so a concurrent sweep cannot
        // double-purge. The column is nullable so `query_scalar` infers
        // `Option<DateTime<Utc>>` and `fetch_optional` wraps that in
        // another `Option` for "row missing"; `.flatten()` collapses
        // "row missing" and "column null" (impossible here because of
        // the WHERE clause, but defensive) into a single `None`.
        let closed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
            r#"
            SELECT closed_at
            FROM terminal_sessions
            WHERE id = $1
              AND closed_at IS NOT NULL
              AND closed_at + make_interval(days => $2) <= $3
            FOR UPDATE
            "#,
        )
        .bind(session_uuid)
        .bind(retention_days_i32)
        .bind(input.now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_SESSION, e))?
        .flatten();

        let Some(closed_at) = closed_at else {
            // Unknown session, still-open session, OR closed-but-inside
            // retention. Nothing to do — let `tx` drop, sqlx rolls
            // back the read-only transaction implicitly. We deliberately
            // do NOT call `tx.commit()` here: an `Err` from `commit()`
            // on a transient network glitch would surface to the worker
            // as a hard failure for what is in fact a benign no-op
            // ineligibility check. Rolling back on drop keeps the wire
            // shape consistent: a no-op returns `Ok(None)` regardless
            // of network weather.
            drop(tx);
            return Ok(None);
        };

        // Aggregate counts + bytes. The chunk projection reads
        // `byte_len` ONLY — never `payload`. `COALESCE(SUM(byte_len),
        // 0)` so an empty chunk set returns 0 instead of NULL.
        let chunk_aggregate: (i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COUNT(*)::BIGINT                 AS chunk_count,
                COALESCE(SUM(byte_len), 0)::BIGINT AS bytes_purged
            FROM terminal_recording_chunks
            WHERE terminal_session_id = $1
            "#,
        )
        .bind(session_uuid)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_CHUNK, e))?;
        let (chunk_count, bytes_purged) = chunk_aggregate;

        let marker_count: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)::BIGINT
            FROM terminal_recording_markers
            WHERE terminal_session_id = $1
            "#,
        )
        .bind(session_uuid)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_MARKER, e))?;

        // Eligibility predicate (3): at least one chunk OR marker row
        // exists. A session that was never recorded — or that a
        // previous purge already cleared — falls out here without any
        // delete or audit insert. This is the schema-side idempotency
        // keystone: re-running the worker against an already-purged
        // session is a byte-identical no-op. Same rollback-on-drop
        // discipline as the predicate (1)+(2) early-return above:
        // commit failures on a no-op path must not surface as hard
        // errors.
        if chunk_count == 0 && marker_count == 0 {
            drop(tx);
            return Ok(None);
        }

        // Recommended delete order (Section 12.4): markers first, then
        // chunks. There is no FK between the two tables; the order
        // is documented for readability, not correctness.
        sqlx::query(
            r#"
            DELETE FROM terminal_recording_markers
            WHERE terminal_session_id = $1
            "#,
        )
        .bind(session_uuid)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_MARKER, e))?;

        sqlx::query(
            r#"
            DELETE FROM terminal_recording_chunks
            WHERE terminal_session_id = $1
            "#,
        )
        .bind(session_uuid)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_CHUNK, e))?;

        // Audit payload built field-by-field from primitives. NEVER a
        // bag of arbitrary types via `serde_json::to_value`. The full
        // redaction list is in `docs/terminal-recording.md`
        // Section 12.5; the AGENTS.md decision-table rule pins the
        // backstop.
        let audit_payload = serde_json::json!({
            "target_kind": "terminal_session",
            "target_id": input.terminal_session_id.into_uuid(),
            "chunk_count": chunk_count,
            "marker_count": marker_count,
            "bytes_purged": bytes_purged,
            "retention_days": input.retention_days,
            "closed_at": closed_at,
            "purged_at": input.now,
            "reason": "retention_expired",
        });
        let audit_id = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO audit_events (
                id, actor_id, kind, payload, remote_addr, recorded_at
            )
            VALUES ($1, NULL, $2, $3, NULL, $4)
            "#,
        )
        .bind(audit_id)
        .bind(AuditEventKind::RecordingPurged.as_str())
        .bind(&audit_payload)
        .bind(input.now)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_AUDIT, e))?;

        tx.commit()
            .await
            .map_err(|e| map_sqlx_error(ENTITY_SESSION, e))?;

        Ok(Some(PurgedRecordingSummary {
            terminal_session_id: input.terminal_session_id,
            chunk_count,
            marker_count,
            bytes_purged,
            closed_at,
            purged_at: input.now,
        }))
    }

    async fn list_eligible_for_retention(
        &self,
        retention_days: u32,
        now: DateTime<Utc>,
        limit: u32,
    ) -> Result<Vec<TerminalSessionId>, RepositoryError> {
        // Privacy contract: the projection is `terminal_sessions.id`
        // ONLY. The eligibility filter touches `terminal_sessions`
        // columns plus an `EXISTS (SELECT 1 FROM ... LIMIT 1)` against
        // the recording tables — never `payload`, never `byte_len`.
        // Aggregating bytes is the job of `purge_for_retention`, not
        // this listing.
        //
        // Bound discipline: this is an internal sweep surface, NOT a
        // user-paginated API call, so we do NOT apply the
        // `LIST_LIMIT_CEILING = 1024` clamp the chunk / marker reads
        // use — that clamp is defence-in-depth against arbitrary
        // browser callers, but the retention sweep's `limit` already
        // comes from the boot-validated `cleanup.batch_size` (capped
        // at 10_000 by the config validator). Forcing the ceiling
        // here would silently truncate a large operator-configured
        // batch and let the `batch_truncated` signal go stale across
        // restarts. The only safety floor we keep is `>= 1`.
        //
        // Ordering: `closed_at ASC` so the oldest backlog drains first
        // across multiple sweep cycles.
        let bounded_limit = i64::from(limit.max(1));
        let retention_days_i32 =
            i32::try_from(retention_days).map_err(|_| RepositoryError::Validation {
                field: "retention_days",
                message: format!("retention_days {retention_days} exceeds i32::MAX"),
            })?;

        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"
            SELECT s.id
            FROM terminal_sessions AS s
            WHERE s.closed_at IS NOT NULL
              AND s.closed_at + make_interval(days => $1) <= $2
              AND (
                  EXISTS (
                      SELECT 1
                      FROM terminal_recording_chunks AS c
                      WHERE c.terminal_session_id = s.id
                  )
                  OR EXISTS (
                      SELECT 1
                      FROM terminal_recording_markers AS m
                      WHERE m.terminal_session_id = s.id
                  )
              )
            ORDER BY s.closed_at ASC
            LIMIT $3
            "#,
        )
        .bind(retention_days_i32)
        .bind(now)
        .bind(bounded_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_SESSION, e))?;

        Ok(rows
            .into_iter()
            .map(|(id,)| TerminalSessionId::from_uuid(id))
            .collect())
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
