//! Postgres-backed integration tests for the repository implementations.
//!
//! These tests are gated behind the `postgres-tests` feature so that
//! `cargo test --workspace` stays runnable without infra. To execute:
//!
//! ```bash
//! docker compose -f deploy/docker-compose.yml up -d postgres
//! DATABASE_URL=postgres://relayterm:relayterm@127.0.0.1:5432/relayterm \
//!   cargo test -p relayterm-db --features postgres-tests
//! ```
//!
//! `sqlx::test` provisions a fresh per-test database and runs the
//! migrations under `apps/backend/migrations/` against it before the test
//! body executes. The `DATABASE_URL` user must therefore have `CREATEDB`
//! privileges (the bundled Compose user does).
//!
//! Coverage is intentionally narrow: each repository gets a happy-path
//! round-trip that exercises the SQL, the row → domain mapping, and any
//! enum / newtype reconstruction. Edge cases (conflicts, validation
//! errors) are deferred until the corresponding HTTP handlers land.

#![cfg(feature = "postgres-tests")]

use chrono::Utc;
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::repository::{
    AuditEventRepository, CreateAuditEvent, CreateHost, CreateKnownHostEntry, CreateServerProfile,
    CreateSessionEvent, CreateSshIdentity, CreateTerminalSession, CreateTerminalSessionAttachment,
    CreateUser, HostRepository, KnownHostEntryRepository, RepositoryError, ServerProfileRepository,
    SessionEventRepository, SshIdentityRepository, TerminalSessionRepository, UserRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_core::terminal_session::TerminalSessionStatus;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_profile_name, validate_ssh_port,
    validate_ssh_username, validate_tag,
};
use relayterm_db::{
    PgAuditEventRepository, PgHostRepository, PgKnownHostEntryRepository,
    PgServerProfileRepository, PgSessionEventRepository, PgSshIdentityRepository,
    PgTerminalSessionRepository, PgUserRepository,
};
use serde_json::json;
use sqlx::PgPool;

// ----------------------------------------------------------------------
// Fixtures
// ----------------------------------------------------------------------

async fn make_user(pool: &PgPool) -> relayterm_core::user::User {
    PgUserRepository::new(pool.clone())
        .create(CreateUser {
            email: format!("u+{}@example.com", uuid::Uuid::new_v4()),
            display_name: "Test User".to_owned(),
        })
        .await
        .expect("create user")
}

async fn make_host(
    pool: &PgPool,
    owner: &relayterm_core::user::User,
) -> relayterm_core::host::Host {
    PgHostRepository::new(pool.clone())
        .create(CreateHost {
            owner_id: owner.id,
            display_name: validate_host_display_name("Prod DB").unwrap(),
            hostname: validate_hostname("db-1.internal.example.com").unwrap(),
            port: validate_ssh_port(22).unwrap(),
            default_username: validate_ssh_username("deploy").unwrap(),
        })
        .await
        .expect("create host")
}

async fn make_identity(
    pool: &PgPool,
    owner: &relayterm_core::user::User,
) -> relayterm_core::ssh_identity::SshIdentity {
    let unique = uuid::Uuid::new_v4().simple().to_string();
    PgSshIdentityRepository::new(pool.clone())
        .create(CreateSshIdentity {
            owner_id: owner.id,
            name: "ed25519-test".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 AAAA...".to_vec(),
            encrypted_private_key: b"opaque-ciphertext".to_vec(),
            fingerprint_sha256: format!("SHA256:{unique}"),
        })
        .await
        .expect("create ssh_identity")
}

async fn make_profile(
    pool: &PgPool,
    owner: &relayterm_core::user::User,
    host: &relayterm_core::host::Host,
    identity: &relayterm_core::ssh_identity::SshIdentity,
) -> relayterm_core::server_profile::ServerProfile {
    let unique = uuid::Uuid::new_v4().simple().to_string();
    PgServerProfileRepository::new(pool.clone())
        .create(CreateServerProfile {
            owner_id: owner.id,
            name: validate_profile_name(&format!("profile-{unique}")).unwrap(),
            host_id: host.id,
            ssh_identity_id: identity.id,
            username_override: Some(validate_ssh_username("root").unwrap()),
            tags: vec![
                validate_tag("prod").unwrap(),
                validate_tag("us-east-1").unwrap(),
            ],
        })
        .await
        .expect("create server_profile")
}

