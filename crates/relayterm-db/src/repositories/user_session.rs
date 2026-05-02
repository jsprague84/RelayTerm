use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::{UserId, UserSessionId};
use relayterm_core::repository::{CreateUserSession, RepositoryError, UserSessionRepository};
use relayterm_core::user_session::UserSession;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::UserSessionRow;

const ENTITY: &str = "user_session";

#[derive(Debug, Clone)]
pub struct PgUserSessionRepository {
    pool: PgPool,
}

impl PgUserSessionRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserSessionRepository for PgUserSessionRepository {
    async fn create(&self, input: CreateUserSession) -> Result<UserSession, RepositoryError> {
        let id = Uuid::new_v4();
        let row: UserSessionRow = sqlx::query_as(
            r#"
            INSERT INTO user_sessions (id, user_id, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING id, user_id, token_hash, created_at, last_seen_at,
                      expires_at, revoked_at, revoked_reason
            "#,
        )
        .bind(id)
        .bind(input.user_id.into_uuid())
        .bind(&input.token_hash)
        .bind(input.expires_at)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.into_domain())
    }

    async fn get_by_token_hash(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<UserSession>, RepositoryError> {
        let row: Option<UserSessionRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, token_hash, created_at, last_seen_at,
                   expires_at, revoked_at, revoked_reason
            FROM user_sessions
            WHERE token_hash = $1
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.map(UserSessionRow::into_domain))
    }

    async fn get(&self, id: UserSessionId) -> Result<Option<UserSession>, RepositoryError> {
        let row: Option<UserSessionRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, token_hash, created_at, last_seen_at,
                   expires_at, revoked_at, revoked_reason
            FROM user_sessions
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.map(UserSessionRow::into_domain))
    }

    async fn touch_last_seen(
        &self,
        id: UserSessionId,
        at: DateTime<Utc>,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            r#"
            UPDATE user_sessions
            SET last_seen_at = $2
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound { entity: ENTITY });
        }
        Ok(())
    }

    async fn revoke(
        &self,
        id: UserSessionId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<(), RepositoryError> {
        // Single statement — `RETURNING id` lets us distinguish "unknown
        // id" (no row matched, surfaced as NotFound) from "redundant
        // revoke" (row matched but already revoked, surfaced as Ok and
        // a no-op). The CASE expressions enforce idempotency in SQL:
        // when `revoked_at IS NOT NULL` we keep the existing timestamp
        // and reason so the audit trail records when revocation
        // actually happened, not when a redundant call was made. Using
        // a single statement closes the SELECT-then-UPDATE race where a
        // row deleted between the two could surface as a silent Ok.
        let row: Option<(Uuid,)> = sqlx::query_as(
            r#"
            UPDATE user_sessions
            SET revoked_at     = CASE WHEN revoked_at IS NULL THEN $2 ELSE revoked_at END,
                revoked_reason = CASE WHEN revoked_at IS NULL THEN $3 ELSE revoked_reason END
            WHERE id = $1
            RETURNING id
            "#,
        )
        .bind(id.into_uuid())
        .bind(at)
        .bind(reason)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        if row.is_none() {
            return Err(RepositoryError::NotFound { entity: ENTITY });
        }
        Ok(())
    }

    async fn revoke_all_for_user(
        &self,
        user_id: UserId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<u64, RepositoryError> {
        let result = sqlx::query(
            r#"
            UPDATE user_sessions
            SET revoked_at     = $2,
                revoked_reason = $3
            WHERE user_id = $1
              AND revoked_at IS NULL
            "#,
        )
        .bind(user_id.into_uuid())
        .bind(at)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(result.rows_affected())
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<UserSession>, RepositoryError> {
        let rows: Vec<UserSessionRow> = sqlx::query_as(
            r#"
            SELECT id, user_id, token_hash, created_at, last_seen_at,
                   expires_at, revoked_at, revoked_reason
            FROM user_sessions
            WHERE user_id = $1
            ORDER BY created_at DESC, id
            "#,
        )
        .bind(user_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(rows.into_iter().map(UserSessionRow::into_domain).collect())
    }

    async fn revoke_for_user(
        &self,
        user_id: UserId,
        session_id: UserSessionId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<bool, RepositoryError> {
        // Ownership is enforced in SQL via `(id, user_id)`. A row
        // owned by a different user OR a row that doesn't exist both
        // miss the filter and surface as NotFound — collapsing the two
        // is the probe-resistance contract for the revoke route.
        //
        // The `old` CTE captures the prior `revoked_at` BEFORE the
        // UPDATE runs so the boolean return cleanly distinguishes a
        // real transition (Ok(true), prior was NULL) from a no-op
        // revoke against an already-revoked row (Ok(false)). A naive
        // `UPDATE ... RETURNING` cannot do this — `RETURNING`
        // observes the post-update row, by which point the prior
        // timestamp is already overwritten.
        //
        // Idempotency mirrors the `revoke` method: the CASE
        // expressions preserve the original `revoked_at` /
        // `revoked_reason` on the redundant call so audit history
        // stays honest.
        let row: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
            r#"
            WITH old AS (
                SELECT id, revoked_at AS prior_revoked_at
                FROM user_sessions
                WHERE id = $1 AND user_id = $2
            )
            UPDATE user_sessions us
            SET revoked_at     = CASE WHEN us.revoked_at IS NULL THEN $3 ELSE us.revoked_at END,
                revoked_reason = CASE WHEN us.revoked_at IS NULL THEN $4 ELSE us.revoked_reason END
            FROM old
            WHERE us.id = old.id
            RETURNING old.prior_revoked_at
            "#,
        )
        .bind(session_id.into_uuid())
        .bind(user_id.into_uuid())
        .bind(at)
        .bind(reason)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        match row {
            None => Err(RepositoryError::NotFound { entity: ENTITY }),
            Some((prior_revoked_at,)) => Ok(prior_revoked_at.is_none()),
        }
    }

    async fn revoke_all_except(
        &self,
        user_id: UserId,
        except_id: UserSessionId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<u64, RepositoryError> {
        let result = sqlx::query(
            r#"
            UPDATE user_sessions
            SET revoked_at     = $3,
                revoked_reason = $4
            WHERE user_id = $1
              AND id <> $2
              AND revoked_at IS NULL
            "#,
        )
        .bind(user_id.into_uuid())
        .bind(except_id.into_uuid())
        .bind(at)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(result.rows_affected())
    }
}
