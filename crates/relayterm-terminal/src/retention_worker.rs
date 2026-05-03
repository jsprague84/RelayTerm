//! Stage B periodic retention worker for the durable terminal-recording corpus.
//!
//! This is the managed background task counterpart to the Stage A
//! startup sweep in [`crate::retention`]. The startup sweep runs once
//! synchronously before the listener binds; the periodic worker runs
//! continuously after the listener binds, on the cadence configured by
//! `[terminal_recording.cleanup].sweep_interval_seconds`.
//!
//! ## Lifecycle
//!
//! - The worker is spawned via [`spawn_recording_retention_periodic_worker`]
//!   and returns a [`RetentionPeriodicWorkerHandle`] holding the
//!   [`tokio::task::JoinHandle`]. It is NEVER `tokio::spawn`-and-forget;
//!   the caller MUST keep the handle alive for the duration of the
//!   process and `await` its `shutdown()` after the listener returns.
//! - Shutdown is wired through a `tokio::sync::watch::Receiver<bool>`.
//!   The same change-of-state event that drives `axum::serve`'s
//!   graceful shutdown drives the worker. A clean shutdown causes the
//!   worker loop to break BEFORE its next sweep tick (it does not
//!   interrupt an in-flight sweep — interrupting a per-session purge
//!   would split a Postgres transaction).
//!
//! ## First-tick timing
//!
//! The first periodic tick fires after `interval` has elapsed — NOT
//! immediately on spawn. Rationale: the startup sweep already drained
//! the eligibility set on the same boot, so an immediate periodic tick
//! would be a redundant no-op. The interval-driven tick is the natural
//! cadence after that.
//!
//! ## Concurrency safety
//!
//! When an [`RetentionAdvisoryLock`] is provided, the worker wraps each
//! tick's sweep in `pg_try_advisory_lock(<fixed key>)`. A second backend
//! instance pointing at the same database will fail to acquire the
//! lock on its tick and skip silently (no error, no log spam). When no
//! lock is provided (single-instance deployments, tests) the sweep
//! runs unguarded — the single-task design already prevents
//! concurrency within one process.
//!
//! ## Privacy contract
//!
//! Every operator-side log line is a static category tag plus
//! primitive counts only — never session ids, never repository error
//! text, never any byte material from the recording corpus. The
//! tick log line carries the same shape as the Stage A sweep log;
//! only the cadence prefix differs (see
//! [`crate::retention::SweepCadence`]).

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use relayterm_core::repository::{RepositoryError, TerminalRecordingRepository};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use crate::retention::{
    RecordingRetentionSweepSummary, SweepCadence, repository_error_kind,
    run_recording_retention_sweep,
};

/// Stable Postgres advisory-lock key for the recording-retention sweep.
///
/// Every backend instance that talks to the same database MUST agree on
/// this value so a Stage B periodic tick is exclusive across the
/// fleet. The value is deliberately arbitrary — Postgres advisory locks
/// are namespace-free i64 keys; a clash would only happen if another
/// part of the system independently picked the same number for a
/// different lock domain. Centralising it here keeps the value
/// auditable.
///
/// See `docs/terminal-recording.md` Section 12.7 "Concurrency safety".
pub const RECORDING_RETENTION_ADVISORY_LOCK_KEY: i64 = 0x5254_5252_4554_454e;

/// Future passed to [`RetentionAdvisoryLock::run_with_lock`] to execute
/// while the lock is held. Boxed so the trait stays object-safe.
pub type SweepWork<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Outcome of one [`RetentionAdvisoryLock::run_with_lock`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvisoryLockOutcome {
    /// The lock was acquired and the sweep ran to completion. The
    /// implementation released the lock before returning.
    Acquired,
    /// The lock was held by another process (or another in-process
    /// caller). The sweep did NOT run; the worker logs a quiet
    /// `debug!` and waits for the next tick.
    Skipped,
}

/// Cross-instance advisory lock for the periodic retention sweep.
///
/// The contract is intentionally narrow:
/// 1. The implementation MUST hold a single Postgres connection for the
///    duration of the closure (advisory locks are connection-scoped).
/// 2. The implementation MUST release the lock before returning, on
///    BOTH the acquired-and-ran path and the failure paths.
/// 3. A repository error talking to Postgres returns
///    [`RepositoryError::Database`] with NO `payload` bytes / driver
///    text / session ids. The worker will surface it as a static
///    `error_kind` tag.
#[async_trait]
pub trait RetentionAdvisoryLock: Send + Sync {
    /// Try to acquire the retention advisory lock. On success run
    /// `work` to completion while holding the lock and return
    /// [`AdvisoryLockOutcome::Acquired`]. On contention skip `work`
    /// and return [`AdvisoryLockOutcome::Skipped`].
    async fn run_with_lock<'a>(
        &'a self,
        work: SweepWork<'a>,
    ) -> Result<AdvisoryLockOutcome, RepositoryError>;
}

