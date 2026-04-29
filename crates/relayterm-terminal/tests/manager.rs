//! Unit tests for `TerminalSessionManager` against an in-memory fake of
//! the repository traits. These exercise the manager's own contracts:
//! the runtime registry, idempotent close, ownership gating, and the
//! lifecycle event log. Postgres-backed integration tests live in
//! `relayterm-api`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::{
    ServerProfileId, SessionEventId, TerminalSessionAttachmentId, TerminalSessionId, UserId,
};
use relayterm_core::repository::{
    CreateSessionEvent, CreateTerminalSession, CreateTerminalSessionAttachment, RepositoryError,
    SessionEventRepository, TerminalSessionRepository,
};
use relayterm_core::session_event::{SessionEvent, SessionEventKind};
use relayterm_core::terminal_session::{
    TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use relayterm_ssh::{ClosedReason, SshPtyError, SshPtyEvent, SshPtyHandle, SshPtyStart};
use relayterm_terminal::{
    AttachSessionRequest, CreateTerminalSessionRequest, LIVE_PTY_ATTACH_MESSAGE,
    RuntimeSessionStatus, STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE,
    STUB_PTY_NOT_IMPLEMENTED_MESSAGE, TerminalSessionManager, TerminalSessionManagerError,
};

#[derive(Default)]
struct InMemoryStores {
    sessions: HashMap<TerminalSessionId, TerminalSession>,
    events: Vec<SessionEvent>,
    attachments: HashMap<TerminalSessionAttachmentId, TerminalSessionAttachment>,
}

#[derive(Clone, Default)]
struct InMemoryRepo {
    inner: Arc<Mutex<InMemoryStores>>,
}

impl InMemoryRepo {
    fn snapshot_events(&self) -> Vec<SessionEvent> {
        self.inner.lock().unwrap().events.clone()
    }

    fn snapshot_session(&self, id: TerminalSessionId) -> Option<TerminalSession> {
        self.inner.lock().unwrap().sessions.get(&id).cloned()
    }

    fn snapshot_attachment(
        &self,
        id: TerminalSessionAttachmentId,
    ) -> Option<TerminalSessionAttachment> {
        self.inner.lock().unwrap().attachments.get(&id).cloned()
    }

    fn force_close(&self, id: TerminalSessionId) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(s) = guard.sessions.get_mut(&id) {
            s.status = TerminalSessionStatus::Closed;
            s.closed_at = Some(Utc::now());
        }
    }
}

#[async_trait]
impl TerminalSessionRepository for InMemoryRepo {
    async fn create(
        &self,
        input: CreateTerminalSession,
    ) -> Result<TerminalSession, RepositoryError> {
        let now = Utc::now();
        let session = TerminalSession {
            id: TerminalSessionId::new(),
            owner_id: input.owner_id,
            server_profile_id: input.server_profile_id,
            status: input.status,
            cols: input.cols,
            rows: input.rows,
            created_at: now,
            last_seen_at: now,
            closed_at: None,
        };
        self.inner
            .lock()
            .unwrap()
            .sessions
            .insert(session.id, session.clone());
        Ok(session)
    }

    async fn get(&self, id: TerminalSessionId) -> Result<Option<TerminalSession>, RepositoryError> {
        Ok(self.inner.lock().unwrap().sessions.get(&id).cloned())
    }

