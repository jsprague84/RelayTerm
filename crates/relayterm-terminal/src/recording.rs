//! Bounded, non-blocking PTY-output recording writer.
//!
//! See `docs/terminal-recording.md` Section 6.1 for the contract.
//!
//! ## Architecture
//!
//! The writer is a *tee*: the manager's PTY forwarder hands each
//! sequenced [`OutputFrame`](crate::replay::OutputFrame) to the writer
//! AFTER fanning it out to live attachments and the in-memory replay
//! ring. The writer does not gate the live wire — when its bounded
//! queue overflows OR a DB write fails, frames are dropped *for
//! recording only* and bracketed by a [`TerminalRecordingMarkerKind::ReplayGap`]
//! marker so the replay viewer surfaces honest discontinuity instead of
//! faking continuity.
//!
//! ## Privacy
//!
//! - PTY OUTPUT bytes flow into the writer ONLY via [`RecordingWriter::record_output`].
//! - The bytes never reach a `tracing::*` line, an `audit_events.payload`,
//!   a thrown `Error.message`, or any `Debug` output. The
//!   [`CreateTerminalRecordingChunk`] input redacts the payload in `Debug`.
//! - Marker payloads are public-safe metadata only (counts, dims, reason
//!   codes). The writer constructs them field-by-field from explicit
//!   primitives — never `serde_json::to_value` against an arbitrary bag.
//! - On error, log lines name the session id and a static category tag
//!   only; never the bytes, never repository internals, never SSH banners.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use relayterm_core::ids::TerminalSessionId;
use relayterm_core::repository::{
    CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, TerminalRecordingRepository,
};
use relayterm_core::terminal_recording::{
    TerminalRecordingCompression, TerminalRecordingMarkerKind, TerminalRecordingPayloadEncryption,
};
use serde_json::{Value as JsonValue, json};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;

/// Lower bound on the bounded command queue between the producer
/// (forwarder) and the writer task.
///
/// Sized so a transient burst (a remote shell echoing a screenful)
/// cannot easily overflow the queue under healthy DB latency, but a
/// stalled writer (DB outage, long network delay) reaches `replay_gap`
/// before unbounded memory is allocated. Each command holds at most one
/// frame's bytes — under the 1 MiB live-wire frame cap, a full queue is
/// at most `QUEUE_CAPACITY * 1 MiB` of additional memory in flight.
const QUEUE_CAPACITY: usize = 256;

/// Bounded shutdown deadline. The writer task drains its queue, flushes
/// the trailing chunk, and writes the `closed` marker; if any of these
/// blocks longer than this, the manager's close response should not
/// stall waiting for it.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// Configuration for an enabled recording writer.
///
/// Mirrors the relevant fields from `[terminal_recording]` in the
/// backend config. The writer never reads the source config struct
/// directly — keeping a renderer-agnostic value type avoids leaking
/// the API/config crate into `relayterm-terminal`.
#[derive(Debug, Clone, Copy)]
pub struct RecordingWriterConfig {
    /// Soft chunk flush target in bytes. When a chunk's accumulated
    /// payload reaches this size, the writer task flushes it on the
    /// next tick.
    pub chunk_target_bytes: u32,
    /// Defence-in-depth row-size cap. The writer NEVER produces a chunk
    /// payload larger than this; a single oversized frame is dropped
    /// (with a `replay_gap` marker) rather than split across rows.
    pub chunk_hard_cap_bytes: u32,
}

impl RecordingWriterConfig {
    /// Sane defaults matching the design doc Section 6.1 numbers
    /// (`chunk_target_bytes = 64 KiB`, `chunk_hard_cap_bytes = 2 MiB`).
    /// Production code MUST plumb operator-tuned values through; this
    /// constant exists so test fixtures aren't forced to invent
    /// numbers.
    pub const DEFAULT: Self = Self {
        chunk_target_bytes: 64 * 1024,
        chunk_hard_cap_bytes: 2 * 1024 * 1024,
    };
}

/// Reasons a `replay_gap` marker is written. Stable categorical strings
/// — pinned in tests so a future helpful rewording is forced through
/// review.
pub mod replay_gap_reason {
    /// The bounded queue between producer and writer overflowed.
    pub const WRITER_OVERFLOW: &str = "writer_overflow";
    /// A repository write (chunk or marker) failed; the writer logged a
    /// safe metadata-only `warn!` and bracketed the lost seq range with
    /// this marker.
    pub const WRITER_ERROR: &str = "writer_error";
    /// A single live frame exceeded the configured `chunk_hard_cap_bytes`
    /// and could not be persisted as a single chunk. The frame is dropped
    /// for recording only; the live wire is unaffected.
    pub const FRAME_OVERSIZED: &str = "frame_oversized";
}

/// Static error categories used in operator-side log lines. Keeping these
/// string-typed avoids leaking driver text or constraint names into the
/// log; the underlying `RepositoryError` Debug is also redaction-aware,
/// but the writer never logs it directly.
mod error_category {
    pub(super) const APPEND_CHUNK: &str = "append_chunk";
    pub(super) const APPEND_MARKER: &str = "append_marker";
}

