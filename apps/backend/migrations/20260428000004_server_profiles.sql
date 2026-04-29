-- Server profiles: the user-facing binding of a host to an SSH identity.
--
-- This is the row a user picks from a "connect to..." list. ssh_identities
-- and hosts are deliberately separate so a single identity can be reused
-- across many hosts.

CREATE TABLE server_profiles (
    id                  UUID        PRIMARY KEY,
    owner_id            UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name                TEXT        NOT NULL,
    host_id             UUID        NOT NULL REFERENCES hosts(id) ON DELETE RESTRICT,
    ssh_identity_id     UUID        NOT NULL REFERENCES ssh_identities(id) ON DELETE RESTRICT,
    username_override   TEXT,
    tags                TEXT[]      NOT NULL DEFAULT ARRAY[]::TEXT[],
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_connected_at   TIMESTAMPTZ
);

CREATE INDEX server_profiles_owner_id_idx ON server_profiles (owner_id);
CREATE INDEX server_profiles_host_id_idx ON server_profiles (host_id);
CREATE INDEX server_profiles_ssh_identity_id_idx ON server_profiles (ssh_identity_id);
CREATE UNIQUE INDEX server_profiles_owner_name_key ON server_profiles (owner_id, name);
