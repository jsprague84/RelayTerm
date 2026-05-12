//! Postgres-backed router/handler tests for the v1 API.
//!
//! Gated behind the `postgres-tests` feature so `cargo test --workspace`
//! stays runnable without infra. Run with:
//!
//! ```bash
//! docker compose -f deploy/docker-compose.yml up -d postgres
//! DATABASE_URL=postgres://relayterm:relayterm@127.0.0.1:5432/relayterm \
//!   cargo test -p relayterm-api --features postgres-tests
//! ```
//!
//! Each test drives the full router via `tower::ServiceExt::oneshot` so the
//! axum extractors, error mapping, JSON shape, and DB layer are all
//! exercised end-to-end.

#![cfg(feature = "postgres-tests")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt as _;
use relayterm_api::{AppState, AuthRoutesConfig, router};
use relayterm_auth::{
    AuthService, LoginThrottleConfig, LoginThrottler, PasswordHasher, PasswordHasherConfig,
};
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::ids::UserId;
use relayterm_core::repository::{
    AuditEventRepository, CreateAuditEvent, CreateHost, CreateKnownHostEntry, CreateServerProfile,
    CreateSshIdentity, CreateUser, HostRepository, KnownHostEntryRepository,
    PasswordCredentialRepository, ServerProfileRepository, SessionEventRepository,
    SshIdentityRepository, TerminalSessionRepository, UserRepository, UserSessionRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_core::terminal_session::TerminalSessionStatus;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_ssh_port, validate_ssh_username,
};
use relayterm_db::{
    Db, PgAuditEventRepository, PgHostRepository, PgKnownHostEntryRepository,
    PgServerProfileRepository, PgSessionEventRepository, PgSshIdentityRepository,
    PgTerminalRecordingRepository, PgTerminalSessionRepository, PgUserRepository,
};
use relayterm_ssh::{
    AuthAttemptKind, AuthCheckOutcome, AuthCheckTarget, CapturedHostKey, HostKeyPreflightService,
    ProbeError, ProbeTarget, SshAuthCheckService, SshAuthChecker, SshHostKeyProbe, SshPtyBridge,
    SshPtyError, SshPtyEvent, SshPtyHandle, SshPtyStart, SshPtyTarget,
};
use relayterm_terminal::TerminalSessionManager;
use relayterm_vault::VaultService;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use zeroize::Zeroizing;

const PRIVATE_KEY_MARKER: &[u8] = b"REDACT-MARKER-API-9F2B";

/// Origin allow-listed by the per-test [`AuthRoutesConfig`]. The
/// shared `CsrfGuard` extractor allows this value on every state-
/// changing browser-write route; GETs are exempt.
const TEST_AUTH_ORIGIN: &str = "https://relay.test.local";

/// Bootstrap token plumbed into every test [`AuthRoutesConfig`].
/// Sentinel-shaped so the audit-redaction tests can assert it never
/// reaches a persisted payload.
const TEST_BOOTSTRAP_TOKEN: &str = "TEST-BOOTSTRAP-TOKEN-MARKER-DO-NOT-LEAK";

fn test_auth(db: &Db) -> Arc<AuthService> {
    // Tuned-down hasher so test runs stay sub-second. Production code
    // path uses `PasswordHasher::default()` (OWASP_2023 baseline) — see
    // `apps/backend/src/main.rs`.
    Arc::new(AuthService::new(
        Arc::new(db.password_credentials()) as Arc<dyn PasswordCredentialRepository>,
        Arc::new(db.user_sessions()) as Arc<dyn UserSessionRepository>,
        PasswordHasher::new(PasswordHasherConfig {
            m_cost: 19_456,
            t_cost: 1,
            p_cost: 1,
        })
        .expect("fast hasher params are valid"),
    ))
}

/// Default login throttler for fixtures that don't care about
/// throttling. The policy mirrors production (`V1_DEFAULT`); tests that
/// drive the throttle deliberately build their own with a tighter
/// bucket via `LoginThrottler::new(...)` and inject it explicitly.
fn test_login_throttler() -> Arc<LoginThrottler> {
    Arc::new(LoginThrottler::new(LoginThrottleConfig::V1_DEFAULT))
}

fn test_auth_routes() -> Arc<AuthRoutesConfig> {
    Arc::new(AuthRoutesConfig {
        // Tests run over `tower::ServiceExt::oneshot` so `Secure` would
        // not be honored anyway; pin to `false` so the auth-route
        // tests don't have to special-case it.
        cookie_secure: false,
        cookie_domain: None,
        allowed_origins: vec![TEST_AUTH_ORIGIN.to_owned()],
        bootstrap_token: Some(zeroize::Zeroizing::new(TEST_BOOTSTRAP_TOKEN.to_owned())),
    })
}

async fn create_user(pool: &PgPool, label: &str) -> UserId {
    PgUserRepository::new(pool.clone())
        .create(CreateUser {
            email: format!("{label}+{}@relayterm.local", uuid::Uuid::new_v4()),
            display_name: label.to_owned(),
        })
        .await
        .expect("create user")
        .id
}

/// Bootstrap a real session for `user_id` directly through
/// [`AuthService`] and return the plaintext cookie token.
///
/// Going through the service rather than the `/auth/login` route keeps
/// the per-test cost down (no JSON body parse, no audit append, no
/// Argon2id hash — the password row is irrelevant for cookie minting)
/// while producing a wire-shaped session: the resulting cookie
/// validates through the same `AuthenticatedUser` extractor production
/// traffic uses. The token plaintext crosses the function boundary
/// EXACTLY ONCE and immediately becomes the value the test attaches to
/// the `Cookie:` header. Mirrors the discipline the production
/// `Set-Cookie` writer keeps.
async fn bootstrap_test_session(auth: &Arc<AuthService>, user_id: UserId) -> String {
    let now = chrono::Utc::now();
    let created = auth
        .create_session(user_id, chrono::Duration::days(30), now)
        .await
        .expect("create fixture session");
    let token = created.token.expose().to_owned();
    drop(created);
    token
}

async fn setup(pool: PgPool) -> (Router, UserId, String) {
    setup_with_probe(pool, default_probe()).await
}

async fn setup_with_probe(
    pool: PgPool,
    probe: Arc<dyn SshHostKeyProbe>,
) -> (Router, UserId, String) {
    setup_full(pool, probe, default_auth_checker()).await
}

async fn setup_full(
    pool: PgPool,
    probe: Arc<dyn SshHostKeyProbe>,
    checker: Arc<dyn SshAuthChecker>,
) -> (Router, UserId, String) {
    setup_with_auth_check_service(pool, probe, Arc::new(SshAuthCheckService::new(checker))).await
}

/// Variant of `setup_full` that takes a pre-built [`SshAuthCheckService`].
/// Tests use this to inject an `SshAuthCheckService::with_limits(...)` so
/// the timeout and concurrency bounds are short enough to exercise the
/// safety guards without burning real wall-clock budget.
async fn setup_with_auth_check_service(
    pool: PgPool,
    probe: Arc<dyn SshHostKeyProbe>,
    auth_check: Arc<SshAuthCheckService>,
) -> (Router, UserId, String) {
    setup_with_full_state(pool, probe, auth_check, default_pty_bridge()).await
}

/// Most general setup: every dependency is injectable. Used by tests
/// that drive the live PTY surface and need the bridge to either
/// succeed (default) or fail with a specific [`SshPtyError`].
async fn setup_with_full_state(
    pool: PgPool,
    probe: Arc<dyn SshHostKeyProbe>,
    auth_check: Arc<SshAuthCheckService>,
    pty_bridge: Arc<dyn SshPtyBridge>,
) -> (Router, UserId, String) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check,
        pty_bridge,
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    (router(state), user_id, cookie)
}

/// Variant of [`setup_with_full_state`] that overrides the manager's
/// per-user live-PTY ceiling (Phase 1B.1 quota). Used by integration
/// tests that drive the `429 too_many_sessions` refusal path without
/// burning the full default `8` sessions.
async fn setup_with_max_live_per_user(
    pool: PgPool,
    cap: std::num::NonZeroU32,
) -> (Router, UserId, String) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager_with_max_live_per_user(&db, cap);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    (router(state), user_id, cookie)
}

/// Variant of [`setup_with_full_state`] that overrides the manager's
/// detach TTL. Used by reconnect tests so the TTL-expiry path runs in
/// well under a second of wall clock instead of the production 30s.
async fn setup_with_full_state_short_ttl(
    pool: PgPool,
    probe: Arc<dyn SshHostKeyProbe>,
    auth_check: Arc<SshAuthCheckService>,
    pty_bridge: Arc<dyn SshPtyBridge>,
    detach_ttl: std::time::Duration,
) -> (Router, UserId, String) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager_with_short_ttl(&db, detach_ttl);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check,
        pty_bridge,
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    (router(state), user_id, cookie)
}

/// Build a `TerminalSessionManager` wired to the same Postgres pool the
/// router will use. Each test gets its own manager — registry state is
/// per-test, which matches production semantics (the registry is not
/// durable; a backend restart drops it).
fn test_terminal_manager(db: &Db) -> Arc<TerminalSessionManager> {
    use relayterm_core::repository::{SessionEventRepository, TerminalSessionRepository};
    Arc::new(TerminalSessionManager::new(
        Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
        Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
    ))
}

/// Like [`test_terminal_manager`] but with a sub-second detach TTL so
/// the timer-driven close path can be exercised without burning real
/// wall-clock budget. Production code MUST construct via the
/// SPEC-pinned default (`TerminalSessionManager::new`).
fn test_terminal_manager_with_short_ttl(
    db: &Db,
    ttl: std::time::Duration,
) -> Arc<TerminalSessionManager> {
    use relayterm_core::repository::{SessionEventRepository, TerminalSessionRepository};
    Arc::new(TerminalSessionManager::with_detach_ttl(
        Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
        Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
        ttl,
    ))
}

/// Like [`test_terminal_manager`] but with a custom per-user live-PTY
/// cap (Phase 1B.1 quota). Used by integration tests that need to
/// drive the `429 too_many_sessions` refusal path without burning the
/// full default `8` sessions.
fn test_terminal_manager_with_max_live_per_user(
    db: &Db,
    cap: std::num::NonZeroU32,
) -> Arc<TerminalSessionManager> {
    use relayterm_core::repository::{SessionEventRepository, TerminalSessionRepository};
    Arc::new(
        TerminalSessionManager::new(
            Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
            Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
        )
        .with_max_live_pty_per_user(cap),
    )
}

/// Like [`test_terminal_manager`] but with a custom per-user starting-burst
/// cap (Phase 1B.2a quota). Used by integration tests that need to
/// drive the `429 too_many_starting_sessions` refusal path. The live
/// cap stays at the default so the tests exercise the starting-quota
/// branch in isolation.
fn test_terminal_manager_with_max_starting_per_user(
    db: &Db,
    cap: std::num::NonZeroU32,
) -> Arc<TerminalSessionManager> {
    use relayterm_core::repository::{SessionEventRepository, TerminalSessionRepository};
    Arc::new(
        TerminalSessionManager::new(
            Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
            Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
        )
        .with_max_starting_per_user(cap),
    )
}

/// Like [`test_terminal_manager`] but with a custom deployment-wide
/// live-PTY cap (Phase 1B.2b quota). The per-user live cap is set
/// high enough that the deployment cap is the limiting factor in the
/// tests that drive `429 too_many_sessions_deployment`.
fn test_terminal_manager_with_max_live_per_deployment(
    db: &Db,
    cap: std::num::NonZeroU32,
) -> Arc<TerminalSessionManager> {
    use relayterm_core::repository::{SessionEventRepository, TerminalSessionRepository};
    Arc::new(
        TerminalSessionManager::new(
            Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
            Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
        )
        .with_max_live_pty_per_deployment(cap),
    )
}

/// Mirrors [`setup_with_max_live_per_user`] but overrides the
/// deployment-wide live-PTY cap (Phase 1B.2b quota). Tests that drive
/// the cross-owner refusal path use this so the deployment cap is the
/// limiting factor while every per-user cap stays at its generous
/// default.
async fn setup_with_max_live_per_deployment(
    pool: PgPool,
    cap: std::num::NonZeroU32,
) -> (Router, UserId, String) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager_with_max_live_per_deployment(&db, cap);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    (router(state), user_id, cookie)
}

/// Vault service backed by a deterministic test master key. Tests that
/// don't exercise the vault still need *some* vault instance because the
/// API state requires it for the `POST /ssh-identities` route.
fn test_vault() -> VaultService {
    VaultService::new(relayterm_vault::VaultMasterKey::from_bytes([0x77u8; 32]))
}

/// Probe used by tests that don't go through the preflight surface.
/// Returns an unreachable error if it ever IS called — that's a test
/// bug, not a real probe failure.
fn default_probe() -> Arc<dyn SshHostKeyProbe> {
    Arc::new(FailingProbe)
}

struct FailingProbe;

#[async_trait]
impl SshHostKeyProbe for FailingProbe {
    async fn capture_host_key(&self, _target: ProbeTarget) -> Result<CapturedHostKey, ProbeError> {
        // Surface as Transport so a test that hits this by mistake fails
        // with a 502 instead of a misleading 500.
        Err(ProbeError::Transport)
    }
}

/// Auth checker used by tests that don't go through the auth-check
/// surface. Returns ConnectionFailed so an accidental hit produces a
/// recognisable wire status instead of a hung await.
fn default_auth_checker() -> Arc<dyn SshAuthChecker> {
    Arc::new(FailingAuthChecker)
}

struct FailingAuthChecker;

#[async_trait]
impl SshAuthChecker for FailingAuthChecker {
    async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
        Err(ProbeError::Transport)
    }
}

/// Fake auth checker: returns a configured outcome and records every call.
/// Used to exercise the auth-check route without a real SSH peer.
#[derive(Clone)]
struct FakeAuthChecker {
    captured: CapturedHostKey,
    kind: AuthAttemptKind,
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

/// Snapshot of one auth-check call. The PEM is held in a `Zeroizing`
/// buffer so the test-side copy of the decrypted private key is wiped
/// when the call record drops, matching the discipline the production
/// code path keeps. Tests that need to assert on the PEM shape do so
/// against this buffer — they MUST NOT clone it into a plain `Vec<u8>`.
#[derive(Clone, Debug)]
struct RecordedCall {
    hostname: String,
    port: u16,
    username: String,
    accept_pin_count: usize,
    private_key_pem: Zeroizing<Vec<u8>>,
}

impl FakeAuthChecker {
    fn new(captured: CapturedHostKey, kind: AuthAttemptKind) -> Self {
        Self {
            captured,
            kind,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl SshAuthChecker for FakeAuthChecker {
    async fn run(&self, target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
        self.calls.lock().unwrap().push(RecordedCall {
            hostname: target.hostname.clone(),
            port: target.port,
            username: target.username.clone(),
            accept_pin_count: target.accept_pins.len(),
            private_key_pem: Zeroizing::new(target.private_key_pem.to_vec()),
        });
        Ok(AuthCheckOutcome {
            captured: self.captured.clone(),
            kind: self.kind,
        })
    }
}

/// Auth checker that always errors — exercises the ConnectionFailed path.
struct ErroringAuthChecker(ProbeError);

#[async_trait]
impl SshAuthChecker for ErroringAuthChecker {
    async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
        Err(match &self.0 {
            ProbeError::Unreachable => ProbeError::Unreachable,
            ProbeError::Timeout => ProbeError::Timeout,
            ProbeError::BadHostKey => ProbeError::BadHostKey,
            ProbeError::Transport => ProbeError::Transport,
        })
    }
}

/// Auth checker that sleeps for a configured duration. Lets a test
/// exercise the outer-timeout guard the service wraps around `run`.
struct SlowAuthChecker {
    delay: std::time::Duration,
    captured: CapturedHostKey,
    kind: AuthAttemptKind,
}

#[async_trait]
impl SshAuthChecker for SlowAuthChecker {
    async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
        tokio::time::sleep(self.delay).await;
        Ok(AuthCheckOutcome {
            captured: self.captured.clone(),
            kind: self.kind,
        })
    }
}

/// Auth checker that signals when entered then blocks until released.
/// Lets a saturation test know — without a sleep — that the only
/// available permit is now held by the in-flight call.
struct BlockingAuthChecker {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    captured: CapturedHostKey,
    kind: AuthAttemptKind,
}

#[async_trait]
impl SshAuthChecker for BlockingAuthChecker {
    async fn run(&self, _target: AuthCheckTarget) -> Result<AuthCheckOutcome, ProbeError> {
        // The service acquires the semaphore BEFORE calling `run`, so
        // notifying here is the canonical "permit is held" signal — no
        // sleep, no polling.
        self.entered.notify_one();
        self.release.notified().await;
        Ok(AuthCheckOutcome {
            captured: self.captured.clone(),
            kind: self.kind,
        })
    }
}

/// Outcome a [`FakePtyBridge`] returns from `start`. `SshPtyError` is
/// not `Clone` (transport variants wrap non-cloneable upstream errors),
/// so the failure variant carries a small sentinel the bridge maps back
/// to a fresh error on each call.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
enum FakePtyOutcome {
    /// Start succeeds; hand back a [`FakePtyHandleRecord`] the test can
    /// drive through `inject_output` and assert against for input/resize.
    Success,
    /// Start fails with the configured error category. Used to exercise
    /// the API's typed error mapping (host_key_not_trusted, auth_failed,
    /// transport, etc.).
    Failure(FakePtyFailure),
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
enum FakePtyFailure {
    InvalidIdentity,
    Transport,
    HostKeyNotTrusted,
    AuthenticationFailed,
    PtyStartFailed,
}

impl FakePtyFailure {
    fn into_error(self) -> SshPtyError {
        match self {
            Self::InvalidIdentity => SshPtyError::InvalidIdentity,
            Self::Transport => SshPtyError::Transport(ProbeError::Transport),
            Self::HostKeyNotTrusted => SshPtyError::HostKeyNotTrusted,
            Self::AuthenticationFailed => SshPtyError::AuthenticationFailed,
            Self::PtyStartFailed => SshPtyError::PtyStartFailed,
        }
    }
}

/// Recorded interactions with one fake PTY handle. Held behind `Arc` so
/// the test side and the SSH-side `SshPtyHandle` impl can share it.
#[allow(dead_code)]
struct FakePtyHandleRecord {
    inputs: Mutex<Vec<Vec<u8>>>,
    resizes: Mutex<Vec<(u16, u16)>>,
    closed: std::sync::atomic::AtomicBool,
    /// Sender into the bridge's `output_rx`, owned by the record so the
    /// test can `inject_output` after the start call returns. Wrapped
    /// in `Mutex<Option<...>>` so tests can also explicitly drop the
    /// sender to simulate transport teardown.
    output_tx: Mutex<Option<tokio::sync::mpsc::Sender<SshPtyEvent>>>,
}

#[allow(dead_code)]
impl FakePtyHandleRecord {
    /// Push raw PTY bytes into the bridge's `output_rx` so the manager's
    /// forwarder fans them out to attached WebSockets.
    async fn inject_output(&self, bytes: Vec<u8>) {
        let tx = {
            let guard = self.output_tx.lock().unwrap();
            guard.as_ref().cloned()
        };
        if let Some(tx) = tx {
            let _ = tx.send(SshPtyEvent::Output(bytes)).await;
        }
    }

    fn input_log(&self) -> Vec<Vec<u8>> {
        self.inputs.lock().unwrap().clone()
    }

    fn resize_log(&self) -> Vec<(u16, u16)> {
        self.resizes.lock().unwrap().clone()
    }

    /// `true` once the manager (or test) called `SshPtyHandle::close`
    /// on this handle. Used by detached-session TTL tests to assert
    /// the bridge stays alive within the TTL window.
    fn was_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Adapter exposing a [`FakePtyHandleRecord`] as an [`SshPtyHandle`] for
/// the SSH bridge contract. Held behind `Box` inside `SshPtyStart`.
struct FakePtyHandleAdapter(Arc<FakePtyHandleRecord>);

#[async_trait]
impl SshPtyHandle for FakePtyHandleAdapter {
    async fn write_input(&self, bytes: Vec<u8>) -> Result<(), SshPtyError> {
        if self.0.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SshPtyError::BridgeClosed);
        }
        self.0.inputs.lock().unwrap().push(bytes);
        Ok(())
    }
    async fn resize(&self, cols: u16, rows: u16) -> Result<(), SshPtyError> {
        if self.0.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SshPtyError::BridgeClosed);
        }
        self.0.resizes.lock().unwrap().push((cols, rows));
        Ok(())
    }
    async fn close(&self) {
        self.0
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Drop the sender so the manager's forwarder sees the channel
        // close and tears down. Mirrors what the russh impl does on
        // shutdown.
        let _ = self.0.output_tx.lock().unwrap().take();
    }
}

/// Recorded inputs to one `start` call.
#[derive(Clone, Debug)]
#[allow(dead_code)]
struct RecordedPtyTarget {
    hostname: String,
    port: u16,
    username: String,
    accept_pin_count: usize,
    cols: u16,
    rows: u16,
    /// Length of the decrypted PEM the bridge received. The actual bytes
    /// are NEVER cloned out of the Zeroizing buffer the test side keeps
    /// — this length is operator-facing and lets a test assert that the
    /// vault-decrypted PEM did reach the bridge.
    pem_len: usize,
}

/// Fake bridge that records every `start` call and hands back a fake
/// handle the test can drive. The configured `outcome` decides whether
/// `start` succeeds or returns a typed error.
struct FakePtyBridge {
    outcome: Mutex<FakePtyOutcome>,
    records: Mutex<Vec<RecordedPtyTarget>>,
    handles: Mutex<Vec<Arc<FakePtyHandleRecord>>>,
}

#[allow(dead_code)]
impl FakePtyBridge {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(FakePtyOutcome::Success),
            records: Mutex::new(Vec::new()),
            handles: Mutex::new(Vec::new()),
        })
    }

    fn with_outcome(outcome: FakePtyOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(outcome),
            records: Mutex::new(Vec::new()),
            handles: Mutex::new(Vec::new()),
        })
    }

    fn last_handle(&self) -> Option<Arc<FakePtyHandleRecord>> {
        self.handles.lock().unwrap().last().cloned()
    }

    fn records(&self) -> Vec<RecordedPtyTarget> {
        self.records.lock().unwrap().clone()
    }

    fn call_count(&self) -> usize {
        self.records.lock().unwrap().len()
    }
}

#[async_trait]
impl SshPtyBridge for FakePtyBridge {
    async fn start(&self, target: SshPtyTarget) -> Result<SshPtyStart, SshPtyError> {
        let SshPtyTarget {
            config,
            private_key_pem,
        } = target;
        self.records.lock().unwrap().push(RecordedPtyTarget {
            hostname: config.hostname.clone(),
            port: config.port,
            username: config.username.clone(),
            accept_pin_count: config.accept_pins.len(),
            cols: config.cols,
            rows: config.rows,
            pem_len: private_key_pem.len(),
        });
        // Drop the PEM right after we've recorded its length. Any
        // assertion about the bytes happens against `record.pem_len` —
        // the plaintext never leaves this scope.
        drop(private_key_pem);

        let outcome = *self.outcome.lock().unwrap();
        match outcome {
            FakePtyOutcome::Success => {
                let (output_tx, output_rx) = tokio::sync::mpsc::channel(64);
                let record = Arc::new(FakePtyHandleRecord {
                    inputs: Mutex::new(Vec::new()),
                    resizes: Mutex::new(Vec::new()),
                    closed: std::sync::atomic::AtomicBool::new(false),
                    output_tx: Mutex::new(Some(output_tx)),
                });
                self.handles.lock().unwrap().push(record.clone());
                Ok(SshPtyStart {
                    handle: Box::new(FakePtyHandleAdapter(record)),
                    output_rx,
                    driver: None,
                })
            }
            FakePtyOutcome::Failure(failure) => Err(failure.into_error()),
        }
    }
}

fn default_pty_bridge() -> Arc<dyn SshPtyBridge> {
    FakePtyBridge::new() as Arc<dyn SshPtyBridge>
}

/// Probe that returns a configured fingerprint and records every call.
/// Used to exercise the preflight + trust paths without a real SSH peer.
#[derive(Clone)]
struct FakeProbe {
    captured: CapturedHostKey,
    calls: Arc<Mutex<Vec<ProbeTarget>>>,
}

