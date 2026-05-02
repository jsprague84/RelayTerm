-- Password credential storage.
--
-- One row per user that has a password set. The plaintext password is
-- never stored — `password_hash` holds an Argon2id PHC string
-- (`$argon2id$...`) produced by the auth service. The PHC string carries
-- the algorithm parameters and the salt; no separate columns are needed
-- for them. A future parameter upgrade verifies the old hash, then
-- rehashes and updates the row on the next successful login.
--
-- ON DELETE CASCADE: a password without a user is unreachable. Cleanup
-- with the owner row is the only sensible behavior.

CREATE TABLE user_passwords (
    user_id              UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    password_hash        TEXT        NOT NULL,
    password_changed_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
