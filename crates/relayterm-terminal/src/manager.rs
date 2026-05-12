//! `TerminalSessionManager` and supporting types.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use crate::recording::{RecordingRuntime, RecordingWriter};
use crate::replay::{OutputFrame, ReplayBuffer, ReplayBufferConfig, ReplayRange, ReplayWindowLost};

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
use relayterm_ssh::{ClosedReason, SshPtyError, SshPtyEvent, SshPtyHandle, SshPtyStart};
use tokio::sync::broadcast;
use tracing::warn;

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
/// shell can be reached. This is the legacy "metadata-only" path used by
/// callers that don't want a live PTY immediately; today the API routes
/// always start a PTY on create and use [`LIVE_PTY_CREATE_MESSAGE`].
pub const STUB_PTY_NOT_IMPLEMENTED_MESSAGE: &str =
    "session metadata created; PTY startup is not implemented yet";

/// Wire-stable message returned alongside a freshly opened WebSocket
/// attachment WHEN the session is metadata-only (no live PTY).
///
/// Pinned in tests so a future helpful rewording is forced through review.
/// MUST disclaim PTY/streaming readiness explicitly.
pub const STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE: &str =
    "attached to RelayTerm session placeholder; PTY streaming is not implemented yet";

/// Wire-stable message returned alongside a freshly created session that
/// has a LIVE PTY backing it.
///
/// Pinned in tests so a future helpful rewording is forced through review.
/// MUST be conservative: a green create response means SSH transport,
/// host-key trust, public-key auth, and PTY allocation succeeded — it
/// does NOT promise replay/resume across reconnects.
pub const LIVE_PTY_CREATE_MESSAGE: &str =
    "ssh pty started; replay across reconnects is not yet implemented";

/// Wire-stable message returned alongside a freshly opened WebSocket
/// attachment WHEN a live PTY is streaming.
///
/// Pinned in tests so a future helpful rewording is forced through review.
/// MUST be conservative: byte streaming is live, but replay across
/// reconnects is future work.
pub const LIVE_PTY_ATTACH_MESSAGE: &str =
    "attached to live RelayTerm session; replay across reconnects is not yet implemented";

/// Capacity of the per-session broadcast that fans PTY output to all
/// active attachments. Bounded — a slow attachment that lags by more
/// than this many `Output` frames is silently dropped (`broadcast::Lagged`)
/// rather than blocking the SSH driver. The renderer that lagged sees
/// missing bytes; the future replay slice will close the gap.
const ATTACHMENT_FANOUT_CAPACITY: usize = 256;

/// Default for how long a live PTY is allowed to linger after the last
/// client detaches before the orchestrator tears it down.
///
/// **Conservative on purpose.** Detached persistence is a *short-term
/// reconnect grace window* on a still-live PTY held by the running
/// backend — NOT durable session resume. A backend restart drops every
/// live PTY regardless; long-term persistent sessions
/// (`tmux`/`screen`-style resurrection) are a separate, future
/// architecture. Reconnect within this window cancels the scheduled
/// close; outside it the PTY closes and a fresh attach to the same
/// session id surfaces the standard `409 conflict { entity:
/// "terminal_session" }` from the upgrade gate.
///
/// Operator-tunable via `terminal_sessions.detached_live_pty_ttl_seconds`
/// in the backend config (env:
/// `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`,
/// bounded `5..=86_400`). The backend wires the configured value
/// through [`TerminalSessionManager::with_detach_ttl`]; this constant
/// is the value [`TerminalSessionManager::new`] uses when no explicit
/// TTL is supplied (test convenience + the documented default the
/// config layer mirrors).
pub const DETACHED_LIVE_PTY_TTL: Duration = Duration::from_secs(30);

/// Default per-user ceiling on concurrent live PTY runtimes. Phase 1B.1
/// of `docs/session-quotas.md` § 4.1 names `8` — conservative for solo
/// homelab use, defensible for a small multi-user deployment. The
/// configuration layer (`apps/backend/src/config.rs`) mirrors this
/// constant; the manager defaults to it when no operator override is
/// supplied (test convenience + the documented default).
pub const DEFAULT_MAX_LIVE_PTY_PER_USER: u32 = 8;

/// Default per-user ceiling on concurrent in-flight starting sessions
/// (Phase 1B.2a of `docs/session-quotas.md` § 4.3). `4` is enough for
/// honest UI burst behaviour (a SPA navigation that opens a few
/// sessions in parallel) but rejects a tight POST loop. Counts the
/// disjoint set of registry entries with `live = None` AND `snapshot.
/// status == Starting`, so it never double-counts the per-user live
/// quota.
pub const DEFAULT_MAX_STARTING_PER_USER: u32 = 4;

/// Default deployment-wide ceiling on concurrent live PTY runtimes
/// (Phase 1B.2b of `docs/session-quotas.md` § 4.2). `64` is
/// conservative for a single-tenant self-hosted deployment;
/// operators running a multi-user homelab can raise it via the
/// `terminal_sessions.max_live_pty_sessions_per_deployment` config
/// knob (bounded `1..=4096`). The configuration layer
/// (`apps/backend/src/config.rs`) mirrors this constant; the manager
/// defaults to it when no operator override is supplied (test
/// convenience + the documented default).
pub const DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT: u32 = 64;

/// In-memory status for a runtime registry entry.
///
/// Distinct from [`TerminalSessionStatus`] (the persisted enum) so the
/// runtime can carry states that are meaningless at rest. `close_session`
/// removes the entry rather than transitioning to a `Closed` runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSessionStatus {
    /// Placeholder created at metadata-write time. No PTY yet.
    Starting,
    /// Live PTY is allocated and bytes are streaming. Input writes,
    /// resize requests, and `Output` events all flow through the
    /// underlying [`SshPtyHandle`].
    Live,
    /// PTY tore down (remote shell exited, transport error, or local
    /// close). The runtime entry is kept transiently so attached
    /// WebSocket tasks can observe the final state before the manager
    /// drops it during `close_session`.
    Ended,
}

/// In-memory runtime entry for a terminal session.
///
/// Public fields surface the metadata-shaped view; the live PTY handle
/// and broadcast fanout are kept opaque (see [`LiveRuntimeView`]).
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

/// Internal entry held in the runtime registry. Carries the public
/// snapshot plus, when live, the SSH PTY handle and the broadcast
/// channel attachments subscribe to. Internal: never crosses the API
/// boundary directly.
struct RuntimeEntry {
    snapshot: TerminalSessionRuntime,
    live: Option<LiveRuntime>,
    /// Monotonic per-session output sequence counter. Carried in the
    /// runtime so it survives PTY teardown; closed/recreated sessions
    /// get a fresh counter via the new entry.
    next_seq: Arc<AtomicU64>,
}

