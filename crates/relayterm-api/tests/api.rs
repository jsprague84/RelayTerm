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
use relayterm_api::{AppState, router};
use relayterm_core::ids::UserId;
use relayterm_core::repository::{
    CreateHost, CreateKnownHostEntry, CreateServerProfile, CreateSshIdentity, CreateUser,
    HostRepository, KnownHostEntryRepository, ServerProfileRepository, SessionEventRepository,
    SshIdentityRepository, TerminalSessionRepository, UserRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_core::terminal_session::TerminalSessionStatus;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_ssh_port, validate_ssh_username,
};
use relayterm_db::{
    Db, PgHostRepository, PgKnownHostEntryRepository, PgServerProfileRepository,
    PgSessionEventRepository, PgSshIdentityRepository, PgTerminalSessionRepository,
    PgUserRepository,
};
use relayterm_ssh::{
    AuthAttemptKind, AuthCheckOutcome, AuthCheckTarget, CapturedHostKey, HostKeyPreflightService,
    ProbeError, ProbeTarget, SshAuthCheckService, SshAuthChecker, SshHostKeyProbe,
};
use relayterm_terminal::TerminalSessionManager;
use relayterm_vault::VaultService;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use zeroize::Zeroizing;

const PRIVATE_KEY_MARKER: &[u8] = b"REDACT-MARKER-API-9F2B";

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
    let user_id = create_user(&pool, "dev").await;
    let db = Db::from_pool(pool);
    let terminal_sessions = test_terminal_manager(&db);
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check,
        terminal_sessions,
        dev_user_id: Some(user_id),
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: None,
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: None,
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: Some(user_id),
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(Arc::new(probe))),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: Some(user_id),
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(Arc::new(ErroringAuthChecker(
            ProbeError::Unreachable,
        )))),
        terminal_sessions,
        dev_user_id: Some(user_id),
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
    let state = AppState {
        db,
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: Some(user_id),
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: None,
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
        terminal_sessions,
        dev_user_id: Some(user_id),
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
        terminal_sessions,
        dev_user_id: Some(user_id),
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
async fn create_terminal_session_returns_starting_placeholder(pool: PgPool) {
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

    assert_eq!(body["status"], "starting");
    assert_eq!(body["cols"], 120);
    assert_eq!(body["rows"], 30);
    assert_eq!(
        body["server_profile_id"].as_str().unwrap(),
        profile_id.to_string()
    );
    assert!(body["id"].is_string());
    assert!(body["created_at"].is_string());
    assert!(body["closed_at"].is_null());

    // Stub message must explicitly disclaim PTY readiness.
    let message = body["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("pty") && message.contains("not implemented"),
        "create response message must signal stub scope, got: {message}",
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

    // A `created` lifecycle event was persisted.
    let session_id = body["id"].as_str().unwrap();
    let session_uuid: uuid::Uuid = session_id.parse().unwrap();
    let events = PgSessionEventRepository::new(pool.clone())
        .list_for_session(relayterm_core::ids::TerminalSessionId::from_uuid(
            session_uuid,
        ))
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionEventKind::Created);
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: None,
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

/// The create response's `message` must explicitly disclaim PTY/SSH
/// readiness so a future "helpful" rewording is forced through review.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn create_terminal_session_message_does_not_overclaim_pty_or_ssh(pool: PgPool) {
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
        message.contains("pty") && message.contains("not implemented"),
        "create message must signal PTY-not-implemented scope, got: {message}",
    );
    for forbidden in [
        "session opened",
        "shell ready",
        "shell spawned",
        "connected to",
        "authenticated",
        "logged in",
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

/// Receive the next text frame and decode it as a [`ServerMsg`]. Panics
/// on transport error / non-text frame so the test surfaces them loudly.
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
async fn ws_attach_emits_session_attached_stub(pool: PgPool) {
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
            assert_eq!(
                status,
                relayterm_protocol::SessionAttachStatus::AttachedStub
            );
            // Wire wording must explicitly disclaim PTY scope.
            let lower = message.to_lowercase();
            assert!(
                lower.contains("pty") && lower.contains("not implemented"),
                "session_attached message must signal PTY-not-implemented: {message}",
            );
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
    let state = AppState {
        db,
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
        terminal_sessions,
        dev_user_id: None,
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
async fn ws_input_returns_pty_not_implemented_and_does_not_echo_payload(pool: PgPool) {
    let (app, user_id) = setup(pool.clone()).await;
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
    let _ = recv_server_msg(&mut socket).await;

    // Sentinel string the test then asserts is NOT present in any
    // response — pinned so a future "helpful" handler change that
    // reflects input back fails loudly.
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
            assert_eq!(code, relayterm_protocol::ErrorCode::PtyNotImplemented);
            let lower = message.to_lowercase();
            assert!(
                lower.contains("pty") && lower.contains("not implemented"),
                "input rejection must name PTY-not-implemented: {message}",
            );
        }
        other => panic!("expected Error frame, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ws_binary_frame_is_rejected_without_echo(pool: PgPool) {
    use tokio_tungstenite::tungstenite::Message;
    let (app, user_id) = setup(pool.clone()).await;
    let profile_id = make_trusted_profile(
        &pool,
        user_id,
        &test_vault(),
        "primary",
        "ws-binary.example.com",
        "SHA256:ws-binary",
    )
    .await;
    let session_id = create_session_via_api(&app, profile_id).await;
    let addr = spawn_app(app).await;
    let mut socket = open_ws(addr, session_id).await;
    let _ = recv_server_msg(&mut socket).await;

    // Sentinel bytes the test asserts the server never reflects. JSON
    // protocol rejects binary frames wholesale — payload must not appear
    // in the error response or in any subsequent frame.
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
        "binary frame rejection must NOT echo payload bytes: {raw}",
    );
    match resp {
        relayterm_protocol::ServerMsg::Error { code, .. } => {
            assert_eq!(code, relayterm_protocol::ErrorCode::InvalidMessage);
        }
        other => panic!("expected Error, got {other:?}"),
    }
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

    // The attachment row's detached_at is stamped.
    let attachments = PgTerminalSessionRepository::new(pool.clone())
        .list_attachments(session_id)
        .await
        .unwrap();
    assert_eq!(attachments.len(), 1);
    assert!(attachments[0].detached_at.is_some());

    // Detached event was written.
    let events = PgSessionEventRepository::new(pool)
        .list_for_session(session_id)
        .await
        .unwrap();
    let detached = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::Detached)
        .count();
    assert_eq!(detached, 1);
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

    let events = PgSessionEventRepository::new(pool)
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
}
