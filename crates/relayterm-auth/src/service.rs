//! [`AuthService`] composes the password and session primitives over
//! the core repository traits.
//!
//! The service is the single point that knows about Argon2id
//! parameters, session-token hashing, and session-policy decisions
//! (revoked / expired / active). Routes call into this module without
//! learning the crypto details. There is no HTTP, cookie, or
//! extractor surface here — those land in a later slice.
//!
//! ## Time handling
//!
//! All methods that read or write a timestamp take `now: DateTime<Utc>`
//! explicitly. A clock trait would buy us the same testability with
//! more rope; passing `now` keeps the surface ten lines smaller and
//! keeps the test fixtures literal.
//!
//! ## Error posture (load-bearing)
//!
//! [`AuthServiceError`] is structural — every variant maps to one of
//! the wire codes the route layer will emit (401, 401, 401, 500,
//! 500). Variants do NOT carry plaintext passwords, hashes, tokens,
//! token digests, or repository SQL. The `Repository` and `Crypto`
//! variants wrap the upstream message for operator logs only; the
//! route layer logs at `warn!` and returns a static body to the
//! client. SPEC.md "Production authentication architecture" pins
//! this redaction contract.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use relayterm_core::ids::{UserId, UserSessionId};
use relayterm_core::repository::{
    CreatePasswordCredential, CreateUserSession, PasswordCredentialRepository, RepositoryError,
    UserSessionRepository,
};
use relayterm_core::user_session::UserSession;

use crate::password::{PasswordHasher, PasswordHashingError};
use crate::session_token::{SessionToken, hash_session_token};

/// Errors a service call may surface.
///
/// `Display` carries only the structural category — never plaintext
/// password, never stored hash, never plaintext or hashed session
/// token.
#[derive(Debug, thiserror::Error)]
pub enum AuthServiceError {
    /// No password row for the user, OR the offered password did
    /// not verify against the stored Argon2id hash. The two cases
    /// are deliberately collapsed: a probe must not be able to
    /// distinguish "user has no password set" from "wrong password".
    #[error("invalid credentials")]
    InvalidCredentials,

    /// The supplied session token does not match any persisted row.
    /// Could be a never-issued token, a revoked-then-pruned token,
    /// or a bit-flipped cookie. Always 401 on the wire.
    #[error("session invalid")]
    SessionInvalid,

    /// The session row exists but is past `expires_at`. Always 401.
    #[error("session expired")]
    SessionExpired,

    /// The session row exists but `revoked_at IS NOT NULL`. Always
    /// 401. A separate variant from `SessionExpired` so internal
    /// logs can distinguish the two; the wire response collapses
    /// both to the same 401 body.
    #[error("session revoked")]
    SessionRevoked,

    /// Repository / database failure. The wrapped string is intended
    /// for operator logs and never echoes secrets per the repository
    /// crate's contract.
    #[error("repository error: {0}")]
    Repository(String),

    /// Cryptographic primitive failure (hash production / hash
    /// verification of a stored row that turned out to be
    /// non-PHC-shaped). `Display` does not include the wrapped
    /// detail — `PasswordHashingError`'s own `Display` is already
    /// redaction-safe but keeping the public string fixed makes the
    /// audit-substring tests in the route slice trivial.
    #[error("crypto failure")]
    Crypto,
}

impl From<RepositoryError> for AuthServiceError {
    fn from(err: RepositoryError) -> Self {
        Self::Repository(err.to_string())
    }
}

impl From<PasswordHashingError> for AuthServiceError {
    fn from(_: PasswordHashingError) -> Self {
        Self::Crypto
    }
}

/// One-shot return shape for a freshly minted session.
///
/// `token` is the plaintext to return to the client (for the
/// `Set-Cookie` header) — it is NOT persisted. `session` is the
/// just-inserted row, returned so the route can immediately stamp an
/// `audit_events` row referring to its `id`.
///
/// `Debug` redacts via the inner types' impls — the wrapper itself
/// derives `Debug` deliberately so a panic message including this
/// type renders the redacted shape instead of the raw fields.
#[derive(Debug)]
pub struct CreatedSession {
    pub token: SessionToken,
    pub session: UserSession,
}

/// Auth service.
///
/// Constructed once at boot and shared via the request-state
/// machinery. Holds the repository handles behind `Arc<dyn ...>` so
/// tests can swap in fakes without re-wiring the route layer.
#[derive(Clone)]
pub struct AuthService {
    passwords: Arc<dyn PasswordCredentialRepository>,
    sessions: Arc<dyn UserSessionRepository>,
    hasher: PasswordHasher,
}

