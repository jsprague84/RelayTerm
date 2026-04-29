use async_trait::async_trait;
use relayterm_core::ids::HostId;
use relayterm_core::known_host::KnownHostEntry;
use relayterm_core::repository::{CreateKnownHostEntry, KnownHostEntryRepository, RepositoryError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::KnownHostEntryRow;

const ENTITY: &str = "known_host_entry";

#[derive(Debug, Clone)]
pub struct PgKnownHostEntryRepository {
    pool: PgPool,
}

impl PgKnownHostEntryRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl KnownHostEntryRepository for PgKnownHostEntryRepository {
    async fn create(&self, input: CreateKnownHostEntry) -> Result<KnownHostEntry, RepositoryError> {
        let id = Uuid::new_v4();
        let row: KnownHostEntryRow = sqlx::query_as(
            r#"
            INSERT INTO known_host_entries (
                id, host_id, key_type, fingerprint_sha256, public_key
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, host_id, key_type, fingerprint_sha256, public_key,
                      first_seen_at, trusted_at, revoked_at
            "#,
        )
        .bind(id)
        .bind(input.host_id.into_uuid())
        .bind(input.key_type.as_str())
        .bind(&input.fingerprint_sha256)
        .bind(&input.public_key)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.try_into_domain()
    }

    async fn list_for_host(&self, host_id: HostId) -> Result<Vec<KnownHostEntry>, RepositoryError> {
        let rows: Vec<KnownHostEntryRow> = sqlx::query_as(
            r#"
            SELECT id, host_id, key_type, fingerprint_sha256, public_key,
                   first_seen_at, trusted_at, revoked_at
            FROM known_host_entries
            WHERE host_id = $1
            ORDER BY first_seen_at ASC
            "#,
        )
        .bind(host_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        rows.into_iter()
            .map(KnownHostEntryRow::try_into_domain)
            .collect()
    }

    async fn find_by_fingerprint(
        &self,
        host_id: HostId,
        fingerprint_sha256: &str,
    ) -> Result<Option<KnownHostEntry>, RepositoryError> {
        let row: Option<KnownHostEntryRow> = sqlx::query_as(
            r#"
            SELECT id, host_id, key_type, fingerprint_sha256, public_key,
                   first_seen_at, trusted_at, revoked_at
            FROM known_host_entries
            WHERE host_id = $1 AND fingerprint_sha256 = $2
            "#,
        )
        .bind(host_id.into_uuid())
        .bind(fingerprint_sha256)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(KnownHostEntryRow::try_into_domain).transpose()
    }

    async fn record_trusted(
        &self,
        input: CreateKnownHostEntry,
    ) -> Result<KnownHostEntry, RepositoryError> {
        // Single-statement upsert: insert a fresh trusted row, or — if one
        // already exists for this (host_id, fingerprint) — stamp
        // `trusted_at` if unset. `COALESCE(..., NOW())` keeps the original
        // trust timestamp on a re-confirm so audit history is preserved.
        //
        // The `WHERE known_host_entries.revoked_at IS NULL` clause on the
        // ON CONFLICT branch is load-bearing: a revoked row must NOT be
        // silently re-trusted by another upsert call. When the WHERE
        // rejects, `RETURNING` produces no row, and we surface a
        // `Conflict` so the caller (or the API layer) can return a 409
        // instead of misreporting success. Recovery from a revoked entry
        // is a deliberate operator action; there is no implicit path.
        let id = Uuid::new_v4();
        let row: Option<KnownHostEntryRow> = sqlx::query_as(
            r#"
            INSERT INTO known_host_entries (
                id, host_id, key_type, fingerprint_sha256, public_key, trusted_at
            )
            VALUES ($1, $2, $3, $4, $5, NOW())
            ON CONFLICT (host_id, fingerprint_sha256) DO UPDATE
                SET trusted_at = COALESCE(known_host_entries.trusted_at, EXCLUDED.trusted_at)
                WHERE known_host_entries.revoked_at IS NULL
            RETURNING id, host_id, key_type, fingerprint_sha256, public_key,
                      first_seen_at, trusted_at, revoked_at
            "#,
        )
        .bind(id)
        .bind(input.host_id.into_uuid())
        .bind(input.key_type.as_str())
        .bind(&input.fingerprint_sha256)
        .bind(&input.public_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        let row = row.ok_or_else(|| RepositoryError::Conflict {
            entity: ENTITY,
            constraint: "revoked".to_owned(),
        })?;
        row.try_into_domain()
    }
}
