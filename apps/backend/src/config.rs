//! Typed configuration loader.
//!
//! Source order (later wins):
//! 1. baked-in defaults
//! 2. `config/relayterm.toml` (if present)
//! 3. environment variables (`RELAYTERM_*`, double-underscore = nesting)
//!
//! Only enough to boot — fields are added as the surfaces that need them
//! land.

use std::{fmt, net::SocketAddr, path::Path, str::FromStr};

use anyhow::{Context, anyhow, bail};
use relayterm_vault::VaultMasterKey;
use serde::Deserialize;
use zeroize::Zeroizing;

#[derive(Debug)]
pub(crate) struct Config {
    pub(crate) server: ServerConfig,
    pub(crate) database: DatabaseConfig,
    pub(crate) auth: AuthConfig,
    pub(crate) vault: VaultConfig,
    pub(crate) terminal_recording: TerminalRecordingConfig,
    pub(crate) terminal_sessions: TerminalSessionsConfig,
}

#[derive(Debug)]
pub(crate) struct ServerConfig {
    pub(crate) bind: SocketAddr,
}

/// `Debug` is implemented manually so the password segment of the
/// Postgres URL never reaches a log line. A typical operator URL looks
/// like `postgres://relayterm:s3cret@host:5432/db` — the `s3cret`
/// segment is masked while host/db stay visible for diagnostics.
pub(crate) struct DatabaseConfig {
    pub(crate) url: String,
    pub(crate) max_connections: u32,
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("url", &format_args!("{}", redact_database_url(&self.url)))
            .field("max_connections", &self.max_connections)
            .finish()
    }
}

/// Mask the password portion of a `scheme://user:password@host/...` URL.
///
/// Returns the URL unchanged if no `://` or no `@` is present (not a
/// password-bearing form), and replaces the password between `:` and `@`
/// with `***`. This is a structural mask, not a parser — the goal is to
/// keep `Debug` output safe without dragging in a URL crate, not to
/// validate the URL.
fn redact_database_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let after_scheme_at = scheme_end + 3;
    let rest = &url[after_scheme_at..];
    let Some(at_pos) = rest.find('@') else {
        return url.to_owned();
    };
    let userinfo = &rest[..at_pos];
    let host_and_path = &rest[at_pos..];
    let masked_userinfo = match userinfo.find(':') {
        Some(colon) => format!("{}:***", &userinfo[..colon]),
        None => userinfo.to_owned(),
    };
    format!(
        "{}://{}{}",
        &url[..scheme_end],
        masked_userinfo,
        host_and_path
    )
}

/// Top-level authentication mode. Decided at boot from typed config and is
/// fail-fast if misconfigured (see [`Config::validate_auth`]).
///
/// Both modes route requests through the same real-auth code path
/// (`AuthenticatedUser` extractor, password verification, opaque server-
/// side sessions). The difference is the boot-time validation envelope:
/// [`AuthMode::Production`] requires a session signing key, a non-empty
/// `allowed_origins` list, and `cookie_secure = true`; [`AuthMode::Dev`]
/// relaxes those requirements for local development. See `SPEC.md`
/// "Production authentication architecture → Auth mode model" for the
/// full contract.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AuthMode {
    #[default]
    Dev,
    Production,
}

impl AuthMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AuthMode::Dev => "dev",
            AuthMode::Production => "production",
        }
    }
}

impl FromStr for AuthMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dev" => Ok(AuthMode::Dev),
            "production" => Ok(AuthMode::Production),
            other => Err(anyhow!(
                "unrecognized auth.mode value (expected \"dev\" or \"production\"): {other:?}"
            )),
        }
    }
}

/// Production authentication configuration.
///
/// `mode` selects the validation envelope; the remaining fields shape
/// runtime auth behaviour (cookie flags, CSRF allow-list, bootstrap-
/// token policy). The session-signing key (`session_signing_key_b64` /
/// `session_signing_key_file`) is reserved for future signed-CSRF /
/// signed-cookie variants — required for `auth.mode = production` per
/// SPEC.md "Security properties to test" property 1, but not yet
/// consumed by the v1 hashed-opaque-token session model.
///
/// `Debug` is implemented manually so the secret-shaped fields
/// (`session_signing_key_b64`, `first_user_bootstrap_token`) never reach a
/// log line — only their *presence* is rendered, mirroring [`VaultConfig`].
pub(crate) struct AuthConfig {
    pub(crate) mode: AuthMode,
    pub(crate) session_signing_key_b64: Option<String>,
    pub(crate) session_signing_key_file: Option<std::path::PathBuf>,
    pub(crate) first_user_bootstrap_token: Option<String>,
    pub(crate) cookie_secure: bool,
    pub(crate) cookie_domain: Option<String>,
    pub(crate) allowed_origins: Vec<String>,
}

impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("mode", &self.mode)
            .field(
                "session_signing_key_b64_set",
                &self.session_signing_key_b64.is_some(),
            )
            .field("session_signing_key_file", &self.session_signing_key_file)
            .field(
                "first_user_bootstrap_token_set",
                &self.first_user_bootstrap_token.is_some(),
            )
            .field("cookie_secure", &self.cookie_secure)
            .field("cookie_domain", &self.cookie_domain)
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
}

/// Vault (encrypted private-key store) configuration.
///
/// Exactly one of `master_key_b64` or `master_key_file` must resolve at
/// boot. `Debug` is implemented manually so the resolved key bytes never
/// reach a log line — only the *presence* of each source is rendered.
///
/// Boot rules (enforced by [`Config::vault_master_key`]):
/// * If `enabled` is `true` and no source resolves → startup fails.
/// * Both sources set → startup fails (ambiguous).
/// * No silent fallback to a randomly generated key.
/// * Error messages mention which source failed (file vs. base64) but
///   never echo the value or any prefix of it.
pub(crate) struct VaultConfig {
    pub(crate) enabled: bool,
    pub(crate) master_key_b64: Option<String>,
    pub(crate) master_key_file: Option<std::path::PathBuf>,
}

impl fmt::Debug for VaultConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultConfig")
            .field("enabled", &self.enabled)
            .field("master_key_b64_set", &self.master_key_b64.is_some())
            .field("master_key_file", &self.master_key_file)
            .finish()
    }
}

/// Encryption mode for durable terminal-recording payloads.
///
/// `Disabled` writes plaintext-at-rest chunks (`payload` column carries
/// raw PTY output bytes). `Required` writes XChaCha20-Poly1305 envelope
/// rows AND, in production, fails boot if no master key resolves. The
/// design doc `docs/terminal-recording.md` Section 6.3 spells out the
/// at-rest threat model and the operator warning that gates the choice.
///
/// The `optional` mode reserved by the design (write encrypted when a
/// key is present, fall through to plaintext otherwise) is intentionally
/// NOT modeled here yet — adding a third state without a writer slice
/// would let an operator configure a posture that no code branches on.
/// A later slice that implements the chunk writer adds it explicitly.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TerminalRecordingEncryptionMode {
    /// No envelope; chunk rows persist plaintext PTY bytes. Operator
    /// has accepted the documented at-rest risk; production refuses
    /// this combination when `terminal_recording.enabled = true`.
    #[default]
    Disabled,
    /// XChaCha20-Poly1305 envelope keyed by the recording master key.
    /// Mandatory in production when recording is enabled. The schema
    /// reserves `encryption = 1` for this mode (see Section 6.3 of the
    /// design doc).
    Required,
}

impl FromStr for TerminalRecordingEncryptionMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "disabled" => Ok(Self::Disabled),
            "required" => Ok(Self::Required),
            other => Err(anyhow!(
                "unrecognized terminal_recording.encryption.mode value (expected \
                 \"disabled\" or \"required\"): {other:?}"
            )),
        }
    }
}

/// Compression mode for durable terminal-recording payloads.
///
/// Only `None` (plain bytes, no zstd) ships in v1; the design doc
/// reserves `zstd` for a later slice (Section 6.2). Modeled as an enum
/// so the env/TOML parsers reject unknown values *now*, before a future
/// reader has to deal with operator-supplied garbage.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TerminalRecordingCompressionMode {
    #[default]
    None,
}

impl FromStr for TerminalRecordingCompressionMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            other => Err(anyhow!(
                "unrecognized terminal_recording.compression.mode value (expected \
                 \"none\"): {other:?}"
            )),
        }
    }
}

/// Encryption configuration for terminal recording.
///
/// `mode` selects the envelope; `master_key_b64` / `master_key_file` are
/// the two key sources. Exactly one source must be set when `mode =
/// required`. `Debug` is implemented manually so the raw key value never
/// reaches a log line — only the *presence* of each source is rendered,
/// mirroring [`VaultConfig`].
pub(crate) struct TerminalRecordingEncryptionConfig {
    pub(crate) mode: TerminalRecordingEncryptionMode,
    pub(crate) master_key_b64: Option<String>,
    pub(crate) master_key_file: Option<std::path::PathBuf>,
}

impl fmt::Debug for TerminalRecordingEncryptionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalRecordingEncryptionConfig")
            .field("mode", &self.mode)
            .field("master_key_b64_set", &self.master_key_b64.is_some())
            .field("master_key_file", &self.master_key_file)
            .finish()
    }
}

/// Compression configuration for terminal recording. Wrapped in its own
/// struct (instead of a bare enum on [`TerminalRecordingConfig`]) so the
/// TOML / env layout matches the design's `[terminal_recording.compression]`
/// section and can grow operator-tunable knobs (e.g. zstd level) in a
/// later slice without breaking the existing keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalRecordingCompressionConfig {
    pub(crate) mode: TerminalRecordingCompressionMode,
}

/// Retention-cleanup configuration for durable terminal recording.
///
/// Operator-facing knobs for the future retention worker. THIS SLICE IS
/// CONFIG-ONLY — no caller drives the cleanup primitive yet (no startup
/// sweep, no periodic worker, no admin / user-triggered purge). The
/// fields are parsed, validated, and otherwise unused at runtime;
/// existing boot behaviour is unchanged. The shape is defined now so
/// the future worker has a canonical place to read from and operators
/// can stage their retention posture without a follow-up code change.
///
/// **Independence from `terminal_recording.enabled`** — load-bearing.
/// Cleanup MUST be allowed to run even when recording is disabled, so
/// turning recording OFF after running it for some time does NOT make
/// the existing recording corpus immortal. The recording writer is
/// gated on `terminal_recording.enabled`; cleanup is gated on
/// `cleanup.enabled`. The two switches are independent and serve
/// different purposes. The validator therefore inspects this struct
/// regardless of the parent `enabled` flag.
///
/// Field semantics (canonical contract: `docs/terminal-recording.md`
/// Section 12.6 / 12.7):
/// * `enabled` — master switch for the future worker. `true` means
///   "the cleanup worker MAY run when it is implemented"; `false`
///   means "do not sweep". No-op today.
/// * `startup_sweep_enabled` — when the future Stage A startup sweep
///   lands, run a single bounded purge at boot before the listener
///   binds.
/// * `periodic_sweep_enabled` — when the future Stage B periodic
///   managed worker lands, run on the cadence in
///   `sweep_interval_seconds`. `false` means "no periodic worker
///   even if the implementation exists".
/// * `sweep_interval_seconds` — periodic cadence. Sentinel `0` means
///   "no periodic schedule"; any non-zero value is bounded
///   `60..=604800`. Sub-60s cadence creates a thundering-herd
///   against an empty corpus; `> 7d` defers retention past the
///   default 30-day window without operator intent.
/// * `batch_size` — max sessions touched per sweep iteration. Each
///   session is its own transaction in the future worker, so a
///   batch boundary is the natural pause point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalRecordingCleanupConfig {
    pub(crate) enabled: bool,
    pub(crate) startup_sweep_enabled: bool,
    pub(crate) periodic_sweep_enabled: bool,
    pub(crate) sweep_interval_seconds: u64,
    pub(crate) batch_size: u32,
}

/// Durable terminal-recording configuration.
///
/// `enabled = false` is the default and the recommended posture for
/// every deployment until the writer slice (Section 13 step 3 of
/// `docs/terminal-recording.md`) lands. Adding this config in step 1b
/// changes NO runtime behaviour — there is no chunk writer, no replay
/// API, no UI. The validation envelope merely refuses to start a
/// production deploy that has *declared intent* to record without a key.
///
/// Numeric defaults match Section 6.1 / 12 of the design doc:
/// * `retention_days = 30` (per-session retention from `closed_at`)
/// * `max_bytes_per_session = 64 MiB` (per-session byte cap)
/// * `chunk_target_bytes = 64 KiB` (soft chunk flush target)
/// * `chunk_hard_cap_bytes = 2 MiB` (defence-in-depth row size cap;
///   covers the 1 MiB envelope frame plus AEAD overhead)
///
/// These numbers do nothing today; they exist so the writer slice has a
/// canonical place to read them from and operators can tune posture
/// without a code change. The validation envelope enforces internal
/// consistency (`chunk_target_bytes <= chunk_hard_cap_bytes`,
/// `chunk_hard_cap_bytes >= 1 MiB + envelope`,
/// `max_bytes_per_session >= chunk_hard_cap_bytes`, retention bounded).
pub(crate) struct TerminalRecordingConfig {
    pub(crate) enabled: bool,
    pub(crate) retention_days: u32,
    pub(crate) max_bytes_per_session: u64,
    pub(crate) chunk_target_bytes: u32,
    pub(crate) chunk_hard_cap_bytes: u32,
    pub(crate) encryption: TerminalRecordingEncryptionConfig,
    pub(crate) compression: TerminalRecordingCompressionConfig,
    pub(crate) cleanup: TerminalRecordingCleanupConfig,
}

impl fmt::Debug for TerminalRecordingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalRecordingConfig")
            .field("enabled", &self.enabled)
            .field("retention_days", &self.retention_days)
            .field("max_bytes_per_session", &self.max_bytes_per_session)
            .field("chunk_target_bytes", &self.chunk_target_bytes)
            .field("chunk_hard_cap_bytes", &self.chunk_hard_cap_bytes)
            .field("encryption", &self.encryption)
            .field("compression", &self.compression)
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

/// Numeric defaults for [`TerminalRecordingConfig`]. Pulled into
/// constants so the test suite, the example TOML, and the production
/// docs reference the same source of truth.
pub(crate) mod terminal_recording_defaults {
    /// Retention window from `terminal_sessions.closed_at`.
    pub(crate) const RETENTION_DAYS: u32 = 30;
    /// Maximum retention window the validator accepts. ~10 years is
    /// already past any reasonable single-tenant compliance window;
    /// values above this almost certainly indicate a unit confusion
    /// (e.g. minutes-as-days) and are easier to refuse than to debug.
    pub(crate) const RETENTION_DAYS_HARD_CAP: u32 = 3_650;
    /// Per-session byte cap (64 MiB).
    pub(crate) const MAX_BYTES_PER_SESSION: u64 = 64 * 1024 * 1024;
    /// Per-session byte cap upper bound the validator accepts (1 TiB).
    /// Same intent as the retention cap: refuse "infinite" values.
    pub(crate) const MAX_BYTES_PER_SESSION_HARD_CAP: u64 = 1024 * 1024 * 1024 * 1024;
    /// Soft chunk flush target (64 KiB; matches Section 6.1).
    pub(crate) const CHUNK_TARGET_BYTES: u32 = 64 * 1024;
    /// Defence-in-depth row size cap (2 MiB; matches Section 5.1 / 6.4).
    /// MUST exceed the live wire's 1 MiB single-frame cap plus envelope
    /// overhead so a legitimate workload is never refused.
    pub(crate) const CHUNK_HARD_CAP_BYTES: u32 = 2 * 1024 * 1024;
    /// Lower bound for `chunk_hard_cap_bytes`. Derived from the binary
    /// envelope's 1 MiB single-frame cap plus a comfortable budget for
    /// XChaCha20-Poly1305 overhead (24-byte nonce, 16-byte tag, magic,
    /// version — roughly 41 bytes total). Rounded up to 1 MiB + 64 KiB
    /// to leave room for any future format-version growth without
    /// forcing every existing operator to retune.
    pub(crate) const CHUNK_HARD_CAP_BYTES_FLOOR: u32 = (1024 * 1024) + (64 * 1024);

