//! In-memory bounded replay buffer for terminal output.
//!
//! ## Contract
//!
//! The replay buffer is a **non-durable** convenience the orchestrator uses
//! to make short reconnects whole. It is sized in both frames and bytes so
//! a single oversized output frame can't blow process memory and so a long
//! quiet stream of tiny frames eventually evicts itself. Both bounds are
//! evaluated after every push: the buffer pops from the front until both
//! are satisfied.
//!
//! Sequence numbers are assigned by the caller (the manager's PTY
//! forwarder) and start at `1` per session. The buffer never re-numbers a
//! frame; replayed frames carry the same `seq` they were stamped with on
//! the live wire.
//!
//! ## Privacy invariants
//!
//! - The buffer stores raw PTY bytes in memory only. It is dropped with
//!   the live runtime entry on close.
//! - `Debug` redacts payload bytes — only `seq` + `len` are formatted, so
//!   an accidental `?frame` in a tracing macro can never leak terminal
//!   output.
//! - The buffer must never be mirrored to Postgres or any disk surface.

use std::collections::VecDeque;
use std::sync::Arc;

/// One stored frame: the monotonic per-session sequence number plus the
/// raw PTY bytes. Cloning is cheap — `data` is an `Arc<[u8]>`.
#[derive(Clone)]
pub struct OutputFrame {
    pub seq: u64,
    pub data: Arc<[u8]>,
}

impl std::fmt::Debug for OutputFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputFrame")
            .field("seq", &self.seq)
            .field("len", &self.data.len())
            .field("data", &"<redacted pty output>")
            .finish()
    }
}

/// Result of a successful replay query: the (possibly empty) range of
/// frames newer than the caller's `last_seen_seq`, plus the latest seq
/// the orchestrator has stamped at query time. The caller wires the
/// frames straight into the WebSocket fanout; `latest_seq` is what goes
/// in the trailing `ReplayEnd { latest_seq }` frame.
#[derive(Clone)]
pub struct ReplayRange {
    pub frames: Vec<OutputFrame>,
    pub latest_seq: u64,
}

impl std::fmt::Debug for ReplayRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayRange")
            .field("frame_count", &self.frames.len())
            .field("latest_seq", &self.latest_seq)
            .finish()
    }
}

/// The caller asked to resume from a `seq` older than the buffer still
/// retains. The orchestrator surfaces this as `ReplayWindowLost` on the
/// wire; the renderer is expected to reset its grid before live frames
/// resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "replay window lost (requested {requested}, oldest available {oldest_available:?}, latest {latest})"
)]
pub struct ReplayWindowLost {
    pub requested: u64,
    pub oldest_available: Option<u64>,
    pub latest: u64,
}

/// Configuration for the replay buffer. Both bounds are inclusive: the
/// buffer evicts as long as either limit is exceeded after a push, so the
/// post-push state always satisfies `frames.len() <= max_frames` AND
/// `bytes <= max_bytes`. Frame size larger than `max_bytes` is allowed —
/// the buffer keeps the single most recent frame even if it overshoots,
/// because dropping it would leave nothing to replay.
#[derive(Clone, Copy, Debug)]
pub struct ReplayBufferConfig {
    pub max_frames: usize,
    pub max_bytes: usize,
}

impl ReplayBufferConfig {
    /// Default sizing tuned for "interactive shell, short hiccups." 1024
    /// frames + 1 MiB is roughly 10–30s of busy output for a typical
    /// shell session and an order of magnitude more for an idle one,
    /// but bounded enough that a forgotten detached PTY can't bleed
    /// the host.
    pub const DEFAULT: Self = Self {
        max_frames: 1024,
        max_bytes: 1 << 20,
    };
}

impl Default for ReplayBufferConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Bounded ring of recent output frames.
///
/// Not `Clone` on purpose: the buffer owns its frames and is held inside
/// a `Mutex` on the live runtime entry. Concurrent readers grab the
/// lock briefly to snapshot a `ReplayRange`.
pub struct ReplayBuffer {
    config: ReplayBufferConfig,
    frames: VecDeque<OutputFrame>,
    bytes: usize,
    /// Highest seq the buffer has ever observed. Survives eviction so a
    /// `ReplayRange::latest_seq` reflects the live stream, not the
    /// buffer's window. Starts at 0 (no frames pushed yet).
    latest_seq: u64,
}

impl std::fmt::Debug for ReplayBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayBuffer")
            .field("frame_count", &self.frames.len())
            .field("bytes", &self.bytes)
            .field("latest_seq", &self.latest_seq)
            .field("config", &self.config)
            .finish()
    }
}

