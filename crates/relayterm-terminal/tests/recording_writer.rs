//! Integration tests for the recording writer wired into
//! [`TerminalSessionManager`].
//!
//! These tests use an in-memory fake of [`TerminalRecordingRepository`]
//! and an in-memory fake PTY bridge so the writer's chunk/marker shape
//! can be observed end-to-end without touching Postgres. Sentinel-byte
//! redaction is checked here too: the chunk payload bytes flow through
//! the writer, but they must NEVER appear in the marker payload row,
//! the manager's runtime Debug, or any operator-side log surface.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::{
    ServerProfileId, SessionEventId, TerminalRecordingChunkId, TerminalRecordingMarkerId,
    TerminalSessionAttachmentId, TerminalSessionId, UserId,
};
use relayterm_core::repository::{
    CreateSessionEvent, CreateTerminalRecordingChunk, CreateTerminalRecordingMarker,
    CreateTerminalSession, CreateTerminalSessionAttachment, RepositoryError,
    SessionEventRepository, TerminalRecordingRepository, TerminalSessionRepository,
};
use relayterm_core::session_event::SessionEvent;
use relayterm_core::terminal_recording::{
    TerminalRecordingChunk, TerminalRecordingMarker, TerminalRecordingMarkerKind,
};
use relayterm_core::terminal_session::{
    TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use relayterm_ssh::{SshPtyError, SshPtyEvent, SshPtyHandle, SshPtyStart};
use relayterm_terminal::{
    CreateTerminalSessionRequest, RecordingRuntime, RecordingWriterConfig, TerminalSessionManager,
    replay_gap_reason,
};

// ----- Session/event in-memory repo (subset borrowed from manager.rs tests) -----

#[derive(Default)]
struct InMemoryStores {
    sessions: HashMap<TerminalSessionId, TerminalSession>,
    events: Vec<SessionEvent>,
    attachments: HashMap<TerminalSessionAttachmentId, TerminalSessionAttachment>,
}

#[derive(Clone, Default)]
struct InMemoryRepo {
    inner: Arc<StdMutex<InMemoryStores>>,
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

// ----- Recording repository fake -----

#[derive(Default)]
struct FakeRecordingRepo {
    chunks: StdMutex<Vec<TerminalRecordingChunk>>,
    markers: StdMutex<Vec<TerminalRecordingMarker>>,
}

impl FakeRecordingRepo {
    fn snapshot_chunks(&self) -> Vec<TerminalRecordingChunk> {
        self.chunks.lock().unwrap().clone()
    }

    fn snapshot_markers(&self) -> Vec<TerminalRecordingMarker> {
        self.markers.lock().unwrap().clone()
    }
}

#[async_trait]
impl TerminalRecordingRepository for FakeRecordingRepo {
    async fn append_chunk(
        &self,
        input: CreateTerminalRecordingChunk,
    ) -> Result<TerminalRecordingChunk, RepositoryError> {
        let chunk = TerminalRecordingChunk {
            id: TerminalRecordingChunkId::new(),
            terminal_session_id: input.terminal_session_id,
            seq_start: input.seq_start,
            seq_end: input.seq_end,
            byte_len: input.byte_len,
            payload: input.payload,
            encryption: input.encryption,
            compression: input.compression,
            created_at: Utc::now(),
        };
        self.chunks.lock().unwrap().push(chunk.clone());
        Ok(chunk)
    }

    async fn append_marker(
        &self,
        input: CreateTerminalRecordingMarker,
    ) -> Result<TerminalRecordingMarker, RepositoryError> {
        let marker = TerminalRecordingMarker {
            id: TerminalRecordingMarkerId::new(),
            terminal_session_id: input.terminal_session_id,
            kind: input.kind,
            seq: input.seq,
            payload: input.payload,
            created_at: Utc::now(),
        };
        self.markers.lock().unwrap().push(marker.clone());
        Ok(marker)
    }

    async fn list_chunks(
        &self,
        _: TerminalSessionId,
        _: i64,
        _: u32,
    ) -> Result<Vec<TerminalRecordingChunk>, RepositoryError> {
        Ok(self.chunks.lock().unwrap().clone())
    }

    async fn list_markers(
        &self,
        _: TerminalSessionId,
        _: i64,
        _: u32,
    ) -> Result<Vec<TerminalRecordingMarker>, RepositoryError> {
        Ok(self.markers.lock().unwrap().clone())
    }

    async fn get_metadata(
        &self,
        terminal_session_id: TerminalSessionId,
    ) -> Result<relayterm_core::TerminalRecordingMetadata, RepositoryError> {
        Ok(relayterm_core::TerminalRecordingMetadata::empty(
            terminal_session_id,
        ))
    }
}

// ----- Fake PTY -----

struct FakeHandle {
    output_tx: Arc<StdMutex<Option<tokio::sync::mpsc::Sender<SshPtyEvent>>>>,
    closed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl SshPtyHandle for FakeHandle {
    async fn write_input(&self, _: Vec<u8>) -> Result<(), SshPtyError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SshPtyError::BridgeClosed);
        }
        Ok(())
    }
    async fn resize(&self, _: u16, _: u16) -> Result<(), SshPtyError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SshPtyError::BridgeClosed);
        }
        Ok(())
    }
    async fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.output_tx.lock().unwrap().take();
    }
}

