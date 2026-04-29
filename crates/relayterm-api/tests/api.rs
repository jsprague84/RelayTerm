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

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt as _;
use relayterm_api::{AppState, router};
use relayterm_core::ids::UserId;
use relayterm_core::repository::{
    CreateHost, CreateSshIdentity, CreateUser, HostRepository, SshIdentityRepository,
    UserRepository,
};
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_ssh_port, validate_ssh_username,
};
use relayterm_db::{Db, PgHostRepository, PgSshIdentityRepository, PgUserRepository};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

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
    let user_id = create_user(&pool, "dev").await;
    let state = AppState {
        db: Db::from_pool(pool),
        vault: Some(test_vault()),
        dev_user_id: Some(user_id),
    };
    (router(state), user_id)
}

/// Vault service backed by a deterministic test master key. Tests that
/// don't exercise the vault still need *some* vault instance because the
/// API state requires it for the `POST /ssh-identities` route.
fn test_vault() -> relayterm_vault::VaultService {
    relayterm_vault::VaultService::new(relayterm_vault::VaultMasterKey::from_bytes([0x77u8; 32]))
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
