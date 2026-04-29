# relayterm-db

Postgres connection pool and SQLx-backed repository implementations for the
contracts in `relayterm-core::repository`.

## Running the integration tests

The tests under `tests/repositories.rs` are gated behind the
`postgres-tests` Cargo feature so default `cargo test --workspace` stays
runnable without a database.

To execute them against a real Postgres:

```bash
# 1. Bring up the bundled Postgres service.
docker compose -f deploy/docker-compose.yml up -d postgres

# 2. Run the gated tests.
DATABASE_URL=postgres://relayterm:relayterm@127.0.0.1:5432/relayterm \
  cargo test -p relayterm-db --features postgres-tests
```

`#[sqlx::test(migrations = "../../apps/backend/migrations")]` provisions a
fresh per-test database and applies every migration before the test body
runs. The `DATABASE_URL` user must therefore have `CREATEDB` privileges —
the bundled Compose user does.

When you're done:

```bash
docker compose -f deploy/docker-compose.yml stop postgres
```

## Why the runtime SQLx API instead of `query!` / `query_as!`?

Runtime `sqlx::query` / `sqlx::query_as::<_, RowType>` does not require a
`DATABASE_URL` or a populated `.sqlx/` offline cache to compile, so
`cargo check` and `cargo build` work in any environment. Once the
integration tests are wired into CI, hot queries can migrate to the macros
and `cargo sqlx prepare --workspace` can populate `.sqlx/`.