    async fn list_for_user(
        &self,
        owner_id: UserId,
    ) -> Result<Vec<TerminalSession>, RepositoryError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .sessions
            .values()
            .filter(|s| s.owner_id == owner_id)
            .cloned()
            .collect())
    }

    async fn set_status(
        &self,
        id: TerminalSessionId,
        status: TerminalSessionStatus,
        closed_at: Option<DateTime<Utc>>,
    ) -> Result<(), RepositoryError> {
        let mut guard = self.inner.lock().unwrap();
        let row = guard
            .sessions
            .get_mut(&id)
            .ok_or(RepositoryError::NotFound {
                entity: "terminal_session",
            })?;
        row.status = status;
        row.closed_at = closed_at;
        row.last_seen_at = Utc::now();
        Ok(())
    }

    async fn create_attachment(
        &self,
        input: CreateTerminalSessionAttachment,
    ) -> Result<TerminalSessionAttachment, RepositoryError> {
        let now = Utc::now();
        let attachment = TerminalSessionAttachment {
            id: TerminalSessionAttachmentId::new(),
            session_id: input.session_id,
            attached_at: now,
            detached_at: None,
            client_info: input.client_info,
            remote_addr: input.remote_addr,
            last_seen_seq: None,
        };
        self.inner
            .lock()
            .unwrap()
            .attachments
            .insert(attachment.id, attachment.clone());
        Ok(attachment)
    }

    async fn list_attachments(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Vec<TerminalSessionAttachment>, RepositoryError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .attachments
            .values()
            .filter(|a| a.session_id == session_id)
            .cloned()
            .collect())
    }

    async fn get_attachment(
        &self,
        id: TerminalSessionAttachmentId,
    ) -> Result<Option<TerminalSessionAttachment>, RepositoryError> {
        Ok(self.inner.lock().unwrap().attachments.get(&id).cloned())
    }

    async fn mark_attachment_detached(
        &self,
        id: TerminalSessionAttachmentId,
        detached_at: DateTime<Utc>,
        last_seen_seq: Option<i64>,
    ) -> Result<(), RepositoryError> {
        let mut guard = self.inner.lock().unwrap();
        let row = guard
            .attachments
            .get_mut(&id)
            .ok_or(RepositoryError::NotFound {
                entity: "terminal_session_attachment",
            })?;
        // Mirror the SQL COALESCE: only stamp on the first detach.
        if row.detached_at.is_none() {
            row.detached_at = Some(detached_at);
            row.last_seen_seq = last_seen_seq;
        }
        Ok(())
    }
}

#[async_trait]
impl SessionEventRepository for InMemoryRepo {
    async fn create(&self, input: CreateSessionEvent) -> Result<SessionEvent, RepositoryError> {
        let event = SessionEvent {
            id: SessionEventId::new(),
            session_id: input.session_id,
            kind: input.kind,
            payload: input.payload,
            recorded_at: Utc::now(),
        };
        self.inner.lock().unwrap().events.push(event.clone());
        Ok(event)
    }

    async fn list_for_session(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Vec<SessionEvent>, RepositoryError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|e| e.session_id == session_id)
            .cloned()
            .collect())
    }

    async fn get(&self, id: SessionEventId) -> Result<Option<SessionEvent>, RepositoryError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .events
            .iter()
            .find(|e| e.id == id)
            .cloned())
    }
}

fn build_manager() -> (TerminalSessionManager, InMemoryRepo) {
    let repo = InMemoryRepo::default();
    let mgr = TerminalSessionManager::new(
        Arc::new(repo.clone()) as Arc<dyn TerminalSessionRepository>,
        Arc::new(repo.clone()) as Arc<dyn SessionEventRepository>,
    );
    (mgr, repo)
}

fn req(owner: UserId) -> CreateTerminalSessionRequest {
    CreateTerminalSessionRequest {
        owner_id: owner,
        server_profile_id: ServerProfileId::new(),
        cols: 120,
        rows: 30,
    }
}

#[tokio::test]
async fn create_session_writes_row_event_and_runtime_placeholder() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();

    let outcome = mgr.create_session(req(owner)).await.expect("create");
    assert_eq!(outcome.session.status, TerminalSessionStatus::Starting);
    assert_eq!(outcome.session.owner_id, owner);
    assert_eq!(outcome.session.cols, 120);
    assert_eq!(outcome.session.rows, 30);
    assert_eq!(outcome.message, STUB_PTY_NOT_IMPLEMENTED_MESSAGE);

    let runtime = mgr.runtime(outcome.session.id).expect("runtime registered");
    assert_eq!(runtime.id, outcome.session.id);
    assert_eq!(runtime.owner_id, owner);
    assert_eq!(runtime.status, RuntimeSessionStatus::Starting);
    assert_eq!(mgr.runtime_count(), 1);

    let events = repo.snapshot_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionEventKind::Created);
    assert_eq!(events[0].session_id, outcome.session.id);
}

