use async_trait::async_trait;
use relayterm_core::ids::UserId;
use relayterm_core::password_credential::PasswordCredential;
use relayterm_core::repository::{
    CreatePasswordCredential, PasswordCredentialRepository, RepositoryError,
};
use sqlx::PgPool;

use crate::error::map_sqlx_error;
use crate::rows::PasswordCredentialRow;

const ENTITY: &str = "user_password";

#[derive(Debug, Clone)]
pub struct PgPasswordCredentialRepository {
    pool: PgPool,
}

impl PgPasswordCredentialRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PasswordCredentialRepository for PgPasswordCredentialRepository {
    async fn upsert_for_user(
        &self,
        input: CreatePasswordCredential,
    ) -> Result<PasswordCredential, RepositoryError> {
        // ON CONFLICT updates `password_hash`, `updated_at`,
        // `password_changed_at`. `created_at` is preserved on the
        // existing row by EXCLUDED-only semantics on the columns we
        // explicitly list.
        let row: PasswordCredentialRow = sqlx::query_as(
            r#"
            INSERT INTO user_passwords (user_id, password_hash)
            VALUES ($1, $2)
            ON CONFLICT (user_id) DO UPDATE
                SET password_hash       = EXCLUDED.password_hash,
                    password_changed_at = NOW(),
                    updated_at          = NOW()
            RETURNING user_id, password_hash, password_changed_at, created_at, updated_at
            "#,
        )
        .bind(input.user_id.into_uuid())
        .bind(&input.password_hash)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.into_domain())
    }

    async fn get_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Option<PasswordCredential>, RepositoryError> {
        let row: Option<PasswordCredentialRow> = sqlx::query_as(
            r#"
            SELECT user_id, password_hash, password_changed_at, created_at, updated_at
            FROM user_passwords
            WHERE user_id = $1
            "#,
        )
        .bind(user_id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.map(PasswordCredentialRow::into_domain))
    }

    async fn any_exists(&self) -> Result<bool, RepositoryError> {
        // `SELECT 1 ... LIMIT 1` keeps the probe cheap regardless of the
        // table's eventual size. The bootstrap route is the only caller
        // and only fires before the first user is set up, so we trade a
        // round-trip for keeping the SQL trivial.
        let row: Option<(i32,)> = sqlx::query_as(
            r#"
            SELECT 1
            FROM user_passwords
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.is_some())
    }
}
