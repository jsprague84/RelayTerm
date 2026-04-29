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
- **terminal_session** (`terminal_sessions`) — long-lived SSH session METADATA only. The live `russh::Channel`, replay ring buffer, libghostty-vt parser state, and PTY descriptors are owned by the orchestrator at runtime and are NEVER persisted. `cols`/`rows` are the last requested PTY size — purely a hint for resume. Status is one of `starting`, `active`, `detached`, `closed`; `starting` is the placeholder set on `POST /api/v1/terminal-sessions` BEFORE a real PTY exists. `active` / `detached` are reserved for the future PTY-bearing slice.
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

### Terminal-session lifecycle contract

A `terminal_session` is a backend-owned runtime object. The metadata row in `terminal_sessions` is the *audit/history surface*; the live state (when it lands — see "Out of scope for this slice" below) lives in the orchestrator's in-memory registry. Postgres never holds a `russh::Channel`, a PTY descriptor, the replay ring buffer, or any terminal output bytes.

**Scope (load-bearing — this slice).** The lifecycle endpoints below create and destroy *metadata* and an in-memory runtime *placeholder*. They do **NOT** open an SSH channel, allocate a PTY, run a shell, execute commands, or stream any terminal data. A successful `POST /api/v1/terminal-sessions` response means a row exists in `terminal_sessions` (status `starting`) and a placeholder is registered in the orchestrator — nothing more. The `message` field on the create response names the stub scope explicitly so the client cannot mistake "row created" for "shell ready."

- **Endpoint**: `POST /api/v1/terminal-sessions`. Request body `{ "server_profile_id": <uuid>, "cols"?: <u16>, "rows"?: <u16> }`. `cols`/`rows` default to `80`/`24` and are clamped to `1..=4096`. The route resolves the (profile, host, identity) trio scoped to the caller's user — any miss collapses to a single `404 not_found` for the `terminal_session` entity so cross-user existence is never leaked. It then verifies the host has at least one *active, trusted, non-revoked* `known_host_entries` row before any session row is written. If no such pin exists, the route returns `409 conflict { entity: "host_key" }` — the client must run `trust-host-key` first. PTY/SSH/auth side effects DO NOT happen.
- **Create response (201)**: `{ id, server_profile_id, status, cols, rows, created_at, last_seen_at, closed_at, message }`. `status` is `starting`. `closed_at` is `null`. `message` is the static string `"session metadata created; PTY startup is not implemented yet"`. The response carries NO key material, NO terminal I/O, NO `owner_id`, NO host-key fingerprint.
- **Endpoint**: `GET /api/v1/terminal-sessions`. Returns the caller's sessions only (any status), ordered by `created_at DESC`. Foreign sessions are NEVER included.
- **Endpoint**: `GET /api/v1/terminal-sessions/:id`. Returns one row scoped to the caller. Foreign-owned ids collapse to a byte-identical 404.
- **Endpoint**: `POST /api/v1/terminal-sessions/:id/close`. Idempotent: closing an already-closed session returns `200 OK` with `already_closed = true` and writes NO additional `closed` event. Closing a foreign-owned id returns the same 404 that a missing id would. On a successful close the row transitions to `status = closed`, `closed_at` is stamped, the `closed` lifecycle event is appended, and the in-memory runtime entry (if any) is dropped.
- **Status semantics**:
  - `starting` — initial state set on creation. PTY allocation is unimplemented in this slice, so a session created today STAYS in `starting` until it's explicitly closed.
  - `active` / `detached` — defined for the future PTY-bearing implementation. Not produced by any endpoint in this slice.
  - `closed` — terminal state stamped on `POST /:id/close`.