#[tokio::test]
async fn create_session_rejects_zero_dimensions() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();

    for (cols, rows, expected_field) in [(0u16, 30u16, "cols"), (120, 0, "rows")] {
        let mut r = req(owner);
        r.cols = cols;
        r.rows = rows;
        let err = mgr.create_session(r).await.unwrap_err();
        match err {
            TerminalSessionManagerError::InvalidDimensions { field, .. } => {
                assert_eq!(field, expected_field);
            }
            other => panic!("expected InvalidDimensions, got {other:?}"),
        }
    }
    assert_eq!(mgr.runtime_count(), 0);
    assert!(repo.snapshot_events().is_empty());
}

#[tokio::test]
async fn create_session_rejects_oversized_dimensions() {
    let (mgr, _) = build_manager();
    let mut r = req(UserId::new());
    r.cols = 5_000;
    let err = mgr.create_session(r).await.unwrap_err();
    assert!(
        matches!(
            err,
            TerminalSessionManagerError::InvalidDimensions { field: "cols", .. }
        ),
        "expected cols InvalidDimensions, got {err:?}",
    );
}

#[tokio::test]
async fn close_session_marks_closed_writes_event_and_drops_runtime() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();

    let created = mgr.create_session(req(owner)).await.unwrap();
    let outcome = mgr.close_session(created.session.id, owner).await.unwrap();

    assert_eq!(outcome.session.status, TerminalSessionStatus::Closed);
    assert!(outcome.session.closed_at.is_some());
    assert!(!outcome.already_closed);
    assert!(mgr.runtime(created.session.id).is_none());
    assert_eq!(mgr.runtime_count(), 0);

    let events = repo.snapshot_events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, SessionEventKind::Created);
    assert_eq!(events[1].kind, SessionEventKind::Closed);
}

#[tokio::test]
async fn close_session_is_idempotent_for_already_closed_row() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();

    let created = mgr.create_session(req(owner)).await.unwrap();
    let first = mgr.close_session(created.session.id, owner).await.unwrap();
    assert!(!first.already_closed);

    // Second close: same row, no extra Closed event written, already_closed=true.
    let second = mgr.close_session(created.session.id, owner).await.unwrap();
    assert!(second.already_closed);
    assert_eq!(second.session.status, TerminalSessionStatus::Closed);

    let closed_events = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed_events, 1,
        "second close must NOT append another Closed event",
    );
}

#[tokio::test]
async fn close_session_unknown_id_returns_not_found() {
    let (mgr, _) = build_manager();
    let err = mgr
        .close_session(TerminalSessionId::new(), UserId::new())
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::NotFound));
}

#[tokio::test]
async fn close_session_foreign_owner_returns_not_found() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let stranger = UserId::new();

    let created = mgr.create_session(req(owner)).await.unwrap();

    let err = mgr
        .close_session(created.session.id, stranger)
        .await
        .unwrap_err();
    assert!(
        matches!(err, TerminalSessionManagerError::NotFound),
        "foreign-owner close must collapse to NotFound, got {err:?}",
    );

    // Row was not mutated, runtime entry still present.
    let row = repo.snapshot_session(created.session.id).unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Starting);
    assert!(mgr.runtime(created.session.id).is_some());
}

#[tokio::test]
async fn create_event_payload_carries_stub_marker() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let _ = mgr.create_session(req(owner)).await.unwrap();
    let events = repo.snapshot_events();
    let payload = &events[0].payload;
    assert_eq!(payload["stub"], serde_json::Value::Bool(true));
    assert_eq!(payload["cols"], serde_json::Value::from(120));
    assert_eq!(payload["rows"], serde_json::Value::from(30));
}

