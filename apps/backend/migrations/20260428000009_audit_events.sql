-- Audit events: append-only security-relevant event log.
--
-- Distinct from session_events: this captures actions that may matter for
-- forensics across the whole system — auth, key vault access, host-key
-- mismatches, profile mutations, etc. actor_id is nullable for pre-auth
-- events (e.g., a failed login where the user is not yet identified).

CREATE TABLE audit_events (
    id              UUID        PRIMARY KEY,
    actor_id        UUID                 REFERENCES users(id) ON DELETE SET NULL,
    kind            TEXT        NOT NULL,
    payload         JSONB       NOT NULL DEFAULT '{}'::jsonb,
    remote_addr     TEXT,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT audit_events_kind_chk CHECK (
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
            'server_profile_deleted',
            'ssh_identity_created',
            'ssh_identity_deleted',
            'session_opened',
            'session_closed',
            'other'
        )
    )
);

CREATE INDEX audit_events_actor_id_idx ON audit_events (actor_id);
CREATE INDEX audit_events_kind_idx ON audit_events (kind);
CREATE INDEX audit_events_recorded_at_idx ON audit_events (recorded_at);
