//! Retention sweep for the durable terminal-recording corpus.
//!
//! Stage A (this slice): a single bounded sweep run at backend startup,
//! AFTER the database pool is ready and AFTER terminal-session
//! reconciliation, BEFORE the HTTP listener binds. See
//! `docs/terminal-recording.md` Section 12.7.
//!
//! ## Shape
//!
//! 1. Ask the repository for up to `batch_size` `terminal_session_id`s
//!    eligible for purge under the current `retention_days` policy.
//! 2. Walk the list, calling [`TerminalRecordingRepository::purge_for_retention`]
//!    once per session. Each purge is its own Postgres transaction (the
//!    repository primitive owns its own `BEGIN`/`COMMIT`).
//! 3. Aggregate the per-session counts and bytes into a
//!    [`RecordingRetentionSweepSummary`] for an operator-visible
//!    one-line log.
//!
//! ## Failure semantics — fail-soft
//!
//! Stage A is **not** fail-fast (unlike Section 9.3 reconciliation). A
//! failure during candidate selection OR during a per-session purge is
//! logged with a static category tag and the sweep stops; the boot
//! continues to the listener bind. Rationale: missing one sweep cycle
//! is operationally undesirable but is not a security-relevant
//! correctness issue — orphaned recording rows are not a security risk
//! per se (the data was already authorised to exist; retention just
//! trims it). See `docs/terminal-recording.md` Section 12.7 "Failure
//! semantics".
//!
//! On the first per-session purge failure the sweep stops to avoid
//! repeated DB trouble; subsequent eligible sessions remain eligible
//! and will be picked up by the next sweep cycle (Stage B's first
//! periodic tick, or the next backend restart). The session's chunks
//! and markers are preserved by the repository's transactional
//! ROLLBACK on audit failure (Section 12.4 fail-closed).
//!
//! ## Privacy
//!
//! - The eligibility query reads `terminal_sessions.id` only — never
//!   chunk `payload`, never `byte_len`, never marker `payload`. The
//!   bytes-purged total comes exclusively from the repository's own
//!   `SUM(byte_len)` aggregate inside the purge transaction.
//! - Operator-side log lines carry only counts (sessions purged,
//!   chunks purged, markers purged, bytes purged) and the static error
//!   tag — never session ids, never repository error text, never PTY
//!   bytes, never marker JSON.
//! - The summary's `Debug` impl is derived because every field is a
//!   public-safe primitive aggregate.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use relayterm_core::repository::{
    PurgeRecordingForRetention, RepositoryError, TerminalRecordingRepository,
};
use tracing::{info, warn};

/// Aggregate result of one retention sweep iteration.
///
/// Every field is a primitive count or byte total. The sweep deliberately
/// does NOT carry per-session ids or per-session error strings — operator
/// logs and any future metric surface use the aggregates only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordingRetentionSweepSummary {
    /// Number of session ids the repository returned as eligible.
    /// Bounded by the caller's `batch_size`.
    pub candidate_count: usize,
    /// Number of sessions actually purged on this sweep. Will be
    /// `candidate_count` on a clean sweep, less when an error stopped
    /// the loop.
    pub purged_sessions: usize,
    /// Sum of `chunk_count` across the purged sessions.
    pub chunks_purged: i64,
    /// Sum of `marker_count` across the purged sessions.
    pub markers_purged: i64,
    /// Sum of `bytes_purged` across the purged sessions.
    pub bytes_purged: i64,
    /// Number of repository errors encountered. The sweep stops after
    /// the first error, so this is at most `1` in Stage A.
    pub errors: u32,
    /// `true` when the candidate listing returned exactly the
    /// caller's `batch_size` AND the sweep ran to completion without
    /// a per-session error. Implies "there may be more eligible
    /// sessions waiting for the next sweep cycle." Cleared back to
    /// `false` on any error so the Stage B periodic worker (and any
    /// future operator surface) can treat `batch_truncated == true`
    /// as an unambiguous "schedule another tick" signal without
    /// secondary checks against `errors`.
    pub batch_truncated: bool,
}