fn attach_req(owner: UserId, session_id: TerminalSessionId) -> AttachSessionRequest {
    AttachSessionRequest {
        owner_id: owner,
        session_id,
        client_info: Some("integration-test/1.0".to_owned()),
        remote_addr: Some("127.0.0.1".to_owned()),
    }
}

#[tokio::test]
async fn attach_session_writes_attachment_event_and_runtime() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    let outcome = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .expect("attach");

    assert_eq!(outcome.session.id, session.id);
    assert_eq!(outcome.attachment.session_id, session.id);
    assert!(outcome.attachment.detached_at.is_none());
    assert_eq!(outcome.message, STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE);

    // Runtime entry registered for the attachment.
    let runtime = mgr.attachment(outcome.attachment.id).expect("registered");
    assert_eq!(runtime.session_id, session.id);
    assert_eq!(runtime.owner_id, owner);
    assert_eq!(mgr.attachment_count(), 1);

    // Attached event was appended.
    let events = repo.snapshot_events();
    let attached: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Attached)
        .collect();
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].session_id, session.id);
    assert_eq!(
        attached[0].payload["attachment_id"],
        serde_json::to_value(outcome.attachment.id).unwrap(),
    );
    assert_eq!(
        attached[0].payload["client_info"],
        serde_json::Value::from("integration-test/1.0"),
    );
}

#[tokio::test]
async fn attach_session_unknown_id_returns_not_found() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let err = mgr
        .attach_session(attach_req(owner, TerminalSessionId::new()))
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::NotFound));
    assert_eq!(mgr.attachment_count(), 0);
}

#[tokio::test]
async fn attach_session_foreign_owner_returns_not_found() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let stranger = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    let err = mgr
        .attach_session(attach_req(stranger, session.id))
        .await
        .unwrap_err();
    assert!(
        matches!(err, TerminalSessionManagerError::NotFound),
        "foreign-owner attach must collapse to NotFound, got {err:?}",
    );
    assert_eq!(mgr.attachment_count(), 0);
    // No spurious attachment row was written.
    let attachments = repo
        .inner
        .lock()
        .unwrap()
        .attachments
        .values()
        .cloned()
        .collect::<Vec<_>>();
    assert!(attachments.is_empty());
}

#[tokio::test]
async fn attach_session_closed_session_is_rejected() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    repo.force_close(session.id);

    let err = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap_err();
    assert!(
        matches!(err, TerminalSessionManagerError::SessionClosed),
        "closed session attach must surface SessionClosed, got {err:?}",
    );
    assert_eq!(mgr.attachment_count(), 0);
}

#[tokio::test]
async fn detach_session_writes_event_and_clears_runtime() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    let outcome = mgr
        .detach_session(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(!outcome.already_detached);
    assert!(outcome.attachment.detached_at.is_some());
    assert!(mgr.attachment(attachment.id).is_none());
    assert_eq!(mgr.attachment_count(), 0);

    // Persisted detach bookkeeping is reflected in the row.
    let row = repo.snapshot_attachment(attachment.id).unwrap();
    assert!(row.detached_at.is_some());
    assert_eq!(row.last_seen_seq, None);

    let kinds: Vec<_> = repo.snapshot_events().into_iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&SessionEventKind::Attached));
    assert!(kinds.contains(&SessionEventKind::Detached));
}

#[tokio::test]
async fn detach_session_idempotent_for_already_detached() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    let first = mgr
        .detach_session(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(!first.already_detached);

    let second = mgr
        .detach_session(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(second.already_detached);

    let detached_count = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Detached)
        .count();
    assert_eq!(
        detached_count, 1,
        "second detach must NOT append another Detached event",
    );
}

#[tokio::test]
async fn detach_session_foreign_owner_returns_not_found() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let stranger = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    let err = mgr
        .detach_session(stranger, session.id, attachment.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::NotFound));
}

