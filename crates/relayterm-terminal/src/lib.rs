//! Backend-owned terminal session orchestrator.
//!
//! `TerminalSessionManager` is the single owner of terminal-session
//! lifecycle. It writes the metadata row in Postgres, appends the
//! lifecycle [`SessionEvent`](relayterm_core::session_event::SessionEvent)s,
//! and tracks each live session in an in-memory runtime registry. The
//! registry holds NO live `russh::Channel`, no PTY descriptor, and no
//! replay ring buffer — those will land in later slices when real PTY
//! allocation is wired up. Today the manager creates a *placeholder*
//! runtime entry so the lifecycle surface (create → close) can be
//! exercised end-to-end without touching SSH.
//!
//! ## Ownership boundary
//!
//! - **Postgres** stores metadata and history (status, cols/rows hint,
//!   created_at/last_seen_at/closed_at, the append-only session_events
//!   log).
//! - **The manager's in-memory registry** owns runtime state. It is NOT
//!   durable: a backend restart clears the registry, so a row that was
//!   `starting` at restart time is operator-visible as a stale metadata
//!   record until it's explicitly closed. A future recovery policy may
//!   sweep these.
//!
//! Callers above this crate (the API handlers) MUST NOT cache or mutate
//! anything that conceptually belongs to the runtime registry. They call
//! `create_session` / `close_session` and read the returned domain value.

pub mod manager;
pub mod recording;
pub mod replay;
pub mod retention;
pub mod retention_worker;

pub use manager::{
    AttachSessionOutcome, AttachSessionRequest, AttachmentRuntime, CloseTerminalSessionOutcome,
    CreateTerminalSessionOutcome, CreateTerminalSessionRequest,
    DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT, DEFAULT_MAX_LIVE_PTY_PER_USER,
    DEFAULT_MAX_STARTING_PER_USER, DETACHED_LIVE_PTY_TTL, DetachInfo, DetachOutcome,
    DetachSessionOutcome, LIVE_PTY_ATTACH_MESSAGE, LIVE_PTY_CREATE_MESSAGE, LiveRuntimeView,
    ResizeSessionOutcome, RuntimeSessionStatus, STUB_PTY_NOT_IMPLEMENTED_ATTACH_MESSAGE,
    STUB_PTY_NOT_IMPLEMENTED_MESSAGE, TerminalSessionManager, TerminalSessionManagerError,
    TerminalSessionRuntime,
};
pub use recording::{RecordingRuntime, RecordingWriter, RecordingWriterConfig, replay_gap_reason};
pub use replay::{OutputFrame, ReplayBuffer, ReplayBufferConfig, ReplayRange, ReplayWindowLost};
pub use retention::{
    RecordingRetentionSweepSummary, SweepCadence, run_recording_retention_startup_sweep,
    run_recording_retention_sweep,
};
pub use retention_worker::{
    AdvisoryLockOutcome, RECORDING_RETENTION_ADVISORY_LOCK_KEY, RecordingRetentionPeriodicConfig,
    RecordingRetentionTickOutcome, RetentionAdvisoryLock, RetentionPeriodicWorkerHandle, SweepWork,
    periodic_worker_config_if_enabled, run_one_periodic_tick,
    spawn_recording_retention_periodic_worker,
};