/// Decide whether the Stage B periodic worker should be spawned, given
/// the operator-supplied cleanup config primitives.
///
/// Returns `Some(config)` only when ALL of:
/// - `cleanup_enabled = true`
/// - `periodic_sweep_enabled = true`
/// - `sweep_interval_seconds > 0`
///
/// Otherwise returns `None` so `apps/backend/src/main.rs` can skip the
/// spawn entirely. This helper is the canonical gating logic — tests
/// pin every disabled branch here so the wiring in `main.rs` stays a
/// thin shim.
#[must_use]
pub fn periodic_worker_config_if_enabled(
    cleanup_enabled: bool,
    periodic_sweep_enabled: bool,
    sweep_interval_seconds: u64,
    retention_days: u32,
    batch_size: u32,
) -> Option<RecordingRetentionPeriodicConfig> {
    if !cleanup_enabled || !periodic_sweep_enabled || sweep_interval_seconds == 0 {
        return None;
    }
    Some(RecordingRetentionPeriodicConfig {
        retention_days,
        batch_size,
        interval: Duration::from_secs(sweep_interval_seconds),
    })
}

/// Configuration for the Stage B periodic retention worker.
///
/// All fields are public-safe primitives; this struct intentionally
/// derives `Debug` because every field is operator-public (no secrets,
/// no session ids, no recording bytes). The interval comes from
/// `cleanup.sweep_interval_seconds`; the validator already bounded it
/// to `60..=604800` (or `0` when periodic is disabled, in which case
/// the worker is not spawned).
#[derive(Debug, Clone, Copy)]
pub struct RecordingRetentionPeriodicConfig {
    pub retention_days: u32,
    pub batch_size: u32,
    pub interval: Duration,
}

/// Handle to a running periodic worker. Held by the application
/// shutdown coordinator; `await` it after the listener returns to
/// guarantee the worker exits cleanly.
pub struct RetentionPeriodicWorkerHandle {
    handle: JoinHandle<()>,
}

impl RetentionPeriodicWorkerHandle {
    /// Wait for the worker loop to exit. Caller is expected to flip
    /// the `shutdown` watch channel BEFORE awaiting; this method does
    /// NOT signal shutdown itself.
    pub async fn shutdown(self) {
        if let Err(err) = self.handle.await {
            warn!(
                category = "retention_worker_join_failed",
                error_kind = if err.is_panic() { "panic" } else { "cancelled" },
                "recording retention periodic worker exited abnormally",
            );
        }
    }
}