    // --- Retention-cleanup defaults ----------------------------------
    //
    // The cleanup worker is not yet implemented. These defaults are the
    // typed shape an operator stages today; the validator enforces them
    // at boot so a future worker slice does not have to re-relitigate
    // the bounds. Source of truth: `docs/terminal-recording.md`
    // Section 12.6.

    /// Default for `terminal_recording.cleanup.enabled`. `true` means
    /// "the future cleanup worker MAY run"; flipping this to `false`
    /// is the explicit opt-out an operator declares when they manage
    /// retention out-of-band.
    pub(crate) const CLEANUP_ENABLED: bool = true;
    /// Default for `terminal_recording.cleanup.startup_sweep_enabled`.
    /// `true` means "the future Stage A startup sweep MAY run". No
    /// caller drives the sweep yet, so this is no-op runtime today.
    pub(crate) const CLEANUP_STARTUP_SWEEP_ENABLED: bool = true;
    /// Default for `terminal_recording.cleanup.periodic_sweep_enabled`.
    /// `false` means "do not run a periodic cadence even if Stage B is
    /// later implemented". Operators opt in by flipping the flag AND
    /// setting a non-zero `sweep_interval_seconds`.
    pub(crate) const CLEANUP_PERIODIC_SWEEP_ENABLED: bool = false;
    /// Default for `terminal_recording.cleanup.sweep_interval_seconds`.
    /// `0` is the sentinel "no periodic schedule"; the validator
    /// allows it whenever `periodic_sweep_enabled = false`.
    pub(crate) const CLEANUP_SWEEP_INTERVAL_SECONDS: u64 = 0;
    /// Default for `terminal_recording.cleanup.batch_size`. Matches
    /// the design's recommended starting cadence — small enough that
    /// a long retention backlog does not block boot, large enough
    /// that the periodic worker makes useful progress per tick.
    pub(crate) const CLEANUP_BATCH_SIZE: u32 = 100;
    /// Lower bound for a non-zero `sweep_interval_seconds`. Sub-60s
    /// cadence creates a thundering-herd against an empty corpus.
    pub(crate) const CLEANUP_SWEEP_INTERVAL_SECONDS_MIN: u64 = 60;
    /// Upper bound for `sweep_interval_seconds`. Anything above one
    /// week defers retention past the default 30-day window without
    /// operator intent and almost certainly indicates a unit mistake.
    pub(crate) const CLEANUP_SWEEP_INTERVAL_SECONDS_MAX: u64 = 7 * 24 * 60 * 60;
    /// Lower bound for `batch_size`. A zero-batch worker is a config
    /// mistake — collapse to a typed boot failure rather than a no-op
    /// runtime that silently never sweeps.
    pub(crate) const CLEANUP_BATCH_SIZE_MIN: u32 = 1;
    /// Upper bound for `batch_size`. 10k sessions per tick is already
    /// well past the bounded-per-batch design intent (Section 12.7);
    /// values above almost certainly indicate a unit mistake (rows
    /// vs. sessions, or "all of them").
    pub(crate) const CLEANUP_BATCH_SIZE_MAX: u32 = 10_000;
}

/// Live-terminal-session orchestration configuration.
///
/// Currently exposes a single operator knob: how long a still-allocated
/// SSH PTY is allowed to linger after every client has detached, before
/// the orchestrator reaps it.
///
/// **Scope is deliberately narrow.** This is a *short-term reconnect
/// grace window* on a still-live PTY held by the running backend — it
/// is NOT durable session resume. A backend restart drops every live
/// PTY regardless of this setting. Long-term persistent sessions
/// (`tmux`/`screen`-style resurrection across restarts, days-long
/// retention, durable shell state) are a separate, future
/// architecture; bumping this value past minutes does not move toward
/// that — it just keeps remote shells pinned alive longer on this
/// process. Higher values consume backend RAM, file descriptors, and
/// the SSH server's PTY budget for the full duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalSessionsConfig {
    /// Lifetime of a detached live PTY before reap, in whole seconds.
    /// Bounded `5..=86_400` by [`Config::validate_terminal_sessions`].
    /// `0` and out-of-range values are a hard boot failure rather than
    /// a silent fall-through to the default — operator intent and
    /// runtime state stay aligned.
    pub(crate) detached_live_pty_ttl_seconds: u64,
    /// Per-user ceiling on concurrent live PTY runtimes (Phase 1B.1 of
    /// the session-quota policy in `docs/session-quotas.md`). Counted
    /// against the in-memory `TerminalSessionManager` registry — both
    /// `active` and `detached` rows are live PTYs and count equally.
    /// Bounded `1..=256` by [`Config::validate_terminal_sessions`]; `0`
    /// would refuse every create and is a hard boot failure.
    pub(crate) max_live_pty_sessions_per_user: u32,
    /// Per-user ceiling on concurrent in-flight starting sessions
    /// (Phase 1B.2a of `docs/session-quotas.md` § 4.3). Counts the
    /// disjoint set of registry entries with `live = None` AND
    /// `snapshot.status == Starting`, so the live and starting quotas
    /// never double-count. Defends against a runaway client that POSTs
    /// many sessions in flight without waiting for the PTY-start
    /// round-trip to complete. Bounded `1..=32` by
    /// [`Config::validate_terminal_sessions`]; `0` would deadlock every
    /// create and is a hard boot failure.
    pub(crate) max_starting_sessions_per_user: u32,
    /// Deployment-wide ceiling on concurrent live PTY runtime entries
    /// across ALL owners (Phase 1B.2b of `docs/session-quotas.md`
    /// § 4.2). Counted against THIS backend instance's in-memory
    /// `TerminalSessionManager` registry — exact for single-instance
    /// deployments, per-instance best-effort for any multi-instance
    /// topology (§ 9 "Multi-instance limitations"). Bounded
    /// `1..=4096` by [`Config::validate_terminal_sessions`]; the
    /// validator additionally rejects values below the per-user live
    /// or starting caps (a per-user ceiling above the deployment
    /// ceiling would be a contradiction — § 5.2). Deliberately NOT
    /// exposed via `GET /api/v1/config/session-policy` (§ 5.4 —
    /// operator-only, fingerprinting risk).
    pub(crate) max_live_pty_sessions_per_deployment: u32,
}

/// Numeric defaults / bounds for [`TerminalSessionsConfig`]. Pulled into
/// constants so the test suite, the example TOML files, and the docs
/// reference the same source of truth.
pub(crate) mod terminal_sessions_defaults {
    /// Default detached-live-PTY TTL. Matches the historical hard-coded
    /// `relayterm_terminal::DETACHED_LIVE_PTY_TTL` value so a deploy
    /// that does not touch this knob behaves identically to the
    /// pre-config baseline.
    pub(crate) const DETACHED_LIVE_PTY_TTL_SECONDS: u64 = 30;
    /// Lower bound for the detached-live-PTY TTL. Below this, the
    /// reconnect grace window is shorter than a typical browser tab
    /// reload + handoff round-trip, defeating the purpose. `0` is
    /// rejected separately as "always reap immediately", which would
    /// surprise every reconnect path.
    pub(crate) const DETACHED_LIVE_PTY_TTL_SECONDS_MIN: u64 = 5;
    /// Upper bound for the detached-live-PTY TTL (24h). Anything
    /// above this almost always indicates a unit confusion (minutes
    /// vs. seconds), and "live SSH PTY held open for >1 day after
    /// disconnect" is far past the *short-term reconnect grace*
    /// scope of this knob — durable persistent sessions are a
    /// separate, future architecture.
    pub(crate) const DETACHED_LIVE_PTY_TTL_SECONDS_MAX: u64 = 24 * 60 * 60;
    /// Default per-user live PTY ceiling. Phase 1B.1 of
    /// `docs/session-quotas.md` recommends `8` — conservative for solo
    /// homelab use and defensible for a small multi-user deployment.
    pub(crate) const MAX_LIVE_PTY_SESSIONS_PER_USER: u32 = 8;
    /// Lower bound for the per-user live PTY ceiling. `0` would refuse
    /// every create and is a config mistake, so the validator rejects
    /// it with a clearer error than "no session can ever start".
    pub(crate) const MAX_LIVE_PTY_SESSIONS_PER_USER_MIN: u32 = 1;
    /// Upper bound for the per-user live PTY ceiling. `256` per user
    /// on a single-tenant deployment is far past the practical
    /// resource ceiling (channels + PTYs + buffers + tasks per user).
    /// Anything above is almost certainly a unit / digit-shift mistake.
    pub(crate) const MAX_LIVE_PTY_SESSIONS_PER_USER_MAX: u32 = 256;
    /// Default per-user starting-burst ceiling. Phase 1B.2a of
    /// `docs/session-quotas.md` § 4.3 names `4` — enough for honest
    /// SPA burst behaviour (a navigation that opens a few sessions in
    /// parallel) but rejects a tight POST loop.
    pub(crate) const MAX_STARTING_SESSIONS_PER_USER: u32 = 4;
    /// Lower bound for the per-user starting-burst ceiling. `0` would
    /// deadlock every create and is a config mistake, so the validator
    /// rejects it with a clearer error than "no session can ever
    /// start".
    pub(crate) const MAX_STARTING_SESSIONS_PER_USER_MIN: u32 = 1;
    /// Upper bound for the per-user starting-burst ceiling. `32` is
    /// well past any honest burst pattern; the live quota is the
    /// load-bearing ceiling and the starting quota only defends against
    /// runaway in-flight POST loops.
    pub(crate) const MAX_STARTING_SESSIONS_PER_USER_MAX: u32 = 32;
    /// Default deployment-wide live PTY ceiling (Phase 1B.2b of
    /// `docs/session-quotas.md` § 4.2). `64` is conservative for a
    /// single-tenant self-hosted v1 deployment; operators running a
    /// multi-user homelab can raise it.
    pub(crate) const MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT: u32 = 64;
    /// Lower bound for the deployment-wide live PTY ceiling. `0`
    /// disables the backend (every create refuses) and is a config
    /// mistake, so the validator rejects it with a clearer error.
    pub(crate) const MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MIN: u32 = 1;
    /// Upper bound for the deployment-wide live PTY ceiling. `4096` is
    /// past the kernel-side FD ceiling on most single-host deployments,
    /// so anything above is almost certainly a configuration mistake.
    pub(crate) const MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MAX: u32 = 4096;
}

impl Config {
    pub(crate) fn load() -> anyhow::Result<Self> {
        let mut cfg = Self::defaults();

        if let Some(path) = Self::resolve_file() {
            let raw = std::fs::read_to_string(&path)?;
            let from_file: FileConfig = toml::from_str(&raw)?;
            from_file.merge_into(&mut cfg);
        }

        Self::apply_env(&mut cfg)?;
        Ok(cfg)
    }

    fn defaults() -> Self {
        Self {
            server: ServerConfig {
                bind: "127.0.0.1:8080"
                    .parse()
                    .expect("static default addr is valid"),
            },
            database: DatabaseConfig {
                url: "postgres://relayterm:relayterm@127.0.0.1:5432/relayterm".to_owned(),
                max_connections: 10,
            },
            // Default to dev mode so local development keeps booting
            // unchanged. Production deploys MUST explicitly set
            // `auth.mode = production`, which then enforces the strict
            // boot-time envelope (session signing key, non-empty
            // `allowed_origins`, `cookie_secure = true`).
            auth: AuthConfig {
                mode: AuthMode::default(),
                session_signing_key_b64: None,
                session_signing_key_file: None,
                first_user_bootstrap_token: None,
                cookie_secure: true,
                cookie_domain: None,
                allowed_origins: Vec::new(),
            },
            vault: VaultConfig {
                enabled: true,
                master_key_b64: None,
                master_key_file: None,
            },
            // Recording is OFF by default. The chunk writer is wired
            // when an operator opts in (`enabled = true` AND
            // `encryption.mode = disabled` — dev-only for now, since
            // `auth.mode = production` rejects the disabled mode at
            // boot to refuse plaintext-at-rest; see Section 13 of
            // `docs/terminal-recording.md`). The replay API, retention
            // worker, encryption-aware writer, and replay UI are still
            // later slices. See SPEC.md "Durable terminal recording
            // and replay architecture" for the staged plan.
            terminal_recording: TerminalRecordingConfig {
                enabled: false,
                retention_days: terminal_recording_defaults::RETENTION_DAYS,
                max_bytes_per_session: terminal_recording_defaults::MAX_BYTES_PER_SESSION,
                chunk_target_bytes: terminal_recording_defaults::CHUNK_TARGET_BYTES,
                chunk_hard_cap_bytes: terminal_recording_defaults::CHUNK_HARD_CAP_BYTES,
                encryption: TerminalRecordingEncryptionConfig {
                    mode: TerminalRecordingEncryptionMode::default(),
                    master_key_b64: None,
                    master_key_file: None,
                },
                compression: TerminalRecordingCompressionConfig::default(),
                cleanup: TerminalRecordingCleanupConfig {
                    enabled: terminal_recording_defaults::CLEANUP_ENABLED,
                    startup_sweep_enabled:
                        terminal_recording_defaults::CLEANUP_STARTUP_SWEEP_ENABLED,
                    periodic_sweep_enabled:
                        terminal_recording_defaults::CLEANUP_PERIODIC_SWEEP_ENABLED,
                    sweep_interval_seconds:
                        terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS,
                    batch_size: terminal_recording_defaults::CLEANUP_BATCH_SIZE,
                },
            },
            terminal_sessions: TerminalSessionsConfig {
                detached_live_pty_ttl_seconds:
                    terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS,
                max_live_pty_sessions_per_user:
                    terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER,
                max_starting_sessions_per_user:
                    terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER,
                max_live_pty_sessions_per_deployment:
                    terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT,
            },
        }
    }

    fn resolve_file() -> Option<std::path::PathBuf> {
        if let Ok(explicit) = std::env::var("RELAYTERM_CONFIG") {
            let p = std::path::PathBuf::from(explicit);
            return p.is_file().then_some(p);
        }
        let candidate = Path::new("config/relayterm.toml");
        candidate.is_file().then(|| candidate.to_path_buf())
    }

    fn apply_env(cfg: &mut Self) -> anyhow::Result<()> {
        Self::apply_env_with(cfg, |k| std::env::var(k).ok())
    }

