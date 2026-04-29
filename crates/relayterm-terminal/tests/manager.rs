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
use relayterm_terminal::{
    CreateTerminalSessionRequest, RuntimeSessionStatus, STUB_PTY_NOT_IMPLEMENTED_MESSAGE,
    TerminalSessionManager, TerminalSessionManagerError,
};

#[derive(Default)]
struct InMemoryStores {
    sessions: HashMap<TerminalSessionId, TerminalSession>,
    events: Vec<SessionEvent>,
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
        _input: CreateTerminalSessionAttachment,
    ) -> Result<TerminalSessionAttachment, RepositoryError> {
        unreachable!("manager unit tests do not exercise attachments")
    }

    async fn list_attachments(
        &self,
        _session_id: TerminalSessionId,
    ) -> Result<Vec<TerminalSessionAttachment>, RepositoryError> {
        Ok(Vec::new())
    }

    async fn get_attachment(
        &self,
        _id: TerminalSessionAttachmentId,
    ) -> Result<Option<TerminalSessionAttachment>, RepositoryError> {
        Ok(None)
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