/// Composable factory for per-session writers.
///
/// Held by [`crate::TerminalSessionManager`] so a single recording-enabled
/// runtime fans out one writer per live session. The runtime is `Clone`
/// (everything behind `Arc`) so the manager can be cheap to share.
#[derive(Clone)]
pub struct RecordingRuntime {
    repository: Arc<dyn TerminalRecordingRepository>,
    config: RecordingWriterConfig,
}

impl fmt::Debug for RecordingRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordingRuntime")
            .field("config", &self.config)
            .finish()
    }
}

impl RecordingRuntime {
    #[must_use]
    pub fn new(
        repository: Arc<dyn TerminalRecordingRepository>,
        config: RecordingWriterConfig,
    ) -> Self {
        Self { repository, config }
    }

    /// Spawn a fresh writer task for `session_id`. Always returns an
    /// enabled writer — callers that want a no-op writer should hold an
    /// `Option<RecordingRuntime>` and call [`RecordingWriter::disabled`]
    /// when the option is `None`.
    #[must_use]
    pub fn writer_for(&self, session_id: TerminalSessionId) -> RecordingWriter {
        RecordingWriter::enabled(self.repository.clone(), session_id, self.config)
    }
}

/// Disabled-mode writer, or handle to an enabled writer's runtime task.
///
/// `Clone` so the manager can hand the same writer to the forwarder and
/// retain a separate handle for lifecycle marker calls (`record_marker`,
/// `shutdown`). The disabled variant is zero-cost (`Arc<()>` clone).
///
/// `Debug` is manual: it formats only the variant tag — never config
/// values that might disclose deployment posture beyond what the
/// runtime already exposes.
#[derive(Clone)]
pub struct RecordingWriter {
    inner: WriterInner,
}

#[derive(Clone)]
enum WriterInner {
    Disabled,
    Enabled(Arc<EnabledHandle>),
}

struct EnabledHandle {
    tx: mpsc::Sender<WriterCommand>,
    /// Tracks the consecutive seq range dropped due to a full queue
    /// (and not yet bracketed by a `replay_gap` marker). `std::sync::Mutex`
    /// because no caller holds the guard across an `.await` — every
    /// helper takes the lock, mutates the `Option<PendingGap>`, and
    /// drops the guard before returning. AGENTS.md "Critical gotchas"
    /// rule: `tokio::sync::Mutex` only when holding the lock across
    /// `.await`.
    pending_gap: Mutex<Option<PendingGap>>,
    /// Background task handle. Owned by the manager-side `RecordingWriter`
    /// only; held in an `Option<JoinHandle>` behind `std::sync::Mutex` so
    /// the shutdown path can `take()` it without an `.await` while
    /// the lock is held. AGENTS.md prohibits `tokio::spawn`-and-forget
    /// for long-lived tasks; this is the orchestrator-side tracker.
    task: Mutex<Option<JoinHandle<()>>>,
    session_id: TerminalSessionId,
    /// Per-writer cap captured at construction. Reading it from the
    /// producer hot path avoids a lock; the value is set once and
    /// never mutated.
    hard_cap_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct PendingGap {
    /// Inclusive lowest seq that was dropped. `>= 1`.
    from_seq: u64,
    /// Inclusive highest seq that was dropped. `>= from_seq`.
    to_seq: u64,
    /// Categorical reason; one of the [`replay_gap_reason`] strings.
    reason: &'static str,
}

impl fmt::Debug for RecordingWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            WriterInner::Disabled => f
                .debug_struct("RecordingWriter")
                .field("mode", &"disabled")
                .finish(),
            WriterInner::Enabled(h) => f
                .debug_struct("RecordingWriter")
                .field("mode", &"enabled")
                .field("session_id", &h.session_id)
                .finish(),
        }
    }
}

/// Internal command shape. Output frames carry an optional `gap_before`
/// so the writer task can flush its current chunk and emit a
/// `replay_gap` marker AHEAD of the new frame, preserving seq ordering.
enum WriterCommand {
    Output {
        seq: u64,
        data: Vec<u8>,
        /// `Some(gap)` means: before processing this frame, flush the
        /// open chunk (if any) and append a `replay_gap` marker covering
        /// the gap. The marker's `seq` is set to `gap.to_seq` so a
        /// reader that filters by `seq >= n + 1` lands on the marker
        /// when the bookmark was inside the gap.
        gap_before: Option<PendingGap>,
    },
    Marker {
        kind: TerminalRecordingMarkerKind,
        seq: i64,
        payload: JsonValue,
    },
    Shutdown {
        last_seq: i64,
        ack: oneshot::Sender<()>,
    },
}

