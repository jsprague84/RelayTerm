-- Partial unique index: at most one `closed` recording marker per
-- (session, seq).
--
-- Backstops the application-level idempotency of the startup
-- reconciliation pass in
-- `crates/relayterm-db/src/repositories/terminal_session.rs::
-- reconcile_orphaned_on_startup`. The pass appends a single
-- `terminal_recording_markers { kind: closed, seq: MAX(seq_end), ... }`
-- row per reconciled session that has chunks. The repository SQL uses
-- `INSERT ... ON CONFLICT DO NOTHING` against this index so a partial
-- earlier run, an operator-written marker at the same seq, or two
-- racing writers all collapse to a single row at the database layer
-- — the idempotency guarantee is a database invariant, not an
-- application convention.
--
-- The index is partial (`WHERE kind = 'closed'`) because:
--   * Other marker kinds legitimately repeat at the same seq:
--     `replay_gap` markers may be emitted multiple times if the writer
--     re-enters backpressure at the same point; `attached` /
--     `detached` / `reattached` describe distinct runtime events that
--     can land at identical seq under the same client.
--   * `closed` by definition is a singular session-end terminator.
--     Two `closed` markers at the same seq are always a writer bug or
--     a duplicate idempotent retry; the index converts both into a
--     no-op.

CREATE UNIQUE INDEX terminal_recording_markers_session_closed_seq_uidx
    ON terminal_recording_markers (terminal_session_id, seq)
    WHERE kind = 'closed';
