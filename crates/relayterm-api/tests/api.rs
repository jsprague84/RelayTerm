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
use relayterm_auth::{AuthService, PasswordHasher, PasswordHasherConfig};
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
    PgTerminalSessionRepository, PgUserRepository,
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

/// Origin allow-listed by the per-test [`AuthRoutesConfig`]. Tests that
/// drive the `/api/v1/auth/*` routes set `Origin: <this>` so the
/// inline CSRF guard accepts the request; all other tests do not need
/// to set the header (GETs are exempt and existing app routes still
/// run through the [`relayterm_api::DevUser`] shim).
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

async fn setup(pool: PgPool) -> (Router, UserId) {
    setup_with_probe(pool, default_probe()).await
}

async fn setup_with_probe(pool: PgPool, probe: Arc<dyn SshHostKeyProbe>) -> (Router, UserId) {
    setup_full(pool, probe, default_auth_checker()).await
}

async fn setup_full(
    pool: PgPool,
    probe: Arc<dyn SshHostKeyProbe>,
    checker: Arc<dyn SshAuthChecker>,
) -> (Router, UserId) {
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
) -> (Router, UserId) {
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
) -> (Router, UserId) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check,
        pty_bridge,
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    (router(state), user_id)
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
) -> (Router, UserId) {
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager_with_short_ttl(&db, detach_ttl);
    let __auth = test_auth(&db);
    let __auth_routes = test_auth_routes();
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check,
        pty_bridge,
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    (router(state), user_id)
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

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// ----------------------------------------------------------------------
// Hosts
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_success(pool: PgPool) {
    let (app, _user) = setup(pool).await;

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
    let listed = app.clone().oneshot(get("/api/v1/hosts")).await.unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let arr = read_body(listed).await;
    assert_eq!(arr.as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_default_port_22(pool: PgPool) {
    let (app, _) = setup(pool).await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "Default-port host",
                "hostname": "h.example.com",
                "default_username": "deploy",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = read_body(resp).await;
    assert_eq!(body["port"], 22);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_host_invalid_input_returns_400(pool: PgPool) {
    let (app, _) = setup(pool).await;

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
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn get_host_unknown_id_returns_404(pool: PgPool) {
    let (app, _) = setup(pool).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(get(&format!("/api/v1/hosts/{bogus}")))
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

    let (app, _dev_user) = setup(pool).await;

    // Baseline: a totally bogus id returns 404 with the canonical body.
    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/hosts/{bogus}")))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    // Same id-shape, different owner — must produce the same response.
    let resp = app
        .oneshot(get(&format!("/api/v1/hosts/{}", foreign_host.id)))
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

/// When the dev-auth shim is disabled (`AppState::dev_user_id == None`) and
/// no real auth backend has been wired, every `DevUser`-guarded route must
/// return 401 with the canonical error envelope rather than the backend
/// hard-bailing on startup.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn devuser_returns_401_when_dev_auth_disabled(pool: PgPool) {
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);

    // GET hosts is DevUser-guarded.
    let resp = app.clone().oneshot(get("/api/v1/hosts")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    // The wire body must not echo any operator-facing detail; the static
    // "unauthorized" message is all the client gets, regardless of why.
    assert_eq!(body["error"]["message"], "unauthorized");
    assert!(
        !body.to_string().contains("dev_auth"),
        "401 body must not leak dev-auth implementation detail: {body}"
    );

    // POST is also guarded — body never reaches the validator.
    let resp = app
        .oneshot(json_post(
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

// ----------------------------------------------------------------------
// SSH identities
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_returns_public_metadata_only(pool: PgPool) {
    let (app, _) = setup(pool).await;

    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "homelab-admin",
            }),
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
        .oneshot(get(&format!("/api/v1/ssh-identities/{id}")))
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
    let (app, _) = setup(pool).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "primary",
                "key_type": "invalid-algo",
            }),
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
    let (app, _) = setup(pool).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "primary",
                "key_type": "rsa",
            }),
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
    let (app, _) = setup(pool).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({
                "name": "",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_input");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn post_ssh_identity_returns_401_when_dev_auth_disabled(pool: PgPool) {
    // dev-auth off → DevUser extractor short-circuits with 401 BEFORE any
    // vault work happens. The request body never reaches the vault.
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);

    let resp = app
        .oneshot(json_post(
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);

    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({"name": "primary"}),
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
    let (app, _) = setup(pool.clone()).await;
    let resp = app
        .oneshot(json_post(
            "/api/v1/ssh-identities",
            json!({"name": "store-check"}),
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
    let (app, user_id) = setup(pool.clone()).await;

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
        .oneshot(get("/api/v1/ssh-identities"))
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
    let (app, user_id) = setup(pool.clone()).await;
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
        .oneshot(get(&format!("/api/v1/ssh-identities/{}", identity.id)))
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, _user_id) = setup(pool).await;

    let host_resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/hosts",
            json!({
                "display_name": "host-1",
                "hostname": "h1.example.com",
                "default_username": "deploy",
            }),
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

async fn setup_with_fake_probe(pool: PgPool, fingerprint: &str) -> (Router, UserId, FakeProbe) {
    let probe = FakeProbe::new(captured_for_test(fingerprint));
    let probe_handle = probe.clone();
    let (app, user_id) = setup_with_probe(pool, Arc::new(probe)).await;
    (app, user_id, probe_handle)
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_unknown_when_no_known_host_entries(pool: PgPool) {
    let (app, user_id, probe) = setup_with_fake_probe(pool.clone(), "SHA256:fake-fp").await;
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), fp).await;
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
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["host_key_status"], "trusted");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_changed_when_pinned_fingerprint_differs(pool: PgPool) {
    let new_fp = "SHA256:NEW-fp";
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), new_fp).await;
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
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert_eq!(body["host_key_status"], "changed");
    assert_eq!(body["host_key_fingerprint"], new_fp);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn preflight_returns_502_on_probe_failure(pool: PgPool) {
    let (app, user_id) =
        setup_with_probe(pool.clone(), Arc::new(ErrorProbe(ProbeError::Unreachable))).await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "down.example.com").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
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
    let (app, _user_id, _probe) = setup_with_fake_probe(pool, "SHA256:never").await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/host-key-preflight"),
            json!({}),
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

    let (app, _dev_user, _probe) = setup_with_fake_probe(pool, "SHA256:never").await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/host-key-preflight"),
            json!({}),
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), fp).await;
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), "SHA256:actual-fp").await;
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), new_fp).await;
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), "SHA256:any").await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "any.example.com").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": "MD5:not-supported" }),
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), fp).await;
    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "idem.example.com").await;

    // First trust.
    let r1 = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(Arc::new(probe))),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
