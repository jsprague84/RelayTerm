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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt as _;
use relayterm_api::{AppState, router};
use relayterm_core::ids::UserId;
use relayterm_core::repository::{
    CreateHost, CreateKnownHostEntry, CreateServerProfile, CreateSshIdentity, CreateUser,
    HostRepository, KnownHostEntryRepository, ServerProfileRepository, SshIdentityRepository,
    UserRepository,
};
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_ssh_port, validate_ssh_username,
};
use relayterm_db::{
    Db, PgHostRepository, PgKnownHostEntryRepository, PgServerProfileRepository,
    PgSshIdentityRepository, PgUserRepository,
};
use relayterm_ssh::{
    AuthAttemptKind, AuthCheckOutcome, AuthCheckTarget, CapturedHostKey, HostKeyPreflightService,
    ProbeError, ProbeTarget, SshAuthCheckService, SshAuthChecker, SshHostKeyProbe,
};
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
    let state = AppState {
        db: Db::from_pool(pool),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(probe)),
        auth_check,
        dev_user_id: Some(user_id),
    };
    (router(state), user_id)
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
    let state = AppState {
        db: Db::from_pool(pool),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(Arc::new(probe))),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(Arc::new(ErroringAuthChecker(
            ProbeError::Unreachable,
        )))),
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: None,
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
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
    let state = AppState {
        db: Db::from_pool(pool),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: Arc::new(SshAuthCheckService::new(default_auth_checker())),
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
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
    let state = AppState {
        db: Db::from_pool(pool.clone()),
        vault: Some(test_vault()),
        preflight: Arc::new(HostKeyPreflightService::new(default_probe())),
        auth_check: svc,
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