struct FakeFixture {
    output_tx: Arc<StdMutex<Option<tokio::sync::mpsc::Sender<SshPtyEvent>>>>,
}

impl FakeFixture {
    async fn inject_output(&self, bytes: Vec<u8>) {
        let tx = self.output_tx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(SshPtyEvent::Output(bytes)).await;
        }
    }
}

fn fake_start() -> (SshPtyStart, FakeFixture) {
    let (output_tx, output_rx) = tokio::sync::mpsc::channel(64);
    let shared_tx = Arc::new(StdMutex::new(Some(output_tx)));
    let handle = FakeHandle {
        output_tx: shared_tx.clone(),
        closed: std::sync::atomic::AtomicBool::new(false),
    };
    let start = SshPtyStart {
        handle: Box::new(handle),
        output_rx,
        driver: None,
    };
    let fixture = FakeFixture {
        output_tx: shared_tx,
    };
    (start, fixture)
}

fn req(owner: UserId) -> CreateTerminalSessionRequest {
    CreateTerminalSessionRequest {
        owner_id: owner,
        server_profile_id: ServerProfileId::new(),
        cols: 120,
        rows: 30,
    }
}

fn build_manager_with_recording(
    cfg: RecordingWriterConfig,
) -> (
    Arc<TerminalSessionManager>,
    InMemoryRepo,
    Arc<FakeRecordingRepo>,
) {
    let session_repo = InMemoryRepo::default();
    let recording_repo = Arc::new(FakeRecordingRepo::default());
    let runtime = RecordingRuntime::new(
        recording_repo.clone() as Arc<dyn TerminalRecordingRepository>,
        cfg,
    );
    let mgr = TerminalSessionManager::new(
        Arc::new(session_repo.clone()) as Arc<dyn TerminalSessionRepository>,
        Arc::new(session_repo.clone()) as Arc<dyn SessionEventRepository>,
    )
    .with_recording(runtime);
    (Arc::new(mgr), session_repo, recording_repo)
}

const SENTINEL_BYTES: &[u8] = b"PTY-SENTINEL-INTEGRATION-A1B2";

/// Spin until `cond` returns true OR the budget elapses. The recording
/// writer is async and runs on its own task; tests need a small loop to
/// observe its eventual writes.
async fn await_for<F>(mut cond: F)
where
    F: FnMut() -> bool,
{
    for _ in 0..80 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("condition not satisfied within budget");
}