#[tokio::test]
async fn detach_session_attachment_for_other_session_returns_not_found() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let s1 = mgr.create_session(req(owner)).await.unwrap().session;
    let s2 = mgr.create_session(req(owner)).await.unwrap().session;
    let a1 = mgr
        .attach_session(attach_req(owner, s1.id))
        .await
        .unwrap()
        .attachment;

    // Try to detach a1 against s2's session id — must not match.
    let err = mgr
        .detach_session(owner, s2.id, a1.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::NotFound));
}

#[tokio::test]
async fn close_session_drops_live_attachments() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;
    assert_eq!(mgr.attachment_count(), 1);

    mgr.close_session(session.id, owner).await.unwrap();
    assert_eq!(
        mgr.attachment_count(),
        0,
        "closing a session must drop its live attachments",
    );
    assert!(mgr.attachment(attachment.id).is_none());
}

#[tokio::test]
async fn resize_session_validates_dims_and_writes_event() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    let outcome = mgr
        .resize_session(owner, session.id, 132, 50)
        .await
        .unwrap();
    assert_eq!(outcome.cols, 132);
    assert_eq!(outcome.rows, 50);

    // Runtime hint is updated.
    let runtime = mgr.runtime(session.id).unwrap();
    assert_eq!(runtime.cols, 132);
    assert_eq!(runtime.rows, 50);

    // Resized event is appended with the new dims.
    let resized: Vec<_> = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Resized)
        .collect();
    assert_eq!(resized.len(), 1);
    assert_eq!(resized[0].payload["cols"], serde_json::Value::from(132));
    assert_eq!(resized[0].payload["rows"], serde_json::Value::from(50));
}

#[tokio::test]
async fn resize_session_rejects_invalid_dims() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    for (cols, rows, expected_field) in
        [(0u16, 24u16, "cols"), (80, 0, "rows"), (5_000, 24, "cols")]
    {
        let err = mgr
            .resize_session(owner, session.id, cols, rows)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                TerminalSessionManagerError::InvalidDimensions { field, .. } if field == expected_field
            ),
            "expected InvalidDimensions on {expected_field}, got {err:?}",
        );
    }
    let resized = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Resized)
        .count();
    assert_eq!(resized, 0, "invalid resize must not write a Resized event");
}

#[tokio::test]
async fn resize_session_foreign_owner_returns_not_found() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let stranger = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    let err = mgr
        .resize_session(stranger, session.id, 80, 24)
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::NotFound));
}

#[tokio::test]
async fn resize_session_closed_session_is_rejected() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    repo.force_close(session.id);

    let err = mgr
        .resize_session(owner, session.id, 80, 24)
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::SessionClosed));
}

#[tokio::test]
async fn stub_attach_message_is_pinned() {
    // Wire-stable string: rewording requires updating this assertion AND
    // every client that surfaces the message to the user.
    assert_eq!(
        STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE,
        "attached to RelayTerm session placeholder; PTY streaming is not implemented yet",
    );
}

// ----------------------------------------------------------------------
// Live PTY runtime
// ----------------------------------------------------------------------

/// Test-only fake handle that records inputs and resizes. Owns the
/// single sender into the bridge's output channel via a shared
/// `Arc<Mutex<Option<...>>>` so [`FakeFixture`] can simulate transport
/// teardown by `take()`ing the sender out.
struct FakeHandle {
    inputs: Arc<Mutex<Vec<Vec<u8>>>>,
    resizes: Arc<Mutex<Vec<(u16, u16)>>>,
    output_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<SshPtyEvent>>>>,
    closed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl SshPtyHandle for FakeHandle {
    async fn write_input(&self, bytes: Vec<u8>) -> Result<(), SshPtyError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SshPtyError::BridgeClosed);
        }
        self.inputs.lock().unwrap().push(bytes);
        Ok(())
    }
    async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshPtyError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SshPtyError::BridgeClosed);
        }
        self.resizes.lock().unwrap().push((cols, rows));
        Ok(())
    }
    async fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Mirror the russh impl: dropping the sender causes the
        // bridge's output_rx to see end-of-stream, which in turn
        // exits the orchestrator's forwarder task.
        let _ = self.output_tx.lock().unwrap().take();
    }
}

