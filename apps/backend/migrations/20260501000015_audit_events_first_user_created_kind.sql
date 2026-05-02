-- Extend the `audit_events.kind` CHECK constraint with the
-- `first_user_created` kind emitted by the auth bootstrap route.
--
-- `login_succeeded`, `login_failed`, and `logout_succeeded` were already
-- in the constraint set; bootstrap is the only new kind this slice
-- requires. Other future auth kinds (`password_changed`, `session_revoked`)
-- are paired with the routes that emit them — adding them here without an
-- emitter would lock in the wire shape before the spec is exercised.
--
-- Strict superset of the previous set — no rows are invalidated.

ALTER TABLE audit_events DROP CONSTRAINT audit_events_kind_chk;

ALTER TABLE audit_events ADD CONSTRAINT audit_events_kind_chk CHECK (
    kind IN (
        'login_succeeded',
        'login_failed',
        'logout_succeeded',
        'first_user_created',
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