impl ReplayBuffer {
    #[must_use]
    pub fn new(config: ReplayBufferConfig) -> Self {
        Self {
            config,
            frames: VecDeque::new(),
            bytes: 0,
            latest_seq: 0,
        }
    }

    /// Append a frame and evict from the front until the configured
    /// bounds are satisfied. The single most recent frame is always
    /// retained even if its size exceeds `max_bytes`.
    pub fn push(&mut self, frame: OutputFrame) {
        debug_assert!(
            frame.seq > self.latest_seq,
            "replay buffer push out of order: got seq {} after {}",
            frame.seq,
            self.latest_seq,
        );
        self.bytes = self.bytes.saturating_add(frame.data.len());
        self.latest_seq = frame.seq;
        self.frames.push_back(frame);
        while self.frames.len() > self.config.max_frames || self.bytes > self.config.max_bytes {
            // Always keep at least one frame so the renderer has something
            // to replay even when a single frame overshoots `max_bytes`.
            if self.frames.len() <= 1 {
                break;
            }
            if let Some(dropped) = self.frames.pop_front() {
                self.bytes = self.bytes.saturating_sub(dropped.data.len());
            }
        }
    }

    /// Frames newer than `last_seen_seq`, in order.
    ///
    /// `last_seen_seq == None` (and `Some(0)`, since seq numbers start
    /// at 1) means "no resume bookmark" — the buffer returns whatever
    /// it currently retains, and `ReplayWindowLost` is NEVER produced
    /// for these callers because they have no expectation about the
    /// older frames the buffer may have evicted.
    ///
    /// `Err(ReplayWindowLost)` fires when a positive bookmark is older
    /// than the oldest frame the buffer still retains. The orchestrator
    /// surfaces this as `ReplayWindowLost` on the wire and then
    /// continues live attach — a lost replay window is a safe-to-resume
    /// condition for the renderer, not a session-fatal error.
    pub fn replay_since(
        &self,
        last_seen_seq: Option<u64>,
    ) -> Result<ReplayRange, ReplayWindowLost> {
        let bookmark = last_seen_seq.unwrap_or(0);
        let oldest_available = self.frames.front().map(|f| f.seq);

        // No frames buffered yet — there is nothing to replay regardless
        // of the bookmark. This is NOT a window-lost condition; it's the
        // "fresh session, no live output yet" case.
        let Some(oldest) = oldest_available else {
            return Ok(ReplayRange {
                frames: Vec::new(),
                latest_seq: self.latest_seq,
            });
        };

        // Caller is current or ahead — nothing to replay.
        if bookmark >= self.latest_seq {
            return Ok(ReplayRange {
                frames: Vec::new(),
                latest_seq: self.latest_seq,
            });
        }

        // Window-lost only applies to callers who actually held a
        // bookmark (`bookmark > 0`). A no-bookmark caller (`None` /
        // `Some(0)`) never triggers a window-lost — they simply get
        // whatever the buffer currently retains.
        if bookmark > 0 && bookmark + 1 < oldest {
            return Err(ReplayWindowLost {
                requested: bookmark,
                oldest_available,
                latest: self.latest_seq,
            });
        }

        let frames: Vec<OutputFrame> = self
            .frames
            .iter()
            .filter(|f| f.seq > bookmark)
            .cloned()
            .collect();
        Ok(ReplayRange {
            frames,
            latest_seq: self.latest_seq,
        })
    }

    /// Highest seq stamped so far. `0` if no frames have been pushed.
    #[must_use]
    pub fn latest_seq(&self) -> u64 {
        self.latest_seq
    }

    /// Number of frames currently retained. Test-only convenience.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Bytes currently retained across all stored frames. Test-only
    /// convenience.
    #[must_use]
    pub fn byte_count(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, len: usize) -> OutputFrame {
        OutputFrame {
            seq,
            data: Arc::from(vec![b'x'; len].into_boxed_slice()),
        }
    }

    #[test]
    fn fresh_buffer_replay_is_empty_with_zero_latest() {
        let buf = ReplayBuffer::new(ReplayBufferConfig::DEFAULT);
        let range = buf.replay_since(None).unwrap();
        assert!(range.frames.is_empty());
        assert_eq!(range.latest_seq, 0);
    }

