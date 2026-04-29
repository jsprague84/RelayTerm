use async_trait::async_trait;
use relayterm_core::ids::{SessionEventId, TerminalSessionId};
use relayterm_core::repository::{CreateSessionEvent, RepositoryError, SessionEventRepository};
use relayterm_core::session_event::SessionEvent;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::SessionEventRow;

const ENTITY: &str = "session_event";

#[derive(Debug, Clone)]
pub struct PgSessionEventRepository {
    pool: PgPool,
}

impl PgSessionEventRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SessionEventRepository for PgSessionEventRepository {
    async fn create(&self, input: CreateSessionEvent) -> Result<SessionEvent, RepositoryError> {
        let id = Uuid::new_v4();
        let row: SessionEventRow = sqlx::query_as(
            r#"
            INSERT INTO session_events (id, session_id, kind, payload)
            VALUES ($1, $2, $3, $4)
            RETURNING id, session_id, kind, payload, recorded_at
            "#,
        )
        .bind(id)
        .bind(input.session_id.into_uuid())
        .bind(input.kind.as_str())
        .bind(&input.payload)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.try_into_domain()
    }

    async fn list_for_session(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Vec<SessionEvent>, RepositoryError> {
        let rows: Vec<SessionEventRow> = sqlx::query_as(
            r#"
            SELECT id, session_id, kind, payload, recorded_at
            FROM session_events
            WHERE session_id = $1
            ORDER BY recorded_at ASC, id ASC
            "#,
        )
        .bind(session_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        rows.into_iter()
            .map(SessionEventRow::try_into_domain)
            .collect()
    }

    async fn get(&self, id: SessionEventId) -> Result<Option<SessionEvent>, RepositoryError> {
        let row: Option<SessionEventRow> = sqlx::query_as(
            r#"
            SELECT id, session_id, kind, payload, recorded_at
            FROM session_events
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(SessionEventRow::try_into_domain).transpose()
    }
}
