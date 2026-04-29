//! `TerminalSessionManager` and supporting types.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use relayterm_core::ids::{
    ServerProfileId, TerminalSessionAttachmentId, TerminalSessionId, UserId,
};
use relayterm_core::repository::{
    CreateSessionEvent, CreateTerminalSession, CreateTerminalSessionAttachment, RepositoryError,
    SessionEventRepository, TerminalSessionRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::terminal_session::{
    TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};

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

/// Wire-stable message returned alongside a freshly opened WebSocket
/// attachment.
///
/// Pinned in tests so a future helpful rewording is forced through review.
/// MUST disclaim PTY/streaming readiness explicitly: a `session_attached`
/// frame does NOT mean an SSH shell exists or that terminal bytes will
/// flow yet. Mirrors the "stub" message returned by the create route.
pub const STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE: &str =
    "attached to RelayTerm session placeholder; PTY streaming is not implemented yet";

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

/// In-memory runtime entry for a single live WebSocket attachment.
///
/// Created on `attach_session`, removed on `detach_session` (or when the
/// owning session is closed). Carries no socket handle or per-frame state
/// — the WebSocket task owns those — only the bookkeeping the manager
/// needs to map an attachment id back to its session and audit metadata.
///
/// Like [`TerminalSessionRuntime`], this is NOT durable: a backend restart
/// drops every entry. Detach bookkeeping that survived to Postgres
/// (`detached_at`, `last_seen_seq`) is the only persistent surface.
#[derive(Debug, Clone)]
pub struct AttachmentRuntime {
    pub id: TerminalSessionAttachmentId,
    pub session_id: TerminalSessionId,
    pub owner_id: UserId,
    pub attached_at: DateTime<Utc>,
    pub client_info: Option<String>,
    pub remote_addr: Option<String>,
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

/// Input to [`TerminalSessionManager::attach_session`].
///
/// `owner_id` is the caller (used to gate ownership). `client_info` and
/// `remote_addr` are audit-only — recorded on the attachment row and the
/// `attached` lifecycle event, never used for auth.
#[derive(Debug, Clone)]
pub struct AttachSessionRequest {
    pub owner_id: UserId,
    pub session_id: TerminalSessionId,
    pub client_info: Option<String>,
    pub remote_addr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AttachSessionOutcome {
    pub session: TerminalSession,
    pub attachment: TerminalSessionAttachment,
    pub message: &'static str,
}

#[derive(Debug, Clone)]
pub struct DetachSessionOutcome {
    pub session: TerminalSession,
    pub attachment: TerminalSessionAttachment,
    /// `true` when this call observed the attachment as already detached.
    /// Lets the WS handler avoid double-emitting `SessionDetached` frames
    /// when both the client `Detach` message and the socket close path race.
    pub already_detached: bool,
}

#[derive(Debug, Clone)]
pub struct ResizeSessionOutcome {
    pub session: TerminalSession,
    pub cols: u16,
    pub rows: u16,
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

    /// The addressed session exists and is owned by the caller, but it's
    /// already in `closed` state. Maps to a 409 at the API boundary so
    /// the client can tell "no such session" from "session is gone."
    /// Closed-session rejection is the only operation that gets its own
    /// error variant — every other ownership/existence miss collapses to
    /// `NotFound` to preserve the cross-user 404 contract.
    #[error("terminal session is closed")]
    SessionClosed,

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
    /// Live attachments keyed by attachment id. A single session may have
    /// multiple entries here (the future "two clients viewing one shell"
    /// shape) — today the WS handler enforces one at a time, but the
    /// registry is shaped for the eventual expansion.
    attachments: RwLock<HashMap<TerminalSessionAttachmentId, AttachmentRuntime>>,
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
            attachments: RwLock::new(HashMap::new()),
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
        // Drop any live attachments belonging to this session. The DB
        // rows still exist; they just won't be addressable through the
        // registry. The WS handler's own task will observe its socket
        // close (or the SessionClosed frame the route emits) and exit.
        self.attachments
            .write()
            .expect("attachment registry lock poisoned")
            .retain(|_, a| a.session_id != id);

        Ok(CloseTerminalSessionOutcome {
            session: updated,
            already_closed: false,
        })
    }

    /// Attach a client to an existing terminal session.
    ///
    /// Writes a `terminal_session_attachments` row, registers the in-memory
    /// runtime entry, and appends an `attached` `session_event`. Ownership
    /// is gated identically to [`Self::close_session`]: a session id that
    /// doesn't resolve to a row owned by `req.owner_id` collapses to
    /// [`TerminalSessionManagerError::NotFound`], regardless of why.
    /// A session in `closed` state surfaces as
    /// [`TerminalSessionManagerError::SessionClosed`] so the API can map
    /// it to a stable 409 — the row exists but is unusable.
    ///
    /// PTY allocation, byte streaming, and replay-buffer fast-forward are
    /// all NOT implemented in this slice. The caller is responsible for
    /// surfacing the stub-attach scope to the WS client (see
    /// [`STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE`]).
    pub async fn attach_session(
        &self,
        req: AttachSessionRequest,
    ) -> Result<AttachSessionOutcome, TerminalSessionManagerError> {
        let session = self
            .sessions
            .get(req.session_id)
            .await?
            .filter(|s| s.owner_id == req.owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        if session.status == TerminalSessionStatus::Closed {
            return Err(TerminalSessionManagerError::SessionClosed);
        }

        let attachment = self
            .sessions
            .create_attachment(CreateTerminalSessionAttachment {
                session_id: session.id,
                client_info: req.client_info.clone(),
                remote_addr: req.remote_addr.clone(),
            })
            .await?;

        // Append the lifecycle event. If it fails, surface the error so
        // the API returns 5xx instead of leaving an attachment row that
        // never made it into the audit log. The orphan row is sweep-able
        // via close (same shape as the create-time partial-success case).
        self.events
            .create(CreateSessionEvent {
                session_id: session.id,
                kind: SessionEventKind::Attached,
                payload: serde_json::json!({
                    "attachment_id": attachment.id,
                    "client_info": req.client_info,
                    "remote_addr": req.remote_addr,
                    "stub": true,
                }),
            })
            .await?;

        let runtime = AttachmentRuntime {
            id: attachment.id,
            session_id: session.id,
            owner_id: session.owner_id,
            attached_at: attachment.attached_at,
            client_info: req.client_info,
            remote_addr: req.remote_addr,
        };
        self.attachments
            .write()
            .expect("attachment registry lock poisoned")
            .insert(attachment.id, runtime);

        Ok(AttachSessionOutcome {
            session,
            attachment,
            message: STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE,
        })
    }

    /// Mark an attachment detached.
    ///
    /// Idempotent: a second call against the same attachment id returns
    /// `already_detached = true` and does NOT append a second `detached`
    /// event. The repository's COALESCE-on-detached_at write also keeps
    /// the original timestamp + last_seen_seq when a redundant call lands.
    ///
    /// `last_seen_seq` is the resume bookmark — the highest output
    /// sequence number this attachment acknowledged before detaching. The
    /// PTY-bearing slice will populate it; today every call passes `None`.
    pub async fn detach_session(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
        attachment_id: TerminalSessionAttachmentId,
        last_seen_seq: Option<i64>,
    ) -> Result<DetachSessionOutcome, TerminalSessionManagerError> {
        let session = self
            .sessions
            .get(session_id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        let attachment = self
            .sessions
            .get_attachment(attachment_id)
            .await?
            .filter(|a| a.session_id == session.id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        if attachment.detached_at.is_some() {
            // Drop any stale runtime entry so the registry stays in sync
            // with the DB even on the redundant path.
            self.attachments
                .write()
                .expect("attachment registry lock poisoned")
                .remove(&attachment_id);
            return Ok(DetachSessionOutcome {
                session,
                attachment,
                already_detached: true,
            });
        }

        let now = Utc::now();
        self.sessions
            .mark_attachment_detached(attachment_id, now, last_seen_seq)
            .await?;
        self.events
            .create(CreateSessionEvent {
                session_id: session.id,
                kind: SessionEventKind::Detached,
                payload: serde_json::json!({
                    "attachment_id": attachment_id,
                    "last_seen_seq": last_seen_seq,
                }),
            })
            .await?;

        let updated = self
            .sessions
            .get_attachment(attachment_id)
            .await?
            .filter(|a| a.session_id == session.id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        self.attachments
            .write()
            .expect("attachment registry lock poisoned")
            .remove(&attachment_id);

        Ok(DetachSessionOutcome {
            session,
            attachment: updated,
            already_detached: false,
        })
    }

    /// Update the runtime PTY dimensions for a session and append a
    /// `resized` event. Validates dims against the same `1..=4096`
    /// envelope the create route enforces. Does NOT update the
    /// `terminal_sessions.cols`/`rows` columns — those are the create-time
    /// hint; persistent resize wiring belongs to the PTY-bearing slice.
    pub async fn resize_session(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeSessionOutcome, TerminalSessionManagerError> {
        validate_dim("cols", cols)?;
        validate_dim("rows", rows)?;

        let session = self
            .sessions
            .get(session_id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        if session.status == TerminalSessionStatus::Closed {
            return Err(TerminalSessionManagerError::SessionClosed);
        }

        // Update the in-memory hint so `runtime(id)` reflects the latest
        // requested size. Absence of a runtime entry is non-fatal — it
        // means the session row outlived its placeholder (e.g. across a
        // restart). The event still gets written so audit history records
        // the resize.
        {
            let mut guard = self
                .runtimes
                .write()
                .expect("runtime registry lock poisoned");
            if let Some(runtime) = guard.get_mut(&session.id) {
                runtime.cols = cols;
                runtime.rows = rows;
            }
        }

        self.events
            .create(CreateSessionEvent {
                session_id: session.id,
                kind: SessionEventKind::Resized,
                payload: serde_json::json!({
                    "cols": cols,
                    "rows": rows,
                }),
            })
            .await?;

        Ok(ResizeSessionOutcome {
            session,
            cols,
            rows,
        })
    }

    /// Read an attachment runtime entry by id. Returns `None` if the
    /// attachment has already been detached or never existed in this
    /// process's lifetime.
    #[must_use]
    pub fn attachment(&self, id: TerminalSessionAttachmentId) -> Option<AttachmentRuntime> {
        self.attachments
            .read()
            .expect("attachment registry lock poisoned")
            .get(&id)
            .cloned()
    }

    /// Number of live attachment entries. Test-only convenience.
    #[must_use]
    pub fn attachment_count(&self) -> usize {
        self.attachments
            .read()
            .expect("attachment registry lock poisoned")
            .len()
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