/// Spawn the Stage B periodic retention worker. Returns a handle the
/// application shutdown coordinator owns.
///
/// Behaviour:
/// - The first tick fires after `cfg.interval` elapses; the worker
///   does NOT run an immediate sweep on spawn (Stage A startup sweep
///   already covers the boot pass).
/// - On every tick the worker either calls `advisory_lock.run_with_lock(...)`
///   (when provided) or runs the sweep directly. Either way the sweep
///   logic is [`run_recording_retention_sweep`] with cadence
///   [`SweepCadence::Periodic`].
/// - A shutdown signal observed via the `shutdown` watch receiver
///   breaks the loop at the next select. An in-flight sweep is allowed
///   to complete; the worker does NOT abort a per-session purge mid
///   transaction.
/// - The worker is fail-soft: a sweep error or an advisory-lock error
///   is logged with a static category tag and the loop continues to
///   the next tick. Missing one tick is operationally undesirable but
///   not security-relevant (Section 12.7).
#[must_use]
pub fn spawn_recording_retention_periodic_worker(
    repo: Arc<dyn TerminalRecordingRepository>,
    advisory_lock: Option<Arc<dyn RetentionAdvisoryLock>>,
    cfg: RecordingRetentionPeriodicConfig,
    mut shutdown: watch::Receiver<bool>,
) -> RetentionPeriodicWorkerHandle {
    let handle = tokio::spawn(async move {
        // Wait one full interval before the first tick. `interval_at`
        // with a `start = now + interval` instant is the canonical way
        // to do this without consuming an immediate tick we then
        // throw away.
        let start = Instant::now() + cfg.interval;
        let mut ticker = interval_at(start, cfg.interval);
        // If a tick is missed (the previous sweep took longer than
        // the cadence) we delay rather than fire a burst. A
        // thundering-herd is the wrong shape for retention — the
        // backlog will be drained next tick anyway.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        info!(
            interval_seconds = cfg.interval.as_secs(),
            batch_size = cfg.batch_size,
            advisory_lock = advisory_lock.is_some(),
            "recording retention periodic worker started",
        );

        loop {
            tokio::select! {
                // `biased` makes the shutdown branch win when both
                // arms are immediately ready, so a shutdown that
                // races a tick still exits cleanly.
                biased;
                changed = shutdown.changed() => {
                    match changed {
                        Ok(()) => {
                            if *shutdown.borrow() {
                                info!("recording retention periodic worker: shutdown signalled");
                                break;
                            }
                            // Spurious wake-up (channel observed but
                            // value is still `false`): keep looping.
                        }
                        Err(_) => {
                            // The sender was dropped — the application
                            // is tearing down. Treat as a shutdown.
                            info!(
                                "recording retention periodic worker: shutdown channel closed"
                            );
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    let _ = run_one_periodic_tick(
                        Arc::clone(&repo),
                        advisory_lock.clone(),
                        cfg.retention_days,
                        cfg.batch_size,
                    )
                    .await;
                }
            }
        }

        info!("recording retention periodic worker: stopped");
    });

    RetentionPeriodicWorkerHandle { handle }
}

/// Outcome of a single periodic-tick attempt. Used by tests to assert
/// the worker took the right path; the production loop discards the
/// value and relies on the inner sweep's own log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingRetentionTickOutcome {
    /// The sweep ran (advisory lock acquired, or no lock configured).
    Ran(RecordingRetentionSweepSummary),
    /// The advisory lock was held by another process; the tick was
    /// skipped silently.
    Skipped,
    /// Talking to the advisory lock failed; the tick was skipped with
    /// a static-tag warning.
    LockError,
}

