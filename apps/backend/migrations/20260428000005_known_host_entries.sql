-- Known-host entries: pinned host public keys per host.
--
-- Every check_server_key decision in the SSH layer must consult this table.
-- A row exists once a host has been observed; trusted_at is set after the
-- user confirms the fingerprint, revoked_at when the entry is invalidated.

CREATE TABLE known_host_entries (
    id                      UUID        PRIMARY KEY,
    host_id                 UUID        NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    key_type                TEXT        NOT NULL,
    fingerprint_sha256      TEXT        NOT NULL,
    public_key              BYTEA       NOT NULL,
    first_seen_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    trusted_at              TIMESTAMPTZ,
    revoked_at              TIMESTAMPTZ,

    CONSTRAINT known_host_entries_key_type_chk CHECK (
        key_type IN ('ed25519', 'rsa', 'ecdsa_p256', 'ecdsa_p384', 'ecdsa_p521')
    )
);

CREATE INDEX known_host_entries_host_id_idx ON known_host_entries (host_id);
CREATE UNIQUE INDEX known_host_entries_host_fingerprint_key
    ON known_host_entries (host_id, fingerprint_sha256);