#[tokio::test]
async fn manager_with_recording_writes_chunk_and_started_marker() {
    let (mgr, _session_repo, recording_repo) =
        build_manager_with_recording(RecordingWriterConfig {
            chunk_target_bytes: 8,
            chunk_hard_cap_bytes: 64 * 1024,
        });
    assert!(mgr.recording_enabled());

    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    fixture.inject_output(b"hello-world".to_vec()).await;

    // Chunk lands once the target threshold is crossed; close still
    // flushes any trailing partial chunk.
    mgr.close_session(session.id, owner).await.unwrap();

    await_for(|| !recording_repo.snapshot_chunks().is_empty()).await;
    let chunks = recording_repo.snapshot_chunks();
    assert!(!chunks.is_empty(), "expected at least one chunk");
    let combined: Vec<u8> = chunks.iter().flat_map(|c| c.payload.clone()).collect();
    assert_eq!(combined, b"hello-world".to_vec());

    let markers = recording_repo.snapshot_markers();
    let kinds: Vec<_> = markers.iter().map(|m| m.kind).collect();
    assert!(
        kinds.contains(&TerminalRecordingMarkerKind::Started),
        "started marker missing; got {kinds:?}"
    );
    assert!(
        kinds.contains(&TerminalRecordingMarkerKind::Closed),
        "closed marker missing; got {kinds:?}"
    );
}

#[tokio::test]
async fn manager_without_recording_writes_no_chunks() {
    use relayterm_terminal::TerminalSessionManager;
    let session_repo = InMemoryRepo::default();
    let mgr = Arc::new(TerminalSessionManager::new(
        Arc::new(session_repo.clone()) as Arc<dyn TerminalSessionRepository>,
        Arc::new(session_repo.clone()) as Arc<dyn SessionEventRepository>,
    ));
    assert!(!mgr.recording_enabled());

    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    fixture.inject_output(SENTINEL_BYTES.to_vec()).await;
    mgr.close_session(session.id, owner).await.unwrap();
    // No state to inspect — pass if no panic and no recording side
    // effects (there is no recording repo to write to).
}

#[tokio::test]
async fn resize_session_records_resized_marker_when_live() {
    let (mgr, _session_repo, recording_repo) =
        build_manager_with_recording(RecordingWriterConfig::DEFAULT);

    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    // Push some output so last_seq is non-zero before resize lands.
    fixture.inject_output(vec![b'x'; 8]).await;
    // Give the forwarder a moment to stamp seq.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    mgr.resize_session(owner, session.id, 132, 50)
        .await
        .unwrap();
    mgr.close_session(session.id, owner).await.unwrap();

    await_for(|| {
        recording_repo
            .snapshot_markers()
            .iter()
            .any(|m| m.kind == TerminalRecordingMarkerKind::Resized)
    })
    .await;
    let resized = recording_repo
        .snapshot_markers()
        .into_iter()
        .find(|m| m.kind == TerminalRecordingMarkerKind::Resized)
        .unwrap();
    assert_eq!(resized.payload["cols"], 132);
    assert_eq!(resized.payload["rows"], 50);
}

#[tokio::test]
async fn close_session_writes_closed_marker_at_last_seq() {
    let (mgr, _session_repo, recording_repo) =
        build_manager_with_recording(RecordingWriterConfig::DEFAULT);

    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    fixture.inject_output(b"ab".to_vec()).await;
    fixture.inject_output(b"cd".to_vec()).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    mgr.close_session(session.id, owner).await.unwrap();
    await_for(|| {
        recording_repo
            .snapshot_markers()
            .iter()
            .any(|m| m.kind == TerminalRecordingMarkerKind::Closed)
    })
    .await;
    let closed = recording_repo
        .snapshot_markers()
        .into_iter()
        .find(|m| m.kind == TerminalRecordingMarkerKind::Closed)
        .unwrap();
    // Two output frames → two distinct seqs (1, 2). The closed marker
    // is stamped at the highest observed seq.
    assert_eq!(
        closed.seq, 2,
        "closed marker seq must equal the last observed output seq"
    );
}

