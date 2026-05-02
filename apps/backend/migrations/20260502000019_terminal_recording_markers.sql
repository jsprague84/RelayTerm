-- Terminal recording markers: durable, append-only metadata events for a
-- terminal_session's recording timeline.
--
-- Markers are NOT PTY bytes — they are small JSON metadata blobs that
-- annotate the chunk stream so a player can render correctly (resize at
-- seq=N, attach/detach bookkeeping, recording-only lifecycle close, and
-- gap markers when the writer dropped chunks under backpressure).
--
-- Load-bearing rules (see `docs/terminal-recording.md` Section 5.5):
--
--   * `payload` is JSONB and obeys the spirit of the audit forbidden-
--     substring rule: `client_info`, `remote_addr`, attachment ids,
--     terminal bytes, error text — none of those belong here. Markers
--     carry public-safe metadata only (counts, dimensions, reason codes,
--     enum strings).
--   * `seq` aligns to the live wire's per-session output seq. The 'started'
--     kind allows seq = 0 because it is written before the first Output
--     frame is stamped; every other kind brackets a real Output frame and
--     therefore carries seq >= 1. The CHECK below pins this exactly.
--   * The FK is `ON DELETE RESTRICT` so a session row cannot be deleted
--     while recording rows reference it (matches the chunk table).
--   * `kind` is open-by-design: future kinds add a CHECK-extending
--     migration (never a replace).

CREATE TABLE terminal_recording_markers (
    id                    UUID         PRIMARY KEY,
    terminal_session_id   UUID         NOT NULL
        REFERENCES terminal_sessions(id) ON DELETE RESTRICT,
    kind                  TEXT         NOT NULL,
    seq                   BIGINT       NOT NULL,
    payload               JSONB        NOT NULL DEFAULT '{}'::jsonb,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    CONSTRAINT terminal_recording_markers_seq_chk
        CHECK (seq >= 0),
    CONSTRAINT terminal_recording_markers_kind_chk CHECK (
        kind IN (
            'started',
            'attached',
            'detached',
            'reattached',
            'resized',
            'closed',
            'replay_gap'
        )
    ),
    -- The 'started' marker brackets the moment the recording began,
    -- before the forwarder has stamped the first Output frame. Every
    -- other kind brackets a real Output frame so seq must be >= 1.
    CONSTRAINT terminal_recording_markers_started_seq_chk CHECK (
        kind = 'started' OR seq >= 1
    ),
    -- Defence-in-depth so a non-object JSON value cannot land in payload
    -- (the writer is expected to construct objects field-by-field).
    CONSTRAINT terminal_recording_markers_payload_object_chk
        CHECK (jsonb_typeof(payload) = 'object')
);

CREATE INDEX terminal_recording_markers_session_seq_idx
    ON terminal_recording_markers (terminal_session_id, seq, created_at);
