//! `TerminalSessionManager` and supporting types.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use relayterm_core::ids::{ServerProfileId, TerminalSessionId, UserId};
use relayterm_core::repository::{
    CreateSessionEvent, CreateTerminalSession, RepositoryError, SessionEventRepository,
    TerminalSessionRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::terminal_session::{TerminalSession, TerminalSessionStatus};

/// Bounds for `cols`/`rows` requested at session creation. Mirrored by the
/// `terminal_sessions_cols_chk` / `_rows_chk` migration so the API rejects
/// out-of-range values BEFORE a row insert would otherwise round-trip a
/// constraint error.
const MIN_DIM: u16 = 1;
const MAX_DIM: u16 = 4096;

/// Wire-stable message returned alongside a freshly created session.
///
/// Pinned in tests so a future helpful rewording is forced through review.
/// MUST disclaim PTY readiness explicitly: a green response from
/// `POST /terminal-sessions` does NOT mean an SSH channel was opened or a
/// shell can be reached.
pub const STUB_PTY_NOT_IMPLEMENTED_MESSAGE: &str =
    "session metadata created; PTY startup is not implemented yet";

/// In-memory status for a runtime registry entry.
///
/// Distinct from [`TerminalSessionStatus`] (the persisted enum) so the
/// runtime can carry states that are meaningless at rest — e.g. a future
/// `Spawning` while `russh::Channel::request_pty` is in flight. For the
/// PTY-less placeholder slice the registry only ever holds `Starting`;
/// `close_session` removes the entry rather than transitioning to a
/// `Closed` runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSessionStatus {
    /// Placeholder created at metadata-write time. No PTY yet.
    Starting,
}

/// In-memory runtime entry for a terminal session.
///
/// Holds NO `russh::Channel`, NO PTY descriptor, NO replay ring buffer.
/// Those land in later slices. Today it's a marker that lets us exercise
/// the lifecycle surface end-to-end without SSH.
#[derive(Debug, Clone)]
pub struct TerminalSessionRuntime {
    pub id: TerminalSessionId,
    pub owner_id: UserId,
    pub server_profile_id: ServerProfileId,
    pub status: RuntimeSessionStatus,
    pub created_at: DateTime<Utc>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone)]
pub struct CreateTerminalSessionRequest {
    pub owner_id: UserId,
    pub server_profile_id: ServerProfileId,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone)]
pub struct CreateTerminalSessionOutcome {
    pub session: TerminalSession,
    pub message: &'static str,
}

