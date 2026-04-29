-- Terminal session attachments: each historical client attachment to a session.
--
-- One terminal_session may have many attachment rows (detach + reattach
-- creates new rows). last_seen_seq is the last sequence number this
-- attachment acknowledged before detaching, used for resume-replay
-- bookkeeping. The replay ring buffer itself is in-memory only.

CREATE TABLE terminal_session_attachments (
    id              UUID        PRIMARY KEY,
    session_id      UUID        NOT NULL REFERENCES terminal_sessions(id) ON DELETE CASCADE,
    attached_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    detached_at     TIMESTAMPTZ,
    client_info     TEXT,
    remote_addr     TEXT,
    last_seen_seq   BIGINT,

    CONSTRAINT tsa_last_seen_seq_chk CHECK (last_seen_seq IS NULL OR last_seen_seq >= 0)
);

CREATE INDEX tsa_session_id_idx ON terminal_session_attachments (session_id);
CREATE INDEX tsa_session_active_idx
    ON terminal_session_attachments (session_id)
    WHERE detached_at IS NULL;
