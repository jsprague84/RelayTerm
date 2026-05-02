-- Server-side opaque browser sessions.
--
-- Each row represents one issued session. The cookie value is a 32-byte
-- random token; only its SHA-256 digest is persisted as `token_hash`.
-- Plaintext tokens MUST NEVER be written to this table or to any log.
--
-- `id` is the stable session identifier referenced by audit-event
-- payloads (`logout_succeeded.session_id`, `session_revoked.revoked_session_id`).
-- It is NOT the cookie value.
--
-- `user_agent` and `remote_addr` are intentionally NOT stored in this
-- slice. The session-list / "active sessions" feature is deferred and
-- those columns can be added by a later migration when the surface that
-- consumes them lands.
--
-- ON DELETE CASCADE: orphan sessions after a user delete would be a
-- logout-bypass.

CREATE TABLE user_sessions (
    id              UUID        PRIMARY KEY,
    user_id         UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash      BYTEA       NOT NULL UNIQUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at      TIMESTAMPTZ NOT NULL,
    revoked_at      TIMESTAMPTZ,
    revoked_reason  TEXT
);

CREATE INDEX user_sessions_user_id_idx ON user_sessions (user_id);
CREATE INDEX user_sessions_expires_at_idx ON user_sessions (expires_at);
