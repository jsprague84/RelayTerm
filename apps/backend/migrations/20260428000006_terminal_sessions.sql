-- Terminal sessions: long-lived SSH session METADATA.
--
-- IMPORTANT: this table stores metadata only. The live russh::Channel,
-- replay ring buffer, libghostty-vt parser state, and PTY descriptors are
-- all owned by the backend orchestrator at runtime and are NEVER persisted
-- here. Postgres is the wrong store for hot terminal state.
--
-- cols/rows are the last requested PTY size and exist purely so a resume
-- can request the same dimensions before live PTY size is re-derived from
-- the renderer.

CREATE TABLE terminal_sessions (
    id                  UUID        PRIMARY KEY,
    owner_id            UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    server_profile_id   UUID        NOT NULL REFERENCES server_profiles(id) ON DELETE RESTRICT,
    status              TEXT        NOT NULL,
    cols                INTEGER     NOT NULL,
    rows                INTEGER     NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    closed_at           TIMESTAMPTZ,

    CONSTRAINT terminal_sessions_status_chk CHECK (
        status IN ('active', 'detached', 'closed')
    ),
    CONSTRAINT terminal_sessions_cols_chk CHECK (cols BETWEEN 1 AND 4096),
    CONSTRAINT terminal_sessions_rows_chk CHECK (rows BETWEEN 1 AND 4096)
);

CREATE INDEX terminal_sessions_owner_id_idx ON terminal_sessions (owner_id);
CREATE INDEX terminal_sessions_server_profile_id_idx
    ON terminal_sessions (server_profile_id);
CREATE INDEX terminal_sessions_status_idx ON terminal_sessions (status);
