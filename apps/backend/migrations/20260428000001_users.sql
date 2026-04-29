-- Users: owner identity rows. Auth credentials (passkeys, password hashes,
-- federated logins) live in tables added by later migrations.

CREATE TABLE users (
    id              UUID        PRIMARY KEY,
    email           TEXT        NOT NULL,
    display_name    TEXT        NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_login_at   TIMESTAMPTZ
);

CREATE UNIQUE INDEX users_email_key ON users (LOWER(email));
