-- Hosts: a reachable SSH endpoint owned by a user.
--
-- A host is "where to connect" — hostname, port, default user. Credentials
-- live in ssh_identities; the binding of a host to an identity (with an
-- optional username override and tags) is server_profiles.

CREATE TABLE hosts (
    id                  UUID        PRIMARY KEY,
    owner_id            UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    display_name        TEXT        NOT NULL,
    hostname            TEXT        NOT NULL,
    port                INTEGER     NOT NULL,
    default_username    TEXT        NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT hosts_port_range CHECK (port BETWEEN 1 AND 65535)
);

CREATE INDEX hosts_owner_id_idx ON hosts (owner_id);