impl RecordingWriter {
    /// Construct a no-op writer. Every method returns immediately;
    /// no task is spawned, no repository is touched.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            inner: WriterInner::Disabled,
        }
    }

    /// Construct an enabled writer backed by the given repository.
    ///
    /// Spawns one [`tokio`] task that owns the chunk batcher and writes
    /// chunk + marker rows. The task exits when [`Self::shutdown`] is
    /// called OR when every clone of the writer's command sender has
    /// been dropped (best-effort cleanup if the manager forgets to
    /// shut down).
    ///
    /// Immediately enqueues a `started` marker at `seq = 0` so a reader
    /// that pages markers by ascending `seq` always observes the
    /// recording's beginning. The session row is the FK target; the
    /// caller must ensure it exists before constructing the writer.
    pub fn enabled(
        repository: Arc<dyn TerminalRecordingRepository>,
        session_id: TerminalSessionId,
        config: RecordingWriterConfig,
    ) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let task = tokio::spawn(writer_task(rx, repository, session_id, config));
        let handle = Arc::new(EnabledHandle {
            tx: tx.clone(),
            pending_gap: Mutex::new(None),
            task: Mutex::new(Some(task)),
            session_id,
            hard_cap_bytes: config.chunk_hard_cap_bytes,
        });
        let writer = Self {
            inner: WriterInner::Enabled(handle),
        };
        // Seed the started marker. Best-effort — a queue full at
        // construction time would only happen if the runtime was
        // instantly hostile, in which case the writer is already
        // useless.
        //
        // Payload is intentionally minimal: the schema only requires
        // a JSON object and the chunk-sizing config is internal
        // posture that we keep out of persisted rows on principle.
        let _ = tx.try_send(WriterCommand::Marker {
            kind: TerminalRecordingMarkerKind::Started,
            seq: 0,
            payload: json!({}),
        });
        writer
    }

    /// Returns `true` if this writer drops every call (no task running,
    /// no DB writes). Useful for tests and diagnostic logs.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self.inner, WriterInner::Enabled(_))
    }

    /// Tee one PTY output frame. Must be called AFTER the live broadcast
    /// fanout so the writer cannot block the wire even with a healthy
    /// queue. `seq` MUST match the seq the forwarder stamped on the
    /// fanout frame.
    ///
    /// Drop semantics:
    /// 1. If the queue is full, the bytes are dropped and the gap
    ///    tracker is extended. The next successful enqueue carries the
    ///    accumulated gap range as `gap_before`, so the writer task
    ///    flushes the open chunk and emits a `replay_gap` marker before
    ///    the new frame.
    /// 2. If `bytes.len() > chunk_hard_cap_bytes`, the frame is dropped
    ///    and bracketed with a `frame_oversized` `replay_gap` marker.
    ///    The marker is best-effort; if the queue is also full the gap
    ///    range still extends and rolls into a future enqueue.
    pub async fn record_output(&self, seq: u64, bytes: &[u8]) {
        let WriterInner::Enabled(handle) = &self.inner else {
            return;
        };
        debug_assert!(seq >= 1, "output seq numbering starts at 1");

        // Single-frame oversize check. Per design Section 6.1 the live
        // wire caps a frame at 1 MiB and the recording hard cap is at
        // least 1 MiB + envelope budget, so a legitimate workload
        // never trips this. If it ever does, drop the frame for
        // recording only and bracket it with a `frame_oversized` gap.
        if bytes.len() > handle.config_hard_cap() {
            extend_gap(&handle.pending_gap, seq, replay_gap_reason::FRAME_OVERSIZED);
            return;
        }

        let gap_before = take_gap(&handle.pending_gap);
        let cmd = WriterCommand::Output {
            seq,
            data: bytes.to_vec(),
            gap_before,
        };
        match handle.tx.try_send(cmd) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                // Restore the gap (we took it but did not enqueue) and
                // extend it with this frame.
                if let WriterCommand::Output {
                    gap_before: Some(g),
                    ..
                } = cmd
                {
                    restore_gap(&handle.pending_gap, g);
                }
                extend_gap(&handle.pending_gap, seq, replay_gap_reason::WRITER_OVERFLOW);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Task is gone. Nothing further we can do — log once
                // operator-side and let subsequent calls also no-op.
                // Tracing format intentionally never names bytes.
                warn!(
                    %handle.session_id,
                    "recording writer task closed before output frame"
                );
            }
        }
    }

    /// Append a metadata-only marker. Best-effort: a full queue drops
    /// the marker (markers are metadata; the chunk stream is what
    /// reconstructs the recording). Never blocks the caller.
    ///
    /// `seq` MUST be the highest output seq observed at the time the
    /// marker fires. The schema CHECK requires `seq >= 1` for every
    /// kind except `started`, which is the only kind allowed at
    /// `seq = 0`. The writer enforces the same invariant.
    pub async fn record_marker(
        &self,
        kind: TerminalRecordingMarkerKind,
        seq: u64,
        payload: JsonValue,
    ) {
        let WriterInner::Enabled(handle) = &self.inner else {
            return;
        };
        // The schema rejects seq=0 for non-started kinds; refuse here
        // too rather than send a row that the DB will reject anyway.
        if seq == 0 && !kind.allows_seq_zero() {
            warn!(
                %handle.session_id,
                kind = kind.as_str(),
                "ignoring recording marker with seq=0 (only `started` is allowed at seq 0)"
            );
            return;
        }
        let seq_i64 = match i64::try_from(seq) {
            Ok(v) => v,
            Err(_) => {
                warn!(
                    %handle.session_id,
                    kind = kind.as_str(),
                    "ignoring recording marker with out-of-range seq"
                );
                return;
            }
        };
        match handle.tx.try_send(WriterCommand::Marker {
            kind,
            seq: seq_i64,
            payload,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    %handle.session_id,
                    kind = kind.as_str(),
                    "recording marker dropped (writer queue full)"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!(
                    %handle.session_id,
                    kind = kind.as_str(),
                    "recording marker dropped (writer task closed)"
                );
            }
        }
    }

    /// Drain the queue, flush the trailing chunk, write a `closed`
    /// marker at `last_seq`, and stop the task.
    ///
    /// Bounded by [`SHUTDOWN_DEADLINE`]: the manager's close response
    /// will not stall arbitrarily on a slow DB. On timeout the task is
    /// aborted and a metadata-only `warn!` is logged.
    pub async fn shutdown(&self, last_seq: u64) {
        let WriterInner::Enabled(handle) = &self.inner else {
            return;
        };
        let last_seq_i64 = i64::try_from(last_seq).unwrap_or(i64::MAX);
        let (ack_tx, ack_rx) = oneshot::channel::<()>();
        let send_result = handle
            .tx
            .send(WriterCommand::Shutdown {
                last_seq: last_seq_i64,
                ack: ack_tx,
            })
            .await;
        if send_result.is_err() {
            // Task already gone; nothing to wait for.
            return;
        }

        // Wait for the task to drain, bounded by SHUTDOWN_DEADLINE.
        let task_handle = handle.task.lock().expect("task mutex poisoned").take();
        match tokio::time::timeout(SHUTDOWN_DEADLINE, ack_rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                // Sender dropped without ack — task panicked or exited.
                warn!(
                    %handle.session_id,
                    "recording writer shutdown ack channel closed"
                );
            }
            Err(_) => {
                warn!(
                    %handle.session_id,
                    "recording writer shutdown timed out; aborting"
                );
                if let Some(h) = &task_handle {
                    h.abort();
                }
            }
        }
        if let Some(h) = task_handle {
            // Best-effort wait so a clean shutdown leaves no zombie
            // task behind. Bound here too.
            let _ = tokio::time::timeout(Duration::from_millis(200), h).await;
        }
    }
}

