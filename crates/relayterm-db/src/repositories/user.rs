use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::UserId;
use relayterm_core::repository::{CreateUser, RepositoryError, UserRepository};
use relayterm_core::user::User;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::UserRow;

const ENTITY: &str = "user";

#[derive(Debug, Clone)]
pub struct PgUserRepository {
    pool: PgPool,
}

impl PgUserRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl UserRepository for PgUserRepository {
    async fn create(&self, input: CreateUser) -> Result<User, RepositoryError> {
        let id = Uuid::new_v4();
        let row: UserRow = sqlx::query_as(
            r#"
            INSERT INTO users (id, email, display_name)
            VALUES ($1, $2, $3)
            RETURNING id, email, display_name, created_at, last_login_at
            "#,
        )
        .bind(id)
        .bind(&input.email)
        .bind(&input.display_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.into_domain())
    }

    async fn get(&self, id: UserId) -> Result<Option<User>, RepositoryError> {
        let row: Option<UserRow> = sqlx::query_as(
            r#"
            SELECT id, email, display_name, created_at, last_login_at
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.map(UserRow::into_domain))
    }

    async fn get_by_email(&self, email: &str) -> Result<Option<User>, RepositoryError> {
        let row: Option<UserRow> = sqlx::query_as(
            r#"
            SELECT id, email, display_name, created_at, last_login_at
            FROM users
            WHERE LOWER(email) = LOWER($1)
            "#,
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.map(UserRow::into_domain))
    }

    async fn touch_last_login(&self, id: UserId, at: DateTime<Utc>) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            r#"
            UPDATE users
            SET last_login_at = $2
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
}
