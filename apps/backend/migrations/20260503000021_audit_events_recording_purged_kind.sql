-- Extend the `audit_events.kind` CHECK constraint with the
-- `recording_purged` kind emitted by the future retention worker.
--
-- `recording_purged` is system-authored: rows carry `actor_id = NULL`
-- (the cleanup worker is not a user) and a public-metadata-only payload
-- (`target_id`, `target_kind = "terminal_session"`, `chunk_count`,
-- `marker_count`, `bytes_purged`, `retention_days`, `closed_at`,
-- `purged_at`, `reason = "retention_expired"`). The payload MUST NEVER
-- carry chunk `payload` bytes (or any base64 form of them), marker
-- payload contents, `client_info`, hostnames, peer banners, raw russh
-- / DB error text, vault internals, session-token bytes, token hashes,
-- password hashes, or bootstrap tokens — see
-- `docs/terminal-recording.md` Section 12.5 for the full redaction list.
--
-- This migration ships ahead of the cleanup worker so the repository
-- purge primitive (slice 8a-prep) can append rows without violating
-- the CHECK. The worker (slice 8a / 8b) is gated separately on a
-- `[terminal_recording.cleanup]` config block that does not exist yet.
--
-- Strict superset of the previous set — no rows are invalidated.

ALTER TABLE audit_events DROP CONSTRAINT audit_events_kind_chk;

ALTER TABLE audit_events ADD CONSTRAINT audit_events_kind_chk CHECK (
    kind IN (
        'login_succeeded',
        'login_failed',
        'logout_succeeded',
        'first_user_created',
        'password_changed',
        'session_revoked',
        'sessions_revoked',
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
        'recording_purged',
        'other'
    )
);
