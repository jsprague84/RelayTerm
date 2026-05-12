use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::{ServerProfileId, UserId};
use relayterm_core::repository::{
    CreateServerProfile, RepositoryError, ServerProfileRepository, SetOptional, UpdateServerProfile,
};
use relayterm_core::server_profile::ServerProfile;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::ServerProfileRow;

const ENTITY: &str = "server_profile";

#[derive(Debug, Clone)]
pub struct PgServerProfileRepository {
    pool: PgPool,
}

impl PgServerProfileRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ServerProfileRepository for PgServerProfileRepository {
    async fn create(&self, input: CreateServerProfile) -> Result<ServerProfile, RepositoryError> {
        let id = Uuid::new_v4();
        let tag_strings: Vec<String> = input.tags.iter().map(|t| t.as_str().to_owned()).collect();
        let username_override = input
            .username_override
            .as_ref()
            .map(|u| u.as_str().to_owned());

        let row: ServerProfileRow = sqlx::query_as(
            r#"
            INSERT INTO server_profiles (
                id, owner_id, name, host_id, ssh_identity_id,
                username_override, tags
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, owner_id, name, host_id, ssh_identity_id,
                      username_override, tags, created_at, updated_at,
                      last_connected_at, disabled_at
            "#,
        )
        .bind(id)
        .bind(input.owner_id.into_uuid())
        .bind(input.name.as_str())
        .bind(input.host_id.into_uuid())
        .bind(input.ssh_identity_id.into_uuid())
        .bind(username_override)
        .bind(&tag_strings)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.into_domain())
    }

    async fn get(&self, id: ServerProfileId) -> Result<Option<ServerProfile>, RepositoryError> {
        let row: Option<ServerProfileRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, name, host_id, ssh_identity_id,
                   username_override, tags, created_at, updated_at,
                   last_connected_at, disabled_at
            FROM server_profiles
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(row.map(ServerProfileRow::into_domain))
    }

    async fn list_for_user(&self, owner_id: UserId) -> Result<Vec<ServerProfile>, RepositoryError> {
        let rows: Vec<ServerProfileRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, name, host_id, ssh_identity_id,
                   username_override, tags, created_at, updated_at,
                   last_connected_at, disabled_at
            FROM server_profiles
            WHERE owner_id = $1
            ORDER BY name ASC
            "#,
        )
        .bind(owner_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(rows
            .into_iter()
            .map(ServerProfileRow::into_domain)
            .collect())
    }

    async fn update(
        &self,
        id: ServerProfileId,
        owner_id: UserId,
        input: UpdateServerProfile,
    ) -> Result<ServerProfile, RepositoryError> {
        // `name`, `host_id`, `ssh_identity_id`, and `tags` use the
        // simple COALESCE-with-`Option` shape — `NULL` parameter leaves
        // the column unchanged, otherwise overwrite.
        //
        // `username_override` is a nullable column with three-state
        // semantics: leave alone vs. clear-to-`NULL` vs. set-to-value.
        // The `set_username_override` boolean parameter ($7) drives a
        // `CASE` that distinguishes "no edit" from "clear to NULL".
        //
        // Tags ride as `TEXT[]`; passing `NULL` for $8 preserves the
        // current array.
        let tag_strings: Option<Vec<String>> = input
            .tags
            .as_ref()
            .map(|tags| tags.iter().map(|t| t.as_str().to_owned()).collect());
        let (set_username_override, username_override_value): (bool, Option<String>) =
            match &input.username_override {
                SetOptional::Unchanged => (false, None),
                SetOptional::Set(None) => (true, None),
                SetOptional::Set(Some(u)) => (true, Some(u.as_str().to_owned())),
            };

        let row: Option<ServerProfileRow> = sqlx::query_as(
            r#"
            UPDATE server_profiles
            SET name              = COALESCE($3, name),
                host_id           = COALESCE($4, host_id),
                ssh_identity_id   = COALESCE($5, ssh_identity_id),
                username_override = CASE
                    WHEN $7::BOOLEAN THEN $6
                    ELSE username_override
                END,
                tags              = COALESCE($8, tags),
                updated_at        = NOW()
            WHERE id = $1 AND owner_id = $2
            RETURNING id, owner_id, name, host_id, ssh_identity_id,
                      username_override, tags, created_at, updated_at,
                      last_connected_at, disabled_at
            "#,
        )
        .bind(id.into_uuid())
        .bind(owner_id.into_uuid())
        .bind(input.name.as_ref().map(|n| n.as_str()))
        .bind(input.host_id.map(|h| h.into_uuid()))
        .bind(input.ssh_identity_id.map(|i| i.into_uuid()))
        .bind(username_override_value)
        .bind(set_username_override)
        .bind(tag_strings)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(ServerProfileRow::into_domain)
            .ok_or(RepositoryError::NotFound { entity: ENTITY })
    }

    async fn delete(&self, id: ServerProfileId, owner_id: UserId) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            r#"
            DELETE FROM server_profiles
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
        id: ServerProfileId,
        owner_id: UserId,
    ) -> Result<bool, RepositoryError> {
        // The profile has dependents when at least one `terminal_sessions`
        // row references it. `terminal_sessions` is owner-scoped at the
        // row level; the join on `owner_id` is defence-in-depth so a
        // hypothetical row whose `owner_id` drifted from the profile's
        // `owner_id` still scopes correctly. EXISTS short-circuits on
        // the first match; `terminal_sessions_server_profile_id_idx`
        // makes the lookup cheap.
        //
        // The query is intentionally agnostic of `status` — closed
        // sessions still count, because `terminal_sessions` rows are
        // NEVER deleted from the user UI (AGENTS.md "Things to avoid";
        // schema FK is `ON DELETE RESTRICT`).
        let exists: (bool,) = sqlx::query_as(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM terminal_sessions
                WHERE server_profile_id = $1 AND owner_id = $2
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

    async fn set_disabled_at(
        &self,
        id: ServerProfileId,
        owner_id: UserId,
        disabled_at: Option<DateTime<Utc>>,
    ) -> Result<ServerProfile, RepositoryError> {
        // Ownership is enforced inside the SQL: an unowned-but-existing row
        // returns zero rows here, indistinguishable from a missing id, and
        // the route layer collapses both into the same 404. The SQL writes
        // the column AND bumps `updated_at` unconditionally; idempotency
        // (preserving the original `disabled_at` on a redundant operator
        // action) lives in the disable / enable handlers, not here.
        let row: Option<ServerProfileRow> = sqlx::query_as(
            r#"
            UPDATE server_profiles
            SET disabled_at = $3,
                updated_at  = NOW()
            WHERE id = $1 AND owner_id = $2
            RETURNING id, owner_id, name, host_id, ssh_identity_id,
                      username_override, tags, created_at, updated_at,
                      last_connected_at, disabled_at
            "#,
        )
        .bind(id.into_uuid())
        .bind(owner_id.into_uuid())
        .bind(disabled_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(ServerProfileRow::into_domain)
            .ok_or(RepositoryError::NotFound { entity: ENTITY })
    }
}
