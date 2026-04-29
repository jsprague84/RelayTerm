use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::{TerminalSessionAttachmentId, TerminalSessionId, UserId};
use relayterm_core::repository::{
    CreateTerminalSession, CreateTerminalSessionAttachment, RepositoryError,
    TerminalSessionRepository,
};
use relayterm_core::terminal_session::{
    TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::{TerminalSessionAttachmentRow, TerminalSessionRow};

const ENTITY: &str = "terminal_session";
const ATTACHMENT_ENTITY: &str = "terminal_session_attachment";

#[derive(Debug, Clone)]
pub struct PgTerminalSessionRepository {
    pool: PgPool,
}

impl PgTerminalSessionRepository {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TerminalSessionRepository for PgTerminalSessionRepository {
    async fn create(
        &self,
        input: CreateTerminalSession,
    ) -> Result<TerminalSession, RepositoryError> {
        let id = Uuid::new_v4();
        let row: TerminalSessionRow = sqlx::query_as(
            r#"
            INSERT INTO terminal_sessions (
                id, owner_id, server_profile_id, status, cols, rows
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, owner_id, server_profile_id, status, cols, rows,
                      created_at, last_seen_at, closed_at
            "#,
        )
        .bind(id)
        .bind(input.owner_id.into_uuid())
        .bind(input.server_profile_id.into_uuid())
        .bind(input.status.as_str())
        .bind(i32::from(input.cols))
        .bind(i32::from(input.rows))
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.try_into_domain()
    }

    async fn get(&self, id: TerminalSessionId) -> Result<Option<TerminalSession>, RepositoryError> {
        let row: Option<TerminalSessionRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, server_profile_id, status, cols, rows,
                   created_at, last_seen_at, closed_at
            FROM terminal_sessions
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        row.map(TerminalSessionRow::try_into_domain).transpose()
    }

    async fn list_for_user(
        &self,
        owner_id: UserId,
    ) -> Result<Vec<TerminalSession>, RepositoryError> {
        let rows: Vec<TerminalSessionRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, server_profile_id, status, cols, rows,
                   created_at, last_seen_at, closed_at
            FROM terminal_sessions
            WHERE owner_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(owner_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        rows.into_iter()
            .map(TerminalSessionRow::try_into_domain)
            .collect()
    }

    async fn set_status(
        &self,
        id: TerminalSessionId,
        status: TerminalSessionStatus,
        closed_at: Option<DateTime<Utc>>,
    ) -> Result<(), RepositoryError> {
        let result = sqlx::query(
            r#"
            UPDATE terminal_sessions
            SET status = $2,
                closed_at = $3,
                last_seen_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .bind(status.as_str())
        .bind(closed_at)
        .execute(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound { entity: ENTITY });
        }
        Ok(())
    }

    async fn create_attachment(
        &self,
        input: CreateTerminalSessionAttachment,
    ) -> Result<TerminalSessionAttachment, RepositoryError> {
        let id = Uuid::new_v4();
        let row: TerminalSessionAttachmentRow = sqlx::query_as(
            r#"
            INSERT INTO terminal_session_attachments (
                id, session_id, client_info, remote_addr
            )
            VALUES ($1, $2, $3, $4)
            RETURNING id, session_id, attached_at, detached_at, client_info,
                      remote_addr, last_seen_seq
            "#,
        )
        .bind(id)
        .bind(input.session_id.into_uuid())
        .bind(input.client_info.as_deref())
        .bind(input.remote_addr.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ATTACHMENT_ENTITY, e))?;

        Ok(row.into_domain())
    }

    async fn list_attachments(
        &self,
        session_id: TerminalSessionId,
    ) -> Result<Vec<TerminalSessionAttachment>, RepositoryError> {
        let rows: Vec<TerminalSessionAttachmentRow> = sqlx::query_as(
            r#"
            SELECT id, session_id, attached_at, detached_at, client_info,
                   remote_addr, last_seen_seq
            FROM terminal_session_attachments
            WHERE session_id = $1
            ORDER BY attached_at ASC
            "#,
        )
        .bind(session_id.into_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ATTACHMENT_ENTITY, e))?;

        Ok(rows
            .into_iter()
            .map(TerminalSessionAttachmentRow::into_domain)
            .collect())
    }

    async fn get_attachment(
        &self,
        id: TerminalSessionAttachmentId,
    ) -> Result<Option<TerminalSessionAttachment>, RepositoryError> {
        let row: Option<TerminalSessionAttachmentRow> = sqlx::query_as(
            r#"
            SELECT id, session_id, attached_at, detached_at, client_info,
                   remote_addr, last_seen_seq
            FROM terminal_session_attachments
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ATTACHMENT_ENTITY, e))?;

        Ok(row.map(TerminalSessionAttachmentRow::into_domain))
    }

    async fn mark_attachment_detached(
        &self,
        id: TerminalSessionAttachmentId,
        detached_at: DateTime<Utc>,
        last_seen_seq: Option<i64>,
    ) -> Result<(), RepositoryError> {
        // Idempotent first-write: only stamp `detached_at` if it's still
        // NULL. A redundant detach call (client drop + WS close path racing,
        // for example) leaves the original timestamp + seq intact rather
        // than overwriting them. The "missing row" case below is the only
        // hard error.
        let result = sqlx::query(
            r#"
            UPDATE terminal_session_attachments
            SET detached_at = COALESCE(detached_at, $2),
                last_seen_seq = COALESCE(last_seen_seq, $3)
            WHERE id = $1
            "#,
        )
        .bind(id.into_uuid())
        .bind(detached_at)
        .bind(last_seen_seq)
        .execute(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(ATTACHMENT_ENTITY, e))?;

        if result.rows_affected() == 0 {
            return Err(RepositoryError::NotFound {
                entity: ATTACHMENT_ENTITY,
            });
        }
        Ok(())
    }
}