impl RecordingRetentionSweepSummary {
    /// `true` iff the sweep wrote zero rows and observed zero errors.
    /// Used by the boot-side log to drop the "swept N sessions" line
    /// to a quieter "nothing to do" tag.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.candidate_count == 0 && self.errors == 0
    }
}

/// Run one bounded retention sweep cycle. Stage A startup behaviour.
///
/// Caller obligations:
/// - Pass the repository as an `Arc<dyn TerminalRecordingRepository>`
///   so the sweep does not own the pool.
/// - `retention_days` is the active policy at sweep time
///   (`terminal_recording.retention_days`). The repository's purge
///   primitive writes this verbatim into the audit row so a later
///   operator audit can correlate "this purge happened under the X-day
///   policy."
/// - `batch_size` is the caller's `cleanup.batch_size`. Bounded to
///   `1..=10_000` by the config validator.
/// - `now` is the worker's authoritative timestamp captured ONCE
///   before the sweep. The same value drives the eligibility predicate
///   AND the per-session `purged_at` audit field. Capturing it once
///   keeps the sweep deterministic against a fast-moving wall clock.
///
/// Failure handling:
/// - A repository error during candidate selection is logged with a
///   static category tag (`"retention_sweep_failed"`) and the sweep
///   returns a summary with `errors = 1` and zero purges. Boot
///   continues.
/// - A repository error during a per-session purge is logged with the
///   same tag and the sweep stops. The summary reflects whatever
///   purges landed before the error.
///
/// On success the operator sees one `info!` line carrying the
/// aggregate counts and bytes — never session ids, never any byte
/// material from the recording corpus.
pub async fn run_recording_retention_startup_sweep(
    repo: Arc<dyn TerminalRecordingRepository>,
    retention_days: u32,
    batch_size: u32,
    now: DateTime<Utc>,
) -> RecordingRetentionSweepSummary {
    let mut summary = RecordingRetentionSweepSummary::default();

    let candidates = match repo
        .list_eligible_for_retention(retention_days, now, batch_size)
        .await
    {
        Ok(ids) => ids,
        Err(err) => {
            // Static category tag only — never echo repository error
            // internals, driver text, or session ids. The category tag
            // matches `docs/terminal-recording.md` Section 12.7.
            warn!(
                category = "retention_sweep_failed",
                stage = "list_eligible",
                error_kind = repository_error_kind(&err),
                "recording retention startup sweep: candidate selection failed; \
                 boot continues, retention deferred",
            );
            summary.errors = 1;
            return summary;
        }
    };

    summary.candidate_count = candidates.len();
    summary.batch_truncated = candidates.len() == batch_size as usize;

    if candidates.is_empty() {
        info!("recording retention startup sweep: no eligible sessions");
        return summary;
    }

    for session_id in candidates {
        match repo
            .purge_for_retention(PurgeRecordingForRetention {
                terminal_session_id: session_id,
                retention_days,
                now,
            })
            .await
        {
            Ok(Some(purged)) => {
                summary.purged_sessions += 1;
                summary.chunks_purged += purged.chunk_count;
                summary.markers_purged += purged.marker_count;
                summary.bytes_purged += purged.bytes_purged;
            }
            Ok(None) => {
                // Eligibility flipped between the listing and the
                // per-session transaction (a parallel close path
                // re-opened a row, a racing sweep already drained it,
                // etc.). Not an error — just a no-op.
            }
            Err(err) => {
                // First per-session error stops the sweep — repeated
                // DB trouble would just amplify whatever underlying
                // outage is in flight. Remaining candidates stay
                // eligible for the next sweep cycle.
                warn!(
                    category = "retention_sweep_failed",
                    stage = "purge_session",
                    error_kind = repository_error_kind(&err),
                    purged_so_far = summary.purged_sessions,
                    "recording retention startup sweep: per-session purge failed; \
                     stopping sweep, boot continues",
                );
                summary.errors = 1;
                // Clear the truncation flag on error so the
                // operator surface treats `batch_truncated` as an
                // unambiguous "schedule another tick" signal.
                summary.batch_truncated = false;
                break;
            }
        }
    }

    if summary.purged_sessions > 0 {
        info!(
            purged_sessions = summary.purged_sessions,
            chunks_purged = summary.chunks_purged,
            markers_purged = summary.markers_purged,
            bytes_purged = summary.bytes_purged,
            batch_truncated = summary.batch_truncated,
            "recording retention startup sweep: completed",
        );
    } else if summary.errors == 0 {
        // Repository returned candidate ids but every per-session
        // purge collapsed to `Ok(None)` (eligibility flipped under
        // us). Surface a quieter line so an operator does not chase
        // a phantom "swept zero" event.
        info!(
            candidate_count = summary.candidate_count,
            "recording retention startup sweep: no eligible sessions remained at purge time",
        );
    }

    summary
}