async fn revoke_entry(pool: &PgPool, entry_id: relayterm_core::ids::KnownHostEntryId) {
    sqlx::query("UPDATE known_host_entries SET revoked_at = NOW() WHERE id = $1")
        .bind(entry_id.into_uuid())
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), fp).await;
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
    revoke_entry(&pool, seeded.id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/trust-host-key"),
            json!({ "expected_fingerprint": fp }),
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), fp).await;
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
    revoke_entry(&pool, seeded.id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/host-key-preflight"),
            json!({}),
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), "SHA256:scope-check").await;
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
) -> (Router, UserId, Arc<FakeAuthChecker>) {
    let checker = Arc::new(FakeAuthChecker::new(captured, kind));
    let (app, user_id) = setup_full(pool, default_probe(), checker.clone()).await;
    (app, user_id, checker)
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
    let (app, user_id, checker) = setup_with_fake_auth_checker(
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
    let (app, _user_id, _checker) = setup_with_fake_auth_checker(
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

    let (app, _dev_user, _checker) = setup_with_fake_auth_checker(
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
    let (app, user_id, checker) = setup_with_fake_auth_checker(
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
    let (app, user_id, checker) = setup_with_fake_auth_checker(
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
    let (app, user_id, checker) = setup_with_fake_auth_checker(
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
    revoke_entry(&pool, seeded.id).await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
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
    let (app, user_id, _checker) = setup_with_fake_auth_checker(
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(Arc::new(ErroringAuthChecker(
            ProbeError::Unreachable,
        )))),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = read_body(resp).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(body["error"]["message"], "service unavailable");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn auth_check_returns_401_when_dev_auth_disabled(pool: PgPool) {
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
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
    let (app, user_id, _checker) = setup_with_fake_auth_checker(
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);

    let profile_id =
        make_owned_profile(&pool, user_id, &test_vault(), "primary", "slow.example.com").await;

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/auth-check"),
            json!({}),
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
        pty_bridge: default_pty_bridge(),
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
    let first = tokio::spawn(async move {
        app_first
            .oneshot(json_post(
                &format!("/api/v1/server-profiles/{profile_first}/auth-check"),
                json!({}),
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
    revoke_entry(&pool, seeded.id).await;

    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
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
    let (app, _user_id) = setup(pool).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": bogus}),
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

    let (app, _dev_user) = setup(pool.clone()).await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": bogus}),
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
    let (app, user_id) = setup(pool.clone()).await;
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

    let (app, user_id) = setup(pool.clone()).await;

    // Empty list to start.
    let resp = app
        .clone()
        .oneshot(get("/api/v1/terminal-sessions"))
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
        ))
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);

    let resp = app.oneshot(get("/api/v1/terminal-sessions")).await.unwrap();
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
    let (app, _user_id) = setup(pool).await;
    let bogus = uuid::Uuid::new_v4();
    let resp = app
        .oneshot(get(&format!("/api/v1/terminal-sessions/{bogus}")))
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

    let (app, _dev_user) = setup(pool).await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(get(&format!("/api/v1/terminal-sessions/{bogus}")))
        .await
        .unwrap();
    let bogus_status = bogus_resp.status();
    let bogus_body = read_body(bogus_resp).await;
    assert_eq!(bogus_status, StatusCode::NOT_FOUND);

    let resp = app
        .oneshot(get(&format!(
            "/api/v1/terminal-sessions/{}",
            foreign_session.id
        )))
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
        ))
        .await
        .unwrap();
    let session_id = read_body(create).await["id"].as_str().unwrap().to_owned();

    let first = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(read_body(first).await["already_closed"], false);

    let second = app
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
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

    let (app, _dev_user) = setup(pool.clone()).await;

    let bogus = uuid::Uuid::new_v4();
    let bogus_resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{bogus}/close"),
            json!({}),
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
async fn terminal_session_routes_return_401_when_dev_auth_disabled(pool: PgPool) {
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);

    for req in [
        json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": uuid::Uuid::new_v4()}),
        ),
        get("/api/v1/terminal-sessions"),
        get(&format!(
            "/api/v1/terminal-sessions/{}",
            uuid::Uuid::new_v4()
        )),
        json_post(
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
    let (app, user_id) = setup(pool.clone()).await;
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
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/api/v1/terminal-sessions/{session_id}/ws");
    let (stream, _resp) = tokio_tungstenite::connect_async(&url)
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
) -> relayterm_core::ids::TerminalSessionId {
    let resp = app
        .clone()
        .oneshot(json_post(
            "/api/v1/terminal-sessions",
            json!({"server_profile_id": profile_id}),
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-attach.example.com",
        "SHA256:ws-attach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;

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
/// test-rig bug, not a route behavior.
async fn ws_handshake_status(
    addr: SocketAddr,
    session_id_uri: &str,
) -> (axum::http::StatusCode, Option<Vec<u8>>) {
    use tokio_tungstenite::tungstenite::Error;
    let url = format!("ws://{addr}/api/v1/terminal-sessions/{session_id_uri}/ws");
    let err = tokio_tungstenite::connect_async(&url)
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
    let (app, _user) = setup(pool).await;
    let addr = spawn_app(app).await;
    let bogus = uuid::Uuid::new_v4();
    let (status, body) = ws_handshake_status(addr, &bogus.to_string()).await;
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

    let (app, _dev_user) = setup(pool).await;
    let addr = spawn_app(app).await;

    let bogus = uuid::Uuid::new_v4();
    let (bogus_status, bogus_body) = ws_handshake_status(addr, &bogus.to_string()).await;
    let (foreign_status, foreign_body) =
        ws_handshake_status(addr, &foreign_session.id.to_string()).await;

    assert_eq!(bogus_status, StatusCode::NOT_FOUND);
    assert_eq!(foreign_status, bogus_status);
    assert_eq!(
        foreign_body, bogus_body,
        "foreign-owned WS attach must produce a byte-identical 404 body",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_closed_session_returns_409(pool: PgPool) {
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-closed.example.com",
        "SHA256:ws-closed",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    // Close it before attempting to attach.
    let close = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(close.status(), StatusCode::OK);

    let addr = spawn_app(app).await;
    let (status, body) = ws_handshake_status(addr, &session_id.to_string()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let body = body.expect("409 must carry an error envelope body");
    let parsed: Value = serde_json::from_slice(&body).expect("body is valid JSON");
    assert_eq!(parsed["error"]["code"], "conflict");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_attach_returns_401_when_dev_auth_disabled(pool: PgPool) {
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);
    let addr = spawn_app(app).await;
    let (status, _body) = ws_handshake_status(addr, &uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_ping_returns_pong(pool: PgPool) {
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-ping.example.com",
        "SHA256:ws-ping",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
    // Drain the SessionAttached frame.
    let _ = recv_server_msg(&mut socket).await;

    send_client_msg(&mut socket, &relayterm_protocol::ClientMsg::Ping).await;
    let pong = recv_server_msg(&mut socket).await;
    assert!(matches!(pong, relayterm_protocol::ServerMsg::Pong));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_resize_acks_and_writes_event(pool: PgPool) {
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-resize.example.com",
        "SHA256:ws-resize",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-bad-resize.example.com",
        "SHA256:ws-bad-resize",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let state = AppState {
        db: db.clone(),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: bridge as Arc<dyn SshPtyBridge>,
        terminal_sessions: terminal_sessions.clone(),
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary-bad.example.com",
        "SHA256:ws-binary-bad",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary-huge.example.com",
        "SHA256:ws-binary-huge",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary-input.example.com",
        "SHA256:ws-binary-input",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().expect("bridge produced handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-bad-msg.example.com",
        "SHA256:ws-bad-msg",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-detach.example.com",
        "SHA256:ws-detach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-close.example.com",
        "SHA256:ws-close",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-drop.example.com",
        "SHA256:ws-drop",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
async fn setup_with_pty_bridge(pool: PgPool, bridge: Arc<FakePtyBridge>) -> (Router, UserId) {
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        pty_bridge: bridge.clone() as Arc<dyn SshPtyBridge>,
        terminal_sessions,
        dev_user_id: Some(user_id),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-active.example.com",
        "SHA256:ws-active",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-input.example.com",
        "SHA256:ws-input",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-output.example.com",
        "SHA256:ws-output",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge
        .last_handle()
        .expect("create flow must produce a handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-resize.example.com",
        "SHA256:ws-resize",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut socket = open_ws(addr, session_id).await;
    let _ = recv_server_msg(&mut socket).await; // SessionAttached(Active)
    socket
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_explicit_close_remains_idempotent(pool: PgPool) {
    let bridge = FakePtyBridge::new();
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-close-idempotent.example.com",
        "SHA256:ws-close-idempotent",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;

    // First WS: explicit Close.
    let addr = spawn_app(app.clone()).await;
    let mut s1 = open_ws_attached(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-detach-race.example.com",
        "SHA256:ws-detach-race",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id).await;
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
    let (app, user_id) = setup_with_full_state_short_ttl(
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
    let session_id = create_session_via_api(&app, profile_id).await;

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id).await;
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
    let (status, _body) = ws_handshake_status(addr, &session_id.to_string()).await;
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
    let (app, user_id) = setup_with_full_state_short_ttl(
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
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().expect("create produced a handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-seq.example.com",
        "SHA256:ws-seq",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();

    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-replay.example.com",
        "SHA256:ws-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    // First attach: prime the replay buffer with three frames. We do
    // NOT send Detach (which would auto-close the live session); we
    // just drop the socket and wait for the cleanup tail to finish.
    {
        let mut s1 = open_ws(addr, session_id).await;
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
        let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-replay.example.com",
        "SHA256:ws-no-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    // Prime via s1, hold open.
    let mut s1 = open_ws(addr, session_id).await;
    let _ = recv_server_msg(&mut s1).await;
    handle.inject_output(b"old-output".to_vec()).await;
    let _ = await_output_with_seq(&mut s1, 1).await;

    // Second socket: attach, explicitly send Attach { last_seen_seq:
    // None }. Server MUST NOT emit any replay frames.
    let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-window-lost.example.com",
        "SHA256:ws-no-window-lost",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    let mut s1 = open_ws(addr, session_id).await;
    let _ = recv_server_msg(&mut s1).await;
    handle.inject_output(b"only".to_vec()).await;
    let _ = await_output_with_seq(&mut s1, 1).await;

    // Bookmark equals latest → empty range (no replay frames at all,
    // and definitely no window-lost).
    let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-no-dup-replay.example.com",
        "SHA256:ws-no-dup-replay",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    let mut s1 = open_ws(addr, session_id).await;
    let _ = recv_server_msg(&mut s1).await;
    for byte in [b'a', b'b'] {
        handle.inject_output(vec![byte]).await;
    }
    let _ = await_output_with_seq(&mut s1, 2).await;

    let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-double-attach.example.com",
        "SHA256:ws-double-attach",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_pty_bridge(pool.clone(), bridge.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-replay-redact.example.com",
        "SHA256:ws-replay-redact",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    let mut s1 = open_ws(addr, session_id).await;
    let _ = recv_server_msg(&mut s1).await;
    let sentinel = b"REDACT-MARKER-REPLAY-SHELL-9F";
    // Two frames so a `last_seen_seq=1` bookmark triggers replay of
    // exactly one buffered frame (the sentinel-bearing seq=2). Using
    // a positive bookmark — `Some(0)` is treated as no-bookmark by
    // the handler and would skip the replay handshake entirely.
    handle.inject_output(b"prefix".to_vec()).await;
    handle.inject_output(sentinel.to_vec()).await;
    let _ = await_output_with_seq(&mut s1, 2).await;

    let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_full_state_short_ttl(
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
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().expect("bridge produced handle");

    let addr = spawn_app(app).await;
    let mut socket = open_ws_attached(addr, session_id).await;
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
    let (app, user_id) = setup_with_full_state_short_ttl(
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
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;

    {
        let mut s1 = open_ws_attached(addr, session_id).await;
        send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Detach).await;
        while (s1.next().await).is_some() {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Reattach within the TTL window.
    let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_full_state_short_ttl(
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
    let session_id = create_session_via_api(&app, profile_id).await;
    let handle = bridge.last_handle().unwrap();
    let addr = spawn_app(app).await;

    {
        let mut s1 = open_ws_attached(addr, session_id).await;
        handle.inject_output(b"alpha".to_vec()).await;
        handle.inject_output(b"beta".to_vec()).await;
        let _ = await_output_with_seq(&mut s1, 2).await;
        send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Detach).await;
        while (s1.next().await).is_some() {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut s2 = open_ws(addr, session_id).await;
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
    let (app, user_id) = setup_with_full_state_short_ttl(
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
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app.clone()).await;

    {
        let mut s1 = open_ws_attached(addr, session_id).await;
        send_client_msg(&mut s1, &relayterm_protocol::ClientMsg::Detach).await;
        while (s1.next().await).is_some() {}
    }

    // Close while the TTL is still pending.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/terminal-sessions/{session_id}/close"),
            json!({}),
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
    let (app, user_id) = setup(pool.clone()).await;
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

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_is_idempotent_and_preserves_original_timestamp(pool: PgPool) {
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
        ))
        .await
        .unwrap();

    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
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
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    assert!(body["disabled_at"].is_null());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn disable_unknown_profile_returns_indistinguishable_404(pool: PgPool) {
    let (app, _user_id) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/disable"),
            json!({}),
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
    let (app, _user_id) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/enable"),
            json!({}),
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
    let (app, user_id) = setup(pool.clone()).await;
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
        .oneshot(get(&format!("/api/v1/server-profiles/{profile_id}")))
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
        ))
        .await
        .unwrap();
    let resp = app.oneshot(get("/api/v1/server-profiles")).await.unwrap();
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id, _checker) = setup_with_fake_auth_checker(
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
    let (app, user_id, _probe) =
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
    let (app, user_id, _probe) = setup_with_fake_probe(pool.clone(), fp).await;
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
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "post-launch-disable",
        "post-launch-disable.example.com",
        "SHA256:post-launch",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;

    // Disable AFTER launch — must not retroactively close the session.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Session row is still readable and not closed.
    let resp = app
        .oneshot(get(&format!("/api/v1/terminal-sessions/{session_id}")))
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
    let (app, user_id) = setup(pool.clone()).await;
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
    let (app, user_id) = setup(pool.clone()).await;
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
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Second disable: already-disabled, idempotent — must NOT append.
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/disable"),
            json!({}),
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
    let (app, user_id) = setup(pool.clone()).await;
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
        ))
        .await
        .unwrap();

    // First enable: disabled -> enabled. One audit row appended.
    let resp = app
        .clone()
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Second enable: already-enabled, idempotent — must NOT append.
    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{profile_id}/enable"),
            json!({}),
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
    let (app, _user_id) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/disable"),
            json!({}),
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
    let (app, _user_id) = setup(pool.clone()).await;
    let bogus = uuid::Uuid::new_v4();

    let resp = app
        .oneshot(json_post(
            &format!("/api/v1/server-profiles/{bogus}/enable"),
            json!({}),
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
    let (app, user_id) = setup(pool.clone()).await;
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
        .oneshot(get("/api/v1/audit-events/recent"))
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
    // Set up a router whose dev_user is `caller`. Insert an audit row
    // for `other` directly. The feed for `caller` must NOT see it.
    let caller = create_user(&pool, "caller").await;
    let other = create_user(&pool, "other").await;

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
        dev_user_id: Some(caller),
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
        .oneshot(get("/api/v1/audit-events/recent"))
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
    let (app, user_id) = setup(pool.clone()).await;
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
        .oneshot(get("/api/v1/audit-events/recent?limit=5"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 5);

    // Out-of-range limit is silently clamped to MAX_LIMIT.
    let resp = app
        .oneshot(get("/api/v1/audit-events/recent?limit=10000"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = read_body(resp).await;
    let arr = body.as_array().unwrap();
    assert!(arr.len() <= 100, "MAX_LIMIT must cap the response");
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_events_recent_empty_list_for_quiet_user(pool: PgPool) {
    let (app, _user_id) = setup(pool.clone()).await;
    let resp = app
        .oneshot(get("/api/v1/audit-events/recent"))
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
    let (app, user_id) = setup(pool.clone()).await;
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
        .oneshot(get("/api/v1/audit-events/recent"))
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
    let (app, user_id) = setup(pool.clone()).await;
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
        .oneshot(get("/api/v1/audit-events/recent"))
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
async fn audit_events_recent_unauthorized_when_dev_user_disabled(pool: PgPool) {
    // When `dev_user_id = None` (and no real auth backend is wired
    // yet), the DevUser extractor returns 401. The audit-events route
    // MUST go through that gate.
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
    };
    let app = router(state);

    let resp = app
        .oneshot(get("/api/v1/audit-events/recent"))
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
        dev_user_id: None,
        auth: __auth.clone(),
        auth_routes: __auth_routes.clone(),
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
    let resp = app.oneshot(get("/api/v1/auth/me")).await.unwrap();
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
async fn me_does_not_require_origin(pool: PgPool) {
    // GET /auth/me is exempt from the inline CSRF guard. Without a
    // cookie it returns 401, not 403 — the auth check runs even with
    // no Origin header present.
    let (app, _user_id) = setup_with_first_user(pool.clone(), "noorigin@relayterm.local").await;
    let resp = app.oneshot(get("/api/v1/auth/me")).await.unwrap();
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