impl FakeProbe {
    fn new(captured: CapturedHostKey) -> Self {
        Self {
            captured,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl SshHostKeyProbe for FakeProbe {
    async fn capture_host_key(&self, target: ProbeTarget) -> Result<CapturedHostKey, ProbeError> {
        self.calls.lock().unwrap().push(target);
        Ok(self.captured.clone())
    }
}

/// Probe that always errors — exercises the BadGateway path.
struct ErrorProbe(ProbeError);

#[async_trait]
impl SshHostKeyProbe for ErrorProbe {
    async fn capture_host_key(&self, _target: ProbeTarget) -> Result<CapturedHostKey, ProbeError> {
        Err(match &self.0 {
            ProbeError::Unreachable => ProbeError::Unreachable,
            ProbeError::Timeout => ProbeError::Timeout,
            ProbeError::BadHostKey => ProbeError::BadHostKey,
            ProbeError::Transport => ProbeError::Transport,
        })
    }
}

fn captured_for_test(fingerprint: &str) -> CapturedHostKey {
    CapturedHostKey {
        key_type: SshKeyType::Ed25519,
        fingerprint_sha256: fingerprint.to_owned(),
        public_key: b"ssh-ed25519 AAAA-host-key".to_vec(),
    }
}

async fn read_body(resp: axum::response::Response) -> Value {
    let bytes = to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("read body");
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).expect("body is valid JSON")
}

/// Build a `POST` request that authenticates with the supplied session
/// cookie token AND carries the test allow-listed `Origin` so writes
/// pass the shared `CsrfGuard`. **The default for every protected
/// browser-write test path** — auth + Origin together so a bad call to
/// the helper can't accidentally pass either guard half.
fn json_post(uri: &str, body: Value, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, TEST_AUTH_ORIGIN)
        .header(header::COOKIE, format!("relayterm_session={cookie}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Build an authenticated `GET` request for `uri`. GETs don't need a
/// CSRF guard, so the `Origin` header is omitted intentionally — the
/// helper never makes a request "browser-write-shaped" beyond what the
/// route demands.
fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, format!("relayterm_session={cookie}"))
        .body(Body::empty())
        .unwrap()
}

/// Variant of [`json_post`] that omits the cookie. Used by tests that
/// pin the "missing session cookie → 401" wire contract.
fn json_post_no_auth(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, TEST_AUTH_ORIGIN)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Variant of [`get`] that omits the cookie. Used by tests that pin
/// the "missing session cookie → 401" wire contract on GET routes.
fn get_no_auth(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// Variant of [`json_post`] that overrides the `Origin` header with
/// `origin`. Used by tests that pin the "bad Origin → 403 before body
/// parse" wire contract on browser-write routes.
fn json_post_with_origin(uri: &str, body: Value, cookie: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, origin)
        .header(header::COOKIE, format!("relayterm_session={cookie}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

// ----------------------------------------------------------------------
// Hosts
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_success(pool: PgPool) {
    let (app, _user, cookie) = setup(pool).await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Bastion",
                "hostname": "bastion.example.com",
                "port": 2222,
                "default_username": "ops",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    assert_eq!(body["display_name"], "Bastion");
    assert_eq!(body["hostname"], "bastion.example.com");
    assert_eq!(body["port"], 2222);
    assert_eq!(body["default_username"], "ops");
    assert!(body["id"].is_string(), "id is serialized as string UUID");
    assert!(
        body.get("owner_id").is_none(),
        "host response should not expose owner_id"
    );

    // Listing surfaces the row we just created.
    let listed = app
        .clone()
        .oneshot(get("/api/v1/hosts", &cookie))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let arr = read_body(listed).await;
    assert_eq!(arr.as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_default_port_22(pool: PgPool) {
    let (app, _, cookie) = setup(pool).await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Default-port host",
                "hostname": "h.example.com",
                "default_username": "deploy",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    assert_eq!(body["port"], 22);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_invalid_input_returns_400(pool: PgPool) {
    let (app, _, cookie) = setup(pool).await;

    // hostname has whitespace, which `validate_hostname` rejects.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Bad",
                "hostname": "bad host",
                "default_username": "ops",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");

    // port out of range.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Bad",
                "hostname": "h.example.com",
                "port": 70_000,
                "default_username": "ops",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // empty default_username.
    let resp = app
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Bad",
                "hostname": "h.example.com",
                "default_username": "",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn get_host_unknown_id_returns_404(pool: PgPool) {
    let (app, _, cookie) = setup(pool).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(get(&format!("/api/v1/hosts/{bogus}"), &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
}

/// A `GET /api/v1/hosts/:id` for a host owned by a different user must be
/// indistinguishable from a genuine 404 — same status, same body. Otherwise
/// an attacker can probe for the existence of other users' resources by id.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn get_host_owned_by_other_user_returns_indistinguishable_404(pool: PgPool) {
    // Provision a foreign user and a host they own, directly via the
    // repository (the API is bound to a different dev user).
    let other_user = create_user(&pool, "other").await;
    let foreign_host = PgHostRepository::new(pool.clone())
        .create(CreateHost {
            owner_id: other_user,
            display_name: validate_host_display_name("Other-user host").unwrap(),
            hostname: validate_hostname("other.example.com").unwrap(),
            port: validate_ssh_port(22).unwrap(),
            default_username: validate_ssh_username("ops").unwrap(),
        })
        .await
        .unwrap();

    let (app, _user, cookie) = setup(pool).await;

    // Baseline: a totally bogus id returns 404 with the canonical body.
    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/hosts/{bogus}"), &cookie))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    // Same id-shape, different owner — must produce the same response.
    let resp = app
        .oneshot(get(&format!("/api/v1/hosts/{}", foreign_host.id), &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "cross-user 404 must be byte-identical to a genuine 404"
    );
    assert_eq!(body["error"]["code"], "not_found");
}

/// Without a session cookie, every protected app route MUST return 401
/// with the canonical error envelope. The body must NOT leak operator-
/// facing detail like the auth-mode plumbing or session-state strings.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn protected_hosts_routes_return_401_without_session_cookie(pool: PgPool) {
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // GET is auth-guarded.
    let resp = app
        .clone()
        .oneshot(get_no_auth("/api/v1/hosts"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    // The wire body must not echo any operator-facing detail; the static
    // "unauthorized" message is all the client gets, regardless of why.
    assert_eq!(body["error"]["message"], "unauthorized");
    assert!(
        !body.to_string().contains("session"),
        "401 body must not leak session-state detail: {body}"
    );

    // POST is also guarded — body never reaches the validator.
    let resp = app
        .oneshot(json_post_no_auth(
            "/api/v1/hosts",
            json!({
                "display_name": "x",
                "hostname": "h.example.com",
                "default_username": "deploy",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["message"], "unauthorized");
}

/// `POST /api/v1/hosts` with a bad `Origin` MUST return `403
/// csrf_origin_mismatch` BEFORE the body is parsed and BEFORE any DB
/// row is written. Even with a valid session cookie, a request whose
/// `Origin` is not in the allow-list is rejected by the shared
/// `CsrfGuard` ahead of `Json<...>`. This is the wire-level proof
/// of the "bad Origin → 403, no DB mutation" contract for the
/// hosts-write surface; the same guarantee applies to every other
/// browser-write route the migration touched.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_bad_origin_returns_403_before_body_parse(pool: PgPool) {
    let (app, _user, cookie) = setup(pool.clone()).await;

    // Malformed JSON body — would normally produce 400/422 if the
    // request reached the body extractor. The Origin guard sits ahead
    // of `Json<...>` so the bytes never reach the parser.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/hosts")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://evil.example.com")
        .header(header::COOKIE, format!("relayterm_session={cookie}"))
        .body(Body::from("{ this is not valid JSON"))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");
    assert_eq!(body["error"]["message"], "forbidden");

    // No host rows persisted.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hosts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "bad-Origin must not create rows");
}

/// `POST /api/v1/hosts` without an `Origin` header MUST return 403
/// `csrf_origin_mismatch`. Pinning the missing-header case so a
/// future "treat missing as same-origin" change is a deliberate one.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_missing_origin_returns_403(pool: PgPool) {
    let (app, _user, cookie) = setup(pool.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/hosts")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("relayterm_session={cookie}"))
        .body(Body::from(
            json!({
                "display_name": "x",
                "hostname": "h.example.com",
                "default_username": "deploy",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");
}

// ----------------------------------------------------------------------
// SSH identities
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_returns_public_metadata_only(pool: PgPool) {
    let (app, _, cookie) = setup(pool).await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "homelab-admin",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();

    // No secret material on the wire — neither the field name nor any
    // bytes that could only have come from the plaintext PEM.
    assert!(
        !raw.contains("encrypted_private_key"),
        "POST response must not expose encrypted_private_key: {raw}"
    );
    assert!(
        !raw.contains("BEGIN OPENSSH PRIVATE KEY"),
        "POST response must not contain a plaintext PEM: {raw}"
    );
    assert!(
        !raw.contains("private_key"),
        "POST response must not contain any private_key field: {raw}"
    );

    let body: Value = serde_json::from_str(&raw).unwrap();
    assert!(
        body["id"].is_string(),
        "id should be present as UUID string"
    );
    assert_eq!(body["name"], "homelab-admin");
    assert_eq!(body["key_type"], "ed25519");
    let public_key = body["public_key"].as_str().expect("public_key string");
    assert!(
        public_key.starts_with("ssh-ed25519 "),
        "public_key should be an OpenSSH ed25519 line: {public_key}"
    );
    assert!(
        public_key.ends_with(" homelab-admin"),
        "public_key should bake the user-supplied name as the OpenSSH comment: {public_key}"
    );
    let fp = body["fingerprint_sha256"].as_str().expect("fingerprint");
    assert!(
        fp.starts_with("SHA256:"),
        "fingerprint should be SHA256:<base64>: {fp}"
    );
    assert!(
        body.get("owner_id").is_none(),
        "ssh identity response should not expose owner_id"
    );

    // Subsequent GET also omits the encrypted blob.
    let id = body["id"].as_str().unwrap();
    let get_resp = app
        .oneshot(get(&format!("/api/v1/ssh-identities/{id}"), &cookie))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let bytes = get_resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!raw.contains("encrypted_private_key"));
    assert!(!raw.contains("BEGIN OPENSSH PRIVATE KEY"));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_invalid_key_type_returns_400(pool: PgPool) {
    let (app, _, cookie) = setup(pool).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "primary",
                "key_type": "invalid-algo",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
    assert_eq!(
        body["error"]["message"],
        "unsupported key_type \"invalid-algo\""
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_rsa_returns_400_unsupported(pool: PgPool) {
    // Ed25519 is the only generator wired up today; RSA and friends are a
    // future slice. Unknown tags and known-but-unsupported tags share one
    // canonical 400 shape so clients can match on it without caring which
    // gate caught them.
    let (app, _, cookie) = setup(pool).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "primary",
                "key_type": "rsa",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
    assert_eq!(body["error"]["message"], "unsupported key_type \"rsa\"");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_empty_name_returns_400(pool: PgPool) {
    let (app, _, cookie) = setup(pool).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_returns_401_without_session_cookie(pool: PgPool) {
    // No session cookie → AuthenticatedUser extractor short-circuits
    // with 401 BEFORE any vault work happens. The request body never
    // reaches the vault.
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .oneshot(json_post_no_auth(
            "/api/v1/ssh-identities",
            json!({"name": "should-never-be-created"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(body["error"]["message"], "unauthorized");

    // And nothing was persisted.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ssh_identities")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "401 must not create rows");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_returns_503_when_vault_disabled(pool: PgPool) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({"name": "primary"}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    // Static wire body — no operator-facing detail leaked.
    assert_eq!(body["error"]["message"], "service unavailable");

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM ssh_identities")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_persists_encrypted_blob(pool: PgPool) {
    // After a successful POST the row exists, the public key matches the
    // API response, and the stored ciphertext does NOT contain the
    // OpenSSH PEM header — proving the blob is actually encrypted.
    let (app, _, cookie) = setup(pool.clone()).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({"name": "store-check"}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    let id_str = body["id"].as_str().unwrap();
    let id_uuid: uuid::Uuid = id_str.parse().unwrap();

    let row: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT public_key, encrypted_private_key FROM ssh_identities WHERE id = $1",
    )
    .bind(id_uuid)
    .fetch_one(&pool)
    .await
    .unwrap();

    let public_key_text = std::str::from_utf8(&row.0).unwrap();
    assert!(public_key_text.starts_with("ssh-ed25519 "));
    let needle = b"BEGIN OPENSSH PRIVATE KEY";
    assert!(
        !row.1.windows(needle.len()).any(|w| w == needle),
        "stored encrypted_private_key must not contain plaintext PEM marker"
    );
    // The envelope magic should be present at the front.
    assert_eq!(&row.1[..4], b"RTV1");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn list_ssh_identities_omits_encrypted_private_key(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;

    PgSshIdentityRepository::new(pool)
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "primary".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 AAAA-public".to_vec(),
            encrypted_private_key: PRIVATE_KEY_MARKER.to_vec(),
            fingerprint_sha256: "SHA256:abcd".to_owned(),
        })
        .await
        .expect("seed identity");

    // List endpoint.
    let resp = app
        .clone()
        .oneshot(get("/api/v1/ssh-identities", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        !raw.contains("encrypted_private_key"),
        "list response must not contain encrypted_private_key field: {raw}"
    );
    assert!(
        !raw.contains("REDACT-MARKER-API-9F2B"),
        "list response must not echo private key bytes: {raw}"
    );
    let arr: Value = serde_json::from_str(&raw).unwrap();
    let item = &arr.as_array().unwrap()[0];
    assert_eq!(item["name"], "primary");
    assert_eq!(item["key_type"], "ed25519");
    assert_eq!(item["public_key"], "ssh-ed25519 AAAA-public");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn get_ssh_identity_omits_encrypted_private_key(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let identity = PgSshIdentityRepository::new(pool)
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "primary".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 PUB".to_vec(),
            encrypted_private_key: PRIVATE_KEY_MARKER.to_vec(),
            fingerprint_sha256: "SHA256:cafe".to_owned(),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(get(
            &format!("/api/v1/ssh-identities/{}", identity.id),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!raw.contains("encrypted_private_key"));
    assert!(!raw.contains("REDACT-MARKER-API-9F2B"));
}

// ----------------------------------------------------------------------
// Server profiles
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_server_profile_success(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let identity = PgSshIdentityRepository::new(pool)
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "primary".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 PUB".to_vec(),
            encrypted_private_key: b"opaque".to_vec(),
            fingerprint_sha256: "SHA256:profile-fp".to_owned(),
        })
        .await
        .unwrap();

    // Create the host through the API to keep the path realistic.
    let host_resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Prod",
                "hostname": "prod.example.com",
                "default_username": "deploy",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(host_resp.status(), StatusCode::CREATED);
    let host_body = read_body(host_resp).await;
    let host_id = host_body["id"].as_str().unwrap().to_owned();

    let resp = app
        .oneshot(json_post(
            "/api/v1/server-profiles",
            json!({
                "name": "Prod / us-east-1",
                "host_id": host_id,
                "ssh_identity_id": identity.id,
                "username_override": "root",
                "tags": ["prod", "us-east-1"],
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    assert_eq!(body["name"], "Prod / us-east-1");
    assert_eq!(body["host_id"], host_id);
    assert_eq!(body["username_override"], "root");
    assert_eq!(body["tags"], json!(["prod", "us-east-1"]));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_server_profile_missing_host_returns_404(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let identity = PgSshIdentityRepository::new(pool)
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "primary".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"pub".to_vec(),
            encrypted_private_key: b"opaque".to_vec(),
            fingerprint_sha256: "SHA256:missing-host".to_owned(),
        })
        .await
        .unwrap();

    let bogus_host = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            "/api/v1/server-profiles",
            json!({
                "name": "no-such-host",
                "host_id": bogus_host,
                "ssh_identity_id": identity.id,
                "tags": [],
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
    assert!(body["error"]["message"].as_str().unwrap().contains("host"));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_server_profile_missing_identity_returns_404(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool).await;

    let host_resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "host-1",
                "hostname": "h1.example.com",
                "default_username": "deploy",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    let host_body = read_body(host_resp).await;
    let host_id = host_body["id"].as_str().unwrap().to_owned();

    let bogus_identity = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            "/api/v1/server-profiles",
            json!({
                "name": "no-such-identity",
                "host_id": host_id,
                "ssh_identity_id": bogus_identity,
                "tags": [],
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ssh_identity")
    );
}

// ----------------------------------------------------------------------
// Preflight + trust-host-key
//
// These tests use a fake `SshHostKeyProbe` so the preflight surface can
// be exercised end-to-end without spinning up an SSH server. The vault
// path IS exercised — every test goes through `vault.decrypt_private_key`
// against a real vault-issued blob.
// ----------------------------------------------------------------------

/// Provision a profile owned by `user_id`, using a real vault-issued
/// SSH identity so the decrypt path runs end-to-end.
async fn make_owned_profile(
    pool: &PgPool,
    user_id: UserId,
    vault: &VaultService,
    name: &str,
    hostname: &str,
) -> relayterm_core::ids::ServerProfileId {
    let host = PgHostRepository::new(pool.clone())
        .create(CreateHost {
            owner_id: user_id,
            display_name: validate_host_display_name(name).unwrap(),
            hostname: validate_hostname(hostname).unwrap(),
            port: validate_ssh_port(22).unwrap(),
            default_username: validate_ssh_username("deploy").unwrap(),
        })
        .await
        .unwrap();

    let generated = vault
        .generate_ssh_identity(SshKeyType::Ed25519, name)
        .unwrap();
    let identity = PgSshIdentityRepository::new(pool.clone())
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: name.to_owned(),
            key_type: generated.key_type,
            public_key: generated.public_key_openssh,
            encrypted_private_key: generated.encrypted_private_key.into_bytes(),
            fingerprint_sha256: generated.fingerprint_sha256,
        })
        .await
        .unwrap();

    PgServerProfileRepository::new(pool.clone())
        .create(CreateServerProfile {
            owner_id: user_id,
            name: relayterm_core::validation::validate_profile_name(name).unwrap(),
            host_id: host.id,
            ssh_identity_id: identity.id,
            username_override: None,
            tags: vec![],
        })
        .await
        .unwrap()
        .id
}

async fn setup_with_fake_probe(
    pool: PgPool,
    fingerprint: &str,
) -> (Router, UserId, FakeProbe, String) {
    let probe = FakeProbe::new(captured_for_test(fingerprint));
    let probe_handle = probe.clone();
    let (app, user_id, cookie) = setup_with_probe(pool, Arc::new(probe)).await;
    (app, user_id, probe_handle, cookie)
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_unknown_when_no_known_host_entries(pool: PgPool) {
    let (app, user_id, probe, cookie) = setup_with_fake_probe(pool.clone(), "SHA256:fake-fp").await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "host-1.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["host_key_status"], "unknown");
    assert_eq!(body["host_key_type"], "ed25519");
    assert_eq!(body["host_key_fingerprint"], "SHA256:fake-fp");
    assert_eq!(body["port"], 22);
    assert_eq!(body["hostname"], "host-1.example.com");
    // No active pin exists when status is `unknown` — the field MUST
    // be null on this code path so the SPA never offers the replace
    // affordance for first-time-seen / revoked-and-reappearing keys.
    assert!(body["active_pin_fingerprint"].is_null());

    // No private-key material leaks via the preflight response.
    let raw = body.to_string();
    assert!(!raw.contains("encrypted_private_key"));
    assert!(!raw.contains("BEGIN OPENSSH PRIVATE KEY"));
    assert!(!raw.contains("private_key"));

    // Probe was actually called with the host's coordinates.
    let calls = probe.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].hostname, "host-1.example.com");
    assert_eq!(calls[0].port, 22);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_trusted_when_pinned_entry_matches(pool: PgPool) {
    let fp = "SHA256:trusted-fp";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "trusted.example.com",
    )
    .await;

    // Look up the host_id and pre-pin the fingerprint.
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-ed25519 AAAA-host-key".to_vec(),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["host_key_status"], "trusted");
    // The active pin matches the captured key, so the replace flow is
    // not applicable — the field MUST be null on the trusted path.
    assert!(body["active_pin_fingerprint"].is_null());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_changed_when_pinned_fingerprint_differs(pool: PgPool) {
    let new_fp = "SHA256:NEW-fp";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), new_fp).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "rotated.example.com",
    )
    .await;

    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:OLD-fp".to_owned(),
            public_key: b"ssh-ed25519 OLD-host-key".to_vec(),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["host_key_status"], "changed");
    assert_eq!(body["host_key_fingerprint"], new_fp);
    // Phase 4 enabler: changed preflight echoes the active pin's
    // fingerprint so the SPA can offer the replace flow without a
    // separate known-host-entries fetch (see
    // `docs/spec/host-key-replace.md` § R6 / Phase 4 notes).
    assert_eq!(body["active_pin_fingerprint"], "SHA256:OLD-fp");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_returns_502_on_probe_failure(pool: PgPool) {
    let (app, user_id, cookie) =
        setup_with_probe(pool.clone(), Arc::new(ErrorProbe(ProbeError::Unreachable))).await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "down.example.com").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "bad_gateway");
    // Static wire body — no ProbeError variant text leaks.
    assert_eq!(body["error"]["message"], "bad gateway");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_unknown_profile_returns_404(pool: PgPool) {
    let (app, _user_id, _probe, cookie) = setup_with_fake_probe(pool, "SHA256:never").await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
}

/// A preflight against another user's profile must produce a response
/// byte-identical to a genuine 404 — same status, same body. This is the
/// `API get_by_id ownership` lesson from AGENTS.md applied to preflight.
///
/// **CSRF guard sanity:** the request goes through `json_post`, which
/// injects the test allow-listed `Origin`, so `CsrfGuard` passes
/// cleanly. The 404 we observe comes from the ownership filter
/// downstream of the guard — not from a CSRF rejection.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_foreign_owned_profile_returns_indistinguishable_404(pool: PgPool) {
    // Provision a profile owned by ANOTHER user.
    let other_user = create_user(&pool, "other").await;
    let foreign_id = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign",
        "foreign.example.com",
    )
    .await;

    let (app, _user, _probe, cookie) = setup_with_fake_probe(pool, "SHA256:never").await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{foreign_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "cross-user preflight 404 must match a genuine 404"
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn trust_host_key_records_pinned_entry_when_expected_matches(pool: PgPool) {
    let fp = "SHA256:trust-me";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "trustme.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["host_key_fingerprint"], fp);
    assert_eq!(body["host_key_type"], "ed25519");
    assert!(body["trusted_at"].is_string());
    assert!(body["known_host_entry_id"].is_string());

    // The entry exists in DB and is trusted.
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(profile.host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fingerprint_sha256, fp);
    assert!(entries[0].trusted_at.is_some());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn trust_host_key_rejects_when_expected_fingerprint_does_not_match(pool: PgPool) {
    // Probe captures `actual-fp`; caller submits `stale-fp`. The route
    // must NOT pin anything.
    let (app, user_id, _probe, cookie) =
        setup_with_fake_probe(pool.clone(), "SHA256:actual-fp").await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "stale.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": "SHA256:stale-fp" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");

    // No entry persisted.
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(profile.host_id)
        .await
        .unwrap();
    assert!(
        entries.is_empty(),
        "mismatched expected fingerprint must NOT auto-pin"
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn trust_host_key_does_not_overwrite_a_changed_pinned_key(pool: PgPool) {
    // An ed25519 entry with fingerprint OLD is pinned. The host now
    // presents NEW. Even if the caller posts NEW as their expected
    // fingerprint, the route must refuse to pin (the classifier's
    // `Changed` verdict wins).
    let new_fp = "SHA256:NEW-fp";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), new_fp).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "changed.example.com",
    )
    .await;

    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:OLD-fp".to_owned(),
            public_key: b"ssh-ed25519 OLD".to_vec(),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": new_fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");

    // Original (OLD) entry still the only one — NEW was NOT silently pinned.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(profile.host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fingerprint_sha256, "SHA256:OLD-fp");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn trust_host_key_rejects_malformed_fingerprint(pool: PgPool) {
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), "SHA256:any").await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "any.example.com").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": "MD5:not-supported" }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn trust_host_key_is_idempotent_for_already_trusted_fingerprint(pool: PgPool) {
    let fp = "SHA256:idempotent-fp";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "idem.example.com").await;

    // First trust.
    let r1 = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let body1 = read_body(r1).await;
    let id1 = body1["known_host_entry_id"].as_str().unwrap().to_owned();
    let trusted_at_1 = body1["trusted_at"].as_str().unwrap().to_owned();

    // Second trust with the same fingerprint — must succeed and return
    // the same row id; trusted_at preserved.
    let r2 = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let body2 = read_body(r2).await;
    assert_eq!(body2["known_host_entry_id"].as_str().unwrap(), id1);
    assert_eq!(body2["trusted_at"].as_str().unwrap(), trusted_at_1);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_returns_503_when_vault_disabled(pool: PgPool) {
    // Without a vault, the route can't decrypt the identity. Must 503.
    let user_id = create_user(&pool, "dev").await;
    let probe = FakeProbe::new(captured_for_test("SHA256:any"));
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(Arc::new(probe))),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // Need a profile to address. Build via direct repos using a fake
    // identity row (no vault available, so create the row directly with
    // opaque bytes — this test never tries to decrypt them, just to reach
    // the vault-check guard).
    let host = PgHostRepository::new(pool.clone())
        .create(CreateHost {
            owner_id: user_id,
            display_name: validate_host_display_name("Vaultless").unwrap(),
            hostname: validate_hostname("v.example.com").unwrap(),
            port: validate_ssh_port(22).unwrap(),
            default_username: validate_ssh_username("deploy").unwrap(),
        })
        .await
        .unwrap();
    let identity = PgSshIdentityRepository::new(pool.clone())
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "vaultless".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 PUB".to_vec(),
            encrypted_private_key: b"opaque".to_vec(),
            fingerprint_sha256: format!("SHA256:vaultless-{}", uuid::Uuid::new_v4()),
        })
        .await
        .unwrap();
    let profile = PgServerProfileRepository::new(pool.clone())
        .create(CreateServerProfile {
            owner_id: user_id,
            name: relayterm_core::validation::validate_profile_name("vaultless").unwrap(),
            host_id: host.id,
            ssh_identity_id: identity.id,
            username_override: None,
            tags: vec![],
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{}/host-key-preflight", profile.id),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(body["error"]["message"], "service unavailable");
}

/// Helper: drive a row in the `known_host_entries` table to revoked.
/// Used to exercise the "revoked must never be silently re-trusted" rule.
///
/// Writes the FULL revoke metadata triple (`revoked_at`, `revoked_by`,
/// `revoked_reason_code`) so the row passes the
/// `known_host_entries_revoked_columns_set_together` CHECK introduced in
/// the Phase 1 host-key-replace migration. `operator_other` is the
/// canonical "no-narrative-attached" tag — using it here mirrors how a
/// future admin "revoke without replace" surface would back-fill an
/// orphaned revoke.
async fn revoke_entry(
    pool: &PgPool,
    entry_id: relayterm_core::ids::KnownHostEntryId,
    revoked_by: UserId,
) {
    sqlx::query(
        "UPDATE known_host_entries
         SET revoked_at = NOW(),
             revoked_by = $2,
             revoked_reason_code = 'operator_other'
         WHERE id = $1",
    )
    .bind(entry_id.into_uuid())
    .bind(revoked_by.into_uuid())
    .execute(pool)
    .await
    .expect("revoke entry");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn trust_host_key_refuses_to_re_trust_a_revoked_fingerprint(pool: PgPool) {
    // A revoked entry exists for fp=X. The server presents fp=X again.
    // The classifier (which filters revoked rows) returns Unknown; the
    // captured fingerprint matches the caller's expected fingerprint.
    // Without an explicit revoked-aware guard the route would silently
    // pin and "trust" the revoked key. This test pins down the contract:
    // 409, no row mutation.
    let fp = "SHA256:was-revoked";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "rev.example.com").await;

    // Seed a revoked entry with the same fingerprint the probe will return.
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let seeded = PgKnownHostEntryRepository::new(pool.clone())
        .create(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    revoke_entry(&pool, seeded.id, user_id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");

    // Row still revoked; trusted_at NOT stamped.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(profile.host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, seeded.id);
    assert!(entries[0].revoked_at.is_some(), "row must remain revoked");
    assert!(
        entries[0].trusted_at.is_none(),
        "trust-host-key must NOT have stamped trusted_at on a revoked row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_treats_revoked_match_as_unknown(pool: PgPool) {
    // The classifier filters revoked rows out of `Trusted` — a revoked-
    // and-reappearing key surfaces as `unknown`, NOT `trusted`. The trust
    // route's separate guard then refuses to pin it; this test pins down
    // the read-side half of that contract so the wire signal is correct.
    let fp = "SHA256:revoked-key";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "rev2.example.com").await;

    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let seeded = PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    revoke_entry(&pool, seeded.id, user_id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(
        body["host_key_status"], "unknown",
        "a revoked match must NOT classify as trusted",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_response_message_does_not_overclaim_auth_or_session_readiness(pool: PgPool) {
    // The wire message must NOT imply that SSH authentication succeeded
    // or that a session can be opened. Pin down the actual phrasing for
    // each status so a future "helpful" rewording trips the test before
    // it reaches users.
    let (app, user_id, _probe, cookie) =
        setup_with_fake_probe(pool.clone(), "SHA256:scope-check").await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "scope.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("KEX-stage probe only"),
        "message must signal the KEX-only scope, got: {message}"
    );
    // Belt-and-braces: must not say things that imply auth/session
    // readiness was checked.
    let lower = message.to_lowercase();
    assert!(
        !lower.contains("authenticated") && !lower.contains("authentication succeeded"),
        "message must not imply authentication: {message}"
    );
    assert!(
        !lower.contains("session is ready") && !lower.contains("ready to use"),
        "message must not imply session readiness: {message}"
    );
}

// ----------------------------------------------------------------------
// Replace-host-key (revoke-and-replace)
//
// `POST /api/v1/server-profiles/:id/replace-host-key`. Atomically revokes
// the active pinned host key and trusts a new one in its place, with a
// strict-enum reason code and paired `host_key_revoked` +
// `host_key_accepted` audit rows. The route never auto-overwrites: the
// caller MUST pass both expected_old and expected_new fingerprints, and
// the freshly-captured probe fingerprint MUST match expected_new before
// any DB mutation. See `docs/spec/host-key-replace.md` § R1–R7.
// ----------------------------------------------------------------------

const REPLACE_OLD_FP: &str = "SHA256:replace-route-OLD";
const REPLACE_NEW_FP: &str = "SHA256:replace-route-NEW";

/// Provision a profile with an OLD active pin already trusted, returning
/// (profile_id, host_id, old_entry_id). The fake probe is configured to
/// return REPLACE_NEW_FP so a `host-key-preflight` would classify as
/// `changed` — this is the canonical setup for the replace flow.
async fn make_profile_with_active_pin(
    pool: &PgPool,
    user_id: UserId,
    name: &str,
    hostname: &str,
) -> (
    relayterm_core::ids::ServerProfileId,
    relayterm_core::ids::HostId,
    relayterm_core::ids::KnownHostEntryId,
) {
    let profile_id = make_owned_profile(pool, user_id, &test_vault(), name, hostname).await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let entry = PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: REPLACE_OLD_FP.to_owned(),
            public_key: b"ssh-ed25519 OLD-host-key".to_vec(),
        })
        .await
        .unwrap();
    (profile_id, profile.host_id, entry.id)
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_happy_path_revokes_old_and_trusts_new_with_paired_audit(pool: PgPool) {
    // The probe captures NEW. The caller posts (OLD, NEW, reason). The
    // route must:
    //   1. revoke the OLD row (revoked_at, revoked_by=caller, reason set,
    //      replaced_by_id pointing at the new row),
    //   2. insert a fresh trusted row for NEW,
    //   3. emit two audit rows (`host_key_revoked` then `host_key_accepted`)
    //      cross-linked via the counterparty entry id and fingerprint.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, host_id, old_entry_id) =
        make_profile_with_active_pin(&pool, user_id, "replace-happy", "replace-happy.example.com")
            .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "server_reinstalled",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["profile_id"].as_str().unwrap(), profile_id.to_string());
    assert_eq!(body["host_id"].as_str().unwrap(), host_id.to_string());
    assert_eq!(
        body["revoked_known_host_entry_id"].as_str().unwrap(),
        old_entry_id.to_string(),
    );
    assert_eq!(body["revoked_fingerprint"], REPLACE_OLD_FP);
    assert_eq!(body["trusted_fingerprint"], REPLACE_NEW_FP);
    assert!(body["trusted_known_host_entry_id"].is_string());
    let new_entry_id = body["trusted_known_host_entry_id"].as_str().unwrap();
    assert_ne!(new_entry_id, old_entry_id.to_string());
    assert!(body["trusted_at"].is_string());

    // Response carries no key material.
    let raw = body.to_string();
    for forbidden in [
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
        "public_key",
    ] {
        assert!(
            !raw.contains(forbidden),
            "replace response must not contain `{forbidden}`: {raw}",
        );
    }

    // Old row revoked with full metadata; new row trusted.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    let old_row = entries.iter().find(|e| e.id == old_entry_id).unwrap();
    let new_row = entries.iter().find(|e| e.id != old_entry_id).unwrap();
    assert!(old_row.revoked_at.is_some(), "old row must be revoked");
    assert_eq!(old_row.revoked_by, Some(user_id));
    assert_eq!(
        old_row.revoked_reason_code,
        Some(relayterm_core::known_host::KnownHostRevocationReason::ServerReinstalled),
    );
    assert_eq!(old_row.replaced_by_id, Some(new_row.id));
    assert!(new_row.trusted_at.is_some());
    assert!(new_row.revoked_at.is_none());
    assert_eq!(new_row.fingerprint_sha256, REPLACE_NEW_FP);

    // Paired audit rows. Both with actor_id = caller, both cross-linked.
    let audit = PgAuditEventRepository::new(pool.clone());
    let recent = audit.recent(50).await.unwrap();
    let revoked: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::HostKeyRevoked)
        .collect();
    let accepted: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::HostKeyAccepted)
        .collect();
    assert_eq!(
        revoked.len(),
        1,
        "expected one host_key_revoked: {recent:?}"
    );
    assert_eq!(
        accepted.len(),
        1,
        "expected one host_key_accepted: {recent:?}",
    );
    let revoked_event = revoked[0];
    let accepted_event = accepted[0];
    assert_eq!(revoked_event.actor_id, Some(user_id));
    assert_eq!(accepted_event.actor_id, Some(user_id));

    // Both payloads carry the host-anchored cross-link.
    let r = &revoked_event.payload;
    assert_eq!(r["host_id"].as_str().unwrap(), host_id.to_string());
    assert_eq!(
        r["known_host_entry_id"].as_str().unwrap(),
        old_entry_id.to_string(),
    );
    assert_eq!(
        r["replacement_known_host_entry_id"].as_str().unwrap(),
        new_row.id.to_string(),
    );
    assert_eq!(r["old_fingerprint"], REPLACE_OLD_FP);
    assert_eq!(r["new_fingerprint"], REPLACE_NEW_FP);
    assert_eq!(r["reason_code"], "server_reinstalled");
    assert_eq!(r["key_type"], "ed25519");
    assert_audit_payload_redacted(r, AuditEventKind::HostKeyRevoked);

    let a = &accepted_event.payload;
    assert_eq!(a["host_id"].as_str().unwrap(), host_id.to_string());
    assert_eq!(
        a["known_host_entry_id"].as_str().unwrap(),
        new_row.id.to_string(),
    );
    assert_eq!(
        a["replacement_known_host_entry_id"].as_str().unwrap(),
        old_entry_id.to_string(),
    );
    assert_eq!(a["old_fingerprint"], REPLACE_OLD_FP);
    assert_eq!(a["new_fingerprint"], REPLACE_NEW_FP);
    assert_eq!(a["reason_code"], "server_reinstalled");
    assert_eq!(a["key_type"], "ed25519");
    assert_audit_payload_redacted(a, AuditEventKind::HostKeyAccepted);

    // Pair-ordering: revoked is recorded BEFORE accepted (the spec says
    // "in order, in the same DB transaction" — newest-first list means
    // accepted appears earlier in `recent`).
    assert!(
        accepted_event.recorded_at >= revoked_event.recorded_at,
        "accepted audit row must be recorded at-or-after revoked",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_rejects_when_active_pin_does_not_match_expected_old(pool: PgPool) {
    // The active pin is OLD; the caller posts a different expected_old.
    // Repository serialises around the FOR UPDATE select; the route
    // collapses `no_active_pin` and `active_pin_mismatch` into one wire
    // reason. Either way: 409, no DB mutation, no audit row.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, host_id, old_entry_id) =
        make_profile_with_active_pin(&pool, user_id, "replace-old", "replace-old.example.com")
            .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": "SHA256:NOT-THE-ACTIVE-PIN",
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "host_key_rotated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("host_key") && msg.contains("active_pin_mismatch"),
        "expected active_pin_mismatch reason, got: {msg}",
    );

    // DB unchanged: original row still active, no new row, no audit.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, old_entry_id);
    assert!(entries[0].revoked_at.is_none());
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_rejects_when_no_active_pin_exists(pool: PgPool) {
    // Distinct subcase of active_pin_mismatch (per spec § R4 / R5):
    // the host has zero active trusted pins. The repository's
    // `FOR UPDATE` select returns zero rows and the constraint mapping
    // collapses it onto the same `active_pin_mismatch` wire reason —
    // pinned here so a future split into a separate `no_active_pin`
    // wire code is detected.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    // Build a profile WITHOUT any known_host_entries — `make_owned_profile`
    // does not seed pins; we deliberately do not call `record_trusted`.
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "replace-no-pin",
        "replace-no-pin.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let host_id = profile.host_id;
    assert!(
        PgKnownHostEntryRepository::new(pool.clone())
            .list_for_host(host_id)
            .await
            .unwrap()
            .is_empty(),
        "precondition: host must have zero pins for this test",
    );

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "host_key_rotated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("host_key") && msg.contains("active_pin_mismatch"),
        "no-active-pin subcase must collapse to active_pin_mismatch, got: {msg}",
    );

    // No row landed; no audit row landed.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert!(entries.is_empty(), "no rows must be inserted: {entries:?}");
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_concurrent_calls_with_same_expected_old_yield_one_winner(pool: PgPool) {
    // Race-safety smoke (spec § R8 backend-integration list): two
    // concurrent calls with the same `expected_old_fingerprint` must
    // produce exactly one success and exactly one
    // `409 active_pin_mismatch`. The repository's `FOR UPDATE` lock
    // serialises them; the loser sees the row already revoked AND the
    // active pin already advanced. No double-revoke; no double-trust.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, host_id, old_entry_id) =
        make_profile_with_active_pin(&pool, user_id, "replace-race", "replace-race.example.com")
            .await;

    let body = json!({
        "expected_old_fingerprint": REPLACE_OLD_FP,
        "expected_new_fingerprint": REPLACE_NEW_FP,
        "reason_code": "lab_target_recreated",
    });
    let uri = format!("/api/v1/server-profiles/{profile_id}/replace-host-key");
    let app_a = app.clone();
    let app_b = app;
    let body_a = body.clone();
    let body_b = body;
    let cookie_a = cookie.clone();
    let cookie_b = cookie;
    let uri_a = uri.clone();
    let uri_b = uri;

    let (resp_a, resp_b) = tokio::join!(
        async move {
            app_a
                .oneshot(json_post(&uri_a, body_a, &cookie_a))
                .await
                .unwrap()
        },
        async move {
            app_b
                .oneshot(json_post(&uri_b, body_b, &cookie_b))
                .await
                .unwrap()
        },
    );

    let mut statuses = [resp_a.status(), resp_b.status()];
    statuses.sort_by_key(StatusCode::as_u16);
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "exactly one winner expected, got: {statuses:?}",
    );

    // Loser carries the active_pin_mismatch wire reason.
    let loser_resp = if resp_a.status() == StatusCode::CONFLICT {
        resp_a
    } else {
        resp_b
    };
    let loser_body = read_body(loser_resp).await;
    assert_eq!(loser_body["error"]["code"], "conflict");
    let loser_msg = loser_body["error"]["message"].as_str().unwrap();
    assert!(
        loser_msg.contains("host_key") && loser_msg.contains("active_pin_mismatch"),
        "loser must surface active_pin_mismatch, got: {loser_msg}",
    );

    // DB end-state: exactly one revoke + one new active pin (no
    // double-revoke, no double-trust). The new active row is unique;
    // the old row is revoked-and-pointed-at the new one.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 2, "want exactly 2 rows: {entries:?}");
    let old_row = entries.iter().find(|e| e.id == old_entry_id).unwrap();
    let new_row = entries.iter().find(|e| e.id != old_entry_id).unwrap();
    assert!(old_row.revoked_at.is_some());
    assert_eq!(old_row.replaced_by_id, Some(new_row.id));
    assert!(new_row.trusted_at.is_some());
    assert!(new_row.revoked_at.is_none());

    // Audit: exactly ONE `host_key_revoked` and exactly ONE
    // `host_key_accepted` row. Two of either would prove a write
    // raced past the lock.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let revoked: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::HostKeyRevoked)
        .collect();
    let accepted: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::HostKeyAccepted)
        .collect();
    assert_eq!(revoked.len(), 1, "race must produce one revoke: {recent:?}");
    assert_eq!(
        accepted.len(),
        1,
        "race must produce one accept: {recent:?}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_rejects_when_captured_does_not_match_expected_new(pool: PgPool) {
    // The probe captures NEW. The caller posts a different expected_new.
    // Race-safety: the freshly-captured fingerprint MUST match what the
    // operator confirmed in the preceding preflight. Mismatch → 409.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, host_id, old_entry_id) =
        make_profile_with_active_pin(&pool, user_id, "replace-cap", "replace-cap.example.com")
            .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": "SHA256:STALE-NEW-FP",
                "reason_code": "lab_target_recreated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("host_key") && msg.contains("captured_mismatch"),
        "expected captured_mismatch reason, got: {msg}",
    );

    // DB unchanged.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, old_entry_id);
    assert!(entries[0].revoked_at.is_none());
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_rejects_when_captured_is_unchanged(pool: PgPool) {
    // The probe captures the CURRENT active pin (host hasn't actually
    // changed). Replace is a no-op; surface 409 with `captured_unchanged`
    // so the SPA copy doesn't pretend a key rotation happened.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_OLD_FP).await;
    let (profile_id, host_id, _) = make_profile_with_active_pin(
        &pool,
        user_id,
        "replace-unchanged",
        "replace-unchanged.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                // Caller asserts NEW; probe says OLD; the
                // captured_unchanged check fires before captured_mismatch.
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "operator_other",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("host_key") && msg.contains("captured_unchanged"),
        "expected captured_unchanged reason, got: {msg}",
    );

    // DB unchanged.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].revoked_at.is_none());
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_refuses_to_re_trust_a_revoked_fingerprint(pool: PgPool) {
    // A revoked row exists for (host, NEW). The probe captures NEW. The
    // route must refuse with `captured_revoked` BEFORE any tx write.
    // Symmetric with trust-host-key's revoked-aware guard.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, host_id, _old_id) = make_profile_with_active_pin(
        &pool,
        user_id,
        "replace-revoked",
        "replace-revoked.example.com",
    )
    .await;
    // Seed a revoked row for the captured fingerprint.
    let seeded = PgKnownHostEntryRepository::new(pool.clone())
        .create(CreateKnownHostEntry {
            host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: REPLACE_NEW_FP.to_owned(),
            public_key: b"ssh-ed25519 NEW-revoked".to_vec(),
        })
        .await
        .unwrap();
    revoke_entry(&pool, seeded.id, user_id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "server_reinstalled",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("host_key") && msg.contains("captured_revoked"),
        "expected captured_revoked reason, got: {msg}",
    );

    // DB unchanged: revoked row still revoked + untrusted; old pin still
    // the active one.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    let revoked = entries.iter().find(|e| e.id == seeded.id).unwrap();
    assert!(revoked.revoked_at.is_some());
    assert!(revoked.trusted_at.is_none());
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_rejects_invalid_reason_code(pool: PgPool) {
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, _host_id, _) = make_profile_with_active_pin(
        &pool,
        user_id,
        "replace-reason",
        "replace-reason.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "operator_freeform",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_rejects_malformed_fingerprint_shapes(pool: PgPool) {
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, _host_id, _) = make_profile_with_active_pin(
        &pool,
        user_id,
        "replace-malformed",
        "replace-malformed.example.com",
    )
    .await;

    // Missing SHA256: prefix on old.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": "MD5:not-supported",
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "host_key_rotated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Missing SHA256: prefix on new.
    let resp2 = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": "garbage",
                "reason_code": "host_key_rotated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_returns_401_without_session_cookie(pool: PgPool) {
    let (app, user_id, _probe, _cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, _host_id, _) =
        make_profile_with_active_pin(&pool, user_id, "replace-401", "replace-401.example.com")
            .await;

    let resp = app
        .oneshot(json_post_no_auth(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "host_key_rotated",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_returns_403_with_bad_origin_before_body_parse(pool: PgPool) {
    // The shared CsrfGuard extractor must fire BEFORE any DB / body work.
    // Pass a malformed body alongside a disallowed Origin: 403, not 400.
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, _host_id, _) =
        make_profile_with_active_pin(&pool, user_id, "replace-csrf", "replace-csrf.example.com")
            .await;

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/server-profiles/{profile_id}/replace-host-key"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://attacker.example.com")
        .header(header::COOKIE, format!("relayterm_session={cookie}"))
        .body(Body::from("{not-json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_foreign_owned_profile_returns_indistinguishable_404(pool: PgPool) {
    // A valid request body addressed to another user's profile must
    // collapse to a 404 byte-identical to a genuine 404.
    let other_user = create_user(&pool, "other").await;
    let (foreign_id, _foreign_host, _) =
        make_profile_with_active_pin(&pool, other_user, "foreign", "foreign.example.com").await;

    let (app, _user, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let bogus = uuid::Uuid::new_v4();
    let body_v = json!({
        "expected_old_fingerprint": REPLACE_OLD_FP,
        "expected_new_fingerprint": REPLACE_NEW_FP,
        "reason_code": "server_reinstalled",
    });

    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/replace-host-key"),
            body_v.clone(),
            &cookie,
        ))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{foreign_id}/replace-host-key"),
            body_v,
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "cross-user replace 404 must match a genuine 404",
    );
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_blocks_when_profile_is_disabled(pool: PgPool) {
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), REPLACE_NEW_FP).await;
    let (profile_id, host_id, _) = make_profile_with_active_pin(
        &pool,
        user_id,
        "replace-disabled",
        "replace-disabled.example.com",
    )
    .await;
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(profile_id, user_id, Some(chrono::Utc::now()))
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "server_reinstalled",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("server_profile") && msg.contains("disabled"),
        "expected server_profile disabled, got: {msg}",
    );
    // No mutation, no audit.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].revoked_at.is_none());
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_returns_502_on_probe_failure(pool: PgPool) {
    // Probe error → 502, no DB mutation, no audit.
    let user_id = create_user(&pool, "dev").await;
    let probe: Arc<dyn SshHostKeyProbe> = Arc::new(ErrorProbe(ProbeError::Unreachable));
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let (profile_id, host_id, _) = make_profile_with_active_pin(
        &pool,
        user_id,
        "replace-probe-fail",
        "replace-probe-fail.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/replace-host-key"),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "host_key_rotated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "bad_gateway");
    assert_eq!(body["error"]["message"], "bad gateway");

    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].revoked_at.is_none());
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_returns_503_when_vault_disabled(pool: PgPool) {
    let user_id = create_user(&pool, "dev").await;
    let probe = FakeProbe::new(captured_for_test(REPLACE_NEW_FP));
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(Arc::new(probe))),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // Build a profile + host + ID directly (vault is None so we can't
    // round-trip a vault-issued identity here; opaque bytes suffice for
    // reaching the vault-check guard).
    let host = PgHostRepository::new(pool.clone())
        .create(CreateHost {
            owner_id: user_id,
            display_name: validate_host_display_name("Vaultless").unwrap(),
            hostname: validate_hostname("v-replace.example.com").unwrap(),
            port: validate_ssh_port(22).unwrap(),
            default_username: validate_ssh_username("deploy").unwrap(),
        })
        .await
        .unwrap();
    let identity = PgSshIdentityRepository::new(pool.clone())
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "vaultless-replace".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 PUB".to_vec(),
            encrypted_private_key: b"opaque".to_vec(),
            fingerprint_sha256: format!("SHA256:vaultless-{}", uuid::Uuid::new_v4()),
        })
        .await
        .unwrap();
    let profile = PgServerProfileRepository::new(pool.clone())
        .create(CreateServerProfile {
            owner_id: user_id,
            name: relayterm_core::validation::validate_profile_name("vaultless-replace").unwrap(),
            host_id: host.id,
            ssh_identity_id: identity.id,
            username_override: None,
            tags: vec![],
        })
        .await
        .unwrap();
    PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: REPLACE_OLD_FP.to_owned(),
            public_key: b"ssh-ed25519 OLD".to_vec(),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{}/replace-host-key", profile.id),
            json!({
                "expected_old_fingerprint": REPLACE_OLD_FP,
                "expected_new_fingerprint": REPLACE_NEW_FP,
                "reason_code": "host_key_rotated",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_no_replace_audit(&pool).await;
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn replace_host_key_does_not_affect_normal_trust_route_behavior_for_changed_keys(
    pool: PgPool,
) {
    // The replace route is additive: the regular trust-host-key route
    // continues to refuse a `changed` key with 409 and writes nothing.
    // This pin protects the TOFU posture from a future regression that
    // wires the replace logic into the normal trust path.
    let new_fp = "SHA256:trust-stays-strict";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), new_fp).await;
    let (profile_id, host_id, _old_id) =
        make_profile_with_active_pin(&pool, user_id, "trust-strict", "trust-strict.example.com")
            .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": new_fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    // Old pin still active; no new entry; no host_key_* audit.
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(host_id)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].fingerprint_sha256, REPLACE_OLD_FP);
    assert!(entries[0].revoked_at.is_none());
    assert_no_replace_audit(&pool).await;
}

/// Assert that NO `host_key_revoked` and NO `host_key_accepted` audit
/// rows have been written. Used by every non-happy-path test so a
/// future regression that emits a "best-effort" audit on a refused
/// replace trips this assertion immediately.
async fn assert_no_replace_audit(pool: &PgPool) {
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(200)
        .await
        .unwrap();
    let any: Vec<_> = recent
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                AuditEventKind::HostKeyRevoked | AuditEventKind::HostKeyAccepted,
            )
        })
        .collect();
    assert!(
        any.is_empty(),
        "no host_key_* audit rows expected on refused replace, got: {any:?}",
    );
}

// ----------------------------------------------------------------------
// Auth-check
//
// These tests use a fake `SshAuthChecker` so the route can be exercised
// end-to-end without a real SSH peer. The vault path IS exercised — every
// test goes through `vault.decrypt_private_key` against a real vault-
// issued blob. The fake checker records every call so accept-pin shape and
// the absence of leaked private-key bytes can be asserted.
// ----------------------------------------------------------------------

async fn setup_with_fake_auth_checker(
    pool: PgPool,
    captured: CapturedHostKey,
    kind: AuthAttemptKind,
) -> (Router, UserId, Arc<FakeAuthChecker>, String) {
    let checker = Arc::new(FakeAuthChecker::new(captured, kind));
    let (app, user_id, cookie) = setup_full(pool, default_probe(), checker.clone()).await;
    (app, user_id, checker, cookie)
}

async fn pin_trusted_entry(pool: &PgPool, host_id: relayterm_core::ids::HostId, fp: &str) {
    PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-ed25519 AAAA-host-key".to_vec(),
        })
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_succeeds_with_trusted_host_key_and_successful_auth(pool: PgPool) {
    let fp = "SHA256:auth-trusted";
    let (app, user_id, checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(fp),
        AuthAttemptKind::Authenticated,
    )
    .await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "auth.example.com").await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    pin_trusted_entry(&pool, profile.host_id, fp).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "authentication_succeeded");
    assert_eq!(body["profile_id"].as_str().unwrap(), profile_id.to_string());
    assert_eq!(
        body["host_id"].as_str().unwrap(),
        profile.host_id.to_string()
    );
    assert!(body["ssh_identity_id"].is_string());
    assert!(body["checked_at"].is_string());

    // Response must NOT contain any host-key, fingerprint, or private-key
    // material — the auth-check surface deliberately omits them.
    let raw = body.to_string();
    for forbidden in [
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
        "fingerprint",
        "SHA256:",
        "host_key",
        "public_key",
    ] {
        assert!(
            !raw.contains(forbidden),
            "auth-check body must not contain `{forbidden}`: {raw}",
        );
    }

    // The fake saw exactly one call; the accept-pins list contained the
    // trusted entry; the PEM passed to the checker did NOT include the
    // ciphertext bytes — proving the vault decrypt happened upstream.
    let calls = checker.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].hostname, "auth.example.com");
    assert_eq!(calls[0].port, 22);
    assert_eq!(calls[0].username, "deploy");
    assert_eq!(calls[0].accept_pin_count, 1);
    let pem = std::str::from_utf8(&calls[0].private_key_pem).unwrap();
    assert!(
        pem.contains("BEGIN OPENSSH PRIVATE KEY"),
        "checker should receive plaintext OpenSSH PEM",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_unknown_profile_returns_404(pool: PgPool) {
    let (app, _user_id, _checker, cookie) = setup_with_fake_auth_checker(
        pool,
        captured_for_test("SHA256:never"),
        AuthAttemptKind::Authenticated,
    )
    .await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
}

/// A foreign-owned profile must produce a response byte-identical to a
/// genuine 404 — same status, same body. AGENTS.md `API get_by_id
/// ownership` lesson applied to auth-check.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_foreign_owned_profile_returns_indistinguishable_404(pool: PgPool) {
    let other_user = create_user(&pool, "other").await;
    let foreign_id = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-auth",
        "foreign.example.com",
    )
    .await;

    let (app, _user, _checker, cookie) = setup_with_fake_auth_checker(
        pool,
        captured_for_test("SHA256:never"),
        AuthAttemptKind::Authenticated,
    )
    .await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{foreign_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "cross-user auth-check 404 must match a genuine 404",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_blocks_when_host_key_unknown(pool: PgPool) {
    let captured_fp = "SHA256:fresh";
    let (app, user_id, checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(captured_fp),
        AuthAttemptKind::HostKeyMismatch,
    )
    .await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "fresh.example.com",
    )
    .await;
    // No known_host_entries pinned at all → status must be host_key_unknown.

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "host_key_unknown");

    // The checker was called (so we know unknown vs changed) and accept_pins
    // was empty.
    let calls = checker.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].accept_pin_count, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_blocks_when_host_key_changed(pool: PgPool) {
    let new_fp = "SHA256:NEW-auth";
    let (app, user_id, checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(new_fp),
        AuthAttemptKind::HostKeyMismatch,
    )
    .await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "rotated-auth.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    // Pin OLD as trusted; the server now presents NEW.
    pin_trusted_entry(&pool, profile.host_id, "SHA256:OLD-auth").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "host_key_changed");

    // Accept-pins handed to the checker contained ONLY the OLD pin —
    // proving the route did not auto-trust the new fingerprint.
    let calls = checker.calls.lock().unwrap().clone();
    assert_eq!(calls[0].accept_pin_count, 1);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_blocks_when_matching_known_host_is_revoked(pool: PgPool) {
    let fp = "SHA256:revoked-auth";
    let (app, user_id, checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(fp),
        AuthAttemptKind::HostKeyMismatch,
    )
    .await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "revoked-auth.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let seeded = PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: fp.to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    revoke_entry(&pool, seeded.id, user_id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "host_key_unknown");

    // accept_pins MUST be empty even though the captured fingerprint
    // matches the row — revoked entries do not enter the pin set.
    let calls = checker.calls.lock().unwrap().clone();
    assert_eq!(calls[0].accept_pin_count, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_returns_authentication_failed_safely(pool: PgPool) {
    let fp = "SHA256:badcred";
    let (app, user_id, _checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(fp),
        AuthAttemptKind::AuthenticationFailed,
    )
    .await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "badcred.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    pin_trusted_entry(&pool, profile.host_id, fp).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "authentication_failed");

    // Body must not surface any russh-side error text or peer banner.
    let raw = body.to_string();
    for forbidden in [
        "russh",
        "peer",
        "permission denied",
        "publickey",
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
    ] {
        assert!(
            !raw.to_lowercase().contains(&forbidden.to_lowercase()),
            "auth-check body must not contain `{forbidden}`: {raw}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_returns_connection_failed_when_checker_errors(pool: PgPool) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(Arc::new(ErroringAuthChecker(
            ProbeError::Unreachable,
        )))),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "unreachable.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    pin_trusted_entry(&pool, profile.host_id, "SHA256:any").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "connection_failed");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_returns_503_when_vault_disabled(pool: PgPool) {
    // Without a vault, the route can't decrypt the identity → 503 with
    // the static service-unavailable body. The auth checker is never called.
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // Provision a profile with an opaque encrypted blob — the route must
    // 503 before it tries to decrypt it.
    let host = PgHostRepository::new(pool.clone())
        .create(CreateHost {
            owner_id: user_id,
            display_name: validate_host_display_name("Vaultless-auth").unwrap(),
            hostname: validate_hostname("va.example.com").unwrap(),
            port: validate_ssh_port(22).unwrap(),
            default_username: validate_ssh_username("deploy").unwrap(),
        })
        .await
        .unwrap();
    let identity = PgSshIdentityRepository::new(pool.clone())
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "vaultless-auth".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 PUB".to_vec(),
            encrypted_private_key: b"opaque".to_vec(),
            fingerprint_sha256: format!("SHA256:vaultless-auth-{}", uuid::Uuid::new_v4()),
        })
        .await
        .unwrap();
    let profile = PgServerProfileRepository::new(pool.clone())
        .create(CreateServerProfile {
            owner_id: user_id,
            name: relayterm_core::validation::validate_profile_name("vaultless-auth").unwrap(),
            host_id: host.id,
            ssh_identity_id: identity.id,
            username_override: None,
            tags: vec![],
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{}/auth-check", profile.id),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(body["error"]["message"], "service unavailable");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_returns_401_without_session_cookie(pool: PgPool) {
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post_no_auth(
            &format!("/api/v1/server-profiles/{bogus}/auth-check"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(body["error"]["message"], "unauthorized");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_response_does_not_overclaim_session_or_command_execution(pool: PgPool) {
    // The success message must NOT imply that a PTY was allocated, a
    // shell was spawned, or a command ran. Pin the wording for each
    // non-error status so an accidental rewording trips the test.
    let fp = "SHA256:scope";
    let (app, user_id, _checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(fp),
        AuthAttemptKind::Authenticated,
    )
    .await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "scope-auth.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    pin_trusted_entry(&pool, profile.host_id, fp).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let message = body["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("no pty") && message.contains("no command"),
        "auth-check success message must explicitly disclaim PTY/command, got: {message}",
    );
    assert!(
        !message.contains("session opened") && !message.contains("shell"),
        "auth-check success message must not imply a shell or session: {message}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_outer_timeout_returns_connection_failed_safely(pool: PgPool) {
    // Outer timeout 50ms; checker sleeps 500ms. The route must return
    // `connection_failed` and the body must NOT leak the slow checker's
    // existence, the configured timeout, or any private-key material.
    let user_id = create_user(&pool, "dev").await;
    let svc = Arc::new(SshAuthCheckService::with_limits(
        Arc::new(SlowAuthChecker {
            delay: std::time::Duration::from_millis(500),
            captured: captured_for_test("SHA256:should-not-reach"),
            kind: AuthAttemptKind::Authenticated,
        }),
        std::time::Duration::from_millis(50),
        4,
    ));
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "slow.example.com").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["status"], "connection_failed");

    // The body must not reveal anything about the timeout, the slow
    // checker, or the decrypted PEM.
    let raw = body.to_string().to_lowercase();
    for forbidden in [
        "timeout",
        "elapsed",
        "deadline",
        "encrypted_private_key",
        "private_key",
        "begin openssh private key",
    ] {
        assert!(
            !raw.contains(forbidden),
            "auth-check timeout body must not contain `{forbidden}`: {raw}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_returns_503_when_concurrency_limit_reached(pool: PgPool) {
    // max_concurrent = 1; the first call holds the slot via a Notify
    // gate; the second call must get a 503 with the static service-
    // unavailable body, NOT a 200 typed status. This is the wire-level
    // proof of the saturation guard.
    let user_id = create_user(&pool, "dev").await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let svc = Arc::new(SshAuthCheckService::with_limits(
        Arc::new(BlockingAuthChecker {
            entered: entered.clone(),
            release: release.clone(),
            captured: captured_for_test("SHA256:any"),
            kind: AuthAttemptKind::Authenticated,
        }),
        std::time::Duration::from_secs(60),
        1,
    ));
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // Two profiles so the two requests address different rows — proves
    // the cap is process-wide rather than per-profile.
    let profile_first =
        make_owned_profile(&pool, user_id, &test_vault(), "first", "first.example.com").await;
    let profile_second = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "second",
        "second.example.com",
    )
    .await;

    // Fire the first request; it parks on the gate.
    let app_first = app.clone();
    let cookie_first = cookie.clone();
    let first = tokio::spawn(async move {
        app_first
            .oneshot(json_post(
                &format!("/api/v1/server-profiles/{profile_first}/auth-check"),
                json!({}),
                &cookie_first,
            ))
            .await
            .unwrap()
    });

    // Wait deterministically until the first request has reached the
    // checker — at which point the service has already acquired the only
    // permit. No sleep, no race: the `entered` notify fires from inside
    // `BlockingAuthChecker::run`, after `try_acquire_owned` returned.
    entered.notified().await;

    let saturated = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_second}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(saturated.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_body(saturated).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(body["error"]["message"], "service unavailable");

    // The 503 body must not leak operator detail about the semaphore,
    // the in-flight call, or any private-key material.
    let raw = body.to_string().to_lowercase();
    for forbidden in [
        "saturated",
        "semaphore",
        "permit",
        "concurrency",
        "encrypted_private_key",
        "private_key",
        "begin openssh private key",
    ] {
        assert!(
            !raw.contains(forbidden),
            "auth-check saturation body must not contain `{forbidden}`: {raw}",
        );
    }

    // Release the first request so the test exits cleanly.
    release.notify_one();
    let first_resp = first.await.unwrap();
    assert_eq!(first_resp.status(), StatusCode::OK);
}

// ----------------------------------------------------------------------
// Terminal sessions
//
// The terminal-session lifecycle surface is the metadata-only foundation
// for the future PTY-bearing orchestrator. These tests pin the wire
// contract: PTY/SSH side-effects MUST NOT happen, ownership rules apply,
// host-key trust is a precondition, and lifecycle events are written.
// ----------------------------------------------------------------------

/// Provision a profile owned by `user_id` AND pin a trusted host-key
/// entry for its host. Returns the profile id, ready for a successful
/// `POST /terminal-sessions` call.
async fn make_trusted_profile(
    pool: &PgPool,
    user_id: UserId,
    vault: &VaultService,
    name: &str,
    hostname: &str,
    fingerprint: &str,
) -> relayterm_core::ids::ServerProfileId {
    let profile_id = make_owned_profile(pool, user_id, vault, name, hostname).await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    pin_trusted_entry(pool, profile.host_id, fingerprint).await;
    profile_id
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_returns_active_with_live_pty(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "host.example.com",
        "SHA256:term-create",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({
                "server_profile_id": profile_id,
                "cols": 120,
                "rows": 30,
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;

    // Default `setup` uses a successful FakePtyBridge — the create
    // route binds a live PTY and transitions the row to `active`.
    assert_eq!(body["status"], "active");
    assert_eq!(body["pty_live"], true);
    assert_eq!(body["cols"], 120);
    assert_eq!(body["rows"], 30);
    assert_eq!(
        body["server_profile_id"].as_str().unwrap(),
        profile_id.to_string()
    );
    assert!(body["id"].is_string());
    assert!(body["created_at"].is_string());
    assert!(body["closed_at"].is_null());

    // Live message must announce PTY started AND caveat replay.
    let message = body["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("ssh pty started") && message.contains("replay"),
        "create response message must announce live pty + caveat replay, got: {message}",
    );

    // Body must NOT contain any key material, terminal I/O, or
    // ownership/internals fields.
    let raw = body.to_string();
    for forbidden in [
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
        "owner_id",
    ] {
        assert!(
            !raw.contains(forbidden),
            "create-terminal-session body must not contain `{forbidden}`: {raw}",
        );
    }

    // The `Created` lifecycle event is the only audit row at this
    // point. SPEC forbids writing `replay_started` until the replay
    // buffer exists, and a precise `live_started` kind is future work.
    let session_id = body["id"].as_str().unwrap();
    let session_uuid: uuid::Uuid = session_id.parse().unwrap();
    let events = PgSessionEventRepository::new(pool.clone())
        .list_for_session(relayterm_core::ids::TerminalSessionId::from_uuid(
            session_uuid,
        ))
        .await
        .unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![SessionEventKind::Created]);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_defaults_dimensions_when_omitted(pool: PgPool) {
    // Default is 80x24; a client that doesn't supply cols/rows should
    // still get a metadata row in starting status.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "default-dim.example.com",
        "SHA256:default-dim",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    assert_eq!(body["cols"], 80);
    assert_eq!(body["rows"], 24);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_without_trusted_host_key_returns_409(pool: PgPool) {
    // No trust entry pinned → host-key is `unknown`. The route must NOT
    // create a session row; it must return a 409 conflict so the client
    // is forced to run `trust-host-key` first.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "untrusted.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("host_key"),
        "conflict message should name the host_key entity, got: {}",
        body["error"]["message"]
    );

    // No metadata row was inserted.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count.0, 0,
        "untrusted host-key must NOT yield a session row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_with_revoked_only_pin_returns_409(pool: PgPool) {
    // A revoked entry is not "trusted" — the create route must refuse.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "revoked.example.com",
    )
    .await;
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let seeded = PgKnownHostEntryRepository::new(pool.clone())
        .record_trusted(CreateKnownHostEntry {
            host_id: profile.host_id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:revoked-term".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    revoke_entry(&pool, seeded.id, user_id).await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");

    // Symmetry with the untrusted-pin variant: no metadata row may be
    // written. A regression that inserts the row before checking the
    // trust gate would pass the status check alone.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count.0, 0,
        "revoked-only host-key must NOT yield a session row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_unknown_profile_returns_404(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": bogus}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
}

/// A foreign-owned profile must produce a 404 byte-identical to a
/// genuine 404. Cross-user existence MUST NOT leak through the create
/// surface.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_foreign_owned_profile_returns_indistinguishable_404(pool: PgPool) {
    let other_user = create_user(&pool, "other").await;
    // Pin a trusted entry against the foreign profile so a successful
    // path is open if the route forgot the ownership filter.
    let foreign_id = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign",
        "foreign.example.com",
    )
    .await;
    let foreign = PgServerProfileRepository::new(pool.clone())
        .get(foreign_id)
        .await
        .unwrap()
        .unwrap();
    pin_trusted_entry(&pool, foreign.host_id, "SHA256:foreign-trust").await;

    let (app, _user, cookie) = setup(pool.clone()).await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": bogus}),
            &cookie,
        ))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": foreign_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "foreign-profile create must produce a byte-identical 404",
    );

    // No row was created — the dev user's listing must be empty.
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions WHERE server_profile_id = $1")
            .bind(foreign_id.into_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count.0, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_invalid_dimensions_returns_400(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "dim.example.com",
        "SHA256:dim",
    )
    .await;

    // cols = 0 — out of range.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id, "cols": 0, "rows": 24}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");

    // rows = 5000 — over the cap.
    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id, "cols": 80, "rows": 5_000}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0, "validation failures must not create rows");
}

// ----------------------------------------------------------------------
// Per-user live-PTY ceiling refusal (Phase 1B.1 quota —
// `docs/session-quotas.md` § 4.1 / § 7.1).
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_refuses_with_429_when_per_user_cap_reached(pool: PgPool) {
    // Drive the create route up to the configured per-user cap, then
    // attempt one more. The refusal MUST be 429 with the new
    // `too_many_sessions` wire code, MUST NOT create a DB row, MUST
    // NOT echo the count/cap or any session id in the body.
    let cap = std::num::NonZeroU32::new(2).unwrap();
    let (app, user_id, cookie) = setup_with_max_live_per_user(pool.clone(), cap).await;
    // Two trusted profiles so the cap-filling creates land on
    // distinct hosts (the SSH-side fake bridge doesn't care, but
    // mimicking real-world usage avoids any accidental unique-key
    // surprises in the test schema).
    let profile_a = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "quota-a",
        "quota-a.example.com",
        "SHA256:quota-a",
    )
    .await;
    let profile_b = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "quota-b",
        "quota-b.example.com",
        "SHA256:quota-b",
    )
    .await;

    // Fill the cap.
    for pid in [profile_a, profile_b] {
        let resp = app
            .clone()
            .oneshot(json_post(
                "/api/v1/terminal-sessions",
                json!({ "server_profile_id": pid }),
                &cookie,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    // Third create — at cap. MUST refuse.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile_a }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_sessions");
    assert_eq!(body["error"]["message"], "too many terminal sessions");
    // No count, no cap, no session id, no profile id, no hostname, no
    // operator detail.
    let raw = body.to_string();
    for forbidden in [
        "current_count",
        "cap",
        "owner_id",
        "user_id",
        "profile_id",
        "session_id",
        "quota-a.example.com",
        "quota-b.example.com",
        // The wire MUST NOT carry the actual numeric values for the
        // current count / cap either (the integers `2` and `3` would
        // surface deployment-shape data the user already knows but
        // exposing them through this surface normalises future leakage).
        &profile_a.to_string(),
        &profile_b.to_string(),
    ] {
        assert!(
            !raw.contains(forbidden),
            "quota-refusal body must not contain `{forbidden}`: {raw}",
        );
    }

    // No new DB row was written by the refusal.
    let row_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions WHERE owner_id = $1")
            .bind(user_id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row_count.0, 2,
        "refused create must not write a terminal_sessions row (have {} rows)",
        row_count.0,
    );

    // No `audit_events` row was written either — quota refusals are
    // operational, not security-relevant (§ 8.2). This catches a
    // regression where a future quota change might accidentally
    // append.
    let audit_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count.0, 0,
        "refused create must not write an audit_events row (have {} rows)",
        audit_count.0,
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_quota_is_owner_scoped(pool: PgPool) {
    // One user's cap MUST NOT leak into another user's create path.
    // Both users share the same backend process / same registry;
    // each is gated independently. Phase 1B.1 § 4.1 rationale.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let (app, alice_id, alice_cookie) = setup_with_max_live_per_user(pool.clone(), cap).await;

    // Provision a second user manually and mint them a session cookie
    // via the auth service (mirrors `setup_with_max_live_per_user`).
    let bob_id = create_user(&pool, "bob").await;
    let bob_auth = test_auth(&Db::from_pool(pool.clone()));
    let bob_cookie = bootstrap_test_session(&bob_auth, bob_id).await;

    let alice_profile = make_trusted_profile(
        &pool,
        alice_id,
        &test_vault(),
        "alice",
        "alice.example.com",
        "SHA256:alice-quota",
    )
    .await;
    let bob_profile = make_trusted_profile(
        &pool,
        bob_id,
        &test_vault(),
        "bob",
        "bob.example.com",
        "SHA256:bob-quota",
    )
    .await;

    // Alice fills her cap (1).
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Alice's second attempt is refused.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // Bob's first attempt MUST succeed despite Alice being at her cap.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": bob_profile }),
            &bob_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "Bob's create must succeed — quota is per-user, not per-deployment",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_refusal_fires_before_vault_decrypt(pool: PgPool) {
    // The quota refusal MUST sit BEFORE vault decrypt + SSH side
    // effects (§ 6.2 ordering). Observable proof: build two routers
    // that share the same `TerminalSessionManager` Arc (so the
    // in-memory registry is shared) — one with vault enabled, one
    // with `vault: None`. Fill the cap via the vault-enabled router;
    // then issue an at-cap create against the vault-disabled router.
    // If the quota check sat AFTER vault decrypt, the second create
    // would surface `503 service unavailable` (the route's
    // vault-disabled branch); if it sits BEFORE (as required), we
    // still get `429 too_many_sessions`.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_live_per_user(&db, cap);
    let auth = test_auth(&db);
    let cookie = bootstrap_test_session(&auth, user_id).await;
    let state_with_vault = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app_with_vault = router(state_with_vault);

    let profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "order",
        "order.example.com",
        "SHA256:order-quota",
    )
    .await;

    // Fill the cap via the vault-enabled router.
    let resp = app_with_vault
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Same `terminal_sessions` Arc → registry is still at the cap.
    // New router with `vault: None`.
    let state_no_vault = AppState {
        db: Db::from_pool(pool.clone()),
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app_no_vault = router(state_no_vault);
    let resp = app_no_vault
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "quota refusal fires before vault decrypt — at-cap second create stays 429, never 503",
    );
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_sessions");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_disabled_profile_409_precedes_quota_429(pool: PgPool) {
    // The quota check MUST sit AFTER the disabled-profile gate
    // (§ 6.2 ordering). A disabled profile MUST still surface
    // `409 conflict { entity: "server_profile", reason: "disabled" }`
    // even if the caller is at their cap — otherwise the quota
    // refusal becomes a side-channel for profile-state inference.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let (app, user_id, cookie) = setup_with_max_live_per_user(pool.clone(), cap).await;
    let live_profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "live",
        "live.example.com",
        "SHA256:live-quota",
    )
    .await;
    let disabled_profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "disabled-at-cap",
        "disabled-at-cap.example.com",
        "SHA256:disabled-at-cap",
    )
    .await;
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(disabled_profile, user_id, Some(chrono::Utc::now()))
        .await
        .unwrap();

    // Fill the cap with the still-live profile.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": live_profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // At cap + disabled profile → 409 (disabled gate fires first).
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": disabled_profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "disabled-profile gate fires before quota — at-cap + disabled collapses to 409, never 429",
    );
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("server_profile") && msg.contains("disabled"),
        "ordering-test 409 must name server_profile + disabled, got: {msg}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_refusal_fires_after_ownership_gate(pool: PgPool) {
    // The quota check MUST sit AFTER ownership resolution (§ 6.2
    // ordering). A foreign / missing profile id MUST still surface
    // `404 not_found` even if the caller is at their cap — otherwise
    // the quota refusal becomes a side-channel for cross-user
    // profile existence.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let (app, user_id, cookie) = setup_with_max_live_per_user(pool.clone(), cap).await;
    let profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "after-owner",
        "after-owner.example.com",
        "SHA256:after-owner",
    )
    .await;
    // Fill the cap.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Ask for a session against a profile id this user does NOT own.
    // Ownership gate fires first → 404. The quota refusal does NOT
    // override it.
    let foreign_owner = create_user(&pool, "foreign").await;
    let foreign_profile = make_trusted_profile(
        &pool,
        foreign_owner,
        &test_vault(),
        "foreign-quota",
        "foreign-quota.example.com",
        "SHA256:foreign-quota",
    )
    .await;
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": foreign_profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "ownership gate fires before quota — foreign profile collapses to 404 even at cap",
    );
}

// ----------------------------------------------------------------------
// Deployment-wide live-PTY quota refusal (Phase 1B.2b —
// `docs/session-quotas.md` § 4.2 / § 7.1). The cap is counted across
// ALL owners against THIS backend instance's in-memory registry.
// Single-instance exact; per-instance best-effort for multi-instance
// topologies (§ 9). The cap is NOT exposed via session-policy
// (operator-only, fingerprinting risk — § 5.4).
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_refuses_with_429_when_deployment_cap_reached(pool: PgPool) {
    // Drive the create route up to the configured deployment-wide cap
    // (across two owners), then attempt one more. The refusal MUST be
    // 429 with the new `too_many_sessions_deployment` wire code, MUST
    // NOT create a DB row, MUST NOT echo the count/cap or any session
    // id/profile id/hostname in the body, and MUST NOT write an
    // audit_events row.
    let cap = std::num::NonZeroU32::new(2).unwrap();
    let (app, alice_id, alice_cookie) = setup_with_max_live_per_deployment(pool.clone(), cap).await;
    let bob_id = create_user(&pool, "bob").await;
    let bob_auth = test_auth(&Db::from_pool(pool.clone()));
    let bob_cookie = bootstrap_test_session(&bob_auth, bob_id).await;

    let alice_profile = make_trusted_profile(
        &pool,
        alice_id,
        &test_vault(),
        "dep-alice",
        "dep-alice.example.com",
        "SHA256:dep-alice",
    )
    .await;
    let bob_profile = make_trusted_profile(
        &pool,
        bob_id,
        &test_vault(),
        "dep-bob",
        "dep-bob.example.com",
        "SHA256:dep-bob",
    )
    .await;

    // Alice + Bob each create one — cap = 2 is reached across owners.
    for (pid, cookie) in [
        (alice_profile, alice_cookie.as_str()),
        (bob_profile, bob_cookie.as_str()),
    ] {
        let resp = app
            .clone()
            .oneshot(json_post(
                "/api/v1/terminal-sessions",
                json!({ "server_profile_id": pid }),
                cookie,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    // Alice's third attempt — at deployment cap (Bob already holds the
    // second slot). MUST refuse with the deployment-scoped code.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    // No `Retry-After` header (`docs/session-quotas.md` § 7.2 — the
    // user must act, not wait on a wall clock). Mirrors the per-user
    // variants and the existing `LoginThrottler` posture.
    assert!(
        resp.headers().get(axum::http::header::RETRY_AFTER).is_none(),
        "deployment-cap 429 must not carry a Retry-After header",
    );
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_sessions_deployment");
    assert_eq!(
        body["error"]["message"],
        "too many terminal sessions for this deployment",
    );
    // No count, no cap, no session id, no profile id, no hostname, no
    // operator detail. The wire MUST NOT carry the numeric values for
    // the current count / cap either.
    let raw = body.to_string();
    for forbidden in [
        "current_count",
        "cap",
        "owner_id",
        "user_id",
        "profile_id",
        "session_id",
        "dep-alice.example.com",
        "dep-bob.example.com",
        &alice_profile.to_string(),
        &bob_profile.to_string(),
    ] {
        assert!(
            !raw.contains(forbidden),
            "deployment-quota refusal must not contain `{forbidden}`: {raw}",
        );
    }

    // No new DB row was written by the refusal.
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        total.0, 2,
        "refused create must not write a terminal_sessions row (have {} rows)",
        total.0,
    );

    // No audit_events row was written either — quota refusals are
    // operational, not security-relevant (§ 8.2).
    let audit_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count.0, 0,
        "deployment-refused create must not write an audit_events row (have {} rows)",
        audit_count.0,
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_deployment_cap_is_cross_owner(pool: PgPool) {
    // The deployment cap MUST close out OTHER users once it is full —
    // unlike the per-user cap, which is owner-scoped. Alice fills the
    // single deployment slot; Bob's first attempt MUST be refused with
    // the deployment-scoped code (not the per-user code, which would
    // mislead Bob into thinking his own session count is the
    // problem).
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let (app, alice_id, alice_cookie) = setup_with_max_live_per_deployment(pool.clone(), cap).await;
    let bob_id = create_user(&pool, "bob").await;
    let bob_auth = test_auth(&Db::from_pool(pool.clone()));
    let bob_cookie = bootstrap_test_session(&bob_auth, bob_id).await;

    let alice_profile = make_trusted_profile(
        &pool,
        alice_id,
        &test_vault(),
        "dep-cross-alice",
        "dep-cross-alice.example.com",
        "SHA256:dep-cross-alice",
    )
    .await;
    let bob_profile = make_trusted_profile(
        &pool,
        bob_id,
        &test_vault(),
        "dep-cross-bob",
        "dep-cross-bob.example.com",
        "SHA256:dep-cross-bob",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Bob's create — deployment cap reached by another user. MUST be
    // the deployment-scoped 429.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": bob_profile }),
            &bob_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_body(resp).await;
    assert_eq!(
        body["error"]["code"], "too_many_sessions_deployment",
        "cross-owner refusal must surface the deployment scope, not the per-user scope",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_per_user_live_cap_beats_deployment_cap(pool: PgPool) {
    // Ordering rule (`docs/session-quotas.md` § 6.2): the per-user
    // live cap MUST fire BEFORE the deployment cap when BOTH would
    // refuse. Rationale: the user's PERSONAL cap is the more specific
    // cause when it fires ("close one of YOUR sessions"); the
    // deployment refusal would misdirect them to "wait for the
    // operator". Build a router where per-user = 1 AND deployment =
    // 1; fill the slot; observe that the second create surfaces
    // `too_many_sessions`, not `too_many_sessions_deployment`.
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    use relayterm_core::repository::{SessionEventRepository, TerminalSessionRepository};
    let terminal_sessions = Arc::new(
        TerminalSessionManager::new(
            Arc::new(db.terminal_sessions()) as Arc<dyn TerminalSessionRepository>,
            Arc::new(db.session_events()) as Arc<dyn SessionEventRepository>,
        )
        .with_max_live_pty_per_user(std::num::NonZeroU32::new(1).unwrap())
        .with_max_live_pty_per_deployment(std::num::NonZeroU32::new(1).unwrap()),
    );
    let auth = test_auth(&db);
    let cookie = bootstrap_test_session(&auth, user_id).await;
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "order-user-vs-dep",
        "order-user-vs-dep.example.com",
        "SHA256:order-user-vs-dep",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_body(resp).await;
    assert_eq!(
        body["error"]["code"], "too_many_sessions",
        "per-user live cap fires before deployment cap when both would refuse",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_deployment_cap_refusal_fires_before_vault_decrypt(pool: PgPool) {
    // The deployment refusal MUST sit BEFORE vault decrypt + SSH side
    // effects (§ 6.2). Observable proof: share the manager Arc across
    // two routers — one vault-enabled (fills the slot), one
    // vault-disabled. If the deployment check sat AFTER vault decrypt,
    // the second create would surface `503 service_unavailable`; if it
    // sits BEFORE (as required), it stays 429
    // too_many_sessions_deployment.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let alice_id = create_user(&pool, "alice").await;
    let bob_id = create_user(&pool, "bob").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_live_per_deployment(&db, cap);
    let auth = test_auth(&db);
    let alice_cookie = bootstrap_test_session(&auth, alice_id).await;
    let bob_cookie = bootstrap_test_session(&auth, bob_id).await;

    let state_with_vault = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app_with_vault = router(state_with_vault);

    let alice_profile = make_trusted_profile(
        &pool,
        alice_id,
        &test_vault(),
        "dep-order-alice",
        "dep-order-alice.example.com",
        "SHA256:dep-order-alice",
    )
    .await;
    let bob_profile = make_trusted_profile(
        &pool,
        bob_id,
        &test_vault(),
        "dep-order-bob",
        "dep-order-bob.example.com",
        "SHA256:dep-order-bob",
    )
    .await;

    let resp = app_with_vault
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Build a second router with `vault: None`, sharing the same
    // terminal_sessions Arc → the deployment slot stays filled.
    let state_no_vault = AppState {
        db: Db::from_pool(pool.clone()),
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app_no_vault = router(state_no_vault);
    let resp = app_no_vault
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": bob_profile }),
            &bob_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "deployment refusal fires before vault decrypt — at-cap create stays 429, never 503",
    );
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_sessions_deployment");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_deployment_cap_foreign_profile_404_beats_quota(pool: PgPool) {
    // Ownership gate MUST sit before the deployment quota (§ 6.2). A
    // foreign profile id at the cap still collapses to 404, never
    // 429 — otherwise the quota refusal would become a side-channel
    // for cross-user profile existence.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let (app, alice_id, alice_cookie) = setup_with_max_live_per_deployment(pool.clone(), cap).await;
    let alice_profile = make_trusted_profile(
        &pool,
        alice_id,
        &test_vault(),
        "dep-foreign-alice",
        "dep-foreign-alice.example.com",
        "SHA256:dep-foreign-alice",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let bob_id = create_user(&pool, "bob").await;
    let bob_profile = make_trusted_profile(
        &pool,
        bob_id,
        &test_vault(),
        "dep-foreign-bob",
        "dep-foreign-bob.example.com",
        "SHA256:dep-foreign-bob",
    )
    .await;

    // Alice asks for Bob's profile — 404 fires first, even at
    // deployment cap.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": bob_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "ownership gate fires before deployment quota — foreign profile collapses to 404",
    );
}

// ----------------------------------------------------------------------
// Per-user starting-burst quota refusal (Phase 1B.2a — `docs/session-quotas.md`
// § 4.3 / § 7.1).
//
// Unlike the live-PTY quota (which fills naturally via successful HTTP
// POSTs because the FakePtyBridge promotes immediately), the starting
// quota counts the disjoint "Starting placeholder, no live PTY yet"
// set. Under the FakePtyBridge that set is empty in steady state — a
// successful create promotes within the same handler call. To exercise
// the refusal path deterministically we share the manager Arc with the
// router, then seed Starting placeholders by calling `create_session`
// directly on the shared manager (bypassing the bridge). The HTTP
// POST that follows then trips the quota under controlled state.
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_refuses_with_429_when_per_user_starting_cap_reached(pool: PgPool) {
    use relayterm_terminal::CreateTerminalSessionRequest as ManagerCreateRequest;
    let cap = std::num::NonZeroU32::new(2).unwrap();
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_starting_per_user(&db, cap);
    let auth = test_auth(&db);
    let cookie = bootstrap_test_session(&auth, user_id).await;
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "starting-cap",
        "starting-cap.example.com",
        "SHA256:starting-cap",
    )
    .await;

    // Seed the starting-burst quota directly on the shared manager —
    // each call inserts a `Starting` placeholder with `live = None`,
    // which is what the new starting-burst counter sums.
    for _ in 0..cap.get() {
        terminal_sessions
            .create_session(ManagerCreateRequest {
                owner_id: user_id,
                server_profile_id: profile,
                cols: 80,
                rows: 24,
            })
            .await
            .unwrap();
    }

    // At cap. The next HTTP create MUST refuse with the new typed
    // 429, MUST NOT echo the count/cap or any session id, MUST NOT
    // write a new DB row beyond the placeholders, and MUST NOT write
    // any audit row.
    let pre_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_starting_sessions");
    assert_eq!(
        body["error"]["message"],
        "too many starting terminal sessions"
    );
    let raw = body.to_string();
    for forbidden in [
        "current_count",
        "cap",
        "owner_id",
        "user_id",
        "profile_id",
        "session_id",
        "starting-cap.example.com",
        &profile.to_string(),
    ] {
        assert!(
            !raw.contains(forbidden),
            "starting-quota refusal body must not contain `{forbidden}`: {raw}",
        );
    }
    let post_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        pre_count.0, post_count.0,
        "refused create must not write a terminal_sessions row (was {}, now {})",
        pre_count.0, post_count.0,
    );
    let audit_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count.0, 0,
        "refused create must not write an audit_events row (have {} rows)",
        audit_count.0,
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_starting_quota_is_owner_scoped(pool: PgPool) {
    // One user's starting placeholders MUST NOT count against another
    // user's starting cap. Mirrors the live-quota owner-scope test.
    use relayterm_terminal::CreateTerminalSessionRequest as ManagerCreateRequest;
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_starting_per_user(&db, cap);
    let auth = test_auth(&db);
    let alice_cookie = bootstrap_test_session(&auth, user_id).await;
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let alice_id = user_id;

    let bob_id = create_user(&pool, "bob").await;
    let bob_auth = test_auth(&Db::from_pool(pool.clone()));
    let bob_cookie = bootstrap_test_session(&bob_auth, bob_id).await;

    let alice_profile = make_trusted_profile(
        &pool,
        alice_id,
        &test_vault(),
        "alice-start",
        "alice-start.example.com",
        "SHA256:alice-start",
    )
    .await;
    let bob_profile = make_trusted_profile(
        &pool,
        bob_id,
        &test_vault(),
        "bob-start",
        "bob-start.example.com",
        "SHA256:bob-start",
    )
    .await;

    // Fill Alice's starting cap (1).
    terminal_sessions
        .create_session(ManagerCreateRequest {
            owner_id: alice_id,
            server_profile_id: alice_profile,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    // Alice's HTTP create is refused.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": alice_profile }),
            &alice_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_starting_sessions");

    // Bob's first attempt MUST succeed despite Alice's burst quota
    // being filled — the cap is per-user, not per-deployment.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": bob_profile }),
            &bob_cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "Bob's create must succeed — starting quota is per-user, not per-deployment",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_starting_refusal_fires_before_vault_decrypt(pool: PgPool) {
    // The starting-quota refusal MUST sit BEFORE vault decrypt + SSH
    // side effects (§ 6.2 ordering). Same shape as the live-quota
    // ordering test: a vault-disabled router still surfaces
    // `429 too_many_starting_sessions` (NOT `503 service_unavailable`)
    // when the quota is full.
    use relayterm_terminal::CreateTerminalSessionRequest as ManagerCreateRequest;
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_starting_per_user(&db, cap);
    let auth = test_auth(&db);
    let cookie = bootstrap_test_session(&auth, user_id).await;
    let state_no_vault = AppState {
        db: Db::from_pool(pool.clone()),
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app_no_vault = router(state_no_vault);

    let profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "start-order",
        "start-order.example.com",
        "SHA256:start-order",
    )
    .await;

    // Fill the cap by seeding a Starting placeholder directly.
    terminal_sessions
        .create_session(ManagerCreateRequest {
            owner_id: user_id,
            server_profile_id: profile,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    // Vault disabled → if the quota check sat AFTER vault decrypt the
    // route would surface 503; the load-bearing assertion is that the
    // 429 fires first.
    let resp = app_no_vault
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "starting-quota refusal fires before vault decrypt — at-cap second create stays 429, never 503",
    );
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "too_many_starting_sessions");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_starting_refusal_fires_after_ownership_gate(pool: PgPool) {
    // Symmetric to the live-quota ordering test
    // (`create_terminal_session_refusal_fires_after_ownership_gate`).
    // The starting-quota refusal MUST sit AFTER ownership resolution
    // (§ 6.2 ordering). A foreign / missing profile id MUST still
    // surface `404 not_found` even if the caller is at their starting
    // cap — otherwise the quota refusal becomes a side-channel for
    // cross-user profile existence.
    use relayterm_terminal::CreateTerminalSessionRequest as ManagerCreateRequest;
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_starting_per_user(&db, cap);
    let auth = test_auth(&db);
    let cookie = bootstrap_test_session(&auth, user_id).await;
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // Caller's own profile — used to seed the starting cap.
    let own_profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "after-owner-start",
        "after-owner-start.example.com",
        "SHA256:after-owner-start",
    )
    .await;
    // Fill the starting cap by seeding a Starting placeholder
    // directly on the shared manager.
    terminal_sessions
        .create_session(ManagerCreateRequest {
            owner_id: user_id,
            server_profile_id: own_profile,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    // Ask for a session against a profile id this user does NOT own.
    // Ownership gate fires first → 404. The starting-quota refusal
    // does NOT override it.
    let foreign_owner = create_user(&pool, "foreign-start").await;
    let foreign_profile = make_trusted_profile(
        &pool,
        foreign_owner,
        &test_vault(),
        "foreign-start-quota",
        "foreign-start-quota.example.com",
        "SHA256:foreign-start-quota",
    )
    .await;
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": foreign_profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "ownership gate fires before starting quota — foreign profile collapses to 404 even at cap",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_starting_quota_does_not_double_count_live(pool: PgPool) {
    // The starting and live quotas count DISJOINT sets — promoting a
    // Starting placeholder to Live (via the normal HTTP create flow,
    // which the FakePtyBridge does atomically) must move the entry
    // from the starting count to the live count, never both. After a
    // successful HTTP create the starting count is `0` and the live
    // count is `1`; a follow-up HTTP create against the cap-filled
    // starting set is therefore not at the starting cap.
    let cap = std::num::NonZeroU32::new(1).unwrap();
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager_with_max_starting_per_user(&db, cap);
    let auth = test_auth(&db);
    let cookie = bootstrap_test_session(&auth, user_id).await;
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions: terminal_sessions.clone(),
        auth: auth.clone(),
        auth_routes: test_auth_routes(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let profile = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "no-double",
        "no-double.example.com",
        "SHA256:no-double",
    )
    .await;
    // First create succeeds end-to-end — the placeholder promotes to
    // Live within the same handler call.
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(
        terminal_sessions.count_starting_for_user(user_id),
        0,
        "promoted-to-live entry must drop out of the starting count",
    );
    assert_eq!(
        terminal_sessions.count_live_pty_for_user(user_id),
        1,
        "promoted-to-live entry counts against the live quota instead",
    );
    // A second HTTP create at starting-cap = 1 must succeed because
    // the starting count is 0 (the previous entry promoted to Live).
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "second create succeeds — starting count is disjoint from live count",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn list_terminal_sessions_returns_only_current_user(pool: PgPool) {
    let other_user = create_user(&pool, "other").await;
    // Provision a foreign session directly via the repository — bypasses
    // the API to model "another user already created a session somehow."
    let foreign_profile = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign",
        "foreign-list.example.com",
    )
    .await;
    let foreign_repo = PgTerminalSessionRepository::new(pool.clone());
    let _ = foreign_repo
        .create(relayterm_core::repository::CreateTerminalSession {
            owner_id: other_user,
            server_profile_id: foreign_profile,
            status: TerminalSessionStatus::Starting,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    let (app, user_id, cookie) = setup(pool.clone()).await;

    // Empty list to start.
    let resp = app
        .clone()
        .oneshot(get("/api/v1/terminal-sessions", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(
        body.as_array().unwrap().len(),
        0,
        "dev user's list must NOT include the other user's session",
    );

    // Create one session for the dev user; confirm it shows up alone.
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "mine",
        "mine.example.com",
        "SHA256:mine-list",
    )
    .await;
    let create = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(get("/api/v1/terminal-sessions", &cookie))
        .await
        .unwrap();
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0]["server_profile_id"].as_str().unwrap(),
        profile_id.to_string()
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn get_terminal_session_unknown_id_returns_404(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(get(&format!("/api/v1/terminal-sessions/{bogus}"), &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Foreign-owned session must look like a genuine 404. Cross-user 404 is
/// byte-identical.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn get_terminal_session_foreign_owned_returns_indistinguishable_404(pool: PgPool) {
    let other_user = create_user(&pool, "other").await;
    let foreign_profile = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-get",
        "foreign-get.example.com",
    )
    .await;
    let foreign_session = PgTerminalSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateTerminalSession {
            owner_id: other_user,
            server_profile_id: foreign_profile,
            status: TerminalSessionStatus::Starting,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    let (app, _user, cookie) = setup(pool).await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/terminal-sessions/{bogus}"), &cookie))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{}", foreign_session.id),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "foreign-owned terminal session GET must match a genuine 404",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn close_terminal_session_marks_closed_and_writes_event(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "close.example.com",
        "SHA256:close",
    )
    .await;

    let create = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let session_id = read_body(create).await["id"].as_str().unwrap().to_owned();

    let close = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(close.status(), StatusCode::OK);
    let body = read_body(close).await;
    assert_eq!(body["status"], "closed");
    assert_eq!(body["already_closed"], false);
    assert!(body["closed_at"].is_string());

    // The DB row is closed.
    let row = PgTerminalSessionRepository::new(pool.clone())
        .get(relayterm_core::ids::TerminalSessionId::from_uuid(
            session_id.parse().unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Closed);
    assert!(row.closed_at.is_some());

    // Closed event was appended.
    let events = PgSessionEventRepository::new(pool)
        .list_for_session(row.id)
        .await
        .unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&SessionEventKind::Created));
    assert!(kinds.contains(&SessionEventKind::Closed));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn close_terminal_session_double_close_is_idempotent(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "idem.example.com",
        "SHA256:idem-close",
    )
    .await;
    let create = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    let session_id = read_body(create).await["id"].as_str().unwrap().to_owned();

    let first = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(read_body(first).await["already_closed"], false);

    let second = app
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    let body = read_body(second).await;
    assert_eq!(body["already_closed"], true);
    assert_eq!(body["status"], "closed");

    // Only ONE Closed event exists on the second close.
    let session_uuid: uuid::Uuid = session_id.parse().unwrap();
    let events = PgSessionEventRepository::new(pool)
        .list_for_session(relayterm_core::ids::TerminalSessionId::from_uuid(
            session_uuid,
        ))
        .await
        .unwrap();
    let closed_count = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed_count, 1,
        "second close must NOT append another Closed event"
    );
}

/// Closing a foreign-owned session looks like a genuine 404 — same status,
/// same body. No status change on the foreign row.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn close_terminal_session_foreign_owned_returns_indistinguishable_404(pool: PgPool) {
    let other_user = create_user(&pool, "other").await;
    let foreign_profile = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-close",
        "foreign-close.example.com",
    )
    .await;
    let foreign_session = PgTerminalSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateTerminalSession {
            owner_id: other_user,
            server_profile_id: foreign_profile,
            status: TerminalSessionStatus::Starting,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    let (app, _user, cookie) = setup(pool.clone()).await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{bogus}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{}/close", foreign_session.id),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), bogus_status);
    let body = read_body(resp).await;
    assert_eq!(
        body, bogus_body,
        "foreign-owned terminal session close must match a genuine 404",
    );

    // Foreign row was NOT mutated.
    let row = PgTerminalSessionRepository::new(pool)
        .get(foreign_session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status,
        TerminalSessionStatus::Starting,
        "foreign session row must not transition on a denied close",
    );
    assert!(row.closed_at.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn terminal_session_routes_return_401_without_session_cookie(pool: PgPool) {
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    for req in [
        json_post_no_auth(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": uuid::Uuid::new_v4()}),
        ),
        get_no_auth("/api/v1/terminal-sessions"),
        get_no_auth(&format!(
            "/api/v1/terminal-sessions/{}",
            uuid::Uuid::new_v4()
        )),
        json_post_no_auth(
            &format!("/api/v1/terminal-sessions/{}/close", uuid::Uuid::new_v4()),
            json!({}),
        ),
    ] {
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = read_body(resp).await;
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["error"]["message"], "unauthorized");
    }
}

/// The create response's `message` must announce the live PTY AND
/// caveat the no-replay scope, never overpromise reconnect/resume. The
/// pinned wording is enforced so a future "helpful" rewording is forced
/// through review.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_message_announces_pty_and_caveats_replay(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "scope.example.com",
        "SHA256:scope-msg",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    let message = body["message"].as_str().unwrap().to_lowercase();

    assert!(
        message.contains("ssh pty started") && message.contains("replay"),
        "create message must announce live pty + caveat replay, got: {message}",
    );
    for forbidden in [
        // Words that would imply more than what the slice attests.
        "logged in",
        "shell ready",
        "shell spawned",
        "connected to",
        "session opened",
        "replay implemented",
        "replay across reconnects is implemented",
    ] {
        assert!(
            !message.contains(forbidden),
            "create message must not imply `{forbidden}`: {message}",
        );
    }
}

// ----------------------------------------------------------------------
// Terminal WebSocket attach/detach lifecycle
// ----------------------------------------------------------------------

/// Spawn the supplied router on an OS-assigned local port and return the
/// bound address. The handle is detached; the server lives until the
/// test process exits or every client connection has dropped.
async fn spawn_app(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// Open a WebSocket against `/api/v1/terminal-sessions/:id/ws` for the
/// given session. Panics on any handshake failure — the tests assert
/// pre-upgrade rejections via the plain HTTP client below.
async fn open_ws(
    addr: SocketAddr,
    session_id: relayterm_core::ids::TerminalSessionId,
    cookie: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = format!("ws://{addr}/api/v1/terminal-sessions/{session_id}/ws");
    let mut req = url.into_client_request().expect("ws url -> request");
    req.headers_mut().insert(
        axum::http::header::COOKIE,
        format!("relayterm_session={cookie}").parse().unwrap(),
    );
    let (stream, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .expect("WebSocket handshake should succeed for an owned, open session");
    stream
}

/// Receive the next protocol frame and decode it.
///
/// Text frames are JSON-decoded into [`ServerMsg`]. Binary frames are
/// decoded as [`relayterm_protocol::BinaryFrame`] of kind `Output` and
/// translated into the equivalent `ServerMsg::Output { seq, data }` —
/// `data` is re-encoded as base64 so existing assertions calling
/// [`relayterm_protocol::output_data_decode`] keep working unchanged.
/// A binary frame whose kind is anything other than `Output` is a
/// protocol violation server-side and panics loudly here.
async fn recv_server_msg(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> relayterm_protocol::ServerMsg {
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).expect("server message must be valid JSON");
            }
            Some(Ok(Message::Binary(bytes))) => {
                let frame = relayterm_protocol::decode_binary_frame(&bytes)
                    .expect("server binary frame must be valid RTB1");
                assert_eq!(
                    frame.kind,
                    relayterm_protocol::BinaryFrameKind::Output,
                    "server only emits binary Output frames",
                );
                return relayterm_protocol::ServerMsg::Output {
                    seq: relayterm_core::SeqNo(frame.seq),
                    data: relayterm_protocol::output_data_encode(&frame.payload),
                };
            }
            Some(Ok(Message::Close(_))) => panic!("socket closed before any text frame"),
            Some(Ok(_)) => continue, // skip ping/pong frames
            Some(Err(err)) => panic!("transport error: {err:?}"),
            None => panic!("socket ended before any text frame"),
        }
    }
}

async fn send_client_msg(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    msg: &relayterm_protocol::ClientMsg,
) {
    use tokio_tungstenite::tungstenite::Message;
    let payload = serde_json::to_string(msg).unwrap();
    socket.send(Message::Text(payload.into())).await.unwrap();
}

/// Drive the create route and return the new session's id.
async fn create_session_via_api(
    app: &Router,
    profile_id: relayterm_core::ids::ServerProfileId,
    cookie: &str,
) -> relayterm_core::ids::TerminalSessionId {
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    body["id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .map(relayterm_core::ids::TerminalSessionId::from_uuid)
        .unwrap()
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_emits_session_attached_with_session_id_and_writes_attachment_row(pool: PgPool) {
    // Default `setup` uses a successful FakePtyBridge, so the create
    // route binds a live PTY. The status MUST be `Active` and the
    // attachment row must land in the DB. Wire wording is asserted
    // separately by `ws_attach_emits_session_attached_active_when_pty_live`.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-attach.example.com",
        "SHA256:ws-attach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;

    let msg = recv_server_msg(&mut socket).await;
    match msg {
        relayterm_protocol::ServerMsg::SessionAttached {
            session_id: got_id,
            attachment_id: _,
            status,
            message,
        } => {
            assert_eq!(got_id, session_id);
            assert_eq!(status, relayterm_protocol::SessionAttachStatus::Active);
            let lower = message.to_lowercase();
            for forbidden in ["session opened", "shell ready", "logged in"] {
                assert!(
                    !lower.contains(forbidden),
                    "session_attached must not imply `{forbidden}`: {message}",
                );
            }
        }
        other => panic!("expected SessionAttached, got {other:?}"),
    }

    // The attachment row exists.
    let session = relayterm_core::ids::TerminalSessionId::from(session_id.into_uuid());
    let attachments = PgTerminalSessionRepository::new(pool)
        .list_attachments(session)
        .await
        .unwrap();
    assert_eq!(attachments.len(), 1);
    assert!(attachments[0].detached_at.is_none());
}

/// `tokio-tungstenite` returns the rejected handshake response inside the
/// `Http` error variant — pull the status code out so tests can assert on
/// the same numbers the HTTP routes use. Any other error variant is a
/// test-rig bug, not a route behavior. `cookie` is the session cookie
/// value to attach to the upgrade handshake; `None` omits the cookie
/// header entirely and exercises the missing-cookie 401 path.
async fn ws_handshake_status(
    addr: SocketAddr,
    session_id_uri: &str,
    cookie: Option<&str>,
) -> (axum::http::StatusCode, Option<Vec<u8>>) {
    use tokio_tungstenite::tungstenite::Error;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = format!("ws://{addr}/api/v1/terminal-sessions/{session_id_uri}/ws");
    let mut req = url.into_client_request().expect("ws url -> request");
    if let Some(c) = cookie {
        req.headers_mut().insert(
            axum::http::header::COOKIE,
            format!("relayterm_session={c}").parse().unwrap(),
        );
    }
    let err = tokio_tungstenite::connect_async(req)
        .await
        .expect_err("expected handshake failure");
    match err {
        Error::Http(resp) => {
            let (parts, body) = resp.into_parts();
            (parts.status, body)
        }
        other => panic!("expected Http error, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_unknown_session_returns_404_before_upgrade(pool: PgPool) {
    let (app, _user, cookie) = setup(pool).await;
    let addr = spawn_app(app).await;
    let bogus = uuid::Uuid::new_v4();
    let (status, body) = ws_handshake_status(addr, &bogus.to_string(), Some(&cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body = body.expect("404 must carry an error envelope body");
    let parsed: Value = serde_json::from_slice(&body).expect("body is valid JSON");
    assert_eq!(parsed["error"]["code"], "not_found");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_foreign_session_returns_indistinguishable_404(pool: PgPool) {
    let other_user = create_user(&pool, "other").await;
    let foreign_profile = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-ws",
        "foreign-ws.example.com",
    )
    .await;
    let foreign_session = PgTerminalSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateTerminalSession {
            owner_id: other_user,
            server_profile_id: foreign_profile,
            status: TerminalSessionStatus::Starting,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    let (app, _user, cookie) = setup(pool).await;
    let addr = spawn_app(app).await;

    let bogus = uuid::Uuid::new_v4();
    let (bogus_status, bogus_body) =
        ws_handshake_status(addr, &bogus.to_string(), Some(&cookie)).await;
    let (foreign_status, foreign_body) =
        ws_handshake_status(addr, &foreign_session.id.to_string(), Some(&cookie)).await;

    assert_eq!(bogus_status, StatusCode::NOT_FOUND);
    assert_eq!(foreign_status, bogus_status);
    assert_eq!(
        foreign_body, bogus_body,
        "foreign-owned WS attach must produce a byte-identical 404 body",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_closed_session_returns_409(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-closed.example.com",
        "SHA256:ws-closed",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    // Close it before attempting to attach.
    let close = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(close.status(), StatusCode::OK);

    let addr = spawn_app(app).await;
    let (status, body) = ws_handshake_status(addr, &session_id.to_string(), Some(&cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let body = body.expect("409 must carry an error envelope body");
    let parsed: Value = serde_json::from_slice(&body).expect("body is valid JSON");
    assert_eq!(parsed["error"]["code"], "conflict");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_returns_401_without_session_cookie(pool: PgPool) {
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let addr = spawn_app(app).await;
    // Empty cookie value → no Cookie header on the upgrade. The
    // AuthenticatedUser extractor short-circuits BEFORE the upgrade
    // handshake completes, so the client sees a clean HTTP 401 instead
    // of a socket-open-then-close.
    let (status, _body) = ws_handshake_status(addr, &uuid::Uuid::new_v4().to_string(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_ping_returns_pong(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-ping.example.com",
        "SHA256:ws-ping",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    // Drain the SessionAttached frame.
    let _ = recv_server_msg(&mut socket).await;

    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Ping).await;
    let pong = recv_server_msg(&mut socket).await;
    assert!(matches!(pong, relayterm_protocol::ServerMsg::Pong));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_resize_acks_and_writes_event(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-resize.example.com",
        "SHA256:ws-resize",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Resize {
            cols: 132,
            rows: 50,
        },
    )
    .await;
    let ack = recv_server_msg(&mut socket).await;
    match ack {
        relayterm_protocol::ServerMsg::Ack { kind } => {
            assert_eq!(kind, relayterm_protocol::AckKind::Resize);
        }
        other => panic!("expected Ack, got {other:?}"),
    }

    // Resized event was persisted with the new dims.
    let events = PgSessionEventRepository::new(pool)
        .list_for_session(session_id)
        .await
        .unwrap();
    let resized: Vec<_> = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Resized)
        .collect();
    assert_eq!(resized.len(), 1);
    assert_eq!(resized[0].payload["cols"], 132);
    assert_eq!(resized[0].payload["rows"], 50);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_resize_invalid_dims_returns_typed_error(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-bad-resize.example.com",
        "SHA256:ws-bad-resize",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Resize { cols: 0, rows: 24 },
    )
    .await;
    let err = recv_server_msg(&mut socket).await;
    match err {
        relayterm_protocol::ServerMsg::Error { code, message } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::InvalidInput);
            assert!(
                message.to_lowercase().contains("dimension")
                    || message.to_lowercase().contains("invalid"),
                "error message should signal invalid dims: {message}",
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // No Resized event was written.
    let events = PgSessionEventRepository::new(pool)
        .list_for_session(session_id)
        .await
        .unwrap();
    let resized = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Resized)
        .count();
    assert_eq!(resized, 0, "invalid resize must not append a Resized event");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_input_against_session_without_live_pty_returns_pty_not_live(pool: PgPool) {
    // Build a session that has NO live PTY (the manager's stub path).
    // We do this by inserting a `terminal_sessions` row directly via the
    // repo, bypassing the create route — exercises the WS handler's
    // `state.live.is_none()` branch which surfaces `pty_not_live` and
    // never reflects the input payload.
    let user_id = create_user(&pool, "dev").await;
    let db = relayterm_db::Db::from_pool(pool.clone());
    let bridge = FakePtyBridge::new();
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db: db.clone(),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: bridge as Arc<dyn SshPtyBridge>,
        terminal_sessions: terminal_sessions.clone(),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    // Insert a row directly — no live PTY runtime is registered.
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-pty.example.com",
        "SHA256:ws-no-pty",
    )
    .await;
    let session = PgTerminalSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateTerminalSession {
            owner_id: user_id,
            server_profile_id: profile_id,
            status: relayterm_core::terminal_session::TerminalSessionStatus::Starting,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let session_id = session.id;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(AttachedStub)

    let sentinel = "REDACT-MARKER-WS-INPUT-3D8F";
    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Input {
            data: sentinel.to_owned(),
        },
    )
    .await;
    let resp = recv_server_msg(&mut socket).await;
    let raw = serde_json::to_string(&resp).unwrap();
    assert!(
        !raw.contains(sentinel),
        "input handler must NOT reflect payload bytes: {raw}",
    );
    match resp {
        relayterm_protocol::ServerMsg::Error { code, message } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::PtyNotLive);
            let lower = message.to_lowercase();
            assert!(
                lower.contains("pty") || lower.contains("live"),
                "input rejection must signal pty-not-live: {message}",
            );
        }
        other => panic!("expected Error frame, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_malformed_binary_frame_is_rejected_without_echo(pool: PgPool) {
    use tokio_tungstenite::tungstenite::Message;
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary-bad.example.com",
        "SHA256:ws-binary-bad",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    // Sentinel bytes (no RTB1 magic) — the server must reject without
    // reflecting any portion of the payload in its error envelope.
    let sentinel = b"REDACT-MARKER-BINARY-FRAME-22EE";
    socket
        .send(Message::Binary(sentinel.to_vec().into()))
        .await
        .unwrap();
    let resp = recv_server_msg(&mut socket).await;
    let raw = serde_json::to_string(&resp).unwrap();
    let sentinel_str = std::str::from_utf8(sentinel).unwrap();
    assert!(
        !raw.contains(sentinel_str),
        "malformed binary frame rejection must NOT echo payload bytes: {raw}",
    );
    match resp {
        relayterm_protocol::ServerMsg::Error { code, .. } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::InvalidMessage);
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_oversized_binary_frame_is_rejected_safely(pool: PgPool) {
    use tokio_tungstenite::tungstenite::Message;
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary-huge.example.com",
        "SHA256:ws-binary-huge",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    // Build a header that CLAIMS u32::MAX bytes of payload. The decoder
    // must reject on the length cap BEFORE allocating, so the server
    // does not fall over and we get a typed error frame back.
    let mut buf = Vec::with_capacity(relayterm_protocol::BINARY_HEADER_LEN);
    buf.extend_from_slice(&relayterm_protocol::BINARY_MAGIC_V1);
    buf.push(relayterm_protocol::BinaryFrameKind::Input.as_u8());
    buf.push(0);
    buf.extend_from_slice(&[0u8, 0u8]);
    buf.extend_from_slice(&0u64.to_be_bytes());
    buf.extend_from_slice(&u32::MAX.to_be_bytes());
    socket.send(Message::Binary(buf.into())).await.unwrap();
    let resp = recv_server_msg(&mut socket).await;
    match resp {
        relayterm_protocol::ServerMsg::Error { code, .. } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::InvalidMessage);
        }
        other => panic!("expected Error, got {other:?}"),
    }
    // Bridge must NOT have observed any input — the malformed frame is
    // dropped before the manager is touched.
    let handle = bridge.last_handle().expect("bridge produced handle");
    assert!(
        handle.input_log().is_empty(),
        "no bytes should reach the bridge after a rejected binary frame",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_binary_input_reaches_live_pty(pool: PgPool) {
    use tokio_tungstenite::tungstenite::Message;
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary-input.example.com",
        "SHA256:ws-binary-input",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().expect("bridge produced handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(Active)

    // Non-UTF-8 bytes — the binary path must carry them losslessly.
    let payload = b"\x1bOP\x00\xff arrow-up?".to_vec();
    let frame = relayterm_protocol::encode_binary_frame(
        relayterm_protocol::BinaryFrameKind::Input,
        0,
        &payload,
    )
    .unwrap();
    socket.send(Message::Binary(frame.into())).await.unwrap();

    // Wait briefly for the manager forwarder to land the bytes on the
    // bridge. The fake handle records each write.
    for _ in 0..200 {
        if !handle.input_log().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let log = handle.input_log();
    let combined: Vec<u8> = log.iter().flat_map(|chunk| chunk.iter().copied()).collect();
    assert_eq!(combined, payload, "fake bridge must observe exact bytes");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_invalid_message_returns_typed_error_without_echo(pool: PgPool) {
    use tokio_tungstenite::tungstenite::Message;
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-bad-msg.example.com",
        "SHA256:ws-bad-msg",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    let sentinel = "REDACT-MARKER-BAD-FRAME-A11C";
    socket
        .send(Message::Text(
            format!("{{\"type\":\"totally-bogus\",\"data\":\"{sentinel}\"}}").into(),
        ))
        .await
        .unwrap();
    let resp = recv_server_msg(&mut socket).await;
    let raw = serde_json::to_string(&resp).unwrap();
    assert!(
        !raw.contains(sentinel),
        "invalid_message handler must NOT reflect frame bytes: {raw}",
    );
    match resp {
        relayterm_protocol::ServerMsg::Error { code, .. } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::InvalidMessage);
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_detach_writes_detached_event_and_closes(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-detach.example.com",
        "SHA256:ws-detach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let attached = recv_server_msg(&mut socket).await;
    let attachment_id = match attached {
        relayterm_protocol::ServerMsg::SessionAttached { attachment_id, .. } => attachment_id,
        other => panic!("expected SessionAttached, got {other:?}"),
    };

    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Detach).await;
    let resp = recv_server_msg(&mut socket).await;
    match resp {
        relayterm_protocol::ServerMsg::SessionDetached {
            session_id: got_session,
            attachment_id: got_attachment,
        } => {
            assert_eq!(got_session, session_id);
            assert_eq!(got_attachment, attachment_id);
        }
        other => panic!("expected SessionDetached, got {other:?}"),
    }

    // No `SessionClosed` is emitted: per the detached-session TTL
    // contract the PTY survives the bounded reconnect window. A second
    // recv on the socket must observe the server-initiated close
    // (Message::Close) rather than another typed frame.
    while (socket.next().await).is_some() {}

    // The attachment row's detached_at is stamped.
    let attachments = PgTerminalSessionRepository::new(pool.clone())
        .list_attachments(session_id)
        .await
        .unwrap();
    assert_eq!(attachments.len(), 1);
    assert!(attachments[0].detached_at.is_some());

    // Exactly one Detached event was written; NO Closed event yet.
    let events = PgSessionEventRepository::new(pool.clone())
        .list_for_session(session_id)
        .await
        .unwrap();
    let detached = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Detached)
        .count();
    let closed = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(detached, 1, "exactly one Detached event must be written");
    assert_eq!(
        closed, 0,
        "TTL window has not expired; Detach must NOT close the session",
    );
    // The session row itself is in the Detached state, not Closed.
    let row = PgTerminalSessionRepository::new(pool)
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Detached);
    assert!(row.closed_at.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_close_transitions_session_and_emits_session_closed(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-close.example.com",
        "SHA256:ws-close",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Close).await;
    let resp = recv_server_msg(&mut socket).await;
    match resp {
        relayterm_protocol::ServerMsg::SessionClosed {
            session_id: got_session,
        } => {
            assert_eq!(got_session, session_id);
        }
        other => panic!("expected SessionClosed, got {other:?}"),
    }

    // The session row is now closed.
    let row = PgTerminalSessionRepository::new(pool.clone())
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Closed);
    assert!(row.closed_at.is_some());

    // The closed event was written.
    let events = PgSessionEventRepository::new(pool)
        .list_for_session(session_id)
        .await
        .unwrap();
    let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&SessionEventKind::Closed));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_socket_drop_marks_attachment_detached(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-drop.example.com",
        "SHA256:ws-drop",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    // Drop the socket without sending Detach. The handler's cleanup
    // tail must still write the detach bookkeeping so the audit row
    // reflects the disconnect.
    socket.close(None).await.unwrap();
    drop(socket);

    // Poll briefly for the detached_at write — the handler runs on a
    // separate task, so this is the natural "wait for cleanup" point.
    let attachment = PgTerminalSessionRepository::new(pool.clone());
    let mut detached_at = None;
    for _ in 0..50 {
        let rows = attachment.list_attachments(session_id).await.unwrap();
        if let Some(row) = rows.into_iter().next() {
            if row.detached_at.is_some() {
                detached_at = row.detached_at;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        detached_at.is_some(),
        "socket drop must surface as detached_at on the attachment row",
    );

    let events = PgSessionEventRepository::new(pool.clone())
        .list_for_session(session_id)
        .await
        .unwrap();
    let detached = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Detached)
        .count();
    assert_eq!(
        detached, 1,
        "socket drop must append a single Detached event"
    );
    // Per the detached-session TTL contract a socket drop on the last
    // live attachment leaves the PTY alive within `DETACHED_LIVE_PTY_TTL`.
    // No `Closed` event is produced unless the timer expires or the
    // operator issues an explicit close.
    let closed = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed, 0,
        "socket drop must NOT close the session within the TTL window",
    );
    let row = PgTerminalSessionRepository::new(pool)
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Detached);
    assert!(row.closed_at.is_none());
}

// ----------------------------------------------------------------------
// Live SSH PTY bridge — integration with the FakePtyBridge
// ----------------------------------------------------------------------

/// Build the standard router with a [`FakePtyBridge`] of the caller's
/// choosing wired into AppState.
async fn setup_with_pty_bridge(
    pool: PgPool,
    bridge: Arc<FakePtyBridge>,
) -> (Router, UserId, String) {
    setup_with_full_state(
        pool,
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge as Arc<dyn SshPtyBridge>,
    )
    .await
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_starts_live_pty_when_trusted_and_auth_ready(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "live.example.com",
        "SHA256:live-create",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id, "cols": 132, "rows": 50}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;

    // Live response shape: status=active, pty_live=true, conservative wording.
    assert_eq!(body["status"], "active");
    assert_eq!(body["pty_live"], true);
    let message = body["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("ssh pty started") && message.contains("replay"),
        "live create message must announce pty + caveat replay, got: {message}",
    );
    for forbidden in ["pty startup is not implemented", "logged in", "shell ready"] {
        assert!(
            !message.contains(forbidden),
            "create message must not contain `{forbidden}`: {message}",
        );
    }

    // Body must NOT contain key material or PEM markers.
    let raw = body.to_string();
    for forbidden in [
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
        "owner_id",
    ] {
        assert!(
            !raw.contains(forbidden),
            "create body must not contain `{forbidden}`: {raw}",
        );
    }

    // Bridge was called once with the trusted pin and a non-empty PEM.
    let records = bridge.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].cols, 132);
    assert_eq!(records[0].rows, 50);
    assert_eq!(records[0].accept_pin_count, 1);
    assert!(
        records[0].pem_len > 0,
        "bridge must receive a non-empty PEM"
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_with_unknown_host_key_blocks_before_bridge(pool: PgPool) {
    // No trusted entry → API returns 409 BEFORE the bridge is called.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "untrusted2.example.com",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        bridge.call_count(),
        0,
        "bridge must not be called before host-key trust",
    );
    // No row was inserted.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM terminal_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_with_bridge_host_key_failure_returns_409(pool: PgPool) {
    let bridge =
        FakePtyBridge::with_outcome(FakePtyOutcome::Failure(FakePtyFailure::HostKeyNotTrusted));
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "race.example.com",
        "SHA256:race",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("host_key"),
        "host-key conflict must surface, got: {}",
        body["error"]["message"]
    );

    // Row was created and then closed-with-reason for audit.
    let row: (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, closed_at FROM terminal_sessions LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, "closed");
    assert!(row.1.is_some());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_with_bridge_auth_failure_returns_conflict(pool: PgPool) {
    let bridge = FakePtyBridge::with_outcome(FakePtyOutcome::Failure(
        FakePtyFailure::AuthenticationFailed,
    ));
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "auth-fail.example.com",
        "SHA256:auth-fail",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    let raw = body.to_string();
    for forbidden in [
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
    ] {
        assert!(
            !raw.contains(forbidden),
            "auth-fail body must not contain `{forbidden}`: {raw}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_with_bridge_transport_failure_returns_502(pool: PgPool) {
    let bridge = FakePtyBridge::with_outcome(FakePtyOutcome::Failure(FakePtyFailure::Transport));
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "transport-fail.example.com",
        "SHA256:transport-fail",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_returns_503_when_vault_disabled(pool: PgPool) {
    // Vault disabled → 503 BEFORE the bridge is called.
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool.clone());
    let bridge = FakePtyBridge::new();
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, user_id).await;
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: bridge.clone() as Arc<dyn SshPtyBridge>,
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "vault-off.example.com",
        "SHA256:vault-off",
    )
    .await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        bridge.call_count(),
        0,
        "vault-disabled path must not reach the bridge",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_emits_session_attached_active_when_pty_live(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-active.example.com",
        "SHA256:ws-active",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let msg = recv_server_msg(&mut socket).await;
    match msg {
        relayterm_protocol::ServerMsg::SessionAttached {
            status, message, ..
        } => {
            assert_eq!(status, relayterm_protocol::SessionAttachStatus::Active);
            let lower = message.to_lowercase();
            assert!(
                lower.contains("live") || lower.contains("ssh"),
                "active attach message must indicate liveness, got: {message}",
            );
            assert!(
                lower.contains("replay"),
                "active attach must caveat replay, got: {message}",
            );
        }
        other => panic!("expected SessionAttached, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_input_forwards_to_live_pty_without_echoing_payload(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-input.example.com",
        "SHA256:ws-input",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(Active)

    let sentinel = "REDACT-MARKER-INPUT-LIVE-7C";
    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Input {
            data: sentinel.to_owned(),
        },
    )
    .await;

    // Poll for the fake handle to record the input. There's no echo
    // frame to wait on — the contract is "no reply on success".
    let handle = {
        let mut out = None;
        for _ in 0..50 {
            if let Some(h) = bridge.last_handle() {
                if !h.input_log().is_empty() {
                    out = Some(h);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        out.expect("input must reach the fake handle within budget")
    };
    let recorded = handle.input_log();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], sentinel.as_bytes());

    // Confirm no reflection of the input in any subsequent server frame.
    // We don't expect any frame at all — assert the socket is quiet.
    let timeout = tokio::time::timeout(std::time::Duration::from_millis(100), socket.next());
    if let Ok(Some(Ok(frame))) = timeout.await {
        let raw = format!("{frame:?}");
        assert!(
            !raw.contains(sentinel),
            "no server frame may echo the input payload: {raw}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_output_from_pty_reaches_attached_client(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-output.example.com",
        "SHA256:ws-output",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge
        .last_handle()
        .expect("create flow must produce a handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(Active)

    // Inject raw PTY bytes from the fake bridge — the orchestrator's
    // forwarder fans them out to the broadcast, which the WS handler
    // subscribes to.
    let payload = b"\xfeNON-UTF8\x80hello".to_vec();
    handle.inject_output(payload.clone()).await;

    // The next server frame on the socket must be an Output frame whose
    // base64 data round-trips to our injected bytes.
    let msg = recv_server_msg(&mut socket).await;
    match msg {
        relayterm_protocol::ServerMsg::Output { seq, data } => {
            let decoded = relayterm_protocol::output_data_decode(&data)
                .expect("output data must be valid base64");
            assert_eq!(decoded, payload);
            assert!(seq.0 >= 1, "seq must be monotonic from 1");
        }
        other => panic!("expected Output, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_resize_forwards_to_live_pty(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-resize.example.com",
        "SHA256:ws-resize",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(Active)

    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Resize {
            cols: 200,
            rows: 60,
        },
    )
    .await;

    // Wait for the Ack frame, which proves the manager processed the
    // resize. The fake handle records the call.
    let msg = recv_server_msg(&mut socket).await;
    match msg {
        relayterm_protocol::ServerMsg::Ack {
            kind: relayterm_protocol::AckKind::Resize,
        } => {}
        other => panic!("expected Ack(resize), got {other:?}"),
    }
    let resizes = handle.resize_log();
    assert!(
        resizes.contains(&(200, 60)),
        "fake handle must record the (cols, rows) pair, got {resizes:?}",
    );
}

// ----------------------------------------------------------------------
// Live SSH PTY bridge — final-detach TTL / reconnect lifecycle
// ----------------------------------------------------------------------

/// Drive a fresh WS attach against the supplied router and return the
/// open socket. The first server frame (SessionAttached) is consumed.
async fn open_ws_attached(
    addr: SocketAddr,
    session_id: relayterm_core::ids::TerminalSessionId,
    cookie: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut socket = open_ws(addr, session_id, cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(Active)
    socket
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_explicit_close_remains_idempotent(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-close-idempotent.example.com",
        "SHA256:ws-close-idempotent",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    // First WS: explicit Close.
    let addr = spawn_app(app.clone()).await;
    let mut s1 = open_ws_attached(addr, session_id, &cookie).await;
    send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Close).await;
    let resp = recv_server_msg(&mut s1).await;
    match resp {
        relayterm_protocol::ServerMsg::SessionClosed { .. } => {}
        other => panic!("expected SessionClosed, got {other:?}"),
    }

    // Second close via the HTTP route is idempotent: same shape, no new event.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["already_closed"], true);

    let closed = PgSessionEventRepository::new(pool)
        .list_for_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed, 1,
        "double close must write exactly one Closed event"
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_socket_drop_after_explicit_detach_does_not_duplicate_events(pool: PgPool) {
    // Race: client sends `Detach`, the server emits SessionDetached and
    // closes the WS. The cleanup tail still runs (no explicit Close
    // from the client). It MUST observe state.detached and skip — only
    // one Detached event must land, the TTL close stays scheduled
    // exactly once (no duplicate timer), and no Closed event has been
    // written yet.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-detach-race.example.com",
        "SHA256:ws-detach-race",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id, &cookie).await;
    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Detach).await;

    // Drain the server frames and let the WS task finish its cleanup.
    while (socket.next().await).is_some() {}

    // Settle: the cleanup tail runs after the loop exits.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = PgSessionEventRepository::new(pool.clone())
        .list_for_session(session_id)
        .await
        .unwrap();
    let detached = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Detached)
        .count();
    let closed = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        detached, 1,
        "Detach + cleanup-tail race must write exactly one Detached event",
    );
    assert_eq!(
        closed, 0,
        "TTL has not expired; no Closed event must exist after the race",
    );
    let row = PgTerminalSessionRepository::new(pool)
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Detached);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_reattach_after_ttl_expiry_returns_409(pool: PgPool) {
    // After socket-drop, the PTY survives the bounded TTL window. Once
    // the timer fires the session row transitions to Closed and a new
    // WS upgrade for the same id must surface 409. Uses a sub-second
    // detach TTL so the test runs in well under a second.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_full_state_short_ttl(
        pool.clone(),
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge.clone() as Arc<dyn SshPtyBridge>,
        std::time::Duration::from_millis(120),
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-reattach.example.com",
        "SHA256:ws-reattach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id, &cookie).await;
    socket.close(None).await.unwrap();
    drop(socket);

    // Wait for the TTL timer to fire and close the session.
    let repo = PgTerminalSessionRepository::new(pool.clone());
    for _ in 0..40 {
        let row = repo.get(session_id).await.unwrap().unwrap();
        if row.status == TerminalSessionStatus::Closed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let row = repo.get(session_id).await.unwrap().unwrap();
    assert_eq!(
        row.status,
        TerminalSessionStatus::Closed,
        "TTL expiry must close the session before the reattach probe",
    );

    // A reattach must surface 409 — the WS upgrade gate sees the closed row.
    let (status, _body) = ws_handshake_status(addr, &session_id.to_string(), Some(&cookie)).await;
    assert_eq!(
        status,
        axum::http::StatusCode::CONFLICT,
        "reattach to a TTL-expired session must return 409",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_input_after_ttl_expiry_does_not_reach_bridge(pool: PgPool) {
    // After the TTL fires the PTY runtime is gone. The in-flight
    // WebSocket has already been closed by the server when Detach
    // landed, so the only surface that could reach the bridge is a
    // fresh upgrade — and the upgrade gate refuses with 409 (asserted
    // separately). This pins the bridge-side invariant: no input
    // bytes appear on the FakePtyBridge handle after a TTL-expired
    // session's lifecycle settles.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_full_state_short_ttl(
        pool.clone(),
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge.clone() as Arc<dyn SshPtyBridge>,
        std::time::Duration::from_millis(120),
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-input-after.example.com",
        "SHA256:ws-input-after",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().expect("create produced a handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id, &cookie).await;
    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Detach).await;
    while (socket.next().await).is_some() {}

    // Wait for the TTL timer to fire and close the session.
    for _ in 0..40 {
        let row = PgTerminalSessionRepository::new(pool.clone())
            .get(session_id)
            .await
            .unwrap()
            .unwrap();
        if row.status == TerminalSessionStatus::Closed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(
        handle.input_log().is_empty(),
        "no input bytes should reach the bridge after final detach + TTL expiry",
    );
}

// ----------------------------------------------------------------------
// Replay buffer / sequence-number wire path
// ----------------------------------------------------------------------

/// Drain the socket until an `Output` frame whose seq matches
/// `expected_seq` arrives. The forwarder runs on a separate task so a
/// just-injected frame may need a few scheduler turns to appear. Returns
/// the decoded payload bytes for the asserted frame.
async fn await_output_with_seq(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    expected_seq: u64,
) -> Vec<u8> {
    for _ in 0..200 {
        let msg = recv_server_msg(socket).await;
        match msg {
            relayterm_protocol::ServerMsg::Output { seq, data } if seq.0 == expected_seq => {
                return relayterm_protocol::output_data_decode(&data)
                    .expect("output data must be valid base64");
            }
            relayterm_protocol::ServerMsg::Output { .. } => continue,
            relayterm_protocol::ServerMsg::Pong => continue,
            other => {
                panic!("unexpected frame while awaiting Output(seq={expected_seq}): {other:?}")
            }
        }
    }
    panic!("never received Output frame with seq={expected_seq}");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_live_output_carries_monotonic_seq_starting_at_one(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-seq.example.com",
        "SHA256:ws-seq",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached

    // Inject three output frames; the wire MUST carry seq=1, 2, 3.
    handle.inject_output(b"first".to_vec()).await;
    handle.inject_output(b"second".to_vec()).await;
    handle.inject_output(b"third".to_vec()).await;

    let mut seqs: Vec<u64> = Vec::new();
    let mut datas: Vec<Vec<u8>> = Vec::new();
    while seqs.len() < 3 {
        let msg = recv_server_msg(&mut socket).await;
        match msg {
            relayterm_protocol::ServerMsg::Output { seq, data } => {
                seqs.push(seq.0);
                datas.push(relayterm_protocol::output_data_decode(&data).unwrap());
            }
            relayterm_protocol::ServerMsg::Pong => continue,
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(
        datas,
        vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_with_last_seen_seq_replays_buffered_frames(pool: PgPool) {
    // First socket primes the replay buffer with frames 1..=3, then
    // detaches. A second socket attaches and explicitly sends
    // `Attach { last_seen_seq: 1 }` — the server MUST emit
    // ReplayStart{2,3}, Output(2), Output(3), ReplayEnd{3}.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-replay.example.com",
        "SHA256:ws-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    // First attach: prime the replay buffer with three frames. We do
    // NOT send Detach (which would auto-close the live session); we
    // just drop the socket and wait for the cleanup tail to finish.
    {
        let mut s1 = open_ws(addr, session_id, &cookie).await;
        let _ = recv_server_msg(&mut s1).await;
        handle.inject_output(b"alpha".to_vec()).await;
        handle.inject_output(b"beta".to_vec()).await;
        handle.inject_output(b"gamma".to_vec()).await;
        let _ = await_output_with_seq(&mut s1, 3).await;
        // Drop without explicit Detach is also problematic — it triggers
        // auto-close. Use explicit Detach but only if we're certain the
        // session will remain alive for the second socket via another
        // attachment.
        // → To keep the live PTY alive across detach, hold s1 open until
        //   AFTER s2 has attached, then detach s1 cleanly.
        // Open s2 first:
        let mut s2 = open_ws(addr, session_id, &cookie).await;
        let _ = recv_server_msg(&mut s2).await; // SessionAttached(Active)
        send_client_msg(
            &mut s2,
            &relayterm_protocol::ClientMsg::Attach {
                session_id: Some(session_id),
                last_seen_seq: Some(relayterm_core::SeqNo(1)),
                client_id: Some("replay-test/1.0".to_owned()),
            },
        )
        .await;

        // Expect: ReplayStart { from_seq: 2, to_seq: 3 }, Output(2),
        // Output(3), ReplayEnd { latest_seq: 3 }.
        let start = recv_server_msg(&mut s2).await;
        match start {
            relayterm_protocol::ServerMsg::ReplayStart { from_seq, to_seq } => {
                assert_eq!(from_seq.0, 2);
                assert_eq!(to_seq.0, 3);
            }
            other => panic!("expected ReplayStart, got {other:?}"),
        }
        let f2 = recv_server_msg(&mut s2).await;
        match f2 {
            relayterm_protocol::ServerMsg::Output { seq, data } => {
                assert_eq!(seq.0, 2);
                assert_eq!(
                    relayterm_protocol::output_data_decode(&data).unwrap(),
                    b"beta"
                );
            }
            other => panic!("expected Output(2), got {other:?}"),
        }
        let f3 = recv_server_msg(&mut s2).await;
        match f3 {
            relayterm_protocol::ServerMsg::Output { seq, data } => {
                assert_eq!(seq.0, 3);
                assert_eq!(
                    relayterm_protocol::output_data_decode(&data).unwrap(),
                    b"gamma"
                );
            }
            other => panic!("expected Output(3), got {other:?}"),
        }
        let end = recv_server_msg(&mut s2).await;
        match end {
            relayterm_protocol::ServerMsg::ReplayEnd { latest_seq } => {
                assert_eq!(latest_seq.0, 3);
            }
            other => panic!("expected ReplayEnd, got {other:?}"),
        }

        // Drop both sockets cleanly.
        drop(s2);
        drop(s1);
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_without_last_seen_seq_does_not_dump_old_output(pool: PgPool) {
    // A brand-new attach (no last_seen_seq) must NOT receive replayed
    // frames — even when the buffer has them.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-replay.example.com",
        "SHA256:ws-no-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    // Prime via s1, hold open.
    let mut s1 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s1).await;
    handle.inject_output(b"old-output".to_vec()).await;
    let _ = await_output_with_seq(&mut s1, 1).await;

    // Second socket: attach, explicitly send Attach { last_seen_seq:
    // None }. Server MUST NOT emit any replay frames.
    let mut s2 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s2).await;
    send_client_msg(
        &mut s2,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: Some(session_id),
            last_seen_seq: None,
            client_id: None,
        },
    )
    .await;

    // Inject a NEW frame; s2 must see ONLY the new live frame (seq=2),
    // not the prior buffered frame.
    handle.inject_output(b"new-live".to_vec()).await;
    let bytes = await_output_with_seq(&mut s2, 2).await;
    assert_eq!(bytes, b"new-live");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_with_in_bounds_bookmark_does_not_emit_replay_window_lost(pool: PgPool) {
    // The buffer is bounded; force a tiny window indirectly: just attach
    // with a bookmark we know is older than anything in the buffer (the
    // PTY emits frames 1..=2; bookmark=1 is recoverable, but we force
    // the lost path by asking for bookmark older than 1 is impossible
    // since seq starts at 1). Instead, attach with bookmark > what was
    // actually streamed: that doesn't trigger window lost (caller is
    // ahead). To genuinely trigger window lost we need a bookmark older
    // than the oldest retained frame. Default config retains 1024 frames
    // / 1 MiB, so a small test cannot evict naturally.
    //
    // Strategy: prime a single frame seq=1, then attach with bookmark=
    // u64::MAX/2 — that's "ahead of latest" which is empty range, NOT
    // window lost. So instead, validate the inverse: there is no path in
    // this slice that produces ReplayWindowLost without artificially
    // shrinking the buffer or emitting >1024 frames. That coverage lives
    // in the unit tests on `ReplayBuffer::replay_since`. The wire-side
    // coverage we CAN provide here is "replay_window_lost is never
    // emitted on a normal in-bounds attach."
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-window-lost.example.com",
        "SHA256:ws-no-window-lost",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    let mut s1 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s1).await;
    handle.inject_output(b"only".to_vec()).await;
    let _ = await_output_with_seq(&mut s1, 1).await;

    // Bookmark equals latest → empty range (no replay frames at all,
    // and definitely no window-lost).
    let mut s2 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s2).await;
    send_client_msg(
        &mut s2,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: Some(session_id),
            last_seen_seq: Some(relayterm_core::SeqNo(1)),
            client_id: None,
        },
    )
    .await;

    // Inject a new frame — the next frame on s2 should be Output(2),
    // never a ReplayStart or ReplayEnd or ReplayWindowLost.
    handle.inject_output(b"next".to_vec()).await;
    let msg = recv_server_msg(&mut s2).await;
    match msg {
        relayterm_protocol::ServerMsg::Output { seq, data } => {
            assert_eq!(seq.0, 2);
            assert_eq!(
                relayterm_protocol::output_data_decode(&data).unwrap(),
                b"next"
            );
        }
        other => panic!("bookmark==latest must skip the replay handshake entirely; got {other:?}",),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_replay_does_not_double_deliver_buffered_frames(pool: PgPool) {
    // After a successful replay handshake, the live broadcast subscriber
    // has been queueing the SAME frames in parallel. The handler MUST
    // drop frames whose seq <= range.latest_seq so the renderer doesn't
    // see the replayed frames twice.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-dup-replay.example.com",
        "SHA256:ws-no-dup-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    let mut s1 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s1).await;
    for byte in [b'a', b'b'] {
        handle.inject_output(vec![byte]).await;
    }
    let _ = await_output_with_seq(&mut s1, 2).await;

    let mut s2 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s2).await;
    // Bookmark = 1 → server replays only frame seq=2. The broadcast
    // subscriber for s2 has been queueing frames 1 AND 2 since attach
    // (the manager pushes to the broadcast on every output), so the
    // handler MUST raise its `min_live_seq` floor to range.latest_seq=2
    // BEFORE emitting the replay or the queued live frames will be
    // double-delivered after the replay drain finishes.
    send_client_msg(
        &mut s2,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: Some(session_id),
            last_seen_seq: Some(relayterm_core::SeqNo(1)),
            client_id: None,
        },
    )
    .await;

    // Drain replay frames: ReplayStart, Output(2), ReplayEnd.
    let _ = recv_server_msg(&mut s2).await; // ReplayStart
    let _ = recv_server_msg(&mut s2).await; // Output(2)
    let _ = recv_server_msg(&mut s2).await; // ReplayEnd

    // Inject ONE more live frame; the next visible Output on s2 must be
    // seq=3, not a duplicated seq=2.
    handle.inject_output(b"c".to_vec()).await;
    let msg = recv_server_msg(&mut s2).await;
    match msg {
        relayterm_protocol::ServerMsg::Output { seq, .. } => {
            assert_eq!(
                seq.0, 3,
                "post-replay live frame must skip past the replayed window",
            );
        }
        other => panic!("expected Output(3), got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_second_explicit_attach_is_rejected(pool: PgPool) {
    // The first explicit Attach after upgrade is accepted (replay
    // handshake). A second explicit Attach is a protocol violation:
    // server MUST emit error { code: invalid_message, message:
    // "already attached" } and keep the socket open.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-double-attach.example.com",
        "SHA256:ws-double-attach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut socket).await;

    // First Attach with no bookmark — accepted, no reply.
    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: None,
            last_seen_seq: None,
            client_id: None,
        },
    )
    .await;
    // Second Attach — rejected.
    send_client_msg(
        &mut socket,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: None,
            last_seen_seq: None,
            client_id: None,
        },
    )
    .await;

    let resp = recv_server_msg(&mut socket).await;
    match resp {
        relayterm_protocol::ServerMsg::Error { code, message } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::InvalidMessage);
            assert!(
                message.to_lowercase().contains("already attached"),
                "second attach must signal already-attached: {message}",
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_replay_messages_do_not_leak_payload_in_serialization(pool: PgPool) {
    // Make sure the replay path never round-trips raw bytes through any
    // `Debug` / `Display` surface that gets logged. We assert at the
    // wire serialization layer: a sentinel byte sequence injected into
    // the PTY must never appear in the JSON-serialized replay control
    // frames (ReplayStart / ReplayEnd). Output frames CARRY the bytes
    // (base64) by design — that's their purpose — but the bracketing
    // frames must stay metadata-only.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-replay-redact.example.com",
        "SHA256:ws-replay-redact",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    let mut s1 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s1).await;
    let sentinel = b"REDACT-MARKER-REPLAY-SHELL-9F";
    // Two frames so a `last_seen_seq=1` bookmark triggers replay of
    // exactly one buffered frame (the sentinel-bearing seq=2). Using
    // a positive bookmark — `Some(0)` is treated as no-bookmark by
    // the handler and would skip the replay handshake entirely.
    handle.inject_output(b"prefix".to_vec()).await;
    handle.inject_output(sentinel.to_vec()).await;
    let _ = await_output_with_seq(&mut s1, 2).await;

    let mut s2 = open_ws(addr, session_id, &cookie).await;
    let _ = recv_server_msg(&mut s2).await;
    send_client_msg(
        &mut s2,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: Some(session_id),
            last_seen_seq: Some(relayterm_core::SeqNo(1)),
            client_id: None,
        },
    )
    .await;

    let start = recv_server_msg(&mut s2).await;
    let end = {
        // Drain Output(2) between Start and End.
        let _ = recv_server_msg(&mut s2).await; // Output(2)
        recv_server_msg(&mut s2).await
    };
    let start_json = serde_json::to_string(&start).unwrap();
    let end_json = serde_json::to_string(&end).unwrap();
    let sentinel_str = std::str::from_utf8(sentinel).unwrap();
    assert!(
        !start_json.contains(sentinel_str),
        "ReplayStart wire payload must be metadata only: {start_json}",
    );
    assert!(
        !end_json.contains(sentinel_str),
        "ReplayEnd wire payload must be metadata only: {end_json}",
    );
}

// ----------------------------------------------------------------------
// Detached-session TTL: reconnect within the window, expire after it
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_detach_keeps_pty_alive_within_ttl_window(pool: PgPool) {
    // Final detach must transition the row to Detached without closing
    // the PTY. The bridge handle stays live until the TTL expires or
    // an explicit close arrives. Uses a generous TTL so the assertion
    // observes the live state without racing the timer.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_full_state_short_ttl(
        pool.clone(),
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge.clone() as Arc<dyn SshPtyBridge>,
        std::time::Duration::from_secs(2),
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-ttl-alive.example.com",
        "SHA256:ws-ttl-alive",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().expect("bridge produced handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id, &cookie).await;
    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Detach).await;
    while (socket.next().await).is_some() {}

    // Settle: the cleanup tail runs after the loop exits.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Row is Detached, NOT Closed.
    let row = PgTerminalSessionRepository::new(pool)
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Detached);
    assert!(row.closed_at.is_none());
    // Bridge handle has not been closed (no close call recorded yet).
    assert!(
        !handle.was_closed(),
        "PTY bridge must not be closed during the TTL window",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_reattach_within_ttl_resumes_active_session(pool: PgPool) {
    // After detach, a fresh WS upgrade within the TTL window must
    // succeed and the row must transition back to Active.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_full_state_short_ttl(
        pool.clone(),
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge.clone() as Arc<dyn SshPtyBridge>,
        std::time::Duration::from_secs(2),
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-ttl-reattach.example.com",
        "SHA256:ws-ttl-reattach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app).await;

    {
        let mut s1 = open_ws_attached(addr, session_id, &cookie).await;
        send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Detach).await;
        while (s1.next().await).is_some() {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Reattach within the TTL window.
    let mut s2 = open_ws(addr, session_id, &cookie).await;
    let attached = recv_server_msg(&mut s2).await;
    match attached {
        relayterm_protocol::ServerMsg::SessionAttached { status, .. } => {
            assert_eq!(
                status,
                relayterm_protocol::SessionAttachStatus::Active,
                "reattach within TTL must surface Active status",
            );
        }
        other => panic!("expected SessionAttached, got {other:?}"),
    }
    let row = PgTerminalSessionRepository::new(pool.clone())
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status,
        TerminalSessionStatus::Active,
        "reattach must transition the row back to Active",
    );
    let kinds: Vec<_> = PgSessionEventRepository::new(pool)
        .list_for_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(
        kinds.contains(&SessionEventKind::Reattached),
        "reattach must append a Reattached event, got {kinds:?}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_reattach_with_last_seen_seq_replays_missed_output_within_ttl(pool: PgPool) {
    // Prime the buffer with frames 1..=2 via s1, detach without
    // closing, then reattach with `last_seen_seq=1` — the server must
    // emit ReplayStart{2,2}, Output(2), ReplayEnd{2}.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_full_state_short_ttl(
        pool.clone(),
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge.clone() as Arc<dyn SshPtyBridge>,
        std::time::Duration::from_secs(2),
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-ttl-replay.example.com",
        "SHA256:ws-ttl-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    {
        let mut s1 = open_ws_attached(addr, session_id, &cookie).await;
        handle.inject_output(b"alpha".to_vec()).await;
        handle.inject_output(b"beta".to_vec()).await;
        let _ = await_output_with_seq(&mut s1, 2).await;
        send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Detach).await;
        while (s1.next().await).is_some() {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut s2 = open_ws(addr, session_id, &cookie).await;
    // Assert the upgrade-time attach landed as Active so a future
    // change to the upgrade frame doesn't silently pass on the wrong
    // shape.
    match recv_server_msg(&mut s2).await {
        relayterm_protocol::ServerMsg::SessionAttached { status, .. } => {
            assert_eq!(
                status,
                relayterm_protocol::SessionAttachStatus::Active,
                "reattach within TTL must surface Active status",
            );
        }
        other => panic!("expected SessionAttached(Active), got {other:?}"),
    }
    send_client_msg(
        &mut s2,
        &relayterm_protocol::ClientMsg::Attach {
            session_id: Some(session_id),
            last_seen_seq: Some(relayterm_core::SeqNo(1)),
            client_id: Some("ttl-replay-test/1.0".to_owned()),
        },
    )
    .await;
    match recv_server_msg(&mut s2).await {
        relayterm_protocol::ServerMsg::ReplayStart { from_seq, to_seq } => {
            assert_eq!(from_seq.0, 2);
            assert_eq!(to_seq.0, 2);
        }
        other => panic!("expected ReplayStart, got {other:?}"),
    }
    match recv_server_msg(&mut s2).await {
        relayterm_protocol::ServerMsg::Output { seq, data } => {
            assert_eq!(seq.0, 2);
            assert_eq!(
                relayterm_protocol::output_data_decode(&data).unwrap(),
                b"beta"
            );
        }
        other => panic!("expected Output(2), got {other:?}"),
    }
    match recv_server_msg(&mut s2).await {
        relayterm_protocol::ServerMsg::ReplayEnd { latest_seq } => {
            assert_eq!(latest_seq.0, 2);
        }
        other => panic!("expected ReplayEnd, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_explicit_close_during_ttl_closes_immediately(pool: PgPool) {
    // Close arriving via the HTTP route during the TTL window must
    // close the session at once, cancelling the pending TTL task. No
    // duplicate Closed event lands later when the timer would have
    // expired.
    let bridge = FakePtyBridge::new();
    let (app, user_id, cookie) = setup_with_full_state_short_ttl(
        pool.clone(),
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        bridge.clone() as Arc<dyn SshPtyBridge>,
        std::time::Duration::from_millis(150),
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-ttl-explicit-close.example.com",
        "SHA256:ws-ttl-explicit-close",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;
    let addr = spawn_app(app.clone()).await;

    {
        let mut s1 = open_ws_attached(addr, session_id, &cookie).await;
        send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Detach).await;
        while (s1.next().await).is_some() {}
    }

    // Close while the TTL is still pending.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Wait past the TTL so any racing timer would have fired.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    let closed = PgSessionEventRepository::new(pool.clone())
        .list_for_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == SessionEventKind::Closed)
        .count();
    assert_eq!(
        closed, 1,
        "explicit close during TTL must produce exactly one Closed event",
    );
    let row = PgTerminalSessionRepository::new(pool)
        .get(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, TerminalSessionStatus::Closed);
}

// ----------------------------------------------------------------------
// Server profile disable / enable
//
// Backend foundation for the inventory-lifecycle disable surface. The
// route is owner-scoped; foreign-owned and missing ids collapse to a
// byte-identical 404. Disable is idempotent (preserves the original
// `disabled_at` on a redundant call); enable is idempotent. Existing
// live `terminal_sessions` are unaffected by disable — see SPEC.md
// "Inventory lifecycle and destructive-action policy".
// ----------------------------------------------------------------------

/// `disable` on an owned profile sets `disabled_at` and returns the
/// updated row. The response carries the new timestamp and never any
/// secret material.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_owned_server_profile_sets_disabled_at(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "disable-me",
        "disable.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["id"].as_str().unwrap(), profile_id.to_string());
    assert!(
        body["disabled_at"].is_string(),
        "disabled_at must be set on the response, got: {body}",
    );
    // Redaction: response must never carry private-key material under any
    // shape, even though the route never touches the vault. Sentinel-style
    // assertion; mirrors the pattern in `create_terminal_session_returns_active_*`.
    let raw = body.to_string();
    for forbidden in [
        "encrypted_private_key",
        "private_key",
        "BEGIN OPENSSH PRIVATE KEY",
    ] {
        assert!(
            !raw.contains(forbidden),
            "disable response must not contain `{forbidden}`: {raw}",
        );
    }
}

/// `POST /server-profiles/:id/disable` with a bad `Origin` MUST return
/// 403 `csrf_origin_mismatch` BEFORE any DB write — the row stays
/// enabled AND no audit event lands. Pinned at the wire level so a
/// future regression that moves the guard below the lifecycle write
/// trips this test.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_with_bad_origin_returns_403_and_writes_no_audit(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "csrf-disable",
        "csrf-disable.example.com",
    )
    .await;

    let resp = app
        .oneshot(json_post_with_origin(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
            "https://attacker.example.com",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");

    // Profile row stayed enabled.
    let row = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.disabled_at.is_none(),
        "bad-Origin disable must not stamp disabled_at",
    );

    // No `server_profile_disabled` audit row landed.
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let disabled = audit
        .iter()
        .filter(|e| e.kind == AuditEventKind::ServerProfileDisabled)
        .count();
    assert_eq!(
        disabled, 0,
        "bad-Origin disable must not append a disabled audit row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_is_idempotent_and_preserves_original_timestamp(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "idempotent",
        "idempotent.example.com",
    )
    .await;

    let resp1 = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let first = read_body(resp1).await["disabled_at"].clone();
    assert!(first.is_string());

    let resp2 = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let second = read_body(resp2).await["disabled_at"].clone();
    assert_eq!(
        first, second,
        "redundant disable must preserve the original disabled_at timestamp",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn enable_clears_disabled_at_and_is_idempotent(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "enable-me",
        "enable.example.com",
    )
    .await;

    let _ = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert!(
        body["disabled_at"].is_null(),
        "enable must clear disabled_at, got: {body}",
    );

    // Idempotent on already-enabled.
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert!(body["disabled_at"].is_null());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_unknown_profile_returns_indistinguishable_404(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bogus_body = read_body(resp).await;
    assert_eq!(bogus_body["error"]["code"], "not_found");

    // Foreign-owned id MUST produce a byte-identical body.
    let other_user = create_user(&pool, "stranger").await;
    let foreign_id = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-disable",
        "foreign-disable.example.com",
    )
    .await;
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{foreign_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let foreign_body = read_body(resp).await;
    assert_eq!(
        foreign_body, bogus_body,
        "cross-user disable 404 must match a genuine 404",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn enable_unknown_profile_returns_indistinguishable_404(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bogus_body = read_body(resp).await;

    let other_user = create_user(&pool, "stranger-enable").await;
    let foreign_id = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-enable",
        "foreign-enable.example.com",
    )
    .await;
    // Disable as the owner so a real "needs enabling" target exists.
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(foreign_id, other_user, Some(chrono::Utc::now()))
        .await
        .unwrap();
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{foreign_id}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let foreign_body = read_body(resp).await;
    assert_eq!(
        foreign_body, bogus_body,
        "cross-user enable 404 must match a genuine 404",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn list_and_get_server_profiles_include_disabled_at(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "list-disabled",
        "list.example.com",
    )
    .await;

    // Fresh profile: disabled_at present and null.
    let resp = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/server-profiles/{profile_id}"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert!(
        body.as_object().unwrap().contains_key("disabled_at"),
        "GET /server-profiles/:id must always include disabled_at: {body}",
    );
    assert!(body["disabled_at"].is_null());

    // After disable, list view reflects the timestamp.
    let _ = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    let resp = app
        .oneshot(get("/api/v1/server-profiles", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().expect("list returns array");
    let row = arr
        .iter()
        .find(|r| r["id"].as_str() == Some(&profile_id.to_string()))
        .unwrap();
    assert!(
        row["disabled_at"].is_string(),
        "list response must echo disabled_at after disable: {row}",
    );
}

// ----- Disabled-profile guards on dependent setup actions -----

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disabled_profile_blocks_terminal_session_create_with_safe_409(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    // A trusted profile would normally launch successfully; disable AFTER
    // pinning so the launch path's only failure cause is `disabled_at`.
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "blocked-launch",
        "blocked.example.com",
        "SHA256:blocked-launch",
    )
    .await;
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(profile_id, user_id, Some(chrono::Utc::now()))
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({ "server_profile_id": profile_id }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("server_profile") && msg.contains("disabled"),
        "disabled-launch 409 must name server_profile + disabled, got: {msg}",
    );
    // No bytes from any inner SSH layer, no peer banners, no secrets.
    let raw = body.to_string();
    for forbidden in ["BEGIN OPENSSH PRIVATE KEY", "private_key"] {
        assert!(!raw.contains(forbidden), "wire body leaked `{forbidden}`");
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disabled_profile_blocks_auth_check(pool: PgPool) {
    let captured_fp = "SHA256:auth-blocked";
    let (app, user_id, _checker, cookie) = setup_with_fake_auth_checker(
        pool.clone(),
        captured_for_test(captured_fp),
        AuthAttemptKind::Authenticated,
    )
    .await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "auth-blocked",
        "auth-blocked.example.com",
        captured_fp,
    )
    .await;
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(profile_id, user_id, Some(chrono::Utc::now()))
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("server_profile") && msg.contains("disabled"),
        "auth-check 409 on disabled profile must name server_profile disabled, got: {msg}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disabled_profile_blocks_host_key_preflight(pool: PgPool) {
    let (app, user_id, _probe, cookie) =
        setup_with_fake_probe(pool.clone(), "SHA256:preflight-blocked").await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "preflight-blocked",
        "preflight-blocked.example.com",
    )
    .await;
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(profile_id, user_id, Some(chrono::Utc::now()))
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("server_profile") && msg.contains("disabled"),
        "preflight 409 on disabled profile must name server_profile disabled, got: {msg}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disabled_profile_blocks_trust_host_key(pool: PgPool) {
    let fp = "SHA256:trust-blocked";
    let (app, user_id, _probe, cookie) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "trust-blocked",
        "trust-blocked.example.com",
    )
    .await;
    PgServerProfileRepository::new(pool.clone())
        .set_disabled_at(profile_id, user_id, Some(chrono::Utc::now()))
        .await
        .unwrap();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "conflict");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("server_profile") && msg.contains("disabled"),
        "trust-host-key 409 on disabled profile must name server_profile disabled, got: {msg}",
    );

    // Defence in depth: no host-key entry was pinned despite the route
    // running with a successful fake probe.
    let profile = PgServerProfileRepository::new(pool.clone())
        .get(profile_id)
        .await
        .unwrap()
        .unwrap();
    let entries = PgKnownHostEntryRepository::new(pool.clone())
        .list_for_host(profile.host_id)
        .await
        .unwrap();
    assert!(
        entries.is_empty(),
        "trust on a disabled profile must NOT pin: got {entries:?}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_after_terminal_session_create_does_not_affect_existing_session_metadata(
    pool: PgPool,
) {
    // Existing live sessions continue when their profile is disabled.
    // This slice doesn't run the live PTY in the test (would require the
    // attach surface), but we can assert the session metadata row stays
    // intact and is still readable post-disable. The runtime guarantee
    // that the WS attach is unaffected is pinned at the route layer
    // (the ws_attach upgrade gate does NOT re-check `disabled_at`).
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "post-launch-disable",
        "post-launch-disable.example.com",
        "SHA256:post-launch",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id, &cookie).await;

    // Disable AFTER launch — must not retroactively close the session.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Session row is still readable and not closed.
    let resp = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session_id}"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_ne!(
        body["status"], "closed",
        "disable must NOT retroactively close existing sessions, got: {body}",
    );
}

// ----------------------------------------------------------------------
// Server profile lifecycle audit emission
//
// Each lifecycle action — create, transition-to-disabled,
// transition-to-enabled — appends one row to `audit_events` with public
// metadata only. The payload contract excludes `private_key`,
// `encrypted_private_key`, PEM bytes, public-key bytes, terminal I/O,
// raw russh / DB error text, and any vault internal. Idempotent calls
// (redundant disable / enable) MUST NOT duplicate the event row. See
// `routes/v1/server_profiles.rs::write_lifecycle_audit` for the contract
// and SPEC.md "Server profile lifecycle audit" for the rationale.
// ----------------------------------------------------------------------

/// Forbidden substrings that must never appear in an audit payload's
/// JSON serialisation. The list mirrors the renderer-redaction sentinels
/// used by `disable_owned_server_profile_sets_disabled_at` and the
/// terminal-session create response asserts. New secret-shaped names
/// belong here so a single test catches every lifecycle audit path.
///
/// `remote_addr` and `user_agent` are listed separately rather than as
/// one hyphenated sentinel — both are real field names that could drift
/// onto a future audit payload via a one-off route-level capture path,
/// and a concatenated form would silently never match. Today's lifecycle
/// payloads carry neither.
const AUDIT_FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "encrypted_private_key",
    "private_key",
    "BEGIN OPENSSH PRIVATE KEY",
    // Auth-shaped sentinels per SPEC.md "Audit events" — extending the
    // shared backstop so any audit-redaction assert catches a future
    // route that smuggles password / session / bootstrap material into
    // a payload, regardless of which kind it emits.
    "password_hash",
    "session_token",
    "token_hash",
    "bootstrap_token",
    "argon2id",
    "client_info",
    "remote_addr",
    "user_agent",
];

fn assert_audit_payload_redacted(payload: &Value, kind: AuditEventKind) {
    let raw = payload.to_string();
    for forbidden in AUDIT_FORBIDDEN_SUBSTRINGS {
        assert!(
            !raw.contains(forbidden),
            "{kind:?} audit payload must not contain `{forbidden}`: {raw}",
            kind = kind,
            forbidden = forbidden,
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_server_profile_writes_one_audit_event(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let identity = PgSshIdentityRepository::new(pool.clone())
        .create(CreateSshIdentity {
            owner_id: user_id,
            name: "audit-create".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 AAAA-pub".to_vec(),
            encrypted_private_key: b"opaque-cipher".to_vec(),
            fingerprint_sha256: "SHA256:audit-create".to_owned(),
        })
        .await
        .unwrap();
    let host_resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Audit Host",
                "hostname": "audit-create.example.com",
                "default_username": "deploy",
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(host_resp.status(), StatusCode::CREATED);
    let host_id = read_body(host_resp).await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = app
        .oneshot(json_post(
            "/api/v1/server-profiles",
            json!({
                "name": "audit-create-profile",
                "host_id": host_id,
                "ssh_identity_id": identity.id,
                "tags": ["audit"],
            }),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    let profile_id = body["id"].as_str().unwrap().to_owned();

    let audit = PgAuditEventRepository::new(pool.clone());
    let recent = audit.recent(50).await.unwrap();
    let created_events: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::ServerProfileCreated)
        .collect();
    assert_eq!(
        created_events.len(),
        1,
        "expected exactly one server_profile_created audit row, got: {recent:?}",
    );
    let event = created_events[0];
    assert_eq!(event.actor_id, Some(user_id));
    let payload = &event.payload;
    assert_eq!(payload["server_profile_id"].as_str().unwrap(), profile_id);
    assert_eq!(payload["host_id"].as_str().unwrap(), host_id);
    assert_eq!(
        payload["ssh_identity_id"].as_str().unwrap(),
        identity.id.to_string(),
    );
    assert_eq!(payload["name"], "audit-create-profile");
    assert!(payload["disabled_at"].is_null());
    assert_audit_payload_redacted(payload, AuditEventKind::ServerProfileCreated);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_server_profile_writes_one_audit_event_only_on_transition(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "audit-disable",
        "audit-disable.example.com",
    )
    .await;

    // First disable: enabled -> disabled. One audit row appended.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Second disable: already-disabled, idempotent — must NOT append.
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let audit = PgAuditEventRepository::new(pool.clone());
    let recent = audit.recent(50).await.unwrap();
    let disabled_events: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::ServerProfileDisabled)
        .collect();
    assert_eq!(
        disabled_events.len(),
        1,
        "redundant disable must not duplicate audit rows, got: {recent:?}",
    );
    let event = disabled_events[0];
    assert_eq!(event.actor_id, Some(user_id));
    let payload = &event.payload;
    assert_eq!(
        payload["server_profile_id"].as_str().unwrap(),
        profile_id.to_string(),
    );
    assert!(
        payload["disabled_at"].is_string(),
        "disable audit must include a stamped disabled_at: {payload}",
    );
    assert!(payload["host_id"].is_string());
    assert!(payload["ssh_identity_id"].is_string());
    assert_audit_payload_redacted(payload, AuditEventKind::ServerProfileDisabled);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn enable_server_profile_writes_one_audit_event_only_on_transition(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "audit-enable",
        "audit-enable.example.com",
    )
    .await;

    // Disable first so a real transition exists.
    let _ = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();

    // First enable: disabled -> enabled. One audit row appended.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Second enable: already-enabled, idempotent — must NOT append.
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let audit = PgAuditEventRepository::new(pool.clone());
    let recent = audit.recent(50).await.unwrap();
    let enabled_events: Vec<_> = recent
        .iter()
        .filter(|e| e.kind == AuditEventKind::ServerProfileEnabled)
        .collect();
    assert_eq!(
        enabled_events.len(),
        1,
        "redundant enable must not duplicate audit rows, got: {recent:?}",
    );
    let event = enabled_events[0];
    assert_eq!(event.actor_id, Some(user_id));
    let payload = &event.payload;
    assert_eq!(
        payload["server_profile_id"].as_str().unwrap(),
        profile_id.to_string(),
    );
    assert!(
        payload["disabled_at"].is_null(),
        "enable audit captures the post-transition state: {payload}",
    );
    assert_audit_payload_redacted(payload, AuditEventKind::ServerProfileEnabled);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn no_audit_events_for_owner_scoped_404_disable(pool: PgPool) {
    // A 404 (foreign-owned or missing id) MUST NOT leak an audit row.
    // Otherwise the audit log would expose cross-user existence by id.
    let (app, _user_id, cookie) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/disable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let audit = PgAuditEventRepository::new(pool.clone());
    let recent = audit.recent(50).await.unwrap();
    let lifecycle: Vec<_> = recent
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                AuditEventKind::ServerProfileCreated
                    | AuditEventKind::ServerProfileDisabled
                    | AuditEventKind::ServerProfileEnabled,
            )
        })
        .collect();
    assert!(
        lifecycle.is_empty(),
        "404 path must not write an audit row, got: {lifecycle:?}",
    );
}

/// Symmetric coverage for the enable route. SPEC.md "Server profile
/// lifecycle audit" pins "401/404 paths write NO audit event" as a
/// load-bearing invariant on every lifecycle entry point — both routes
/// must satisfy it, not just disable.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn no_audit_events_for_owner_scoped_404_enable(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/enable"),
            json!({}),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let audit = PgAuditEventRepository::new(pool.clone());
    let recent = audit.recent(50).await.unwrap();
    let lifecycle: Vec<_> = recent
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                AuditEventKind::ServerProfileCreated
                    | AuditEventKind::ServerProfileDisabled
                    | AuditEventKind::ServerProfileEnabled,
            )
        })
        .collect();
    assert!(
        lifecycle.is_empty(),
        "enable 404 path must not write an audit row, got: {lifecycle:?}",
    );
}

// ----------------------------------------------------------------------
// GET /api/v1/audit-events/recent
//
// Read-only current-user audit feed. The route filters at the SQL layer
// via `AuditEventRepository::recent_for_actor` — a foreign-actor row
// MUST NOT reach the wire. Limit is clamped to `1..=100`. Payload is
// sanitised through `AuditEventResponse::from_event`; raw payload
// fields with secret-shaped names MUST NOT appear in the response body.
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_returns_current_user_lifecycle_events(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    // Create + disable a profile so we have two lifecycle audit rows
    // for the current user.
    let profile_id = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "audit-feed",
        "audit-feed.example.com",
    )
    .await;
    // The profile inserted via repo above does NOT route through
    // `write_lifecycle_audit`, so write the create-event manually for
    // a faithful feed shape. Use the same payload contract the route
    // emits.
    let audit = PgAuditEventRepository::new(pool.clone());
    audit
        .create(CreateAuditEvent {
            actor_id: Some(user_id),
            kind: AuditEventKind::ServerProfileCreated,
            payload: json!({
                "server_profile_id": profile_id,
                "name": "audit-feed",
                "host_id": uuid::Uuid::new_v4(),
                "ssh_identity_id": uuid::Uuid::new_v4(),
                "disabled_at": null,
            }),
            remote_addr: None,
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(get("/api/v1/audit-events/recent", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().expect("expected JSON array");
    assert!(!arr.is_empty(), "current user should see their own row");

    let first = &arr[0];
    assert_eq!(first["kind"], "server_profile_created");
    assert!(first["id"].is_string());
    assert!(first["recorded_at"].is_string());
    let summary = &first["summary"];
    assert_eq!(summary["kind"], "server_profile_lifecycle");
    assert_eq!(
        summary["server_profile_id"].as_str().unwrap(),
        profile_id.to_string(),
    );
    assert_eq!(summary["name"], "audit-feed");

    // The DTO must drop actor_id and remote_addr.
    assert!(first.get("actor_id").is_none(), "DTO must omit actor_id");
    assert!(
        first.get("remote_addr").is_none(),
        "DTO must omit remote_addr",
    );
    assert!(
        first.get("payload").is_none(),
        "DTO must not echo raw payload",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_excludes_other_users_events(pool: PgPool) {
    // Set up a router whose authenticated caller is `caller`. Insert an
    // audit row for `other` directly. The feed for `caller` must NOT
    // see it.
    let caller = create_user(&pool, "caller").await;
    let other = create_user(&pool, "other").await;

    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let cookie = bootstrap_test_session(&__auth, caller).await;
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let audit = PgAuditEventRepository::new(pool.clone());
    audit
        .create(CreateAuditEvent {
            actor_id: Some(other),
            kind: AuditEventKind::ServerProfileCreated,
            payload: json!({
                "server_profile_id": uuid::Uuid::new_v4(),
                "name": "other-prof",
            }),
            remote_addr: None,
        })
        .await
        .unwrap();
    // Pre-auth row (NULL actor) — must also be invisible.
    audit
        .create(CreateAuditEvent {
            actor_id: None,
            kind: AuditEventKind::LoginFailed,
            payload: json!({ "reason": "bad_password" }),
            remote_addr: Some("203.0.113.7".to_owned()),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(get("/api/v1/audit-events/recent", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert!(
        arr.is_empty(),
        "current-user feed must hide foreign-actor and NULL-actor rows: {body}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_clamps_limit(pool: PgPool) {
    // Insert 12 lifecycle events for the current user, then ask for
    // limit=5: the response MUST contain at most 5. A limit much larger
    // than `MAX_LIMIT` (10000) clamps silently to 100.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let audit = PgAuditEventRepository::new(pool.clone());
    for i in 0..12 {
        audit
            .create(CreateAuditEvent {
                actor_id: Some(user_id),
                kind: AuditEventKind::ServerProfileCreated,
                payload: json!({
                    "server_profile_id": uuid::Uuid::new_v4(),
                    "name": format!("p-{i}"),
                }),
                remote_addr: None,
            })
            .await
            .unwrap();
    }

    let resp = app
        .clone()
        .oneshot(get("/api/v1/audit-events/recent?limit=5", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 5);

    // Out-of-range limit is silently clamped to MAX_LIMIT.
    let resp = app
        .oneshot(get("/api/v1/audit-events/recent?limit=10000", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert!(arr.len() <= 100, "MAX_LIMIT must cap the response");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_empty_list_for_quiet_user(pool: PgPool) {
    let (app, _user_id, cookie) = setup(pool.clone()).await;
    let resp = app
        .oneshot(get("/api/v1/audit-events/recent", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().expect("expected an array");
    assert!(arr.is_empty(), "fresh user should see an empty feed");
}

/// Sentinel-style redaction guarantee for the audit-events feed. A
/// payload row crafted with every name in `AUDIT_FORBIDDEN_SUBSTRINGS`
/// MUST NOT see any of them survive into the wire response. The
/// sanitizer is the redaction backstop; this test is the "if the
/// sanitizer drifts, the route still strips it" assertion.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_redacts_secret_shaped_payload_fields(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let audit = PgAuditEventRepository::new(pool.clone());
    audit
        .create(CreateAuditEvent {
            actor_id: Some(user_id),
            kind: AuditEventKind::ServerProfileCreated,
            payload: json!({
                "server_profile_id": uuid::Uuid::new_v4(),
                "name": "redact-me",
                "host_id": uuid::Uuid::new_v4(),
                "ssh_identity_id": uuid::Uuid::new_v4(),
                "disabled_at": null,
                // Forbidden names smuggled into the payload — must not
                // appear in the response.
                "encrypted_private_key": "BEGIN OPENSSH PRIVATE KEY...",
                "private_key": "PEM bytes",
                "client_info": "Mozilla/5.0",
                "remote_addr": "203.0.113.7",
                "user_agent": "tauri/2",
            }),
            // remote_addr field is also a sentinel — make sure DTO doesn't
            // surface the column either.
            remote_addr: Some("203.0.113.7".to_owned()),
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(get("/api/v1/audit-events/recent", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let raw =
        String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    for forbidden in AUDIT_FORBIDDEN_SUBSTRINGS {
        assert!(
            !raw.contains(forbidden),
            "audit feed response must not contain `{forbidden}`: {raw}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_unknown_kind_collapses_to_generic_summary(pool: PgPool) {
    // A row whose kind doesn't have an explicit sanitizer (e.g.
    // `Other`) must surface as `summary.kind = "generic"` and MUST NOT
    // echo any of the row's payload fields.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let audit = PgAuditEventRepository::new(pool.clone());
    audit
        .create(CreateAuditEvent {
            actor_id: Some(user_id),
            kind: AuditEventKind::Other,
            payload: json!({
                "raw_error": "russh internal: handshake failed",
                "private_key": "leak-me",
            }),
            remote_addr: None,
        })
        .await
        .unwrap();

    let resp = app
        .oneshot(get("/api/v1/audit-events/recent", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let entry = &body.as_array().unwrap()[0];
    assert_eq!(entry["kind"], "other");
    assert_eq!(entry["summary"]["kind"], "generic");
    let raw = entry.to_string();
    assert!(!raw.contains("raw_error"));
    assert!(!raw.contains("private_key"));
    assert!(!raw.contains("russh"));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_unauthorized_without_session_cookie(pool: PgPool) {
    // The audit-events route is auth-guarded. With no session cookie
    // (and the dev-auth shim disabled) the AuthenticatedUser extractor
    // short-circuits with 401 before any DB read.
    let db = Db::from_pool(pool.clone());
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .oneshot(get_no_auth("/api/v1/audit-events/recent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ----------------------------------------------------------------------
// Auth routes (/api/v1/auth/*)
// ----------------------------------------------------------------------

/// Sentinel password used by the auth-route tests. Long enough to clear
/// the boundary minimum (12 chars) and unique-looking so a redaction
/// test can prove it never reaches a persisted audit payload, log line,
/// or response body.
const TEST_AUTH_PASSWORD: &str = "TEST-AUTH-PASSWORD-DO-NOT-LEAK-1234";

fn auth_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, TEST_AUTH_ORIGIN)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn auth_post_with_origin(uri: &str, body: Value, origin: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, origin)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn auth_post_no_origin(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn auth_get_with_cookie(uri: &str, cookie_value: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, format!("relayterm_session={cookie_value}"))
        .body(Body::empty())
        .unwrap()
}

fn auth_post_with_cookie(uri: &str, cookie_value: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, TEST_AUTH_ORIGIN)
        .header(header::COOKIE, format!("relayterm_session={cookie_value}"))
        .body(Body::from("{}"))
        .unwrap()
}

fn extract_set_cookie(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Pull the cookie token value from a `Set-Cookie` header. Returns the
/// segment between `relayterm_session=` and the first `;`.
fn cookie_token_from_set_cookie(set_cookie: &str) -> &str {
    let rest = set_cookie
        .strip_prefix("relayterm_session=")
        .expect("Set-Cookie starts with the session cookie name");
    match rest.find(';') {
        Some(i) => &rest[..i],
        None => rest,
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bootstrap_creates_first_user_and_does_not_set_cookie(pool: PgPool) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
                "email": "first@relayterm.local",
                "display_name": "First Operator",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(
        resp.headers().get(header::SET_COOKIE).is_none(),
        "bootstrap MUST NOT mint a session cookie",
    );
    let body = read_body(resp).await;
    assert_eq!(body["email"], "first@relayterm.local");
    assert_eq!(body["display_name"], "First Operator");
    assert!(body["id"].is_string());
    // No secret-shaped fields on the response.
    for forbidden in [
        "password",
        "password_hash",
        "session_token",
        "token_hash",
        "bootstrap_token",
        "argon2id",
    ] {
        let raw = body.to_string();
        assert!(
            !raw.contains(forbidden),
            "bootstrap response must not contain `{forbidden}`: {raw}",
        );
    }

    // First-user-created audit event written, with safe payload.
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let row = audit
        .iter()
        .find(|e| e.kind == AuditEventKind::FirstUserCreated)
        .expect("first_user_created audit row");
    assert!(row.actor_id.is_some());
    let raw_payload = row.payload.to_string();
    assert!(!raw_payload.contains(TEST_BOOTSTRAP_TOKEN));
    assert!(!raw_payload.contains(TEST_AUTH_PASSWORD));
    for forbidden in [
        "password",
        "password_hash",
        "session_token",
        "token_hash",
        "bootstrap_token",
        "argon2id",
    ] {
        assert!(
            !raw_payload.contains(forbidden),
            "first_user_created payload must not contain `{forbidden}`: {raw_payload}",
        );
    }
    // Email/display_name MUST NOT be in the payload (PII / SET NULL on
    // delete contract — see SPEC.md "Audit events" table).
    assert!(!raw_payload.contains("first@relayterm.local"));
    assert!(!raw_payload.contains("First Operator"));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bootstrap_rejects_wrong_token_without_echo(pool: PgPool) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let attempted_token = "WRONG-BOOTSTRAP-TOKEN-DO-NOT-LEAK";
    let resp = app
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": attempted_token,
                "email": "first@relayterm.local",
                "display_name": "First Operator",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    let raw = body.to_string();
    assert!(!raw.contains(attempted_token));
    assert!(!raw.contains(TEST_BOOTSTRAP_TOKEN));
    assert!(!raw.contains(TEST_AUTH_PASSWORD));

    // login_failed audit row exists with NULL actor and reason "bad_token".
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let row = audit
        .iter()
        .find(|e| e.kind == AuditEventKind::LoginFailed)
        .expect("login_failed row");
    assert!(row.actor_id.is_none());
    let raw_payload = row.payload.to_string();
    assert!(raw_payload.contains("\"reason\":\"bad_token\""));
    assert!(!raw_payload.contains(attempted_token));
    assert!(!raw_payload.contains(TEST_BOOTSTRAP_TOKEN));
    assert!(!raw_payload.contains(TEST_AUTH_PASSWORD));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bootstrap_rejects_when_already_bootstrapped(pool: PgPool) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let body = json!({
        "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
        "email": "first@relayterm.local",
        "display_name": "First Operator",
        "password": TEST_AUTH_PASSWORD,
    });
    let first = app
        .clone()
        .oneshot(auth_post("/api/v1/auth/bootstrap", body.clone()))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    // Second bootstrap with the same token: blocked.
    let second = app
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
                "email": "second@relayterm.local",
                "display_name": "Second",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let body = read_body(second).await;
    assert_eq!(body["error"]["code"], "conflict");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already_bootstrapped"),
    );

    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let row = audit
        .iter()
        .find(|e| {
            e.kind == AuditEventKind::LoginFailed
                && e.payload.get("reason").and_then(|v| v.as_str()) == Some("already_bootstrapped")
        })
        .expect("login_failed already_bootstrapped row");
    assert!(row.actor_id.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bootstrap_returns_503_when_no_token_configured(pool: PgPool) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = Arc::new(AuthRoutesConfig {
        cookie_secure: false,
        cookie_domain: None,
        allowed_origins: vec![TEST_AUTH_ORIGIN.to_owned()],
        bootstrap_token: None,
    });
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": "anything",
                "email": "first@relayterm.local",
                "display_name": "First",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Helper: set up an app + bootstrap a single user with the test
/// password, returning (app, user_id).
async fn setup_with_first_user(pool: PgPool, email: &str) -> (Router, UserId) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
                "email": email,
                "display_name": "Operator",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    let user_id: UserId = serde_json::from_value(body["id"].clone()).unwrap();
    (app, user_id)
}

/// Production-shaped router: cookie_secure = true, allow-list set, no
/// dev escape hatches. Exercises the same code path the production
/// boot follows after `Config::validate_auth` accepts an
/// `auth.mode = production` config. The `Set-Cookie` Secure assertion
/// in [`production_login_sets_secure_cookie_and_authenticates_protected_route`]
/// is the wire-level proof.
async fn setup_production_first_user(pool: PgPool, email: &str) -> (Router, UserId) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let auth_routes = Arc::new(AuthRoutesConfig {
        cookie_secure: true,
        cookie_domain: None,
        allowed_origins: vec![TEST_AUTH_ORIGIN.to_owned()],
        bootstrap_token: Some(zeroize::Zeroizing::new(TEST_BOOTSTRAP_TOKEN.to_owned())),
    });
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes,
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
                "email": email,
                "display_name": "Operator",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    let user_id: UserId = serde_json::from_value(body["id"].clone()).unwrap();
    (app, user_id)
}

/// Production-shaped router (Secure cookies, allow-listed Origin)
/// must mint a session cookie carrying `Secure` AND that cookie must
/// authenticate every protected `/api/v1/*` route the same way a dev-
/// shaped router does. This is the wire-level proof that flipping
/// `auth.mode = production` in `Config::validate_auth` does not change
/// the route-handler contract — every previously-`DevUser`-protected
/// surface keeps working under real cookie-backed auth.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn production_login_sets_secure_cookie_and_authenticates_protected_route(pool: PgPool) {
    let (app, _user_id) = setup_production_first_user(pool.clone(), "prod@relayterm.local").await;

    // Login.
    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "prod@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let set_cookie = extract_set_cookie(&login).expect("Set-Cookie present");
    assert!(
        set_cookie.contains("Secure"),
        "production cookie must carry Secure flag: {set_cookie}"
    );
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let token = cookie_token_from_set_cookie(&set_cookie).to_owned();
    let _ = login.into_body();

    // Protected GET — proves the cookie authenticates a former
    // `DevUser`-only route under the production-shaped state.
    let resp = app
        .clone()
        .oneshot(get("/api/v1/hosts", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Negative half: the same production-shaped router must still
    // reject an unauthenticated request with the canonical 401 — proves
    // production mode does not silently accept routes without a cookie.
    let resp = app.oneshot(get_no_auth("/api/v1/hosts")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(body["error"]["message"], "unauthorized");
}

/// Property-1 runtime gate: `auth.mode = production` with no first user
/// AND no `first_user_bootstrap_token` configured MUST fail before the
/// listener binds. Replicates the exact predicate `apps/backend/src/main.rs`
/// runs after the DB connect and asserts the gate fires for that
/// combination but NOT when the operator has supplied a token OR a user
/// already exists.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn production_no_first_user_no_token_runtime_gate(pool: PgPool) {
    // Mirror of the predicate in `apps/backend/src/main.rs`. Kept as a
    // closure so this test is the canonical place to update if the gate
    // condition ever changes — a drift between the two becomes
    // immediately visible in the diff.
    fn gate_blocks_boot(token_set: bool, any_users_exist: bool) -> bool {
        !token_set && !any_users_exist
    }

    let creds = relayterm_db::PgPasswordCredentialRepository::new(pool.clone());

    // Fresh DB — `users` and `user_passwords` are both empty. This is
    // the exact "operator just deployed, no user exists yet" state the
    // gate guards against.
    let any_users_exist = creds.any_exists().await.expect("count password rows");
    assert!(
        !any_users_exist,
        "fixture must start with no users for this test to be meaningful"
    );

    // Operator state A: no token configured + no users → gate MUST
    // fire. Without a token, there is no path to create a first user,
    // so the backend MUST refuse to start.
    assert!(
        gate_blocks_boot(false, any_users_exist),
        "production + no users + no token must trigger the bail! gate \
         in apps/backend/src/main.rs",
    );

    // Operator state B: token IS configured. The gate must NOT fire —
    // the operator can hit `/api/v1/auth/bootstrap` to create the first
    // user.
    assert!(
        !gate_blocks_boot(true, any_users_exist),
        "production + token-set must proceed past the bail! gate even \
         when no first user exists yet",
    );

    // Operator state C: a first user already exists. Token presence
    // becomes irrelevant — the gate's negative branch is `any_exists()`
    // returning true. Bootstrap a real user via the auth service to
    // simulate "deploy already in steady state."
    let auth = test_auth(&Db::from_pool(pool.clone()));
    let user_id = create_user(&pool, "steady").await;
    auth.set_password(user_id, "PASSWORD-STEADY-OK-1234")
        .await
        .expect("set password");
    let any_users_exist = creds.any_exists().await.expect("count password rows");
    assert!(
        any_users_exist,
        "after set_password, any_exists must be true"
    );
    assert!(
        !gate_blocks_boot(false, any_users_exist),
        "production + first user already exists must proceed regardless \
         of whether the bootstrap token is still configured",
    );
    assert!(
        !gate_blocks_boot(true, any_users_exist),
        "production + first user + token-set is a no-op for the gate",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_succeeds_and_sets_strict_httponly_cookie(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "login@relayterm.local").await;

    let resp = app
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "login@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = extract_set_cookie(&resp).expect("Set-Cookie header set");
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("Max-Age=2592000"));
    assert!(cookie.starts_with("relayterm_session="));
    let body = read_body(resp).await;
    let raw = body.to_string();
    // Response carries safe DTO only.
    assert_eq!(body["email"], "login@relayterm.local");
    assert_eq!(body["id"].as_str().unwrap(), user_id.to_string());
    for forbidden in [
        "password",
        "password_hash",
        "session_token",
        "token_hash",
        "argon2id",
    ] {
        assert!(
            !raw.contains(forbidden),
            "login response must not contain `{forbidden}`: {raw}",
        );
    }
    let token = cookie_token_from_set_cookie(&cookie);
    assert!(
        !raw.contains(token),
        "login body must not echo the cookie token"
    );

    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let row = audit
        .iter()
        .find(|e| e.kind == AuditEventKind::LoginSucceeded)
        .expect("login_succeeded audit row");
    assert_eq!(row.actor_id, Some(user_id));
    let raw_payload = row.payload.to_string();
    assert!(raw_payload.contains("\"method\":\"password\""));
    assert!(!raw_payload.contains(TEST_AUTH_PASSWORD));
    assert!(!raw_payload.contains(token));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_with_wrong_password_returns_401_and_logs_login_failed(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "wrong@relayterm.local").await;

    let resp = app
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "wrong@relayterm.local",
                "password": "WRONG-PASSWORD-DO-NOT-LEAK-12345",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(body["error"]["message"], "unauthorized");

    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let row = audit
        .iter()
        .find(|e| e.kind == AuditEventKind::LoginFailed)
        .expect("login_failed audit row");
    assert!(row.actor_id.is_none());
    let raw_payload = row.payload.to_string();
    assert!(raw_payload.contains("\"reason\":\"bad_credentials\""));
    assert!(!raw_payload.contains("WRONG-PASSWORD-DO-NOT-LEAK"));
    assert!(!raw_payload.contains(TEST_AUTH_PASSWORD));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_unknown_email_is_indistinguishable_from_wrong_password(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "known@relayterm.local").await;

    let known_resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "known@relayterm.local",
                "password": "this-is-not-the-password-1234",
            }),
        ))
        .await
        .unwrap();
    let unknown_resp = app
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "stranger@relayterm.local",
                "password": "this-is-not-the-password-1234",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(known_resp.status(), unknown_resp.status());
    assert_eq!(known_resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_body(known_resp).await, read_body(unknown_resp).await);

    // Audit-row equivalence: BOTH branches must write a `login_failed`
    // row with `actor_id IS NULL` and `payload.reason = "bad_credentials"`,
    // and neither row may carry the offered email or password. Without
    // this assertion, a future change that splits the audit emission
    // by branch would silently re-introduce the probe channel through
    // the persisted audit feed.
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let failed: Vec<_> = audit
        .iter()
        .filter(|e| e.kind == AuditEventKind::LoginFailed)
        .collect();
    assert_eq!(
        failed.len(),
        2,
        "expected one login_failed audit row per request; got {}: {failed:?}",
        failed.len(),
    );
    for row in &failed {
        assert!(row.actor_id.is_none());
        let raw = row.payload.to_string();
        assert!(raw.contains("\"reason\":\"bad_credentials\""));
        assert!(!raw.contains("known@relayterm.local"));
        assert!(!raw.contains("stranger@relayterm.local"));
        assert!(!raw.contains("this-is-not-the-password-1234"));
        assert_audit_payload_redacted(&row.payload, AuditEventKind::LoginFailed);
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_returns_user_for_valid_cookie(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "me@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "me@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["id"].as_str().unwrap(), user_id.to_string());
    assert_eq!(body["email"], "me@relayterm.local");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_rejects_missing_cookie(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "missing@relayterm.local").await;
    let resp = app.oneshot(get_no_auth("/api/v1/auth/me")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_rejects_unknown_cookie(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "unknown@relayterm.local").await;
    let resp = app
        .oneshot(auth_get_with_cookie(
            "/api/v1/auth/me",
            "absolutely-not-a-real-token",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn logout_revokes_session_and_clears_cookie(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "logout@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "logout@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();

    let logout = app
        .clone()
        .oneshot(auth_post_with_cookie("/api/v1/auth/logout", &token))
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let clear = extract_set_cookie(&logout).unwrap();
    assert!(clear.contains("Max-Age=0"));
    assert!(clear.contains("HttpOnly"));
    assert!(clear.contains("SameSite=Strict"));

    // The same cookie is now revoked — /me MUST 401.
    let resp = app
        .clone()
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // logout_succeeded audit row written with safe payload.
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let row = audit
        .iter()
        .find(|e| e.kind == AuditEventKind::LogoutSucceeded)
        .expect("logout_succeeded audit row");
    assert_eq!(row.actor_id, Some(user_id));
    let raw_payload = row.payload.to_string();
    assert!(!raw_payload.contains(&token));
    assert!(!raw_payload.contains(TEST_AUTH_PASSWORD));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn logout_is_idempotent_for_missing_or_unknown_cookie(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "idempotent@relayterm.local").await;

    // No cookie at all: still 204 with a clear cookie.
    let bare = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ORIGIN, TEST_AUTH_ORIGIN)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::NO_CONTENT);
    assert!(extract_set_cookie(&bare).unwrap().contains("Max-Age=0"));

    // Cookie that never corresponded to any session row: still 204.
    let bogus = app
        .clone()
        .oneshot(auth_post_with_cookie(
            "/api/v1/auth/logout",
            "definitely-not-a-real-token-string",
        ))
        .await
        .unwrap();
    assert_eq!(bogus.status(), StatusCode::NO_CONTENT);

    // No logout_succeeded audit rows should have been written for these
    // probe paths.
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let count = audit
        .iter()
        .filter(|e| e.kind == AuditEventKind::LogoutSucceeded)
        .count();
    assert_eq!(count, 0, "no logout_succeeded rows for no-op logout calls");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_post_routes_reject_missing_origin(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "csrf@relayterm.local").await;
    let resp = app
        .oneshot(auth_post_no_origin(
            "/api/v1/auth/login",
            json!({
                "email": "csrf@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_post_routes_reject_disallowed_origin(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "csrf2@relayterm.local").await;
    let resp = app
        .oneshot(auth_post_with_origin(
            "/api/v1/auth/login",
            json!({
                "email": "csrf2@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
            "https://evil.example.com",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bad_origin_rejects_before_body_parsing(pool: PgPool) {
    // The `CsrfGuard` extractor runs AHEAD of `Json<...>` in the
    // bootstrap / login / logout signatures. A malformed body paired
    // with a disallowed Origin must therefore return 403 (the guard's
    // verdict), NOT 400 (the body parser's verdict). If this test
    // ever sees a 400 it means the guard moved behind body parsing —
    // which would let an unauthenticated probe parse-bomb the JSON
    // path before the Origin check fires.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "csrforder@relayterm.local").await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://evil.example.com")
        .body(Body::from("{not-json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bad_origin_login_does_not_write_login_failed_audit(pool: PgPool) {
    // A disallowed-Origin login must NOT touch the password verifier
    // OR write a `login_failed` audit row — the request is rejected
    // before any auth work runs. SPEC.md "CSRF posture" pins the
    // pre-auth-check ordering and the redaction tests rely on it
    // (a `login_failed` row from a CSRF-rejected probe would expose
    // login-throttle telemetry to whichever Origin showed up).
    let (app, _user_id) = setup_with_first_user(pool.clone(), "noaudit@relayterm.local").await;

    // Snapshot pre-existing `login_failed` rows so the assertion is
    // robust against the bootstrap-helper's own audit emissions.
    let before = PgAuditEventRepository::new(pool.clone())
        .recent(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == AuditEventKind::LoginFailed)
        .count();

    let resp = app
        .oneshot(auth_post_with_origin(
            "/api/v1/auth/login",
            json!({
                "email": "noaudit@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
            "https://evil.example.com",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let after = PgAuditEventRepository::new(pool.clone())
        .recent(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == AuditEventKind::LoginFailed)
        .count();
    assert_eq!(
        after, before,
        "CSRF-rejected login MUST NOT emit a login_failed audit row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bad_origin_bootstrap_does_not_create_user_or_audit(pool: PgPool) {
    // Bootstrap is the first-user-creation surface. A disallowed-Origin
    // bootstrap must NOT create a user row, MUST NOT emit any audit
    // row (no `first_user_created`, no `login_failed` with a probe
    // reason), and MUST collapse to the same 403 / `csrf_origin_mismatch`
    // wire shape the other auth routes use.
    //
    // Builds `AppState` by hand instead of using `setup_with_first_user`
    // because the assertion needs the pool to start with zero users and
    // zero auth-shaped audit rows — the helper bootstraps a user, which
    // would pollute the audit-row count this test verifies.
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);

    let resp = app
        .oneshot(auth_post_with_origin(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
                "email": "blocked@relayterm.local",
                "display_name": "Blocked",
                "password": TEST_AUTH_PASSWORD,
            }),
            "https://evil.example.com",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");

    // No matching user.
    let user = PgUserRepository::new(pool.clone())
        .get_by_email("blocked@relayterm.local")
        .await
        .unwrap();
    assert!(
        user.is_none(),
        "CSRF-rejected bootstrap MUST NOT create a user",
    );

    // Zero auth-related audit rows.
    let audit = PgAuditEventRepository::new(pool.clone())
        .recent(200)
        .await
        .unwrap();
    let auth_rows = audit
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                AuditEventKind::FirstUserCreated
                    | AuditEventKind::LoginSucceeded
                    | AuditEventKind::LoginFailed
                    | AuditEventKind::LogoutSucceeded
            )
        })
        .count();
    assert_eq!(
        auth_rows, 0,
        "CSRF-rejected bootstrap MUST NOT emit any auth audit row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_does_not_require_origin(pool: PgPool) {
    // GET /auth/me is exempt from the shared CSRF guard. Without a
    // cookie it returns 401, not 403 — the auth check runs even with
    // no Origin header present.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "noorigin@relayterm.local").await;
    let resp = app.oneshot(get_no_auth("/api/v1/auth/me")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_validation_rejects_short_password(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "short@relayterm.local").await;
    let resp = app
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "short@relayterm.local",
                "password": "short",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
}

// ----------------------------------------------------------------------
// AuthenticatedUser extractor (cookie-backed) — extractor-specific
// edge cases (expired / revoked / prefix-confusion) live below; the
// happy-path coverage rides on every `setup*` fixture's bootstrapped
// session cookie.
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_rejects_expired_session_via_extractor(pool: PgPool) {
    // Insert a session row with `expires_at` already in the past,
    // then drive `/auth/me` with the matching plaintext cookie. The
    // extractor's `validate_session_token` MUST surface
    // `SessionExpired` and the route MUST collapse to 401 on the
    // wire.
    let (app, user_id) = setup_with_first_user(pool.clone(), "expired@relayterm.local").await;
    let token = relayterm_auth::SessionToken::generate();
    let token_hash = token.hash().into_bytes();
    let already_expired = chrono::Utc::now() - chrono::Duration::seconds(60);
    relayterm_db::PgUserSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateUserSession {
            user_id,
            token_hash,
            expires_at: already_expired,
        })
        .await
        .expect("insert pre-expired session");

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", token.expose()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_rejects_revoked_session_via_extractor(pool: PgPool) {
    // Mint a real session via /login, revoke its row, then drive
    // /me. The extractor MUST see `SessionRevoked` and return 401.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "revoked@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "revoked@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    // Revoke the underlying session row directly.
    let token_hash = relayterm_auth::hash_session_token(&token).into_bytes();
    let sessions = relayterm_db::PgUserSessionRepository::new(pool.clone());
    let row = sessions
        .get_by_token_hash(&token_hash)
        .await
        .expect("lookup session row")
        .expect("session row exists");
    sessions
        .revoke(row.id, chrono::Utc::now(), Some("test-revoke"))
        .await
        .expect("revoke");

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_rejects_prefix_named_cookie_via_extractor(pool: PgPool) {
    // A cookie named `relayterm_session_other` shares a prefix with
    // the real session cookie. The exact-match parser in the
    // extractor MUST NOT pick it up — the route MUST surface 401 as
    // if no cookie were present at all.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "prefix@relayterm.local").await;

    // First mint a real session, then attempt to reuse its token
    // value under the wrong cookie name.
    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "prefix@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header(header::COOKIE, format!("relayterm_session_other={token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_rejects_suffix_named_cookie_via_extractor(pool: PgPool) {
    // Sibling of the prefix test: a `fake_relayterm_session=<real-token>`
    // cookie shares a suffix with the real session cookie. The
    // exact-match parser MUST NOT pick it up — the route MUST 401.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "suffix@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "suffix@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header(header::COOKIE, format!("fake_relayterm_session={token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_response_does_not_leak_session_or_password_material(pool: PgPool) {
    // The /me JSON body MUST NOT contain the cookie token, the
    // password, the bootstrap token, or any Argon2id PHC marker.
    // Sentinel-shaped strings make the assertion exhaustive against
    // a future field-rename that re-introduces the leak.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "leak@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "leak@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let raw = body.to_string();
    for sentinel in [
        token.as_str(),
        TEST_AUTH_PASSWORD,
        TEST_BOOTSTRAP_TOKEN,
        "$argon2id$",
    ] {
        assert!(
            !raw.contains(sentinel),
            "/me response must not contain `{sentinel}`: {raw}",
        );
    }
    // No secret-shaped names should appear either, even if a future
    // rename collides.
    for forbidden in [
        "password",
        "password_hash",
        "session_token",
        "token_hash",
        "bootstrap_token",
    ] {
        assert!(
            !raw.contains(forbidden),
            "/me response must not contain field `{forbidden}`: {raw}",
        );
    }
}

// ----------------------------------------------------------------------
// last_seen_at touch (SPEC.md "Auth extractor and route migration")
// ----------------------------------------------------------------------
//
// Every successful `AuthenticatedUser` extraction stamps
// `user_sessions.last_seen_at`. Failed / expired / revoked / unknown
// extractions MUST NOT touch the column. The touch is best-effort —
// a repository failure logs `warn!` and the request still succeeds.
//
// The tests below pin the four positive / negative guarantees. The
// "best-effort under repository failure" half of the contract is
// exercised at the unit level via the extractor docs and in code
// review — a `Db`-level fault injection would require swapping the
// repository accessor to a trait object on `AppState`, which is
// scoped beyond this slice; the wire-side guarantees the tests cover
// (no behavior change on success, no `last_seen_at` write on failure)
// are the load-bearing observable contract.

/// Look up the persisted `last_seen_at` for the given plaintext token.
/// Helper kept local to the `last_seen_at` tests so the wire-side
/// assertions stay terse. The token plaintext crosses the helper
/// boundary read-only and is hashed via the same one-way digest the
/// repository uses for lookups — never persisted, never logged.
async fn read_last_seen_at(pool: &PgPool, token: &str) -> chrono::DateTime<chrono::Utc> {
    let token_hash = relayterm_auth::hash_session_token(token).into_bytes();
    let row = relayterm_db::PgUserSessionRepository::new(pool.clone())
        .get_by_token_hash(&token_hash)
        .await
        .expect("lookup session row")
        .expect("session row exists");
    row.last_seen_at
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn me_advances_last_seen_at_on_valid_cookie(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "lastseen-me@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "lastseen-me@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    let before = read_last_seen_at(&pool, &token).await;

    // Sleep past microsecond resolution so the post-touch timestamp
    // is strictly greater on every platform's `Utc::now()` clock.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after = read_last_seen_at(&pool, &token).await;
    assert!(
        after > before,
        "last_seen_at must advance on successful /auth/me extraction \
         (before={before}, after={after})",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn protected_get_advances_last_seen_at_on_valid_cookie(pool: PgPool) {
    // Same property as above but on a non-auth route (`/api/v1/hosts`)
    // so the touch is proven to ride on the shared extractor and not
    // a per-route accident.
    let (app, _user_id, cookie) = setup(pool.clone()).await;

    let before = read_last_seen_at(&pool, &cookie).await;

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let resp = app.oneshot(get("/api/v1/hosts", &cookie)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after = read_last_seen_at(&pool, &cookie).await;
    assert!(
        after > before,
        "last_seen_at must advance on successful protected GET \
         (before={before}, after={after})",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn expired_session_does_not_touch_last_seen_at(pool: PgPool) {
    // Insert a session row with `expires_at` already in the past and
    // capture the initial `last_seen_at`. Drive `/auth/me` — the
    // extractor MUST surface 401 AND MUST NOT have stamped
    // `last_seen_at`.
    let (app, user_id) =
        setup_with_first_user(pool.clone(), "lastseen-expired@relayterm.local").await;
    let token = relayterm_auth::SessionToken::generate();
    let token_hash = token.hash().into_bytes();
    let already_expired = chrono::Utc::now() - chrono::Duration::seconds(60);
    let row = relayterm_db::PgUserSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateUserSession {
            user_id,
            token_hash,
            expires_at: already_expired,
        })
        .await
        .expect("insert pre-expired session");

    let before = row.last_seen_at;

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", token.expose()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let after = read_last_seen_at(&pool, token.expose()).await;
    assert_eq!(
        after, before,
        "last_seen_at must NOT advance for an expired session \
         (before={before}, after={after})",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoked_session_does_not_touch_last_seen_at(pool: PgPool) {
    // Mint a real session via /login, revoke its row directly, and
    // then drive /auth/me. The extractor MUST 401 AND MUST NOT have
    // stamped `last_seen_at` post-revoke.
    let (app, _user_id) =
        setup_with_first_user(pool.clone(), "lastseen-revoked@relayterm.local").await;

    let login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "lastseen-revoked@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let cookie = extract_set_cookie(&login).unwrap();
    let token = cookie_token_from_set_cookie(&cookie).to_owned();
    let _ = login.into_body();

    // Revoke the session row directly so the cookie is structurally
    // valid but no longer authoritative.
    let token_hash = relayterm_auth::hash_session_token(&token).into_bytes();
    let sessions = relayterm_db::PgUserSessionRepository::new(pool.clone());
    let row = sessions
        .get_by_token_hash(&token_hash)
        .await
        .expect("lookup session row")
        .expect("session row exists");
    sessions
        .revoke(row.id, chrono::Utc::now(), Some("test-revoke"))
        .await
        .expect("revoke");

    let before = read_last_seen_at(&pool, &token).await;

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let resp = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let after = read_last_seen_at(&pool, &token).await;
    assert_eq!(
        after, before,
        "last_seen_at must NOT advance for a revoked session \
         (before={before}, after={after})",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn unknown_token_does_not_create_or_touch_any_session(pool: PgPool) {
    // A cookie value that never corresponded to any session row MUST
    // NOT result in a session insert (no row appears) AND MUST NOT
    // disturb existing rows. The extractor returns 401 and walks away.
    let (app, _user_id) =
        setup_with_first_user(pool.clone(), "lastseen-unknown@relayterm.local").await;

    // Bootstrap a real session for the same user so we have an
    // existing `last_seen_at` row to confirm is left untouched.
    let real_login = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "lastseen-unknown@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    let real_cookie = extract_set_cookie(&real_login).unwrap();
    let real_token = cookie_token_from_set_cookie(&real_cookie).to_owned();
    let _ = real_login.into_body();

    let real_before = read_last_seen_at(&pool, &real_token).await;

    // No `tokio::time::sleep` here (unlike the four "advances" /
    // "does-not-advance-after-touch" tests) — this test asserts
    // strict equality between `real_before` and `real_after`, not
    // ordering, so microsecond-level clock resolution does not
    // matter. A touch of the legitimate session row would be
    // observable as ANY change to `last_seen_at`, however small.
    // Drive /auth/me with a token that hashes to a digest no row owns.
    let stranger = relayterm_auth::SessionToken::generate();
    let resp = app
        .clone()
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", stranger.expose()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // No row exists for the stranger token's hash.
    let stranger_hash = stranger.hash().into_bytes();
    let strangers_row = relayterm_db::PgUserSessionRepository::new(pool.clone())
        .get_by_token_hash(&stranger_hash)
        .await
        .expect("lookup stranger");
    assert!(
        strangers_row.is_none(),
        "an unknown-token request must not create a session row",
    );

    // The legitimate session's last_seen_at must still match the
    // pre-stranger sample — proves the failed extraction did not
    // touch any unrelated row.
    let real_after = read_last_seen_at(&pool, &real_token).await;
    assert_eq!(
        real_after, real_before,
        "an unknown-token request must not touch any other session's last_seen_at",
    );
}

// ----------------------------------------------------------------------
// Login throttling (SPEC.md "Password authentication (v1)" → Throttling)
// ----------------------------------------------------------------------

/// Build an app with a router that uses a tight in-memory login
/// throttler so the test rig can drive the bucket in three calls
/// instead of five. Returns the same `(app, user_id)` shape as
/// `setup_with_first_user` for symmetry with the existing tests.
///
/// The throttler is keyed exclusively on the normalized email — there
/// is no IP-aware key in v1, so a per-test override is sufficient to
/// exercise every property in SPEC.md "Security properties to test"
/// → property 11.
async fn setup_with_first_user_and_tight_throttler(
    pool: PgPool,
    email: &str,
    throttler: Arc<LoginThrottler>,
) -> (Router, UserId) {
    let db = Db::from_pool(pool.clone());
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: throttler,
    };
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/bootstrap",
            json!({
                "bootstrap_token": TEST_BOOTSTRAP_TOKEN,
                "email": email,
                "display_name": "Operator",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    let user_id: UserId = serde_json::from_value(body["id"].clone()).unwrap();
    (app, user_id)
}

/// Throttler tuned so the route trips after 3 failures, with a long
/// block window so the test does not need to wait on wall-clock time.
fn tight_throttler() -> Arc<LoginThrottler> {
    Arc::new(LoginThrottler::new(LoginThrottleConfig {
        max_failures: 3,
        window: chrono::Duration::minutes(15),
        block: chrono::Duration::minutes(15),
    }))
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_throttle_blocks_after_threshold_with_safe_response(pool: PgPool) {
    // Three wrong-password attempts trip the threshold; the fourth
    // returns a safe 429 with the static `too_many_requests` body.
    // The throttle key is the normalized email; the wire body must
    // never echo it, must never echo the offered password, and must
    // never carry timing/throttle telemetry.
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "throttle@relayterm.local",
        throttler.clone(),
    )
    .await;

    let bad_password = "WRONG-THROTTLE-PASSWORD-DO-NOT-LEAK-9999";
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": "throttle@relayterm.local",
                    "password": bad_password,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // Fourth attempt is throttled.
    let throttled = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "throttle@relayterm.local",
                "password": bad_password,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(throttled.headers().get(header::SET_COOKIE).is_none());
    let body = read_body(throttled).await;
    assert_eq!(body["error"]["code"], "too_many_requests");
    assert_eq!(body["error"]["message"], "too many requests");
    let raw = body.to_string();
    assert!(
        !raw.contains("throttle@relayterm.local"),
        "throttled response must not echo the identifier: {raw}",
    );
    assert!(
        !raw.contains(bad_password),
        "throttled response must not echo the offered password: {raw}",
    );
    assert!(
        !raw.contains("retry"),
        "v1 throttled response intentionally omits Retry-After detail: {raw}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_throttle_unknown_email_path_records_into_the_throttler(pool: PgPool) {
    // SPEC.md probe-resistance contract: the `unknown-email` branch of
    // the login route MUST call `record_failure` against the same
    // throttle key the `wrong-password` branch uses. The wire shape of
    // both branches is already byte-identical (collapsed to 401 with
    // `bad_credentials`); this test pins that the throttle channel
    // does not become a side-channel.
    //
    // Note on test shape: a single email address is either known OR
    // unknown — we cannot exercise BOTH branches against the SAME
    // normalized key in one test. The shared-key invariant is enforced
    // at the code level (the route uses one `throttle_key` local for
    // both branches). This test exercises the unknown-email branch
    // end-to-end as the missing half — the wrong-password branch is
    // exercised by `login_throttle_blocks_after_threshold_with_safe_response`.
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "shared-known@relayterm.local",
        throttler.clone(),
    )
    .await;

    let unknown_email = "shared-unknown@relayterm.local";
    let bad_password = "WRONG-PROBE-PASSWORD-DO-NOT-LEAK";
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": unknown_email,
                    "password": bad_password,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "unknown-email failure should still be 401 below threshold",
        );
    }

    // Fourth unknown-email attempt is throttled with the same wire
    // shape a wrong-password throttle would produce.
    let throttled = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": unknown_email,
                "password": bad_password,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = read_body(throttled).await;
    assert_eq!(body["error"]["code"], "too_many_requests");

    // The known user (different normalized key) is unaffected — the
    // throttle is per-key, not global.
    let unrelated = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "shared-known@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(unrelated.status(), StatusCode::OK);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn correct_password_during_active_block_returns_429(pool: PgPool) {
    // Pins the operator-side expectation documented in
    // `docs/production-auth.md` "Verifying a deploy" step 8: a correct
    // password attempted DURING an active block must continue to
    // return 429 (not 200). Without this guarantee an operator who
    // triggers their own throttle by typing could "talk past" the
    // block by submitting the right password — which would defeat the
    // throttle for any attacker who happens to guess correctly while
    // the bucket is open.
    //
    // Distinct from `successful_login_clears_throttle_bucket`, which
    // exercises the under-threshold path (success clears the bucket
    // BEFORE the block engages).
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "block-correct@relayterm.local",
        throttler.clone(),
    )
    .await;

    // Trip the threshold (max_failures = 3 in tight_throttler).
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": "block-correct@relayterm.local",
                    "password": "definitely-not-the-password",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // Correct password during the block: still 429, not 200. No cookie
    // is minted; the password verify never runs (the throttle check
    // short-circuits before the user lookup).
    let blocked = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "block-correct@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.headers().get(header::SET_COOKIE).is_none());
    let body = read_body(blocked).await;
    assert_eq!(body["error"]["code"], "too_many_requests");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_failed_audit_reasons_split_bad_credentials_and_throttled(pool: PgPool) {
    // The route emits `login_failed` with `reason = "bad_credentials"`
    // for the under-threshold failures AND with `reason = "throttled"`
    // once the bucket trips. Both are best-effort but must reach the
    // persisted feed under normal operation. Audit payload redaction is
    // re-checked against `AUDIT_FORBIDDEN_SUBSTRINGS` for both reasons.
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "audit-throttle@relayterm.local",
        throttler.clone(),
    )
    .await;

    // Per-test `#[sqlx::test]` databases start empty and the bootstrap
    // helper above does NOT emit any `LoginFailed` rows on success
    // (only `first_user_created`). So the post-test counts are the
    // ground truth — no baseline subtraction needed.

    let bad_password = "WRONG-AUDIT-PASSWORD-DO-NOT-LEAK-1234";
    for _ in 0..3 {
        let _ = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": "audit-throttle@relayterm.local",
                    "password": bad_password,
                }),
            ))
            .await
            .unwrap();
    }
    // One throttled attempt past the threshold.
    let throttled = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "audit-throttle@relayterm.local",
                "password": bad_password,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

    let rows = PgAuditEventRepository::new(pool.clone())
        .recent(200)
        .await
        .unwrap();
    let failed: Vec<_> = rows
        .iter()
        .filter(|e| e.kind == AuditEventKind::LoginFailed)
        .collect();
    assert_eq!(
        failed.len(),
        4,
        "expected 3 bad_credentials + 1 throttled rows; got {}: {failed:?}",
        failed.len(),
    );
    let bad_count = failed
        .iter()
        .filter(|e| {
            e.payload
                .to_string()
                .contains("\"reason\":\"bad_credentials\"")
        })
        .count();
    let throttled_count = failed
        .iter()
        .filter(|e| e.payload.to_string().contains("\"reason\":\"throttled\""))
        .count();
    assert_eq!(bad_count, 3, "expected 3 bad_credentials rows");
    assert_eq!(throttled_count, 1, "expected 1 throttled row");

    for row in &failed {
        assert!(row.actor_id.is_none(), "login_failed actor_id must be NULL");
        let raw = row.payload.to_string();
        assert!(
            !raw.contains(bad_password),
            "audit payload must not echo offered password: {raw}",
        );
        assert!(
            !raw.contains("audit-throttle@relayterm.local"),
            "audit payload must not echo identifier: {raw}",
        );
        assert_audit_payload_redacted(&row.payload, AuditEventKind::LoginFailed);
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn successful_login_clears_throttle_bucket(pool: PgPool) {
    // A correct-password login must clear the throttle bucket so a
    // legitimate user who typo'd two attempts under the threshold can
    // log in successfully on the third try and not be locked out on
    // the fourth typo. With max_failures = 3 we record two failures,
    // then succeed, then record three more — only the three "after
    // success" failures should count, leaving the next attempt still
    // allowed (but failing for bad credentials, NOT for throttle).
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "clear@relayterm.local",
        throttler.clone(),
    )
    .await;

    // Two wrong attempts (under threshold).
    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": "clear@relayterm.local",
                    "password": "definitely-not-the-password",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    // Successful login MUST clear the bucket.
    let success = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "clear@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(success.status(), StatusCode::OK);
    assert_eq!(throttler.tracked_keys(), 0, "success must drop the entry");

    // Three more failures bring the bucket right to threshold.
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": "clear@relayterm.local",
                    "password": "still-not-the-password",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    // The fourth wrong attempt is now throttled (threshold tripped on
    // the third post-success failure).
    let throttled = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "clear@relayterm.local",
                "password": "still-not-the-password",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn bad_origin_login_does_not_engage_throttler(pool: PgPool) {
    // A CSRF-rejected login must NOT increment the throttle bucket —
    // the request is dropped before the route logic runs. Without this
    // guarantee an attacker on a third-party origin could lock out a
    // legitimate user by triggering 403s against their email.
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "csrf-throttle@relayterm.local",
        throttler.clone(),
    )
    .await;

    for _ in 0..10 {
        let resp = app
            .clone()
            .oneshot(auth_post_with_origin(
                "/api/v1/auth/login",
                json!({
                    "email": "csrf-throttle@relayterm.local",
                    "password": "any-wrong-password",
                }),
                "https://evil.example.com",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        throttler.tracked_keys(),
        0,
        "CSRF-rejected login must NOT touch the throttle map",
    );

    // A legitimate login from the allow-listed origin still works.
    let success = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "csrf-throttle@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(success.status(), StatusCode::OK);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn login_throttle_is_keyed_on_normalized_email(pool: PgPool) {
    // Casing variants of the same email MUST share the throttle key;
    // a probe cannot work around the bucket by alternating capitalization.
    let throttler = tight_throttler();
    let (app, _) = setup_with_first_user_and_tight_throttler(
        pool.clone(),
        "norm@relayterm.local",
        throttler.clone(),
    )
    .await;

    for email in [
        "Norm@relayterm.local",
        "NORM@RELAYTERM.LOCAL",
        "norm@relayterm.local",
    ] {
        let resp = app
            .clone()
            .oneshot(auth_post(
                "/api/v1/auth/login",
                json!({
                    "email": email,
                    "password": "wrong-password-here",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    // Threshold tripped — even the canonical lower-case form is now
    // throttled because the bucket is shared.
    let throttled = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "norm@relayterm.local",
                "password": "wrong-password-here",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
}

// ----------------------------------------------------------------------
// Current-user session management (`/api/v1/auth/sessions`)
// ----------------------------------------------------------------------

/// Log the bootstrap user in and return the cookie token. The two-step
/// bootstrap-then-login dance mirrors what the SPA does and produces a
/// real `relayterm_session` cookie that the protected routes accept.
async fn login_and_get_token(app: &Router, email: &str) -> String {
    let resp = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": email,
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = extract_set_cookie(&resp).expect("login Set-Cookie");
    cookie_token_from_set_cookie(&cookie).to_owned()
}

/// Build a `POST` to a session-management route with a session cookie
/// and an empty body, allow-listed `Origin` so the `CsrfGuard` passes.
fn session_post_with_cookie(uri: &str, cookie_value: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, TEST_AUTH_ORIGIN)
        .header(header::COOKIE, format!("relayterm_session={cookie_value}"))
        .body(Body::from("{}"))
        .unwrap()
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn list_sessions_returns_only_callers_sessions_marks_current(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "list@relayterm.local").await;
    let current_token = login_and_get_token(&app, "list@relayterm.local").await;

    // Mint a second session for the same user via AuthService so we
    // can confirm the listing surfaces both rows and the response
    // marks only the current session.
    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create second session");
    let other_id = other.session.id;
    drop(other);

    // Mint a third session belonging to a DIFFERENT user — must NOT
    // appear in the response (cross-user redaction in SQL).
    let foreign_user = create_user(&pool, "foreign-list").await;
    let foreign = auth
        .create_session(foreign_user, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create foreign session");
    let foreign_id = foreign.session.id;
    drop(foreign);

    let resp = app
        .oneshot(auth_get_with_cookie(
            "/api/v1/auth/sessions",
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body["sessions"].as_array().expect("sessions array");
    assert_eq!(arr.len(), 2, "must include only caller's sessions: {body}");

    let foreign_id_str = foreign_id.to_string();
    assert!(
        !body.to_string().contains(&foreign_id_str),
        "foreign session id must not leak into wire body: {body}"
    );

    // Wire shape: token-redacted by construction; check explicitly.
    for forbidden in [
        "token_hash",
        "session_token",
        "password",
        "password_hash",
        "bootstrap_token",
    ] {
        assert!(
            !body.to_string().contains(forbidden),
            "session list must not expose `{forbidden}`: {body}"
        );
    }

    // Exactly one row is `current = true` and it is the caller's
    // current session (i.e. NOT the second-session id we minted).
    let other_id_str = other_id.to_string();
    let mut current_count = 0;
    let mut saw_current_marker_on_other = false;
    for item in arr {
        if item["current"].as_bool().unwrap() {
            current_count += 1;
            if item["id"].as_str().unwrap() == other_id_str {
                saw_current_marker_on_other = true;
            }
        }
        // Every item carries the SessionStatus discriminator.
        assert!(item["status"].is_string(), "missing status: {item}");
    }
    assert_eq!(current_count, 1, "exactly one current=true row");
    assert!(
        !saw_current_marker_on_other,
        "second-session id must not be marked current",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn list_sessions_requires_authentication(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "anon@relayterm.local").await;
    let resp = app
        .oneshot(get_no_auth("/api/v1/auth/sessions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_other_session_returns_204_does_not_clear_cookie_and_audits(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "revoke@relayterm.local").await;
    let current_token = login_and_get_token(&app, "revoke@relayterm.local").await;

    // Mint another session owned by the same user.
    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create second session");
    let other_id = other.session.id;
    drop(other);

    let resp = app
        .clone()
        .oneshot(session_post_with_cookie(
            &format!("/api/v1/auth/sessions/{other_id}/revoke"),
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    // No cookie clear because the revoked session is not the current one.
    assert!(
        extract_set_cookie(&resp).is_none(),
        "non-current revoke must not clear cookie",
    );

    // Caller's cookie still works after revoking a different session.
    let me = app
        .clone()
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &current_token))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);

    // Audit row written — payload must redact and mark current_session=false.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let event = recent
        .iter()
        .find(|e| e.kind == AuditEventKind::SessionRevoked)
        .expect("session_revoked audit row");
    assert_eq!(event.actor_id, Some(user_id));
    assert_eq!(
        event.payload["session_id"].as_str().unwrap(),
        other_id.to_string(),
    );
    assert_eq!(event.payload["current_session"], false);
    assert_audit_payload_redacted(&event.payload, AuditEventKind::SessionRevoked);
    // Token plaintext MUST NOT survive into the audit payload.
    assert!(
        !event.payload.to_string().contains(&current_token),
        "audit payload must not echo cookie token",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_current_session_returns_204_clears_cookie_and_invalidates(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "self@relayterm.local").await;
    let current_token = login_and_get_token(&app, "self@relayterm.local").await;

    // Discover the current session's id from the listing response.
    let listed = app
        .clone()
        .oneshot(auth_get_with_cookie(
            "/api/v1/auth/sessions",
            &current_token,
        ))
        .await
        .unwrap();
    let body = read_body(listed).await;
    let current_session_id = body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["current"].as_bool().unwrap())
        .expect("current session present")["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let resp = app
        .clone()
        .oneshot(session_post_with_cookie(
            &format!("/api/v1/auth/sessions/{current_session_id}/revoke"),
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let clear = extract_set_cookie(&resp).expect("Set-Cookie present");
    assert!(clear.contains("Max-Age=0"), "cookie cleared: {clear}");
    assert!(clear.contains("HttpOnly"));
    assert!(clear.contains("SameSite=Strict"));

    // The same cookie is now revoked — /me MUST 401.
    let me = app
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &current_token))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::UNAUTHORIZED);

    // Audit payload marks current_session=true.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let event = recent
        .iter()
        .find(|e| e.kind == AuditEventKind::SessionRevoked)
        .expect("session_revoked audit row");
    assert_eq!(event.actor_id, Some(user_id));
    assert_eq!(event.payload["current_session"], true);
    assert_audit_payload_redacted(&event.payload, AuditEventKind::SessionRevoked);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_foreign_session_returns_404_writes_no_audit(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "owner@relayterm.local").await;
    let token = login_and_get_token(&app, "owner@relayterm.local").await;

    // Mint a session that belongs to a DIFFERENT user.
    let foreign_user = create_user(&pool, "foreign-revoke").await;
    let auth = test_auth(&Db::from_pool(pool.clone()));
    let foreign = auth
        .create_session(foreign_user, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create foreign session");
    let foreign_id = foreign.session.id;
    drop(foreign);

    let resp = app
        .oneshot(session_post_with_cookie(
            &format!("/api/v1/auth/sessions/{foreign_id}/revoke"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "foreign session id must collapse to 404"
    );

    // Foreign row must NOT have transitioned.
    let row = relayterm_db::PgUserSessionRepository::new(pool.clone())
        .get(foreign_id)
        .await
        .unwrap()
        .expect("foreign row still present");
    assert!(
        row.revoked_at.is_none(),
        "foreign session must not be revoked",
    );
    assert!(row.revoked_reason.is_none());

    // No session_revoked audit row written.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    assert!(
        !recent
            .iter()
            .any(|e| e.kind == AuditEventKind::SessionRevoked),
        "no session_revoked audit row should be written for a 404",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_session_idempotent_call_writes_no_duplicate_audit(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "idem@relayterm.local").await;
    let token = login_and_get_token(&app, "idem@relayterm.local").await;

    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create second session");
    let other_id = other.session.id;
    drop(other);

    // First revoke — transitions, writes one audit row.
    let r1 = app
        .clone()
        .oneshot(session_post_with_cookie(
            &format!("/api/v1/auth/sessions/{other_id}/revoke"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::NO_CONTENT);

    // Second revoke — already-revoked, no-op, no new audit row.
    let r2 = app
        .clone()
        .oneshot(session_post_with_cookie(
            &format!("/api/v1/auth/sessions/{other_id}/revoke"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::NO_CONTENT);

    let count = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == AuditEventKind::SessionRevoked)
        .count();
    assert_eq!(
        count, 1,
        "redundant revoke must not write a second audit row",
    );
}

/// `POST /api/v1/auth/sessions/:id/revoke` with a disallowed `Origin`
/// MUST 403 and MUST NOT mutate the targeted row. Note: this handler
/// has no body extractor (no `Json<...>`), so the canonical
/// "malformed JSON + bad Origin → 403 not 400" form of the
/// pre-body-parse test (see `bad_origin_rejects_before_body_parsing`)
/// does not apply — there is no body-parse path to race the guard
/// against. The invariant pinned here is the lifecycle one: a
/// rejected request leaves the addressed row in `revoked_at IS NULL`.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_session_bad_origin_returns_403_before_db_mutation(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "csrf@relayterm.local").await;
    let token = login_and_get_token(&app, "csrf@relayterm.local").await;

    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create second session");
    let other_id = other.session.id;
    drop(other);

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/auth/sessions/{other_id}/revoke"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://evil.example.com")
        .header(header::COOKIE, format!("relayterm_session={token}"))
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "csrf_origin_mismatch");

    // Row must NOT have been touched by the rejected request.
    let row = relayterm_db::PgUserSessionRepository::new(pool.clone())
        .get(other_id)
        .await
        .unwrap()
        .unwrap();
    assert!(row.revoked_at.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_all_except_current_revokes_others_keeps_current_returns_count(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "all@relayterm.local").await;
    let current_token = login_and_get_token(&app, "all@relayterm.local").await;

    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other_a = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create other_a");
    let other_a_id = other_a.session.id;
    drop(other_a);
    let other_b = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create other_b");
    let other_b_id = other_b.session.id;
    drop(other_b);

    // Foreign user with their own session.
    let foreign_user = create_user(&pool, "foreign-revoke-all").await;
    let foreign = auth
        .create_session(foreign_user, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create foreign session");
    let foreign_id = foreign.session.id;
    drop(foreign);

    let resp = app
        .clone()
        .oneshot(session_post_with_cookie(
            "/api/v1/auth/sessions/revoke-all-except-current",
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Cookie stays — the user wants to keep the current session.
    assert!(
        extract_set_cookie(&resp).is_none(),
        "revoke-all-except-current must not clear current cookie",
    );
    let body = read_body(resp).await;
    assert_eq!(body["revoked_count"], 2);

    // Token redaction sentinels.
    for forbidden in ["token_hash", "session_token", "password_hash"] {
        assert!(
            !body.to_string().contains(forbidden),
            "response must not expose `{forbidden}`: {body}"
        );
    }

    // Caller's current session still works.
    let me = app
        .clone()
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &current_token))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);

    // Other sessions are revoked.
    let repo = relayterm_db::PgUserSessionRepository::new(pool.clone());
    for id in [other_a_id, other_b_id] {
        let row = repo.get(id).await.unwrap().unwrap();
        assert!(row.revoked_at.is_some(), "{id} must be revoked");
    }
    // Foreign user's session untouched.
    let f_row = repo.get(foreign_id).await.unwrap().unwrap();
    assert!(f_row.revoked_at.is_none(), "foreign session untouched");

    // Single sessions_revoked audit row.
    let events = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let event = events
        .iter()
        .find(|e| e.kind == AuditEventKind::SessionsRevoked)
        .expect("sessions_revoked audit row");
    assert_eq!(event.actor_id, Some(user_id));
    assert_eq!(event.payload["revoked_count"], 2);
    assert_audit_payload_redacted(&event.payload, AuditEventKind::SessionsRevoked);
    // Per-row session ids must NOT appear in the payload.
    for id in [other_a_id, other_b_id] {
        assert!(
            !event.payload.to_string().contains(&id.to_string()),
            "sessions_revoked payload must not carry per-row ids",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_all_except_current_with_no_others_writes_no_audit(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "noop@relayterm.local").await;
    let token = login_and_get_token(&app, "noop@relayterm.local").await;

    // Caller's current session is the only one. The call must succeed
    // with `revoked_count: 0` and write zero audit rows.
    let resp = app
        .oneshot(session_post_with_cookie(
            "/api/v1/auth/sessions/revoke-all-except-current",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["revoked_count"], 0);

    let events = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|e| e.kind == AuditEventKind::SessionsRevoked),
        "no-op call must not write a sessions_revoked audit row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn revoke_all_except_current_bad_origin_returns_403_before_mutation(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "csrf2@relayterm.local").await;
    let token = login_and_get_token(&app, "csrf2@relayterm.local").await;

    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create second session");
    let other_id = other.session.id;
    drop(other);

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/sessions/revoke-all-except-current")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ORIGIN, "https://evil.example.com")
        .header(header::COOKIE, format!("relayterm_session={token}"))
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let row = relayterm_db::PgUserSessionRepository::new(pool.clone())
        .get(other_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.revoked_at.is_none(),
        "rejected request must not have revoked anything",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn session_management_routes_require_authentication(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "anon2@relayterm.local").await;

    // GET /sessions
    let resp = app
        .clone()
        .oneshot(get_no_auth("/api/v1/auth/sessions"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // POST /sessions/:id/revoke
    let resp = app
        .clone()
        .oneshot(json_post_no_auth(
            &format!("/api/v1/auth/sessions/{}/revoke", uuid::Uuid::new_v4()),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // POST /sessions/revoke-all-except-current
    let resp = app
        .oneshot(json_post_no_auth(
            "/api/v1/auth/sessions/revoke-all-except-current",
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ----------------------------------------------------------------------
// Current-user password change (`POST /api/v1/auth/change-password`)
// ----------------------------------------------------------------------

const TEST_AUTH_NEW_PASSWORD: &str = "TEST-AUTH-NEW-PASSWORD-DO-NOT-LEAK-9876";

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn change_password_succeeds_revokes_other_sessions_and_audits(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "rotate@relayterm.local").await;
    let current_token = login_and_get_token(&app, "rotate@relayterm.local").await;

    // Mint two extra browser sessions for the same user via AuthService —
    // they should be revoked as part of the rotation while the current
    // cookie keeps working.
    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other_a = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create other session A");
    let other_a_id = other_a.session.id;
    drop(other_a);
    let other_b = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create other session B");
    let other_b_id = other_b.session.id;
    drop(other_b);

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/auth/change-password",
            json!({
                "current_password": TEST_AUTH_PASSWORD,
                "new_password": TEST_AUTH_NEW_PASSWORD,
            }),
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["revoked_other_sessions"], 2);

    // Wire body must not echo any password material or token shapes.
    let body_str = body.to_string();
    for forbidden in [
        TEST_AUTH_PASSWORD,
        TEST_AUTH_NEW_PASSWORD,
        "current_password",
        "new_password",
        "password_hash",
        "session_token",
        "token_hash",
        "argon2id",
    ] {
        assert!(
            !body_str.contains(forbidden),
            "change-password response must not contain `{forbidden}`: {body_str}"
        );
    }

    // Current cookie is still valid — /me succeeds.
    let me = app
        .clone()
        .oneshot(auth_get_with_cookie("/api/v1/auth/me", &current_token))
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);

    // Old password no longer works.
    let stale = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "rotate@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

    // New password DOES work.
    let fresh = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "rotate@relayterm.local",
                "password": TEST_AUTH_NEW_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(fresh.status(), StatusCode::OK);

    // Both other sessions now revoked.
    let session_repo = relayterm_db::PgUserSessionRepository::new(pool.clone());
    for id in [other_a_id, other_b_id] {
        let row = session_repo.get(id).await.unwrap().expect("session row");
        assert!(
            row.revoked_at.is_some(),
            "other session {id} must be revoked",
        );
        assert_eq!(row.revoked_reason.as_deref(), Some("password_changed"));
    }

    // Single password_changed audit row, payload carries only the count.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let event = recent
        .iter()
        .find(|e| e.kind == AuditEventKind::PasswordChanged)
        .expect("password_changed audit row");
    assert_eq!(event.actor_id, Some(user_id));
    assert_eq!(event.payload["revoked_other_sessions"], 2);
    assert!(
        event.payload.get("changed_at").is_some(),
        "audit payload should carry `changed_at`",
    );
    assert_audit_payload_redacted(&event.payload, AuditEventKind::PasswordChanged);

    // Token plaintext + password material MUST NOT survive into audit.
    let raw = event.payload.to_string();
    assert!(!raw.contains(&current_token));
    assert!(!raw.contains(TEST_AUTH_PASSWORD));
    assert!(!raw.contains(TEST_AUTH_NEW_PASSWORD));
    // Per-session ids never appear — the count is the only payload field
    // with cardinality.
    assert!(!raw.contains(&other_a_id.to_string()));
    assert!(!raw.contains(&other_b_id.to_string()));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn change_password_with_no_other_sessions_returns_zero_count(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "solo@relayterm.local").await;
    let current_token = login_and_get_token(&app, "solo@relayterm.local").await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/auth/change-password",
            json!({
                "current_password": TEST_AUTH_PASSWORD,
                "new_password": TEST_AUTH_NEW_PASSWORD,
            }),
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["revoked_other_sessions"], 0);

    // Audit row IS still written — the password change itself is the
    // signal worth logging — and carries the honest zero count.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    let event = recent
        .iter()
        .find(|e| e.kind == AuditEventKind::PasswordChanged)
        .expect("password_changed audit row");
    assert_eq!(event.actor_id, Some(user_id));
    assert_eq!(event.payload["revoked_other_sessions"], 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn change_password_wrong_current_returns_401_does_not_mutate(pool: PgPool) {
    let (app, user_id) = setup_with_first_user(pool.clone(), "wrong@relayterm.local").await;
    let current_token = login_and_get_token(&app, "wrong@relayterm.local").await;

    // Mint another session. It must NOT be revoked when the rotation
    // fails on a wrong current password.
    let auth = test_auth(&Db::from_pool(pool.clone()));
    let other = auth
        .create_session(user_id, chrono::Duration::days(30), chrono::Utc::now())
        .await
        .expect("create other session");
    let other_id = other.session.id;
    drop(other);

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/auth/change-password",
            json!({
                "current_password": "definitely-not-the-real-password",
                "new_password": TEST_AUTH_NEW_PASSWORD,
            }),
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");

    // Old password STILL works — the password row was not touched.
    let still_old = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "wrong@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(still_old.status(), StatusCode::OK);

    // New password DOES NOT work — it was never persisted.
    let new_rejected = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "wrong@relayterm.local",
                "password": TEST_AUTH_NEW_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(new_rejected.status(), StatusCode::UNAUTHORIZED);

    // Other session still active — wrong-current must NOT cascade revoke.
    let session_repo = relayterm_db::PgUserSessionRepository::new(pool.clone());
    let row = session_repo
        .get(other_id)
        .await
        .unwrap()
        .expect("session row");
    assert!(
        row.revoked_at.is_none(),
        "wrong-current must not revoke other sessions",
    );

    // No password_changed audit row.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    assert!(
        !recent
            .iter()
            .any(|e| e.kind == AuditEventKind::PasswordChanged),
        "wrong-current attempt must not write a password_changed audit row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn change_password_short_new_password_returns_400(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "short@relayterm.local").await;
    let current_token = login_and_get_token(&app, "short@relayterm.local").await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/auth/change-password",
            json!({
                "current_password": TEST_AUTH_PASSWORD,
                "new_password": "tiny",
            }),
            &current_token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // No password_changed audit row.
    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    assert!(
        !recent
            .iter()
            .any(|e| e.kind == AuditEventKind::PasswordChanged),
        "validation failure must not write a password_changed audit row",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn change_password_requires_authentication(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "anon3@relayterm.local").await;

    let resp = app
        .oneshot(json_post_no_auth(
            "/api/v1/auth/change-password",
            json!({
                "current_password": TEST_AUTH_PASSWORD,
                "new_password": TEST_AUTH_NEW_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn change_password_bad_origin_returns_403_before_verify(pool: PgPool) {
    let (app, _user_id) = setup_with_first_user(pool.clone(), "csrf3@relayterm.local").await;
    let current_token = login_and_get_token(&app, "csrf3@relayterm.local").await;

    let resp = app
        .clone()
        .oneshot(json_post_with_origin(
            "/api/v1/auth/change-password",
            json!({
                "current_password": TEST_AUTH_PASSWORD,
                "new_password": TEST_AUTH_NEW_PASSWORD,
            }),
            &current_token,
            "https://evil.example.com",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // The CSRF rejection happens BEFORE the password verify and BEFORE
    // any DB write. Old password must STILL work, new password must
    // NOT, and no audit row was written.
    let still_old = app
        .clone()
        .oneshot(auth_post(
            "/api/v1/auth/login",
            json!({
                "email": "csrf3@relayterm.local",
                "password": TEST_AUTH_PASSWORD,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(still_old.status(), StatusCode::OK);

    let recent = PgAuditEventRepository::new(pool.clone())
        .recent(50)
        .await
        .unwrap();
    assert!(
        !recent
            .iter()
            .any(|e| e.kind == AuditEventKind::PasswordChanged),
        "CSRF-rejected request must not write a password_changed audit row",
    );
}

// ----------------------------------------------------------------------
// Terminal recording read API
// ----------------------------------------------------------------------

/// Sentinel bytes inserted into a chunk payload. Tests assert the raw
/// sentinel never reaches a JSON body except via the base64 wrapper.
const RECORDING_API_PAYLOAD_SENTINEL: &[u8] = b"PTY-OUTPUT-SENTINEL-API-7Q";

/// Create a terminal-session row directly via the repository, bypassing
/// the API. Owned by `owner` and bound to `profile`.
async fn create_terminal_session_row(
    pool: &PgPool,
    owner: UserId,
    profile: relayterm_core::ids::ServerProfileId,
) -> relayterm_core::ids::TerminalSessionId {
    PgTerminalSessionRepository::new(pool.clone())
        .create(relayterm_core::repository::CreateTerminalSession {
            owner_id: owner,
            server_profile_id: profile,
            status: TerminalSessionStatus::Starting,
            cols: 80,
            rows: 24,
        })
        .await
        .expect("create terminal session row")
        .id
}

/// Append a chunk row to the recording for `session`. Wraps the
/// repository call so tests can keep the payload literal small.
async fn append_recording_chunk(
    pool: &PgPool,
    session: relayterm_core::ids::TerminalSessionId,
    seq_start: i64,
    seq_end: i64,
    payload: &[u8],
) {
    use relayterm_core::repository::TerminalRecordingRepository;
    PgTerminalRecordingRepository::new(pool.clone())
        .append_chunk(relayterm_core::repository::CreateTerminalRecordingChunk {
            terminal_session_id: session,
            seq_start,
            seq_end,
            byte_len: payload.len() as i32,
            payload: payload.to_vec(),
            encryption: relayterm_core::TerminalRecordingPayloadEncryption::None,
            compression: relayterm_core::TerminalRecordingCompression::None,
        })
        .await
        .expect("append recording chunk");
}

async fn append_recording_marker(
    pool: &PgPool,
    session: relayterm_core::ids::TerminalSessionId,
    kind: relayterm_core::TerminalRecordingMarkerKind,
    seq: i64,
    payload: Value,
) {
    use relayterm_core::repository::TerminalRecordingRepository;
    PgTerminalRecordingRepository::new(pool.clone())
        .append_marker(relayterm_core::repository::CreateTerminalRecordingMarker {
            terminal_session_id: session,
            kind,
            seq,
            payload,
        })
        .await
        .expect("append recording marker");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_read_routes_return_401_without_session_cookie(pool: PgPool) {
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
        login_throttler: test_login_throttler(),
    };
    let app = router(state);
    let bogus = uuid::Uuid::new_v4();

    for path in [
        format!("/api/v1/terminal-sessions/{bogus}/recording/metadata"),
        format!("/api/v1/terminal-sessions/{bogus}/recording/chunks"),
        format!("/api/v1/terminal-sessions/{bogus}/recording/markers"),
    ] {
        let resp = app.clone().oneshot(get_no_auth(&path)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "path={path}");
        let body = read_body(resp).await;
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["error"]["message"], "unauthorized");
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_unknown_session_returns_404(pool: PgPool) {
    let (app, _user, cookie) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{bogus}/recording/metadata"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(body["error"]["message"], "terminal_session not found");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_routes_foreign_owned_returns_indistinguishable_404(pool: PgPool) {
    // Same shape as the existing terminal-session ownership tests: a
    // foreign-owned id MUST collapse to a byte-identical 404 response
    // with a wholly-bogus id, so cross-user existence is never leaked.
    let other_user = create_user(&pool, "other").await;
    let foreign_profile = make_owned_profile(
        &pool,
        other_user,
        &test_vault(),
        "foreign-rec",
        "foreign-rec.example.com",
    )
    .await;
    let foreign_session = create_terminal_session_row(&pool, other_user, foreign_profile).await;
    // Recording rows on the foreign session — the route must not even
    // hint at their existence to a different caller.
    append_recording_chunk(&pool, foreign_session, 1, 4, RECORDING_API_PAYLOAD_SENTINEL).await;
    append_recording_marker(
        &pool,
        foreign_session,
        relayterm_core::TerminalRecordingMarkerKind::Started,
        0,
        json!({}),
    )
    .await;

    let (app, _user, cookie) = setup(pool.clone()).await;

    let bogus = uuid::Uuid::new_v4();
    for path_template in [
        "/api/v1/terminal-sessions/{}/recording/metadata",
        "/api/v1/terminal-sessions/{}/recording/chunks",
        "/api/v1/terminal-sessions/{}/recording/markers",
    ] {
        let bogus_path = path_template.replace("{}", &bogus.to_string());
        let bogus_resp = app
            .clone()
            .oneshot(get(&bogus_path, &cookie))
            .await
            .unwrap();
        let bogus_status = bogus_resp.status();
        let bogus_body = read_body(bogus_resp).await;
        assert_eq!(bogus_status, StatusCode::NOT_FOUND);

        let foreign_path = path_template.replace("{}", &foreign_session.to_string());
        let resp = app
            .clone()
            .oneshot(get(&foreign_path, &cookie))
            .await
            .unwrap();
        assert_eq!(resp.status(), bogus_status, "path={foreign_path}");
        let body = read_body(resp).await;
        assert_eq!(
            body, bogus_body,
            "foreign-owned 404 must be byte-identical to a genuine 404 (path={foreign_path})",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_empty_session_returns_no_recording(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "empty-rec",
        "empty-rec.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;

    let resp = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/metadata"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["has_recording"], false);
    assert_eq!(body["chunk_count"], 0);
    assert_eq!(body["marker_count"], 0);
    assert!(body["first_seq"].is_null());
    assert!(body["last_seq"].is_null());
    assert!(body["first_recorded_at"].is_null());
    assert!(body["last_recorded_at"].is_null());

    // Empty chunks / markers lists.
    let chunks = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/chunks"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(chunks.status(), StatusCode::OK);
    let chunks_body = read_body(chunks).await;
    assert_eq!(chunks_body.as_array().unwrap().len(), 0);

    let markers = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/markers"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(markers.status(), StatusCode::OK);
    let markers_body = read_body(markers).await;
    assert_eq!(markers_body.as_array().unwrap().len(), 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_routes_owner_can_read_metadata_chunks_markers(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "rec-owner",
        "rec-owner.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;
    append_recording_marker(
        &pool,
        session,
        relayterm_core::TerminalRecordingMarkerKind::Started,
        0,
        json!({}),
    )
    .await;
    append_recording_chunk(&pool, session, 1, 4, RECORDING_API_PAYLOAD_SENTINEL).await;
    append_recording_chunk(&pool, session, 100, 110, b"second-chunk").await;
    append_recording_marker(
        &pool,
        session,
        relayterm_core::TerminalRecordingMarkerKind::Resized,
        17,
        json!({ "cols": 132, "rows": 40 }),
    )
    .await;

    // Metadata.
    let resp = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/metadata"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["has_recording"], true);
    assert_eq!(body["chunk_count"], 2);
    assert_eq!(body["marker_count"], 2);
    assert_eq!(body["first_seq"], 1);
    assert_eq!(body["last_seq"], 110);
    assert!(body["first_recorded_at"].is_string());
    assert!(body["last_recorded_at"].is_string());
    assert_eq!(
        body["terminal_session_id"].as_str().unwrap(),
        session.to_string()
    );

    // Chunks: data_b64 round-trips back to the sentinel; raw sentinel
    // string must NOT appear anywhere outside the base64 surface.
    let chunks = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/chunks"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(chunks.status(), StatusCode::OK);
    let chunks_body = read_body(chunks).await;
    let arr = chunks_body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let first = &arr[0];
    assert_eq!(first["seq_start"], 1);
    assert_eq!(first["seq_end"], 4);
    assert_eq!(first["encryption"], "none");
    assert_eq!(first["compression"], "none");
    assert_eq!(
        first["byte_len"],
        RECORDING_API_PAYLOAD_SENTINEL.len() as i64
    );
    let data_b64 = first["data_b64"].as_str().expect("data_b64 string");
    let decoded = relayterm_protocol::output_data_decode(data_b64)
        .expect("data_b64 decodes via the protocol helper");
    assert_eq!(decoded, RECORDING_API_PAYLOAD_SENTINEL);

    // Sentinel must NOT be present anywhere in the JSON body except via
    // the base64-encoded `data_b64` field.
    let raw_json = chunks_body.to_string();
    let sentinel_str = std::str::from_utf8(RECORDING_API_PAYLOAD_SENTINEL).unwrap();
    assert!(
        !raw_json.contains(sentinel_str),
        "raw payload sentinel must not appear in JSON body: {raw_json}",
    );

    // Markers.
    let markers = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/markers"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(markers.status(), StatusCode::OK);
    let markers_body = read_body(markers).await;
    let marr = markers_body.as_array().unwrap();
    assert_eq!(marr.len(), 2);
    assert_eq!(marr[0]["kind"], "started");
    assert_eq!(marr[0]["seq"], 0);
    assert_eq!(marr[1]["kind"], "resized");
    assert_eq!(marr[1]["seq"], 17);
    assert_eq!(marr[1]["payload"]["cols"], 132);
    assert_eq!(marr[1]["payload"]["rows"], 40);

    // Marker payload must NOT contain terminal byte sentinels — markers
    // are metadata-only by writer contract.
    let raw_markers = markers_body.to_string();
    assert!(
        !raw_markers.contains(sentinel_str),
        "marker JSON must not contain raw terminal bytes: {raw_markers}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunks_from_seq_filter_works(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "rec-from-seq",
        "rec-from-seq.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;
    for &(start, end) in &[(1_i64, 9_i64), (100, 109), (200, 209), (300, 309)] {
        append_recording_chunk(&pool, session, start, end, b"data").await;
    }

    let resp = app
        .clone()
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/chunks?from_seq=150"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let starts: Vec<i64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["seq_start"].as_i64().unwrap())
        .collect();
    assert_eq!(starts, vec![200, 300]);

    // limit=1 still returns the smallest matching item.
    let resp = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/chunks?from_seq=150&limit=1"),
            &cookie,
        ))
        .await
        .unwrap();
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["seq_start"], 200);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_routes_reject_negative_from_seq(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "rec-neg",
        "rec-neg.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;

    for path in [
        format!("/api/v1/terminal-sessions/{session}/recording/chunks?from_seq=-1"),
        format!("/api/v1/terminal-sessions/{session}/recording/markers?from_seq=-5"),
    ] {
        let resp = app.clone().oneshot(get(&path, &cookie)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "path={path}");
        let body = read_body(resp).await;
        assert_eq!(body["error"]["code"], "invalid_input");
        // Wire body must not echo the offending value.
        assert!(
            !body.to_string().contains("-1") && !body.to_string().contains("-5"),
            "wire body must not echo the offending from_seq value: {body}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_routes_reject_malformed_query_params(pool: PgPool) {
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "rec-malformed",
        "rec-malformed.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;

    // axum's `Query` extractor 400s on non-numeric / out-of-range values
    // BEFORE the handler runs. The default `QueryRejection` body is plain
    // text (not the API JSON envelope) — confirm the offending value is
    // NOT reflected in the bytes a probe would observe. A future change
    // that swaps the rejection for a JSON envelope MUST keep the same
    // "no value reflected" property.
    for (path, sentinel) in [
        (
            format!("/api/v1/terminal-sessions/{session}/recording/chunks?limit=not-a-number"),
            "not-a-number",
        ),
        (
            format!("/api/v1/terminal-sessions/{session}/recording/chunks?from_seq=abc"),
            "abc",
        ),
    ] {
        let resp = app.clone().oneshot(get(&path, &cookie)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "path={path}");
        let bytes = to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("read body");
        let body_text = String::from_utf8_lossy(&bytes);
        assert!(
            !body_text.contains(sentinel),
            "wire body must not echo offending query value `{sentinel}`: {body_text}",
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunks_limit_caps_at_max(pool: PgPool) {
    // Asking for limit=u32::MAX must clamp at the API-layer ceiling
    // (1024) — no caller can pull a whole session's worth of chunks
    // in one query. With only 5 inserted rows we can't observe the
    // 1024 ceiling directly here; the unit-test `limit_clamping_table`
    // in `routes::v1::terminal_recordings::tests` pins the clamp logic
    // exactly. This test pins that the route accepts u32::MAX without
    // 500-ing on the bind / query path.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "rec-limit",
        "rec-limit.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;
    // Insert a small handful — clamp logic doesn't need 1024+ rows to
    // verify it accepted the request and didn't 500.
    for i in 0..5_i64 {
        let seq_start = 1 + i * 10;
        append_recording_chunk(&pool, session, seq_start, seq_start + 5, b"data").await;
    }

    let resp = app
        .oneshot(get(
            &format!("/api/v1/terminal-sessions/{session}/recording/chunks?limit=4294967295"),
            &cookie,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 5);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_read_routes_write_no_audit_rows(pool: PgPool) {
    // Read endpoints are intentionally NOT audited — pin the contract
    // so a future "add a recording_read audit_kind" change is forced
    // through review.
    let (app, user_id, cookie) = setup(pool.clone()).await;
    let profile = make_owned_profile(
        &pool,
        user_id,
        &test_vault(),
        "rec-noaudit",
        "rec-noaudit.example.com",
    )
    .await;
    let session = create_terminal_session_row(&pool, user_id, profile).await;
    append_recording_chunk(&pool, session, 1, 2, RECORDING_API_PAYLOAD_SENTINEL).await;
    append_recording_marker(
        &pool,
        session,
        relayterm_core::TerminalRecordingMarkerKind::Started,
        0,
        json!({}),
    )
    .await;

    let audit = PgAuditEventRepository::new(pool.clone());
    let before = audit.recent(50).await.unwrap().len();

    for path in [
        format!("/api/v1/terminal-sessions/{session}/recording/metadata"),
        format!("/api/v1/terminal-sessions/{session}/recording/chunks"),
        format!("/api/v1/terminal-sessions/{session}/recording/markers"),
    ] {
        let resp = app.clone().oneshot(get(&path, &cookie)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "path={path}");
    }

    let after = audit.recent(50).await.unwrap().len();
    assert_eq!(
        before, after,
        "recording read endpoints must not write audit rows",
    );
}

// ----------------------------------------------------------------------
// Session policy (/api/v1/config/session-policy)
// ----------------------------------------------------------------------
//
// Public read-only surface that exposes the deployment's effective
// detached-live-PTY TTL to the authenticated frontend so production UX
// copy can stop hardcoding the legacy `~30s` literal. Three properties
// the route pins:
//
//   1. Authenticated. Missing cookie → 401 before any state read.
//   2. Returns the configured TTL — the value the orchestrator is
//      actually running with (`TerminalSessionManager::detach_ttl`),
//      not whatever the default is.
//   3. Wire shape is minimal. No secret-shaped / vault / cookie /
//      CSRF / db / env-name fields leak through — the redaction
//      backstop is the `AUDIT_FORBIDDEN_SUBSTRINGS` sweep.

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn session_policy_unauthorized_without_session_cookie(pool: PgPool) {
    let (app, _user, _cookie) = setup(pool).await;
    let resp = app
        .oneshot(get_no_auth("/api/v1/config/session-policy"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn session_policy_returns_default_ttl(pool: PgPool) {
    // Default `TerminalSessionManager::new` uses the SPEC-pinned
    // `relayterm_terminal::DETACHED_LIVE_PTY_TTL` (30 s). The endpoint
    // surfaces the orchestrator's live value so a future production
    // override does not regress to the constant silently.
    let (app, _user, cookie) = setup(pool).await;
    let resp = app
        .oneshot(get("/api/v1/config/session-policy", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(
        body["detached_live_pty_ttl_seconds"], 30,
        "endpoint must return the orchestrator's live detach TTL",
    );
    // Phase 1B.1 per-user live-PTY ceiling. The default manager
    // applies `DEFAULT_MAX_LIVE_PTY_PER_USER` (= 8) — pinned here so
    // a future change to either constant surfaces in CI rather than
    // silently in production behaviour.
    assert_eq!(
        body["max_live_pty_sessions_per_user"], 8,
        "endpoint must return the default per-user live-PTY ceiling",
    );
    // Phase 1B.2a per-user starting-burst ceiling. The default
    // manager applies `DEFAULT_MAX_STARTING_PER_USER` (= 4) — same
    // pinning rationale as the live ceiling above.
    assert_eq!(
        body["max_starting_sessions_per_user"], 4,
        "endpoint must return the default per-user starting-burst ceiling",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn session_policy_returns_configured_custom_ttl(pool: PgPool) {
    // Drive the route from a router whose manager runs with a custom
    // TTL (here 7 s — well outside the 30 s default and inside the
    // `5..=86_400` validator bound). The route MUST read the live
    // orchestrator's value, not a hardcoded constant. This is the
    // load-bearing honesty contract for the Phase 1A UI copy.
    let (app, _user, cookie) = setup_with_full_state_short_ttl(
        pool,
        default_probe(),
        Arc::new(SshAuthCheckService::new(default_auth_checker())),
        default_pty_bridge(),
        std::time::Duration::from_secs(7),
    )
    .await;
    let resp = app
        .oneshot(get("/api/v1/config/session-policy", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["detached_live_pty_ttl_seconds"], 7);
    // The custom-TTL setup helper uses the manager's default per-user
    // cap. Phase 1B.1: a custom TTL must not silently regress the cap
    // surface — both fields must be present on every successful response.
    assert_eq!(body["max_live_pty_sessions_per_user"], 8);
    // Phase 1B.2a starting-burst cap: same defense — surfaces on
    // every successful response, even when the test rewires the TTL.
    assert_eq!(body["max_starting_sessions_per_user"], 4);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn session_policy_response_shape_is_minimal(pool: PgPool) {
    // Exactly three wire fields (Phase 1B.2a: TTL + per-user live cap +
    // per-user starting-burst cap). A future widening (vault / cookie
    // / CSRF / database url / env name / deployment paths /
    // deployment-wide quota) MUST fail this assertion until the
    // redaction posture is consciously re-reviewed.
    let (app, _user, cookie) = setup(pool).await;
    let resp = app
        .oneshot(get("/api/v1/config/session-policy", &cookie))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let obj = body.as_object().expect("response is a JSON object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "detached_live_pty_ttl_seconds",
            "max_live_pty_sessions_per_user",
            "max_starting_sessions_per_user",
        ],
        "session-policy response carries exactly the TTL knob + per-user live cap + per-user \
         starting cap; got {keys:?}",
    );
    // Sentinel sweep: nothing secret-shaped reaches the wire. Re-uses
    // the existing audit-redaction substring set so a future widening
    // that smuggles credentials / sessions / vault material through
    // this route fails the same backstop the audit feed already pins.
    let raw = body.to_string();
    for forbidden in AUDIT_FORBIDDEN_SUBSTRINGS {
        assert!(
            !raw.contains(forbidden),
            "session-policy response must not contain `{forbidden}`: {raw}",
        );
    }
    // Belt-and-suspenders: the env name is intentionally NOT a public
    // wire field even though it is operator-documented elsewhere.
    assert!(
        !raw.contains("RELAYTERM_"),
        "session-policy response must not leak env names",
    );
    // The deployment-wide ceiling (a Phase 1B.2 deployment concept
    // that is deliberately NOT exposed on this surface — operator-
    // only, fingerprinting risk) MUST NOT smuggle onto the wire.
    assert!(
        !raw.contains("max_live_pty_sessions_per_deployment"),
        "session-policy response must not expose the deployment-wide ceiling",
    );
}