/// Live PTY surface for one terminal session. Held by the manager and
/// shared with attachments so they can subscribe to the broadcast
/// without touching the SSH layer directly.
struct LiveRuntime {
    /// Handle to the running PTY bridge. Cheap to share across handlers
    /// (`Arc`); [`SshPtyHandle`] methods are `&self`.
    handle: Arc<dyn SshPtyHandle>,
    /// Broadcast surface attachments subscribe to. The forwarder pushes
    /// every frame into both this channel AND `replay` so the wire
    /// fanout stays single-source-of-truth and lagging subscribers can
    /// recover via the replay path.
    output_tx: broadcast::Sender<OutputFrame>,
    /// Shared bounded replay buffer for this session. Behind `Mutex` so
    /// the forwarder can push from one task while attach handlers
    /// snapshot it from another. `Arc` shared with the
    /// [`LiveRuntimeView`] handed to handlers.
    replay: Arc<Mutex<ReplayBuffer>>,
    /// Forwarder task handle. Tied to the lifetime of the runtime entry
    /// so the manager can detach it cleanly on close. The close path
    /// awaits this (bounded) so every PTY frame the forwarder will
    /// observe has been teed into the recording writer BEFORE the
    /// writer's shutdown drains its queue — without this barrier a
    /// race between `handle.close()` dropping the bridge sender and
    /// `recording.shutdown()` sending its `Shutdown` command would
    /// allow the writer to exit before the trailing frames landed.
    forwarder: Option<tokio::task::JoinHandle<()>>,
    /// Bridge's own driver task (russh impl: the channel multiplexer;
    /// fakes: `None`). Stored so the manager can `abort()` it on close
    /// rather than relying solely on the channel-closure teardown path.
    /// AGENTS.md prohibits `tokio::spawn`-and-forget for long-lived
    /// tasks; this field is the orchestrator-side tracker.
    driver: Option<tokio::task::JoinHandle<()>>,
    /// `Some` only while the session is in the **detached-but-alive**
    /// window. Carries the deadline plus the TTL close task. Cleared
    /// (and the task aborted) on reattach or explicit close.
    detach_close: Option<DetachClose>,
    /// Recording writer for this session. Disabled when recording is
    /// off at config time (every method is a no-op); enabled when the
    /// manager was constructed with a [`RecordingRuntime`]. The
    /// forwarder calls `record_output(seq, bytes)` on every PTY frame
    /// AFTER fanning it out to broadcast/replay so recording can never
    /// block the live wire. `close_session` calls `shutdown` on this
    /// writer to flush the trailing chunk and write the `closed`
    /// marker.
    recording: RecordingWriter,
    /// Highest output seq the forwarder stamped. Updated on every
    /// frame so `close_session` / `resize_session` know which seq to
    /// stamp on lifecycle markers (resize, closed) without touching
    /// the broadcast surface.
    last_seq: Arc<AtomicU64>,
}

/// In-memory bookkeeping for a session that has been detached but whose
/// PTY is still alive within the [`DETACHED_LIVE_PTY_TTL`] window.
///
/// Held inside [`LiveRuntime::detach_close`]; aborted on reattach (the
/// reconnecting client cancels the close) or via `Drop` on the runtime
/// when the session closes for any other reason.
struct DetachClose {
    info: DetachInfo,
    /// Spawned task that fires `close_session` when the TTL elapses.
    /// Aborted on reattach OR via `Drop` on the runtime so an explicit
    /// close never races with the timer.
    handle: tokio::task::JoinHandle<()>,
}

/// Public, payload-only view of the detached-window state for a single
/// session. Returned by [`TerminalSessionManager::detach_state`] for
/// diagnostic surfaces; the operator can render `expires_at - now()` as
/// a "session closes in N seconds" hint without touching internal
/// runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachInfo {
    pub detached_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Drop for LiveRuntime {
    fn drop(&mut self) {
        // Best-effort: abort the spawned tasks so a lingering registry
        // drop doesn't leave them running. The driver task in russh_pty
        // would also tear down on its own when the handle's senders
        // drop, but `abort()` is the explicit "stop now" signal.
        if let Some(forwarder) = self.forwarder.take() {
            forwarder.abort();
        }
        if let Some(driver) = self.driver.take() {
            driver.abort();
        }
        if let Some(d) = self.detach_close.take() {
            d.handle.abort();
        }
    }
}

/// Read-only handle handed to attachment tasks so they can subscribe
/// to the live output broadcast and snapshot the replay buffer without
/// holding the manager's outer lock.
#[derive(Clone)]
pub struct LiveRuntimeView {
    pub handle: Arc<dyn SshPtyHandle>,
    pub output_tx: broadcast::Sender<OutputFrame>,
    /// Shared replay buffer for this live session. The handler grabs
    /// the lock briefly during the attach handshake to compute a
    /// `ReplayRange` from a client-provided `last_seen_seq`.
    pub replay: Arc<Mutex<ReplayBuffer>>,
}

impl std::fmt::Debug for LiveRuntimeView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveRuntimeView")
            .field("handle", &"<dyn SshPtyHandle>")
            .field("output_tx", &"<broadcast::Sender<OutputFrame>>")
            .field("replay", &"<Mutex<ReplayBuffer>>")
            .finish()
    }
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
    /// `true` once the manager has bound a live PTY runtime to the
    /// session (i.e. SSH transport, host-key trust, public-key auth,
    /// PTY/shell allocation all succeeded). When `false`, the row was
    /// written but no PTY exists — typically only the legacy stub path.
    pub pty_live: bool,
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
    /// `Some` if a live PTY is bound to this session — the WebSocket
    /// task subscribes to `output_tx` for fanout and routes input
    /// through `handle`. `None` for stub/closed sessions.
    pub live: Option<LiveRuntimeView>,
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

/// Combined outcome for the [`TerminalSessionManager::detach_attachment`]
/// helper. Carries the regular detach result plus, when the manager
/// scheduled a TTL close because this was the last attachment of a live
/// PTY, the deadline metadata.
///
/// The session is **not closed yet** when `detached_pending_close` is
/// `Some` — the PTY survives until the deadline OR until the next
/// reattach cancels it OR until an explicit close arrives. The fact
/// that the close is scheduled is exposed so the WS handler can tell
/// the renderer "you've been detached; reconnect within N seconds to
/// resume."
#[derive(Debug, Clone)]
pub struct DetachOutcome {
    pub detach: DetachSessionOutcome,
    /// `Some(info)` when this detach left a live PTY with zero
    /// attachments and the manager scheduled a TTL close. `None` when
    /// the session had no live PTY, when other attachments remain, or
    /// when this detach was already-idempotent (the previous detach
    /// already scheduled or executed the close).
    pub detached_pending_close: Option<DetachInfo>,
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

    /// Caller wrote input or asked for a resize on a session whose live
    /// PTY runtime is not present (startup failed, PTY tore down, or
    /// the session never had a PTY). Distinct from `SessionClosed` —
    /// the row may still be `starting` while the PTY is gone.
    #[error("terminal session has no live pty")]
    PtyNotLive,

