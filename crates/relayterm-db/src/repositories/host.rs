use async_trait::async_trait;
use relayterm_core::host::Host;
use relayterm_core::ids::{HostId, UserId};
use relayterm_core::repository::{CreateHost, HostRepository, RepositoryError, UpdateHost};
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

    async fn update(
        &self,
        id: HostId,
        owner_id: UserId,
        input: UpdateHost,
    ) -> Result<Host, RepositoryError> {
        // COALESCE($n::T, column) lets each field stay independently
        // optional in a single SQL statement: a `NULL` parameter leaves
        // the column unchanged, a non-`NULL` parameter overwrites.
        // Casting the parameters keeps Postgres' parameter inference
        // unambiguous when the binder sees `Option<T>`.
        //
        // Owner scoping is enforced inside the `WHERE` clause; an
        // unowned-but-existing row OR a missing id both produce zero
        // rows and surface as `NotFound` so cross-user existence is
        // never leaked.
        let row: Option<HostRow> = sqlx::query_as(
            r#"
            UPDATE hosts
            SET display_name     = COALESCE($3, display_name),
                hostname         = COALESCE($4, hostname),
                port             = COALESCE($5, port),
                default_username = COALESCE($6, default_username),
                updated_at       = NOW()
            WHERE id = $1 AND owner_id = $2
            RETURNING id, owner_id, display_name, hostname, port,
                      default_username, created_at, updated_at
            "#,
        )
        .bind(id.into_uuid())
        .bind(owner_id.into_uuid())
        .bind(input.display_name.as_ref().map(|v| v.as_str()))
        .bind(input.hostname.as_ref().map(|v| v.as_str()))
        .bind(input.port.map(|p| i32::from(p.get())))
        .bind(input.default_username.as_ref().map(|v| v.as_str()))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(HostRow::try_into_domain)
            .transpose()?
            .ok_or(RepositoryError::NotFound { entity: ENTITY })
    }

    async fn delete(&self, id: HostId, owner_id: UserId) -> Result<(), RepositoryError> {
        // Owner scoping is in SQL so a foreign-owned id collapses to
        // `NotFound` just like the read path. A FK violation from a
        // racing profile-create that slips between the route's
        // pre-check and the DELETE surfaces as `Conflict` via the
        // foreign-key branch of `map_sqlx_error`.
        let result = sqlx::query(
            r#"
            DELETE FROM hosts
            WHERE id = $1 AND owner_id = $2
            "#,
        )
        .bind(id.into_uuid())
        .bind(owner_id.into_uuid())
        .execute(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound { entity: ENTITY });
        }
        Ok(())
    }

    async fn any_dependents_for_user(
        &self,
        id: HostId,
        owner_id: UserId,
    ) -> Result<bool, RepositoryError> {
        // The host has dependents when either:
        //  - any `server_profiles` row owned by this user references it, OR
        //  - any `known_host_entries` row exists for it (regardless of
        //    owner — the host itself is owner-scoped, so all rows for
        //    this host belong to the same owner transitively).
        //
        // Both subqueries short-circuit on EXISTS; the planner uses the
        // indexes `server_profiles_host_id_idx` and the FK index on
        // `known_host_entries.host_id`.
        let exists: (bool,) = sqlx::query_as(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM server_profiles
                WHERE host_id = $1 AND owner_id = $2
            ) OR EXISTS (
                SELECT 1 FROM known_host_entries
                WHERE host_id = $1
            )
            "#,
        )
        .bind(id.into_uuid())
        .bind(owner_id.into_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;
        Ok(exists.0)
    }
}
