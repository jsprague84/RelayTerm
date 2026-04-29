use async_trait::async_trait;
use relayterm_core::host::Host;
use relayterm_core::ids::{HostId, UserId};
use relayterm_core::repository::{CreateHost, HostRepository, RepositoryError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::HostRow;

const ENTITY: &str = "host";

#[derive(Debug, Clone)]
pub struct PgHostRepository {
    pool: PgPool,
}

impl PgHostRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HostRepository for PgHostRepository {
    async fn create(&self, input: CreateHost) -> Result<Host, RepositoryError> {
        let id = Uuid::new_v4();
        let row: HostRow = sqlx::query_as(
            r#"
            INSERT INTO hosts (
                id, owner_id, display_name, hostname, port, default_username
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, owner_id, display_name, hostname, port,
                      default_username, created_at, updated_at
            "#,
        )
        .bind(id)
        .bind(input.owner_id.into_uuid())
        .bind(input.display_name.as_str())
        .bind(input.hostname.as_str())
        .bind(i32::from(input.port.get()))
        .bind(input.default_username.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.try_into_domain()
    }

    async fn get(&self, id: HostId) -> Result<Option<Host>, RepositoryError> {
        let row: Option<HostRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, display_name, hostname, port,
                   default_username, created_at, updated_at
            FROM hosts
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(HostRow::try_into_domain).transpose()
    }

    async fn list_for_user(&self, owner_id: UserId) -> Result<Vec<Host>, RepositoryError> {
        let rows: Vec<HostRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, display_name, hostname, port,
                   default_username, created_at, updated_at
            FROM hosts
            WHERE owner_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(owner_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        rows.into_iter().map(HostRow::try_into_domain).collect()
    }
}
