-- SSH identities: backend-managed credential records.
--
-- An identity is the *credential* (keypair + algorithm metadata). It is NOT
-- bound to a host here — that binding lives in server_profiles, so a single
-- key can be reused across multiple hosts.
--
-- encrypted_private_key holds ciphertext bytes only. The encryption scheme
-- (key derivation, AEAD, KEK rotation) is owned by the vault crate and is
-- intentionally NOT implemented in this migration. A later migration may
-- add nonce / kdf / version columns if we move to envelope encryption.

CREATE TABLE ssh_identities (
    id                      UUID        PRIMARY KEY,
    owner_id                UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name                    TEXT        NOT NULL,
    key_type                TEXT        NOT NULL,
    public_key              BYTEA       NOT NULL,
    encrypted_private_key   BYTEA       NOT NULL,
    fingerprint_sha256      TEXT        NOT NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_used_at            TIMESTAMPTZ,

    CONSTRAINT ssh_identities_key_type_chk CHECK (
        key_type IN ('ed25519', 'rsa', 'ecdsa_p256', 'ecdsa_p384', 'ecdsa_p521')
    )
);

CREATE INDEX ssh_identities_owner_id_idx ON ssh_identities (owner_id);
CREATE UNIQUE INDEX ssh_identities_owner_fingerprint_key
    ON ssh_identities (owner_id, fingerprint_sha256);
