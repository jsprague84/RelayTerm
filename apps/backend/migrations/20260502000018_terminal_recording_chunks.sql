-- Terminal recording chunks: durable, append-only output-byte chunks for a
-- terminal_session.
--
-- Each row stores a contiguous run of PTY OUTPUT bytes (the same bytes that
-- ride `Output { seq, data }` on the live wire) for a span of sequence
-- numbers. Recording is the design's "display history" surface — it is NEVER
-- the live wire and NEVER the live PTY.
--
-- Load-bearing rules (see `docs/terminal-recording.md` and SPEC.md
-- "Durable terminal recording and replay architecture"):
--
--   * `payload` is opaque PTY output bytes. After encryption/compression
--     (none in this slice), it MUST NEVER appear in `audit_events.payload`,
--     in any `tracing::*` line, in any panic message, in any HTTP error
--     response body, in any frontend storage, or in any Debug output that
--     formats the bytes themselves. The repository's Rust types redact
--     `payload` to `seq_start..=seq_end + len` only.
--   * The FK is `ON DELETE RESTRICT` on purpose — recording rows are NEVER
--     cascade-deleted with their session row. Retention sweeps (a future
--     slice) are the only path that removes them; that sweep deletes
--     chunk + marker rows but leaves the `terminal_sessions` row in place.
--   * `seq_start`/`seq_end` are inclusive and aligned to the live wire's
--     monotonic per-session output seq (starts at 1; see SPEC.md "Output
--     sequence + in-memory replay buffer contract"). The chunk writer (a
--     future slice) is the sole writer; this schema does not assume one.
--   * `byte_len` is defence-in-depth against a runaway row. The chunk
--     writer's bounded queue is the primary cap; the CHECK is a backstop.
--     The 2 MiB upper bound covers the worst-case 1 MiB single output
--     frame plus envelope overhead from a future encrypted-row scheme
--     (XChaCha20-Poly1305 nonce + tag + magic + version ≈ 41 bytes).
--   * `encryption` and `compression` are TEXT enums. v1 only writes
--     'none' for both. Future slices add 'recording_v1' / 'zstd' via
--     dedicated migrations that EXTEND the CHECK (never replace).

CREATE TABLE terminal_recording_chunks (
    id                    UUID         PRIMARY KEY,
    terminal_session_id   UUID         NOT NULL
        REFERENCES terminal_sessions(id) ON DELETE RESTRICT,
    seq_start             BIGINT       NOT NULL,
    seq_end               BIGINT       NOT NULL,
    byte_len              INTEGER      NOT NULL,
    payload               BYTEA        NOT NULL,
    encryption            TEXT         NOT NULL DEFAULT 'none',
    compression           TEXT         NOT NULL DEFAULT 'none',
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    CONSTRAINT terminal_recording_chunks_seq_start_chk
        CHECK (seq_start >= 1),
    CONSTRAINT terminal_recording_chunks_seq_end_chk
        CHECK (seq_end >= seq_start),
    CONSTRAINT terminal_recording_chunks_byte_len_chk
        CHECK (byte_len > 0 AND byte_len <= 2097152),
    CONSTRAINT terminal_recording_chunks_payload_len_chk
        CHECK (octet_length(payload) = byte_len),
    CONSTRAINT terminal_recording_chunks_encryption_chk
        CHECK (encryption IN ('none')),
    CONSTRAINT terminal_recording_chunks_compression_chk
        CHECK (compression IN ('none')),
    CONSTRAINT terminal_recording_chunks_session_seq_start_uq
        UNIQUE (terminal_session_id, seq_start)
);

-- Index supports `from_seq` paged reads. PostgreSQL serves the unique
-- constraint via its own index, but spell this out so a future migration
-- that drops the unique cannot silently regress the read path.
CREATE INDEX terminal_recording_chunks_session_seq_idx
    ON terminal_recording_chunks (terminal_session_id, seq_start);