struct FakeFixture {
    inputs: Arc<Mutex<Vec<Vec<u8>>>>,
    resizes: Arc<Mutex<Vec<(u16, u16)>>>,
    /// Shared with the [`FakeHandle`] so the test can inject output AND
    /// simulate teardown by taking the single sender out.
    output_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<SshPtyEvent>>>>,
}

impl FakeFixture {
    async fn inject_output(&self, bytes: Vec<u8>) {
        let tx = self.output_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(SshPtyEvent::Output(bytes)).await;
        }
    }

    /// Drop the bridge's sole output sender, simulating an SSH transport
    /// teardown. The orchestrator's forwarder will see `None` on its
    /// next `recv()` and run its closed-session bookkeeping.
    fn simulate_teardown(&self) {
        let _ = self.output_tx.lock().unwrap().take();
    }
}

fn fake_start() -> (SshPtyStart, FakeFixture) {
    let (output_tx, output_rx) = tokio::sync::mpsc::channel(16);
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let resizes = Arc::new(Mutex::new(Vec::new()));
    let shared_tx = Arc::new(Mutex::new(Some(output_tx)));
    let handle = FakeHandle {
        inputs: inputs.clone(),
        resizes: resizes.clone(),
        output_tx: shared_tx.clone(),
        closed: std::sync::atomic::AtomicBool::new(false),
    };
    let start = SshPtyStart {
        handle: Box::new(handle),
        output_rx,
        // Fakes don't spawn a separate driver task; the FakeHandle
        // multiplexes input/output through shared mutexes.
        driver: None,
    };
    let fixture = FakeFixture {
        inputs,
        resizes,
        output_tx: shared_tx,
    };
    (start, fixture)
}

#[tokio::test]
async fn start_live_pty_promotes_runtime_and_returns_active_session() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    let (start, _fixture) = fake_start();
    let updated = mgr
        .start_live_pty(owner, session.id, start)
        .await
        .expect("start_live_pty");
    assert_eq!(updated.status, TerminalSessionStatus::Active);
    let runtime = mgr.runtime(session.id).expect("runtime");
    assert_eq!(runtime.status, RuntimeSessionStatus::Live);
    assert!(mgr.live(session.id).is_some());

    // No new `SessionEventKind` is written on PTY-start in this slice —
    // SPEC explicitly forbids `replay_started` until the replay buffer
    // exists, and a precise `live_started` kind is future work that
    // requires a migration. The `Created` event from `create_session`
    // is the only audit row at this point.
    let kinds: Vec<_> = repo.snapshot_events().into_iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![SessionEventKind::Created]);
    let _ = repo;
}

#[tokio::test]
async fn write_pty_input_routes_to_handle_when_live() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    let inputs = fixture.inputs.clone();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    mgr.write_pty_input(owner, session.id, b"hello".to_vec())
        .await
        .unwrap();
    let recorded = inputs.lock().unwrap().clone();
    assert_eq!(recorded, vec![b"hello".to_vec()]);
}

#[tokio::test]
async fn write_pty_input_returns_pty_not_live_for_stub_session() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    let err = mgr
        .write_pty_input(owner, session.id, b"x".to_vec())
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::PtyNotLive));
}

#[tokio::test]
async fn write_pty_input_foreign_owner_returns_not_found() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let stranger = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    let err = mgr
        .write_pty_input(stranger, session.id, b"x".to_vec())
        .await
        .unwrap_err();
    assert!(matches!(err, TerminalSessionManagerError::NotFound));
}

#[tokio::test]
async fn apply_pty_resize_calls_handle_and_returns_true_when_live() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    let resizes = fixture.resizes.clone();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    let live = mgr
        .apply_pty_resize(owner, session.id, 132, 50)
        .await
        .unwrap();
    assert!(live);
    assert_eq!(resizes.lock().unwrap().clone(), vec![(132, 50)]);
}