    /// Test-friendly variant of [`Config::apply_env`] that reads variables
    /// through a caller-supplied getter rather than the global process
    /// environment. Production callers go through [`Config::apply_env`];
    /// tests inject a `HashMap`-backed closure so they can exercise env
    /// parsing without touching shared process state.
    fn apply_env_with<F>(cfg: &mut Self, getenv: F) -> anyhow::Result<()>
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(v) = getenv("RELAYTERM_SERVER__BIND")
            && let Ok(parsed) = v.parse()
        {
            cfg.server.bind = parsed;
        }
        if let Some(v) = getenv("RELAYTERM_DATABASE__URL") {
            cfg.database.url = v;
        }
        if let Some(v) = getenv("RELAYTERM_DATABASE__MAX_CONNECTIONS")
            && let Ok(parsed) = v.parse()
        {
            cfg.database.max_connections = parsed;
        }
        // DATABASE_URL is honored as a convenience for `sqlx-cli` parity.
        if let Some(v) = getenv("DATABASE_URL") {
            cfg.database.url = v;
        }
        // Auth mode is parsed strictly: an unrecognized value is a hard
        // boot failure rather than a silent fall-through to default,
        // because misreading "production" → "dev" would silently disable
        // auth on a deploy that asked for it.
        if let Some(v) = getenv("RELAYTERM_AUTH__MODE") {
            cfg.auth.mode = AuthMode::from_str(&v).context("RELAYTERM_AUTH__MODE")?;
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64") {
            cfg.auth.session_signing_key_b64 = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE") {
            cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from(v));
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN") {
            cfg.auth.first_user_bootstrap_token = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__COOKIE_SECURE")
            && let Ok(parsed) = v.parse()
        {
            cfg.auth.cookie_secure = parsed;
        }
        if let Some(v) = getenv("RELAYTERM_AUTH__COOKIE_DOMAIN") {
            cfg.auth.cookie_domain = Some(v);
        }
        // Comma-separated list. Empty entries are dropped so a trailing
        // comma or stray whitespace does not silently widen the allow-list.
        if let Some(v) = getenv("RELAYTERM_AUTH__ALLOWED_ORIGINS") {
            cfg.auth.allowed_origins = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if let Some(v) = getenv("RELAYTERM_VAULT__ENABLED")
            && let Ok(parsed) = v.parse()
        {
            cfg.vault.enabled = parsed;
        }
        if let Some(v) = getenv("RELAYTERM_VAULT__MASTER_KEY_B64") {
            cfg.vault.master_key_b64 = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_VAULT__MASTER_KEY_FILE") {
            cfg.vault.master_key_file = Some(std::path::PathBuf::from(v));
        }
        // Terminal recording. Stricter parse policy than the vault /
        // auth scalar fields above: a malformed scalar env var is a hard
        // boot failure here rather than a silent fall-through to default.
        // Reasoning: `RELAYTERM_TERMINAL_RECORDING__ENABLED=yes` (or `1`,
        // or any other not-a-bool typo) under the silent-discard pattern
        // would leave recording disabled while the operator believed the
        // env override took effect — and the production envelope's
        // `enabled = true ⇒ encryption.mode = required` gate is designed
        // to catch exactly the misconfigured case where intent and
        // configured state diverge. The same posture applies to the
        // numeric bounds: silent fall-through to a default that no longer
        // matches operator intent is a worse outcome than a startup
        // error pointing at the bad input.
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__ENABLED") {
            cfg.terminal_recording.enabled =
                v.parse().context("RELAYTERM_TERMINAL_RECORDING__ENABLED")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__RETENTION_DAYS") {
            cfg.terminal_recording.retention_days = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__RETENTION_DAYS")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__MAX_BYTES_PER_SESSION") {
            cfg.terminal_recording.max_bytes_per_session = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__MAX_BYTES_PER_SESSION")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CHUNK_TARGET_BYTES") {
            cfg.terminal_recording.chunk_target_bytes = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CHUNK_TARGET_BYTES")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CHUNK_HARD_CAP_BYTES") {
            cfg.terminal_recording.chunk_hard_cap_bytes = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CHUNK_HARD_CAP_BYTES")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MODE") {
            cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::from_str(&v)
                .context("RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MODE")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_B64") {
            cfg.terminal_recording.encryption.master_key_b64 = Some(v);
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_FILE") {
            cfg.terminal_recording.encryption.master_key_file = Some(std::path::PathBuf::from(v));
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__COMPRESSION__MODE") {
            cfg.terminal_recording.compression.mode =
                TerminalRecordingCompressionMode::from_str(&v)
                    .context("RELAYTERM_TERMINAL_RECORDING__COMPRESSION__MODE")?;
        }
        // Retention-cleanup env overrides. Same strict-parse posture as
        // the recording scalars above: a malformed value is a hard boot
        // failure rather than a silent fall-through to default. The
        // cleanup worker is not yet wired, but an operator who set
        // `RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE=abc`
        // expecting an override would otherwise believe the value took
        // effect — fail-fast keeps configured intent and configured
        // state aligned, exactly as Section 12.6 of
        // `docs/terminal-recording.md` requires.
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED") {
            cfg.terminal_recording.cleanup.enabled = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CLEANUP__STARTUP_SWEEP_ENABLED") {
            cfg.terminal_recording.cleanup.startup_sweep_enabled = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CLEANUP__STARTUP_SWEEP_ENABLED")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED") {
            cfg.terminal_recording.cleanup.periodic_sweep_enabled = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS") {
            cfg.terminal_recording.cleanup.sweep_interval_seconds = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE") {
            cfg.terminal_recording.cleanup.batch_size = v
                .parse()
                .context("RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE")?;
        }
        // Live-terminal-session orchestration. Same strict-parse posture
        // as the recording scalars above: a malformed value is a hard
        // boot failure rather than a silent fall-through to default, so
        // operator intent and runtime state stay aligned. The bounds
        // (positive, in `5..=86_400`) are enforced separately by
        // `validate_terminal_sessions` after merge — this stage only
        // checks that the input parses as a `u64`.
        if let Some(v) = getenv("RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS") {
            cfg.terminal_sessions.detached_live_pty_ttl_seconds = v
                .parse()
                .context("RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER") {
            cfg.terminal_sessions.max_live_pty_sessions_per_user = v
                .parse()
                .context("RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER") {
            cfg.terminal_sessions.max_starting_sessions_per_user = v
                .parse()
                .context("RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER")?;
        }
        if let Some(v) = getenv("RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT")
        {
            cfg.terminal_sessions.max_live_pty_sessions_per_deployment = v
                .parse()
                .context("RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT")?;
        }
        Ok(())
    }

    /// Validate the auth configuration at boot.
    ///
    /// Boot-time gate: inspects the resolved [`AuthConfig`] and refuses to
    /// proceed when it describes a state the running build cannot serve
    /// safely. Does NOT consume any secret material — secrets are still
    /// owned by `AuthConfig` after a successful return. Error messages
    /// name the failing input but never echo a value (same redaction
    /// posture as [`Config::vault_master_key`]).
    ///
    /// Cases:
    /// * `mode = dev` → always Ok. Local development picks its own
    ///   cookie/origin posture (insecure cookies, loopback origins,
    ///   missing signing key are all acceptable). The same real-auth
    ///   code path runs as in production; only the validation envelope
    ///   differs.
    /// * `mode = production` → enforce, in this order:
    ///   1. Exactly one of `session_signing_key_b64` /
    ///      `session_signing_key_file` is set (zero → reject; both →
    ///      reject as ambiguous).
    ///   2. `allowed_origins` is non-empty (an empty list rejects every
    ///      browser-write — that is the secure default the CSRF guard
    ///      already enforces, but failing fast here gives a clearer
    ///      operator signal than every POST returning 403).
    ///   3. `cookie_secure = true` (production cookies MUST carry the
    ///      `Secure` flag — there is no escape hatch).
    ///
    /// `first_user_bootstrap_token` is NOT required by this gate; the
    /// bootstrap route returns `503` when the token is unset and
    /// `409 already_bootstrapped` once a first user exists. The
    /// "production + no first user + no token" hard-fail is a runtime
    /// check in `apps/backend/src/main.rs` after the DB connect, not a
    /// config-only check (config cannot see DB state).
    pub(crate) fn validate_auth(&self) -> anyhow::Result<()> {
        match self.auth.mode {
            AuthMode::Dev => Ok(()),
            AuthMode::Production => {
                match (
                    self.auth.session_signing_key_b64.is_some(),
                    self.auth.session_signing_key_file.is_some(),
                ) {
                    (false, false) => bail!(
                        "auth.mode = production requires a session signing key — set \
                         auth.session_signing_key_b64 or auth.session_signing_key_file \
                         (RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64 / \
                         RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE)"
                    ),
                    (true, true) => bail!(
                        "auth.session_signing_key_b64 and auth.session_signing_key_file are \
                         both set; pick exactly one"
                    ),
                    _ => {}
                }
                if self.auth.allowed_origins.is_empty() {
                    bail!(
                        "auth.mode = production requires auth.allowed_origins to list at \
                         least one origin (state-changing browser-write routes reject every \
                         request when the allow-list is empty)"
                    );
                }
                if !self.auth.cookie_secure {
                    bail!(
                        "auth.mode = production requires auth.cookie_secure = true — \
                         session cookies must carry the Secure flag"
                    );
                }
                Ok(())
            }
        }
    }

    /// Validate the durable terminal-recording configuration at boot.
    ///
    /// Same posture as [`Config::validate_auth`]: pure inspection of the
    /// resolved structure, no filesystem reads, no key consumption. Error
    /// messages name the failing field but never echo a key value or any
    /// prefix of it. Numeric bounds are checked unconditionally; key
    /// sources are checked only when `enabled = true`, mirroring the
    /// vault's "disabled means we don't care" policy.
    ///
    /// Production envelope (when `enabled = true` AND `auth.mode =
    /// production`):
    /// 1. `encryption.mode = required` (operator MUST opt in to
    ///    at-rest envelope encryption — `docs/terminal-recording.md`
    ///    Section 6.3 documents the threat model).
    /// 2. Exactly one of `encryption.master_key_b64` /
    ///    `encryption.master_key_file` is set (zero → reject; both →
    ///    reject as ambiguous).
    ///
    /// Cross-cutting (regardless of mode, when `enabled = true`):
    /// * The recording master key MUST be SEPARATE from the vault master
    ///   key. When both configs use the same source kind we compare the
    ///   raw values and reject equal pairs. Mixed sources (one b64, one
    ///   file) cannot be compared without filesystem reads — that gap is
    ///   documented and a future writer slice that loads both keys can
    ///   add a runtime check on resolved bytes.
    /// * Numeric bounds are internally consistent:
    ///   `chunk_target_bytes <= chunk_hard_cap_bytes`,
    ///   `chunk_hard_cap_bytes >= CHUNK_HARD_CAP_BYTES_FLOOR`,
    ///   `max_bytes_per_session >= chunk_hard_cap_bytes`,
    ///   retention bounded `1..=RETENTION_DAYS_HARD_CAP`,
    ///   `max_bytes_per_session <= MAX_BYTES_PER_SESSION_HARD_CAP`.
    pub(crate) fn validate_terminal_recording(&self) -> anyhow::Result<()> {
        let rec = &self.terminal_recording;

        // Numeric bounds run regardless of `enabled` so a misconfigured
        // operator who toggles `enabled = true` later does not boot a
        // half-validated runtime — the bounds are also a useful sanity
        // check on a disabled config that ships pre-tuned numbers.
        if rec.retention_days == 0 {
            bail!(
                "terminal_recording.retention_days must be greater than 0 (got 0); a 0-day \
                 retention would purge every chunk on the next sweep — disable recording \
                 instead"
            );
        }
        if rec.retention_days > terminal_recording_defaults::RETENTION_DAYS_HARD_CAP {
            bail!(
                "terminal_recording.retention_days = {got} exceeds the hard cap of {cap} days; \
                 values above this almost always indicate a unit confusion",
                got = rec.retention_days,
                cap = terminal_recording_defaults::RETENTION_DAYS_HARD_CAP,
            );
        }
        if rec.max_bytes_per_session == 0 {
            bail!("terminal_recording.max_bytes_per_session must be greater than 0");
        }
        if rec.max_bytes_per_session > terminal_recording_defaults::MAX_BYTES_PER_SESSION_HARD_CAP {
            bail!(
                "terminal_recording.max_bytes_per_session = {got} exceeds the hard cap of {cap} \
                 bytes",
                got = rec.max_bytes_per_session,
                cap = terminal_recording_defaults::MAX_BYTES_PER_SESSION_HARD_CAP,
            );
        }
        if rec.chunk_hard_cap_bytes < terminal_recording_defaults::CHUNK_HARD_CAP_BYTES_FLOOR {
            bail!(
                "terminal_recording.chunk_hard_cap_bytes = {got} is below the floor of {floor} \
                 bytes; the cap MUST cover a 1 MiB live-wire frame plus AEAD envelope overhead",
                got = rec.chunk_hard_cap_bytes,
                floor = terminal_recording_defaults::CHUNK_HARD_CAP_BYTES_FLOOR,
            );
        }
        if rec.chunk_target_bytes == 0 {
            bail!("terminal_recording.chunk_target_bytes must be greater than 0");
        }
        if rec.chunk_target_bytes > rec.chunk_hard_cap_bytes {
            bail!(
                "terminal_recording.chunk_target_bytes = {target} must be <= \
                 chunk_hard_cap_bytes = {cap} (target is the soft flush size, cap is the row \
                 size ceiling)",
                target = rec.chunk_target_bytes,
                cap = rec.chunk_hard_cap_bytes,
            );
        }
        // `chunk_hard_cap_bytes` is u32 (max ~4 GiB); `max_bytes_per_session`
        // is u64. Compare in u64 to avoid spurious type mismatches.
        if rec.max_bytes_per_session < u64::from(rec.chunk_hard_cap_bytes) {
            bail!(
                "terminal_recording.max_bytes_per_session = {bytes} must be >= \
                 chunk_hard_cap_bytes = {cap}; otherwise a single legitimate chunk could \
                 exceed the per-session budget",
                bytes = rec.max_bytes_per_session,
                cap = rec.chunk_hard_cap_bytes,
            );
        }

        // Retention-cleanup bounds run REGARDLESS of `enabled`. The
        // cleanup worker (when implemented) MUST be allowed to run
        // even when recording is later turned off — disabling
        // recording must NOT make an existing recording corpus
        // immortal. `docs/terminal-recording.md` Section 12.6 spells
        // out the independence rule. We therefore inspect the cleanup
        // sub-struct here, BEFORE the `!rec.enabled` early return.
        Self::validate_terminal_recording_cleanup(&rec.cleanup)?;

        if !rec.enabled {
            // Stale key sources on a disabled recording config are not
            // a boot failure — operator may be staging future enable
            // and we should not punish them at validate time. Mirrors
            // the vault's "disabled drops sources without resolving"
            // policy.
            return Ok(());
        }

        // Encryption mode must match production posture. Dev permits
        // `disabled` (plaintext-at-rest) so a contributor exercising
        // recording locally without a key file does not have to mint
        // one; production refuses every shape that would persist
        // plaintext bytes.
        match rec.encryption.mode {
            TerminalRecordingEncryptionMode::Disabled => {
                if matches!(self.auth.mode, AuthMode::Production) {
                    bail!(
                        "terminal_recording.enabled = true with auth.mode = production requires \
                         terminal_recording.encryption.mode = required (production refuses to \
                         persist plaintext PTY bytes)"
                    );
                }
                // In dev mode + disabled encryption we still refuse
                // accidental key sources so the config is honest about
                // what it stores: a key set under `mode = disabled` is
                // either dead config (will never be read) or evidence
                // that the operator confused the modes.
                if rec.encryption.master_key_b64.is_some()
                    || rec.encryption.master_key_file.is_some()
                {
                    bail!(
                        "terminal_recording.encryption.mode = disabled but a master key source \
                         is configured; either set encryption.mode = required to consume the \
                         key, or unset the key sources"
                    );
                }
            }
            TerminalRecordingEncryptionMode::Required => {
                match (
                    rec.encryption.master_key_b64.is_some(),
                    rec.encryption.master_key_file.is_some(),
                ) {
                    (false, false) => bail!(
                        "terminal_recording.encryption.mode = required but no master key is \
                         configured — set terminal_recording.encryption.master_key_b64 or \
                         terminal_recording.encryption.master_key_file \
                         (RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_B64 / \
                         RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_FILE)"
                    ),
                    (true, true) => bail!(
                        "terminal_recording.encryption.master_key_b64 and \
                         terminal_recording.encryption.master_key_file are both set; pick \
                         exactly one"
                    ),
                    _ => {}
                }
            }
        }

        // Recording master key MUST be a different secret from the vault
        // master key. We can compare statically only when both configs
        // use the same source kind; mixed sources are out-of-scope for
        // this static check and need a runtime check after key load.
        if let (Some(rec_b64), Some(vault_b64)) = (
            rec.encryption.master_key_b64.as_deref(),
            self.vault.master_key_b64.as_deref(),
        ) && rec_b64 == vault_b64
        {
            bail!(
                "terminal_recording.encryption.master_key_b64 must not equal \
                 vault.master_key_b64; recording uses a SEPARATE key so a recording \
                 compromise does not leak SSH identities"
            );
        }
        if let (Some(rec_path), Some(vault_path)) = (
            rec.encryption.master_key_file.as_deref(),
            self.vault.master_key_file.as_deref(),
        ) && rec_path == vault_path
        {
            bail!(
                "terminal_recording.encryption.master_key_file must not equal \
                 vault.master_key_file; recording uses a SEPARATE key so a recording \
                 compromise does not leak SSH identities"
            );
        }

        Ok(())
    }

    /// Validate the retention-cleanup sub-config.
    ///
    /// Pure inspection: no filesystem reads, no key consumption, no
    /// effect on running state. Bounds match `terminal_recording_defaults`
    /// and the canonical contract in `docs/terminal-recording.md`
    /// Section 12.6 / 12.7.
    ///
    /// Independence rule (load-bearing): this validator runs regardless
    /// of `terminal_recording.enabled`. Cleanup MUST be allowed to run
    /// even when recording is disabled — turning recording off later
    /// must NOT make an existing recording corpus immortal.
    ///
    /// Cases:
    /// * `batch_size` — bounded `1..=10_000`. A zero-batch worker is a
    ///   config mistake (no progress per tick) and a > 10k value almost
    ///   certainly indicates a unit confusion.
    /// * `sweep_interval_seconds` — `0` is the sentinel "no periodic
    ///   schedule"; any non-zero value MUST sit in `60..=604800`.
    ///   Sub-60s cadence is a thundering-herd; > 7d defers retention
    ///   past the default 30-day window without operator intent.
    /// * `periodic_sweep_enabled = true` requires a non-zero,
    ///   in-bounds `sweep_interval_seconds`. The validator refuses
    ///   the contradictory `enabled-but-no-cadence` posture so a
    ///   future worker slice can trust its config.
    /// * `periodic_sweep_enabled = false` accepts either `0` (no
    ///   schedule, common) or a valid in-bounds value (operator
    ///   stages a future enable). Both are intentional shapes.
    /// * `enabled = false` and `startup_sweep_enabled = false` are
    ///   permitted regardless of the other fields — they are the
    ///   explicit opt-outs for an operator who manages retention
    ///   out-of-band.
    fn validate_terminal_recording_cleanup(
        cleanup: &TerminalRecordingCleanupConfig,
    ) -> anyhow::Result<()> {
        if cleanup.batch_size < terminal_recording_defaults::CLEANUP_BATCH_SIZE_MIN {
            bail!(
                "terminal_recording.cleanup.batch_size = {got} must be >= {min}; a zero-batch \
                 worker would never make progress",
                got = cleanup.batch_size,
                min = terminal_recording_defaults::CLEANUP_BATCH_SIZE_MIN,
            );
        }
        if cleanup.batch_size > terminal_recording_defaults::CLEANUP_BATCH_SIZE_MAX {
            bail!(
                "terminal_recording.cleanup.batch_size = {got} exceeds the hard cap of {max}; \
                 values above this almost always indicate a unit confusion",
                got = cleanup.batch_size,
                max = terminal_recording_defaults::CLEANUP_BATCH_SIZE_MAX,
            );
        }
        // Non-zero cadence must sit in the documented band. Zero is the
        // sentinel "no periodic schedule" and is always accepted unless
        // `periodic_sweep_enabled = true` (checked below).
        if cleanup.sweep_interval_seconds != 0
            && cleanup.sweep_interval_seconds
                < terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MIN
        {
            bail!(
                "terminal_recording.cleanup.sweep_interval_seconds = {got} must be 0 (disabled) \
                 or >= {min}s; sub-{min}s cadence is a thundering-herd against an empty corpus",
                got = cleanup.sweep_interval_seconds,
                min = terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MIN,
            );
        }
        if cleanup.sweep_interval_seconds
            > terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MAX
        {
            bail!(
                "terminal_recording.cleanup.sweep_interval_seconds = {got} exceeds the hard cap \
                 of {max}s (one week); values above this defer retention past the default \
                 retention window without operator intent",
                got = cleanup.sweep_interval_seconds,
                max = terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MAX,
            );
        }
        if cleanup.periodic_sweep_enabled && cleanup.sweep_interval_seconds == 0 {
            bail!(
                "terminal_recording.cleanup.periodic_sweep_enabled = true requires \
                 sweep_interval_seconds > 0 (set a cadence in {min}..={max} seconds, or flip \
                 periodic_sweep_enabled = false)",
                min = terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MIN,
                max = terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MAX,
            );
        }
        Ok(())
    }

    /// Validate the live-terminal-session orchestration config at boot.
    ///
    /// Pure inspection: no filesystem reads, no key consumption, no
    /// effect on running state. The detached-live-PTY TTL must be
    /// positive and sit inside the documented `5..=86_400` band — see
    /// [`terminal_sessions_defaults`] for the rationale on each bound.
    /// Error messages name the failing field and the rejected numeric
    /// value (the value is not secret-shaped) but never echo unrelated
    /// secret-bearing config fields.
    pub(crate) fn validate_terminal_sessions(&self) -> anyhow::Result<()> {
        let ttl = self.terminal_sessions.detached_live_pty_ttl_seconds;
        if ttl == 0 {
            bail!(
                "terminal_sessions.detached_live_pty_ttl_seconds must be greater than 0; a \
                 zero TTL would reap every detached PTY immediately, defeating the reconnect \
                 grace window"
            );
        }
        if ttl < terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MIN {
            bail!(
                "terminal_sessions.detached_live_pty_ttl_seconds = {got} must be >= {min}s; \
                 sub-{min}s windows are shorter than a typical reconnect round-trip",
                got = ttl,
                min = terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MIN,
            );
        }
        if ttl > terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MAX {
            bail!(
                "terminal_sessions.detached_live_pty_ttl_seconds = {got} exceeds the hard cap \
                 of {max}s (24h); values above this almost always indicate a unit confusion \
                 (minutes vs. seconds), and durable persistent sessions are a separate, \
                 future architecture",
                got = ttl,
                max = terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MAX,
            );
        }
        let max_live = self.terminal_sessions.max_live_pty_sessions_per_user;
        if max_live < terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MIN {
            bail!(
                "terminal_sessions.max_live_pty_sessions_per_user = {got} must be >= {min}; \
                 a zero cap would refuse every terminal-session create",
                got = max_live,
                min = terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MIN,
            );
        }
        if max_live > terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MAX {
            bail!(
                "terminal_sessions.max_live_pty_sessions_per_user = {got} exceeds the hard cap \
                 of {max}; per-user concurrent live PTYs above this are almost certainly a \
                 configuration mistake — each live PTY consumes a russh channel, a target-host \
                 PTY, an in-memory replay buffer, and one or more tasks",
                got = max_live,
                max = terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MAX,
            );
        }
        let max_starting = self.terminal_sessions.max_starting_sessions_per_user;
        if max_starting < terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MIN {
            bail!(
                "terminal_sessions.max_starting_sessions_per_user = {got} must be >= {min}; a \
                 zero cap would deadlock every terminal-session create",
                got = max_starting,
                min = terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MIN,
            );
        }
        if max_starting > terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MAX {
            bail!(
                "terminal_sessions.max_starting_sessions_per_user = {got} exceeds the hard cap \
                 of {max}; this is a defensive burst quota — values above {max} are well past \
                 any honest in-flight pattern",
                got = max_starting,
                max = terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MAX,
            );
        }
        let max_deployment = self.terminal_sessions.max_live_pty_sessions_per_deployment;
        if max_deployment < terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MIN {
            bail!(
                "terminal_sessions.max_live_pty_sessions_per_deployment = {got} must be >= {min}; \
                 a zero deployment ceiling would refuse every terminal-session create",
                got = max_deployment,
                min = terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MIN,
            );
        }
        if max_deployment > terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MAX {
            bail!(
                "terminal_sessions.max_live_pty_sessions_per_deployment = {got} exceeds the hard \
                 cap of {max}; deployment ceilings above {max} are past the kernel-side FD \
                 ceiling on most single-host deployments and are almost certainly a configuration \
                 mistake",
                got = max_deployment,
                max = terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MAX,
            );
        }
        // Cross-field bounds (`docs/session-quotas.md` § 5.2). The
        // deployment-wide ceiling MUST sit at or above every per-user
        // ceiling — a per-user cap above the global cap is a
        // contradiction the enforcement layer cannot resolve sensibly.
        // Each error names both fields explicitly so the operator can
        // fix the right one.
        if max_deployment < max_live {
            bail!(
                "terminal_sessions.max_live_pty_sessions_per_deployment = {dep} must be >= \
                 terminal_sessions.max_live_pty_sessions_per_user = {user}; a per-user live \
                 ceiling above the deployment ceiling is a contradiction (every user would be \
                 capped by the deployment value before reaching their personal cap)",
                dep = max_deployment,
                user = max_live,
            );
        }
        if max_deployment < max_starting {
            bail!(
                "terminal_sessions.max_live_pty_sessions_per_deployment = {dep} must be >= \
                 terminal_sessions.max_starting_sessions_per_user = {user}; a starting-burst cap \
                 above the deployment ceiling would let one user's burst exhaust the deployment \
                 slot before any session promotes to live",
                dep = max_deployment,
                user = max_starting,
            );
        }
        Ok(())
    }

    /// Detached-live-PTY TTL as a `Duration`. Production callers pass
    /// the result into [`relayterm_terminal::TerminalSessionManager::with_detach_ttl`].
    /// Assumes [`Self::validate_terminal_sessions`] has already passed —
    /// the value is post-validation, in-range, and a `u64`-as-seconds
    /// conversion never overflows for any value the validator accepts.
    pub(crate) fn detached_live_pty_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.terminal_sessions.detached_live_pty_ttl_seconds)
    }

    /// Per-user live PTY ceiling (Phase 1B.1 quota). Production callers
    /// pass this into [`relayterm_terminal::TerminalSessionManager::with_max_live_pty_per_user`].
    /// Assumes [`Self::validate_terminal_sessions`] has already passed
    /// — the value is post-validation, in-range, and a positive `u32`.
    pub(crate) fn max_live_pty_sessions_per_user(&self) -> u32 {
        self.terminal_sessions.max_live_pty_sessions_per_user
    }

    /// Per-user starting-burst ceiling (Phase 1B.2a quota). Production
    /// callers pass this into
    /// [`relayterm_terminal::TerminalSessionManager::with_max_starting_per_user`].
    /// Assumes [`Self::validate_terminal_sessions`] has already passed
    /// — the value is post-validation, in-range, and a positive `u32`.
    pub(crate) fn max_starting_sessions_per_user(&self) -> u32 {
        self.terminal_sessions.max_starting_sessions_per_user
    }

    /// Deployment-wide live PTY ceiling (Phase 1B.2b quota). Production
    /// callers pass this into
    /// [`relayterm_terminal::TerminalSessionManager::with_max_live_pty_per_deployment`].
    /// Assumes [`Self::validate_terminal_sessions`] has already passed
    /// — the value is post-validation, in-range, and a positive `u32`.
    pub(crate) fn max_live_pty_sessions_per_deployment(&self) -> u32 {
        self.terminal_sessions.max_live_pty_sessions_per_deployment
    }

    /// Resolve the configured master key, or return `None` when the vault
    /// is intentionally disabled. Consumes the configured key sources so
    /// the raw base64 string does not linger on the heap for the process
    /// lifetime — `take()`d into a `Zeroizing<String>` that wipes itself
    /// when this function returns.
    ///
    /// Failure modes (each returns a descriptive error that does NOT echo
    /// the key value):
    /// * vault enabled and neither source set → "no master key configured"
    /// * vault enabled and both sources set → "ambiguous master key"
    /// * source set but unreadable / invalid → wraps the structural reason
    pub(crate) fn vault_master_key(&mut self) -> anyhow::Result<Option<VaultMasterKey>> {
        if !self.vault.enabled {
            // Drop any configured sources up front so a disabled vault
            // never keeps the raw key on the heap.
            let _ = self.vault.master_key_b64.take().map(Zeroizing::new);
            self.vault.master_key_file.take();
            return Ok(None);
        }
        let b64 = self.vault.master_key_b64.take().map(Zeroizing::new);
        let path = self.vault.master_key_file.take();
        match (b64, path) {
            (Some(_), Some(_)) => bail!(
                "vault.master_key_b64 and vault.master_key_file are both set; pick exactly one"
            ),
            (Some(b64), None) => VaultMasterKey::from_base64(&b64)
                .map(Some)
                .map_err(|e| anyhow!("vault.master_key_b64 invalid: {e}")),
            (None, Some(path)) => VaultMasterKey::from_file(&path)
                .map(Some)
                .with_context(|| format!("vault.master_key_file at {}", path.display())),
            (None, None) => bail!(
                "vault.enabled = true but no master key configured (set vault.master_key_b64 \
                 or vault.master_key_file, or flip vault.enabled = false to opt out)"
            ),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    server: Option<FileServerConfig>,
    database: Option<FileDatabaseConfig>,
    auth: Option<FileAuthConfig>,
    vault: Option<FileVaultConfig>,
    terminal_recording: Option<FileTerminalRecordingConfig>,
    terminal_sessions: Option<FileTerminalSessionsConfig>,
}

#[derive(Debug, Deserialize)]
struct FileServerConfig {
    bind: Option<SocketAddr>,
}

#[derive(Debug, Deserialize)]
struct FileDatabaseConfig {
    url: Option<String>,
    max_connections: Option<u32>,
}

/// File-side mirror of [`AuthConfig`]. `Debug` is implemented manually
/// (NOT derived) so the secret-shaped fields never reach a log line —
/// only their presence is rendered. Mirrors the redaction discipline on
/// the merged [`AuthConfig`]; without it the deserialized intermediate
/// would silently re-introduce the leak. An unrecognized `mode` value in
/// TOML is rejected by serde at deserialize time (see the
/// `#[serde(rename_all = "lowercase")]` on [`AuthMode`]).
#[derive(Deserialize)]
struct FileAuthConfig {
    mode: Option<AuthMode>,
    session_signing_key_b64: Option<String>,
    session_signing_key_file: Option<std::path::PathBuf>,
    first_user_bootstrap_token: Option<String>,
    cookie_secure: Option<bool>,
    cookie_domain: Option<String>,
    allowed_origins: Option<Vec<String>>,
}

impl fmt::Debug for FileAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileAuthConfig")
            .field("mode", &self.mode)
            .field(
                "session_signing_key_b64_set",
                &self.session_signing_key_b64.is_some(),
            )
            .field("session_signing_key_file", &self.session_signing_key_file)
            .field(
                "first_user_bootstrap_token_set",
                &self.first_user_bootstrap_token.is_some(),
            )
            .field("cookie_secure", &self.cookie_secure)
            .field("cookie_domain", &self.cookie_domain)
            .field("allowed_origins", &self.allowed_origins)
            .finish()
    }
}