impl AuthService {
    /// Build a service from concrete repositories and a configured
    /// password hasher.
    #[must_use]
    pub fn new(
        passwords: Arc<dyn PasswordCredentialRepository>,
        sessions: Arc<dyn UserSessionRepository>,
        hasher: PasswordHasher,
    ) -> Self {
        Self {
            passwords,
            sessions,
            hasher,
        }
    }

    /// Hash and store a password for an existing user.
    ///
    /// Caller is responsible for ensuring the `users.id` row exists;
    /// a foreign-key violation surfaces as
    /// [`AuthServiceError::Repository`]. The stored hash is an
    /// Argon2id PHC string with a per-call random salt — calling
    /// `set_password` with the same plaintext twice produces two
    /// different hashes.
    pub async fn set_password(
        &self,
        user_id: UserId,
        plaintext: &str,
    ) -> Result<(), AuthServiceError> {
        let hash = self.hasher.hash_password(plaintext)?;
        self.passwords
            .upsert_for_user(CreatePasswordCredential {
                user_id,
                password_hash: hash,
            })
            .await?;
        Ok(())
    }

    /// Verify an offered password against the stored hash.
    ///
    /// Returns `Ok(())` on a successful verify and
    /// [`AuthServiceError::InvalidCredentials`] otherwise. Specific
    /// failure shapes ("no row", "wrong password", "stored row was
    /// corrupt") are collapsed at this boundary so a future wire
    /// response cannot accidentally leak the distinction.
    pub async fn verify_password(
        &self,
        user_id: UserId,
        plaintext: &str,
    ) -> Result<(), AuthServiceError> {
        let row = self.passwords.get_for_user(user_id).await?;
        let Some(credential) = row else {
            return Err(AuthServiceError::InvalidCredentials);
        };
        let ok = self
            .hasher
            .verify_password(plaintext, &credential.password_hash)
            // A corrupt PHC string in the row is treated the same as
            // a wrong-password verify — never surface the corrupt-row
            // signal to the caller.
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(AuthServiceError::InvalidCredentials)
        }
    }

    /// Mint a fresh session for a user.
    ///
    /// Generates a 32-byte random token via `OsRng`, persists ONLY
    /// its SHA-256 digest, and returns the plaintext token to the
    /// caller exactly once. `expires_at` is `now + ttl`.
    pub async fn create_session(
        &self,
        user_id: UserId,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<CreatedSession, AuthServiceError> {
        let token = SessionToken::generate();
        let token_hash = token.hash();
        let session = self
            .sessions
            .create(CreateUserSession {
                user_id,
                token_hash: token_hash.into_bytes(),
                expires_at: now + ttl,
            })
            .await?;
        Ok(CreatedSession { token, session })
    }

    /// Validate a plaintext session token.
    ///
    /// Returns the matching [`UserSession`] row when the token is
    /// known AND not expired AND not revoked. Otherwise returns the
    /// most informative variant: `SessionInvalid` (no row),
    /// `SessionExpired`, `SessionRevoked`. The route layer collapses
    /// all three to a single 401 response.
    ///
    /// `last_seen_at` is **not** touched here. Stamping it is the
    /// extractor's responsibility (best-effort, error-tolerant —
    /// SPEC.md "Auth extractor and route migration"). Keeping the
    /// service free of that side effect lets a route that wants a
    /// pure read (e.g. a future `/api/v1/auth/me`) call this without
    /// also writing.
    pub async fn validate_session_token(
        &self,
        plaintext_token: &str,
        now: DateTime<Utc>,
    ) -> Result<UserSession, AuthServiceError> {
        let digest = hash_session_token(plaintext_token);
        let row = self.sessions.get_by_token_hash(digest.as_bytes()).await?;
        let Some(session) = row else {
            return Err(AuthServiceError::SessionInvalid);
        };
        if session.is_revoked() {
            return Err(AuthServiceError::SessionRevoked);
        }
        if session.is_expired_at(now) {
            return Err(AuthServiceError::SessionExpired);
        }
        Ok(session)
    }

    /// Revoke a single session by primary key.
    ///
    /// Idempotent (the repository keeps the original `revoked_at`
    /// and `revoked_reason` on a redundant call). Returns
    /// [`AuthServiceError::SessionInvalid`] if the row does not
    /// exist — surfacing `NotFound` to the caller would let a probe
    /// distinguish "your session id is unknown" from "your session
    /// was already revoked", which the route layer must not allow.
    pub async fn revoke_session(
        &self,
        id: UserSessionId,
        now: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<(), AuthServiceError> {
        match self.sessions.revoke(id, now, reason).await {
            Ok(()) => Ok(()),
            Err(RepositoryError::NotFound { .. }) => Err(AuthServiceError::SessionInvalid),
            Err(other) => Err(other.into()),
        }
    }

    /// Revoke every non-revoked session for a user.
    ///
    /// Returns the number of rows transitioned from non-revoked to
    /// revoked. An unknown `user_id` returns `0` — that is the
    /// repository contract and matches the "log out everywhere"
    /// route's expected behavior on a no-op.
    pub async fn revoke_all_for_user(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<u64, AuthServiceError> {
        Ok(self
            .sessions
            .revoke_all_for_user(user_id, now, reason)
            .await?)
    }
}

/// Manual `Debug` so the password hasher's parameters never get
/// re-introduced through the service handle. The repository handles
/// already redact themselves via the `Debug` impls on the row
/// wrappers; we just elide everything here for symmetry.
impl fmt::Debug for AuthService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthService")
            .field("passwords", &"<repository>")
            .field("sessions", &"<repository>")
            .field("hasher", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use relayterm_core::password_credential::PasswordCredential;
    use relayterm_core::repository::{
        CreatePasswordCredential, CreateUserSession, PasswordCredentialRepository,
        UserSessionRepository,
    };
    use std::sync::Mutex;

    // --- In-memory fakes -----------------------------------------

    #[derive(Default)]
    struct FakePasswordRepo {
        rows: Mutex<Vec<PasswordCredential>>,
    }

    #[async_trait]
    impl PasswordCredentialRepository for FakePasswordRepo {
        async fn upsert_for_user(
            &self,
            input: CreatePasswordCredential,
        ) -> Result<PasswordCredential, RepositoryError> {
            let now = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
            let mut rows = self.rows.lock().expect("rows lock");
            if let Some(existing) = rows.iter_mut().find(|r| r.user_id == input.user_id) {
                existing.password_hash = input.password_hash;
                existing.password_changed_at = now;
                existing.updated_at = now;
                return Ok(existing.clone());
            }
            let row = PasswordCredential {
                user_id: input.user_id,
                password_hash: input.password_hash,
                password_changed_at: now,
                created_at: now,
                updated_at: now,
            };
            rows.push(row.clone());
            Ok(row)
        }

        async fn get_for_user(
            &self,
            user_id: UserId,
        ) -> Result<Option<PasswordCredential>, RepositoryError> {
            let rows = self.rows.lock().expect("rows lock");
            Ok(rows.iter().find(|r| r.user_id == user_id).cloned())
        }
    }

    #[derive(Default)]
    struct FakeSessionRepo {
        rows: Mutex<Vec<UserSession>>,
    }

    #[async_trait]
    impl UserSessionRepository for FakeSessionRepo {
        async fn create(&self, input: CreateUserSession) -> Result<UserSession, RepositoryError> {
            let now = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
            let mut rows = self.rows.lock().expect("rows lock");
            if rows.iter().any(|r| r.token_hash == input.token_hash) {
                return Err(RepositoryError::Conflict {
                    entity: "user_session",
                    constraint: "user_sessions_token_hash_key".to_owned(),
                });
            }
            let row = UserSession {
                id: UserSessionId::new(),
                user_id: input.user_id,
                token_hash: input.token_hash,
                created_at: now,
                last_seen_at: now,
                expires_at: input.expires_at,
                revoked_at: None,
                revoked_reason: None,
            };
            rows.push(row.clone());
            Ok(row)
        }

        async fn get_by_token_hash(
            &self,
            token_hash: &[u8],
        ) -> Result<Option<UserSession>, RepositoryError> {
            let rows = self.rows.lock().expect("rows lock");
            Ok(rows.iter().find(|r| r.token_hash == token_hash).cloned())
        }

        async fn get(&self, id: UserSessionId) -> Result<Option<UserSession>, RepositoryError> {
            let rows = self.rows.lock().expect("rows lock");
            Ok(rows.iter().find(|r| r.id == id).cloned())
        }

        async fn touch_last_seen(
            &self,
            id: UserSessionId,
            at: DateTime<Utc>,
        ) -> Result<(), RepositoryError> {
            let mut rows = self.rows.lock().expect("rows lock");
            let Some(row) = rows.iter_mut().find(|r| r.id == id) else {
                return Err(RepositoryError::NotFound {
                    entity: "user_session",
                });
            };
            row.last_seen_at = at;
            Ok(())
        }

        async fn revoke(
            &self,
            id: UserSessionId,
            at: DateTime<Utc>,
            reason: Option<&str>,
        ) -> Result<(), RepositoryError> {
            let mut rows = self.rows.lock().expect("rows lock");
            let Some(row) = rows.iter_mut().find(|r| r.id == id) else {
                return Err(RepositoryError::NotFound {
                    entity: "user_session",
                });
            };
            // Idempotent: preserve original revocation metadata.
            if row.revoked_at.is_none() {
                row.revoked_at = Some(at);
                row.revoked_reason = reason.map(str::to_owned);
            }
            Ok(())
        }

        async fn revoke_all_for_user(
            &self,
            user_id: UserId,
            at: DateTime<Utc>,
            reason: Option<&str>,
        ) -> Result<u64, RepositoryError> {
            let mut rows = self.rows.lock().expect("rows lock");
            let mut count = 0_u64;
            for row in rows.iter_mut() {
                if row.user_id == user_id && row.revoked_at.is_none() {
                    row.revoked_at = Some(at);
                    row.revoked_reason = reason.map(str::to_owned);
                    count += 1;
                }
            }
            Ok(count)
        }
    }

    fn fast_service() -> (AuthService, Arc<FakePasswordRepo>, Arc<FakeSessionRepo>) {
        let passwords: Arc<FakePasswordRepo> = Arc::new(FakePasswordRepo::default());
        let sessions: Arc<FakeSessionRepo> = Arc::new(FakeSessionRepo::default());
        // Tuned-down hasher for unit-test speed. Production uses
        // OWASP_2023; the tests under `password.rs` pin the default.
        let hasher = PasswordHasher::new(crate::password::PasswordHasherConfig {
            m_cost: 4_096,
            t_cost: 1,
            p_cost: 1,
        })
        .expect("fast test params are valid");
        let service = AuthService::new(
            passwords.clone() as Arc<dyn PasswordCredentialRepository>,
            sessions.clone() as Arc<dyn UserSessionRepository>,
            hasher,
        );
        (service, passwords, sessions)
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap()
    }

    // --- Password lifecycle --------------------------------------

    #[tokio::test]
    async fn set_password_stores_redacted_credential() {
        let (service, passwords, _) = fast_service();
        let user_id = UserId::new();
        service
            .set_password(user_id, "correct-horse-battery-staple")
            .await
            .expect("set_password");

        let stored = passwords
            .get_for_user(user_id)
            .await
            .expect("get_for_user")
            .expect("row present");
        assert!(
            stored.password_hash.starts_with("$argon2id$"),
            "expected argon2id PHC prefix"
        );
        assert!(
            !stored
                .password_hash
                .contains("correct-horse-battery-staple"),
            "stored hash must not embed plaintext"
        );

        // The repository's wrappers redact in Debug — re-checked here
        // because the service is the only legitimate writer.
        let dbg = format!("{stored:?}");
        assert!(
            !dbg.contains("$argon2id$"),
            "PasswordCredential Debug must not echo PHC bytes"
        );
        assert!(
            !dbg.contains("correct-horse-battery-staple"),
            "PasswordCredential Debug must not echo plaintext"
        );
    }

    #[tokio::test]
    async fn verify_password_succeeds_for_correct_input() {
        let (service, _, _) = fast_service();
        let user_id = UserId::new();
        service.set_password(user_id, "hunter2").await.expect("set");
        service
            .verify_password(user_id, "hunter2")
            .await
            .expect("verify ok");
    }

    #[tokio::test]
    async fn verify_password_fails_for_wrong_input() {
        let (service, _, _) = fast_service();
        let user_id = UserId::new();
        service.set_password(user_id, "hunter2").await.expect("set");
        let err = service
            .verify_password(user_id, "wrong")
            .await
            .expect_err("verify should fail");
        assert!(matches!(err, AuthServiceError::InvalidCredentials));
    }

    #[tokio::test]
    async fn verify_password_fails_for_unknown_user() {
        let (service, _, _) = fast_service();
        let err = service
            .verify_password(UserId::new(), "anything")
            .await
            .expect_err("verify should fail");
        assert!(matches!(err, AuthServiceError::InvalidCredentials));
    }

    #[tokio::test]
    async fn verify_password_collapses_corrupt_stored_hash() {
        // Inject a row whose `password_hash` is not a PHC string —
        // the service must surface this as InvalidCredentials, not
        // as a typed "your row is corrupt" error.
        let (service, passwords, _) = fast_service();
        let user_id = UserId::new();
        let now = fixed_now();
        passwords.rows.lock().unwrap().push(PasswordCredential {
            user_id,
            password_hash: "definitely-not-a-phc-string".to_owned(),
            password_changed_at: now,
            created_at: now,
            updated_at: now,
        });
        let err = service
            .verify_password(user_id, "anything")
            .await
            .expect_err("verify should fail");
        assert!(matches!(err, AuthServiceError::InvalidCredentials));
    }

    // --- Session lifecycle ---------------------------------------

    #[tokio::test]
    async fn create_session_stores_only_hash_and_returns_plaintext() {
        let (service, _, sessions) = fast_service();
        let user_id = UserId::new();
        let now = fixed_now();
        let created = service
            .create_session(user_id, Duration::days(30), now)
            .await
            .expect("create_session");

        // Row exists and matches.
        let by_hash = sessions
            .get_by_token_hash(created.token.hash().as_bytes())
            .await
            .expect("get_by_token_hash")
            .expect("row present");
        assert_eq!(by_hash.id, created.session.id);
        assert_eq!(by_hash.user_id, user_id);
        assert_eq!(by_hash.expires_at, now + Duration::days(30));

        // The plaintext token bytes MUST NOT appear in the row's
        // `token_hash` field.
        let token_bytes = created.token.expose().as_bytes().to_vec();
        assert_ne!(
            by_hash.token_hash, token_bytes,
            "stored token_hash must be the SHA-256 digest, not the plaintext bytes"
        );
        assert_eq!(
            by_hash.token_hash.len(),
            32,
            "stored hash must be SHA-256-sized"
        );
    }

    #[tokio::test]
    async fn validate_session_token_succeeds_for_active_session() {
        let (service, _, _) = fast_service();
        let now = fixed_now();
        let created = service
            .create_session(UserId::new(), Duration::days(30), now)
            .await
            .expect("create_session");

        let row = service
            .validate_session_token(created.token.expose(), now)
            .await
            .expect("validate ok");
        assert_eq!(row.id, created.session.id);
    }

    #[tokio::test]
    async fn validate_session_token_rejects_unknown_token() {
        let (service, _, _) = fast_service();
        // Generate a token without persisting it.
        let stranger = SessionToken::generate();
        let err = service
            .validate_session_token(stranger.expose(), fixed_now())
            .await
            .expect_err("validate should fail");
        assert!(matches!(err, AuthServiceError::SessionInvalid));
    }

    #[tokio::test]
    async fn validate_session_token_rejects_expired_session() {
        let (service, _, _) = fast_service();
        let now = fixed_now();
        let created = service
            .create_session(UserId::new(), Duration::seconds(60), now)
            .await
            .expect("create_session");
        let later = now + Duration::seconds(120);
        let err = service
            .validate_session_token(created.token.expose(), later)
            .await
            .expect_err("validate should fail");
        assert!(matches!(err, AuthServiceError::SessionExpired));
    }

    #[tokio::test]
    async fn validate_session_token_rejects_revoked_session() {
        let (service, _, _) = fast_service();
        let now = fixed_now();
        let created = service
            .create_session(UserId::new(), Duration::days(30), now)
            .await
            .expect("create_session");
        service
            .revoke_session(created.session.id, now, Some("logout"))
            .await
            .expect("revoke");
        let err = service
            .validate_session_token(created.token.expose(), now)
            .await
            .expect_err("validate should fail");
        assert!(matches!(err, AuthServiceError::SessionRevoked));
    }

    #[tokio::test]
    async fn validate_session_token_prefers_revoked_over_expired() {
        // Pin the priority of the two failure shapes: a session that
        // is BOTH revoked AND expired must surface as SessionRevoked,
        // because revocation is a deliberate operator/user action and
        // expiry is a passive timestamp. The wire collapses both to
        // a single 401 body, but operator logs (and the future
        // session-revoked audit row) need the deliberate-action
        // signal to remain visible.
        let (service, _, _) = fast_service();
        let now = fixed_now();
        let created = service
            .create_session(UserId::new(), Duration::seconds(60), now)
            .await
            .expect("create_session");
        // Revoke at `now`.
        service
            .revoke_session(created.session.id, now, Some("logout"))
            .await
            .expect("revoke");
        // Validate well after expiry — the session is now both
        // revoked AND expired.
        let later = now + Duration::seconds(120);
        let err = service
            .validate_session_token(created.token.expose(), later)
            .await
            .expect_err("validate should fail");
        assert!(
            matches!(err, AuthServiceError::SessionRevoked),
            "revoked-and-expired must surface as SessionRevoked, got {err:?}"
        );
    }

    #[tokio::test]
    async fn revoke_session_is_idempotent() {
        let (service, _, sessions) = fast_service();
        let now = fixed_now();
        let created = service
            .create_session(UserId::new(), Duration::days(30), now)
            .await
            .expect("create_session");

        service
            .revoke_session(created.session.id, now, Some("logout"))
            .await
            .expect("first revoke");
        let later = now + Duration::seconds(60);
        service
            .revoke_session(created.session.id, later, Some("admin"))
            .await
            .expect("second revoke is a no-op");

        // Original revoked_at and reason preserved.
        let row = sessions
            .get(created.session.id)
            .await
            .expect("get")
            .expect("row present");
        assert_eq!(row.revoked_at, Some(now));
        assert_eq!(row.revoked_reason.as_deref(), Some("logout"));
    }

    #[tokio::test]
    async fn revoke_session_returns_session_invalid_for_unknown_id() {
        let (service, _, _) = fast_service();
        let err = service
            .revoke_session(UserSessionId::new(), fixed_now(), None)
            .await
            .expect_err("revoke unknown id should fail");
        assert!(matches!(err, AuthServiceError::SessionInvalid));
    }

    #[tokio::test]
    async fn revoke_all_for_user_revokes_only_active_rows() {
        let (service, _, sessions) = fast_service();
        let user_id = UserId::new();
        let now = fixed_now();

        let s1 = service
            .create_session(user_id, Duration::days(30), now)
            .await
            .expect("s1");
        let s2 = service
            .create_session(user_id, Duration::days(30), now)
            .await
            .expect("s2");

        // Pre-revoke s1 so revoke_all_for_user only flips s2.
        service
            .revoke_session(s1.session.id, now, Some("logout"))
            .await
            .expect("pre-revoke");
        let count = service
            .revoke_all_for_user(user_id, now, Some("admin"))
            .await
            .expect("revoke_all");
        assert_eq!(count, 1);

        let s2_row = sessions
            .get(s2.session.id)
            .await
            .expect("get")
            .expect("row present");
        assert!(s2_row.revoked_at.is_some());
    }

    // --- Redaction posture ---------------------------------------

    #[tokio::test]
    async fn errors_do_not_leak_secrets() {
        // Sentinel password and a sentinel-shaped token. After the
        // service returns each of its error variants, the rendered
        // strings must not contain either sentinel.
        let (service, _, _) = fast_service();
        let user_id = UserId::new();
        let bogus_password = "DO-NOT-LEAK-password-bytes";
        let bogus_token = SessionToken::generate();

        let invalid = service
            .verify_password(user_id, bogus_password)
            .await
            .expect_err("invalid");
        let expired_or_invalid = service
            .validate_session_token(bogus_token.expose(), fixed_now())
            .await
            .expect_err("invalid");

        for err in [invalid, expired_or_invalid] {
            let display = format!("{err}");
            let debug = format!("{err:?}");
            for sentinel in ["DO-NOT-LEAK-password-bytes", bogus_token.expose()] {
                assert!(
                    !display.contains(sentinel),
                    "error Display must not echo `{sentinel}`: rendered=`{display}`"
                );
                assert!(
                    !debug.contains(sentinel),
                    "error Debug must not echo `{sentinel}`: rendered=`{debug}`"
                );
            }
        }
    }

    #[tokio::test]
    async fn created_session_debug_is_redacted() {
        let (service, _, _) = fast_service();
        let now = fixed_now();
        let created = service
            .create_session(UserId::new(), Duration::days(30), now)
            .await
            .expect("create_session");
        let exposed = created.token.expose().to_owned();
        let dbg = format!("{created:?}");
        assert!(
            !dbg.contains(&exposed),
            "CreatedSession Debug must not echo the plaintext token"
        );
        assert!(
            dbg.contains("redacted"),
            "CreatedSession Debug must mark redaction"
        );
    }

    #[tokio::test]
    async fn auth_service_debug_does_not_leak() {
        let (service, _, _) = fast_service();
        let dbg = format!("{service:?}");
        assert!(
            !dbg.contains("19456") && !dbg.contains("19_456"),
            "AuthService Debug must not surface hasher params"
        );
        assert!(dbg.contains("AuthService"));
    }
}