- **Lifecycle events** (`session_events`): `created` is appended on a successful create with payload `{ "cols", "rows", "stub": true }`; `closed` is appended on a successful (non-idempotent) close. `attached`, `detached`, `reattached`, `resized`, `replay_started`, and `replay_completed` are reserved for future slices and MUST NOT be written until the corresponding behavior exists.
- **Failure modes**: `400 invalid_input` for cols/rows out of `1..=4096`; `401 unauthorized` when dev-auth is disabled; `404 not_found` for a missing or foreign-owned profile (create) or session (get/close); `409 conflict { entity: "host_key" }` when no trusted pin exists for the profile's host on create; `500 internal_error` for repository/database failures (static body, never echoes SQL). Responses NEVER contain encrypted private-key bytes, plaintext PEM, fingerprints, peer banners, or terminal I/O.
- **Backend restart behavior**: the in-memory runtime registry is NOT durable. On restart, any pre-restart `starting` row is operator-visible as a stale metadata record until it's explicitly closed via `POST /:id/close`. A future recovery policy may sweep these — for now the close route is the single hand-back surface. (The DB row itself survives normally; only the placeholder runtime entry is lost.)
- **What this slice does NOT do**: open an SSH transport, allocate a PTY, request a shell, run a command, stream terminal output, write any session_event other than `created`/`closed`, persist replay-ring or VT-state data, accept user-uploaded private keys, or recover stale `starting` rows on restart. (The WebSocket attach/detach surface, including `attached`/`detached`/`resized` lifecycle events, is described in the next section.) Each remaining capability is a separate, deliberate slice.

### Terminal WebSocket attach/detach contract

After a terminal session row exists (see preceding section), clients open a WebSocket to attach their renderer to that session. This slice implements the **lifecycle skeleton only**: the typed protocol envelope, attachment audit rows, and the events that go with them. There is **no PTY, no SSH channel, no terminal byte streaming**. The contract below is what the client gets to rely on; the byte-streaming slice will extend it without breaking these shapes.

**Scope (load-bearing — this slice).** A successful WebSocket attach attests ONLY to:

1. Dev-auth resolved a [`UserId`] for the request.
2. The addressed `terminal_session` row exists, is owned by the caller, and is not `closed`.
3. A new `terminal_session_attachments` row was written and an in-memory attachment runtime entry is registered with the orchestrator.
4. The session's lifecycle log gained an `attached` event.

