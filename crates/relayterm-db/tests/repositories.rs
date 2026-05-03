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

use chrono::{Duration, Utc};
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::ids::UserSessionId;
use relayterm_core::repository::{
    AuditEventRepository, CreateAuditEvent, CreateHost, CreateKnownHostEntry,
    CreatePasswordCredential, CreateServerProfile, CreateSessionEvent, CreateSshIdentity,
    CreateTerminalRecordingChunk, CreateTerminalRecordingMarker, CreateTerminalSession,
    CreateTerminalSessionAttachment, CreateUser, CreateUserSession, HostRepository,
    KnownHostEntryRepository, PasswordCredentialRepository, RepositoryError,
    ServerProfileRepository, SessionEventRepository, SshIdentityRepository,
    TerminalRecordingRepository, TerminalSessionRepository, UserRepository, UserSessionRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::ssh_identity::SshKeyType;
use relayterm_core::terminal_recording::{
    TerminalRecordingCompression, TerminalRecordingMarkerKind, TerminalRecordingPayloadEncryption,
};
use relayterm_core::terminal_session::TerminalSessionStatus;
use relayterm_core::validation::{
    validate_host_display_name, validate_hostname, validate_profile_name, validate_ssh_port,
    validate_ssh_username, validate_tag,
};
use relayterm_db::{
    PgAuditEventRepository, PgHostRepository, PgKnownHostEntryRepository,
    PgPasswordCredentialRepository, PgServerProfileRepository, PgSessionEventRepository,
    PgSshIdentityRepository, PgTerminalRecordingRepository, PgTerminalSessionRepository,
    PgUserRepository, PgUserSessionRepository,
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

async fn make_terminal_session(
    pool: &PgPool,
    owner: &relayterm_core::user::User,
    profile: &relayterm_core::server_profile::ServerProfile,
) -> relayterm_core::terminal_session::TerminalSession {
    PgTerminalSessionRepository::new(pool.clone())
        .create(CreateTerminalSession {
            owner_id: owner.id,
            server_profile_id: profile.id,
            status: TerminalSessionStatus::Active,
            cols: 80,
            rows: 24,
        })
        .await
        .expect("create terminal_session")
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
    // Newly-created profiles are enabled by default. Pinned here so a
    // future migration that defaults the column to `NOW()` would surface
    // as a clear test failure, not silent semantic drift.
    assert!(listed[0].disabled_at.is_none());
    assert!(!listed[0].is_disabled());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn server_profile_set_disabled_at_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let repo = PgServerProfileRepository::new(pool.clone());
    let profile = make_profile(&pool, &user, &host, &identity).await;

    // Disable: a fresh profile transitions to disabled with `Some(t)`.
    let now = Utc::now();
    let disabled = repo
        .set_disabled_at(profile.id, user.id, Some(now))
        .await
        .expect("disable owned profile");
    assert!(disabled.is_disabled());
    assert!(disabled.disabled_at.is_some());

    // Idempotent on repeated disable: the SQL writes unconditionally so a
    // second call still succeeds, but `is_disabled()` stays true. The
    // route layer wraps this with a get-then-skip so the original
    // `disabled_at` survives a redundant call.
    let again_disabled = repo
        .set_disabled_at(profile.id, user.id, Some(Utc::now()))
        .await
        .expect("idempotent disable");
    assert!(again_disabled.is_disabled());

    // Enable: clears the timestamp.
    let enabled = repo
        .set_disabled_at(profile.id, user.id, None)
        .await
        .expect("enable owned profile");
    assert!(!enabled.is_disabled());
    assert!(enabled.disabled_at.is_none());

    // Idempotent on repeated enable.
    let again_enabled = repo
        .set_disabled_at(profile.id, user.id, None)
        .await
        .expect("idempotent enable");
    assert!(!again_enabled.is_disabled());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn server_profile_set_disabled_at_unknown_returns_not_found(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgServerProfileRepository::new(pool.clone());
    let bogus = relayterm_core::ids::ServerProfileId::from_uuid(uuid::Uuid::new_v4());

    let err = repo
        .set_disabled_at(bogus, user.id, Some(Utc::now()))
        .await
        .expect_err("unknown id should be NotFound");
    assert!(matches!(
        err,
        RepositoryError::NotFound {
            entity: "server_profile"
        }
    ));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn server_profile_set_disabled_at_foreign_returns_not_found(pool: PgPool) {
    // Cross-user existence must be indistinguishable from "no such row" at
    // the repository layer. The route relies on this collapsing into a
    // single 404 — see AGENTS.md "Encountered Lessons" 2026-04-28.
    let owner = make_user(&pool).await;
    let host = make_host(&pool, &owner).await;
    let identity = make_identity(&pool, &owner).await;
    let stranger = make_user(&pool).await;
    let repo = PgServerProfileRepository::new(pool.clone());
    let profile = make_profile(&pool, &owner, &host, &identity).await;

    let err = repo
        .set_disabled_at(profile.id, stranger.id, Some(Utc::now()))
        .await
        .expect_err("foreign owner_id should be NotFound");
    assert!(matches!(
        err,
        RepositoryError::NotFound {
            entity: "server_profile"
        }
    ));
    // And the original row was not mutated.
    let untouched = repo.get(profile.id).await.unwrap().unwrap();
    assert!(!untouched.is_disabled());
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

/// `recent_for_actor` must scope to the actor and exclude `actor_id IS
/// NULL` rows. The current-user audit read route relies on this — a
/// regression here would either leak cross-user events or surface
/// pre-auth failed-login rows to a normal user.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_event_recent_for_actor_scopes_to_actor_and_excludes_null(pool: PgPool) {
    let repo = PgAuditEventRepository::new(pool.clone());
    let alice = make_user(&pool).await;
    let bob = make_user(&pool).await;

    // Alice's row.
    let a = repo
        .create(CreateAuditEvent {
            actor_id: Some(alice.id),
            kind: AuditEventKind::ServerProfileCreated,
            payload: json!({ "server_profile_id": uuid::Uuid::new_v4(), "name": "alice-prof" }),
            remote_addr: None,
        })
        .await
        .unwrap();
    // Bob's row.
    let b = repo
        .create(CreateAuditEvent {
            actor_id: Some(bob.id),
            kind: AuditEventKind::ServerProfileCreated,
            payload: json!({ "server_profile_id": uuid::Uuid::new_v4(), "name": "bob-prof" }),
            remote_addr: None,
        })
        .await
        .unwrap();
    // Pre-auth row (NULL actor) — must not appear in either feed.
    let _anon = repo
        .create(CreateAuditEvent {
            actor_id: None,
            kind: AuditEventKind::LoginFailed,
            payload: json!({ "reason": "bad_password" }),
            remote_addr: Some("203.0.113.7".to_owned()),
        })
        .await
        .unwrap();

    let alice_feed = repo.recent_for_actor(alice.id, 50).await.unwrap();
    assert_eq!(alice_feed.len(), 1);
    assert_eq!(alice_feed[0].id, a.id);

    let bob_feed = repo.recent_for_actor(bob.id, 50).await.unwrap();
    assert_eq!(bob_feed.len(), 1);
    assert_eq!(bob_feed[0].id, b.id);

    // The shared `recent` admin-shape query still sees all three.
    assert_eq!(repo.recent(50).await.unwrap().len(), 3);
}

/// `recent_for_actor` must order newest-first and clamp the row count
/// at the SQL `LIMIT`. The route's clamp covers user input; this is
/// the SQL-level guarantee.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_event_recent_for_actor_orders_and_limits(pool: PgPool) {
    let repo = PgAuditEventRepository::new(pool.clone());
    let user = make_user(&pool).await;
    let mut ids = Vec::new();
    for i in 0..5 {
        let row = repo
            .create(CreateAuditEvent {
                actor_id: Some(user.id),
                kind: AuditEventKind::ServerProfileCreated,
                payload: json!({
                    "server_profile_id": uuid::Uuid::new_v4(),
                    "name": format!("p-{i}"),
                }),
                remote_addr: None,
            })
            .await
            .unwrap();
        ids.push(row.id);
    }

    // Page of 3 must take the most recently inserted three, in
    // reverse-insertion order (recorded_at DESC, id DESC).
    let page = repo.recent_for_actor(user.id, 3).await.unwrap();
    assert_eq!(page.len(), 3);
    let observed: Vec<_> = page.iter().map(|e| e.id).collect();
    let expected: Vec<_> = ids.iter().rev().take(3).copied().collect();
    assert_eq!(observed, expected);
}

/// The `audit_events_kind_chk` CHECK constraint must accept the
/// server-profile lifecycle kinds emitted by the disable/enable routes.
/// A failure here means the migration that extended the constraint did
/// not land — the API-side audit emission would silently break.
#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn audit_event_accepts_server_profile_lifecycle_kinds(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgAuditEventRepository::new(pool.clone());

    for kind in [
        AuditEventKind::ServerProfileCreated,
        AuditEventKind::ServerProfileDisabled,
        AuditEventKind::ServerProfileEnabled,
    ] {
        let created = repo
            .create(CreateAuditEvent {
                actor_id: Some(user.id),
                kind,
                payload: json!({ "server_profile_id": uuid::Uuid::new_v4() }),
                remote_addr: None,
            })
            .await
            .expect("audit_events_kind_chk should accept lifecycle kinds");
        assert_eq!(created.kind, kind);
    }
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

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn mark_attachment_detached_idempotent_and_round_trips(pool: PgPool) {
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
    let attachment = repo
        .create_attachment(CreateTerminalSessionAttachment {
            session_id: session.id,
            client_info: None,
            remote_addr: None,
        })
        .await
        .unwrap();

    let first_at = chrono::Utc::now();
    repo.mark_attachment_detached(attachment.id, first_at, Some(42))
        .await
        .unwrap();
    let after = repo.get_attachment(attachment.id).await.unwrap().unwrap();
    assert!(after.detached_at.is_some());
    assert_eq!(after.last_seen_seq, Some(42));

    // Second call with different timestamp + seq must be a no-op:
    // COALESCE on detached_at preserves the original.
    let later = first_at + chrono::Duration::seconds(60);
    repo.mark_attachment_detached(attachment.id, later, Some(99))
        .await
        .unwrap();
    let after_second = repo.get_attachment(attachment.id).await.unwrap().unwrap();
    assert_eq!(
        after_second.detached_at, after.detached_at,
        "second detach must not overwrite the original detached_at",
    );
    assert_eq!(
        after_second.last_seen_seq,
        Some(42),
        "second detach must not overwrite the original last_seen_seq",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn mark_attachment_detached_unknown_id_returns_not_found(pool: PgPool) {
    let repo = PgTerminalSessionRepository::new(pool);
    let err = repo
        .mark_attachment_detached(
            relayterm_core::ids::TerminalSessionAttachmentId::new(),
            chrono::Utc::now(),
            None,
        )
        .await
        .expect_err("unknown attachment id must not silently succeed");
    match err {
        relayterm_core::repository::RepositoryError::NotFound { entity } => {
            assert_eq!(entity, "terminal_session_attachment");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
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

// ----------------------------------------------------------------------
// Password credentials
// ----------------------------------------------------------------------
//
// Sentinel hash strings are deliberately distinctive so a test that
// asserts "no hash leaked into Debug / RepositoryError" has something
// to grep for. They are NOT real Argon2id PHC strings — the auth
// service will produce real ones; this layer just stores text.

const PHC_SENTINEL_V1: &str = "$argon2id$v=19$m=19456,t=2,p=1$DO-NOT-LEAK-SALT$DO-NOT-LEAK-HASH-V1";
const PHC_SENTINEL_V2: &str = "$argon2id$v=19$m=19456,t=2,p=1$DO-NOT-LEAK-SALT$DO-NOT-LEAK-HASH-V2";

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn password_credential_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgPasswordCredentialRepository::new(pool.clone());

    let created = repo
        .upsert_for_user(CreatePasswordCredential {
            user_id: user.id,
            password_hash: PHC_SENTINEL_V1.to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(created.user_id, user.id);
    assert_eq!(created.password_hash, PHC_SENTINEL_V1);
    assert_eq!(created.created_at, created.updated_at);
    assert_eq!(created.created_at, created.password_changed_at);

    let fetched = repo
        .get_for_user(user.id)
        .await
        .unwrap()
        .expect("get returns row");
    assert_eq!(fetched, created);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn password_credential_upsert_replaces_hash_and_bumps_changed_at(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgPasswordCredentialRepository::new(pool.clone());

    let first = repo
        .upsert_for_user(CreatePasswordCredential {
            user_id: user.id,
            password_hash: PHC_SENTINEL_V1.to_owned(),
        })
        .await
        .unwrap();

    // Sleep a few ms so NOW() advances measurably between INSERT and UPDATE.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let second = repo
        .upsert_for_user(CreatePasswordCredential {
            user_id: user.id,
            password_hash: PHC_SENTINEL_V2.to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(second.user_id, user.id);
    assert_eq!(second.password_hash, PHC_SENTINEL_V2);
    // created_at is preserved across upsert.
    assert_eq!(second.created_at, first.created_at);
    // password_changed_at and updated_at advance.
    assert!(second.password_changed_at > first.password_changed_at);
    assert!(second.updated_at > first.updated_at);

    // get returns the post-upsert row, not a stale one.
    let fetched = repo.get_for_user(user.id).await.unwrap().unwrap();
    assert_eq!(fetched.password_hash, PHC_SENTINEL_V2);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn password_credential_get_for_user_without_password_returns_none(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgPasswordCredentialRepository::new(pool.clone());

    let result = repo.get_for_user(user.id).await.unwrap();
    assert!(
        result.is_none(),
        "get_for_user must return None for a user without a password row"
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn password_credential_get_for_nonexistent_user_returns_none(pool: PgPool) {
    let repo = PgPasswordCredentialRepository::new(pool.clone());
    // The FK is enforced on upsert (writes a row referencing users.id),
    // not on get — a get for a never-existed user id must collapse to
    // `None`, not surface an error. This matches the route layer's
    // existing "byte-identical 404 for unknown vs unauthorized" rule.
    let result = repo
        .get_for_user(relayterm_core::ids::UserId::new())
        .await
        .unwrap();
    assert!(result.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn password_credential_redaction_sentinels(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgPasswordCredentialRepository::new(pool.clone());

    let cred = repo
        .upsert_for_user(CreatePasswordCredential {
            user_id: user.id,
            password_hash: PHC_SENTINEL_V1.to_owned(),
        })
        .await
        .unwrap();

    // Domain-level Debug must redact.
    let dbg = format!("{cred:?}");
    assert!(
        !dbg.contains("DO-NOT-LEAK-HASH"),
        "PasswordCredential Debug leaked hash: {dbg}"
    );

    // Input-level Debug must redact.
    let input_dbg = format!(
        "{:?}",
        CreatePasswordCredential {
            user_id: user.id,
            password_hash: PHC_SENTINEL_V1.to_owned(),
        }
    );
    assert!(
        !input_dbg.contains("DO-NOT-LEAK-HASH"),
        "CreatePasswordCredential Debug leaked hash: {input_dbg}"
    );
}

// ----------------------------------------------------------------------
// User sessions
// ----------------------------------------------------------------------

const TOKEN_HASH_SENTINEL_A: [u8; 32] = [0xAA; 32];
const TOKEN_HASH_SENTINEL_B: [u8; 32] = [0xBB; 32];

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());
    let now = Utc::now();
    let expires = now + Duration::days(30);

    let created = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: TOKEN_HASH_SENTINEL_A.to_vec(),
            expires_at: expires,
        })
        .await
        .unwrap();

    assert_eq!(created.user_id, user.id);
    assert_eq!(created.token_hash, TOKEN_HASH_SENTINEL_A);
    assert!(created.revoked_at.is_none());
    assert!(created.revoked_reason.is_none());
    // Postgres rounds to microseconds; allow a small delta.
    assert!((created.expires_at - expires).num_milliseconds().abs() < 5);

    // get_by_token_hash returns the same row.
    let by_hash = repo
        .get_by_token_hash(&TOKEN_HASH_SENTINEL_A)
        .await
        .unwrap()
        .expect("by-hash lookup returns row");
    assert_eq!(by_hash.id, created.id);
    assert_eq!(by_hash.token_hash, created.token_hash);

    // get by id round-trips.
    let by_id = repo.get(created.id).await.unwrap().unwrap();
    assert_eq!(by_id.id, created.id);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_get_by_unknown_token_hash_returns_none(pool: PgPool) {
    let repo = PgUserSessionRepository::new(pool.clone());
    let absent = repo.get_by_token_hash(&[0xCC; 32]).await.unwrap();
    assert!(absent.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_get_by_unknown_id_returns_none(pool: PgPool) {
    let repo = PgUserSessionRepository::new(pool.clone());
    let absent = repo.get(UserSessionId::new()).await.unwrap();
    assert!(absent.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_duplicate_token_hash_conflicts(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());
    let expires = Utc::now() + Duration::days(30);

    repo.create(CreateUserSession {
        user_id: user.id,
        token_hash: TOKEN_HASH_SENTINEL_A.to_vec(),
        expires_at: expires,
    })
    .await
    .unwrap();

    let err = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: TOKEN_HASH_SENTINEL_A.to_vec(),
            expires_at: expires,
        })
        .await
        .expect_err("duplicate token_hash must conflict");

    match err {
        RepositoryError::Conflict { entity, constraint } => {
            assert_eq!(entity, "user_session");
            // The unique index name in the migration.
            assert_eq!(constraint, "user_sessions_token_hash_key");
            // Constraint must NEVER echo the hash bytes — the entire
            // point of the redaction contract is that the digest is
            // unreachable through a public error.
            assert!(
                !constraint.contains("aa") && !constraint.contains("AA"),
                "constraint must not echo token_hash bytes: {constraint}"
            );
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_touch_last_seen_updates_timestamp(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());

    let session = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: TOKEN_HASH_SENTINEL_A.to_vec(),
            expires_at: Utc::now() + Duration::days(30),
        })
        .await
        .unwrap();

    let later = session.last_seen_at + Duration::seconds(5);
    repo.touch_last_seen(session.id, later).await.unwrap();

    let touched = repo.get(session.id).await.unwrap().unwrap();
    assert!(touched.last_seen_at >= later - Duration::milliseconds(5));
    assert!(touched.last_seen_at >= session.last_seen_at);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_touch_last_seen_unknown_id_returns_not_found(pool: PgPool) {
    let repo = PgUserSessionRepository::new(pool.clone());
    let err = repo
        .touch_last_seen(UserSessionId::new(), Utc::now())
        .await
        .expect_err("unknown id must surface NotFound");
    match err {
        RepositoryError::NotFound { entity } => assert_eq!(entity, "user_session"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_is_idempotent(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());

    let session = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: TOKEN_HASH_SENTINEL_A.to_vec(),
            expires_at: Utc::now() + Duration::days(30),
        })
        .await
        .unwrap();

    let first = Utc::now();
    repo.revoke(session.id, first, Some("logout"))
        .await
        .unwrap();

    let after_first = repo.get(session.id).await.unwrap().unwrap();
    assert!(after_first.revoked_at.is_some());
    assert_eq!(after_first.revoked_reason.as_deref(), Some("logout"));

    // Second revoke is a no-op: original revoked_at and reason are preserved.
    let later = first + Duration::seconds(60);
    repo.revoke(session.id, later, Some("admin_revoke"))
        .await
        .unwrap();

    let after_second = repo.get(session.id).await.unwrap().unwrap();
    assert_eq!(after_second.revoked_at, after_first.revoked_at);
    assert_eq!(after_second.revoked_reason, after_first.revoked_reason);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_unknown_id_returns_not_found(pool: PgPool) {
    let repo = PgUserSessionRepository::new(pool.clone());
    let err = repo
        .revoke(UserSessionId::new(), Utc::now(), None)
        .await
        .expect_err("unknown id must surface NotFound");
    match err {
        RepositoryError::NotFound { entity } => assert_eq!(entity, "user_session"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_all_for_user_only_touches_that_user(pool: PgPool) {
    let user_a = make_user(&pool).await;
    let user_b = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());
    let expires = Utc::now() + Duration::days(30);

    let a1 = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x01; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    let a2 = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x02; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    let b1 = repo
        .create(CreateUserSession {
            user_id: user_b.id,
            token_hash: vec![0x03; 32],
            expires_at: expires,
        })
        .await
        .unwrap();

    // Pre-revoke a2 so we can confirm idempotency: the second sweep
    // does NOT count it.
    repo.revoke(a2.id, Utc::now(), Some("logout"))
        .await
        .unwrap();

    let touched = repo
        .revoke_all_for_user(user_a.id, Utc::now(), Some("admin_revoke"))
        .await
        .unwrap();
    assert_eq!(
        touched, 1,
        "only the still-active session should transition"
    );

    // a1 is now revoked.
    let a1_after = repo.get(a1.id).await.unwrap().unwrap();
    assert!(a1_after.revoked_at.is_some());
    // a2's original logout timestamp is preserved (idempotency).
    let a2_after = repo.get(a2.id).await.unwrap().unwrap();
    assert_eq!(a2_after.revoked_reason.as_deref(), Some("logout"));
    // b1 (user B) is untouched.
    let b1_after = repo.get(b1.id).await.unwrap().unwrap();
    assert!(b1_after.revoked_at.is_none());
    assert!(b1_after.revoked_reason.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_all_for_unknown_user_returns_zero(pool: PgPool) {
    let repo = PgUserSessionRepository::new(pool.clone());
    let touched = repo
        .revoke_all_for_user(relayterm_core::ids::UserId::new(), Utc::now(), None)
        .await
        .unwrap();
    assert_eq!(touched, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_list_for_user_only_returns_owned_rows_newest_first(pool: PgPool) {
    let user_a = make_user(&pool).await;
    let user_b = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());
    let expires = Utc::now() + Duration::days(30);

    // Insert in deliberate order; the list ordering is `created_at
    // DESC` so the second row comes first.
    let a1 = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x11; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    // A small spin to make sure created_at is strictly later.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let a2 = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x12; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    let _b1 = repo
        .create(CreateUserSession {
            user_id: user_b.id,
            token_hash: vec![0x13; 32],
            expires_at: expires,
        })
        .await
        .unwrap();

    let listed = repo.list_for_user(user_a.id).await.unwrap();
    assert_eq!(listed.len(), 2, "list must include only user_a's rows");
    assert_eq!(listed[0].id, a2.id, "newest row first by created_at DESC");
    assert_eq!(listed[1].id, a1.id);
    // Cross-user redaction at the SQL boundary.
    assert!(listed.iter().all(|r| r.user_id == user_a.id));

    // Unknown user returns empty Vec, not NotFound.
    let empty = repo
        .list_for_user(relayterm_core::ids::UserId::new())
        .await
        .unwrap();
    assert!(empty.is_empty());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_for_user_owned_transitions_then_idempotent(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());

    let session = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: vec![0x21; 32],
            expires_at: Utc::now() + Duration::days(30),
        })
        .await
        .unwrap();

    // First call returns true (transition).
    let first = repo
        .revoke_for_user(user.id, session.id, Utc::now(), Some("user_revoke"))
        .await
        .unwrap();
    assert!(first, "first revoke_for_user must transition");

    let after_first = repo.get(session.id).await.unwrap().unwrap();
    let original_revoked_at = after_first.revoked_at;
    assert!(original_revoked_at.is_some());
    assert_eq!(after_first.revoked_reason.as_deref(), Some("user_revoke"));

    // Second call returns false (idempotent no-op) and preserves the
    // original revoked_at and reason.
    let later = Utc::now() + Duration::seconds(60);
    let second = repo
        .revoke_for_user(user.id, session.id, later, Some("admin_revoke"))
        .await
        .unwrap();
    assert!(!second, "second revoke_for_user must be a no-op");

    let after_second = repo.get(session.id).await.unwrap().unwrap();
    assert_eq!(after_second.revoked_at, original_revoked_at);
    assert_eq!(after_second.revoked_reason.as_deref(), Some("user_revoke"));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_for_user_foreign_user_collapses_to_not_found(pool: PgPool) {
    let user_a = make_user(&pool).await;
    let user_b = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());

    // Session belongs to user_a.
    let session = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x31; 32],
            expires_at: Utc::now() + Duration::days(30),
        })
        .await
        .unwrap();

    // user_b attempts to revoke user_a's session — must surface as
    // NotFound, not as a silent success and not as a typed
    // ownership-mismatch error. This is the probe-resistance contract.
    let err = repo
        .revoke_for_user(user_b.id, session.id, Utc::now(), Some("user_revoke"))
        .await
        .expect_err("foreign user must not revoke user_a's session");
    match err {
        RepositoryError::NotFound { entity } => assert_eq!(entity, "user_session"),
        other => panic!("expected NotFound, got {other:?}"),
    }

    // Row stays untouched.
    let row = repo.get(session.id).await.unwrap().unwrap();
    assert!(row.revoked_at.is_none());
    assert!(row.revoked_reason.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_for_user_unknown_id_returns_not_found(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());

    let err = repo
        .revoke_for_user(user.id, UserSessionId::new(), Utc::now(), None)
        .await
        .expect_err("unknown id must surface NotFound");
    match err {
        RepositoryError::NotFound { entity } => assert_eq!(entity, "user_session"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_all_except_only_touches_other_active_rows(pool: PgPool) {
    let user_a = make_user(&pool).await;
    let user_b = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());
    let expires = Utc::now() + Duration::days(30);

    // user_a has three sessions: current + two others. user_b has one,
    // which must remain untouched.
    let current = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x41; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    let other1 = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x42; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    let other2 = repo
        .create(CreateUserSession {
            user_id: user_a.id,
            token_hash: vec![0x43; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    let foreign = repo
        .create(CreateUserSession {
            user_id: user_b.id,
            token_hash: vec![0x44; 32],
            expires_at: expires,
        })
        .await
        .unwrap();

    // Pre-revoke other1 to confirm idempotency: the sweep does NOT
    // count an already-revoked row.
    repo.revoke(other1.id, Utc::now(), Some("logout"))
        .await
        .unwrap();

    let count = repo
        .revoke_all_except(user_a.id, current.id, Utc::now(), Some("user_revoke_all"))
        .await
        .unwrap();
    assert_eq!(count, 1, "only the still-active other row transitions");

    // current is untouched.
    let current_after = repo.get(current.id).await.unwrap().unwrap();
    assert!(current_after.revoked_at.is_none());
    assert!(current_after.revoked_reason.is_none());

    // other1 keeps its original logout timestamp/reason (idempotency).
    let other1_after = repo.get(other1.id).await.unwrap().unwrap();
    assert_eq!(other1_after.revoked_reason.as_deref(), Some("logout"));

    // other2 is now revoked with the new reason.
    let other2_after = repo.get(other2.id).await.unwrap().unwrap();
    assert!(other2_after.revoked_at.is_some());
    assert_eq!(
        other2_after.revoked_reason.as_deref(),
        Some("user_revoke_all"),
    );

    // Cross-user row untouched.
    let foreign_after = repo.get(foreign.id).await.unwrap().unwrap();
    assert!(foreign_after.revoked_at.is_none());
    assert!(foreign_after.revoked_reason.is_none());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_all_except_unknown_user_returns_zero(pool: PgPool) {
    let repo = PgUserSessionRepository::new(pool.clone());
    let count = repo
        .revoke_all_except(
            relayterm_core::ids::UserId::new(),
            UserSessionId::new(),
            Utc::now(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_revoke_all_except_does_not_touch_already_revoked_except(pool: PgPool) {
    // Even if the `except_id` row is itself already revoked, the
    // call must NOT toggle it. The revoke-all-except surface is for
    // "kill every OTHER session"; the except row is a passive marker.
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());
    let expires = Utc::now() + Duration::days(30);

    let except = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: vec![0x51; 32],
            expires_at: expires,
        })
        .await
        .unwrap();
    repo.revoke(except.id, Utc::now(), Some("logout"))
        .await
        .unwrap();
    let except_before = repo.get(except.id).await.unwrap().unwrap();

    let count = repo
        .revoke_all_except(user.id, except.id, Utc::now(), Some("user_revoke_all"))
        .await
        .unwrap();
    assert_eq!(count, 0);

    let except_after = repo.get(except.id).await.unwrap().unwrap();
    assert_eq!(except_after.revoked_at, except_before.revoked_at);
    assert_eq!(except_after.revoked_reason, except_before.revoked_reason);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn user_session_redaction_sentinels(pool: PgPool) {
    let user = make_user(&pool).await;
    let repo = PgUserSessionRepository::new(pool.clone());

    let session = repo
        .create(CreateUserSession {
            user_id: user.id,
            token_hash: TOKEN_HASH_SENTINEL_B.to_vec(),
            expires_at: Utc::now() + Duration::days(30),
        })
        .await
        .unwrap();

    // Domain-level Debug.
    let session_dbg = format!("{session:?}");
    // 0xBB byte rendered as Vec<u8> Debug would be "187"; check both
    // that string and the raw byte-pattern fragments.
    assert!(
        !session_dbg.contains("187, 187, 187"),
        "UserSession Debug leaked token_hash bytes: {session_dbg}"
    );

    // Input-level Debug.
    let input_dbg = format!(
        "{:?}",
        CreateUserSession {
            user_id: user.id,
            token_hash: TOKEN_HASH_SENTINEL_B.to_vec(),
            expires_at: session.expires_at,
        }
    );
    assert!(
        !input_dbg.contains("187, 187, 187"),
        "CreateUserSession Debug leaked token_hash bytes: {input_dbg}"
    );
}

// ----------------------------------------------------------------------
// TerminalRecording — chunks
// ----------------------------------------------------------------------

const RECORDING_CHUNK_PAYLOAD_SENTINEL: &[u8] = b"PTY-OUTPUT-SENTINEL-31C";

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let payload = b"\x1b[2J\x1b[H$ ls\r\nfoo bar baz\r\n".to_vec();
    let byte_len = payload.len() as i32;
    let created = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: 1,
            seq_end: 4,
            byte_len,
            payload: payload.clone(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect("append chunk");

    assert_eq!(created.terminal_session_id, session.id);
    assert_eq!(created.seq_start, 1);
    assert_eq!(created.seq_end, 4);
    assert_eq!(created.byte_len, byte_len);
    // Bytes round-trip exactly through the domain field — repository is
    // the only path that surfaces them.
    assert_eq!(created.payload, payload);
    assert_eq!(created.encryption, TerminalRecordingPayloadEncryption::None,);
    assert_eq!(created.compression, TerminalRecordingCompression::None);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunks_list_ordered_by_seq_start(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    // Insert in non-monotonic order; list MUST come back ordered.
    for &(seq_start, seq_end) in &[(50_i64, 60_i64), (1, 10), (200, 210), (100, 110)] {
        repo.append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start,
            seq_end,
            byte_len: 4,
            payload: b"data".to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .unwrap();
    }

    let listed = repo.list_chunks(session.id, 1, 100).await.unwrap();
    assert_eq!(listed.len(), 4);
    let starts: Vec<i64> = listed.iter().map(|c| c.seq_start).collect();
    assert_eq!(starts, vec![1, 50, 100, 200]);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunks_list_filters_from_seq(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    for start in [1_i64, 100, 200, 300] {
        repo.append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: start,
            seq_end: start + 9,
            byte_len: 4,
            payload: b"data".to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .unwrap();
    }

    // from_seq=150 should exclude (1, 100) and include (200, 300).
    let listed = repo.list_chunks(session.id, 150, 100).await.unwrap();
    let starts: Vec<i64> = listed.iter().map(|c| c.seq_start).collect();
    assert_eq!(starts, vec![200, 300]);

    // limit=1 still returns the smallest matching.
    let limited = repo.list_chunks(session.id, 150, 1).await.unwrap();
    let starts_lim: Vec<i64> = limited.iter().map(|c| c.seq_start).collect();
    assert_eq!(starts_lim, vec![200]);

    // Unknown session id returns empty, never errors.
    let bogus = relayterm_core::ids::TerminalSessionId::new();
    let empty = repo.list_chunks(bogus, 1, 100).await.unwrap();
    assert!(empty.is_empty());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_duplicate_seq_start_is_conflict(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    repo.append_chunk(CreateTerminalRecordingChunk {
        terminal_session_id: session.id,
        seq_start: 5,
        seq_end: 10,
        byte_len: 4,
        payload: b"data".to_vec(),
        encryption: TerminalRecordingPayloadEncryption::None,
        compression: TerminalRecordingCompression::None,
    })
    .await
    .unwrap();

    let err = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: 5,
            seq_end: 12,
            byte_len: 4,
            payload: b"data".to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect_err("duplicate seq_start must conflict");
    match err {
        RepositoryError::Conflict { entity, constraint } => {
            assert_eq!(entity, "terminal_recording_chunk");
            assert!(
                constraint.contains("session_seq_start"),
                "unexpected constraint name: {constraint}"
            );
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_invalid_seq_start_rejected(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let err = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: 0,
            seq_end: 0,
            byte_len: RECORDING_CHUNK_PAYLOAD_SENTINEL.len() as i32,
            payload: RECORDING_CHUNK_PAYLOAD_SENTINEL.to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect_err("seq_start=0 must violate CHECK");
    assert!(matches!(err, RepositoryError::Database(_)));
    let err_str = err.to_string();
    assert!(
        !err_str.contains("PTY-OUTPUT-SENTINEL"),
        "constraint error must not echo payload bytes: {err_str}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_byte_len_zero_rejected(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let err = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: 1,
            seq_end: 1,
            byte_len: 0,
            payload: Vec::new(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect_err("byte_len=0 must violate CHECK");
    assert!(matches!(err, RepositoryError::Database(_)));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_byte_len_payload_mismatch_rejected(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    // payload is 4 bytes but byte_len declares 5 — schema CHECK pins this.
    let err = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: 1,
            seq_end: 1,
            byte_len: 5,
            payload: b"data".to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect_err("byte_len/payload mismatch must violate CHECK");
    assert!(matches!(err, RepositoryError::Database(_)));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_unknown_session_fk_violation(pool: PgPool) {
    let bogus = relayterm_core::ids::TerminalSessionId::new();
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let err = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: bogus,
            seq_start: 1,
            seq_end: 1,
            byte_len: RECORDING_CHUNK_PAYLOAD_SENTINEL.len() as i32,
            payload: RECORDING_CHUNK_PAYLOAD_SENTINEL.to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect_err("unknown session_id must FK-fail");
    assert!(matches!(err, RepositoryError::Database(_)));
    let err_str = err.to_string();
    assert!(
        !err_str.contains("PTY-OUTPUT-SENTINEL"),
        "FK error must not echo payload bytes: {err_str}",
    );
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_chunk_payload_not_in_error_or_debug(pool: PgPool) {
    // The bytes are reachable ONLY via the parsed domain field. They must
    // never appear in repository errors, in `Debug` formatting of the
    // domain struct, or in `Debug` formatting of the input struct.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let payload = RECORDING_CHUNK_PAYLOAD_SENTINEL.to_vec();
    let chunk = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start: 1,
            seq_end: 1,
            byte_len: payload.len() as i32,
            payload: payload.clone(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .unwrap();
    assert_eq!(chunk.payload, payload);

    let dbg = format!("{chunk:?}");
    assert!(
        !dbg.contains("PTY-OUTPUT-SENTINEL-31C"),
        "TerminalRecordingChunk Debug leaked payload sentinel: {dbg}",
    );

    // A failed insert (FK violation here, since the session_id is bogus)
    // must NOT echo the bytes back through the error.
    let bogus = relayterm_core::ids::TerminalSessionId::new();
    let err = repo
        .append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: bogus,
            seq_start: 1,
            seq_end: 1,
            byte_len: payload.len() as i32,
            payload: payload.clone(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .expect_err("FK violation must error");
    let err_str = err.to_string();
    let err_dbg = format!("{err:?}");
    assert!(
        !err_str.contains("PTY-OUTPUT-SENTINEL-31C"),
        "RepositoryError Display leaked payload sentinel: {err_str}",
    );
    assert!(
        !err_dbg.contains("PTY-OUTPUT-SENTINEL-31C"),
        "RepositoryError Debug leaked payload sentinel: {err_dbg}",
    );
}

// ----------------------------------------------------------------------
// TerminalRecording — markers
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_marker_round_trip(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let created = repo
        .append_marker(CreateTerminalRecordingMarker {
            terminal_session_id: session.id,
            kind: TerminalRecordingMarkerKind::Resized,
            seq: 17,
            payload: json!({ "cols": 132, "rows": 40 }),
        })
        .await
        .unwrap();

    assert_eq!(created.kind, TerminalRecordingMarkerKind::Resized);
    assert_eq!(created.seq, 17);
    assert_eq!(created.payload, json!({ "cols": 132, "rows": 40 }));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_marker_started_allows_seq_zero(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let started = repo
        .append_marker(CreateTerminalRecordingMarker {
            terminal_session_id: session.id,
            kind: TerminalRecordingMarkerKind::Started,
            seq: 0,
            payload: json!({}),
        })
        .await
        .expect("started must allow seq=0");
    assert_eq!(started.kind, TerminalRecordingMarkerKind::Started);
    assert_eq!(started.seq, 0);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_marker_seq_zero_rejected_for_other_kinds(pool: PgPool) {
    // The schema CHECK only allows seq=0 for the 'started' kind. Pin the
    // rejection for every other kind in one test so a future migration
    // that loosens the check surfaces here.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    for kind in [
        TerminalRecordingMarkerKind::Attached,
        TerminalRecordingMarkerKind::Detached,
        TerminalRecordingMarkerKind::Reattached,
        TerminalRecordingMarkerKind::Resized,
        TerminalRecordingMarkerKind::Closed,
        TerminalRecordingMarkerKind::ReplayGap,
    ] {
        let err = repo
            .append_marker(CreateTerminalRecordingMarker {
                terminal_session_id: session.id,
                kind,
                seq: 0,
                payload: json!({}),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, RepositoryError::Database(_)),
            "expected Database error for {kind:?} at seq=0, got {err:?}"
        );
    }
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_marker_seq_zero_rejected_resized(pool: PgPool) {
    // Focused expect_err on the single most-likely real-world misuse, in
    // case the multi-kind variant above changes shape.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let err = repo
        .append_marker(CreateTerminalRecordingMarker {
            terminal_session_id: session.id,
            kind: TerminalRecordingMarkerKind::Resized,
            seq: 0,
            payload: json!({ "cols": 80, "rows": 24 }),
        })
        .await
        .expect_err("seq=0 must violate CHECK for Resized");
    assert!(matches!(err, RepositoryError::Database(_)));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_markers_list_ordered_and_filtered(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    repo.append_marker(CreateTerminalRecordingMarker {
        terminal_session_id: session.id,
        kind: TerminalRecordingMarkerKind::Started,
        seq: 0,
        payload: json!({}),
    })
    .await
    .unwrap();
    for (kind, seq) in [
        (TerminalRecordingMarkerKind::Attached, 1_i64),
        (TerminalRecordingMarkerKind::Resized, 17),
        (TerminalRecordingMarkerKind::Detached, 200),
        (TerminalRecordingMarkerKind::Closed, 500),
    ] {
        repo.append_marker(CreateTerminalRecordingMarker {
            terminal_session_id: session.id,
            kind,
            seq,
            payload: json!({}),
        })
        .await
        .unwrap();
    }

    let listed = repo.list_markers(session.id, 0, 100).await.unwrap();
    let seqs: Vec<i64> = listed.iter().map(|m| m.seq).collect();
    assert_eq!(seqs, vec![0, 1, 17, 200, 500]);

    let filtered = repo.list_markers(session.id, 18, 100).await.unwrap();
    let seqs_f: Vec<i64> = filtered.iter().map(|m| m.seq).collect();
    assert_eq!(seqs_f, vec![200, 500]);
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_marker_unknown_session_fk_violation(pool: PgPool) {
    let bogus = relayterm_core::ids::TerminalSessionId::new();
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let err = repo
        .append_marker(CreateTerminalRecordingMarker {
            terminal_session_id: bogus,
            kind: TerminalRecordingMarkerKind::Started,
            seq: 0,
            payload: json!({}),
        })
        .await
        .expect_err("unknown session_id must FK-fail");
    assert!(matches!(err, RepositoryError::Database(_)));
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_repository_is_session_scoped_only(pool: PgPool) {
    // The repository surface is session-scoped, NOT owner-scoped: foreign
    // ownership is entirely the route layer's job. This test pins that
    // contract by verifying that two users' sessions are isolated by
    // `terminal_session_id` alone, with no `owner_id` filter inside the
    // repository.
    let alice = make_user(&pool).await;
    let bob = make_user(&pool).await;
    let host_a = make_host(&pool, &alice).await;
    let identity_a = make_identity(&pool, &alice).await;
    let profile_a = make_profile(&pool, &alice, &host_a, &identity_a).await;
    let host_b = make_host(&pool, &bob).await;
    let identity_b = make_identity(&pool, &bob).await;
    let profile_b = make_profile(&pool, &bob, &host_b, &identity_b).await;

    let session_a = make_terminal_session(&pool, &alice, &profile_a).await;
    let session_b = make_terminal_session(&pool, &bob, &profile_b).await;

    let repo = PgTerminalRecordingRepository::new(pool.clone());
    repo.append_chunk(CreateTerminalRecordingChunk {
        terminal_session_id: session_a.id,
        seq_start: 1,
        seq_end: 1,
        byte_len: 4,
        payload: b"data".to_vec(),
        encryption: TerminalRecordingPayloadEncryption::None,
        compression: TerminalRecordingCompression::None,
    })
    .await
    .unwrap();
    repo.append_chunk(CreateTerminalRecordingChunk {
        terminal_session_id: session_b.id,
        seq_start: 1,
        seq_end: 1,
        byte_len: 4,
        payload: b"data".to_vec(),
        encryption: TerminalRecordingPayloadEncryption::None,
        compression: TerminalRecordingCompression::None,
    })
    .await
    .unwrap();

    let alices = repo.list_chunks(session_a.id, 1, 100).await.unwrap();
    let bobs = repo.list_chunks(session_b.id, 1, 100).await.unwrap();
    assert_eq!(alices.len(), 1);
    assert_eq!(bobs.len(), 1);
    assert_eq!(alices[0].terminal_session_id, session_a.id);
    assert_eq!(bobs[0].terminal_session_id, session_b.id);
}

// ----------------------------------------------------------------------
// TerminalRecording — metadata
// ----------------------------------------------------------------------

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_empty_for_session_with_no_rows(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    let meta = repo.get_metadata(session.id).await.unwrap();
    assert_eq!(meta.terminal_session_id, session.id);
    assert_eq!(meta.chunk_count, 0);
    assert_eq!(meta.marker_count, 0);
    assert_eq!(meta.first_seq, None);
    assert_eq!(meta.last_seq, None);
    assert!(meta.first_recorded_at.is_none());
    assert!(meta.last_recorded_at.is_none());
    assert!(!meta.has_recording());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_unknown_session_returns_empty(pool: PgPool) {
    // Repository surface is session-scoped; an unknown id surfaces as the
    // empty metadata shape (route layer is responsible for owner scoping
    // and 404). This pins that the aggregate query never errors when the
    // session row is missing.
    let bogus = relayterm_core::ids::TerminalSessionId::new();
    let repo = PgTerminalRecordingRepository::new(pool.clone());
    let meta = repo.get_metadata(bogus).await.unwrap();
    assert_eq!(meta.chunk_count, 0);
    assert_eq!(meta.marker_count, 0);
    assert!(!meta.has_recording());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_aggregates_chunks_and_markers(pool: PgPool) {
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    repo.append_marker(CreateTerminalRecordingMarker {
        terminal_session_id: session.id,
        kind: TerminalRecordingMarkerKind::Started,
        seq: 0,
        payload: json!({}),
    })
    .await
    .unwrap();
    for &(seq_start, seq_end) in &[(1_i64, 10_i64), (50, 60), (200, 250)] {
        repo.append_chunk(CreateTerminalRecordingChunk {
            terminal_session_id: session.id,
            seq_start,
            seq_end,
            byte_len: 4,
            payload: b"data".to_vec(),
            encryption: TerminalRecordingPayloadEncryption::None,
            compression: TerminalRecordingCompression::None,
        })
        .await
        .unwrap();
    }
    repo.append_marker(CreateTerminalRecordingMarker {
        terminal_session_id: session.id,
        kind: TerminalRecordingMarkerKind::Resized,
        seq: 17,
        payload: json!({ "cols": 132, "rows": 40 }),
    })
    .await
    .unwrap();

    let meta = repo.get_metadata(session.id).await.unwrap();
    assert_eq!(meta.chunk_count, 3);
    assert_eq!(meta.marker_count, 2);
    assert_eq!(meta.first_seq, Some(1));
    assert_eq!(meta.last_seq, Some(250));
    let first = meta.first_recorded_at.expect("first_recorded_at");
    let last = meta.last_recorded_at.expect("last_recorded_at");
    assert!(first <= last, "first must be <= last");
    assert!(meta.has_recording());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_session_with_only_markers(pool: PgPool) {
    // A session that has a `started` marker but no chunks yet still
    // counts as `has_recording = true`. The seq bounds remain `None`
    // because they are derived from chunks only.
    let user = make_user(&pool).await;
    let host = make_host(&pool, &user).await;
    let identity = make_identity(&pool, &user).await;
    let profile = make_profile(&pool, &user, &host, &identity).await;
    let session = make_terminal_session(&pool, &user, &profile).await;
    let repo = PgTerminalRecordingRepository::new(pool.clone());

    repo.append_marker(CreateTerminalRecordingMarker {
        terminal_session_id: session.id,
        kind: TerminalRecordingMarkerKind::Started,
        seq: 0,
        payload: json!({}),
    })
    .await
    .unwrap();

    let meta = repo.get_metadata(session.id).await.unwrap();
    assert_eq!(meta.chunk_count, 0);
    assert_eq!(meta.marker_count, 1);
    assert_eq!(meta.first_seq, None);
    assert_eq!(meta.last_seq, None);
    assert!(meta.has_recording());
    assert!(meta.first_recorded_at.is_some());
    assert!(meta.last_recorded_at.is_some());
}

#[sqlx::test(migrations = "../../apps/backend/migrations")]
async fn recording_metadata_isolates_per_session(pool: PgPool) {
    let alice = make_user(&pool).await;
    let bob = make_user(&pool).await;
    let host_a = make_host(&pool, &alice).await;
    let identity_a = make_identity(&pool, &alice).await;
    let profile_a = make_profile(&pool, &alice, &host_a, &identity_a).await;
    let host_b = make_host(&pool, &bob).await;
    let identity_b = make_identity(&pool, &bob).await;
    let profile_b = make_profile(&pool, &bob, &host_b, &identity_b).await;
    let session_a = make_terminal_session(&pool, &alice, &profile_a).await;
    let session_b = make_terminal_session(&pool, &bob, &profile_b).await;

    let repo = PgTerminalRecordingRepository::new(pool.clone());
    repo.append_chunk(CreateTerminalRecordingChunk {
        terminal_session_id: session_a.id,
        seq_start: 1,
        seq_end: 5,
        byte_len: 4,
        payload: b"data".to_vec(),
        encryption: TerminalRecordingPayloadEncryption::None,
        compression: TerminalRecordingCompression::None,
    })
    .await
    .unwrap();

    let meta_a = repo.get_metadata(session_a.id).await.unwrap();
    let meta_b = repo.get_metadata(session_b.id).await.unwrap();
    assert_eq!(meta_a.chunk_count, 1);
    assert_eq!(meta_b.chunk_count, 0);
}
