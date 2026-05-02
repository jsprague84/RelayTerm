-- Extend the `audit_events.kind` CHECK constraint with the
-- `session_revoked` and `sessions_revoked` kinds emitted by the
-- current-user session-management routes.
--
-- `session_revoked` fires when one specific session row is revoked
-- through `POST /api/v1/auth/sessions/:id/revoke` (including the
-- caller's own current session). `sessions_revoked` fires once when
-- `POST /api/v1/auth/sessions/revoke-all-except-current` transitions
-- one or more rows from non-revoked to revoked; the payload carries the
-- count, never per-row session ids.
--
-- Strict superset of the previous set — no rows are invalidated.

ALTER TABLE audit_events DROP CONSTRAINT audit_events_kind_chk;

ALTER TABLE audit_events ADD CONSTRAINT audit_events_kind_chk CHECK (
    kind IN (
        'login_succeeded',
        'login_failed',
        'logout_succeeded',
        'first_user_created',
        'session_revoked',
        'sessions_revoked',
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
