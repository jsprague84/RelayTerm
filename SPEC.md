# SPEC.md

> Product specification for RelayTerm. Defines what the system does, who uses it, and the data + behavior contracts.
> AGENTS.md governs *how* code is written; SPEC.md governs *what* it should do.
>
> Keep this in sync with implementation via the `spec-sync` sub-agent (`/agents spec-sync`).

## Overview

RelayTerm is a web/mobile SSH terminal where SSH sessions live on a Rust backend, the browser/Tauri client only renders, and the terminal state is owned by a session orchestrator that survives client disconnects. Clients can detach and reconnect arbitrarily; on reconnect the backend replays missed output by sequence number. The terminal renderer is intentionally pluggable (xterm.js baseline, plus wterm / ghostty-web / restty experiments) so renderer choice doesn't affect session correctness.

**Primary users:** TODO — who connects and from which devices (desktop browser, Tauri desktop, Tauri Android), and at what frequency.
**Goals:** TODO — top 2-3 outcomes (e.g. "tabs survive flaky mobile networks," "single audited backend issues all SSH credentials," "renderer is swappable per device class").
**Non-goals:** TODO — things this is NOT (e.g. NOT a web-based VS Code; NOT an SSH proxy that exposes raw keys to clients).

## Architectural invariants (load-bearing)

These are normative. Drift from any of these is a spec bug, not an implementation freedom.

1. **Session ownership is on the backend.** The browser/Tauri client never holds the live `russh::Channel` or any private key.
2. **Terminal state ownership is on the orchestrator.** Sequence numbers, replay ring buffer, and (eventually) the libghostty-vt VT state machine live in the backend session crate, not in the renderer.
3. **Renderers are interchangeable.** A renderer is allowed to render and to capture user input. It is NOT allowed to: persist state across disconnects, decide auth, or reorder output.
4. **The client may disappear at any moment.** All correctness invariants must hold across `client_dropped → reconnect → resume_at_sequence_n`.
5. **Backend-issued credentials only.** SSH private keys are generated and stored encrypted on the backend; clients receive nothing more than a session token. Known-hosts checks happen on the backend.

## Data model

> Source of truth: `apps/backend/migrations/`. This section is the human-readable summary; auto-update via `/agents spec-sync` when schema changes.

### Entities

Source of truth is `apps/backend/migrations/` and `crates/relayterm-core/`. Initial set (v1):

- **user** (`users`) — owner identity. Email is the login handle; auth credentials (passkeys / dev-mode password) are layered on top in a later migration.
- **host** (`hosts`) — a reachable SSH endpoint: `display_name`, `hostname`, `port`, `default_username`. A host carries NO credentials.
- **ssh_identity** (`ssh_identities`) — a backend-managed credential record (keypair + algorithm metadata). Bound to a `user`, NOT to a host. `encrypted_private_key` is opaque ciphertext produced by the vault crate (XChaCha20-Poly1305 with a 32-byte master key from typed config); the envelope carries a magic prefix and version byte so future schemes can be introduced without schema churn. Plaintext private bytes never leave the vault and never appear in API responses or logs.
- **server_profile** (`server_profiles`) — the user-facing binding of a `host` to an `ssh_identity`. This is the row a user picks from a "connect to..." list. Carries optional `username_override` and `tags`. Splitting host + identity from this binding lets a single key be reused across many hosts.
- **known_host_entry** (`known_host_entries`) — pinned public key per host. Every `check_server_key` decision in the SSH layer must consult this table; `trusted_at` is set when the user confirms a fingerprint, `revoked_at` when the entry is invalidated.
- **terminal_session** (`terminal_sessions`) — long-lived SSH session METADATA only. The live `russh::Channel`, replay ring buffer, libghostty-vt parser state, and PTY descriptors are owned by the orchestrator at runtime and are NEVER persisted. `cols`/`rows` are the last requested PTY size — purely a hint for resume.
- **terminal_session_attachment** (`terminal_session_attachments`) — one row per historical client attachment. `last_seen_seq` records the last sequence number that attachment acknowledged before detaching, used for resume bookkeeping. The replay buffer itself stays in memory.
- **session_event** (`session_events`) — append-only lifecycle log for a `terminal_session`: `created`, `attached`, `detached`, `reattached`, `resized`, `replay_started`, `replay_completed`, `closed`. NOT a per-output log.
- **audit_event** (`audit_events`) — append-only security log: auth outcomes, key vault access, host-key mismatch, profile/identity mutations, session open/close. `actor_id` is nullable for pre-auth events.

### ER diagram

TODO: Mermaid ER diagram. Update when entities change.

## Surfaces

