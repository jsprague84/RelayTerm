//! In-memory login throttler.
//!
//! Tracks failed login attempts keyed on a normalized identifier
//! (e.g. the lower-cased + trimmed email). After [`LoginThrottleConfig::max_failures`]
//! failures within [`LoginThrottleConfig::window`], every further
//! attempt against the same key is rejected for [`LoginThrottleConfig::block`].
//! A successful login clears the key.
//!
//! ## Probe-resistance
//!
//! Keys are constructed from the normalized identifier ONLY — unknown-email
//! and wrong-password failures BOTH increment the same key. A probe
//! cannot distinguish "user exists, wrong password" from "user does
//! not exist" through the throttle channel.
//!
//! ## Local-process only
//!
//! State lives in a single in-memory map behind a `std::sync::Mutex`.
//! A multi-instance deployment SHOULD layer reverse-proxy rate-limiting
//! on top per `docs/production-auth.md` until a distributed limiter
//! lands. Restarting the backend resets the state — that is intentional
//! for v1 and documented in SPEC.md "Password authentication (v1)" →
//! "Throttling".
//!
//! ## Memory bound
//!
//! The map is capped at [`MAX_TRACKED_KEYS`]. Opportunistic cleanup
//! runs on every [`LoginThrottler::record_failure`]; if the map is
//! still at capacity after cleanup the new key is silently dropped —
//! the throttle disengages for fresh attackers under saturation rather
//! than refusing service. A single entry costs ~64 bytes; 10,000
//! entries ≈ 640 KiB.
//!
//! ## Redaction
//!
//! Keys MUST be normalized identifiers (no password material). The
//! throttler `Debug` impl renders only the entry count, never the keys
//! themselves.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

/// Soft cap on the number of distinct keys tracked at once.
///
/// At 10k entries the map is ~640 KiB. Beyond this the throttler
/// silently disengages for new keys (`record_failure` becomes a no-op
/// for keys not already tracked) until cleanup frees room.
pub const MAX_TRACKED_KEYS: usize = 10_000;

/// v1 throttling policy. Defaults: 5 failures per 15-minute window
/// triggers a 15-minute block.
#[derive(Clone, Copy)]
pub struct LoginThrottleConfig {
    /// Failures within `window` that trigger a block. Must be >= 1.
    pub max_failures: u32,
    /// Sliding window over which failures accumulate.
    pub window: Duration,
    /// Block duration once the threshold is crossed.
    pub block: Duration,
}

impl LoginThrottleConfig {
    /// v1 default: 5 failures / 15 min triggers a 15-min block.
    pub const V1_DEFAULT: Self = Self {
        max_failures: 5,
        window: Duration::minutes(15),
        block: Duration::minutes(15),
    };
}

impl Default for LoginThrottleConfig {
    fn default() -> Self {
        Self::V1_DEFAULT
    }
}

impl fmt::Debug for LoginThrottleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginThrottleConfig")
            .field("max_failures", &self.max_failures)
            .field("window_seconds", &self.window.num_seconds())
            .field("block_seconds", &self.block.num_seconds())
            .finish()
    }
}

/// Outcome of a [`LoginThrottler::check`] call.
///
/// `Throttled.retry_after_seconds` is the upper bound on the remaining
/// block duration in seconds, suitable for a `Retry-After` header. The
/// route layer is free to drop the value if it does not want to surface
/// timing detail.
///
/// **v1 status (load-bearing):** `POST /api/v1/auth/login` deliberately
/// destructures with `{ .. }` and emits a `429 too_many_requests` with
/// **no** `Retry-After` header. Surfacing the countdown would leak
/// throttle-key telemetry to a probe (e.g. "the bucket I just hit
/// resets in 14:51 — the legitimate user just attempted 9 seconds
/// ago"). The field is precomputed so a future audit / admin surface
/// can use it without re-deriving the math; it is intentionally not on
/// the public error response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleDecision {
    Allowed,
    Throttled { retry_after_seconds: u64 },
}

/// Per-key throttle state.
#[derive(Debug, Clone)]
struct AttemptState {
    /// Failure timestamps within the current sliding window. Bounded
    /// by `max_failures + 1` so a long-running key cannot accumulate
    /// unbounded entries.
    failures: Vec<DateTime<Utc>>,
    /// When set, the key is blocked until this timestamp.
    blocked_until: Option<DateTime<Utc>>,
}