// ----------------------------------------------------------------------
// User
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_round_trip(pool: PgPool) {
    let repo = PgUserRepository::new(pool.clone());
    let created = repo
        .create(CreateUser {
            email: "alice@example.com".to_owned(),
            display_name: "Alice".to_owned(),
        })
        .await
        .unwrap();

    let by_id = repo
        .get(created.id)
        .await
        .unwrap()
        .expect("get returns row");
    assert_eq!(by_id, created);

    let by_email = repo
        .get_by_email("ALICE@example.com")
        .await
        .unwrap()
        .expect("case-insensitive lookup");
    assert_eq!(by_email.id, created.id);

    let now = Utc::now();
    repo.touch_last_login(created.id, now).await.unwrap();
    let touched = repo.get(created.id).await.unwrap().unwrap();
    assert!(touched.last_login_at.is_some());
}

// ----------------------------------------------------------------------
// Host
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn host_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgHostRepository::new(pool.clone());

    let created = repo
        .create(CreateHost {
            owner_id: user.id,
            display_name: validate_host_display_name("Bastion").unwrap(),
            hostname: validate_hostname("bastion.example.com").unwrap(),
            port: validate_ssh_port(2222).unwrap(),
            default_username: validate_ssh_username("ops").unwrap(),
        })
        .await
        .unwrap();

    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.port.get(), 2222);

    let listed = repo.list_for_user(user.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
}

// ----------------------------------------------------------------------
// SshIdentity
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ssh_identity_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgSshIdentityRepository::new(pool.clone());

    let created = repo
        .create(CreateSshIdentity {
            owner_id: user.id,
            name: "primary".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 AAAA-public".to_vec(),
            encrypted_private_key: b"\x00\x01\x02opaque".to_vec(),
            fingerprint_sha256: "SHA256:abcd1234".to_owned(),
        })
        .await
        .unwrap();

    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.key_type, SshKeyType::Ed25519);
    // Bytes round-trip exactly through the domain field.
    assert_eq!(
        fetched.encrypted_private_key,
        b"\x00\x01\x02opaque".to_vec()
    );

    let listed = repo.list_for_user(user.id).await.unwrap();
    assert_eq!(listed.len(), 1);
}

/// The encrypted private key must only be reachable via the
/// `encrypted_private_key` field. It must not appear in `Debug` output
/// (which the tracing macros call) or in repository error messages.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn ssh_identity_private_key_not_leaked(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgSshIdentityRepository::new(pool.clone());

    // A distinctive marker that would be easy to grep for if it leaked.
    let secret_marker = b"REDACT-MARKER-9F2B".to_vec();

    let identity = repo
        .create(CreateSshIdentity {
            owner_id: user.id,
            name: "leak-test".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"ssh-ed25519 PUB".to_vec(),
            encrypted_private_key: secret_marker.clone(),
            fingerprint_sha256: "SHA256:leak-test".to_owned(),
        })
        .await
        .unwrap();

    // The bytes are still reachable through the domain field.
    assert_eq!(identity.encrypted_private_key, secret_marker);

    // Debug output must not include the bytes.
    let dbg_identity = format!("{identity:?}");
    assert!(
        !dbg_identity.contains("REDACT-MARKER-9F2B"),
        "encrypted_private_key leaked into SshIdentity Debug output: {dbg_identity}",
    );
    assert!(
        dbg_identity.contains("redacted"),
        "Debug output should mention redaction: {dbg_identity}",
    );

    // Same for the input struct, in case it gets traced before insertion.
    let create_input = CreateSshIdentity {
        owner_id: user.id,
        name: "leak-test-input".to_owned(),
        key_type: SshKeyType::Ed25519,
        public_key: b"pub".to_vec(),
        encrypted_private_key: secret_marker.clone(),
        fingerprint_sha256: "SHA256:input".to_owned(),
    };
    let dbg_input = format!("{create_input:?}");
    assert!(
        !dbg_input.contains("REDACT-MARKER-9F2B"),
        "encrypted_private_key leaked into CreateSshIdentity Debug output: {dbg_input}",
    );

    // A failed create (FK violation here, since the owner_id is bogus)
    // must not echo the bytes back through the error.
    let bogus_owner = relayterm_core::ids::UserId::new();
    let err = repo
        .create(CreateSshIdentity {
            owner_id: bogus_owner,
            name: "fk-fail".to_owned(),
            key_type: SshKeyType::Ed25519,
            public_key: b"pub".to_vec(),
            encrypted_private_key: secret_marker.clone(),
            fingerprint_sha256: "SHA256:fk-fail".to_owned(),
        })
        .await
        .expect_err("FK violation must error");
    let err_str = err.to_string();
    let err_dbg = format!("{err:?}");
    assert!(
        !err_str.contains("REDACT-MARKER-9F2B"),
        "encrypted_private_key leaked into RepositoryError Display: {err_str}",
    );
    assert!(
        !err_dbg.contains("REDACT-MARKER-9F2B"),
        "encrypted_private_key leaked into RepositoryError Debug: {err_dbg}",
    );
}

