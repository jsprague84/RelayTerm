-- Extend the `audit_events.kind` CHECK constraint with the
-- `password_changed` kind emitted by `POST /api/v1/auth/change-password`.
--
-- The route lets an authenticated user rotate their own password after
-- proving knowledge of the current one. The audit row is appended only
-- on a real password rotation (verify-current succeeded AND set-new
-- succeeded); a wrong-current-password attempt writes no audit row at
-- this kind. The payload carries `revoked_other_sessions: u64` only —
-- never the offered current/new password, never any password hash,
-- never session token bytes or token-hash bytes, never per-session ids.
--
-- Strict superset of the previous set — no rows are invalidated.

ALTER TABLE audit_events DROP CONSTRAINT audit_events_kind_chk;

ALTER TABLE audit_events ADD CONSTRAINT audit_events_kind_chk CHECK (
    kind IN (
        'login_succeeded',
        'login_failed',
        'logout_succeeded',
        'first_user_created',
        'password_changed',
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
