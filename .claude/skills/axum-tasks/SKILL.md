---
name: axum-tasks
description: Situational guidance for axum 0.8.x — load when editing backend Rust files. Covers WebSocket/SSE patterns, graceful shutdown, extractor usage, and integration with russh/tokio/sqlx for the long-lived RelayTerm session orchestrator.
paths: "apps/backend/**/*.rs"
---

# axum (0.8.x)

> Auto-loads on `apps/backend/**/*.rs`. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `0.8.x`.** axum `0.9` is in active development on `main`; the `0.8` line is what crates.io ships. RelayTerm's WebSocket relay path depends on the `0.8` `axum::extract::ws` API.

## Critical gotchas

### Server bootstrap

**Don't** call `app.into_make_service()` for plain routes — `axum::serve(listener, app)` accepts the `Router` directly.

**Do:**
```rust
let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_signal())
    .await?;
```

### WebSocket upgrade

The `ws` feature must be enabled (`axum = { version = "0.8", features = ["ws"] }`). Use the `WebSocketUpgrade` extractor and `on_upgrade(handler)`. Each `Message::Text(...)` carries a `Utf8Bytes` (the `String`-typed variant from older docs is gone) — write `Message::Text("hello".into())`.

### SSE keep-alive

For long-lived SSE under reverse proxies (Traefik, nginx), set a keep-alive — otherwise idle connections get culled:

```rust
Sse::new(stream).keep_alive(
    KeepAlive::new()
        .interval(Duration::from_secs(10))
        .text("keep-alive"),
)
```

### Graceful shutdown

Pair `with_graceful_shutdown(shutdown_signal())` with a `tokio::select!` over `signal::ctrl_c()` and `SignalKind::terminate()`. Without this, SIGTERM from Docker drops in-flight WebSockets immediately.

### State sharing

Use `axum::extract::State<AppState>` for shared `PgPool` / `SessionManager` handles. Don't reach for global statics or `lazy_static` — `State` is the canonical extractor and integrates with `Router::with_state(...)`.

## Integration footguns (RelayTerm-specific)

- **WebSocket → russh handoff** — when an upgraded socket relays bytes to a `russh::Channel`, run the read and write directions on separate tasks (`tokio::spawn`) and join with `tokio::select!`. A single-task duplex loop will deadlock under partial reads.
- **Long-lived per-connection state** — `WebSocket` is `Send + 'static`; capture the `SessionManager` handle by clone, not by reference. State must outlive the connection task.
- **PgPool sizing** — `PgPool::set_max_connections` must exceed the worst-case parallel session count + a margin for HTTP routes. Otherwise concurrent `fetch_one` calls block on the pool semaphore.

## Default tooling

| Task | Command |
|---|---|
| Type-check | `cargo check -p backend --all-targets` |
| Lint | `cargo clippy -p backend --all-targets -- -D warnings` |
| Format | `cargo fmt -p backend` |
| Test | `cargo test -p backend` |
| Dev server | `cargo run -p backend` |
| Production build | `cargo build -p backend --release` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