// ----------------------------------------------------------------------
// ServerProfile
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn server_profile_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let repo = PgServerProfileRepository::new(pool.clone());

    let created = repo
        .create(CreateServerProfile {
            owner_id: user.id,
            name: validate_profile_name("Prod / us-east-1").unwrap(),
            host_id: host.id,
            ssh_identity_id: identity.id,
            username_override: Some(validate_ssh_username("root").unwrap()),
            tags: vec![
                validate_tag("prod").unwrap(),
                validate_tag("k8s_node").unwrap(),
            ],
        })
        .await
        .unwrap();

    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.tags.len(), 2);
    assert_eq!(fetched.tags[0].as_str(), "prod");

    let listed = repo.list_for_user(user.id).await.unwrap();
    assert_eq!(listed.len(), 1);
}

// ----------------------------------------------------------------------
// KnownHostEntry
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn known_host_entry_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let repo = PgKnownHostEntryRepository::new(pool.clone());

    let created = repo
        .create(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:host-fp".to_owned(),
            public_key: b"ssh-ed25519 AAAA-host-key".to_vec(),
        })
        .await
        .unwrap();

    let by_fp = repo
        .find_by_fingerprint(host.id, "SHA256:host-fp")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_fp, created);

    let listed = repo.list_for_host(host.id).await.unwrap();
    assert_eq!(listed.len(), 1);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn known_host_entry_record_trusted_inserts_with_timestamp(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let repo = PgKnownHostEntryRepository::new(pool.clone());

    let entry = repo
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:trusted-fp".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    assert!(
        entry.trusted_at.is_some(),
        "fresh insert must stamp trusted_at"
    );
    assert!(entry.revoked_at.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn known_host_entry_record_trusted_is_idempotent(pool: PgPool) {
    // Re-recording the same (host_id, fingerprint) returns the existing
    // row with the original `trusted_at` preserved — important so the
    // audit timestamp doesn't drift on every retry.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let repo = PgKnownHostEntryRepository::new(pool.clone());

    let first = repo
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:idem".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();

    let second = repo
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:idem".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();

    assert_eq!(first.id, second.id, "must return the same row on re-trust");
    assert_eq!(
        first.trusted_at, second.trusted_at,
        "trusted_at must not drift"
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn known_host_entry_record_trusted_rejects_revoked_row(pool: PgPool) {
    // A revoked row must NEVER be silently re-trusted. `record_trusted`
    // returns Conflict so the API layer can surface a clear 409 instead
    // of misreporting success. The row stays revoked.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let repo = PgKnownHostEntryRepository::new(pool.clone());

    let entry = repo
        .create(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:revoked-fp".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE known_host_entries SET revoked_at = NOW() WHERE id = $1")
        .bind(entry.id.into_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let err = repo
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:revoked-fp".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, RepositoryError::Conflict { entity: "known_host_entry", ref constraint } if constraint == "revoked"),
        "expected revoked conflict, got: {err:?}",
    );

    // The row is still revoked and untrusted.
    let row = repo
        .find_by_fingerprint(host.id, "SHA256:revoked-fp")
        .await
        .unwrap()
        .unwrap();
    assert!(row.revoked_at.is_some(), "revoked_at must remain set");
    assert!(
        row.trusted_at.is_none(),
        "trusted_at must NOT have been stamped",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn known_host_entry_record_trusted_stamps_existing_untrusted_row(pool: PgPool) {
    // A pre-existing row inserted via plain `create` (no trusted_at) gets
    // stamped on re-record.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let repo = PgKnownHostEntryRepository::new(pool.clone());

    let untrusted = repo
        .create(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:was-untrusted".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    assert!(untrusted.trusted_at.is_none());

    let trusted = repo
        .record_trusted(CreateKnownHostEntry {
            host_id: host.id,
            key_type: SshKeyType::Ed25519,
            fingerprint_sha256: "SHA256:was-untrusted".to_owned(),
            public_key: b"ssh-ed25519 AAAA".to_vec(),
        })
        .await
        .unwrap();
    assert_eq!(trusted.id, untrusted.id);
    assert!(
        trusted.trusted_at.is_some(),
        "record_trusted must stamp the row"
    );
}

// ----------------------------------------------------------------------
// TerminalSession
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn terminal_session_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let repo = PgTerminalSessionRepository::new(pool.clone());

    let created = repo
        .create(CreateTerminalSession {
            owner_id: user.id,
            server_profile_id: profile.id,
            status: TerminalSessionStatus::Active,
            cols: 120,
            rows: 40,
        })
        .await
        .unwrap();

    let fetched = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(fetched, created);
    assert_eq!(fetched.status, TerminalSessionStatus::Active);

    let now = Utc::now();
    repo.set_status(created.id, TerminalSessionStatus::Closed, Some(now))
        .await
        .unwrap();
    let after = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(after.status, TerminalSessionStatus::Closed);
    assert!(after.closed_at.is_some());

    let listed = repo.list_for_user(user.id).await.unwrap();
    assert_eq!(listed.len(), 1);

    // No attachments yet — list should be empty, not error.
    let attachments = repo.list_attachments(created.id).await.unwrap();
    assert!(attachments.is_empty());
}

// ----------------------------------------------------------------------
// SessionEvent
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn session_event_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = PgTerminalSessionRepository::new(pool.clone())
        .create(CreateTerminalSession {
            owner_id: user.id,
            server_profile_id: profile.id,
            status: TerminalSessionStatus::Active,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    let repo = PgSessionEventRepository::new(pool.clone());
    let created = repo
        .create(CreateSessionEvent {
            session_id: session.id,
            kind: SessionEventKind::Created,
            payload: json!({ "by": "test" }),
        })
        .await
        .unwrap();

    assert_eq!(created.kind, SessionEventKind::Created);
    assert_eq!(created.payload, json!({ "by": "test" }));

    let listed = repo.list_for_session(session.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let by_id = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(by_id, created);
    assert!(
        repo.get(relayterm_core::ids::SessionEventId::new())
            .await
            .unwrap()
            .is_none()
    );
}

// ----------------------------------------------------------------------
// AuditEvent
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_event_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgAuditEventRepository::new(pool.clone());

    let created = repo
        .create(CreateAuditEvent {
            actor_id: Some(user.id),
            kind: AuditEventKind::LoginSucceeded,
            payload: json!({ "method": "password" }),
            remote_addr: Some("127.0.0.1".to_owned()),
        })
        .await
        .unwrap();

    assert_eq!(created.kind, AuditEventKind::LoginSucceeded);

    let recent = repo.recent(10).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].id, created.id);

    // Pre-auth event with NULL actor.
    let anon = repo
        .create(CreateAuditEvent {
            actor_id: None,
            kind: AuditEventKind::LoginFailed,
            payload: json!({ "reason": "bad_password" }),
            remote_addr: Some("10.0.0.1".to_owned()),
        })
        .await
        .unwrap();
    assert!(anon.actor_id.is_none());

    let recent2 = repo.recent(10).await.unwrap();
    assert_eq!(recent2.len(), 2);

    let by_id = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(by_id, created);
    assert!(
        repo.get(relayterm_core::ids::AuditEventId::new())
            .await
            .unwrap()
            .is_none()
    );
}

// ----------------------------------------------------------------------
// TerminalSessionAttachment
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn terminal_session_attachment_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let repo = PgTerminalSessionRepository::new(pool.clone());

    let session = repo
        .create(CreateTerminalSession {
            owner_id: user.id,
            server_profile_id: profile.id,
            status: TerminalSessionStatus::Active,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    let created = repo
        .create_attachment(CreateTerminalSessionAttachment {
            session_id: session.id,
            client_info: Some("relayterm-web/0.0.0".to_owned()),
            remote_addr: Some("127.0.0.1".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(created.session_id, session.id);
    assert!(created.detached_at.is_none());
    assert!(created.last_seen_seq.is_none());

    let listed = repo.list_attachments(session.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let by_id = repo.get_attachment(created.id).await.unwrap().unwrap();
    assert_eq!(by_id, created);
    assert!(
        repo.get_attachment(relayterm_core::ids::TerminalSessionAttachmentId::new())
            .await
            .unwrap()
            .is_none()
    );
}

// ----------------------------------------------------------------------
// Unique constraint conflict
// ----------------------------------------------------------------------

/// Inserting a second user with the same email (case-insensitive) must
/// surface as `RepositoryError::Conflict` with a non-empty constraint
/// name, and the constraint string must not echo SQL or the user's input.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_email_unique_conflict(pool: PgPool) {
    let repo = PgUserRepository::new(pool.clone());
    repo.create(CreateUser {
        email: "dup@example.com".to_owned(),
        display_name: "First".to_owned(),
    })
    .await
    .unwrap();

    let err = repo
        .create(CreateUser {
            email: "DUP@example.com".to_owned(),
            display_name: "Second".to_owned(),
        })
        .await
        .expect_err("duplicate email must conflict");

    match err {
        RepositoryError::Conflict { entity, constraint } => {
            assert_eq!(entity, "user");
            assert!(!constraint.is_empty(), "constraint name should be set");
            // Constraint name is metadata, not SQL or user input.
            assert!(
                !constraint.contains(' '),
                "constraint must not contain spaces / SQL fragments"
            );
            assert!(
                !constraint.to_ascii_lowercase().contains("dup@example.com"),
                "constraint must not echo user input: {constraint}",
            );
            // The migration calls it `users_email_key`; assert that exact value
            // so that a future schema rename surfaces here loudly.
            assert_eq!(constraint, "users_email_key");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}