    /// Live PTY startup failed (SSH transport / auth / pty alloc /
    /// shell start). Carries the bridge's typed error so the API can
    /// map it to a stable wire status. The bridge layer handles
    /// secret redaction; this variant is the boundary.
    #[error("ssh pty start failed: {0}")]
    PtyStart(#[from] SshPtyError),

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
    runtimes: RwLock<HashMap<TerminalSessionId, RuntimeEntry>>,
    /// Live attachments keyed by attachment id. A single session may have
    /// multiple entries here (the future "two clients viewing one shell"
    /// shape) — today the WS handler enforces one at a time, but the
    /// registry is shaped for the eventual expansion.
    attachments: RwLock<HashMap<TerminalSessionAttachmentId, AttachmentRuntime>>,
    /// How long a live PTY lingers after the last client detaches. The
    /// default ([`DETACHED_LIVE_PTY_TTL`]) is the production value;
    /// tests construct managers with a shorter TTL via
    /// [`Self::with_detach_ttl`] so they don't have to burn real
    /// wall-clock budget driving the timer.
    detach_ttl: Duration,
    /// Per-user ceiling on concurrent live PTY runtime entries. Phase 1B.1
    /// of `docs/session-quotas.md`; default
    /// [`DEFAULT_MAX_LIVE_PTY_PER_USER`], operator-tunable via
    /// `terminal_sessions.max_live_pty_sessions_per_user`. Always
    /// positive — `NonZeroU32` keeps the type-level invariant. The
    /// manager owns the value so a single accessor surfaces it to
    /// both the create route (for enforcement) and the
    /// `session-policy` route (for the SPA cap copy).
    max_live_pty_per_user: NonZeroU32,
    /// Per-user ceiling on concurrent in-flight starting sessions
    /// (Phase 1B.2a of `docs/session-quotas.md` § 4.3). Counts the
    /// disjoint set of registry entries with `live = None` AND
    /// `snapshot.status == Starting`. Default
    /// [`DEFAULT_MAX_STARTING_PER_USER`], operator-tunable via
    /// `terminal_sessions.max_starting_sessions_per_user`. Always
    /// positive — `NonZeroU32` keeps the type-level invariant.
    max_starting_per_user: NonZeroU32,
    /// Deployment-wide ceiling on concurrent live PTY runtime entries
    /// across ALL owners (Phase 1B.2b of `docs/session-quotas.md`
    /// § 4.2). Default [`DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT`],
    /// operator-tunable via
    /// `terminal_sessions.max_live_pty_sessions_per_deployment`.
    /// Counted against THIS backend instance's in-memory registry —
    /// exact for single-instance deployments, per-instance best-effort
    /// for any multi-instance topology (§ 9). Always positive —
    /// `NonZeroU32` keeps the type-level invariant.
    max_live_pty_per_deployment: NonZeroU32,
    /// Recording runtime (`Some` when `terminal_recording.enabled =
    /// true` and the writer is supported in the configured mode). When
    /// `None`, every live session gets a [`RecordingWriter::disabled`]
    /// so the PTY forwarder's tee call sites stay branch-free.
    recording: Option<RecordingRuntime>,
}

impl TerminalSessionManager {
    #[must_use]
    pub fn new(
        sessions: Arc<dyn TerminalSessionRepository>,
        events: Arc<dyn SessionEventRepository>,
    ) -> Self {
        Self::with_detach_ttl(sessions, events, DETACHED_LIVE_PTY_TTL)
    }

    /// Construct a manager with a custom detach TTL. Used by unit and
    /// integration tests so the TTL-expiry path can be exercised in
    /// real time. Production code SHOULD use [`Self::new`] so the
    /// SPEC-pinned [`DETACHED_LIVE_PTY_TTL`] is the single source of
    /// truth.
    #[must_use]
    pub fn with_detach_ttl(
        sessions: Arc<dyn TerminalSessionRepository>,
        events: Arc<dyn SessionEventRepository>,
        detach_ttl: Duration,
    ) -> Self {
        Self {
            sessions,
            events,
            runtimes: RwLock::new(HashMap::new()),
            attachments: RwLock::new(HashMap::new()),
            detach_ttl,
            max_live_pty_per_user: NonZeroU32::new(DEFAULT_MAX_LIVE_PTY_PER_USER)
                .expect("DEFAULT_MAX_LIVE_PTY_PER_USER is non-zero"),
            max_starting_per_user: NonZeroU32::new(DEFAULT_MAX_STARTING_PER_USER)
                .expect("DEFAULT_MAX_STARTING_PER_USER is non-zero"),
            max_live_pty_per_deployment: NonZeroU32::new(DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT)
                .expect("DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT is non-zero"),
            recording: None,
        }
    }

    /// Override the per-user live-PTY ceiling (Phase 1B.1 quota).
    ///
    /// Builder-style: returns `self` so the caller can chain construction
    /// at backend boot. The configuration layer
    /// (`apps/backend/src/config.rs::Config::validate_terminal_sessions`)
    /// has already bounded the value `1..=256` before this is called;
    /// passing a value outside that range is a programmer bug, not an
    /// operator-recoverable state.
    #[must_use]
    pub fn with_max_live_pty_per_user(mut self, cap: NonZeroU32) -> Self {
        self.max_live_pty_per_user = cap;
        self
    }

    /// Override the per-user starting-burst ceiling (Phase 1B.2a quota).
    ///
    /// Builder-style: returns `self` so the caller can chain construction
    /// at backend boot. The configuration layer
    /// (`apps/backend/src/config.rs::Config::validate_terminal_sessions`)
    /// has already bounded the value `1..=32` before this is called;
    /// passing a value outside that range is a programmer bug, not an
    /// operator-recoverable state.
    #[must_use]
    pub fn with_max_starting_per_user(mut self, cap: NonZeroU32) -> Self {
        self.max_starting_per_user = cap;
        self
    }

    /// Override the deployment-wide live-PTY ceiling (Phase 1B.2b quota).
    ///
    /// Builder-style: returns `self` so the caller can chain construction
    /// at backend boot. The configuration layer
    /// (`apps/backend/src/config.rs::Config::validate_terminal_sessions`)
    /// has already bounded the value `1..=4096` AND confirmed it sits at
    /// or above every per-user cap before this is called; passing a
    /// value outside that range is a programmer bug, not an
    /// operator-recoverable state.
    #[must_use]
    pub fn with_max_live_pty_per_deployment(mut self, cap: NonZeroU32) -> Self {
        self.max_live_pty_per_deployment = cap;
        self
    }

    /// Attach a [`RecordingRuntime`] for durable PTY-output recording.
    ///
    /// Builder-style: returns `self` so the caller can chain construction
    /// at backend boot. When recording is configured-disabled the caller
    /// simply does not call this method and every live session uses a
    /// [`RecordingWriter::disabled`] tee point with zero overhead.
    #[must_use]
    pub fn with_recording(mut self, recording: RecordingRuntime) -> Self {
        self.recording = Some(recording);
        self
    }

