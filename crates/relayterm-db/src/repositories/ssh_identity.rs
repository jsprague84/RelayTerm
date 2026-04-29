use async_trait::async_trait;
use relayterm_core::ids::{SshIdentityId, UserId};
use relayterm_core::repository::{CreateSshIdentity, RepositoryError, SshIdentityRepository};
use relayterm_core::ssh_identity::SshIdentity;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::SshIdentityRow;

const ENTITY: &str = "ssh_identity";

#[derive(Debug, Clone)]
pub struct PgSshIdentityRepository {
    pool: PgPool,
}

impl PgSshIdentityRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SshIdentityRepository for PgSshIdentityRepository {
    async fn create(&self, input: CreateSshIdentity) -> Result<SshIdentity, RepositoryError> {
        let id = Uuid::new_v4();
        let row: SshIdentityRow = sqlx::query_as(
            r#"
            INSERT INTO ssh_identities (
                id, owner_id, name, key_type, public_key,
                encrypted_private_key, fingerprint_sha256
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id, owner_id, name, key_type, public_key,
                      encrypted_private_key, fingerprint_sha256,
                      created_at, last_used_at
            "#,
        )
        .bind(id)
        .bind(input.owner_id.into_uuid())
        .bind(&input.name)
        .bind(input.key_type.as_str())
        .bind(&input.public_key)
        .bind(&input.encrypted_private_key)
        .bind(&input.fingerprint_sha256)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.try_into_domain()
    }

    async fn get(&self, id: SshIdentityId) -> Result<Option<SshIdentity>, RepositoryError> {
        let row: Option<SshIdentityRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, name, key_type, public_key,
                   encrypted_private_key, fingerprint_sha256,
                   created_at, last_used_at
            FROM ssh_identities
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(SshIdentityRow::try_into_domain).transpose()
    }

    async fn list_for_user(&self, owner_id: UserId) -> Result<Vec<SshIdentity>, RepositoryError> {
        let rows: Vec<SshIdentityRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, name, key_type, public_key,
                   encrypted_private_key, fingerprint_sha256,
                   created_at, last_used_at
            FROM ssh_identities
            WHERE owner_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(owner_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        rows.into_iter()
            .map(SshIdentityRow::try_into_domain)
            .collect()
    }
}