impl EnabledHandle {
    fn config_hard_cap(&self) -> usize {
        self.hard_cap_bytes as usize
    }
}

fn take_gap(slot: &Mutex<Option<PendingGap>>) -> Option<PendingGap> {
    slot.lock().expect("pending_gap mutex poisoned").take()
}

/// Restore a previously-taken gap that did not make it into the writer
/// queue (the `try_send` saw `Full` and returned the command back).
///
/// Race note: a second producer that called `take_gap` between this
/// caller's `take_gap` and `restore_gap` observed `None` and enqueued
/// its frame with `gap_before = None`. That is correct: the writer
/// task processes commands FIFO, so when the restored gap is
/// eventually attached to a future successful enqueue it still lands
/// before any chunk produced after the gap. The only observable
/// effect is "the gap marker is delayed by at most one concurrent
/// enqueue" — the chunk-continuity invariant is preserved.
fn restore_gap(slot: &Mutex<Option<PendingGap>>, gap: PendingGap) {
    let mut guard = slot.lock().expect("pending_gap mutex poisoned");
    *guard = Some(match *guard {
        Some(existing) => merge_gaps(existing, gap),
        None => gap,
    });
}

fn extend_gap(slot: &Mutex<Option<PendingGap>>, seq: u64, reason: &'static str) {
    let mut guard = slot.lock().expect("pending_gap mutex poisoned");
    *guard = Some(match *guard {
        Some(existing) => PendingGap {
            from_seq: existing.from_seq.min(seq),
            to_seq: existing.to_seq.max(seq),
            // Earliest reason wins so a subsequent overflow does not
            // mask a frame_oversized first cause.
            reason: existing.reason,
        },
        None => PendingGap {
            from_seq: seq,
            to_seq: seq,
            reason,
        },
    });
}

fn merge_gaps(a: PendingGap, b: PendingGap) -> PendingGap {
    PendingGap {
        from_seq: a.from_seq.min(b.from_seq),
        to_seq: a.to_seq.max(b.to_seq),
        reason: a.reason,
    }
}

/// Open chunk batch — accumulates payload bytes from consecutive output
/// frames until either `chunk_target_bytes` is reached, a gap forces a
/// flush, or the writer shuts down.
struct ChunkBatch {
    seq_start: u64,
    seq_end: u64,
    payload: Vec<u8>,
}

impl ChunkBatch {
    fn open(seq: u64, data: Vec<u8>) -> Self {
        Self {
            seq_start: seq,
            seq_end: seq,
            payload: data,
        }
    }

    fn append(&mut self, seq: u64, data: &[u8]) {
        self.seq_end = seq;
        self.payload.extend_from_slice(data);
    }

    fn would_overshoot_hard_cap(&self, frame_len: usize, hard_cap: usize) -> bool {
        self.payload.len().saturating_add(frame_len) > hard_cap
    }
}