    /// Returns `true` when this manager will fan PTY output bytes
    /// into a backing recording repository. Test-only convenience.
    #[must_use]
    pub fn recording_enabled(&self) -> bool {
        self.recording.is_some()
    }

    /// Currently-configured detach TTL. Diagnostic getter for tests
    /// and the dev-only operator surfaces.
    #[must_use]
    pub fn detach_ttl(&self) -> Duration {
        self.detach_ttl
    }

    /// Create a metadata row in `Starting` status, append the `created`
    /// session event, and register an in-memory runtime placeholder.
    ///
    /// This call does NOT open an SSH channel — the PTY is bound in a
    /// follow-up [`Self::start_live_pty`] call. The two-step shape lets
    /// the API route apply preconditions (host-key trust, vault decrypt,
    /// dim validation) between the row write and the PTY start without
    /// the manager owning that orchestration.
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

        let entry = RuntimeEntry {
            snapshot: TerminalSessionRuntime {
                id: session.id,
                owner_id: session.owner_id,
                server_profile_id: session.server_profile_id,
                status: RuntimeSessionStatus::Starting,
                created_at: session.created_at,
                cols: session.cols,
                rows: session.rows,
            },
            live: None,
            next_seq: Arc::new(AtomicU64::new(1)),
        };
        self.runtimes
            .write()
            .expect("runtime registry lock poisoned")
            .insert(session.id, entry);

        Ok(CreateTerminalSessionOutcome {
            session,
            message: STUB_PTY_NOT_IMPLEMENTED_MESSAGE,
            pty_live: false,
        })
    }