#[tokio::test]
async fn apply_pty_resize_returns_false_for_stub_session() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let live = mgr
        .apply_pty_resize(owner, session.id, 80, 24)
        .await
        .unwrap();
    assert!(!live);
}

#[tokio::test]
async fn close_session_drops_live_runtime() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    assert!(mgr.live(session.id).is_some());

    mgr.close_session(session.id, owner).await.unwrap();
    assert!(mgr.live(session.id).is_none());
    assert!(mgr.runtime(session.id).is_none());
}

#[tokio::test]
async fn attach_session_returns_active_message_when_live() {
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    let outcome = mgr
        .attach_session(AttachSessionRequest {
            owner_id: owner,
            session_id: session.id,
            client_info: None,
            remote_addr: None,
        })
        .await
        .unwrap();
    assert_eq!(outcome.message, LIVE_PTY_ATTACH_MESSAGE);
    assert!(
        outcome.live.is_some(),
        "live runtime view should be present"
    );
}

#[tokio::test]
async fn record_pty_start_failed_marks_session_closed() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;

    mgr.record_pty_start_failed(owner, session.id, "host_key_not_trusted")
        .await
        .unwrap();

    let row = repo.snapshot_session(session.id).unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Closed);
    assert!(row.closed_at.is_some());

    let closed = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .collect::<Vec<_>>();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].payload["reason"], "ssh_start_failed");
    assert_eq!(closed[0].payload["category"], "host_key_not_trusted");
}

#[tokio::test]
async fn pty_teardown_marks_session_closed_via_forwarder() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    // Simulate the bridge's transport tearing down. The forwarder must
    // observe Closed, persist a closed event, and transition the row.
    fixture.inject_output(b"final-banner".to_vec()).await; // pre-teardown bytes
    let tx_clone = fixture.output_tx.lock().unwrap().clone();
    if let Some(tx) = tx_clone {
        tx.send(SshPtyEvent::Closed {
            reason: ClosedReason::TransportError,
        })
        .await
        .unwrap();
    }
    fixture.simulate_teardown(); // drop the sender so recv() returns None

    // Give the forwarder a moment to run. We use a short sleep loop —
    // yield_now() alone is not enough because the forwarder's exit path
    // chains several .await calls (sessions.get, set_status, events.create)
    // which need real scheduler progress, not just a single yield.
    for _ in 0..40 {
        let row = repo.snapshot_session(session.id).unwrap();
        if row.status == TerminalSessionStatus::Closed {
            assert!(
                row.closed_at.is_some(),
                "closed_at must be stamped on transport teardown"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "session row did not transition to Closed after pty teardown within budget; got {:?}",
        repo.snapshot_session(session.id).map(|s| s.status),
    );
}

// ----------------------------------------------------------------------
// detach_attachment: final-detach auto-close for live PTY sessions
// ----------------------------------------------------------------------

#[tokio::test]
async fn detach_attachment_closes_live_session_on_final_detach() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    // Detach the only attachment — should auto-close the live session.
    let outcome = mgr
        .detach_attachment(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(!outcome.detach.already_detached);
    let close = outcome
        .also_closed
        .expect("final detach of a live session must auto-close");
    assert!(!close.already_closed);
    assert_eq!(close.session.status, TerminalSessionStatus::Closed);
    assert!(close.session.closed_at.is_some());

    // Live runtime + attachment registry both empty.
    assert!(mgr.runtime(session.id).is_none());
    assert!(mgr.live(session.id).is_none());
    assert_eq!(mgr.attachment_count(), 0);

    // Exactly one Detached event and one Closed event were written.
    let events = repo.snapshot_events();
    let detached = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Detached)
        .count();
    let closed = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(detached, 1, "exactly one Detached event must be written");
    assert_eq!(closed, 1, "exactly one Closed event must be written");
}