/// File-side mirror of [`VaultConfig`]. `Debug` is implemented manually
/// (NOT derived) so the raw base64 master key never reaches a log line —
/// only its presence is rendered. Mirrors the redaction discipline on the
/// merged [`VaultConfig`]; without it the deserialized intermediate would
/// silently re-introduce the leak.
#[derive(Deserialize)]
struct FileVaultConfig {
    enabled: Option<bool>,
    master_key_b64: Option<String>,
    master_key_file: Option<std::path::PathBuf>,
}

impl fmt::Debug for FileVaultConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileVaultConfig")
            .field("enabled", &self.enabled)
            .field("master_key_b64_set", &self.master_key_b64.is_some())
            .field("master_key_file", &self.master_key_file)
            .finish()
    }
}

/// File-side mirror of [`TerminalRecordingConfig`]. Same redaction
/// discipline as [`FileVaultConfig`] / [`FileAuthConfig`]: `Debug`
/// renders only the *presence* of each key source, never the value.
#[derive(Default, Deserialize)]
struct FileTerminalRecordingConfig {
    enabled: Option<bool>,
    retention_days: Option<u32>,
    max_bytes_per_session: Option<u64>,
    chunk_target_bytes: Option<u32>,
    chunk_hard_cap_bytes: Option<u32>,
    encryption: Option<FileTerminalRecordingEncryptionConfig>,
    compression: Option<FileTerminalRecordingCompressionConfig>,
    cleanup: Option<FileTerminalRecordingCleanupConfig>,
}