    /// Bind a live SSH PTY runtime to an existing session.
    ///
    /// On success the session row transitions to `Active`, the runtime
    /// entry stores the [`SshPtyHandle`] + broadcast surface, and a
    /// forwarder task drains the bridge's `output_rx` into the broadcast
    /// (stamping a monotonic `seq`). On PTY exit the forwarder appends
    /// a `Closed` lifecycle event and transitions the session row to
    /// `Closed`.
    ///
    /// On failure the session row is transitioned to `Closed` with a
    /// `closed` event payload that names the failure category — the
    /// orphan-row pattern from the metadata-only slice still applies.
    pub async fn start_live_pty(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
        start: SshPtyStart,
    ) -> Result<TerminalSession, TerminalSessionManagerError> {
        let session = self
            .sessions
            .get(session_id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;
        if session.status == TerminalSessionStatus::Closed {
            return Err(TerminalSessionManagerError::SessionClosed);
        }

        // Promote the runtime entry to Live and spawn the forwarder.
        let SshPtyStart {
            handle,
            output_rx,
            driver,
        } = start;
        let handle: Arc<dyn SshPtyHandle> = Arc::from(handle);
        let (output_tx, _) = broadcast::channel::<OutputFrame>(ATTACHMENT_FANOUT_CAPACITY);
        let replay = Arc::new(Mutex::new(ReplayBuffer::new(ReplayBufferConfig::DEFAULT)));

        let next_seq = {
            let runtimes = self
                .runtimes
                .read()
                .expect("runtime registry lock poisoned");
            runtimes
                .get(&session_id)
                .map(|e| e.next_seq.clone())
                .ok_or(TerminalSessionManagerError::NotFound)?
        };
        let last_seq = Arc::new(AtomicU64::new(0));

        // Spawn the recording writer for this session BEFORE the
        // forwarder so the forwarder owns a clone of the writer's
        // sender from the very first frame. The writer is `Disabled`
        // (zero-cost) when recording is configured-off.
        let recording_writer = match &self.recording {
            Some(runtime) => runtime.writer_for(session_id),
            None => RecordingWriter::disabled(),
        };
        let recording_for_task = recording_writer.clone();

        let sessions_repo = self.sessions.clone();
        let events_repo = self.events.clone();
        let output_tx_for_task = output_tx.clone();
        let next_seq_for_task = next_seq.clone();
        let replay_for_task = replay.clone();
        let last_seq_for_task = last_seq.clone();
        let forwarder_session_id = session_id;
        let forwarder_owner_id = owner_id;
        let forwarder = tokio::spawn(forward_pty_output(
            output_rx,
            output_tx_for_task,
            next_seq_for_task,
            replay_for_task,
            sessions_repo,
            events_repo,
            forwarder_session_id,
            forwarder_owner_id,
            recording_for_task,
            last_seq_for_task,
        ));

        // Mark the persisted row Active. `closed_at` stays NULL on the
        // row; only the close path stamps it.
        self.sessions
            .set_status(session_id, TerminalSessionStatus::Active, None)
            .await?;
        // Re-fetch so the response carries the row the database stamped.
        // Re-filter on owner_id for defense-in-depth.
        let updated = self
            .sessions
            .get(session_id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;

        // Promote the registry entry. If the entry vanished between the
        // initial check and now (concurrent close), tear the bridge
        // down so we don't leak an orphan task. The guard is released
        // BEFORE any await so the compiler can prove the future is
        // `Send`.
        //
        // The pattern: build the LiveRuntime up-front, hand it to the
        // registry under a write lock, and only on the failure path
        // does the local own it again. Dropping the un-installed
        // LiveRuntime fires its `Drop` impl, which aborts the forwarder
        // and the bridge driver — no orphan tasks even on the race.
        let candidate = LiveRuntime {
            handle: handle.clone(),
            output_tx,
            replay,
            forwarder: Some(forwarder),
            driver,
            detach_close: None,
            recording: recording_writer,
            last_seq,
        };
        let leftover = {
            let mut runtimes = self
                .runtimes
                .write()
                .expect("runtime registry lock poisoned");
            if let Some(entry) = runtimes.get_mut(&session_id) {
                entry.snapshot.status = RuntimeSessionStatus::Live;
                entry.live = Some(candidate);
                None
            } else {
                Some(candidate)
            }
        };
        if let Some(leftover) = leftover {
            // Drop the un-installed runtime first so the abort fires
            // before we await on `close()`. This avoids leaving the
            // tasks running while the SSH transport tears down.
            drop(leftover);
            handle.close().await;
            return Err(TerminalSessionManagerError::NotFound);
        }
        Ok(updated)
    }

    /// Mark the live PTY runtime as torn down without removing the
    /// metadata row. Idempotent. Used by the API layer when SSH startup
    /// fails partway through `start_live_pty`'s side-effects so the
    /// session row can still be returned to the operator.
    pub async fn record_pty_start_failed(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
        category: &'static str,
    ) -> Result<(), TerminalSessionManagerError> {
        // Drop any partial live-runtime entry; the bridge is gone.
        if let Some(entry) = self
            .runtimes
            .write()
            .expect("runtime registry lock poisoned")
            .get_mut(&session_id)
        {
            entry.live = None;
            entry.snapshot.status = RuntimeSessionStatus::Ended;
        }

        let session = self
            .sessions
            .get(session_id)
            .await?
            .filter(|s| s.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;
        if session.status == TerminalSessionStatus::Closed {
            return Ok(());
        }

        let now = Utc::now();
        self.sessions
            .set_status(session_id, TerminalSessionStatus::Closed, Some(now))
            .await?;
        let _ = self
            .events
            .create(CreateSessionEvent {
                session_id,
                kind: SessionEventKind::Closed,
                payload: serde_json::json!({
                    "reason": "ssh_start_failed",
                    "category": category,
                }),
            })
            .await;
        // Drop attachments, runtime entry — same shape as close.
        self.attachments
            .write()
            .expect("attachment registry lock poisoned")
            .retain(|_, a| a.session_id != session_id);
        self.runtimes
            .write()
            .expect("runtime registry lock poisoned")
            .remove(&session_id);
        Ok(())
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
            // Use take() so the live runtime's drop runs OUTSIDE the
            // lock — the forwarder abort might otherwise contend.
            let stale = self
                .runtimes
                .write()
                .expect("runtime registry lock poisoned")
                .remove(&id);
            if let Some(stale) = stale {
                self.shutdown_runtime(stale).await;
            }
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

        let removed = self
            .runtimes
            .write()
            .expect("runtime registry lock poisoned")
            .remove(&id);
        if let Some(entry) = removed {
            self.shutdown_runtime(entry).await;
        }
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

    /// Tear down a runtime entry's live PTY (if any) without holding
    /// any registry lock. Best-effort. Drops the [`LiveRuntime`] which
    /// fires its `Drop` impl: forwarder + driver + any pending TTL
    /// close task are aborted as a unit.
    async fn shutdown_runtime(&self, entry: RuntimeEntry) {
        if let Some(mut live) = entry.live {
            // Notify the bridge handle so the SSH session is torn down
            // promptly; the forwarder task observes the resulting
            // `Closed` event and exits.
            live.handle.close().await;
            // Bounded await on the forwarder so every queued PTY frame
            // has been teed into the recording writer BEFORE we send
            // the writer's shutdown signal. Without this barrier the
            // writer can exit before the trailing frames are
            // observable to the recording reader. The deadline is
            // tight (the bridge's output_rx is bounded ≤ 256 frames
            // and `recv()` on a closed channel resolves as fast as the
            // task is scheduled).
            if let Some(forwarder) = live.forwarder.take() {
                let _ = tokio::time::timeout(Duration::from_millis(500), forwarder).await;
            }
            // Bounded shutdown of the recording writer: drain pending
            // queue, flush trailing chunk, write the `closed` marker.
            // The writer's own deadline keeps the close response
            // bounded — the manager never stalls on a slow DB.
            let last_seq = live.last_seq.load(Ordering::SeqCst);
            live.recording.shutdown(last_seq).await;
            let _ = live;
        }
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
    /// Returns whichever attach surface (live or stub) matches the
    /// session's current runtime state. The caller is the WebSocket
    /// route, which uses the returned `live` view (if any) to subscribe
    /// to the broadcast and route input through the SSH handle.
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

        // Cancel any pending TTL close BEFORE we write the attachment
        // row. The TTL task could otherwise fire mid-attach and close
        // the session out from under the new client. The check is
        // cheap (single registry-write lock) and safe to run on every
        // attach — sessions without a pending close just no-op.
        let was_detached_pending_close = self.cancel_pending_close(session.id);

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
        let live_view = self.runtime_view(session.id);
        let live_for_event = live_view.is_some();
        self.events
            .create(CreateSessionEvent {
                session_id: session.id,
                kind: SessionEventKind::Attached,
                payload: serde_json::json!({
                    "attachment_id": attachment.id,
                    "client_info": req.client_info,
                    "remote_addr": req.remote_addr,
                    "stub": !live_for_event,
                    "live": live_for_event,
                }),
            })
            .await?;

        // If this attach landed inside a TTL window, transition the row
        // from `Detached` back to `Active` and append a `Reattached`
        // lifecycle event so the audit log reflects the resume. We do
        // this AFTER the `Attached` event so the audit ordering matches
        // the wire ordering: `attached(new attachment)` →
        // `reattached(session)`.
        if was_detached_pending_close {
            self.sessions
                .set_status(session.id, TerminalSessionStatus::Active, None)
                .await?;
            let _ = self
                .events
                .create(CreateSessionEvent {
                    session_id: session.id,
                    kind: SessionEventKind::Reattached,
                    payload: serde_json::json!({
                        "attachment_id": attachment.id,
                    }),
                })
                .await;
        }

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

        let message = if live_view.is_some() {
            LIVE_PTY_ATTACH_MESSAGE
        } else {
            STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE
        };

        Ok(AttachSessionOutcome {
            session,
            attachment,
            message,
            live: live_view,
        })
    }

    /// Abort any pending TTL close task for `session_id` and return
    /// whether one was found. Pure registry mutation; the lock is held
    /// only across the take/abort. Used by [`Self::attach_session`] to
    /// cancel the close before the new client races with the timer.
    fn cancel_pending_close(&self, session_id: TerminalSessionId) -> bool {
        let close_task = {
            let mut runtimes = self
                .runtimes
                .write()
                .expect("runtime registry lock poisoned");
            runtimes
                .get_mut(&session_id)
                .and_then(|e| e.live.as_mut())
                .and_then(|l| l.detach_close.take())
        };
        match close_task {
            Some(d) => {
                d.handle.abort();
                true
            }
            None => false,
        }
    }

    /// Forward an input byte buffer to the live PTY. Returns
    /// [`TerminalSessionManagerError::PtyNotLive`] if no live runtime is
    /// bound (startup failed, PTY exited, session closed, etc.).
    /// Ownership-gated: foreign-owner ids collapse to `NotFound`.
    pub async fn write_pty_input(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
        bytes: Vec<u8>,
    ) -> Result<(), TerminalSessionManagerError> {
        let handle = self.live_handle_for(owner_id, session_id)?;
        match handle.write_input(bytes).await {
            Ok(()) => Ok(()),
            Err(SshPtyError::BridgeClosed) => Err(TerminalSessionManagerError::PtyNotLive),
            Err(e) => Err(TerminalSessionManagerError::PtyStart(e)),
        }
    }

    /// Apply a window-size change to the live PTY in addition to the
    /// metadata-only [`Self::resize_session`] path. Use this from the
    /// WS resize handler so both the runtime hint AND the remote PTY
    /// stay in sync. Returns `Ok(false)` if no live runtime is bound
    /// (the metadata-only resize still happened).
    pub async fn apply_pty_resize(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
        cols: u16,
        rows: u16,
    ) -> Result<bool, TerminalSessionManagerError> {
        let Some(handle) = self.maybe_live_handle_for(owner_id, session_id)? else {
            return Ok(false);
        };
        match handle.resize(cols, rows).await {
            Ok(()) => Ok(true),
            Err(SshPtyError::BridgeClosed) => Ok(false),
            Err(e) => Err(TerminalSessionManagerError::PtyStart(e)),
        }
    }

    fn live_handle_for(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
    ) -> Result<Arc<dyn SshPtyHandle>, TerminalSessionManagerError> {
        let runtimes = self
            .runtimes
            .read()
            .expect("runtime registry lock poisoned");
        let entry = runtimes
            .get(&session_id)
            .filter(|e| e.snapshot.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;
        let live = entry
            .live
            .as_ref()
            .ok_or(TerminalSessionManagerError::PtyNotLive)?;
        Ok(live.handle.clone())
    }

    fn maybe_live_handle_for(
        &self,
        owner_id: UserId,
        session_id: TerminalSessionId,
    ) -> Result<Option<Arc<dyn SshPtyHandle>>, TerminalSessionManagerError> {
        let runtimes = self
            .runtimes
            .read()
            .expect("runtime registry lock poisoned");
        let entry = runtimes
            .get(&session_id)
            .filter(|e| e.snapshot.owner_id == owner_id)
            .ok_or(TerminalSessionManagerError::NotFound)?;
        Ok(entry.live.as_ref().map(|l| l.handle.clone()))
    }

    fn runtime_view(&self, session_id: TerminalSessionId) -> Option<LiveRuntimeView> {
        let runtimes = self
            .runtimes
            .read()
            .expect("runtime registry lock poisoned");
        runtimes
            .get(&session_id)
            .and_then(|e| e.live.as_ref())
            .map(|l| LiveRuntimeView {
                handle: l.handle.clone(),
                output_tx: l.output_tx.clone(),
                replay: l.replay.clone(),
            })
    }

    /// Snapshot the replay buffer for a session at the caller's
    /// `last_seen_seq`. Returns `Ok(None)` when the session has no live
    /// PTY (stub session, or PTY torn down), so the WS handler can fall
    /// back to live-only attach. The lock is held only for the snapshot
    /// — callers should NOT keep the lock across `.await`.
    pub fn replay_since(
        &self,
        session_id: TerminalSessionId,
        last_seen_seq: Option<u64>,
    ) -> Option<Result<ReplayRange, ReplayWindowLost>> {
        let view = self.runtime_view(session_id)?;
        let buf = view.replay.lock().expect("replay buffer lock poisoned");
        Some(buf.replay_since(last_seen_seq))
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

    /// Detach a single attachment AND, if this leaves a live PTY with
    /// zero attached clients, schedule a bounded TTL close.
    ///
    /// This is the lifecycle helper the WebSocket route uses on every
    /// detach path (explicit `Detach` frame, socket-drop cleanup tail).
    ///
    /// Behaviour:
    /// * `detach_session` runs first and is idempotent against the
    ///   attachment row.
    /// * If the call observed `already_detached == true`, the manager
    ///   does NOT schedule a close — that path runs every time the WS
    ///   handler's cleanup tail fires after an explicit detach, and
    ///   re-scheduling would duplicate the lifecycle bookkeeping.
    /// * If the session does not have a live PTY (stub session, or PTY
    ///   already torn down by the forwarder), the manager does NOT
    ///   schedule a close — there is nothing live to reap.
    /// * If other attachments are still live for this session, the
    ///   manager does NOT schedule a close.
    /// * Otherwise: transition the session row to `Detached`, append a
    ///   single `Detached` (already done by `detach_session`) lifecycle
    ///   event, and spawn a TTL close task. The PTY survives until the
    ///   timer fires OR the next [`Self::attach_session`] cancels it OR
    ///   an explicit close arrives. Re-running this helper on the same
    ///   session while the close is already scheduled is a no-op.
    ///
    /// Takes `&Arc<Self>` so the spawned TTL task can hold a `Weak<Self>`
    /// back to the manager — the task is cancellable in either
    /// direction (manager drops → upgrade fails → task exits silently;
    /// reattach → handle aborted).
    pub async fn detach_attachment(
        self: &Arc<Self>,
        owner_id: UserId,
        session_id: TerminalSessionId,
        attachment_id: TerminalSessionAttachmentId,
        last_seen_seq: Option<i64>,
    ) -> Result<DetachOutcome, TerminalSessionManagerError> {
        let detach = self
            .detach_session(owner_id, session_id, attachment_id, last_seen_seq)
            .await?;

        // Decide whether to schedule a TTL close. The decision is made
        // under read locks then released BEFORE the close-or-spawn
        // section so the future stays Send.
        let should_schedule = if detach.already_detached {
            false
        } else {
            let any_remaining_attachment = self
                .attachments
                .read()
                .expect("attachment registry lock poisoned")
                .values()
                .any(|a| a.session_id == session_id);
            let session_has_live_pty = self
                .runtimes
                .read()
                .expect("runtime registry lock poisoned")
                .get(&session_id)
                .and_then(|e| e.live.as_ref())
                .is_some();
            !any_remaining_attachment && session_has_live_pty
        };

        let detached_pending_close = if should_schedule {
            self.schedule_detach_close(session_id, owner_id).await
        } else {
            None
        };

        Ok(DetachOutcome {
            detach,
            detached_pending_close,
        })
    }

    /// Mark a session row `Detached` and spawn the TTL close task.
    ///
    /// Returns the new [`DetachInfo`] when the close was scheduled, or
    /// `None` when the session already has a pending close (idempotent),
    /// when the live runtime vanished between the detach and this call,
    /// or when the row mutate failed (in which case the error is
    /// logged operator-side; the API caller still sees a successful
    /// detach because the attachment-level bookkeeping landed).
    async fn schedule_detach_close(
        self: &Arc<Self>,
        session_id: TerminalSessionId,
        owner_id: UserId,
    ) -> Option<DetachInfo> {
        // Persist the `Detached` transition first so the row reflects
        // reality even if the spawn races with a future restart. A
        // failure here is operator-visible but non-fatal for safety:
        // the TTL task still runs and `close_session` re-issues the
        // status write at expiry, so the session never escapes the
        // bounded window even when this transient write fails. The
        // audit-log oddness — a `Reattached` event landing without a
        // preceding `Detached` row state — is logged at `error` so
        // operator dashboards surface the inconsistency.
        if let Err(err) = self
            .sessions
            .set_status(session_id, TerminalSessionStatus::Detached, None)
            .await
        {
            tracing::error!(
                ?err,
                %session_id,
                "failed to mark session detached on final detach; row may briefly stay Active inside the TTL window",
            );
        }

        let now = Utc::now();
        let ttl = self.detach_ttl;
        let info = DetachInfo {
            detached_at: now,
            expires_at: now + chrono::Duration::from_std(ttl).unwrap_or_default(),
        };

        // Spawn the TTL close task. The task holds a `Weak<Self>` so a
        // manager drop releases it cleanly without the task pinning the
        // registry alive. On wake-up the upgrade is checked; if the
        // manager is gone the task exits silently.
        let weak = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let Some(mgr) = weak.upgrade() else {
                return;
            };
            mgr.expire_detach_close(session_id, owner_id).await;
        });

        // Install the close task on the live runtime. If a previous
        // detach already scheduled one (idempotent re-detach), abort
        // the new task and keep the original deadline so the close
        // doesn't drift forward on every cleanup-tail run.
        let mut runtimes = self
            .runtimes
            .write()
            .expect("runtime registry lock poisoned");
        let Some(live) = runtimes.get_mut(&session_id).and_then(|e| e.live.as_mut()) else {
            // Live runtime vanished between detach and now — close task
            // is meaningless; abort it and surface None.
            handle.abort();
            return None;
        };
        if let Some(existing) = live.detach_close.as_ref() {
            let kept = existing.info;
            handle.abort();
            Some(kept)
        } else {
            live.detach_close = Some(DetachClose { info, handle });
            Some(info)
        }
    }

    /// TTL task body. Closes the session iff it is still in the
    /// detached-pending-close window — a reattach in the meantime
    /// would have aborted the task, but `tokio::sync::abort` is a
    /// best-effort signal and a racing wake-up still hits this path.
    /// Idempotency is preserved because `close_session` is itself
    /// idempotent and the pre-check filters out the reattached case.
    async fn expire_detach_close(&self, session_id: TerminalSessionId, owner_id: UserId) {
        // If the runtime no longer has a `detach_close` entry, a
        // reattach beat us here. Bail out before touching the DB so the
        // audit log doesn't pick up a Closed event for a session that
        // is now Active.
        let still_pending = self
            .runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .get(&session_id)
            .and_then(|e| e.live.as_ref())
            .map(|l| l.detach_close.is_some())
            .unwrap_or(false);
        if !still_pending {
            return;
        }
        match self.close_session(session_id, owner_id).await {
            Ok(_) => {}
            Err(err) => {
                warn!(
                    ?err,
                    %session_id,
                    "failed to close session on detach TTL expiry"
                );
            }
        }
    }

    /// Public read of the detached-window state for a session. Returns
    /// `Some(info)` while the session is in the bounded TTL window,
    /// `None` when it is attached, has no live PTY, or the close
    /// already fired.
    #[must_use]
    pub fn detach_state(&self, session_id: TerminalSessionId) -> Option<DetachInfo> {
        self.runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .get(&session_id)
            .and_then(|e| e.live.as_ref())
            .and_then(|l| l.detach_close.as_ref())
            .map(|d| d.info)
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
        //
        // While we hold the lock briefly: snapshot the recording writer
        // and the live seq counter so a `resized` marker can be appended
        // OUTSIDE the lock. The marker's seq is the latest output seq
        // observed by the forwarder so a reader paging by ascending
        // `seq >= n + 1` sees the resize alongside the surrounding
        // chunk window. If no live runtime exists (or the writer is
        // disabled), the `record_marker` call is a no-op.
        let recording_marker_target = {
            let mut guard = self
                .runtimes
                .write()
                .expect("runtime registry lock poisoned");
            if let Some(entry) = guard.get_mut(&session.id) {
                entry.snapshot.cols = cols;
                entry.snapshot.rows = rows;
                entry
                    .live
                    .as_ref()
                    .map(|live| (live.recording.clone(), live.last_seq.load(Ordering::SeqCst)))
            } else {
                None
            }
        };
        if let Some((writer, latest)) = recording_marker_target {
            // The schema requires `seq >= 1` for non-`started` markers;
            // record_marker enforces the same and silently drops a
            // pre-output resize.
            let marker_seq = latest.max(1);
            writer
                .record_marker(
                    relayterm_core::terminal_recording::TerminalRecordingMarkerKind::Resized,
                    marker_seq,
                    serde_json::json!({
                        "cols": cols,
                        "rows": rows,
                    }),
                )
                .await;
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

    /// Read the current runtime snapshot, if any. Returns a clone — the
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
            .map(|e| e.snapshot.clone())
    }

    /// Live PTY runtime view for an active session, if a PTY is bound.
    /// Returns `None` for sessions without a live PTY (stub or after
    /// teardown).
    #[must_use]
    pub fn live(&self, id: TerminalSessionId) -> Option<LiveRuntimeView> {
        self.runtime_view(id)
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

    /// Per-user ceiling on concurrent live PTY runtime entries. The
    /// configured operator value (defaults to
    /// [`DEFAULT_MAX_LIVE_PTY_PER_USER`]). Phase 1B.1 of
    /// `docs/session-quotas.md` § 4.1.
    #[must_use]
    pub fn max_live_pty_per_user(&self) -> NonZeroU32 {
        self.max_live_pty_per_user
    }

    /// Count of an owner's runtime-registry entries whose live PTY is
    /// currently bound (`entry.live.is_some()`). Equivalent to "rows in
    /// the registry whose `snapshot.status == RuntimeSessionStatus::Live`"
    /// because `start_live_pty` sets both atomically under the same
    /// write-lock guard.
    ///
    /// Used by the create route (Phase 1B.1 quota enforcement) to
    /// refuse a new session when this would exceed
    /// [`Self::max_live_pty_per_user`]. The check sits AFTER ownership
    /// and host-key gating, and BEFORE vault decrypt or SSH side
    /// effects, so the refusal does no outbound work and cannot be
    /// used to probe for foreign / disabled / untrusted profiles.
    ///
    /// O(N) over the registry under the existing `RwLock` read guard;
    /// the registry is bounded by the deployment-wide configuration so
    /// the scan is a small handful of comparisons.
    #[must_use]
    pub fn count_live_pty_for_user(&self, owner_id: UserId) -> usize {
        self.runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .values()
            .filter(|entry| entry.snapshot.owner_id == owner_id && entry.live.is_some())
            .count()
    }

    /// Per-user ceiling on concurrent in-flight starting sessions (Phase
    /// 1B.2a quota — `docs/session-quotas.md` § 4.3). The configured
    /// operator value (defaults to [`DEFAULT_MAX_STARTING_PER_USER`]).
    #[must_use]
    pub fn max_starting_per_user(&self) -> NonZeroU32 {
        self.max_starting_per_user
    }

    /// Count of an owner's runtime-registry entries that hold a
    /// `Starting` snapshot AND have NOT yet bound a live PTY. Equivalent
    /// to "rows in the registry whose `live = None` AND `snapshot.status
    /// == RuntimeSessionStatus::Starting`" — the disjoint complement of
    /// the live-PTY count, so the two quotas never double-count
    /// (`docs/session-quotas.md` § 4.1 / § 4.3).
    ///
    /// Used by the create route (Phase 1B.2a quota enforcement) to
    /// refuse a new session when this would exceed
    /// [`Self::max_starting_per_user`]. Same ordering posture as the
    /// per-user live counter: AFTER ownership and host-key gating, and
    /// BEFORE vault decrypt or SSH side effects, so the refusal does
    /// no outbound work and cannot be used to probe for foreign /
    /// disabled / untrusted profiles.
    ///
    /// O(N) over the registry under the existing `RwLock` read guard;
    /// the registry is bounded by the operator quota so the scan is a
    /// small handful of comparisons.
    #[must_use]
    pub fn count_starting_for_user(&self, owner_id: UserId) -> usize {
        self.runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .values()
            .filter(|entry| {
                entry.snapshot.owner_id == owner_id
                    && entry.live.is_none()
                    && entry.snapshot.status == RuntimeSessionStatus::Starting
            })
            .count()
    }

    /// Deployment-wide ceiling on concurrent live PTY runtime entries
    /// across ALL owners. The configured operator value (defaults to
    /// [`DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT`]). Phase 1B.2b of
    /// `docs/session-quotas.md` § 4.2.
    #[must_use]
    pub fn max_live_pty_per_deployment(&self) -> NonZeroU32 {
        self.max_live_pty_per_deployment
    }

    /// Count of runtime-registry entries whose live PTY is currently
    /// bound (`entry.live.is_some()`), summed across ALL owners.
    /// Equivalent to "rows in the registry whose `snapshot.status ==
    /// RuntimeSessionStatus::Live`" because `start_live_pty` sets both
    /// atomically under the same write-lock guard.
    ///
    /// Used by the create route (Phase 1B.2b deployment quota
    /// enforcement) to refuse a new session when this would exceed
    /// [`Self::max_live_pty_per_deployment`]. The check sits AFTER the
    /// per-user live quota and BEFORE the per-user starting quota
    /// (`docs/session-quotas.md` § 6.2 ordering) so a refusal does no
    /// outbound work and cannot be used to probe for foreign /
    /// disabled / untrusted profiles.
    ///
    /// Counts active AND detached live PTYs equally (both are
    /// `RuntimeSessionStatus::Live` in the registry and hold the same
    /// resource tuple). Does NOT count `Starting` placeholders,
    /// `Closed` sessions (their registry entries are gone), recording
    /// chunk-writer tasks, or `terminal_session_attachments` rows.
    ///
    /// O(N) over the registry under the existing `RwLock` read guard;
    /// the registry is bounded by the deployment-wide cap so the scan
    /// is a small handful of comparisons.
    #[must_use]
    pub fn count_live_pty_total(&self) -> usize {
        self.runtimes
            .read()
            .expect("runtime registry lock poisoned")
            .values()
            .filter(|entry| entry.live.is_some())
            .count()
    }
}

/// Drain a bridge's `output_rx`, stamp monotonic sequence numbers, and
/// fan out to attachments via the per-session broadcast. Exits when
/// `output_rx.recv()` returns `None` (bridge tore down) or when the
/// broadcast `output_tx` has no remaining subscribers AND the bridge
/// has emitted `Closed` — the latter signals end of stream.
///
/// On exit, transitions the session row to `Closed` (idempotent) and
/// appends a `closed` lifecycle event with the bridge's reason. The
/// runtime entry is NOT removed here — close_session handles that to
/// keep registry mutation centralised.
#[allow(clippy::too_many_arguments)]
async fn forward_pty_output(
    mut output_rx: tokio::sync::mpsc::Receiver<SshPtyEvent>,
    output_tx: broadcast::Sender<OutputFrame>,
    next_seq: Arc<AtomicU64>,
    replay: Arc<Mutex<ReplayBuffer>>,
    sessions: Arc<dyn TerminalSessionRepository>,
    events: Arc<dyn SessionEventRepository>,
    session_id: TerminalSessionId,
    owner_id: UserId,
    recording: RecordingWriter,
    last_seq: Arc<AtomicU64>,
) {
    let mut closed_reason: Option<ClosedReason> = None;

    while let Some(evt) = output_rx.recv().await {
        match evt {
            SshPtyEvent::Output(bytes) => {
                let seq = next_seq.fetch_add(1, Ordering::SeqCst);
                let frame = OutputFrame {
                    seq,
                    data: Arc::from(bytes.into_boxed_slice()),
                };
                // Mirror the frame into the bounded replay ring BEFORE
                // fanning out so a transient subscriber that races with
                // attach can recover via the replay path. The lock is
                // held briefly and never across an `.await`.
                {
                    let mut buf = replay.lock().expect("replay buffer lock poisoned");
                    buf.push(frame.clone());
                }
                // `send` returns the number of subscribers reached; we
                // ignore — broadcast::Sender has no failure mode short
                // of "no subscribers", which is fine.
                let _ = output_tx.send(frame.clone());
                // Tee the frame into the durable recording AFTER the
                // live wire and replay ring. The writer is bounded and
                // non-blocking — a full queue drops the frame for
                // recording only and brackets it with a `replay_gap`
                // marker. Live attachments never wait on recording.
                last_seq.store(seq, Ordering::SeqCst);
                recording.record_output(seq, &frame.data).await;
            }
            SshPtyEvent::Exit { status: _ } => {
                // Recorded operator-side via tracing only. The wire signal
                // for the renderer is the upcoming Closed event.
                // ExitStatus is informational; do NOT log raw output.
            }
            SshPtyEvent::Closed { reason } => {
                closed_reason = Some(reason);
                // Don't break — drain any final Output bytes that may
                // arrive after Closed (russh may emit them before
                // `wait()` returns None).
            }
        }
    }

    // Bridge is gone. Mark the session closed in the DB so a stale
    // Active row doesn't survive a remote shell exit.
    let reason_str = match closed_reason {
        Some(ClosedReason::RemoteEof) => "remote_eof",
        Some(ClosedReason::TransportError) => "transport_error",
        Some(ClosedReason::LocalClose) | None => "local_close",
    };

    // Fetch the session to confirm it's still owned by the user (defense-
    // in-depth) and not already closed. Treat any error as best-effort —
    // we don't have a request to fail.
    match sessions.get(session_id).await {
        Ok(Some(session))
            if session.owner_id == owner_id && session.status != TerminalSessionStatus::Closed =>
        {
            let now = Utc::now();
            if let Err(err) = sessions
                .set_status(session_id, TerminalSessionStatus::Closed, Some(now))
                .await
            {
                warn!(?err, %session_id, "failed to mark session closed after pty teardown");
            }
            if let Err(err) = events
                .create(CreateSessionEvent {
                    session_id,
                    kind: SessionEventKind::Closed,
                    payload: serde_json::json!({
                        "reason": "pty_teardown",
                        "category": reason_str,
                    }),
                })
                .await
            {
                warn!(?err, %session_id, "failed to append closed event after pty teardown");
            }
        }
        Ok(_) => { /* already closed or vanished — nothing to do */ }
        Err(err) => warn!(?err, %session_id, "failed to read session row during pty teardown"),
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
