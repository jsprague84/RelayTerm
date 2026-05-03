//! Postgres advisory-lock implementation for the recording-retention
//! periodic worker.
//!
//! Postgres advisory locks are connection-scoped: the lock is released
//! when the connection that took it is returned to the pool, dropped,
//! or explicitly unlocked. This implementation acquires a single
//! `PoolConnection<Postgres>`, takes the lock on it, runs the caller's
//! sweep future on the same task (the connection only needs to live
//! for the duration of the await), and releases the lock before
//! returning the connection.
//!
//! Privacy contract: the lock key is a public constant; failure modes
//! map to [`RepositoryError::Database`] with a short, generic message.
//! No driver text, no session ids, no recording bytes.

use async_trait::async_trait;
use relayterm_core::repository::RepositoryError;
use relayterm_terminal::{
    AdvisoryLockOutcome, RECORDING_RETENTION_ADVISORY_LOCK_KEY, RetentionAdvisoryLock, SweepWork,
};
use sqlx::PgPool;
use tracing::warn;

/// Postgres-backed advisory lock for the periodic retention worker.
///
/// Single-deployment safety: the worker does NOT need a lock when only
/// one backend instance is running. Multi-instance deployments
/// configure this lock so a periodic tick is exclusive across the
/// fleet.
#[derive(Debug, Clone)]
pub struct PgRetentionAdvisoryLock {
    pool: PgPool,
    key: i64,
}

impl PgRetentionAdvisoryLock {
    /// Construct with the canonical retention lock key.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            key: RECORDING_RETENTION_ADVISORY_LOCK_KEY,
        }
    }

    /// Construct with a custom key. Tests use this to exercise
    /// contention semantics without colliding with another integration
    /// test running in parallel.
    #[must_use]
    pub fn with_key(pool: PgPool, key: i64) -> Self {
        Self { pool, key }
    }
}

#[async_trait]
impl RetentionAdvisoryLock for PgRetentionAdvisoryLock {
    async fn run_with_lock<'a>(
        &'a self,
        work: SweepWork<'a>,
    ) -> Result<AdvisoryLockOutcome, RepositoryError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| RepositoryError::Database(map_lock_error("acquire", &e)))?;

        // `pg_try_advisory_lock(int8)` returns true on acquire,
        // false on contention. Connection-scoped: the lock is held
        // until we either call `pg_advisory_unlock` on the same
        // connection or the connection closes.
        let acquired: (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock($1)")
            .bind(self.key)
            .fetch_one(conn.as_mut())
            .await
            .map_err(|e| RepositoryError::Database(map_lock_error("try_lock", &e)))?;

        if !acquired.0 {
            // Drop the connection back into the pool. Postgres will
            // not have anything to release since `pg_try_advisory_lock`
            // returned false.
            return Ok(AdvisoryLockOutcome::Skipped);
        }

        // Hold the lock while the sweep runs. We MUST release the
        // lock on the same connection regardless of how the sweep
        // future returns (it returns `()` and never errors, but a
        // panic during the sweep would otherwise leave the lock held
        // until connection close — `PoolConnection` drop closes the
        // connection on panic, which Postgres treats as session
        // close, which releases all advisory locks. The explicit
        // unlock on the success path is the cheap belt-and-suspenders
        // alternative.).
        work.await;

        // Best-effort release. A failure here logs a static-tag
        // warning and is surfaced as a database error so the worker
        // can record the tick as `LockError`. The lock will still be
        // released when the connection returns to the pool and is
        // eventually closed; we surface the failure so an operator
        // can correlate it with whatever DB hiccup caused the
        // unlock query to fail.
        match sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(self.key)
            .execute(conn.as_mut())
            .await
        {
            Ok(_) => Ok(AdvisoryLockOutcome::Acquired),
            Err(e) => {
                warn!(
                    category = "retention_sweep_failed",
                    stage = "advisory_unlock",
                    error_kind = "unlock_failed",
                    "recording retention advisory unlock failed; \
                     connection close will release the lock",
                );
                Err(RepositoryError::Database(map_lock_error("unlock", &e)))
            }
        }
    }
}

/// Map a SQLx error into a short, generic operator-side string.
///
/// Deliberately does NOT include the raw `sqlx::Error` message — that
/// can carry driver text (constraint names are fine; arbitrary error
/// strings are not). The returned string is a stable category-style
/// tag.
fn map_lock_error(stage: &'static str, _err: &sqlx::Error) -> String {
    format!("recording retention advisory lock {stage} failed")
}
