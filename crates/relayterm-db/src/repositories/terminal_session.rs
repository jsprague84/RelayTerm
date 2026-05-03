use async_trait::async_trait;
use chrono::{DateTime, Utc};
use relayterm_core::ids::{TerminalSessionAttachmentId, TerminalSessionId, UserId};
use relayterm_core::repository::{
    CreateTerminalSession, CreateTerminalSessionAttachment, RepositoryError,
    TerminalSessionRepository,
};
use relayterm_core::session_event::SessionEventKind;
use relayterm_core::terminal_session::{
    ReconciledTerminalSession, TerminalSession, TerminalSessionAttachment, TerminalSessionStatus,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::{TerminalSessionAttachmentRow, TerminalSessionRow};

const ENTITY: &str = "terminal_session";
const ATTACHMENT_ENTITY: &str = "terminal_session_attachment";
const SESSION_EVENT_ENTITY: &str = "session_event";

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

    async fn reconcile_orphaned_on_startup(
        &self,
        at: DateTime<Utc>,
    ) -> Result<Vec<ReconciledTerminalSession>, RepositoryError> {
        // The whole sweep runs in one transaction so the status
        // transition and its matching `session_events` row are
        // committed together. Reconciliation is once-at-startup and
        // strictly bounded by the count of orphaned rows; the
        // transaction stays short.
        //
        // `FOR UPDATE` locks the candidate rows so a second startup
        // racing the same database (operator restart with overlap)
        // observes consistent state instead of double-closing a row
        // and writing two `session_events`. Locks are released when
        // the transaction commits.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;

        let candidates: Vec<(Uuid, String)> = sqlx::query_as(
            r#"
            SELECT id, status
            FROM terminal_sessions
            WHERE status IN ('starting', 'active', 'detached')
            ORDER BY id
            FOR UPDATE
            "#,
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY, e))?;

        let mut reconciled = Vec::with_capacity(candidates.len());
        for (id, status_str) in candidates {
            let previous_status =
                TerminalSessionStatus::from_str_tag(&status_str).ok_or_else(|| {
                    RepositoryError::Validation {
                        field: "status",
                        message: format!("unknown terminal session status `{status_str}`"),
                    }
                })?;

            sqlx::query(
                r#"
                UPDATE terminal_sessions
                SET status = 'closed',
                    closed_at = $2,
                    last_seen_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(id)
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;

            // Public metadata only: the reason code, the previous
            // status (`starting` / `active` / `detached`), and the
            // reconciliation timestamp. NEVER terminal output, peer
            // banners, recording bytes, or `client_info`.
            let payload = serde_json::json!({
                "reason": "startup_reconciliation",
                "previous_status": previous_status.as_str(),
                "reconciled_at": at,
            });
            let event_id = Uuid::new_v4();
            sqlx::query(
                r#"
                INSERT INTO session_events (id, session_id, kind, payload)
                VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(event_id)
            .bind(id)
            .bind(SessionEventKind::Closed.as_str())
            .bind(&payload)
            .execute(&mut *tx)
            .await
            .map_err(|e| map_sqlx_error(SESSION_EVENT_ENTITY, e))?;

            reconciled.push(ReconciledTerminalSession {
                session_id: TerminalSessionId::from_uuid(id),
                previous_status,
            });
        }

        tx.commit().await.map_err(|e| map_sqlx_error(ENTITY, e))?;
        Ok(reconciled)
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