It does **NOT** mean a PTY was allocated, an SSH channel was opened, a shell was spawned, or that any terminal bytes will flow. The first server frame on every successful attach is a [`session_attached`](#wire-messages) message whose `message` field carries the static string `"attached to RelayTerm session placeholder; PTY streaming is not implemented yet"` so the client cannot mistake "socket open" for "shell ready."

- **Endpoint**: `GET /api/v1/terminal-sessions/:id/ws`. The route resolves the session scoped to the caller's user; missing or foreign-owned ids collapse to a byte-identical `404 not_found` BEFORE the WebSocket handshake completes (no upgrade is performed). A session in `closed` state is rejected with `409 conflict { entity: "terminal_session" }`. With dev-auth disabled the request short-circuits to `401 unauthorized` at the extractor — the upgrade never runs. `User-Agent` is captured (length-capped to 256 chars) and persisted on the attachment row as `client_info`; `remote_addr` is recorded as `NULL` until `ConnectInfo` is plumbed through the listener.

#### Wire messages

The protocol is JSON-over-WebSocket; binary frames are rejected with `invalid_message`. Message tags (`type`) and `code` strings are wire-stable; new variants/codes append, never renumber.

**Client → server** ([`relayterm_protocol::ClientMsg`]):

| `type`     | Fields                                       | Server reply                                                                                                                                                                                                |
|------------|----------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `ping`     | none                                         | `pong`.                                                                                                                                                                                                     |
| `attach`   | `session_id?`, `last_seen_seq?`, `client_id?` | Rejected with `error { code: "invalid_message" }` — the route already attaches on upgrade, so a redundant attach frame would orphan the first row. `last_seen_seq` is reserved for the future replay slice. |
| `input`    | `data: string`                               | Rejected with `error { code: "pty_not_implemented" }`. The payload is NEVER reflected back, logged, or forwarded. The handler's only side effect is the static error frame.                                 |
| `resize`   | `cols: u16`, `rows: u16`                     | `ack { kind: "resize" }` on success; `error { code: "invalid_input" }` for cols/rows outside `1..=4096`. Updates the runtime hint and appends a `resized` `session_event`.                                  |
| `detach`   | none                                         | `session_detached { session_id, attachment_id }`, then the server initiates a clean WebSocket close. Stamps `detached_at` on the attachment row and appends a `detached` event. Idempotent.                 |
| `close`    | none                                         | `session_closed { session_id }`, then the server initiates a clean WebSocket close. Transitions the session to `closed`, appends a `closed` event, drops live attachments. Idempotent.                      |

**Server → client** ([`relayterm_protocol::ServerMsg`]):

| `type`              | Fields                                                                                                  | Notes                                                                                                                                                          |
|---------------------|---------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `session_attached`  | `session_id`, `attachment_id`, `status: "attached_stub"`, `message: string`                             | First frame on every successful attach. `message` explicitly disclaims PTY readiness — the wire string is pinned in a test.                                    |
| `pong`              | none                                                                                                    | Reply to `ping`.                                                                                                                                               |
| `ack`               | `kind: "resize"`                                                                                        | Acknowledges a state-changing non-data client message. New `kind` variants append.                                                                             |
| `session_detached`  | `session_id`, `attachment_id`                                                                           | Server confirms detach bookkeeping landed.                                                                                                                     |
| `session_closed`    | `session_id`                                                                                            | Server confirms session-close transition landed.                                                                                                               |
| `output`            | `seq: u64`, `data: string`                                                                              | Reserved for the future PTY-bearing slice. Never emitted by this implementation.                                                                               |
| `replay_window_lost`| none                                                                                                    | Reserved for the future replay slice. Never emitted by this implementation.                                                                                    |
| `error`             | `code: ErrorCode`, `message: string`                                                                    | `code` is one of `invalid_message`, `invalid_input`, `pty_not_implemented`, `internal`. `message` is short and static — never echoes input or operator detail. |

#### Lifecycle events written by the WebSocket handler

`session_events` rows are appended at these points:

- `attached` — on successful attach. Payload: `{ "attachment_id", "client_info", "remote_addr", "stub": true }`.
- `detached` — on successful first detach (idempotent on subsequent calls). Payload: `{ "attachment_id", "last_seen_seq" }`. `last_seen_seq` is `null` until the PTY-bearing slice populates it.
- `resized` — on successful resize. Payload: `{ "cols", "rows" }`.
- `closed` — on successful first close (delegated to the existing close path; idempotent on subsequent calls). Payload: `{ "reason": "client_requested" }`.

`reattached`, `replay_started`, and `replay_completed` are still reserved for future slices and MUST NOT be written until the corresponding behavior exists.

#### Handler behavior on socket exit

If the underlying transport drops (client disconnect, network failure, abrupt close) without an explicit `Detach` or `Close` frame, the handler MUST still write the detach bookkeeping (`detached_at` stamp + `detached` lifecycle event) so the audit row reflects reality. The first detach wins — `mark_attachment_detached` is COALESCE-on-`detached_at` so a race between the explicit `Detach` frame and the cleanup tail can't corrupt the original timestamp.

#### Logging and reflection prohibitions

- The raw `Input.data` payload MUST NEVER appear in tracing logs, panic messages, error responses, or any frame the server emits. The protocol's `Debug` impl masks the payload at the type level as a last line of defense.
- An invalid (unparseable) frame MUST NEVER be reflected back in the `error` response or logged at any level above trace. The handler emits a static `"invalid message"` body with no payload echo.
- `error` frames carry a wire-stable `code` plus a short, static, public `message`. SQL fragments, repository errors, ssh peer banners, encrypted-key bytes, and PEM markers are NEVER permitted in any WebSocket frame.

#### Future work (explicit out-of-scope for this slice)

PTY allocation and `russh::Channel` ownership; forwarding `Input` bytes to the SSH peer; emitting `output` frames with monotonic sequence numbers; the replay ring buffer and the `replay_window_lost` recovery path; multi-client (collaborative) attach behavior; backend-restart recovery for `starting` rows; binary frame format; and `ConnectInfo`-based `remote_addr` capture all belong to later, deliberate slices.

### Authenticated SSH credential check contract

After the host key is pinned and trusted (see preceding section), an operator may run an authenticated check to confirm the configured `ssh_identity` actually authenticates against the target. The check is deliberately scoped to "did the credentials work?" — it never opens a PTY, runs a shell, or executes a command, so it cannot be abused to drive arbitrary SSH activity through the API.

**Scope (load-bearing).** A successful auth-check response attests ONLY to:

1. The TCP+SSH transport reached the target.
2. The captured server host key matches an active, trusted, non-revoked `known_host_entries` row for the host.
3. The vault-stored private key decrypted and parsed as a valid OpenSSH key.
4. SSH public-key authentication succeeded against the server for the configured username.

It does **NOT** mean a PTY can be allocated, a shell can be spawned, a command can be executed, or that a long-lived terminal session can be opened. Those are separate, later concerns. The auth-check route holds the connection only long enough to read the auth result and tears it down before returning.

- **Endpoint**: `POST /api/v1/server-profiles/:id/auth-check`. No request body. The route resolves the (profile, host, ssh_identity) trio scoped to the caller's user, decrypts the identity inside the vault, computes the active+trusted+non-revoked accept-pin set from `known_host_entries`, and asks the SSH auth checker to (a) connect, (b) verify the captured host key matches an accept-pin in `check_server_key`, (c) attempt public-key authentication if it does, and (d) disconnect. If the captured key is not in the accept-pin set, the checker returns `Ok(false)` from `check_server_key` and russh tears the transport down BEFORE any client signature is sent. No PTY is requested, no channel is opened, no command is executed.
- **Response (200)**: `{ profile_id, host_id, ssh_identity_id, status, message, checked_at }`. `status` is one of `authentication_succeeded`, `authentication_failed`, `host_key_unknown`, `host_key_changed`, `connection_failed`. `message` is a short, static, human-facing string keyed off `status` that explicitly disclaims PTY/command/shell scope on success. The response carries NO host-key fingerprint, NO public/private key material, NO peer banner, and NO russh error text.
- **Status semantics**:
  - `authentication_succeeded` — the host key matched a trusted pin AND `authenticate_publickey` returned success.
  - `authentication_failed` — the host key matched a trusted pin, but the server rejected the credential for the configured username. Auth was attempted; this is the operator-facing "wrong key / wrong user" diagnostic.
  - `host_key_unknown` — no active, trusted, non-revoked row matches the captured host key. Auth was NOT attempted. Trust the host key first via `trust-host-key`.
  - `host_key_changed` — an active, non-revoked row exists for the same key type with a DIFFERENT fingerprint. Auth was NOT attempted. The pin is NOT auto-overwritten — the operator must explicitly revoke the old pin before re-trusting.
  - `connection_failed` — the SSH transport failed before authentication could complete (TCP refused, timeout, malformed peer). Auth was NOT attempted.
- **Host-key trust precondition (load-bearing)**: the auth checker is given ONLY the active, trusted, non-revoked pins. Anything else triggers `Ok(false)` from `check_server_key` and the connection terminates before auth — guaranteeing no client signature is ever sent to an unverified peer. A revoked-and-reappearing key surfaces as `host_key_unknown`; the route never auto-trusts it.
- **Failure modes** (HTTP error, distinct from typed status responses):
  - `401 unauthorized` when dev-auth is disabled.
  - `404 not_found` for a missing or foreign-owned profile (cross-user 404 is byte-identical to a genuine 404).
  - `500 internal_error` if the decrypted private key fails to parse (data-integrity bug in the vault row).
  - `503 service_unavailable` when the vault is disabled OR the auth-check concurrency cap is saturated. The wire body is the static `service unavailable` string in either case — operator detail (vault flag, semaphore state) is logged but never echoed.
  Auth failure, host-key mismatch, and connection failure are NOT HTTP errors — they are typed `status` outcomes on a 200 response, because they are diagnostic answers the operator UI surfaces directly.
- **Outbound-network safety bounds**: every auth-check is wrapped by two guards owned by `SshAuthCheckService`. (1) A hard outer timeout (default 25s) caps the whole `checker.run` call; if it fires, the response collapses to `connection_failed` so a stuck checker can never hold an HTTP request indefinitely. (2) A process-wide [`tokio::sync::Semaphore`] caps concurrent in-flight auth-checks (default 4); callers past the cap get a 503 with the static `service unavailable` body and may retry. Both bounds are configurable via `SshAuthCheckService::with_limits` so tests can exercise them without burning real wall-clock budget.
- **What this slice does NOT do**: open an interactive session, request a PTY, run a shell, execute a command, persist any session state, exchange any non-auth SSH messages, log decrypted private-key bytes, surface peer banners, or accept user-uploaded private keys. The decrypted PEM lives only inside `SshAuthCheckService::auth_check` and the russh checker call, and is wiped from memory when the request struct drops.

## Behavior contracts

- **Reconnect replay**: when a client reconnects with `(session_id, last_seen_seq)`, the backend MUST send all events with `seq > last_seen_seq` from the ring buffer in order, then resume live streaming. If `last_seen_seq` is older than the ring buffer's tail, the backend returns a `replay_window_lost` error and the client must request a full re-render or close the session. **Status (this slice):** the WebSocket protocol carries `last_seen_seq` on `attach` already, but the backend does not yet persist a ring buffer to replay from — the `output` and `replay_window_lost` frames are reserved for a later slice and MUST NOT be emitted until that work lands.
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