#[tokio::test]
async fn marker_payloads_never_carry_pty_byte_sentinel() {
    let (mgr, _session_repo, recording_repo) =
        build_manager_with_recording(RecordingWriterConfig::DEFAULT);

    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    // Sentinel bytes flow through the PTY. They must be visible in the
    // chunk payload (we recorded them) but MUST NOT appear in the
    // marker payload, the marker Debug, or any other operator-side
    // string the markers expose.
    fixture.inject_output(SENTINEL_BYTES.to_vec()).await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    mgr.resize_session(owner, session.id, 80, 24).await.unwrap();
    mgr.close_session(session.id, owner).await.unwrap();

    await_for(|| {
        recording_repo
            .snapshot_markers()
            .iter()
            .any(|m| m.kind == TerminalRecordingMarkerKind::Closed)
    })
    .await;

    for m in recording_repo.snapshot_markers() {
        let body = m.payload.to_string();
        assert!(
            !body.contains("PTY-SENTINEL"),
            "marker payload leaked PTY sentinel: kind={:?} body={body}",
            m.kind
        );
        let dbg = format!("{m:?}");
        assert!(
            !dbg.contains("PTY-SENTINEL"),
            "marker Debug leaked PTY sentinel: {dbg}"
        );
    }

    // Sanity: chunk payload DID record the sentinel — the test would be
    // vacuous without this.
    let combined: Vec<u8> = recording_repo
        .snapshot_chunks()
        .iter()
        .flat_map(|c| c.payload.clone())
        .collect();
    assert!(
        combined
            .windows(SENTINEL_BYTES.len())
            .any(|w| w == SENTINEL_BYTES),
        "expected the sentinel in chunk payload to prove the test path runs"
    );
}

#[tokio::test]
async fn pty_input_is_not_recorded_as_output() {
    // The recording writer is fed only by the PTY forwarder's output
    // path. Input written via `write_pty_input` MUST NEVER reach the
    // chunk stream (no command-inspection / keylogger surface).
    let (mgr, _session_repo, recording_repo) =
        build_manager_with_recording(RecordingWriterConfig::DEFAULT);

    let owner = UserId::new();
    let session = mgr.create_session(req(owner)).await.unwrap().session;
    let (start, _fixture) = fake_start();
    mgr.start_live_pty(owner, session.id, start).await.unwrap();

    // Send input that contains a sentinel; it must NOT appear as a
    // chunk payload nor in any marker.
    let input_sentinel: &[u8] = b"INPUT-KEYSTROKE-SENTINEL-Z9X8";
    mgr.write_pty_input(owner, session.id, input_sentinel.to_vec())
        .await
        .unwrap();

    mgr.close_session(session.id, owner).await.unwrap();
    // Give writer a chance to drain.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let chunks = recording_repo.snapshot_chunks();
    for c in &chunks {
        assert!(
            !c.payload
                .windows(input_sentinel.len())
                .any(|w| w == input_sentinel),
            "input bytes leaked into a recording chunk"
        );
    }
    for m in recording_repo.snapshot_markers() {
        let body = m.payload.to_string();
        assert!(
            !body.contains("INPUT-KEYSTROKE"),
            "input bytes leaked into a recording marker: {body}"
        );
    }
}

#[tokio::test]
async fn replay_gap_reason_constants_are_stable() {
    // Pinned: a future helpful rewording of the reason strings is a
    // wire / replay-viewer breaking change and should be forced
    // through review.
    assert_eq!(replay_gap_reason::WRITER_OVERFLOW, "writer_overflow");
    assert_eq!(replay_gap_reason::WRITER_ERROR, "writer_error");
    assert_eq!(replay_gap_reason::FRAME_OVERSIZED, "frame_oversized");
}