/// Run one periodic tick. Public so integration / smoke tests can
/// drive a tick deterministically without spinning the full worker;
/// production code only drives it through
/// [`spawn_recording_retention_periodic_worker`].
pub async fn run_one_periodic_tick(
    repo: Arc<dyn TerminalRecordingRepository>,
    advisory_lock: Option<Arc<dyn RetentionAdvisoryLock>>,
    retention_days: u32,
    batch_size: u32,
) -> RecordingRetentionTickOutcome {
    if let Some(lock) = advisory_lock {
        // `tokio::sync::Mutex` is the right primitive here: we hold
        // the lock across `.await` inside the boxed sweep future
        // (`*slot.lock().await = Some(summary)`), and the lock has to
        // be `Send` to ride inside the `SweepWork<'a>` boxed future.
        // `std::sync::Mutex` would forbid the `.await` while the
        // lock is held; this is the documented exception in
        // AGENTS.md "Critical gotchas" for tokio.
        let summary_slot: Arc<tokio::sync::Mutex<Option<RecordingRetentionSweepSummary>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let slot_for_work = Arc::clone(&summary_slot);
        let work_repo = Arc::clone(&repo);
        let work: SweepWork<'_> = Box::pin(async move {
            let now = Utc::now();
            let summary = run_recording_retention_sweep(
                work_repo,
                retention_days,
                batch_size,
                now,
                SweepCadence::Periodic,
            )
            .await;
            *slot_for_work.lock().await = Some(summary);
        });

        match lock.run_with_lock(work).await {
            Ok(AdvisoryLockOutcome::Acquired) => {
                let summary = summary_slot.lock().await.take().unwrap_or_default();
                RecordingRetentionTickOutcome::Ran(summary)
            }
            Ok(AdvisoryLockOutcome::Skipped) => {
                debug!(
                    category = "retention_sweep_skipped",
                    reason = "advisory_lock_contention",
                    "recording retention periodic sweep: \
                     lock held by another instance, skipping tick",
                );
                RecordingRetentionTickOutcome::Skipped
            }
            Err(err) => {
                warn!(
                    category = "retention_sweep_failed",
                    stage = "advisory_lock",
                    error_kind = repository_error_kind(&err),
                    "recording retention periodic sweep: advisory lock acquisition failed; \
                     skipping tick",
                );
                RecordingRetentionTickOutcome::LockError
            }
        }
    } else {
        let now = Utc::now();
        let summary = run_recording_retention_sweep(
            repo,
            retention_days,
            batch_size,
            now,
            SweepCadence::Periodic,
        )
        .await;
        RecordingRetentionTickOutcome::Ran(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use relayterm_core::ids::TerminalSessionId;
    use relayterm_core::repository::{
        CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, PurgeRecordingForRetention,
    };
    use relayterm_core::terminal_recording::{
        PurgedRecordingSummary, TerminalRecordingChunk, TerminalRecordingMarker,
        TerminalRecordingMetadata,
    };
    use std::sync::Mutex;
    use tokio::sync::watch;
    use tokio::time::{Duration, advance};

    /// Minimal repo fake whose only job is to count how many ticks
    /// invoked the sweep. The sweep itself is exercised by
    /// `retention.rs::tests`; here we only assert that a tick happened
    /// (or didn't).
    #[derive(Default)]
    struct CountingRepo {
        list_calls: Mutex<u32>,
        eligible_per_call: Mutex<Vec<Vec<TerminalSessionId>>>,
    }

    impl CountingRepo {
        fn list_calls(&self) -> u32 {
            *self.list_calls.lock().unwrap()
        }

        fn push_batch(&self, ids: Vec<TerminalSessionId>) {
            self.eligible_per_call.lock().unwrap().push(ids);
        }
    }

    #[async_trait]
    impl TerminalRecordingRepository for CountingRepo {
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
            Ok(Some(PurgedRecordingSummary {
                terminal_session_id: input.terminal_session_id,
                chunk_count: 1,
                marker_count: 1,
                bytes_purged: 8,
                closed_at: input.now - chrono::Duration::days(31),
                purged_at: input.now,
            }))
        }
        async fn list_eligible_for_retention(
            &self,
            _retention_days: u32,
            _now: DateTime<Utc>,
            _limit: u32,
        ) -> Result<Vec<TerminalSessionId>, RepositoryError> {
            *self.list_calls.lock().unwrap() += 1;
            let mut batches = self.eligible_per_call.lock().unwrap();
            if batches.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(batches.remove(0))
            }
        }
    }

    /// Lock fake that always grants the lock.
    struct GrantingLock {
        calls: Mutex<u32>,
    }

    impl Default for GrantingLock {
        fn default() -> Self {
            Self {
                calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl RetentionAdvisoryLock for GrantingLock {
        async fn run_with_lock<'a>(
            &'a self,
            work: SweepWork<'a>,
        ) -> Result<AdvisoryLockOutcome, RepositoryError> {
            *self.calls.lock().unwrap() += 1;
            work.await;
            Ok(AdvisoryLockOutcome::Acquired)
        }
    }

    /// Lock fake that always reports contention.
    struct ContendedLock {
        calls: Mutex<u32>,
    }

    impl Default for ContendedLock {
        fn default() -> Self {
            Self {
                calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl RetentionAdvisoryLock for ContendedLock {
        async fn run_with_lock<'a>(
            &'a self,
            _work: SweepWork<'a>,
        ) -> Result<AdvisoryLockOutcome, RepositoryError> {
            *self.calls.lock().unwrap() += 1;
            Ok(AdvisoryLockOutcome::Skipped)
        }
    }

    /// Lock fake that errors out talking to Postgres.
    struct ErroringLock;

    #[async_trait]
    impl RetentionAdvisoryLock for ErroringLock {
        async fn run_with_lock<'a>(
            &'a self,
            _work: SweepWork<'a>,
        ) -> Result<AdvisoryLockOutcome, RepositoryError> {
            Err(RepositoryError::Database("synthetic lock error".into()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn first_tick_fires_after_full_interval_then_continues_on_cadence() {
        let repo_concrete = Arc::new(CountingRepo::default());
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let cfg = RecordingRetentionPeriodicConfig {
            retention_days: 30,
            batch_size: 100,
            interval: Duration::from_secs(60),
        };
        let handle = spawn_recording_retention_periodic_worker(repo, None, cfg, shutdown_rx);
        // Let the worker capture `Instant::now()` BEFORE we start
        // advancing time. Without this yield the worker's reference
        // instant slides forward with our `advance` calls and the
        // first tick lands a full interval later than expected.
        tokio::task::yield_now().await;

        // Before the first interval, no tick should have fired.
        advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            repo_concrete.list_calls(),
            0,
            "no tick before interval elapses"
        );

        // First tick at t = 60s.
        advance(Duration::from_secs(31)).await;
        // Yield enough times for the worker to: wake from select,
        // run the sweep future to completion, and re-park on the
        // next select.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(repo_concrete.list_calls(), 1, "first tick after interval");

        // Second tick at t = 120s.
        advance(Duration::from_secs(60)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(repo_concrete.list_calls(), 2, "second tick on cadence");

        // Signal shutdown and join.
        shutdown_tx.send(true).expect("send shutdown");
        handle.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_breaks_loop_without_running_tick() {
        let repo_concrete = Arc::new(CountingRepo::default());
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let cfg = RecordingRetentionPeriodicConfig {
            retention_days: 30,
            batch_size: 100,
            interval: Duration::from_secs(120),
        };
        let handle = spawn_recording_retention_periodic_worker(repo, None, cfg, shutdown_rx);
        // Let the worker capture its reference instant first.
        tokio::task::yield_now().await;

        // Advance past most of the interval but before it fires.
        advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        // Signal shutdown BEFORE the first tick fires.
        shutdown_tx.send(true).expect("send shutdown");
        handle.shutdown().await;

        assert_eq!(
            repo_concrete.list_calls(),
            0,
            "shutdown wins the race against the next tick",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn truncated_batch_does_not_trigger_immediate_extra_tick() {
        // The interval is the only pacing — `MissedTickBehavior::Delay`
        // means a slow sweep does not produce a burst, AND a tick that
        // returns `batch_truncated = true` does NOT cause the worker
        // to fire an immediate follow-up. The remaining backlog waits
        // for the next interval.
        let repo_concrete = Arc::new(CountingRepo::default());
        // Pre-load TWO non-empty batches so a truncated tick has
        // visible follow-up work; the assertion is that the second
        // tick fires AFTER the full interval, not immediately.
        repo_concrete.push_batch(vec![TerminalSessionId::new()]);
        repo_concrete.push_batch(vec![TerminalSessionId::new()]);
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let cfg = RecordingRetentionPeriodicConfig {
            retention_days: 30,
            batch_size: 1, // batch_size = 1 so the first tick truncates
            interval: Duration::from_secs(60),
        };
        let handle = spawn_recording_retention_periodic_worker(repo, None, cfg, shutdown_rx);
        tokio::task::yield_now().await;

        // First tick fires after 60s.
        advance(Duration::from_secs(61)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(repo_concrete.list_calls(), 1, "first tick fired");

        // Even though the first tick truncated, the second tick must
        // wait for another full interval — no immediate burst.
        advance(Duration::from_secs(30)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            repo_concrete.list_calls(),
            1,
            "no immediate follow-up tick after truncated batch",
        );

        advance(Duration::from_secs(31)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            repo_concrete.list_calls(),
            2,
            "second tick fired on cadence"
        );

        shutdown_tx.send(true).expect("send shutdown");
        handle.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_shutdown_sender_stops_worker() {
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::new(CountingRepo::default());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let cfg = RecordingRetentionPeriodicConfig {
            retention_days: 30,
            batch_size: 100,
            interval: Duration::from_secs(60),
        };
        let handle = spawn_recording_retention_periodic_worker(repo, None, cfg, shutdown_rx);

        drop(shutdown_tx);
        // The worker should observe the closed channel and exit
        // without waiting for the next tick.
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn periodic_tick_with_granting_lock_runs_sweep() {
        let repo_concrete = Arc::new(CountingRepo::default());
        let id = TerminalSessionId::new();
        repo_concrete.push_batch(vec![id]);
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;
        let lock: Arc<dyn RetentionAdvisoryLock> = Arc::new(GrantingLock::default());
        let outcome = run_one_periodic_tick(repo, Some(lock), 30, 100).await;
        match outcome {
            RecordingRetentionTickOutcome::Ran(summary) => {
                assert_eq!(summary.candidate_count, 1);
                assert_eq!(summary.purged_sessions, 1);
            }
            other => panic!("expected Ran, got {other:?}"),
        }
        assert_eq!(repo_concrete.list_calls(), 1);
    }

    #[tokio::test]
    async fn periodic_tick_with_contended_lock_skips_sweep() {
        let repo_concrete = Arc::new(CountingRepo::default());
        let id = TerminalSessionId::new();
        repo_concrete.push_batch(vec![id]);
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;
        let lock: Arc<dyn RetentionAdvisoryLock> = Arc::new(ContendedLock::default());
        let outcome = run_one_periodic_tick(repo, Some(lock), 30, 100).await;
        assert_eq!(outcome, RecordingRetentionTickOutcome::Skipped);
        assert_eq!(
            repo_concrete.list_calls(),
            0,
            "sweep must not run under contention"
        );
    }

    #[tokio::test]
    async fn periodic_tick_with_lock_error_skips_sweep() {
        let repo_concrete = Arc::new(CountingRepo::default());
        let id = TerminalSessionId::new();
        repo_concrete.push_batch(vec![id]);
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;
        let lock: Arc<dyn RetentionAdvisoryLock> = Arc::new(ErroringLock);
        let outcome = run_one_periodic_tick(repo, Some(lock), 30, 100).await;
        assert_eq!(outcome, RecordingRetentionTickOutcome::LockError);
        assert_eq!(repo_concrete.list_calls(), 0);
    }

    #[test]
    fn periodic_worker_config_is_some_only_when_all_gates_pass() {
        // All gates green → Some.
        assert!(periodic_worker_config_if_enabled(true, true, 60, 30, 100).is_some());

        // cleanup.enabled = false dominates.
        assert!(periodic_worker_config_if_enabled(false, true, 60, 30, 100).is_none());
        // Even with periodic enabled, cleanup off blocks the worker.
        assert!(periodic_worker_config_if_enabled(false, true, 600, 30, 100).is_none());

        // periodic_sweep_enabled = false dominates.
        assert!(periodic_worker_config_if_enabled(true, false, 60, 30, 100).is_none());

        // sweep_interval_seconds = 0 dominates (the "no periodic schedule" sentinel).
        assert!(periodic_worker_config_if_enabled(true, true, 0, 30, 100).is_none());
    }

    #[test]
    fn periodic_worker_config_carries_through_runtime_values() {
        let cfg =
            periodic_worker_config_if_enabled(true, true, 120, 14, 50).expect("all gates pass");
        assert_eq!(cfg.retention_days, 14);
        assert_eq!(cfg.batch_size, 50);
        assert_eq!(cfg.interval, Duration::from_secs(120));
    }

    #[tokio::test]
    async fn periodic_tick_without_lock_runs_sweep() {
        let repo_concrete = Arc::new(CountingRepo::default());
        let id = TerminalSessionId::new();
        repo_concrete.push_batch(vec![id]);
        let repo: Arc<dyn TerminalRecordingRepository> = Arc::clone(&repo_concrete) as _;
        let outcome = run_one_periodic_tick(repo, None, 30, 100).await;
        match outcome {
            RecordingRetentionTickOutcome::Ran(summary) => {
                assert_eq!(summary.candidate_count, 1);
            }
            other => panic!("expected Ran, got {other:?}"),
        }
    }
}
