-- Extend the `audit_events.kind` CHECK constraint with the
-- server-profile lifecycle kinds emitted by the disable/enable routes.
--
-- The existing constraint already allowed `server_profile_created`,
-- `server_profile_updated`, and `server_profile_deleted`. This migration
-- adds the disable/enable transition kinds so the create/disable/enable
-- handlers can append rows without violating the CHECK.
--
-- Strict superset of the previous set — no rows are invalidated.

ALTER TABLE audit_events DROP CONSTRAINT audit_events_kind_chk;

ALTER TABLE audit_events ADD CONSTRAINT audit_events_kind_chk CHECK (
    kind IN (
        'login_succeeded',
        'login_failed',
        'logout_succeeded',
        'key_vault_access',
        'key_vault_decrypt_failed',
        'host_key_accepted',
        'host_key_mismatch',
        'host_key_revoked',
        'server_profile_created',
        'server_profile_updated',
        'server_profile_disabled',
        'server_profile_enabled',
        'server_profile_deleted',
        'ssh_identity_created',
        'ssh_identity_deleted',
        'session_opened',
        'session_closed',
        'other'
    )
);
