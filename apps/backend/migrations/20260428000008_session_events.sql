-- Session events: append-only lifecycle log for terminal_sessions.
--
-- These are NOT the per-output replay events — those live in the
-- orchestrator's in-memory ring buffer and never touch Postgres. These rows
-- describe state transitions and operations on a session.

CREATE TABLE session_events (
    id              UUID        PRIMARY KEY,
    session_id      UUID        NOT NULL REFERENCES terminal_sessions(id) ON DELETE CASCADE,
    kind            TEXT        NOT NULL,
    payload         JSONB       NOT NULL DEFAULT '{}'::jsonb,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT session_events_kind_chk CHECK (
        kind IN (
            'created',
            'attached',
            'detached',
            'reattached',
            'resized',
            'replay_started',
            'replay_completed',
            'closed'
        )
    )
);

CREATE INDEX session_events_session_id_idx ON session_events (session_id);
CREATE INDEX session_events_recorded_at_idx ON session_events (recorded_at);
