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

### Host-key preflight + known-host trust contract

Before opening a real SSH session against a `server_profile`, clients run a host-key preflight to capture the server's host key and classify it against the host's pinned `known_host_entries`. The trust workflow is split into two endpoints so the user has an explicit moment to confirm a fingerprint before any private key is risked.

**Scope (load-bearing).** A successful host-key preflight response attests ONLY to host-key-stage reachability classification. It does **NOT** mean SSH authentication succeeded, that the configured identity is installed in `authorized_keys` on the target, or that a PTY/shell can be opened. Wire response wording is deliberately conservative; do not loosen it. Auth-side validation and session-readiness checks are separate, later concerns.

- **Endpoint**: `POST /api/v1/server-profiles/:id/host-key-preflight`. No request body. The route resolves the profile, host, and SSH identity (all scoped to the caller's user), decrypts the identity inside the vault for round-trip validation only, opens an SSH transport to `host:port`, captures the server's public host key during key exchange, and disconnects WITHOUT attempting authentication. No client credentials are ever transmitted to a host whose key has not been pinned.
- **Preflight response (200)**: `{ profile_id, host_id, hostname, port, host_key_status, host_key_type, host_key_fingerprint, message }`. `host_key_status` is one of `unknown`, `trusted`, `changed`. `host_key_fingerprint` is the `SHA256:<base64>` form. `message` is a short, static, human-facing explanation per status that explicitly names the KEX-only scope. The response carries ONLY public host-side data — no decrypted key, no encrypted blob, no vault internals, no russh error text.
- **Status semantics**:
  - `unknown` — no active (non-revoked) `known_host_entries` row matches the captured `(key_type, fingerprint)`. First-time-seen state. Also returned if a row exists but `trusted_at` was never stamped, AND when a row matching the captured fingerprint exists but is revoked (the trust route then refuses to silently re-trust it; see below).
  - `trusted` — an active, non-revoked matching row has `trusted_at` set.
  - `changed` — an active, non-revoked row exists for the same `key_type` with a DIFFERENT fingerprint. This is the MITM signal. The backend never auto-overwrites a `changed` entry; the operator must explicitly revoke the old pin first.
- **Endpoint**: `POST /api/v1/server-profiles/:id/trust-host-key`. Request body `{ "expected_fingerprint": "SHA256:<base64>" }`. The route runs a fresh probe, verifies the captured fingerprint matches the caller's `expected_fingerprint`, AND verifies no revoked row exists for that `(key_type, fingerprint)`, then inserts (or stamps `trusted_at` on) a `known_host_entries` row. If the captured fingerprint differs from `expected_fingerprint`, the classifier returns `changed`, OR a revoked row exists for the captured fingerprint, the route returns `409 conflict` and writes nothing. Re-trusting an already-trusted entry is idempotent — the original `trusted_at` is preserved so audit history doesn't drift.
- **Trust response (200)**: `{ known_host_entry_id, host_id, host_key_type, host_key_fingerprint, trusted_at }`.
- **Revoked-entry behavior (explicit)**:
  - The classifier filters revoked rows out of `trusted`/`changed`. A revoked-and-reappearing key surfaces as `unknown`, NOT `trusted`.
  - The trust route enforces a separate revoked-aware guard: a `(key_type, fingerprint)` that has any revoked row — trusted or not — is refused with `409 conflict`. Two-layer defense: the route check produces a clean response BEFORE any write, and the `record_trusted` SQL also rejects updates to revoked rows via `WHERE revoked_at IS NULL` on the conflict branch (returning a `Conflict` repository error).
  - There is no implicit recovery path for a revoked entry. An explicit unrevoke / restore route is a future, deliberate operator-facing slice.
- **Failure modes**: `400` for a malformed `expected_fingerprint`; `401` when dev-auth is disabled; `404` for a missing or foreign-owned profile (cross-user 404 is byte-identical to a genuine 404); `409` for a host-key mismatch (changed key, stale expected fingerprint, OR revoked-row-for-this-fingerprint); `502 bad_gateway` for any SSH probe failure (unreachable, timeout, transport error, unsupported host-key algorithm) — the wire body is a static `"bad gateway"` string so peer banners and topology never leak; `503 service_unavailable` when the vault is disabled.
- **What this slice does NOT do**: open an interactive session, request a PTY, run a shell, attempt SSH authentication, validate that the configured identity works against the target, or unrevoke/restore a previously revoked known-host entry. The decrypted private key is parsed for round-trip validation only; it is never sent to the peer during the host-key probe. Auth-side verification, persistent session orchestration, reconnect-replay, and revoked-entry recovery all belong to later slices.

## Behavior contracts

- **Reconnect replay**: when a client reconnects with `(session_id, last_seen_seq)`, the backend MUST send all events with `seq > last_seen_seq` from the ring buffer in order, then resume live streaming. If `last_seen_seq` is older than the ring buffer's tail, the backend returns a `replay_window_lost` error and the client must request a full re-render or close the session.
- **Renderer swap**: the user MAY change the active renderer for a session at any time. The new renderer subscribes from the current sequence number; no replay is required.
- **Session lifecycle**: a session enters `detached` immediately on client drop, NOT after a timeout. A `detached` session continues to receive PTY output and append to the ring buffer until `inactivity_timeout` or explicit close. Audit log records every state transition.
- **Host-key change**: on `check_server_key` mismatch, the backend rejects the connection, logs an `audit_event`, and surfaces the mismatch to the user; it does NOT silently update the known_hosts entry. The preflight + trust-host-key endpoints (see "SSH preflight + known-host trust contract") implement this for the pre-session probe; the same rule applies to live sessions once they land.
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