async fn writer_task(
    mut rx: mpsc::Receiver<WriterCommand>,
    repository: Arc<dyn TerminalRecordingRepository>,
    session_id: TerminalSessionId,
    config: RecordingWriterConfig,
) {
    let target = config.chunk_target_bytes as usize;
    let hard_cap = config.chunk_hard_cap_bytes as usize;

    let mut open: Option<ChunkBatch> = None;
    let mut highest_seq_observed: u64 = 0;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            WriterCommand::Output {
                seq,
                data,
                gap_before,
            } => {
                if let Some(gap) = gap_before {
                    flush_chunk(&repository, session_id, open.take()).await;
                    write_replay_gap(&repository, session_id, gap).await;
                }

                if data.len() > hard_cap {
                    // Should never happen — the producer-side check
                    // already filtered this. Defence in depth.
                    flush_chunk(&repository, session_id, open.take()).await;
                    write_replay_gap(
                        &repository,
                        session_id,
                        PendingGap {
                            from_seq: seq,
                            to_seq: seq,
                            reason: replay_gap_reason::FRAME_OVERSIZED,
                        },
                    )
                    .await;
                    highest_seq_observed = highest_seq_observed.max(seq);
                    continue;
                }

                match open.as_mut() {
                    Some(batch) => {
                        // If appending this frame would push the chunk
                        // past the hard cap, flush the current batch
                        // first and start a new one.
                        if batch.would_overshoot_hard_cap(data.len(), hard_cap) {
                            flush_chunk(&repository, session_id, open.take()).await;
                            open = Some(ChunkBatch::open(seq, data));
                        } else {
                            batch.append(seq, &data);
                        }
                    }
                    None => {
                        open = Some(ChunkBatch::open(seq, data));
                    }
                }

                if let Some(batch) = open.as_ref() {
                    if batch.payload.len() >= target {
                        flush_chunk(&repository, session_id, open.take()).await;
                    }
                }

                highest_seq_observed = highest_seq_observed.max(seq);
            }
            WriterCommand::Marker { kind, seq, payload } => {
                // Markers should land in seq order with the chunk stream
                // for any reader paging by seq. Flush the current chunk
                // first (if it has accumulated bytes) so a `resized`
                // marker at seq=N ends up between chunk[..N] and
                // chunk[N+1..].
                flush_chunk(&repository, session_id, open.take()).await;
                write_marker(&repository, session_id, kind, seq, payload).await;
                if seq >= 0 {
                    highest_seq_observed = highest_seq_observed.max(seq as u64);
                }
            }
            WriterCommand::Shutdown { last_seq, ack } => {
                // Drain any remaining commands so a producer that
                // pushed before us shutting down still lands its data.
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        WriterCommand::Output {
                            seq,
                            data,
                            gap_before,
                        } => {
                            if let Some(gap) = gap_before {
                                flush_chunk(&repository, session_id, open.take()).await;
                                write_replay_gap(&repository, session_id, gap).await;
                            }
                            if data.len() > hard_cap {
                                flush_chunk(&repository, session_id, open.take()).await;
                                write_replay_gap(
                                    &repository,
                                    session_id,
                                    PendingGap {
                                        from_seq: seq,
                                        to_seq: seq,
                                        reason: replay_gap_reason::FRAME_OVERSIZED,
                                    },
                                )
                                .await;
                                highest_seq_observed = highest_seq_observed.max(seq);
                                continue;
                            }
                            match open.as_mut() {
                                Some(batch) => {
                                    if batch.would_overshoot_hard_cap(data.len(), hard_cap) {
                                        flush_chunk(&repository, session_id, open.take()).await;
                                        open = Some(ChunkBatch::open(seq, data));
                                    } else {
                                        batch.append(seq, &data);
                                    }
                                }
                                None => {
                                    open = Some(ChunkBatch::open(seq, data));
                                }
                            }
                            // Mirror the main event loop's target-flush
                            // so a burst of frames queued just before
                            // shutdown produces target-sized chunks
                            // instead of one trailing oversized chunk.
                            if let Some(batch) = open.as_ref() {
                                if batch.payload.len() >= target {
                                    flush_chunk(&repository, session_id, open.take()).await;
                                }
                            }
                            highest_seq_observed = highest_seq_observed.max(seq);
                        }
                        WriterCommand::Marker { kind, seq, payload } => {
                            flush_chunk(&repository, session_id, open.take()).await;
                            write_marker(&repository, session_id, kind, seq, payload).await;
                        }
                        WriterCommand::Shutdown { ack: ack2, .. } => {
                            let _ = ack2.send(());
                        }
                    }
                }
                flush_chunk(&repository, session_id, open.take()).await;
                let close_seq = if last_seq >= 1 {
                    last_seq
                } else {
                    // No output landed; the closed marker still needs
                    // a non-zero seq per schema. Use 1 as the floor so
                    // the row inserts cleanly.
                    1
                };
                write_marker(
                    &repository,
                    session_id,
                    TerminalRecordingMarkerKind::Closed,
                    close_seq,
                    json!({ "reason": "session_close" }),
                )
                .await;
                let _ = ack.send(());
                let _ = highest_seq_observed; // silence unused warning when shutdown lands without writes
                return;
            }
        }
    }

    // Producer dropped every sender without an explicit shutdown.
    // Flush the trailing chunk and write a best-effort `closed`
    // marker so the recording is bounded. Use the highest observed
    // seq as the close marker's seq.
    flush_chunk(&repository, session_id, open.take()).await;
    let close_seq = highest_seq_observed.max(1);
    write_marker(
        &repository,
        session_id,
        TerminalRecordingMarkerKind::Closed,
        i64::try_from(close_seq).unwrap_or(i64::MAX),
        json!({ "reason": "writer_dropped" }),
    )
    .await;
}

