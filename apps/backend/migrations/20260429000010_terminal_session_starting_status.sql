-- Allow the 'starting' status for terminal sessions.
--
-- The orchestrator creates a session row in `starting` BEFORE any real PTY
-- or SSH channel exists. PTY startup is unimplemented in this slice, so a
-- session created via `POST /api/v1/terminal-sessions` stays in `starting`
-- until an explicit close.
--
-- Not destructive: no existing row uses `starting` (the value is new), so
-- the constraint can be replaced in-place.

ALTER TABLE terminal_sessions
    DROP CONSTRAINT terminal_sessions_status_chk;

ALTER TABLE terminal_sessions
    ADD CONSTRAINT terminal_sessions_status_chk CHECK (
        status IN ('starting', 'active', 'detached', 'closed')
    );