impl AttemptState {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            blocked_until: None,
        }
    }

    /// Drop failures older than `now - window`.
    fn prune_window(&mut self, now: DateTime<Utc>, window: Duration) {
        let cutoff = now - window;
        self.failures.retain(|t| *t > cutoff);
    }

    /// True iff the key is currently blocked at `now`.
    fn is_blocked(&self, now: DateTime<Utc>) -> bool {
        match self.blocked_until {
            Some(until) => now < until,
            None => false,
        }
    }

    /// True iff the entry can be evicted by a cleanup pass at `now`
    /// (no live block AND no live window failures).
    fn is_idle(&self, now: DateTime<Utc>, window: Duration) -> bool {
        if self.is_blocked(now) {
            return false;
        }
        let cutoff = now - window;
        !self.failures.iter().any(|t| *t > cutoff)
    }
}

/// In-memory login throttler.
///
/// Cheap to clone via `Arc`; the inner `Mutex` is held only for
/// microseconds per call. No `.await` happens under the lock — the
/// throttler does no I/O.
pub struct LoginThrottler {
    config: LoginThrottleConfig,
    inner: Mutex<HashMap<String, AttemptState>>,
}

impl LoginThrottler {
    pub fn new(config: LoginThrottleConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Configured policy. Exposed for diagnostics / tests.
    pub fn config(&self) -> LoginThrottleConfig {
        self.config
    }

    /// Decide whether the next attempt for `key` is allowed.
    ///
    /// Pure read; never mutates the map. Use [`Self::record_failure`]
    /// after a failed attempt and [`Self::record_success`] after a
    /// successful one.
    #[must_use]
    pub fn check(&self, key: &str, now: DateTime<Utc>) -> ThrottleDecision {
        let map = self.inner.lock().expect("login throttler mutex");
        let Some(state) = map.get(key) else {
            return ThrottleDecision::Allowed;
        };
        match state.blocked_until {
            Some(until) if now < until => {
                let retry_after_seconds = (until - now).num_seconds().max(1) as u64;
                ThrottleDecision::Throttled {
                    retry_after_seconds,
                }
            }
            _ => ThrottleDecision::Allowed,
        }
    }

    /// Record a failure against `key` at `now`.
    ///
    /// Idempotent under the cap: if the map is already at
    /// [`MAX_TRACKED_KEYS`] AND `key` is not present, the call is a
    /// silent no-op (see "Memory bound" in the module docs). Triggers
    /// a block once the windowed failure count reaches
    /// `config.max_failures`.
    pub fn record_failure(&self, key: &str, now: DateTime<Utc>) {
        let mut map = self.inner.lock().expect("login throttler mutex");

        // Opportunistic cleanup before a potential insert. This keeps
        // the typical map size bounded by the live block / window
        // population without a separate sweeper task.
        if map.len() >= MAX_TRACKED_KEYS {
            map.retain(|_, state| !state.is_idle(now, self.config.window));
        }

        if !map.contains_key(key) {
            // Fail-open under saturation rather than denying service.
            // The alternative — refusing all writes — gives an attacker
            // a way to disable login by exhausting the map.
            if map.len() >= MAX_TRACKED_KEYS {
                tracing::warn!(
                    "login throttler at capacity ({MAX_TRACKED_KEYS}); dropping new key"
                );
                return;
            }
            map.insert(key.to_owned(), AttemptState::new());
        }

        let state = map.get_mut(key).expect("entry just ensured present above");
        state.prune_window(now, self.config.window);

        // Cap the failures vec so a long-running key cannot grow it
        // unboundedly. `max_failures + 1` is enough to cross the
        // threshold; older entries beyond that are window-pruned anyway.
        if state.failures.len() < (self.config.max_failures as usize).saturating_add(1) {
            state.failures.push(now);
        }

        if state.failures.len() >= self.config.max_failures as usize {
            // Either start a new block or extend the current one to
            // `now + block`. Never shorten a block.
            let new_until = now + self.config.block;
            state.blocked_until = Some(match state.blocked_until {
                Some(prev) if prev > new_until => prev,
                _ => new_until,
            });
        }
    }

    /// Clear throttle state for `key`. Call on a successful login so
    /// a typo'd attempt that made it under the threshold does not
    /// linger.
    pub fn record_success(&self, key: &str) {
        let mut map = self.inner.lock().expect("login throttler mutex");
        map.remove(key);
    }

    /// Drop entries with no live block AND no live window failures.
    /// Safe to call at any time; runs in O(n) over the map.
    pub fn cleanup(&self, now: DateTime<Utc>) {
        let mut map = self.inner.lock().expect("login throttler mutex");
        map.retain(|_, state| !state.is_idle(now, self.config.window));
    }

    /// Number of entries currently tracked. Diagnostic only.
    pub fn tracked_keys(&self) -> usize {
        self.inner.lock().expect("login throttler mutex").len()
    }
}

impl fmt::Debug for LoginThrottler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginThrottler")
            .field("config", &self.config)
            .field("tracked_keys", &self.tracked_keys())
            .finish()
    }
}