impl fmt::Debug for FileTerminalRecordingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileTerminalRecordingConfig")
            .field("enabled", &self.enabled)
            .field("retention_days", &self.retention_days)
            .field("max_bytes_per_session", &self.max_bytes_per_session)
            .field("chunk_target_bytes", &self.chunk_target_bytes)
            .field("chunk_hard_cap_bytes", &self.chunk_hard_cap_bytes)
            .field("encryption", &self.encryption)
            .field("compression", &self.compression)
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

/// File-side mirror of [`TerminalRecordingCleanupConfig`]. Carries no
/// secret material — boolean flags and small numeric bounds — so a
/// derived `Debug` is fine. Each field is `Option` so the merge step
/// only overrides fields the operator explicitly set, exactly like the
/// other file-side mirrors.
#[derive(Debug, Default, Deserialize)]
struct FileTerminalRecordingCleanupConfig {
    enabled: Option<bool>,
    startup_sweep_enabled: Option<bool>,
    periodic_sweep_enabled: Option<bool>,
    sweep_interval_seconds: Option<u64>,
    batch_size: Option<u32>,
}

#[derive(Default, Deserialize)]
struct FileTerminalRecordingEncryptionConfig {
    mode: Option<TerminalRecordingEncryptionMode>,
    master_key_b64: Option<String>,
    master_key_file: Option<std::path::PathBuf>,
}

impl fmt::Debug for FileTerminalRecordingEncryptionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileTerminalRecordingEncryptionConfig")
            .field("mode", &self.mode)
            .field("master_key_b64_set", &self.master_key_b64.is_some())
            .field("master_key_file", &self.master_key_file)
            .finish()
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileTerminalRecordingCompressionConfig {
    mode: Option<TerminalRecordingCompressionMode>,
}

/// File-side mirror of [`TerminalSessionsConfig`]. No secret material —
/// a derived `Debug` is fine. Each field is `Option` so the merge step
/// only overrides what the operator explicitly set.
#[derive(Debug, Default, Deserialize)]
struct FileTerminalSessionsConfig {
    detached_live_pty_ttl_seconds: Option<u64>,
    max_live_pty_sessions_per_user: Option<u32>,
    max_starting_sessions_per_user: Option<u32>,
    max_live_pty_sessions_per_deployment: Option<u32>,
}

impl FileConfig {
    fn merge_into(self, cfg: &mut Config) {
        if let Some(s) = self.server
            && let Some(bind) = s.bind
        {
            cfg.server.bind = bind;
        }
        if let Some(d) = self.database {
            if let Some(url) = d.url {
                cfg.database.url = url;
            }
            if let Some(mx) = d.max_connections {
                cfg.database.max_connections = mx;
            }
        }
        if let Some(a) = self.auth {
            if let Some(mode) = a.mode {
                cfg.auth.mode = mode;
            }
            if let Some(b64) = a.session_signing_key_b64 {
                cfg.auth.session_signing_key_b64 = Some(b64);
            }
            if let Some(p) = a.session_signing_key_file {
                cfg.auth.session_signing_key_file = Some(p);
            }
            if let Some(t) = a.first_user_bootstrap_token {
                cfg.auth.first_user_bootstrap_token = Some(t);
            }
            if let Some(s) = a.cookie_secure {
                cfg.auth.cookie_secure = s;
            }
            if let Some(d) = a.cookie_domain {
                cfg.auth.cookie_domain = Some(d);
            }
            if let Some(o) = a.allowed_origins {
                cfg.auth.allowed_origins = o;
            }
        }
        if let Some(v) = self.vault {
            if let Some(enabled) = v.enabled {
                cfg.vault.enabled = enabled;
            }
            if let Some(b64) = v.master_key_b64 {
                cfg.vault.master_key_b64 = Some(b64);
            }
            if let Some(p) = v.master_key_file {
                cfg.vault.master_key_file = Some(p);
            }
        }
        if let Some(r) = self.terminal_recording {
            if let Some(enabled) = r.enabled {
                cfg.terminal_recording.enabled = enabled;
            }
            if let Some(d) = r.retention_days {
                cfg.terminal_recording.retention_days = d;
            }
            if let Some(b) = r.max_bytes_per_session {
                cfg.terminal_recording.max_bytes_per_session = b;
            }
            if let Some(t) = r.chunk_target_bytes {
                cfg.terminal_recording.chunk_target_bytes = t;
            }
            if let Some(c) = r.chunk_hard_cap_bytes {
                cfg.terminal_recording.chunk_hard_cap_bytes = c;
            }
            if let Some(enc) = r.encryption {
                if let Some(m) = enc.mode {
                    cfg.terminal_recording.encryption.mode = m;
                }
                if let Some(b64) = enc.master_key_b64 {
                    cfg.terminal_recording.encryption.master_key_b64 = Some(b64);
                }
                if let Some(p) = enc.master_key_file {
                    cfg.terminal_recording.encryption.master_key_file = Some(p);
                }
            }
            if let Some(comp) = r.compression
                && let Some(m) = comp.mode
            {
                cfg.terminal_recording.compression.mode = m;
            }
            if let Some(c) = r.cleanup {
                if let Some(enabled) = c.enabled {
                    cfg.terminal_recording.cleanup.enabled = enabled;
                }
                if let Some(s) = c.startup_sweep_enabled {
                    cfg.terminal_recording.cleanup.startup_sweep_enabled = s;
                }
                if let Some(p) = c.periodic_sweep_enabled {
                    cfg.terminal_recording.cleanup.periodic_sweep_enabled = p;
                }
                if let Some(i) = c.sweep_interval_seconds {
                    cfg.terminal_recording.cleanup.sweep_interval_seconds = i;
                }
                if let Some(b) = c.batch_size {
                    cfg.terminal_recording.cleanup.batch_size = b;
                }
            }
        }
        if let Some(s) = self.terminal_sessions {
            if let Some(ttl) = s.detached_live_pty_ttl_seconds {
                cfg.terminal_sessions.detached_live_pty_ttl_seconds = ttl;
            }
            if let Some(cap) = s.max_live_pty_sessions_per_user {
                cfg.terminal_sessions.max_live_pty_sessions_per_user = cap;
            }
            if let Some(cap) = s.max_starting_sessions_per_user {
                cfg.terminal_sessions.max_starting_sessions_per_user = cap;
            }
            if let Some(cap) = s.max_live_pty_sessions_per_deployment {
                cfg.terminal_sessions.max_live_pty_sessions_per_deployment = cap;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    fn empty_cfg() -> Config {
        Config {
            server: ServerConfig {
                bind: "127.0.0.1:8080".parse().unwrap(),
            },
            database: DatabaseConfig {
                url: "x".to_owned(),
                max_connections: 1,
            },
            auth: AuthConfig {
                mode: AuthMode::Dev,
                session_signing_key_b64: None,
                session_signing_key_file: None,
                first_user_bootstrap_token: None,
                cookie_secure: true,
                cookie_domain: None,
                allowed_origins: Vec::new(),
            },
            vault: VaultConfig {
                enabled: true,
                master_key_b64: None,
                master_key_file: None,
            },
            terminal_recording: TerminalRecordingConfig {
                enabled: false,
                retention_days: terminal_recording_defaults::RETENTION_DAYS,
                max_bytes_per_session: terminal_recording_defaults::MAX_BYTES_PER_SESSION,
                chunk_target_bytes: terminal_recording_defaults::CHUNK_TARGET_BYTES,
                chunk_hard_cap_bytes: terminal_recording_defaults::CHUNK_HARD_CAP_BYTES,
                encryption: TerminalRecordingEncryptionConfig {
                    mode: TerminalRecordingEncryptionMode::Disabled,
                    master_key_b64: None,
                    master_key_file: None,
                },
                compression: TerminalRecordingCompressionConfig::default(),
                cleanup: TerminalRecordingCleanupConfig {
                    enabled: terminal_recording_defaults::CLEANUP_ENABLED,
                    startup_sweep_enabled:
                        terminal_recording_defaults::CLEANUP_STARTUP_SWEEP_ENABLED,
                    periodic_sweep_enabled:
                        terminal_recording_defaults::CLEANUP_PERIODIC_SWEEP_ENABLED,
                    sweep_interval_seconds:
                        terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS,
                    batch_size: terminal_recording_defaults::CLEANUP_BATCH_SIZE,
                },
            },
            terminal_sessions: TerminalSessionsConfig {
                detached_live_pty_ttl_seconds:
                    terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS,
                max_live_pty_sessions_per_user:
                    terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER,
                max_starting_sessions_per_user:
                    terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER,
                max_live_pty_sessions_per_deployment:
                    terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT,
            },
        }
    }

    /// Production-shaped config that satisfies every `validate_auth`
    /// requirement. Tests build on this and mutate the field that the
    /// case under test cares about.
    fn production_cfg() -> Config {
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production;
        cfg.auth.session_signing_key_b64 = Some(BASE64_STANDARD.encode([0x42u8; 32]));
        cfg.auth.cookie_secure = true;
        cfg.auth.allowed_origins = vec!["https://relay.example.com".to_owned()];
        cfg
    }

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn vault_config_debug_redacts_master_key_b64() {
        let cfg = VaultConfig {
            enabled: true,
            master_key_b64: Some("AAAA-MASTER-KEY-MARKER-AAAA".to_owned()),
            master_key_file: None,
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains("AAAA-MASTER-KEY-MARKER-AAAA"),
            "VaultConfig debug must not echo master_key_b64: {s}"
        );
        assert!(s.contains("master_key_b64_set: true"));
    }

    #[test]
    fn file_vault_config_debug_redacts_master_key_b64() {
        let fv = FileVaultConfig {
            enabled: Some(true),
            master_key_b64: Some("AAAA-FILE-MASTER-KEY-MARKER-AAAA".to_owned()),
            master_key_file: None,
        };
        let s = format!("{fv:?}");
        assert!(
            !s.contains("AAAA-FILE-MASTER-KEY-MARKER-AAAA"),
            "FileVaultConfig debug must not echo master_key_b64: {s}"
        );
        assert!(s.contains("master_key_b64_set: true"));
    }

    #[test]
    fn vault_master_key_consumes_b64_source_on_success() {
        let mut cfg = empty_cfg();
        cfg.vault.master_key_b64 = Some(BASE64_STANDARD.encode([0x42u8; 32]));

        let key = cfg.vault_master_key().unwrap();
        assert!(key.is_some(), "valid b64 key should resolve");
        assert!(
            cfg.vault.master_key_b64.is_none(),
            "raw b64 source must be wiped from Config after key resolution"
        );
        assert!(cfg.vault.master_key_file.is_none());
    }

    #[test]
    fn vault_master_key_consumes_b64_source_on_failure() {
        // Even on a decode failure the source must not linger — `take`
        // happens unconditionally so the heap copy gets dropped.
        let mut cfg = empty_cfg();
        cfg.vault.master_key_b64 = Some("not-valid-base64-!@#$".to_owned());

        let err = cfg.vault_master_key().unwrap_err();
        assert!(
            err.to_string().contains("vault.master_key_b64 invalid"),
            "error should name the failing source: {err}"
        );
        assert!(
            cfg.vault.master_key_b64.is_none(),
            "failed b64 source must still be wiped from Config"
        );
    }

    #[test]
    fn vault_master_key_disabled_drops_sources_without_resolving() {
        let mut cfg = empty_cfg();
        cfg.vault.enabled = false;
        cfg.vault.master_key_b64 = Some(BASE64_STANDARD.encode([0u8; 32]));
        cfg.vault.master_key_file = Some(std::path::PathBuf::from("/dev/null"));

        let key = cfg.vault_master_key().unwrap();
        assert!(key.is_none());
        assert!(cfg.vault.master_key_b64.is_none());
        assert!(cfg.vault.master_key_file.is_none());
    }

    #[test]
    fn vault_master_key_rejects_both_sources_set() {
        let mut cfg = empty_cfg();
        cfg.vault.master_key_b64 = Some(BASE64_STANDARD.encode([0u8; 32]));
        cfg.vault.master_key_file = Some(std::path::PathBuf::from("/dev/null"));

        let err = cfg.vault_master_key().unwrap_err();
        assert!(
            err.to_string()
                .contains("master_key_b64 and vault.master_key_file are both set"),
            "error should describe ambiguity: {err}"
        );
    }

    #[test]
    fn vault_master_key_requires_a_source_when_enabled() {
        let mut cfg = empty_cfg();
        let err = cfg.vault_master_key().unwrap_err();
        assert!(
            err.to_string().contains("no master key configured"),
            "error should explain missing key: {err}"
        );
    }

    #[test]
    fn database_url_debug_redacts_password() {
        let db = DatabaseConfig {
            url: "postgres://relayterm:s3cret-passw0rd@db.internal:5432/relayterm".to_owned(),
            max_connections: 10,
        };
        let s = format!("{db:?}");
        assert!(
            !s.contains("s3cret-passw0rd"),
            "database url debug must not contain the password: {s}"
        );
        // Diagnostically useful pieces should survive: host, port, db name,
        // username, and the masking marker so it's clear redaction happened.
        assert!(s.contains("relayterm:***"));
        assert!(s.contains("db.internal:5432/relayterm"));
    }

    #[test]
    fn database_url_redaction_passthrough_on_passwordless_url() {
        // A URL without userinfo or a password segment should round-trip
        // unchanged so operators can still see the bind detail.
        assert_eq!(
            redact_database_url("postgres://db.internal:5432/relayterm"),
            "postgres://db.internal:5432/relayterm"
        );
        assert_eq!(
            redact_database_url("postgres://relayterm@db.internal:5432/relayterm"),
            "postgres://relayterm@db.internal:5432/relayterm"
        );
    }

    #[test]
    fn database_url_redaction_handles_non_url_strings() {
        // If the value isn't a `scheme://...` form, leave it alone — the
        // mask is a safety net, not a parser.
        assert_eq!(redact_database_url("not-a-url"), "not-a-url");
    }

    // --- Auth config + validation -----------------------------------

    #[test]
    fn auth_config_default_resolves_to_dev_mode() {
        let cfg = Config::defaults();
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        cfg.validate_auth().expect("default config must validate");
    }

    #[test]
    fn auth_mode_from_env_dev_validates() {
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Production; // ensure env genuinely overrides
        Config::apply_env_with(&mut cfg, env_from(&[("RELAYTERM_AUTH__MODE", "dev")])).unwrap();
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        cfg.validate_auth().expect("dev mode must validate");
    }

    #[test]
    fn auth_mode_dev_with_loose_settings_validates() {
        // Dev mode is the relaxed envelope: insecure cookies, empty
        // allow-list, missing signing key are all acceptable. The same
        // real-auth code path runs as in production; only the boot
        // validation differs.
        let mut cfg = empty_cfg();
        cfg.auth.mode = AuthMode::Dev;
        cfg.auth.cookie_secure = false;
        cfg.auth.allowed_origins = Vec::new();
        cfg.auth.session_signing_key_b64 = None;
        cfg.auth.session_signing_key_file = None;
        cfg.validate_auth().expect("dev mode must validate");
    }

    #[test]
    fn auth_mode_production_with_valid_config_validates() {
        // The canonical "production deploy" shape: signing key set,
        // non-empty allow-list, Secure cookies. Today the signing key
        // is reserved (the v1 session model uses opaque random tokens),
        // but its presence is required.
        let cfg = production_cfg();
        cfg.validate_auth()
            .expect("production with full config must validate");
    }

    #[test]
    fn auth_mode_production_missing_signing_key_fails_fast() {
        let mut cfg = production_cfg();
        cfg.auth.session_signing_key_b64 = None;
        cfg.auth.session_signing_key_file = None;
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("session signing key"),
            "error must name the missing signing key: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_both_signing_key_sources_set_is_ambiguous() {
        let mut cfg = production_cfg();
        cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from("/dev/null"));
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("both") || msg.contains("pick exactly one"),
            "error must describe ambiguity: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_empty_allowed_origins_fails_fast() {
        let mut cfg = production_cfg();
        cfg.auth.allowed_origins = Vec::new();
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("allowed_origins"),
            "error must name allowed_origins: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_cookie_secure_false_fails_fast() {
        let mut cfg = production_cfg();
        cfg.auth.cookie_secure = false;
        let err = cfg.validate_auth().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cookie_secure"),
            "error must name cookie_secure: {msg}"
        );
        assert!(
            msg.contains("Secure"),
            "error must mention the Secure flag: {msg}"
        );
    }

    #[test]
    fn auth_mode_production_with_signing_key_file_only_validates() {
        // The b64 source is the alternate; either branch satisfies the
        // "exactly one" requirement.
        let mut cfg = production_cfg();
        cfg.auth.session_signing_key_b64 = None;
        cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from("/etc/relayterm/key"));
        cfg.validate_auth()
            .expect("production with key file only must validate");
    }

