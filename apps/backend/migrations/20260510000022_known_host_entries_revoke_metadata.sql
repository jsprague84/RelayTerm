-- Known-host entries: revoke metadata for the host-key replace flow.
--
-- The "revoke and replace" host-key flow (docs/spec/host-key-replace.md)
-- needs every revoked row to be self-describing without forcing the UI
-- into an audit-table join: who revoked it, why, and which fresh row
-- replaced it. The schema-level CHECK that the three "set together"
-- columns are written atomically is defence in depth against a future
-- code path that forgets one of them.
--
-- Notes for future migrations:
--
-- - `replaced_by_id` is intentionally NOT in the "set together" CHECK.
--   A future "revoke without replace" admin slice (covered by SPEC.md
--   "Known-host revocation policy") needs to revoke a pin that is not
--   being replaced; that case sets `revoked_at + revoked_by +
--   revoked_reason_code` but leaves `replaced_by_id` NULL. The CHECK
--   here therefore covers the three revoke-metadata columns only.
-- - The reason enum's accept-list is the second-layer backstop; the
--   primary validator lives at the route boundary
--   (KnownHostRevocationReason in relayterm-core). A schema migration
--   is required to add a new reason code.

ALTER TABLE known_host_entries
    ADD COLUMN revoked_by              UUID
        REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN revoked_reason_code     TEXT,
    ADD COLUMN replaced_by_id          UUID
        REFERENCES known_host_entries(id) ON DELETE SET NULL;

ALTER TABLE known_host_entries
    ADD CONSTRAINT known_host_entries_revoked_reason_chk CHECK (
        revoked_reason_code IS NULL
        OR revoked_reason_code IN (
            'server_reinstalled',
            'host_key_rotated',
            'lab_target_recreated',
            'operator_other'
        )
    );

ALTER TABLE known_host_entries
    ADD CONSTRAINT known_host_entries_revoked_columns_set_together CHECK (
        (revoked_at IS NULL
            AND revoked_by IS NULL
            AND revoked_reason_code IS NULL)
        OR (revoked_at IS NOT NULL
            AND revoked_by IS NOT NULL
            AND revoked_reason_code IS NOT NULL)
    );