#[derive(Debug, Clone)]
pub struct CloseTerminalSessionOutcome {
    pub session: TerminalSession,
    /// `true` when the session was already in `Closed` at call time. The
    /// caller still gets the row back; idempotent close is a non-error.
    pub already_closed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalSessionManagerError {
    /// A `cols` or `rows` value was outside the permitted range.
    /// `field` and `message` are operator-facing — the API maps this to
    /// a 400 with the wrapped message.
    #[error("invalid {field}: {message}")]
    InvalidDimensions {
        field: &'static str,
        message: String,
    },

    /// The addressed session does not exist OR is not owned by the caller.
    /// The two are intentionally indistinguishable so an attacker can't
    /// probe for cross-user session existence by id.
    #[error("terminal session not found")]
    NotFound,

    /// Underlying repository failure. Map at the API boundary —
    /// `RepositoryError::Database` collapses to a 500 with the static
    /// `internal error` message.
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

/// Single owner of terminal-session lifecycle. Cheap to clone (everything
/// behind `Arc`) so handlers can hold it via `AppState`.
pub struct TerminalSessionManager {
    sessions: Arc<dyn TerminalSessionRepository>,
    events: Arc<dyn SessionEventRepository>,
    runtimes: RwLock<HashMap<TerminalSessionId, TerminalSessionRuntime>>,
}

impl TerminalSessionManager {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn TerminalSessionRepository>,
        events: Arc<dyn SessionEventRepository>,
    ) -> Self {
        Self {
            sessions,
            events,
            runtimes: RwLock::new(HashMap::new()),
        }
    }

    /// Create a metadata row in `Starting` status, append the `created`
    /// session event, and register an in-memory runtime placeholder.
    ///
    /// This call does NOT open an SSH channel, allocate a PTY, or stream
    /// any terminal data. The returned [`CreateTerminalSessionOutcome`]
    /// carries the static `STUB_PTY_NOT_IMPLEMENTED_MESSAGE` to make the
    /// stub nature explicit on the wire.
    pub async fn create_session(
        &self,
        req: CreateTerminalSessionRequest,
    ) -> Result<CreateTerminalSessionOutcome, TerminalSessionManagerError> {
        validate_dim("cols", req.cols)?;
        validate_dim("rows", req.rows)?;

        let session = self
            .sessions
            .create(CreateTerminalSession {
                owner_id: req.owner_id,
                server_profile_id: req.server_profile_id,
                status: TerminalSessionStatus::Starting,
                cols: req.cols,
                rows: req.rows,
            })
            .await?;

        // Append the lifecycle event. If it fails, surface the error: a
        // metadata row without its `created` event is an audit gap and
        // we want the caller to see the failure rather than a partial
        // success. The DB row stays — operator can sweep it via close.
        self.events
            .create(CreateSessionEvent {
                session_id: session.id,
                kind: SessionEventKind::Created,
                payload: serde_json::json!({
                    "cols": session.cols,
                    "rows": session.rows,
                    "stub": true,
                }),
            })
            .await?;

        let runtime = TerminalSessionRuntime {
            id: session.id,
            owner_id: session.owner_id,
            server_profile_id: session.server_profile_id,
            status: RuntimeSessionStatus::Starting,
            created_at: session.created_at,
            cols: session.cols,
            rows: session.rows,
        };
        self.runtimes
            .write()
            .expect("runtime registry lock poisoned")
            .insert(session.id, runtime);

        Ok(CreateTerminalSessionOutcome {
            session,
            message: STUB_PTY_NOT_IMPLEMENTED_MESSAGE,
        })
    }

    /// Mark a session closed.
    ///
    /// Ownership-gated: a session whose `owner_id` doesn't match the
    /// caller is treated as if it doesn't exist (`NotFound`), matching
    /// the cross-user 404 contract used elsewhere in the API.
    ///
    /// Idempotent: closing an already-closed session returns
    /// `already_closed = true` rather than an error, so the API can map
    /// double-close requests to a stable 200/204 response without the
    /// caller having to inspect the prior state.
    pub async fn close_session(
        &self,
        id: TerminalSessionId,
        owner_id: UserId,
    ) -> Result<CloseTerminalSessionOutcome, TerminalSessionManagerError> {
        let session = self
            .sessions
            .get(id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        if session.status == TerminalSessionStatus::Closed {
            // Drop any stale runtime entry that survived a partial close.
            self.runtimes
                .write()
                .expect("runtime registry lock poisoned")
                .remove(&id);
            return Ok(CloseTerminalSessionOutcome {
                session,
                already_closed: true,
            });
        }

        let now = Utc::now();
        self.sessions
            .set_status(id, TerminalSessionStatus::Closed, Some(now))
            .await?;
        self.events
            .create(CreateSessionEvent {
                session_id: id,
                kind: SessionEventKind::Closed,
                payload: serde_json::json!({"reason": "client_requested"}),
            })
            .await?;

        // Re-read so the response carries the authoritative `closed_at`
        // / `last_seen_at` the database stamped in `set_status`.
        // Re-filter on `owner_id` for defense-in-depth: the initial fetch
        // already gated ownership, but if a future caller reuses this
        // method from a privileged context the gate at the top of the
        // function could be the only check, and a missing ownership
        // filter on the re-read would silently expose foreign rows.
        let updated = self
            .sessions
            .get(id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        self.runtimes
            .write()
            .expect("runtime registry lock poisoned")
            .remove(&id);

        Ok(CloseTerminalSessionOutcome {
            session: updated,
            already_closed: false,
        })
    }

    /// Read the current runtime entry, if any. Returns a snapshot — the
    /// caller is free to drop the result without holding the lock.
    ///
    /// Absence does NOT mean the session is gone: a metadata row can
    /// outlive its runtime entry across a backend restart. Treat
    /// `runtime(id) == None` as "no live placeholder" only.
    #[must_use]
    pub fn runtime(&self, id: TerminalSessionId) -> Option<TerminalSessionRuntime> {
        self.runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .get(&id)
            .cloned()
    }

    /// Number of live runtime entries. Test-only convenience; production
    /// code should not depend on this for correctness.
    #[must_use]
    pub fn runtime_count(&self) -> usize {
        self.runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .len()
    }
}

fn validate_dim(field: &'static str, value: u16) -> Result<(), TerminalSessionManagerError> {
    if !(MIN_DIM..=MAX_DIM).contains(&value) {
        return Err(TerminalSessionManagerError::InvalidDimensions {
            field,
            message: format!("expected {MIN_DIM}..={MAX_DIM}, got {value}"),
        });
    }
    Ok(())
}
