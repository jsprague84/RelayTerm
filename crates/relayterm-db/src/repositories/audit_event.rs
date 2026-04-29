use async_trait::async_trait;
use relayterm_core::audit_event::AuditEvent;
use relayterm_core::ids::AuditEventId;
use relayterm_core::repository::{AuditEventRepository, CreateAuditEvent, RepositoryError};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::AuditEventRow;

const ENTITY: &str = "audit_event";

#[derive(Debug, Clone)]
pub struct PgAuditEventRepository {
    pool: PgPool,
}

impl PgAuditEventRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditEventRepository for PgAuditEventRepository {
    async fn create(&self, input: CreateAuditEvent) -> Result<AuditEvent, RepositoryError> {
        let id = Uuid::new_v4();
        let row: AuditEventRow = sqlx::query_as(
            r#"
            INSERT INTO audit_events (id, actor_id, kind, payload, remote_addr)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, actor_id, kind, payload, remote_addr, recorded_at
            "#,
        )
        .bind(id)
        .bind(input.actor_id.map(relayterm_core::ids::UserId::into_uuid))
        .bind(input.kind.as_str())
        .bind(&input.payload)
        .bind(input.remote_addr.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.try_into_domain()
    }

    async fn recent(&self, limit: u32) -> Result<Vec<AuditEvent>, RepositoryError> {
        let rows: Vec<AuditEventRow> = sqlx::query_as(
            r#"
            SELECT id, actor_id, kind, payload, remote_addr, recorded_at
            FROM audit_events
            ORDER BY recorded_at DESC, id DESC
            LIMIT $1
            "#,
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        rows.into_iter()
            .map(AuditEventRow::try_into_domain)
            .collect()
    }

    async fn get(&self, id: AuditEventId) -> Result<Option<AuditEvent>, RepositoryError> {
        let row: Option<AuditEventRow> = sqlx::query_as(
            r#"
            SELECT id, actor_id, kind, payload, remote_addr, recorded_at
            FROM audit_events
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(AuditEventRow::try_into_domain).transpose()
    }
}
