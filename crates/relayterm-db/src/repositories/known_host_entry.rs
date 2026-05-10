use async_trait::async_trait;
use relayterm_core::audit_event::AuditEventKind;
use relayterm_core::ids::HostId;
use relayterm_core::known_host::KnownHostEntry;
use relayterm_core::repository::{
    CreateKnownHostEntry, KnownHostEntryRepository, ReplaceActivePin, ReplacedKnownHostEntries,
    RepositoryError,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::map_sqlx_error;
use crate::rows::KnownHostEntryRow;

const ENTITY: &str = "known_host_entry";
const ENTITY_AUDIT: &str = "audit_event";

/// `SELECT` projection used by every read in this module. Listed once so a
/// future column add only touches this constant + [`KnownHostEntryRow`].
const KNOWN_HOST_ENTRY_COLUMNS: &str = "id, host_id, key_type, fingerprint_sha256, public_key, \
    first_seen_at, trusted_at, revoked_at, revoked_by, revoked_reason_code, replaced_by_id";

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
        let sql = format!(
            "INSERT INTO known_host_entries (
                id, host_id, key_type, fingerprint_sha256, public_key
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING {KNOWN_HOST_ENTRY_COLUMNS}",
        );
        let row: KnownHostEntryRow = sqlx::query_as(&sql)
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
        let sql = format!(
            "SELECT {KNOWN_HOST_ENTRY_COLUMNS}
            FROM known_host_entries
            WHERE host_id = $1
            ORDER BY first_seen_at ASC",
        );
        let rows: Vec<KnownHostEntryRow> = sqlx::query_as(&sql)
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
        let sql = format!(
            "SELECT {KNOWN_HOST_ENTRY_COLUMNS}
            FROM known_host_entries
            WHERE host_id = $1 AND fingerprint_sha256 = $2",
        );
        let row: Option<KnownHostEntryRow> = sqlx::query_as(&sql)
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
        let sql = format!(
            "INSERT INTO known_host_entries (
                id, host_id, key_type, fingerprint_sha256, public_key, trusted_at
            )
            VALUES ($1, $2, $3, $4, $5, NOW())
            ON CONFLICT (host_id, fingerprint_sha256) DO UPDATE
                SET trusted_at = COALESCE(known_host_entries.trusted_at, EXCLUDED.trusted_at)
                WHERE known_host_entries.revoked_at IS NULL
            RETURNING {KNOWN_HOST_ENTRY_COLUMNS}",
        );
        let row: Option<KnownHostEntryRow> = sqlx::query_as(&sql)
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

    async fn replace_active_pin(
        &self,
        input: ReplaceActivePin,
    ) -> Result<ReplacedKnownHostEntries, RepositoryError> {
        // Single transaction: lock the active pin, refuse if either the
        // active row is gone / mismatched OR a revoked row already
        // exists for the new fingerprint, INSERT the new row, UPDATE
        // the old row, then APPEND the paired `host_key_revoked` +
        // `host_key_accepted` audit rows. Either every write commits or
        // none do (option (a) per `docs/spec/host-key-replace.md` § R7,
        // mirrors `TerminalRecordingRepository::purge_for_retention`).
        // An audit-insert failure ROLLBACKs the row mutations: a
        // partial-success orphan (replace without audit) is the worst
        // possible shape on a security-sensitive replace, so the design
        // is deliberately fail-closed.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;

        // 1. Lock the active row matching (host_id, expected_old_fingerprint,
        //    revoked_at IS NULL, trusted_at IS NOT NULL). `FOR UPDATE`
        //    serialises against any concurrent replace targeting the same
        //    row. Zero rows collapses "no active pin" and "active pin
        //    mismatch" into a single typed conflict — the route layer
        //    reads the active pin again before calling this and surfaces
        //    the precise SPA copy.
        let select_active_sql = format!(
            "SELECT {KNOWN_HOST_ENTRY_COLUMNS}
            FROM known_host_entries
            WHERE host_id = $1
              AND fingerprint_sha256 = $2
              AND revoked_at IS NULL
              AND trusted_at IS NOT NULL
            FOR UPDATE",
        );
        let old_row: Option<KnownHostEntryRow> = sqlx::query_as(&select_active_sql)
            .bind(input.host_id.into_uuid())
            .bind(&input.expected_old_fingerprint)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;
        let old_row = old_row.ok_or_else(|| RepositoryError::Conflict {
            entity: ENTITY,
            constraint: "active_pin_mismatch".to_owned(),
        })?;

        // 2. TOCTOU-close: re-assert that no row exists for (host_id,
        //    new_fingerprint_sha256) inside the open transaction. Postgres
        //    READ COMMITTED is sufficient — a committed concurrent revoke
        //    is visible to a fresh SELECT. Distinguish revoked vs.
        //    non-revoked existing rows so the route layer can surface
        //    captured_revoked vs. duplicate-trust precisely. Otherwise
        //    the unique index on (host_id, fingerprint_sha256) would
        //    fire on the INSERT and produce a generic conflict.
        let select_existing_sql = format!(
            "SELECT {KNOWN_HOST_ENTRY_COLUMNS}
            FROM known_host_entries
            WHERE host_id = $1 AND fingerprint_sha256 = $2",
        );
        let existing_new: Option<KnownHostEntryRow> = sqlx::query_as(&select_existing_sql)
            .bind(input.host_id.into_uuid())
            .bind(&input.new_fingerprint_sha256)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;
        if let Some(existing) = existing_new {
            let constraint = if existing.revoked_at.is_some() {
                "new_fingerprint_revoked"
            } else {
                "new_fingerprint_already_active"
            };
            return Err(RepositoryError::Conflict {
                entity: ENTITY,
                constraint: constraint.to_owned(),
            });
        }

        // 3. INSERT the new pin. `trusted_at = NOW()`, `revoked_at = NULL`,
        //    `replaced_by_id = NULL` by default. Any constraint failure
        //    here (FK on host_id, key_type CHECK, etc.) bubbles through
        //    map_sqlx_error and ROLLBACKs the entire tx.
        let new_id = Uuid::new_v4();
        let insert_new_sql = format!(
            "INSERT INTO known_host_entries (
                id, host_id, key_type, fingerprint_sha256, public_key, trusted_at
            )
            VALUES ($1, $2, $3, $4, $5, NOW())
            RETURNING {KNOWN_HOST_ENTRY_COLUMNS}",
        );
        let new_row: KnownHostEntryRow = sqlx::query_as(&insert_new_sql)
            .bind(new_id)
            .bind(input.host_id.into_uuid())
            .bind(input.new_key_type.as_str())
            .bind(&input.new_fingerprint_sha256)
            .bind(&input.new_public_key)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;

        // 4. UPDATE the old row with the full revoke metadata atomically.
        //    The schema CHECK `known_host_entries_revoked_columns_set_together`
        //    is the defence-in-depth backstop: it would refuse a partial
        //    UPDATE here. Because we lock the row in step 1, a concurrent
        //    revoke of the same row is impossible inside this tx.
        let update_old_sql = format!(
            "UPDATE known_host_entries
            SET revoked_at          = NOW(),
                revoked_by          = $2,
                revoked_reason_code = $3,
                replaced_by_id      = $4
            WHERE id = $1
            RETURNING {KNOWN_HOST_ENTRY_COLUMNS}",
        );
        let revoked_old_row: KnownHostEntryRow = sqlx::query_as(&update_old_sql)
            .bind(old_row.id)
            .bind(input.revoked_by.into_uuid())
            .bind(input.reason_code.as_str())
            .bind(new_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| map_sqlx_error(ENTITY, e))?;

        // 5. Paired audit rows (`host_key_revoked` then
        //    `host_key_accepted`) inside the SAME transaction
        //    (`docs/spec/host-key-replace.md` § R2 / R7 option (a)).
        //    Payloads are built field-by-field from public-safe
        //    primitives — host_id, the two known-host-entry ids,
        //    fingerprints (the public form of the host key), key_type,
        //    and the operator-supplied reason_code. NEVER the public
        //    key bytes (the fingerprint already identifies the key),
        //    NEVER the host's hostname/port (those are downstream of
        //    host_id), NEVER any russh / DB error text, NEVER any
        //    operator-supplied free text (the schema's `reason_code`
        //    enum is the only operator input persisted).
        //
        //    The two payloads cross-link via `replacement_known_host_entry_id`
        //    so an audit feed can present the pair as a single intent
        //    (§ R2). An audit-insert failure flows through
        //    `map_sqlx_error` and bubbles out of this function; `tx`
        //    drops without committing, so neither audit row AND
        //    neither row mutation lands. Sentinel-string redaction
        //    tests in the API test crate (`AUDIT_FORBIDDEN_SUBSTRINGS`)
        //    are the second-line guard.
        let key_type_str = input.new_key_type.as_str();
        let reason_code_str = input.reason_code.as_str();
        let revoked_payload = serde_json::json!({
            "host_id": input.host_id.into_uuid(),
            "known_host_entry_id": old_row.id,
            "replacement_known_host_entry_id": new_id,
            "old_fingerprint": &input.expected_old_fingerprint,
            "new_fingerprint": &input.new_fingerprint_sha256,
            "key_type": key_type_str,
            "reason_code": reason_code_str,
        });
        let accepted_payload = serde_json::json!({
            "host_id": input.host_id.into_uuid(),
            "known_host_entry_id": new_id,
            "replacement_known_host_entry_id": old_row.id,
            "old_fingerprint": &input.expected_old_fingerprint,
            "new_fingerprint": &input.new_fingerprint_sha256,
            "key_type": key_type_str,
            "reason_code": reason_code_str,
        });
        let revoked_audit_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO audit_events (id, actor_id, kind, payload, remote_addr)
             VALUES ($1, $2, $3, $4, NULL)",
        )
        .bind(revoked_audit_id)
        .bind(input.revoked_by.into_uuid())
        .bind(AuditEventKind::HostKeyRevoked.as_str())
        .bind(&revoked_payload)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_AUDIT, e))?;
        let accepted_audit_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO audit_events (id, actor_id, kind, payload, remote_addr)
             VALUES ($1, $2, $3, $4, NULL)",
        )
        .bind(accepted_audit_id)
        .bind(input.revoked_by.into_uuid())
        .bind(AuditEventKind::HostKeyAccepted.as_str())
        .bind(&accepted_payload)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_sqlx_error(ENTITY_AUDIT, e))?;

        tx.commit().await.map_err(|e| map_sqlx_error(ENTITY, e))?;

        Ok(ReplacedKnownHostEntries {
            revoked_old: revoked_old_row.try_into_domain()?,
            trusted_new: new_row.try_into_domain()?,
        })
    }
}