async fn flush_chunk(
    repository: &Arc<dyn TerminalRecordingRepository>,
    session_id: TerminalSessionId,
    batch: Option<ChunkBatch>,
) {
    let Some(batch) = batch else {
        return;
    };
    if batch.payload.is_empty() {
        return;
    }
    let byte_len = match i32::try_from(batch.payload.len()) {
        Ok(v) => v,
        Err(_) => {
            warn!(
                %session_id,
                error = error_category::APPEND_CHUNK,
                "chunk payload exceeds i32 range; dropping"
            );
            return;
        }
    };
    let seq_start = match i64::try_from(batch.seq_start) {
        Ok(v) => v,
        Err(_) => {
            warn!(%session_id, error = error_category::APPEND_CHUNK, "seq_start out of range");
            return;
        }
    };
    let seq_end = match i64::try_from(batch.seq_end) {
        Ok(v) => v,
        Err(_) => {
            warn!(%session_id, error = error_category::APPEND_CHUNK, "seq_end out of range");
            return;
        }
    };
    let input = CreateTerminalRecordingChunk {
        terminal_session_id: session_id,
        seq_start,
        seq_end,
        byte_len,
        payload: batch.payload,
        encryption: TerminalRecordingPayloadEncryption::None,
        compression: TerminalRecordingCompression::None,
    };
    if let Err(_err) = repository.append_chunk(input).await {
        // Map to a `writer_error` replay_gap. The repository's
        // RepositoryError::Display is metadata-only by contract,
        // but we deliberately do NOT format `?err` — the sentinel
        // strategy treats the writer's log surface as
        // "category-only" so a future RepositoryError variant
        // can't accidentally leak text.
        warn!(
            %session_id,
            error = error_category::APPEND_CHUNK,
            "recording chunk write failed; emitting replay_gap"
        );
        // Use a fresh seq range covering the dropped batch.
        write_replay_gap(
            repository,
            session_id,
            PendingGap {
                from_seq: batch.seq_start,
                to_seq: batch.seq_end,
                reason: replay_gap_reason::WRITER_ERROR,
            },
        )
        .await;
    }
}

async fn write_marker(
    repository: &Arc<dyn TerminalRecordingRepository>,
    session_id: TerminalSessionId,
    kind: TerminalRecordingMarkerKind,
    seq: i64,
    payload: JsonValue,
) {
    let input = CreateTerminalRecordingMarker {
        terminal_session_id: session_id,
        kind,
        seq,
        payload,
    };
    if repository.append_marker(input).await.is_err() {
        warn!(
            %session_id,
            kind = kind.as_str(),
            error = error_category::APPEND_MARKER,
            "recording marker write failed"
        );
    }
}