/// Normalize a login identifier (typically an email) into the throttle
/// key. Lower-cases ASCII and trims surrounding whitespace.
///
/// Centralizing normalization here means the route layer cannot
/// accidentally key on a casing variant — `Alice@example.com` and
/// `alice@example.com` MUST share throttle state. Non-ASCII characters
/// are left untouched; v1 emails are ASCII per `validate_email` at the
/// DTO boundary.
#[must_use]
pub fn normalize_login_identifier(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(s: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + s, 0).single().unwrap()
    }

    fn fast_throttler() -> LoginThrottler {
        // Tighter policy than production so tests stay literal.
        LoginThrottler::new(LoginThrottleConfig {
            max_failures: 3,
            window: Duration::seconds(60),
            block: Duration::seconds(120),
        })
    }

    #[test]
    fn unknown_key_is_allowed() {
        let t = fast_throttler();
        assert_eq!(t.check("alice", at(0)), ThrottleDecision::Allowed);
    }

    #[test]
    fn first_n_failures_are_allowed() {
        let t = fast_throttler();
        // max_failures = 3 — the first two failures leave the next
        // check still Allowed; the third trips the block.
        t.record_failure("alice", at(0));
        assert_eq!(t.check("alice", at(1)), ThrottleDecision::Allowed);
        t.record_failure("alice", at(2));
        assert_eq!(t.check("alice", at(3)), ThrottleDecision::Allowed);
    }

    #[test]
    fn exceeding_threshold_blocks_subsequent_attempts() {
        let t = fast_throttler();
        for s in 0..3 {
            t.record_failure("alice", at(s));
        }
        match t.check("alice", at(4)) {
            ThrottleDecision::Throttled {
                retry_after_seconds,
            } => {
                // Block = 120s, 4s into the block → ~116s remaining.
                assert!((100..=120).contains(&retry_after_seconds));
            }
            other => panic!("expected Throttled, got {other:?}"),
        }
    }

    #[test]
    fn block_expires_after_block_duration() {
        let t = fast_throttler();
        for s in 0..3 {
            t.record_failure("alice", at(s));
        }
        // Block was set at t=2 for 120s → unblocked at t=122.
        assert!(matches!(
            t.check("alice", at(121)),
            ThrottleDecision::Throttled { .. }
        ));
        assert_eq!(t.check("alice", at(123)), ThrottleDecision::Allowed);
    }

    #[test]
    fn record_success_clears_state() {
        let t = fast_throttler();
        for s in 0..3 {
            t.record_failure("alice", at(s));
        }
        assert!(matches!(
            t.check("alice", at(4)),
            ThrottleDecision::Throttled { .. }
        ));
        t.record_success("alice");
        assert_eq!(t.check("alice", at(5)), ThrottleDecision::Allowed);
        // And the bucket really did clear — a single fresh failure
        // does not re-trip the block.
        t.record_failure("alice", at(6));
        assert_eq!(t.check("alice", at(7)), ThrottleDecision::Allowed);
    }

    #[test]
    fn failures_outside_window_do_not_count() {
        let t = fast_throttler();
        // Two failures right now, then one well after the 60s window
        // — the old two should have aged out, leaving the third alone.
        t.record_failure("alice", at(0));
        t.record_failure("alice", at(1));
        t.record_failure("alice", at(200));
        assert_eq!(t.check("alice", at(201)), ThrottleDecision::Allowed);
    }

    #[test]
    fn distinct_keys_are_isolated() {
        let t = fast_throttler();
        for s in 0..3 {
            t.record_failure("alice", at(s));
        }
        // Bob's bucket is untouched.
        assert_eq!(t.check("bob", at(4)), ThrottleDecision::Allowed);
        assert!(matches!(
            t.check("alice", at(4)),
            ThrottleDecision::Throttled { .. }
        ));
    }

    #[test]
    fn normalized_identifiers_share_state() {
        let t = fast_throttler();
        let alice = normalize_login_identifier("Alice@Example.COM");
        let alice_lower = normalize_login_identifier("alice@example.com");
        let alice_padded = normalize_login_identifier("  alice@example.com  ");
        assert_eq!(alice, alice_lower);
        assert_eq!(alice, alice_padded);

        for s in 0..3 {
            t.record_failure(&alice, at(s));
        }
        assert!(matches!(
            t.check(&alice_padded, at(4)),
            ThrottleDecision::Throttled { .. }
        ));
    }

    #[test]
    fn cleanup_drops_idle_entries() {
        let t = fast_throttler();
        t.record_failure("alice", at(0));
        t.record_failure("bob", at(0));
        for s in 0..3 {
            t.record_failure("carol", at(s));
        }
        assert_eq!(t.tracked_keys(), 3);

        // Well past the window AND past Carol's block: every entry
        // should be reapable.
        t.cleanup(at(10_000));
        assert_eq!(t.tracked_keys(), 0);
    }

    #[test]
    fn cleanup_keeps_blocked_entries() {
        let t = fast_throttler();
        for s in 0..3 {
            t.record_failure("carol", at(s));
        }
        // Right after the block was set — Carol is still blocked.
        t.cleanup(at(5));
        assert_eq!(t.tracked_keys(), 1);
    }

    #[test]
    fn debug_does_not_echo_keys() {
        let t = fast_throttler();
        let secret_marker = "alice-DO-NOT-LEAK@example.com";
        t.record_failure(secret_marker, at(0));
        let dbg = format!("{t:?}");
        assert!(
            !dbg.contains(secret_marker),
            "throttler Debug must not echo any tracked key"
        );
        assert!(
            dbg.contains("tracked_keys"),
            "throttler Debug should expose the entry count for diagnostics"
        );
    }

    #[test]
    fn config_v1_default_matches_documented_policy() {
        let cfg = LoginThrottleConfig::V1_DEFAULT;
        assert_eq!(cfg.max_failures, 5);
        assert_eq!(cfg.window.num_minutes(), 15);
        assert_eq!(cfg.block.num_minutes(), 15);
    }

    #[test]
    fn capacity_drop_is_a_silent_noop_for_new_keys() {
        let t = fast_throttler();
        // Pre-fill the map with idle entries and force the cap by
        // crafting more keys than `MAX_TRACKED_KEYS`. We use a small
        // override here so the test runs quickly.
        //
        // The behavior under saturation: existing keys still record
        // failures (the entry is already present), but a brand-new key
        // is silently dropped after a cleanup pass that doesn't free
        // anything — i.e. when every entry is itself live.
        //
        // To exercise the hard-cap branch we simulate by inserting
        // `MAX_TRACKED_KEYS` blocked entries directly so cleanup
        // cannot reap them.
        {
            let mut map = t.inner.lock().expect("lock");
            for i in 0..MAX_TRACKED_KEYS {
                let mut state = AttemptState::new();
                state.blocked_until = Some(at(10_000));
                map.insert(format!("k{i}"), state);
            }
        }
        // A new key now should not be inserted.
        t.record_failure("brand-new-key", at(0));
        assert_eq!(
            t.check("brand-new-key", at(0)),
            ThrottleDecision::Allowed,
            "saturated map drops new keys silently — fail-open"
        );
        // Existing keys still record failures.
        t.record_failure("k0", at(1));
        assert!(t.tracked_keys() <= MAX_TRACKED_KEYS);
    }
}