TODO: list each user-facing surface — web routes, Tauri windows, WebSocket message types, REST endpoints. For each: purpose, auth/role requirements, inputs, outputs, validation rules.

### Credential creation contract

The preferred (and currently only) way to create an `ssh_identity` is for the backend to generate the keypair. The user never sees, transmits, or stores the private key.

- **Endpoint**: `POST /api/v1/ssh-identities`.
- **Request**: `{ "name": <string>, "key_type"?: "ed25519" }`. `key_type` defaults to `ed25519`. Other algorithms parse but return `400 invalid_input` until the vault grows a generator for them.
- **Response (201)**: `{ id, name, key_type, public_key, fingerprint_sha256, created_at, last_used_at }`. `public_key` is an OpenSSH `authorized_keys`-compatible line with the user-supplied `name` baked in as the key comment, ready to install on the target server.
- **Never returned**: `encrypted_private_key`, plaintext private key bytes, vault internals (master key, nonce, version byte), or `owner_id`.
- **At rest**: `encrypted_private_key` is the opaque vault envelope (see entity description above). Plaintext bytes only ever exist inside `VaultService::generate_ssh_identity` and are wiped before the call returns.
- **Failure modes**: `400` for an empty/oversized name or unknown `key_type`; `401` when the dev-auth shim is disabled; `503` `service_unavailable` when the vault has no master key configured. The 503 body is a static string — operator detail is logged but never echoed.
- **Out of scope for this slice**: opening an SSH session with the generated identity, automated public-key installation (`ssh-copy-id`-style password bootstrap), and importing a user-supplied private key. The user installs the returned `public_key` on the target server manually for now.

## Behavior contracts

- **Reconnect replay**: when a client reconnects with `(session_id, last_seen_seq)`, the backend MUST send all events with `seq > last_seen_seq` from the ring buffer in order, then resume live streaming. If `last_seen_seq` is older than the ring buffer's tail, the backend returns a `replay_window_lost` error and the client must request a full re-render or close the session.
- **Renderer swap**: the user MAY change the active renderer for a session at any time. The new renderer subscribes from the current sequence number; no replay is required.
- **Session lifecycle**: a session enters `detached` immediately on client drop, NOT after a timeout. A `detached` session continues to receive PTY output and append to the ring buffer until `inactivity_timeout` or explicit close. Audit log records every state transition.
- **Host-key change**: on `check_server_key` mismatch, the backend rejects the connection, logs an `audit_event`, and surfaces the mismatch to the user; it does NOT silently update the known_hosts entry.
- **Key vault access**: the encrypted private key is decrypted only inside the SSH session task. Decrypted bytes never cross a boundary (no log, no IPC payload, no DB write).

## Integration points

- **PostgreSQL** — primary store for users, sessions, audit log, key vault. sqlx connection pool; `runtime-tokio-rustls`.
- **Vault master key** — 32-byte secret loaded once at boot, supplied via `vault.master_key_b64` (config / `RELAYTERM_VAULT__MASTER_KEY_B64` env) or `vault.master_key_file`. Exactly one source must resolve, or the backend refuses to start. There is no fallback to a randomly generated key — that would orphan all previously stored ciphertext after a restart. Setting `vault.enabled = false` disables backend-generated identities (the POST route returns 503) and lets the rest of the API run.
- **Traefik** — reverse proxy in front of the backend; terminates TLS; routes `/api/*` and `/ws/*`.
- **WireGuard** (optional) — used only when the backend lives on a remote box and SSH targets are reachable only via the WireGuard mesh.
- **Object storage** — TODO if the project ever needs file upload/download via SCP/SFTP. Out of scope for v1.
- **Passkeys / WebAuthn** — TODO future phase; v1 uses opaque session cookies issued after dev-mode password login.

## Out of scope (v1)

TODO — explicit list of features deferred so the agent doesn't "helpfully" implement them. Likely:

- SCP/SFTP file transfer surface.
- Multi-user shared sessions / "screen-share."
- Public-cloud-hosted multi-tenant deployment (v1 is single-tenant Docker Compose).
- iOS Tauri build (Android first; iOS later).
- libghostty-vt state engine swap (planned; xterm.js drives the baseline until then).

## Open questions

TODO — known ambiguities for the owner to resolve. Each: question, options considered, current default if any.

- Replay buffer policy: fixed bytes vs fixed events vs time-window? Default: TODO.
- How long does a `detached` session linger before auto-close? Default: TODO.
- Should the renderer choice be per-session or per-device? Default: per-device.

---

> When implementation diverges from this spec, run `/agents spec-sync` to surface the drift. Don't update SPEC.md without intent — the spec leading code is the point.