async fn write_replay_gap(
    repository: &Arc<dyn TerminalRecordingRepository>,
    session_id: TerminalSessionId,
    gap: PendingGap,
) {
    let from_seq_i64 = match i64::try_from(gap.from_seq) {
        Ok(v) => v,
        Err(_) => return,
    };
    let to_seq_i64 = match i64::try_from(gap.to_seq) {
        Ok(v) => v,
        Err(_) => return,
    };
    let payload = json!({
        "from_seq": from_seq_i64,
        "to_seq": to_seq_i64,
        "reason": gap.reason,
    });
    write_marker(
        repository,
        session_id,
        TerminalRecordingMarkerKind::ReplayGap,
        // Per schema, ReplayGap requires `seq >= 1`. The marker's
        // `seq` is the upper bound of the gap so a reader paging by
        // ascending `seq >= n + 1` finds the marker when their
        // bookmark is inside the gap.
        to_seq_i64.max(1),
        payload,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use relayterm_core::ids::{
        TerminalRecordingChunkId, TerminalRecordingMarkerId, TerminalSessionId,
    };
    use relayterm_core::repository::{
        CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, PurgeRecordingForRetention,
        RepositoryError, TerminalRecordingRepository,
    };
    use relayterm_core::terminal_recording::{TerminalRecordingChunk, TerminalRecordingMarker};
    use std::sync::Mutex as StdMutex;

    /// In-memory recording repository fake. Captures every chunk and
    /// marker so tests can assert ordering, contents, and absence.
    #[derive(Default)]
    struct FakeRecordingRepo {
        chunks: StdMutex<Vec<TerminalRecordingChunk>>,
        markers: StdMutex<Vec<TerminalRecordingMarker>>,
        /// If set, append_chunk fails until cleared.
        fail_chunk: std::sync::atomic::AtomicBool,
    }

    impl FakeRecordingRepo {
        fn snapshot_chunks(&self) -> Vec<TerminalRecordingChunk> {
            self.chunks.lock().unwrap().clone()
        }
        fn snapshot_markers(&self) -> Vec<TerminalRecordingMarker> {
            self.markers.lock().unwrap().clone()
        }
        fn set_fail_chunk(&self, fail: bool) {
            self.fail_chunk
                .store(fail, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl TerminalRecordingRepository for FakeRecordingRepo {
        async fn append_chunk(
            &self,
            input: CreateTerminalRecordingChunk,
        ) -> Result<TerminalRecordingChunk, RepositoryError> {
            if self.fail_chunk.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(RepositoryError::Database(
                    "append_chunk_failure".to_string(),
                ));
            }
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

        async fn purge_for_retention(
            &self,
            _input: PurgeRecordingForRetention,
        ) -> Result<Option<relayterm_core::PurgedRecordingSummary>, RepositoryError> {
            // The recording-writer test fakes never exercise the
            // retention purge path; the worker that drives this method
            // is its own slice. Returning `None` keeps the trait
            // implementable without leaking any behaviour into the
            // writer fake.
            Ok(None)
        }
    }

    fn build(
        repo: Arc<FakeRecordingRepo>,
        cfg: RecordingWriterConfig,
    ) -> (RecordingWriter, TerminalSessionId) {
        let session_id = TerminalSessionId::new();
        let w = RecordingWriter::enabled(repo, session_id, cfg);
        (w, session_id)
    }

    const SENTINEL_BYTES: &[u8] = b"PTY-SENTINEL-7E2A";

    #[tokio::test]
    async fn disabled_writer_drops_calls_silently() {
        let w = RecordingWriter::disabled();
        assert!(!w.is_enabled());
        w.record_output(1, SENTINEL_BYTES).await;
        w.record_marker(
            TerminalRecordingMarkerKind::Resized,
            5,
            json!({ "cols": 80, "rows": 24 }),
        )
        .await;
        w.shutdown(5).await;
        // No state to inspect — pass if we got here without panicking.
    }

    #[tokio::test]
    async fn enabled_writer_writes_started_marker_eventually() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 64,
                chunk_hard_cap_bytes: 64 * 1024,
            },
        );
        w.shutdown(0).await;
        let markers = repo.snapshot_markers();
        let kinds: Vec<_> = markers.iter().map(|m| m.kind).collect();
        assert!(
            kinds.contains(&TerminalRecordingMarkerKind::Started),
            "started marker must land before shutdown completes; got {kinds:?}"
        );
        assert!(
            kinds.contains(&TerminalRecordingMarkerKind::Closed),
            "closed marker must land at shutdown; got {kinds:?}"
        );
    }

    #[tokio::test]
    async fn small_outputs_batch_into_one_chunk() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, sid) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 1024,
                chunk_hard_cap_bytes: 64 * 1024,
            },
        );
        w.record_output(1, b"hello ").await;
        w.record_output(2, b"world").await;
        w.shutdown(2).await;
        let chunks = repo.snapshot_chunks();
        assert_eq!(
            chunks.len(),
            1,
            "expected one batched chunk; got {chunks:?}"
        );
        let c = &chunks[0];
        assert_eq!(c.terminal_session_id, sid);
        assert_eq!(c.seq_start, 1);
        assert_eq!(c.seq_end, 2);
        assert_eq!(c.payload, b"hello world".to_vec());
        assert_eq!(c.byte_len, b"hello world".len() as i32);
        assert_eq!(c.encryption, TerminalRecordingPayloadEncryption::None);
        assert_eq!(c.compression, TerminalRecordingCompression::None);
    }

    #[tokio::test]
    async fn target_bytes_flushes_chunk() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 8,
                chunk_hard_cap_bytes: 64 * 1024,
            },
        );
        // Each frame is 4 bytes; second frame triggers flush at >= 8 bytes.
        w.record_output(1, b"abcd").await;
        w.record_output(2, b"efgh").await;
        w.record_output(3, b"ij").await;
        w.shutdown(3).await;
        let chunks = repo.snapshot_chunks();
        assert!(chunks.len() >= 2, "expected >= 2 chunks; got {chunks:?}");
        assert_eq!(chunks[0].seq_start, 1);
        assert_eq!(chunks[0].seq_end, 2);
        assert_eq!(chunks[0].payload, b"abcdefgh".to_vec());
        // Last chunk holds the trailing two bytes.
        let last = chunks.last().unwrap();
        assert_eq!(last.seq_start, 3);
        assert_eq!(last.seq_end, 3);
        assert_eq!(last.payload, b"ij".to_vec());
    }

    #[tokio::test]
    async fn flush_on_close_writes_remaining_chunk() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 1 << 20,
                chunk_hard_cap_bytes: 1 << 20,
            },
        );
        w.record_output(1, b"trailing-bytes").await;
        w.shutdown(1).await;
        let chunks = repo.snapshot_chunks();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].payload, b"trailing-bytes".to_vec());
    }

    #[tokio::test]
    async fn payload_order_preserved() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 8,
                chunk_hard_cap_bytes: 64 * 1024,
            },
        );
        for seq in 1..=10u64 {
            // Encode the seq into the payload so a reorder is visible.
            let mut data = Vec::new();
            data.extend_from_slice(&seq.to_be_bytes());
            w.record_output(seq, &data).await;
        }
        w.shutdown(10).await;
        let chunks = repo.snapshot_chunks();
        // Concatenate all chunk payloads and decode as 8-byte BE seqs.
        let mut concat = Vec::new();
        let mut last_end = 0;
        for c in &chunks {
            assert!(c.seq_start >= last_end, "chunks must be seq-ordered");
            assert!(c.seq_end >= c.seq_start);
            last_end = c.seq_end;
            concat.extend_from_slice(&c.payload);
        }
        let mut idx = 0;
        let mut seqs = Vec::new();
        while idx + 8 <= concat.len() {
            let arr: [u8; 8] = concat[idx..idx + 8].try_into().unwrap();
            seqs.push(u64::from_be_bytes(arr));
            idx += 8;
        }
        assert_eq!(seqs, (1..=10u64).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn hard_cap_triggers_flush_before_overshooting() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 1 << 20,
                chunk_hard_cap_bytes: 16,
            },
        );
        // Two 12-byte frames: second one would overshoot 16 → must
        // start a new chunk.
        w.record_output(1, &[b'a'; 12]).await;
        w.record_output(2, &[b'b'; 12]).await;
        w.shutdown(2).await;
        let chunks = repo.snapshot_chunks();
        assert_eq!(chunks.len(), 2, "got {chunks:?}");
        assert!(chunks[0].payload.len() <= 16);
        assert!(chunks[1].payload.len() <= 16);
        assert_eq!(chunks[0].seq_start, 1);
        assert_eq!(chunks[0].seq_end, 1);
        assert_eq!(chunks[1].seq_start, 2);
        assert_eq!(chunks[1].seq_end, 2);
    }

    #[tokio::test]
    async fn oversized_single_frame_is_dropped_with_replay_gap() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 1024,
                chunk_hard_cap_bytes: 16,
            },
        );
        // 32 bytes > 16-byte cap → drop with frame_oversized gap marker.
        w.record_output(1, &[b'x'; 32]).await;
        w.record_output(2, b"ok").await;
        w.shutdown(2).await;

        let chunks = repo.snapshot_chunks();
        // Only seq=2 survives.
        let payloads: Vec<&Vec<u8>> = chunks.iter().map(|c| &c.payload).collect();
        assert!(payloads.iter().any(|p| p.as_slice() == b"ok"));
        assert!(
            !payloads.iter().any(|p| p.iter().all(|&b| b == b'x')),
            "oversized payload must not be persisted"
        );
        let markers = repo.snapshot_markers();
        let gap = markers
            .iter()
            .find(|m| m.kind == TerminalRecordingMarkerKind::ReplayGap)
            .expect("replay_gap marker must be written");
        assert_eq!(
            gap.payload["reason"],
            replay_gap_reason::FRAME_OVERSIZED,
            "wrong replay_gap reason: {:?}",
            gap.payload
        );
        assert_eq!(gap.payload["from_seq"], 1);
        assert_eq!(gap.payload["to_seq"], 1);
    }

    #[tokio::test]
    async fn append_chunk_failure_emits_writer_error_replay_gap() {
        let repo = Arc::new(FakeRecordingRepo::default());
        repo.set_fail_chunk(true);
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 4,
                chunk_hard_cap_bytes: 64 * 1024,
            },
        );
        w.record_output(1, b"abcd").await; // triggers flush
        // give task time
        tokio::time::sleep(Duration::from_millis(50)).await;
        repo.set_fail_chunk(false);
        w.shutdown(1).await;
        let markers = repo.snapshot_markers();
        let gap = markers
            .iter()
            .find(|m| {
                m.kind == TerminalRecordingMarkerKind::ReplayGap
                    && m.payload["reason"] == replay_gap_reason::WRITER_ERROR
            })
            .expect("writer_error replay_gap marker must be written");
        assert_eq!(gap.payload["from_seq"], 1);
        assert_eq!(gap.payload["to_seq"], 1);
    }

    #[tokio::test]
    async fn marker_payload_excludes_terminal_byte_sentinel() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(
            repo.clone(),
            RecordingWriterConfig {
                chunk_target_bytes: 4,
                chunk_hard_cap_bytes: 64,
            },
        );
        w.record_output(1, SENTINEL_BYTES).await;
        w.record_marker(
            TerminalRecordingMarkerKind::Resized,
            1,
            json!({ "cols": 80, "rows": 24 }),
        )
        .await;
        w.shutdown(1).await;

        let markers = repo.snapshot_markers();
        for m in markers {
            let body = m.payload.to_string();
            assert!(
                !body.contains("PTY-SENTINEL"),
                "marker payload leaked output sentinel: {body}",
            );
            let dbg = format!("{m:?}");
            assert!(
                !dbg.contains("PTY-SENTINEL"),
                "marker Debug leaked output sentinel: {dbg}",
            );
        }
    }

    #[tokio::test]
    async fn writer_debug_does_not_leak_payload() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(repo, RecordingWriterConfig::DEFAULT);
        w.record_output(1, SENTINEL_BYTES).await;
        let dbg = format!("{w:?}");
        assert!(!dbg.contains("PTY-SENTINEL"), "{dbg}");
        w.shutdown(1).await;
    }

    #[tokio::test]
    async fn marker_seq_zero_only_for_started() {
        let repo = Arc::new(FakeRecordingRepo::default());
        let (w, _) = build(repo.clone(), RecordingWriterConfig::DEFAULT);
        // Record an illegal seq=0 Resized marker; it must be dropped
        // before reaching the repository.
        w.record_marker(
            TerminalRecordingMarkerKind::Resized,
            0,
            json!({ "cols": 80, "rows": 24 }),
        )
        .await;
        w.shutdown(1).await;
        let markers = repo.snapshot_markers();
        for m in markers {
            // The only seq=0 marker that may exist is `started`.
            if m.seq == 0 {
                assert_eq!(m.kind, TerminalRecordingMarkerKind::Started);
            }
        }
    }
}