/// Map a `RepositoryError` to a short static category tag for
/// operator-side logging. Returns one of a fixed set of strings; the
/// underlying error message is NEVER returned (it may carry driver
/// text or constraint names that the worker keeps out of logs).
const fn repository_error_kind(err: &RepositoryError) -> &'static str {
    match err {
        RepositoryError::NotFound { .. } => "not_found",
        RepositoryError::Conflict { .. } => "conflict",
        RepositoryError::Validation { .. } => "validation",
        RepositoryError::Database(_) => "database",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use relayterm_core::ids::TerminalSessionId;
    use relayterm_core::repository::{
        CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, RepositoryError,
    };
    use relayterm_core::terminal_recording::{
        PurgedRecordingSummary, TerminalRecordingChunk, TerminalRecordingMarker,
        TerminalRecordingMetadata,
    };
    use std::sync::Mutex;

    /// In-memory fake whose only job is to drive the sweep loop deterministically.
    /// It tracks how many ids it returned and whether `purge_for_retention` was
    /// called per session id.
    #[derive(Default)]
    struct FakeRepo {
        candidates: Mutex<Vec<TerminalSessionId>>,
        purged: Mutex<Vec<TerminalSessionId>>,
        per_session_summary: Mutex<Vec<(i64, i64, i64)>>,
        error_on_list: Mutex<bool>,
        error_on_purge_after: Mutex<Option<usize>>,
        purge_returns_none_for: Mutex<Vec<TerminalSessionId>>,
    }

    impl FakeRepo {
        fn with_candidates(ids: Vec<TerminalSessionId>) -> Self {
            Self {
                candidates: Mutex::new(ids),
                ..Default::default()
            }
        }

        fn set_per_session_summary(&self, summaries: Vec<(i64, i64, i64)>) {
            *self.per_session_summary.lock().unwrap() = summaries;
        }

        fn set_error_on_list(&self) {
            *self.error_on_list.lock().unwrap() = true;
        }

        fn set_error_on_purge_after(&self, n: usize) {
            *self.error_on_purge_after.lock().unwrap() = Some(n);
        }

        fn purges(&self) -> Vec<TerminalSessionId> {
            self.purged.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TerminalRecordingRepository for FakeRepo {
        async fn append_chunk(
            &self,
            _input: CreateTerminalRecordingChunk,
        ) -> Result<TerminalRecordingChunk, RepositoryError> {
            unimplemented!()
        }

        async fn append_marker(
            &self,
            _input: CreateTerminalRecordingMarker,
        ) -> Result<TerminalRecordingMarker, RepositoryError> {
            unimplemented!()
        }

        async fn list_chunks(
            &self,
            _terminal_session_id: TerminalSessionId,
            _from_seq: i64,
            _limit: u32,
        ) -> Result<Vec<TerminalRecordingChunk>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn list_markers(
            &self,
            _terminal_session_id: TerminalSessionId,
            _from_seq: i64,
            _limit: u32,
        ) -> Result<Vec<TerminalRecordingMarker>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn get_metadata(
            &self,
            terminal_session_id: TerminalSessionId,
        ) -> Result<TerminalRecordingMetadata, RepositoryError> {
            Ok(TerminalRecordingMetadata::empty(terminal_session_id))
        }

        async fn purge_for_retention(
            &self,
            input: PurgeRecordingForRetention,
        ) -> Result<Option<PurgedRecordingSummary>, RepositoryError> {
            // Optionally fail after N successful purges.
            let already = self.purged.lock().unwrap().len();
            if let Some(n) = *self.error_on_purge_after.lock().unwrap()
                && already >= n
            {
                return Err(RepositoryError::Database("synthetic purge error".into()));
            }
            // Optionally collapse to None for a specific id (eligibility
            // flipped between listing and purge).
            if self
                .purge_returns_none_for
                .lock()
                .unwrap()
                .contains(&input.terminal_session_id)
            {
                return Ok(None);
            }
            let mut purges = self.purged.lock().unwrap();
            purges.push(input.terminal_session_id);
            let idx = purges.len() - 1;
            let (chunk_count, marker_count, bytes_purged) = self
                .per_session_summary
                .lock()
                .unwrap()
                .get(idx)
                .copied()
                .unwrap_or((1, 1, 64));
            Ok(Some(PurgedRecordingSummary {
                terminal_session_id: input.terminal_session_id,
                chunk_count,
                marker_count,
                bytes_purged,
                closed_at: input.now - chrono::Duration::days(31),
                purged_at: input.now,
            }))
        }

        async fn list_eligible_for_retention(
            &self,
            _retention_days: u32,
            _now: DateTime<Utc>,
            limit: u32,
        ) -> Result<Vec<TerminalSessionId>, RepositoryError> {
            if *self.error_on_list.lock().unwrap() {
                return Err(RepositoryError::Database("synthetic list error".into()));
            }
            let mut ids = self.candidates.lock().unwrap().clone();
            ids.truncate(limit as usize);
            Ok(ids)
        }
    }

    #[tokio::test]
    async fn empty_candidate_set_is_a_clean_no_op() {
        let repo = Arc::new(FakeRepo::default());
        let summary =
            run_recording_retention_startup_sweep(repo.clone(), 30, 100, Utc::now()).await;
        assert_eq!(summary.candidate_count, 0);
        assert_eq!(summary.purged_sessions, 0);
        assert_eq!(summary.errors, 0);
        assert!(summary.is_empty());
        assert!(repo.purges().is_empty());
    }

    #[tokio::test]
    async fn sweep_purges_every_candidate_and_aggregates_counts() {
        let ids = vec![
            TerminalSessionId::new(),
            TerminalSessionId::new(),
            TerminalSessionId::new(),
        ];
        let repo = Arc::new(FakeRepo::with_candidates(ids.clone()));
        repo.set_per_session_summary(vec![(2, 2, 100), (3, 1, 250), (1, 1, 50)]);

        let summary =
            run_recording_retention_startup_sweep(repo.clone(), 30, 100, Utc::now()).await;

        assert_eq!(summary.candidate_count, 3);
        assert_eq!(summary.purged_sessions, 3);
        assert_eq!(summary.chunks_purged, 6);
        assert_eq!(summary.markers_purged, 4);
        assert_eq!(summary.bytes_purged, 400);
        assert_eq!(summary.errors, 0);
        assert!(!summary.batch_truncated);
        assert_eq!(repo.purges(), ids);
    }

    #[tokio::test]
    async fn list_error_returns_error_summary_without_purges() {
        let repo = Arc::new(FakeRepo::with_candidates(vec![TerminalSessionId::new()]));
        repo.set_error_on_list();
        let summary =
            run_recording_retention_startup_sweep(repo.clone(), 30, 100, Utc::now()).await;
        assert_eq!(summary.candidate_count, 0);
        assert_eq!(summary.purged_sessions, 0);
        assert_eq!(summary.errors, 1);
        assert!(repo.purges().is_empty());
    }

    #[tokio::test]
    async fn first_purge_error_stops_sweep_after_partial_progress() {
        let ids = vec![
            TerminalSessionId::new(),
            TerminalSessionId::new(),
            TerminalSessionId::new(),
        ];
        let repo = Arc::new(FakeRepo::with_candidates(ids.clone()));
        repo.set_per_session_summary(vec![(1, 1, 10), (2, 2, 20), (3, 3, 30)]);
        repo.set_error_on_purge_after(2);

        let summary =
            run_recording_retention_startup_sweep(repo.clone(), 30, 100, Utc::now()).await;

        assert_eq!(summary.candidate_count, 3);
        assert_eq!(summary.purged_sessions, 2);
        assert_eq!(summary.chunks_purged, 3);
        assert_eq!(summary.markers_purged, 3);
        assert_eq!(summary.bytes_purged, 30);
        assert_eq!(summary.errors, 1);
        assert_eq!(repo.purges(), ids[..2]);
    }

    #[tokio::test]
    async fn purge_returning_none_is_not_an_error() {
        let id = TerminalSessionId::new();
        let repo = Arc::new(FakeRepo::with_candidates(vec![id]));
        // The repository returns None at purge time (race / already
        // purged); the sweep treats this as a benign no-op, NOT an
        // error.
        *repo.purge_returns_none_for.lock().unwrap() = vec![id];

        let summary =
            run_recording_retention_startup_sweep(repo.clone(), 30, 100, Utc::now()).await;
        assert_eq!(summary.candidate_count, 1);
        assert_eq!(summary.purged_sessions, 0);
        assert_eq!(summary.errors, 0);
    }

    #[tokio::test]
    async fn batch_truncated_flagged_when_listing_returns_full_batch() {
        let ids = vec![TerminalSessionId::new(), TerminalSessionId::new()];
        let repo = Arc::new(FakeRepo::with_candidates(ids.clone()));
        let summary = run_recording_retention_startup_sweep(repo.clone(), 30, 2, Utc::now()).await;
        assert_eq!(summary.candidate_count, 2);
        assert!(summary.batch_truncated);
    }

    #[tokio::test]
    async fn purge_error_clears_batch_truncated() {
        // The sweep is initially marked as a full batch, but the
        // first per-session purge fails. The summary must clear the
        // `batch_truncated` flag so a downstream operator surface
        // does not interpret an error path as "schedule another
        // tick."
        let ids = vec![TerminalSessionId::new(), TerminalSessionId::new()];
        let repo = Arc::new(FakeRepo::with_candidates(ids));
        repo.set_per_session_summary(vec![(1, 1, 10), (2, 2, 20)]);
        repo.set_error_on_purge_after(0);

        let summary = run_recording_retention_startup_sweep(repo.clone(), 30, 2, Utc::now()).await;
        assert_eq!(summary.errors, 1);
        assert!(
            !summary.batch_truncated,
            "an error must clear batch_truncated even if the listing was full",
        );
    }

    #[test]
    fn summary_debug_does_not_carry_session_ids() {
        // Defence in depth: the summary struct is primitives only.
        // Sanity-check that Debug renders the field names + numbers
        // and never grew a `Vec<TerminalSessionId>` field.
        let summary = RecordingRetentionSweepSummary {
            candidate_count: 3,
            purged_sessions: 3,
            chunks_purged: 12,
            markers_purged: 5,
            bytes_purged: 1024,
            errors: 0,
            batch_truncated: false,
        };
        let dbg = format!("{summary:?}");
        assert!(dbg.contains("candidate_count"));
        assert!(dbg.contains("bytes_purged"));
        assert!(!dbg.contains("TerminalSessionId"));
    }
}
