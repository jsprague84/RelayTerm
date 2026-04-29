---
name: russh-tasks
description: Situational guidance for russh — load when editing the backend SSH client wrapper. Covers Handler trait, channel/PTY lifecycle, host-key verification, and reconnect strategy for RelayTerm's session orchestrator.
paths: "apps/backend/src/ssh/**/*.rs"
---

# russh

> Auto-loads on `apps/backend/src/ssh/**/*.rs`. Project-wide rules in `AGENTS.md`.

## Version + rationale

**Pinned to current `0.5x` line.** The `Channel` API (`channel.into_stream()`, `request_pty`, `window_change`, `wait()` returning `ChannelMsg`) is what RelayTerm targets. If upgrading, re-validate this skill — older 0.4x docs use a different pattern.

## Critical gotchas

### Host-key verification (security-critical)

**Don't:**
```rust
async fn check_server_key(&mut self, _: &ssh_key::PublicKey) -> Result<bool, _> {
    Ok(true)  // wide-open MITM
}
```

**Do:** look up the host's pinned fingerprint in the `known_host_entries` table; reject (`Ok(false)`) on mismatch and emit an `audit_event`. New hosts go through an explicit user-confirmation flow before being pinned.

### PTY request

```rust
channel.request_pty(true, "xterm-256color", cols, rows, 0, 0, &[]).await?;
channel.request_shell(true).await?;
```

The `want_reply: true` flag matters — without it, server-side errors are silent and the channel just produces no output. Always pass terminal size from the active client; default to 80×24 only when no client is attached.

### Window resize

When the client viewport changes, send `channel.window_change(cols, rows, 0, 0).await`. Skipping this causes redraw glitches inside `vim`/`htop` etc.

### Stdout vs stderr

```rust
match msg {
    ChannelMsg::Data { data } => { /* stdout */ },
    ChannelMsg::ExtendedData { data, ext: 1 } => { /* stderr */ },
    ChannelMsg::Eof | ChannelMsg::Close => break,
    ChannelMsg::ExitStatus { exit_status } => { /* record */ },
    _ => {}
}
```

The `ext: 1` discriminant is stderr per the SSH spec — easy to miss in 2024-era examples that only handle `Data`.

### Channel I/O via streams

For bidirectional copy with the WebSocket relay, use `let stream = channel.into_stream()` (implements `AsyncRead + AsyncWrite`). Or `let (read, write) = channel.split()` for half-duplex.

### Keepalives

Configure `keepalive_interval`, `keepalive_max`, and `inactivity_timeout` in `client::Config` — without them, dead servers leave sessions stuck waiting forever:

```rust
client::Config {
    keepalive_interval: Some(Duration::from_secs(30)),
    keepalive_max: 3,
    inactivity_timeout: Some(Duration::from_secs(300)),
    ..Default::default()
}
```

## Integration footguns

- **Channels are session-bound.** A `Channel` does NOT survive its parent `client::Handle` dropping. On reconnect (whether triggered by client drop or transport failure), reopen on a fresh `Handle`.
- **Don't hold `Channel` across `.await` on shared state.** The orchestrator should own the `Channel` inside a single task and communicate with the rest of the system via `mpsc`/`broadcast` channels.
- **Authentication**: prefer `authenticate_publickey` with backend-issued keys loaded via `load_secret_key`. Never accept user-uploaded private keys at the wire — keys are vault-encrypted and only decrypted inside the SSH session task.

## Default tooling

Inherits from the `axum-tasks` skill: `cargo check`, `cargo clippy`, `cargo test`. There's no separate russh CLI.

<!-- agentic-init: curated above this line -->

## Project-specific patterns

*(no entries yet)*