    #[test]
    fn push_increments_latest_and_keeps_frame() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig::DEFAULT);
        buf.push(frame(1, 10));
        assert_eq!(buf.latest_seq(), 1);
        assert_eq!(buf.frame_count(), 1);
        let range = buf.replay_since(None).unwrap();
        assert_eq!(range.frames.len(), 1);
        assert_eq!(range.frames[0].seq, 1);
        assert_eq!(range.latest_seq, 1);
    }

    #[test]
    fn replay_since_skips_already_seen() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig::DEFAULT);
        for seq in 1..=5 {
            buf.push(frame(seq, 4));
        }
        let range = buf.replay_since(Some(2)).unwrap();
        let seqs: Vec<u64> = range.frames.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        assert_eq!(range.latest_seq, 5);
    }

    #[test]
    fn replay_since_caller_at_or_ahead_of_latest_is_empty() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig::DEFAULT);
        for seq in 1..=3 {
            buf.push(frame(seq, 4));
        }
        for bookmark in [3, 4, 999] {
            let range = buf.replay_since(Some(bookmark)).unwrap();
            assert!(
                range.frames.is_empty(),
                "bookmark {bookmark} should yield no frames",
            );
            assert_eq!(range.latest_seq, 3);
        }
    }

    #[test]
    fn evicts_when_max_frames_exceeded() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig {
            max_frames: 3,
            max_bytes: usize::MAX,
        });
        for seq in 1..=5 {
            buf.push(frame(seq, 1));
        }
        assert_eq!(buf.frame_count(), 3);
        let range = buf.replay_since(None).unwrap();
        let seqs: Vec<u64> = range.frames.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        assert_eq!(range.latest_seq, 5);
    }

    #[test]
    fn evicts_when_max_bytes_exceeded() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig {
            max_frames: 100,
            max_bytes: 10,
        });
        for seq in 1..=5 {
            buf.push(frame(seq, 4));
        }
        assert!(buf.byte_count() <= 10);
        let range = buf.replay_since(None).unwrap();
        // 5 frames * 4 bytes = 20; capped to 10 → 2 frames retained.
        // Eviction is deterministic from the back-most retained.
        assert_eq!(range.frames.len(), 2);
        let seqs: Vec<u64> = range.frames.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![4, 5]);
        assert_eq!(range.latest_seq, 5);
    }

    #[test]
    fn keeps_at_least_one_oversized_frame() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig {
            max_frames: 100,
            max_bytes: 10,
        });
        buf.push(frame(1, 32));
        assert_eq!(buf.frame_count(), 1);
        let range = buf.replay_since(None).unwrap();
        assert_eq!(range.frames.len(), 1);
        assert_eq!(range.frames[0].seq, 1);
    }

    #[test]
    fn replay_window_lost_when_bookmark_predates_buffer() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig {
            max_frames: 2,
            max_bytes: usize::MAX,
        });
        for seq in 1..=4 {
            buf.push(frame(seq, 1));
        }
        // Buffer now holds [3, 4]; bookmark 1 is older than oldest=3-1=2.
        let err = buf.replay_since(Some(1)).unwrap_err();
        assert_eq!(err.requested, 1);
        assert_eq!(err.oldest_available, Some(3));
        assert_eq!(err.latest, 4);
    }

    #[test]
    fn replay_window_lost_boundary_keeps_adjacent_bookmark() {
        // Buffer holds [3, 4]; bookmark = 2 means the very next frame
        // they need IS the oldest we have, so this is NOT a window-lost
        // condition.
        let mut buf = ReplayBuffer::new(ReplayBufferConfig {
            max_frames: 2,
            max_bytes: usize::MAX,
        });
        for seq in 1..=4 {
            buf.push(frame(seq, 1));
        }
        let range = buf.replay_since(Some(2)).unwrap();
        let seqs: Vec<u64> = range.frames.iter().map(|f| f.seq).collect();
        assert_eq!(seqs, vec![3, 4]);
    }

    #[test]
    fn debug_redacts_frame_bytes() {
        // Use 0xFF bytes so the negative assertion can't be vacuously
        // true if a future change accidentally derives Debug — derive
        // would render the Vec as `[255, 255, ...]` which contains
        // ASCII "255" we can detect.
        let data = vec![0xFFu8; 16];
        let f = OutputFrame {
            seq: 7,
            data: Arc::from(data.into_boxed_slice()),
        };
        let dbg = format!("{f:?}");
        assert!(dbg.contains("seq: 7"));
        assert!(dbg.contains("len: 16"));
        assert!(dbg.contains("<redacted pty output>"));
        assert!(
            !dbg.contains("255"),
            "Debug must not render the underlying byte values: {dbg}",
        );
    }

    #[test]
    fn debug_replay_range_does_not_format_frames() {
        let mut buf = ReplayBuffer::new(ReplayBufferConfig::DEFAULT);
        for seq in 1..=3 {
            buf.push(frame(seq, 8));
        }
        let range = buf.replay_since(None).unwrap();
        let dbg = format!("{range:?}");
        assert!(dbg.contains("frame_count: 3"));
        assert!(dbg.contains("latest_seq: 3"));
        assert!(!dbg.contains("xxx"));
    }
}