#[tokio::test]
async fn detach_attachment_does_not_close_when_other_attachments_remain() {
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    let a1 = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;
    let _a2 = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    let outcome = mgr
        .detach_attachment(owner, session.id, a1.id, None)
        .await
        .unwrap();
    assert!(
        outcome.also_closed.is_none(),
        "another attachment is still live"
    );
    // Session remains Active, runtime stays bound.
    let row = repo.snapshot_session(session.id).unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Active);
    assert!(mgr.live(session.id).is_some());
    assert_eq!(mgr.attachment_count(), 1);
}

#[tokio::test]
async fn detach_attachment_does_not_close_stub_session() {
    // No live PTY → no auto-close even if the only attachment detaches.
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    let outcome = mgr
        .detach_attachment(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(
        outcome.also_closed.is_none(),
        "stub session must not auto-close"
    );
    let row = repo.snapshot_session(session.id).unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Starting);
}

#[tokio::test]
async fn detach_attachment_idempotent_on_already_detached_row() {
    // Mirrors the WS race: explicit Detach frame fires, the cleanup tail
    // also fires `detach_attachment` — must NOT write a second Closed
    // event.
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    let first = mgr
        .detach_attachment(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(first.also_closed.is_some());
    let second = mgr
        .detach_attachment(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    assert!(
        second.detach.already_detached,
        "second detach must observe the row as already_detached",
    );
    assert!(
        second.also_closed.is_none(),
        "second detach must NOT auto-close again",
    );

    let closed = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed, 1,
        "race between Detach and cleanup-tail must write exactly one Closed event",
    );
}

#[tokio::test]
async fn explicit_close_remains_idempotent() {
    // Explicit Close, then second Close: must surface `already_closed`
    // and write no second Closed event.
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    mgr.attach_session(attach_req(owner, session.id))
        .await
        .unwrap();

    let first = mgr.close_session(session.id, owner).await.unwrap();
    assert!(!first.already_closed);
    let second = mgr.close_session(session.id, owner).await.unwrap();
    assert!(second.already_closed);

    let closed = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed, 1,
        "double explicit close must write exactly one Closed event",
    );
    // Live runtime is gone after the first close.
    assert!(mgr.live(session.id).is_none());
    assert!(mgr.runtime(session.id).is_none());
}

#[tokio::test]
async fn detach_after_explicit_close_does_not_auto_close_again() {
    // Race scenario: explicit Close ran (registry attachment removed,
    // session row Closed); a subsequent detach call (e.g. from a
    // misbehaving external caller) must NOT trigger another auto-close
    // — there's no live PTY to close.
    let (mgr, repo) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    mgr.close_session(session.id, owner).await.unwrap();

    let detach = mgr
        .detach_attachment(owner, session.id, attachment.id, None)
        .await
        .unwrap();
    // The runtime entry was dropped by close_session, so the helper's
    // "session has live pty" check is false → no auto-close.
    assert!(
        detach.also_closed.is_none(),
        "detach after close must NOT auto-close again",
    );

    let closed = repo
        .snapshot_events()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed, 1,
        "explicit close + later detach must write exactly one Closed event",
    );
}

#[tokio::test]
async fn write_pty_input_after_final_detach_returns_pty_not_live() {
    // After final detach auto-closed the session, the PTY runtime is
    // gone — input must surface PtyNotLive (or NotFound), and never
    // reach the (now-aborted) bridge.
    let (mgr, _) = build_manager();
    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    let inputs = fixture.inputs.clone();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();
    let attachment = mgr
        .attach_session(attach_req(owner, session.id))
        .await
        .unwrap()
        .attachment;

    mgr.detach_attachment(owner, session.id, attachment.id, None)
        .await
        .unwrap();

    let err = mgr
        .write_pty_input(owner, session.id, b"after-detach".to_vec())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            TerminalSessionManagerError::PtyNotLive | TerminalSessionManagerError::NotFound
        ),
        "input after auto-close must surface PtyNotLive or NotFound, got {err:?}",
    );
    assert!(
        inputs.lock().unwrap().is_empty(),
        "no input bytes should reach the fake bridge after auto-close",
    );
}