    #[test]
    fn auth_mode_production_with_optional_bootstrap_token_validates() {
        // The bootstrap token is OPTIONAL at the config-validation
        // layer; the "no first user + no token" hard-fail is a runtime
        // check in main.rs (it needs DB state). validate_auth must not
        // refuse a production config solely because the token is unset.
        let mut cfg = production_cfg();
        cfg.auth.first_user_bootstrap_token = None;
        cfg.validate_auth()
            .expect("production without bootstrap token must validate at config layer");
    }

    #[test]
    fn dev_auth_env_var_is_silently_ignored() {
        // The legacy `RELAYTERM_DEV_AUTH__ENABLED` knob no longer maps
        // to any field on `Config`. An operator who still has it set
        // should not see a config-load failure; dropping the field
        // makes the env var a no-op. Pin this so a future "let's
        // re-introduce a dev-shim toggle" PR has to delete this test
        // explicitly.
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_DEV_AUTH__ENABLED", "true")]),
        )
        .expect("legacy dev_auth env var is silently ignored");
        cfg.validate_auth()
            .expect("validation unaffected by legacy var");
    }

    #[test]
    fn legacy_dev_auth_toml_section_is_silently_ignored() {
        // Same policy at the TOML layer — an operator's stale
        // `[dev_auth]` block must not block a config load. Default
        // serde rejects unknown fields only when annotated; the
        // `FileConfig` derive does not opt in, so this is a behavior
        // test, not a structural one.
        let raw = r#"
[dev_auth]
enabled = true

[auth]
mode = "dev"
"#;
        let parsed: FileConfig = toml::from_str(raw).expect("legacy dev_auth section ignored");
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
    }

    #[test]
    fn auth_mode_invalid_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_AUTH__MODE", "totally-bogus")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_AUTH__MODE"),
            "error must name the failing input: {msg}"
        );
        // The bogus value MAY appear in the error (it's not a secret), but
        // the parse must reject the whole apply_env call so the cfg is not
        // half-mutated past this point.
    }

    #[test]
    fn auth_mode_invalid_toml_value_fails_safely() {
        let raw = "[auth]\nmode = \"halfway\"\n";
        let err = toml::from_str::<FileConfig>(raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown variant") && msg.contains("halfway"),
            "TOML parse error must name the unknown variant: {msg}"
        );
        assert!(
            msg.contains("dev") && msg.contains("production"),
            "TOML parse error must list the accepted variants: {msg}"
        );
    }

    #[test]
    fn auth_config_debug_redacts_session_signing_key_b64() {
        let cfg = AuthConfig {
            mode: AuthMode::Dev,
            session_signing_key_b64: Some("AAAA-SIGNING-KEY-MARKER-AAAA".to_owned()),
            session_signing_key_file: None,
            first_user_bootstrap_token: None,
            cookie_secure: true,
            cookie_domain: None,
            allowed_origins: Vec::new(),
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains("AAAA-SIGNING-KEY-MARKER-AAAA"),
            "AuthConfig debug must not echo session_signing_key_b64: {s}"
        );
        assert!(s.contains("session_signing_key_b64_set: true"));
    }

    #[test]
    fn auth_config_debug_redacts_first_user_bootstrap_token() {
        let cfg = AuthConfig {
            mode: AuthMode::Dev,
            session_signing_key_b64: None,
            session_signing_key_file: None,
            first_user_bootstrap_token: Some("AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA".to_owned()),
            cookie_secure: true,
            cookie_domain: None,
            allowed_origins: Vec::new(),
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains("AAAA-BOOTSTRAP-TOKEN-MARKER-AAAA"),
            "AuthConfig debug must not echo first_user_bootstrap_token: {s}"
        );
        assert!(s.contains("first_user_bootstrap_token_set: true"));
    }

    #[test]
    fn file_auth_config_debug_redacts_secrets() {
        let fa = FileAuthConfig {
            mode: Some(AuthMode::Production),
            session_signing_key_b64: Some("AAAA-FILE-SIGNING-KEY-MARKER-AAAA".to_owned()),
            session_signing_key_file: None,
            first_user_bootstrap_token: Some("AAAA-FILE-BOOTSTRAP-MARKER-AAAA".to_owned()),
            cookie_secure: Some(true),
            cookie_domain: None,
            allowed_origins: Some(vec!["https://relay.example.com".to_owned()]),
        };
        let s = format!("{fa:?}");
        assert!(
            !s.contains("AAAA-FILE-SIGNING-KEY-MARKER-AAAA"),
            "FileAuthConfig debug must not echo session_signing_key_b64: {s}"
        );
        assert!(
            !s.contains("AAAA-FILE-BOOTSTRAP-MARKER-AAAA"),
            "FileAuthConfig debug must not echo first_user_bootstrap_token: {s}"
        );
        assert!(s.contains("session_signing_key_b64_set: true"));
        assert!(s.contains("first_user_bootstrap_token_set: true"));
    }

    #[test]
    fn auth_validation_errors_do_not_echo_secret_env_values() {
        // A secret-shaped value supplied via env must not survive into the
        // validation error string. We pin the policy here so a future edit
        // that starts substituting the value into the error gets caught.
        // Drive every reachable production-mode failure path: missing
        // signing key, ambiguous signing key, empty allow-list, and
        // cookie_secure=false. Each case primes the secret-shaped fields
        // so the assertion is meaningful regardless of which check fires.
        const SECRET_MARKER: &str = "AAAA-SECRET-IN-ERROR-MARKER-AAAA";

        // Missing signing key — the bootstrap token is set but never
        // consulted by the validator on this path; the assertion proves
        // the validator does not opportunistically interpolate it.
        {
            let mut cfg = production_cfg();
            cfg.auth.session_signing_key_b64 = None;
            cfg.auth.session_signing_key_file = None;
            cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "missing-key error must not echo secret-shaped values: {err}"
            );
        }

        // Ambiguous signing key — both b64 and file are set; the b64
        // value is sentinel-shaped.
        {
            let mut cfg = production_cfg();
            cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
            cfg.auth.session_signing_key_file = Some(std::path::PathBuf::from("/dev/null"));
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "ambiguous-key error must not echo secret-shaped values: {err}"
            );
        }

        // Empty allow-list — the bootstrap token + signing key are set
        // but the validator must still not echo them.
        {
            let mut cfg = production_cfg();
            cfg.auth.allowed_origins = Vec::new();
            cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
            cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "empty-allowed-origins error must not echo secret-shaped values: {err}"
            );
        }

        // cookie_secure = false — same redaction posture.
        {
            let mut cfg = production_cfg();
            cfg.auth.cookie_secure = false;
            cfg.auth.session_signing_key_b64 = Some(SECRET_MARKER.to_owned());
            cfg.auth.first_user_bootstrap_token = Some(SECRET_MARKER.to_owned());
            let err = cfg.validate_auth().unwrap_err();
            assert!(
                !err.to_string().contains(SECRET_MARKER),
                "cookie_secure error must not echo secret-shaped values: {err}"
            );
        }
    }

    #[test]
    fn auth_env_parses_allowed_origins_csv() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_AUTH__ALLOWED_ORIGINS",
                "https://a.example.com, https://b.example.com,",
            )]),
        )
        .unwrap();
        assert_eq!(
            cfg.auth.allowed_origins,
            vec![
                "https://a.example.com".to_owned(),
                "https://b.example.com".to_owned()
            ]
        );
    }

    #[test]
    fn auth_env_parses_secret_fields_into_options() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[
                ("RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64", "k"),
                ("RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN", "t"),
                ("RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE", "/dev/null"),
                ("RELAYTERM_AUTH__COOKIE_DOMAIN", "relay.example.com"),
                ("RELAYTERM_AUTH__COOKIE_SECURE", "false"),
            ]),
        )
        .unwrap();
        assert!(cfg.auth.session_signing_key_b64.is_some());
        assert!(cfg.auth.first_user_bootstrap_token.is_some());
        assert_eq!(
            cfg.auth.session_signing_key_file.as_deref(),
            Some(std::path::Path::new("/dev/null"))
        );
        assert_eq!(cfg.auth.cookie_domain.as_deref(), Some("relay.example.com"));
        assert!(!cfg.auth.cookie_secure);
    }

    #[test]
    fn auth_config_toml_round_trip() {
        let raw = r#"
[auth]
mode = "dev"
cookie_secure = false
cookie_domain = "relay.example.com"
allowed_origins = ["https://relay.example.com"]
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.auth.mode, AuthMode::Dev);
        assert!(!cfg.auth.cookie_secure);
        assert_eq!(cfg.auth.cookie_domain.as_deref(), Some("relay.example.com"));
        assert_eq!(
            cfg.auth.allowed_origins,
            vec!["https://relay.example.com".to_owned()]
        );
    }

    // --- Terminal recording config -----------------------------------

    /// Sentinel string injected into recording-secret-shaped fields so a
    /// future regression that starts substituting key values into errors
    /// or `Debug` output gets caught the same way the auth tests catch
    /// the matching auth surface.
    const RECORDING_SECRET_MARKER: &str = "AAAA-RECORDING-SECRET-MARKER-AAAA";

    fn b64_32() -> String {
        BASE64_STANDARD.encode([0x42u8; 32])
    }

    /// Production + recording-enabled fixture that satisfies every
    /// `validate_terminal_recording` requirement. Tests build on this
    /// and mutate the field they care about, mirroring `production_cfg`.
    fn production_recording_cfg() -> Config {
        let mut cfg = production_cfg();
        cfg.terminal_recording.enabled = true;
        cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::Required;
        cfg.terminal_recording.encryption.master_key_b64 = Some(b64_32());
        cfg
    }

    #[test]
    fn terminal_recording_default_is_disabled_and_validates() {
        let cfg = Config::defaults();
        assert!(
            !cfg.terminal_recording.enabled,
            "recording must default to disabled to keep step 1b a no-op for existing operators"
        );
        cfg.validate_terminal_recording()
            .expect("default recording config must validate");
    }

    #[test]
    fn terminal_recording_disabled_config_validates_without_key() {
        // Disabled is the default and must be a config-only no-op.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.enabled = false;
        cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::Disabled;
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_dev_enabled_with_disabled_encryption_validates() {
        // A contributor exercising recording locally without minting a
        // key must be able to boot. Production is the strict envelope;
        // dev relaxes the at-rest posture, same shape as auth.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.enabled = true;
        cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::Disabled;
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_production_enabled_without_key_fails() {
        let mut cfg = production_cfg();
        cfg.terminal_recording.enabled = true;
        // mode left at default `disabled` — production must refuse
        // because a plaintext-at-rest posture is not a deployable
        // production configuration.
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("auth.mode = production") || msg.contains("encryption.mode = required"),
            "error must point at the production posture: {msg}"
        );
    }

    #[test]
    fn terminal_recording_production_required_without_key_source_fails() {
        let mut cfg = production_recording_cfg();
        cfg.terminal_recording.encryption.master_key_b64 = None;
        cfg.terminal_recording.encryption.master_key_file = None;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("master key"),
            "error must name the missing key: {msg}"
        );
    }

    #[test]
    fn terminal_recording_required_with_both_key_sources_fails() {
        let mut cfg = production_recording_cfg();
        cfg.terminal_recording.encryption.master_key_file =
            Some(std::path::PathBuf::from("/dev/null"));
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("both") || msg.contains("pick exactly one"),
            "error must describe ambiguity: {msg}"
        );
    }

    #[test]
    fn terminal_recording_required_with_b64_key_only_validates() {
        let cfg = production_recording_cfg();
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_required_with_file_key_only_validates() {
        let mut cfg = production_recording_cfg();
        cfg.terminal_recording.encryption.master_key_b64 = None;
        cfg.terminal_recording.encryption.master_key_file =
            Some(std::path::PathBuf::from("/etc/relayterm/recording-key"));
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_disabled_mode_with_key_source_is_dead_config() {
        // A key set under `mode = disabled` is never read; reject
        // rather than silently ignore so the operator notices.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.enabled = true;
        cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::Disabled;
        cfg.terminal_recording.encryption.master_key_b64 = Some(b64_32());
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("encryption.mode = disabled"),
            "error must explain the inconsistency: {msg}"
        );
    }

    #[test]
    fn terminal_recording_key_equal_to_vault_b64_fails() {
        let mut cfg = production_recording_cfg();
        let shared = b64_32();
        cfg.vault.master_key_b64 = Some(shared.clone());
        cfg.terminal_recording.encryption.master_key_b64 = Some(shared);
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("SEPARATE"),
            "error must mandate separateness: {msg}"
        );
    }

    #[test]
    fn terminal_recording_key_equal_to_vault_file_path_fails() {
        let mut cfg = production_recording_cfg();
        cfg.terminal_recording.encryption.master_key_b64 = None;
        cfg.terminal_recording.encryption.master_key_file =
            Some(std::path::PathBuf::from("/etc/relayterm/key"));
        cfg.vault.master_key_b64 = None;
        cfg.vault.master_key_file = Some(std::path::PathBuf::from("/etc/relayterm/key"));
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("SEPARATE"),
            "error must mandate separateness: {msg}"
        );
    }

    #[test]
    fn terminal_recording_zero_retention_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.retention_days = 0;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("retention_days"),
            "error must name retention_days: {msg}"
        );
    }

    #[test]
    fn terminal_recording_huge_retention_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.retention_days =
            terminal_recording_defaults::RETENTION_DAYS_HARD_CAP + 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("retention_days"),
            "error must name retention_days: {msg}"
        );
    }

    #[test]
    fn terminal_recording_max_bytes_below_chunk_cap_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.max_bytes_per_session =
            u64::from(cfg.terminal_recording.chunk_hard_cap_bytes) - 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_bytes_per_session"),
            "error must name the failing field: {msg}"
        );
    }

    #[test]
    fn terminal_recording_max_bytes_zero_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.max_bytes_per_session = 0;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_bytes_per_session"),
            "error must name max_bytes_per_session: {msg}"
        );
    }

    #[test]
    fn terminal_recording_huge_max_bytes_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.max_bytes_per_session =
            terminal_recording_defaults::MAX_BYTES_PER_SESSION_HARD_CAP + 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_bytes_per_session"),
            "error must name max_bytes_per_session: {msg}"
        );
    }

    #[test]
    fn terminal_recording_chunk_target_above_hard_cap_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.chunk_target_bytes = cfg.terminal_recording.chunk_hard_cap_bytes + 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("chunk_target_bytes"),
            "error must name chunk_target_bytes: {msg}"
        );
    }

    #[test]
    fn terminal_recording_chunk_hard_cap_below_floor_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.chunk_hard_cap_bytes =
            terminal_recording_defaults::CHUNK_HARD_CAP_BYTES_FLOOR - 1;
        cfg.terminal_recording.chunk_target_bytes = cfg.terminal_recording.chunk_hard_cap_bytes;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("chunk_hard_cap_bytes"),
            "error must name chunk_hard_cap_bytes: {msg}"
        );
    }

    #[test]
    fn terminal_recording_malformed_enabled_env_value_fails_safely() {
        // The recording env parser is stricter than the vault / auth
        // scalar parsers (see the env-loading comment in config.rs):
        // `ENABLED=yes` is a typo, not a no-op. Pin the policy so a
        // future cleanup that "harmonises" the parsers does not
        // re-introduce the silent-discard channel for the recording
        // gate. Production validation depends on `enabled = true`
        // surfacing accurately so the `encryption.mode = required`
        // requirement actually fires.
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_TERMINAL_RECORDING__ENABLED", "yes")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__ENABLED"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_malformed_retention_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_TERMINAL_RECORDING__RETENTION_DAYS", "abc")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__RETENTION_DAYS"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_malformed_max_bytes_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_TERMINAL_RECORDING__MAX_BYTES_PER_SESSION", "-1")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__MAX_BYTES_PER_SESSION"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_unsupported_encryption_mode_in_env_fails() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MODE",
                "totally-bogus",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MODE"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_unsupported_compression_mode_in_env_fails() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_RECORDING__COMPRESSION__MODE",
                "zstd-later",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__COMPRESSION__MODE"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_unsupported_encryption_mode_in_toml_fails() {
        let raw = "[terminal_recording.encryption]\nmode = \"halfway\"\n";
        let err = toml::from_str::<FileConfig>(raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown variant") && msg.contains("halfway"),
            "TOML parse error must name the unknown variant: {msg}"
        );
    }

    #[test]
    fn terminal_recording_config_debug_redacts_master_key_b64() {
        let cfg = TerminalRecordingEncryptionConfig {
            mode: TerminalRecordingEncryptionMode::Required,
            master_key_b64: Some(RECORDING_SECRET_MARKER.to_owned()),
            master_key_file: None,
        };
        let s = format!("{cfg:?}");
        assert!(
            !s.contains(RECORDING_SECRET_MARKER),
            "TerminalRecordingEncryptionConfig debug must not echo master_key_b64: {s}"
        );
        assert!(s.contains("master_key_b64_set: true"));
    }

    #[test]
    fn file_terminal_recording_config_debug_redacts_master_key_b64() {
        let fr = FileTerminalRecordingEncryptionConfig {
            mode: Some(TerminalRecordingEncryptionMode::Required),
            master_key_b64: Some(RECORDING_SECRET_MARKER.to_owned()),
            master_key_file: None,
        };
        let s = format!("{fr:?}");
        assert!(
            !s.contains(RECORDING_SECRET_MARKER),
            "FileTerminalRecordingEncryptionConfig debug must not echo master_key_b64: {s}"
        );
        assert!(s.contains("master_key_b64_set: true"));
    }

    #[test]
    fn terminal_recording_outer_debug_redacts_master_key_b64() {
        // The outer struct's manual Debug impl must propagate the
        // redaction discipline from the encryption sub-struct, mirroring
        // how `AuthConfig`'s Debug propagates from its secret-shaped
        // fields.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.encryption.master_key_b64 = Some(RECORDING_SECRET_MARKER.to_owned());
        let s = format!("{:?}", cfg.terminal_recording);
        assert!(
            !s.contains(RECORDING_SECRET_MARKER),
            "TerminalRecordingConfig debug must not echo master_key_b64 through any field: {s}"
        );
    }

    #[test]
    fn terminal_recording_validation_errors_do_not_echo_secret_values() {
        // Same redaction posture as `auth_validation_errors_do_not_echo_secret_env_values`:
        // every reachable failure path on a recording config that has
        // sentinel-shaped secrets must NOT splice those secrets into the
        // operator-visible error message.

        // (1) Production + enabled + mode = disabled (key present is
        // also dead config but the production check fires first).
        {
            let mut cfg = production_cfg();
            cfg.terminal_recording.enabled = true;
            cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::Disabled;
            cfg.terminal_recording.encryption.master_key_b64 =
                Some(RECORDING_SECRET_MARKER.to_owned());
            let err = cfg.validate_terminal_recording().unwrap_err();
            assert!(
                !err.to_string().contains(RECORDING_SECRET_MARKER),
                "production-disabled error must not echo recording-key value: {err}"
            );
        }

        // (2) Required + both sources set (b64 carries the marker).
        {
            let mut cfg = production_recording_cfg();
            cfg.terminal_recording.encryption.master_key_b64 =
                Some(RECORDING_SECRET_MARKER.to_owned());
            cfg.terminal_recording.encryption.master_key_file =
                Some(std::path::PathBuf::from("/dev/null"));
            let err = cfg.validate_terminal_recording().unwrap_err();
            assert!(
                !err.to_string().contains(RECORDING_SECRET_MARKER),
                "ambiguous-key error must not echo recording-key value: {err}"
            );
        }

        // (3) Recording key equal to vault key (b64). The error mandates
        // separateness; it MUST NOT echo the shared secret value.
        {
            let mut cfg = production_recording_cfg();
            cfg.vault.master_key_b64 = Some(RECORDING_SECRET_MARKER.to_owned());
            cfg.terminal_recording.encryption.master_key_b64 =
                Some(RECORDING_SECRET_MARKER.to_owned());
            let err = cfg.validate_terminal_recording().unwrap_err();
            assert!(
                !err.to_string().contains(RECORDING_SECRET_MARKER),
                "recording==vault error must not echo the shared key value: {err}"
            );
        }

        // (4) Disabled-mode dead-config rejection (key carries the marker).
        {
            let mut cfg = empty_cfg();
            cfg.terminal_recording.enabled = true;
            cfg.terminal_recording.encryption.mode = TerminalRecordingEncryptionMode::Disabled;
            cfg.terminal_recording.encryption.master_key_b64 =
                Some(RECORDING_SECRET_MARKER.to_owned());
            let err = cfg.validate_terminal_recording().unwrap_err();
            assert!(
                !err.to_string().contains(RECORDING_SECRET_MARKER),
                "disabled-mode-with-key error must not echo recording-key value: {err}"
            );
        }
    }

    #[test]
    fn terminal_recording_env_overrides_apply() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[
                ("RELAYTERM_TERMINAL_RECORDING__ENABLED", "true"),
                ("RELAYTERM_TERMINAL_RECORDING__RETENTION_DAYS", "14"),
                (
                    "RELAYTERM_TERMINAL_RECORDING__MAX_BYTES_PER_SESSION",
                    "67108864",
                ),
                ("RELAYTERM_TERMINAL_RECORDING__CHUNK_TARGET_BYTES", "65536"),
                (
                    "RELAYTERM_TERMINAL_RECORDING__CHUNK_HARD_CAP_BYTES",
                    "2097152",
                ),
                ("RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MODE", "required"),
                (
                    "RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_B64",
                    "k",
                ),
                (
                    "RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_FILE",
                    "/dev/null",
                ),
                ("RELAYTERM_TERMINAL_RECORDING__COMPRESSION__MODE", "none"),
            ]),
        )
        .unwrap();
        assert!(cfg.terminal_recording.enabled);
        assert_eq!(cfg.terminal_recording.retention_days, 14);
        assert_eq!(cfg.terminal_recording.max_bytes_per_session, 67_108_864);
        assert_eq!(cfg.terminal_recording.chunk_target_bytes, 65_536);
        assert_eq!(cfg.terminal_recording.chunk_hard_cap_bytes, 2_097_152);
        assert_eq!(
            cfg.terminal_recording.encryption.mode,
            TerminalRecordingEncryptionMode::Required
        );
        assert_eq!(
            cfg.terminal_recording.encryption.master_key_b64.as_deref(),
            Some("k")
        );
        assert_eq!(
            cfg.terminal_recording.encryption.master_key_file.as_deref(),
            Some(std::path::Path::new("/dev/null"))
        );
        assert_eq!(
            cfg.terminal_recording.compression.mode,
            TerminalRecordingCompressionMode::None
        );
    }

    #[test]
    fn terminal_recording_toml_round_trip() {
        let raw = r#"
[terminal_recording]
enabled = true
retention_days = 7
max_bytes_per_session = 33554432
chunk_target_bytes = 32768
chunk_hard_cap_bytes = 2097152

[terminal_recording.encryption]
mode = "required"
master_key_b64 = "AAAA-BASE64-PLACEHOLDER-AAAA"

[terminal_recording.compression]
mode = "none"
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert!(cfg.terminal_recording.enabled);
        assert_eq!(cfg.terminal_recording.retention_days, 7);
        assert_eq!(cfg.terminal_recording.max_bytes_per_session, 33_554_432);
        assert_eq!(cfg.terminal_recording.chunk_target_bytes, 32_768);
        assert_eq!(cfg.terminal_recording.chunk_hard_cap_bytes, 2_097_152);
        assert_eq!(
            cfg.terminal_recording.encryption.mode,
            TerminalRecordingEncryptionMode::Required
        );
        assert!(
            cfg.terminal_recording.encryption.master_key_b64.is_some(),
            "TOML key must be merged into the runtime config"
        );
        assert_eq!(
            cfg.terminal_recording.compression.mode,
            TerminalRecordingCompressionMode::None
        );
    }

    // --- Terminal recording cleanup config -------------------------

    #[test]
    fn terminal_recording_cleanup_default_validates() {
        // Defaults are picked deliberately so a fresh boot does NOT
        // change runtime behaviour on the way in (no caller drives the
        // cleanup primitive yet) and validation still passes — the
        // future worker reads exactly these numbers.
        let cfg = Config::defaults();
        assert!(cfg.terminal_recording.cleanup.enabled);
        assert!(cfg.terminal_recording.cleanup.startup_sweep_enabled);
        assert!(!cfg.terminal_recording.cleanup.periodic_sweep_enabled);
        assert_eq!(cfg.terminal_recording.cleanup.sweep_interval_seconds, 0);
        assert_eq!(
            cfg.terminal_recording.cleanup.batch_size,
            terminal_recording_defaults::CLEANUP_BATCH_SIZE
        );
        cfg.validate_terminal_recording()
            .expect("default cleanup config must validate");
    }

    #[test]
    fn terminal_recording_cleanup_validates_when_recording_disabled() {
        // Independence rule (Section 12.6): cleanup must validate even
        // when `terminal_recording.enabled = false`. Disabling
        // recording later must not make an existing recording corpus
        // immortal.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.enabled = false;
        // Explicit cleanup that would be exercised on a future tick.
        cfg.terminal_recording.cleanup.enabled = true;
        cfg.terminal_recording.cleanup.periodic_sweep_enabled = true;
        cfg.terminal_recording.cleanup.sweep_interval_seconds = 21_600;
        cfg.terminal_recording.cleanup.batch_size = 250;
        cfg.validate_terminal_recording()
            .expect("cleanup must validate independently of recording.enabled");
    }

    #[test]
    fn terminal_recording_cleanup_zero_batch_size_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.batch_size = 0;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("batch_size"),
            "error must name batch_size: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_huge_batch_size_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.batch_size =
            terminal_recording_defaults::CLEANUP_BATCH_SIZE_MAX + 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("batch_size"),
            "error must name batch_size: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_zero_interval_with_periodic_disabled_validates() {
        // The canonical "no periodic schedule" shape: sentinel `0` and
        // `periodic_sweep_enabled = false`.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.periodic_sweep_enabled = false;
        cfg.terminal_recording.cleanup.sweep_interval_seconds = 0;
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_cleanup_valid_interval_with_periodic_disabled_validates() {
        // Operator stages a future enable: keep `periodic_sweep_enabled
        // = false` for now but pin the cadence in advance. Validator
        // accepts both shapes.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.periodic_sweep_enabled = false;
        cfg.terminal_recording.cleanup.sweep_interval_seconds = 3_600;
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_cleanup_sub_minimum_interval_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.sweep_interval_seconds =
            terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MIN - 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("sweep_interval_seconds"),
            "error must name sweep_interval_seconds: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_above_max_interval_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.sweep_interval_seconds =
            terminal_recording_defaults::CLEANUP_SWEEP_INTERVAL_SECONDS_MAX + 1;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("sweep_interval_seconds"),
            "error must name sweep_interval_seconds: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_periodic_enabled_without_interval_fails() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.periodic_sweep_enabled = true;
        cfg.terminal_recording.cleanup.sweep_interval_seconds = 0;
        let err = cfg.validate_terminal_recording().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("periodic_sweep_enabled"),
            "error must name periodic_sweep_enabled: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_periodic_enabled_with_valid_interval_validates() {
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.periodic_sweep_enabled = true;
        cfg.terminal_recording.cleanup.sweep_interval_seconds = 21_600;
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_cleanup_disabled_master_switch_validates() {
        // `cleanup.enabled = false` is the explicit opt-out; the
        // remaining fields are still bounds-checked but valid.
        let mut cfg = empty_cfg();
        cfg.terminal_recording.cleanup.enabled = false;
        cfg.terminal_recording.cleanup.startup_sweep_enabled = false;
        cfg.terminal_recording.cleanup.periodic_sweep_enabled = false;
        cfg.terminal_recording.cleanup.sweep_interval_seconds = 0;
        cfg.terminal_recording.cleanup.batch_size = 1;
        cfg.validate_terminal_recording().unwrap();
    }

    #[test]
    fn terminal_recording_cleanup_malformed_enabled_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED", "maybe")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_malformed_batch_size_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[("RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE", "abc")]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_malformed_startup_sweep_enabled_env_value_fails_safely() {
        // The `STARTUP_SWEEP_ENABLED` boolean parses through the same
        // strict path as `ENABLED`; pin its env-failure shape too so a
        // future cleanup that "harmonises" the parsers cannot
        // re-introduce a silent-discard channel for any one of the
        // four cleanup booleans.
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_RECORDING__CLEANUP__STARTUP_SWEEP_ENABLED",
                "yes",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__CLEANUP__STARTUP_SWEEP_ENABLED"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_malformed_periodic_sweep_enabled_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED",
                "1",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_malformed_interval_env_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS",
                "-1",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_recording_cleanup_env_overrides_apply() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[
                ("RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED", "false"),
                (
                    "RELAYTERM_TERMINAL_RECORDING__CLEANUP__STARTUP_SWEEP_ENABLED",
                    "false",
                ),
                (
                    "RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED",
                    "true",
                ),
                (
                    "RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS",
                    "21600",
                ),
                ("RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE", "250"),
            ]),
        )
        .unwrap();
        assert!(!cfg.terminal_recording.cleanup.enabled);
        assert!(!cfg.terminal_recording.cleanup.startup_sweep_enabled);
        assert!(cfg.terminal_recording.cleanup.periodic_sweep_enabled);
        assert_eq!(
            cfg.terminal_recording.cleanup.sweep_interval_seconds,
            21_600
        );
        assert_eq!(cfg.terminal_recording.cleanup.batch_size, 250);
    }

    #[test]
    fn terminal_recording_cleanup_toml_round_trip() {
        let raw = r#"
[terminal_recording.cleanup]
enabled = true
startup_sweep_enabled = true
periodic_sweep_enabled = true
sweep_interval_seconds = 21600
batch_size = 100
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert!(cfg.terminal_recording.cleanup.enabled);
        assert!(cfg.terminal_recording.cleanup.startup_sweep_enabled);
        assert!(cfg.terminal_recording.cleanup.periodic_sweep_enabled);
        assert_eq!(
            cfg.terminal_recording.cleanup.sweep_interval_seconds,
            21_600
        );
        assert_eq!(cfg.terminal_recording.cleanup.batch_size, 100);
        cfg.validate_terminal_recording()
            .expect("TOML round-trip must validate");
    }

    // --- Terminal sessions (detached-live-PTY TTL) ------------------

    #[test]
    fn terminal_sessions_default_ttl_matches_pre_config_constant() {
        // The default MUST match `relayterm_terminal::DETACHED_LIVE_PTY_TTL`
        // so a deploy that does not touch this knob behaves identically
        // to the pre-config baseline. Pinning the seconds value here is
        // sufficient — the wrapper-side test in
        // `crates/relayterm-terminal/tests/manager.rs::detach_ttl_default_matches_pinned_constant`
        // re-asserts the manager-level invariant.
        let cfg = Config::defaults();
        assert_eq!(
            cfg.terminal_sessions.detached_live_pty_ttl_seconds,
            terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS,
        );
        assert_eq!(
            cfg.terminal_sessions.detached_live_pty_ttl_seconds,
            relayterm_terminal::DETACHED_LIVE_PTY_TTL.as_secs(),
        );
        cfg.validate_terminal_sessions()
            .expect("default TTL must validate");
        assert_eq!(
            cfg.detached_live_pty_ttl(),
            relayterm_terminal::DETACHED_LIVE_PTY_TTL,
        );
    }

    #[test]
    fn terminal_sessions_env_override_parses() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS",
                "300",
            )]),
        )
        .unwrap();
        assert_eq!(cfg.terminal_sessions.detached_live_pty_ttl_seconds, 300);
        cfg.validate_terminal_sessions().unwrap();
        assert_eq!(
            cfg.detached_live_pty_ttl(),
            std::time::Duration::from_secs(300)
        );
    }

    #[test]
    fn terminal_sessions_toml_round_trip() {
        let raw = r#"
[terminal_sessions]
detached_live_pty_ttl_seconds = 600
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.terminal_sessions.detached_live_pty_ttl_seconds, 600);
        cfg.validate_terminal_sessions()
            .expect("TOML round-trip must validate");
    }

    #[test]
    fn terminal_sessions_env_invalid_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS",
                "not-a-number",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_zero_ttl_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.detached_live_pty_ttl_seconds = 0;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("detached_live_pty_ttl_seconds") && msg.contains("greater than 0"),
            "zero TTL must be rejected with a clear message: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_below_min_ttl_rejected() {
        let mut cfg = empty_cfg();
        // Just under the documented floor.
        cfg.terminal_sessions.detached_live_pty_ttl_seconds =
            terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MIN - 1;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("detached_live_pty_ttl_seconds") && msg.contains(">="),
            "below-min TTL must name the bound: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_above_max_ttl_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.detached_live_pty_ttl_seconds =
            terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MAX + 1;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("detached_live_pty_ttl_seconds") && msg.contains("hard cap"),
            "above-max TTL must name the bound: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_min_and_max_boundaries_validate() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.detached_live_pty_ttl_seconds =
            terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MIN;
        cfg.validate_terminal_sessions().expect("min boundary ok");
        cfg.terminal_sessions.detached_live_pty_ttl_seconds =
            terminal_sessions_defaults::DETACHED_LIVE_PTY_TTL_SECONDS_MAX;
        cfg.validate_terminal_sessions().expect("max boundary ok");
    }

    // --- Terminal sessions (per-user live PTY quota — Phase 1B.1) ----

    #[test]
    fn terminal_sessions_default_max_live_pty_per_user_is_eight() {
        // Phase 1B.1 default. `docs/session-quotas.md` § 4.1 names `8`
        // as the recommended default — pinned here so a future bump in
        // either source surfaces here in CI rather than silently in
        // production behaviour.
        let cfg = Config::defaults();
        assert_eq!(
            cfg.terminal_sessions.max_live_pty_sessions_per_user,
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER,
        );
        assert_eq!(cfg.terminal_sessions.max_live_pty_sessions_per_user, 8);
        cfg.validate_terminal_sessions()
            .expect("default max-live cap must validate");
        assert_eq!(cfg.max_live_pty_sessions_per_user(), 8);
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_user_env_override_parses() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER",
                "16",
            )]),
        )
        .unwrap();
        assert_eq!(cfg.terminal_sessions.max_live_pty_sessions_per_user, 16);
        cfg.validate_terminal_sessions().unwrap();
        assert_eq!(cfg.max_live_pty_sessions_per_user(), 16);
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_user_toml_round_trip() {
        let raw = r#"
[terminal_sessions]
max_live_pty_sessions_per_user = 4
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.terminal_sessions.max_live_pty_sessions_per_user, 4);
        cfg.validate_terminal_sessions()
            .expect("TOML round-trip must validate");
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_user_env_invalid_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER",
                "not-a-number",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_zero_max_live_pty_per_user_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_user = 0;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_live_pty_sessions_per_user") && msg.contains(">="),
            "zero cap must be rejected naming the field and the bound: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_above_max_live_pty_per_user_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_user =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MAX + 1;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_live_pty_sessions_per_user") && msg.contains("hard cap"),
            "above-max cap must be rejected naming the field and the bound: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_user_min_and_max_boundaries_validate() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_user =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MIN;
        cfg.validate_terminal_sessions()
            .expect("min boundary ok (per-user live cap)");
        // Bump the deployment cap to its own MAX so the cross-field
        // check (`max_live_pty_sessions_per_deployment >=
        // max_live_pty_sessions_per_user`) is satisfied at the
        // per-user upper boundary.
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MAX;
        cfg.terminal_sessions.max_live_pty_sessions_per_user =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MAX;
        cfg.validate_terminal_sessions()
            .expect("max boundary ok (per-user live cap)");
    }

    // --- Terminal sessions (per-user starting quota — Phase 1B.2a) --

    #[test]
    fn terminal_sessions_default_max_starting_per_user_is_four() {
        // Phase 1B.2a default. `docs/session-quotas.md` § 4.3 names `4`
        // — pinned here so a future bump in either source surfaces in
        // CI rather than silently in production behaviour.
        let cfg = Config::defaults();
        assert_eq!(
            cfg.terminal_sessions.max_starting_sessions_per_user,
            terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER,
        );
        assert_eq!(cfg.terminal_sessions.max_starting_sessions_per_user, 4);
        cfg.validate_terminal_sessions()
            .expect("default starting cap must validate");
        assert_eq!(cfg.max_starting_sessions_per_user(), 4);
    }

    #[test]
    fn terminal_sessions_max_starting_per_user_env_override_parses() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER",
                "8",
            )]),
        )
        .unwrap();
        assert_eq!(cfg.terminal_sessions.max_starting_sessions_per_user, 8);
        cfg.validate_terminal_sessions().unwrap();
        assert_eq!(cfg.max_starting_sessions_per_user(), 8);
    }

    #[test]
    fn terminal_sessions_max_starting_per_user_toml_round_trip() {
        let raw = r#"
[terminal_sessions]
max_starting_sessions_per_user = 2
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(cfg.terminal_sessions.max_starting_sessions_per_user, 2);
        cfg.validate_terminal_sessions()
            .expect("TOML round-trip must validate");
    }

    #[test]
    fn terminal_sessions_max_starting_per_user_env_invalid_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER",
                "not-a-number",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER"),
            "error must name the failing input: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_zero_max_starting_per_user_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_starting_sessions_per_user = 0;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_starting_sessions_per_user") && msg.contains(">="),
            "zero starting cap must be rejected naming the field and the bound: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_above_max_starting_per_user_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_starting_sessions_per_user =
            terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MAX + 1;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_starting_sessions_per_user") && msg.contains("hard cap"),
            "above-max starting cap must be rejected naming the field and the bound: {msg}"
        );
    }

    #[test]
    fn terminal_sessions_max_starting_per_user_min_and_max_boundaries_validate() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_starting_sessions_per_user =
            terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MIN;
        cfg.validate_terminal_sessions()
            .expect("min boundary ok (per-user starting cap)");
        // The default deployment cap (64) is already > MAX_STARTING_SESSIONS_PER_USER_MAX (32),
        // so no cross-field adjustment is needed at the upper boundary here.
        cfg.terminal_sessions.max_starting_sessions_per_user =
            terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MAX;
        cfg.validate_terminal_sessions()
            .expect("max boundary ok (per-user starting cap)");
    }

    // --- Terminal sessions (deployment live quota — Phase 1B.2b) -----

    #[test]
    fn terminal_sessions_default_max_live_pty_per_deployment_is_sixty_four() {
        // Phase 1B.2b default. `docs/session-quotas.md` § 4.2 names
        // `64` — pinned here so a future bump in either source
        // surfaces in CI rather than silently in production behaviour.
        let cfg = Config::defaults();
        assert_eq!(
            cfg.terminal_sessions.max_live_pty_sessions_per_deployment,
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT,
        );
        assert_eq!(
            cfg.terminal_sessions.max_live_pty_sessions_per_deployment,
            64
        );
        cfg.validate_terminal_sessions()
            .expect("default deployment cap must validate");
        assert_eq!(cfg.max_live_pty_sessions_per_deployment(), 64);
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_deployment_env_override_parses() {
        let mut cfg = empty_cfg();
        Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT",
                "128",
            )]),
        )
        .unwrap();
        assert_eq!(
            cfg.terminal_sessions.max_live_pty_sessions_per_deployment,
            128,
        );
        cfg.validate_terminal_sessions().unwrap();
        assert_eq!(cfg.max_live_pty_sessions_per_deployment(), 128);
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_deployment_toml_round_trip() {
        let raw = r#"
[terminal_sessions]
max_live_pty_sessions_per_deployment = 32
"#;
        let parsed: FileConfig = toml::from_str(raw).unwrap();
        let mut cfg = Config::defaults();
        parsed.merge_into(&mut cfg);
        assert_eq!(
            cfg.terminal_sessions.max_live_pty_sessions_per_deployment,
            32,
        );
        cfg.validate_terminal_sessions()
            .expect("TOML round-trip must validate");
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_deployment_env_invalid_value_fails_safely() {
        let mut cfg = empty_cfg();
        let err = Config::apply_env_with(
            &mut cfg,
            env_from(&[(
                "RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT",
                "not-a-number",
            )]),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT"),
            "error must name the failing input: {msg}",
        );
    }

    #[test]
    fn terminal_sessions_zero_max_live_pty_per_deployment_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment = 0;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_live_pty_sessions_per_deployment") && msg.contains(">="),
            "zero deployment cap must be rejected naming the field and the bound: {msg}",
        );
    }

    #[test]
    fn terminal_sessions_above_max_live_pty_per_deployment_rejected() {
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MAX + 1;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_live_pty_sessions_per_deployment") && msg.contains("hard cap"),
            "above-max deployment cap must be rejected naming the field and the bound: {msg}",
        );
    }

    #[test]
    fn terminal_sessions_max_live_pty_per_deployment_min_and_max_boundaries_validate() {
        let mut cfg = empty_cfg();
        // Lower the per-user caps so the min-boundary deployment cap is
        // still >= every per-user cap (cross-field rule).
        cfg.terminal_sessions.max_live_pty_sessions_per_user =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_USER_MIN;
        cfg.terminal_sessions.max_starting_sessions_per_user =
            terminal_sessions_defaults::MAX_STARTING_SESSIONS_PER_USER_MIN;
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MIN;
        cfg.validate_terminal_sessions()
            .expect("min boundary ok (deployment cap)");
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment =
            terminal_sessions_defaults::MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT_MAX;
        cfg.validate_terminal_sessions()
            .expect("max boundary ok (deployment cap)");
    }

    #[test]
    fn terminal_sessions_deployment_below_per_user_live_rejected() {
        // Cross-field rule (`docs/session-quotas.md` § 5.2): the
        // deployment-wide cap MUST sit at or above the per-user live
        // cap. The error names both fields.
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_user = 16;
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment = 8;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_live_pty_sessions_per_deployment")
                && msg.contains("max_live_pty_sessions_per_user"),
            "deployment-below-per-user-live must name both fields: {msg}",
        );
    }

    #[test]
    fn terminal_sessions_deployment_below_starting_rejected() {
        // Cross-field rule (`docs/session-quotas.md` § 5.2): the
        // deployment-wide cap MUST sit at or above the starting cap.
        let mut cfg = empty_cfg();
        // Keep per-user live below the deployment cap so the starting
        // mismatch is what trips the validator.
        cfg.terminal_sessions.max_live_pty_sessions_per_user = 1;
        cfg.terminal_sessions.max_starting_sessions_per_user = 16;
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment = 8;
        let err = cfg.validate_terminal_sessions().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("max_live_pty_sessions_per_deployment")
                && msg.contains("max_starting_sessions_per_user"),
            "deployment-below-starting must name both fields: {msg}",
        );
    }

    #[test]
    fn terminal_sessions_deployment_equal_to_per_user_live_validates() {
        // Equal-bound at the cross-field check is acceptable (>=, not
        // >). Single-user deployments can configure dep == per-user.
        let mut cfg = empty_cfg();
        cfg.terminal_sessions.max_live_pty_sessions_per_user = 4;
        cfg.terminal_sessions.max_starting_sessions_per_user = 4;
        cfg.terminal_sessions.max_live_pty_sessions_per_deployment = 4;
        cfg.validate_terminal_sessions()
            .expect("dep == per-user is allowed at the boundary");
    }
}
