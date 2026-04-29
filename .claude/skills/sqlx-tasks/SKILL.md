---
name: sqlx-tasks
description: Situational guidance for sqlx 0.8.x with PostgreSQL — load when editing backend DB code or migrations. Covers query!/query_as!, .sqlx/ offline cache, migration workflow, and PgPool patterns.
paths: "apps/backend/src/db/**/*.rs,apps/backend/migrations/**"
---

# sqlx (0.8.x, PostgreSQL)

> Auto-loads on `apps/backend/src/db/**` and migration files. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `0.8.x`.** 0.8 made `.sqlx/` the canonical offline cache (replacing the legacy `sqlx-data.json`) and changed default features. RelayTerm uses `runtime-tokio-rustls` — pure-Rust TLS, no OpenSSL.

Recommended features:
```toml
sqlx = { version = "0.8", features = [
    "runtime-tokio-rustls",
    "postgres",
    "macros",
    "migrate",
    "uuid",
    "time",
    "json",
] }
```

## Critical gotchas

### Offline cache

**Don't** look for `sqlx-data.json` (legacy 0.7) — it doesn't exist in 0.8.

**Do** commit the `.sqlx/` *folder* at the repo root. Run `cargo sqlx prepare --workspace` after any schema or `query!`/`query_as!` change. CI builds without `DATABASE_URL` rely on this folder.

### `query!` vs `query_as!`

- `query!("SELECT id, name FROM users WHERE org = $1", org)` returns an anonymous struct with field types inferred from the live schema.
- `query_as!(User, "SELECT id, name FROM users WHERE org = $1", org)` maps to a named struct. The struct must declare each column's Rust type.

For columns the planner thinks may be NULL but you know are non-null (typical with `LEFT JOIN ... COUNT(...)`), force non-null with `as "alias!"`:
```rust
sqlx::query_as!(Summary, r#"
    SELECT u.id, u.name, COUNT(p.id) as "post_count!"
    FROM users u LEFT JOIN posts p ON p.user_id = u.id
    GROUP BY u.id, u.name
"#).fetch_all(pool).await?;
```

### `fetch_one` vs `fetch_optional`

`fetch_one` errors with `RowNotFound` when zero rows come back. Use `fetch_optional` whenever absence is a valid outcome (lookups, "find by id"). Use `fetch_one` only when the query MUST return a row (e.g. `RETURNING` after a successful `INSERT`).

### Transaction lifetimes

Don't pass a `&mut Transaction<'_, Postgres>` deeply through async call stacks — the borrow gets thorny across `.await` and you'll fight the borrow checker. Prefer `&mut *tx` when calling executor APIs, and keep transaction scope short.

## Migrations

```bash
sqlx migrate add -r <name>          # creates an up + down pair
sqlx migrate run                    # apply pending
sqlx migrate revert                 # roll back last
cargo sqlx prepare --workspace      # refresh .sqlx/ for offline mode
```

Migration files live at `apps/backend/migrations/` named `YYYYMMDDHHMMSS_<name>.up.sql` / `.down.sql`. They MUST be additive within a release: add a column, then backfill, then make it NOT NULL — never combine.

## PgPool inside axum

Hold a single `PgPool` in `AppState`; clone is cheap (Arc). Set `max_connections` ≥ peak concurrent session count + a margin for HTTP routes. Don't keep a transaction open while `.await`-ing on user input — long-held transactions block VACUUM and accumulate WAL.

## Default tooling

| Task | Command |
|---|---|
| Type-check | `cargo check -p backend` |
| Test | `cargo test -p backend` |
| Migration add | `sqlx migrate add -r <name>` |
| Migration run | `sqlx migrate run --source apps/backend/migrations` |
| Offline cache refresh | `cargo sqlx prepare --workspace` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
