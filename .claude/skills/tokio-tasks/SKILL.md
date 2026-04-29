---
name: tokio-tasks
description: Situational guidance for tokio ^1 — load when editing backend Rust files. Covers cancellation safety, channel selection, blocking work, and structured concurrency patterns critical for long-lived SSH session tasks.
paths: "apps/backend/**/*.rs"
---

# tokio (^1)

> Auto-loads on `apps/backend/**/*.rs`. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to `^1`.** Tokio has been API-stable since 1.0; minor versions add features without breaking. Pin to current `1.x`. Required features for RelayTerm: `["full"]` is fine; if trimming, you need at least `["rt-multi-thread", "macros", "sync", "io-util", "net", "signal", "time", "process"]`.

## Critical gotchas

### Mutex choice

**Don't** reach for `tokio::sync::Mutex` for state that's only touched synchronously between awaits — the std mutex is faster and has lower allocation cost.

**Do** use `tokio::sync::Mutex` *only* when the lock must be held across an `.await` point. Otherwise `std::sync::Mutex` (or `parking_lot::Mutex`) is correct.

### `select!` cancellation safety

Each branch of `tokio::select!` may be dropped mid-execution. Branches must be cancel-safe — they can't leave half-mutated state if dropped. `Receiver::recv()` is cancel-safe; `Sender::send()` is cancel-safe; arbitrary user-defined async fns are usually NOT. Wrap unsafe operations in a non-cancellable helper task and `select!` over its `JoinHandle`.

### Blocking work

**Don't** call CPU-heavy or filesystem-blocking code directly inside `async fn` running on the multi-thread runtime — it stalls a worker thread and degrades latency for every other task.

**Do** wrap with `tokio::task::spawn_blocking(|| { ... }).await?`.

### Channel selection (relevant to session orchestrator)

| Use case | Channel |
|---|---|
| Many producers → one consumer (PTY input multiplex) | `mpsc::channel(N)` (bounded for backpressure) |
| One producer → many consumers (output fan-out to active clients) | `broadcast::channel(N)` |
| Latest-value snapshot (session status, window size) | `watch::channel(initial)` |
| One-shot reply (RPC handoff) | `oneshot::channel()` |

### Structured concurrency

**Don't** scatter `tokio::spawn` calls and forget the `JoinHandle`. A panic in a detached task is silently swallowed.

**Do** use `JoinSet` when spawning a dynamic number of tasks; await `set.join_next()` to surface panics and aggregate results.

## Integration footguns

- **Signal handlers** — `signal::unix::signal(SignalKind::terminate())` is needed for SIGTERM; `ctrl_c()` covers SIGINT. RelayTerm's graceful shutdown uses both.
- **`tokio::process::Command`** — when piping stdin/stdout to a child, drop the writer side after sending input or the child blocks on EOF.

## Default tooling

| Task | Command |
|---|---|
| Type-check | `cargo check --workspace --all-targets` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Test | `cargo test --workspace` |

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
