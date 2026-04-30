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

### Frontend terminal-core contract

The browser/Tauri client never talks to the WebSocket directly — it goes through the `@relayterm/terminal-core` package. That package owns the wire protocol, the transport, and the per-attachment state machine. **Renderers (xterm.js, libghostty-vt, restty, wterm) plug in through a renderer-neutral interface; none are dependencies of the core.** This load-bearing separation is what lets a renderer swap (or a future native Tauri renderer) drop in without changing protocol or state-machine code.

**Scope (load-bearing — this slice).** A successful `terminal-core` integration attests ONLY to the typed protocol envelope, transport lifecycle, and a renderer-neutral plug interface. It does **NOT** include replay-buffer behavior, real PTY byte streaming on the wire, or any reconnect policy beyond the explicit `attach`/`detach` lifecycle. (The xterm.js baseline renderer adapter — `@relayterm/terminal-xterm` — landed as a separate slice; see "xterm.js baseline renderer adapter" below.) Each remaining capability is a separate, deliberate slice.

#### Package layout

`packages/terminal-core/` exports four orthogonal layers — protocol, transport, renderer interface, session client — from one barrel `index.ts`. No file in this package may import an xterm/ghostty/restty/wterm package; if a future component needs renderer-specific behavior it lives in `packages/terminal-<name>/` and consumes `@relayterm/terminal-core` as a peer.

| Module       | Owns                                                                                          |
|--------------|-----------------------------------------------------------------------------------------------|
| `protocol`   | TS mirrors of `relayterm_protocol::{ClientMsg, ServerMsg, ErrorCode, AckKind, SessionAttachStatus}` plus a non-throwing `decodeServerMsg` that returns `{ok, message}` or `{ok:false, failure}`. |
| `transport`  | `TerminalTransport` interface and `WebSocketTerminalTransport` impl. Encodes outbound frames, decodes inbound text frames, surfaces close/error events. |
| `renderer`   | `TerminalRenderer` interface (mount/write/focus/resize/dispose/onInput) and the renderer-neutral `TerminalPreferences` placeholder type. |
| `client`     | `TerminalSessionClient` — the lifecycle state machine that ties transport + protocol together and exposes typed events to UI/renderer code. |
| `events`     | Internal `TypedEmitter` used by transport and client. Listener errors are swallowed so a misbehaving listener can't break the dispatch loop. |

#### `TerminalSessionClient` state machine

States and the legal transitions out of each:

- `idle` — initial. Calling `attach()` moves to `connecting`.
- `connecting` — WebSocket is opening; the wire `attach` frame will be sent the moment `connect()` resolves. The first server frame MUST be `session_attached`; anything else collapses to `error`. A transport close in this state collapses to `error`.
- `attached` — happy path. `ping`/`resize`/`input`/`detach`/`close` are all accepted; the server's typed responses fan out to the matching events.
- `detached` — terminal-on-this-attachment state, reached by either a server `session_detached` frame OR a transport close on an attached client (the backend always writes the detach bookkeeping on socket exit, so a transport-close-after-attach is treated as a clean detach by the client).
- `closed` — terminal-on-this-session state, reached by a server `session_closed` frame.
- `error` — terminal state for protocol or transport failures during attach. The client is dead; create a new instance to retry.

**Send guards (load-bearing):**
- `sendInput` / `sendResize` / `sendPing` / `detach` / `close` are all REJECTED before `attached` and after a terminal state. Resize is **not queued** — the simpler and safer policy is to reject and let the renderer re-fire on the `attached` event. Rejection emits `input_rejected_or_stubbed` with a stable `reason` (`not_attached` | `pty_not_implemented` | `after_terminal_state`) plus the `attempted` action tag (`input` | `resize` | `ping` | `detach` | `close`). The raw payload of any rejected `input` call is NEVER reflected into events, errors, or logs.
- The backend's stub `pty_not_implemented` error frame is translated into the same `input_rejected_or_stubbed` event (`reason: "pty_not_implemented"`, `attempted: "input"`) so callers don't have to special-case server vs client rejection.

**Typed events emitted by `TerminalSessionClient`:**

| Event                          | Payload                          | When                                                                                  |
|--------------------------------|----------------------------------|---------------------------------------------------------------------------------------|
| `state_change`                 | `TerminalSessionState`            | Every state transition.                                                                |
| `attached`                     | `SessionAttachedMsg`              | First server frame after a successful upgrade.                                         |
| `detached`                     | `SessionDetachedMsg`              | Server confirmed detach OR transport closed after attach.                              |
| `closed`                       | `SessionClosedMsg`                | Server confirmed session-close transition.                                             |
| `ack`                          | `AckMsg`                          | Generic ack (`kind: "resize"` today).                                                  |
| `resize_ack`                   | `AckMsg`                          | Convenience event fired in addition to `ack` when `kind === "resize"`.                 |
| `pong`                         | `PongMsg`                         | Reply to `ping`.                                                                       |
| `output`                       | `OutputMsg`                       | Reserved for the future PTY slice; never emitted today.                                |
| `replay_window_lost`           | `ReplayWindowLostMsg`             | Reserved for the future replay slice; never emitted today.                             |
| `error`                        | `TerminalClientError`             | Transport, decode, unexpected-first-frame, server `error`, or send-while-not-attached. |
| `input_rejected_or_stubbed`    | `{reason, attempted}`             | See "Send guards" above; never includes payload bytes.                                 |

#### `TerminalRenderer` interface

A renderer is a class implementing `mount(element)`, `write(data)`, `focus()`, `resize(cols, rows)`, `dispose()`, and `onInput(cb)` — plus an optional `onResize(cb)` for renderers that own their own cell-grid measurement. The interface is deliberately minimal:

- `write(data)` accepts `string | Uint8Array` so a future binary-frame slice doesn't have to widen the type.
- The renderer never sees the protocol. The session client decides what bytes to call `write()` with; today that is nothing (no PTY).
- `dispose()` MUST release every listener and DOM/WebGL resource. Renderers MUST NOT carry state across a `dispose`/`mount` cycle — the orchestrator owns reconnect/replay.
- Renderer-specific config (xterm option names, ghostty-vt parser flags) does NOT belong in this interface. It lives in the adapter package's own constructor / config surface. The renderer-neutral `TerminalPreferences` type in `renderer.ts` (font, theme, cursor style, scrollback) is reserved for a future preferences slice and is intentionally lowest-common-denominator.

#### Renderer-neutral rule

Across `packages/terminal-core/`:
- No file imports xterm.js, libghostty-vt, restty, wterm, or any concrete drawing library.
- No interface uses xterm-specific option names or DOM/canvas/WebGPU-specific shapes.
- The protocol stays RelayTerm-shaped, never xterm-shaped.

A renderer adapter (xterm, ghostty-web, restty, wterm) is a separate package under `packages/terminal-<name>/` that imports `@relayterm/terminal-core`, NEVER the other way around. Adding a new renderer is an architectural surface — see AGENTS.md "When unsure" — propose before adding.

#### Diagnostic UI

`apps/web/src/lib/dev/TerminalProtocolLab.svelte` is a developer-only page that drives the client against a real backend WebSocket. It is NOT the production terminal UI; it has no renderer, and its log deliberately does NOT echo the bytes of any `input` frame the user sends (the diagnostic UI follows the same redaction rule as the protocol layer).

#### Future work (explicit out-of-scope for this slice)

ghostty-web / restty / wterm renderer adapters; real PTY byte streaming through `output` frames; replay-buffer integration on reconnect; auth handshake on the WebSocket beyond dev-auth; per-renderer preference persistence; mobile/Tauri shell integration of the lab UI. Each is a separate, deliberate slice.

### xterm.js baseline renderer adapter

`@relayterm/terminal-xterm` is the first concrete `TerminalRenderer` implementation. xterm.js is the **compatibility baseline**, not the architecture: the protocol stays RelayTerm-shaped, the session client never sees xterm types, and the adapter is one of N planned renderers (ghostty-web, restty, wterm, future native/Tauri).

**Scope (load-bearing — this slice).** A successful integration attests ONLY to the renderer interface bridging xterm.js bidirectionally — `mount`/`write`/`focus`/`resize`/`dispose`/`onInput`/`onResize` all flow through xterm cleanly. It does **NOT** mean PTY bytes stream end-to-end (the backend still rejects `input` with `pty_not_implemented`), it does **NOT** include the replay buffer, and the production terminal UI is still not implemented — only a dev-only renderer lab consumes the adapter today.

#### Package layout

`packages/terminal-xterm/` is a workspace package alongside `terminal-core`. Its only neighbors today are the protocol/client core; future renderers live as siblings. Keys:

- `src/XtermRenderer.ts` — the only file in the repo that imports `@xterm/xterm`. Implements `TerminalRenderer` from `terminal-core` and exposes a `fit()` helper for callers that own the container.
- `src/options.ts` — renderer-neutral `XtermRendererOptions` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`) and the `RendererTheme` shape (background/foreground/cursor/selectionBackground + 16 named ANSI slots). A `xtermOnly` escape hatch passes raw `ITerminalOptions` through and is documented as **non-portable**.
- `src/styles.ts` — side-effect entry that imports `@xterm/xterm/css/xterm.css`. Split out of `index.ts` so Node consumers (vitest) can import the renderer without bundler help. Browser consumers do `import "@relayterm/terminal-xterm/styles"` once at app boot.
- `package.json` declares `"sideEffects": ["./src/styles.ts", "**/*.css"]` so Rollup tree-shakes unused JS in non-dev builds while preserving the styles side-effect import for callers that explicitly want it.

#### Adapter contract

- `XtermRenderer` is the **only** xterm.js consumer in the repo. `terminal-core` does not depend on `@xterm/xterm`. `apps/web` depends on `@relayterm/terminal-xterm` (workspace) — never directly on `@xterm/xterm`.
- Constructor takes `XtermRendererOptions` only; the underlying `Terminal` instance is private.
- `mount` is allowed exactly once per renderer instance. Re-mount throws — silent re-attach would mask a misuse. Calls to `write` before `mount` are queued and flushed on mount; calls to `write` after `dispose` are silent no-ops.
- `dispose` is idempotent and tears down the Terminal, addons (FitAddon, WebLinksAddon), the `onData`/`onResize` subscriptions, and the listener sets in one shot.
- A throwing user listener inside `onInput` is caught and dropped — it MUST NOT interrupt sibling listeners or surface the input bytes through the error envelope (the redaction rule is enforced inside the adapter and re-asserted by tests in `tests/xtermRenderer.test.ts`).

#### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `@xterm/*` and the protocol stays RelayTerm-shaped, never xterm-shaped.
- `XtermRendererOptions` is the **first** concrete shape future renderer adapters are expected to honor 1:1 for the portable knobs. Renderer-only escape-hatch fields (the `xtermOnly` block) are explicitly NOT promised to behave the same on a future adapter.

#### Diagnostic UI

The dev-only live-terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — is the manual exercise surface for the renderer adapter; see "Live SSH PTY bridge contract → Diagnostic UI" below for its contract. The xterm baseline renderer adapter has no separate dev lab — the protocol-only `TerminalProtocolLab` covers the renderer-less wire path, and the live-terminal lab covers the renderer-bridged path. Both labs gate on `import.meta.env.DEV`; the production bundle drops the JS via Rollup tree-shaking (JS bundle is ~28KB without the labs vs. ~322KB with the renderer eagerly included before the `sideEffects` marker landed).

#### Future work (explicit out-of-scope for this slice)

Real PTY byte streaming through `output` frames; ghostty-web / restty / wterm renderer adapters; renderer benchmarking harness; persistent per-renderer preferences; production terminal UI; renderer-swap UX; mobile/Tauri shell integration. Each is a separate, deliberate slice.

### Live SSH PTY bridge contract

After the host key is pinned and trusted (preceding section), an operator may open a `terminal_session` that is backed by a **live SSH PTY**. The create flow does the metadata write AND starts the PTY in one shot; if any precondition fails the row is transitioned to `closed` with a `closed { reason: ssh_start_failed, category }` event.

**Scope (load-bearing — this slice).** A successful create + attach attests ONLY to:

1. The (server_profile, host, ssh_identity) trio resolves and is owned by the caller.
2. The host has at least one active, trusted, non-revoked `known_host_entries` row.
3. The vault decrypted the identity's `encrypted_private_key` to a valid OpenSSH PEM.
4. The SSH transport reached the target, the captured host key matched an accept-pin in `check_server_key`, public-key authentication succeeded, an interactive PTY was allocated, and the user's default login shell started.
5. WebSocket attachments stream raw PTY bytes (base64-encoded inside the JSON `output` frame) from the remote shell, and forward `input`/`resize` to the SSH PTY.

It does **NOT** yet provide:

- Replay or resume across reconnects. A client that drops MUST treat its byte stream as truncated; the server's monotonic `seq` exists for the future replay slice but no ring buffer is persisted yet.
- Multi-client collaborative attach. Today the manager registers fan-out via a `tokio::sync::broadcast` channel, but only one WS attachment per session is exercised by the API tests.
- Backend-restart recovery for live sessions. A restart drops the in-memory runtime registry; metadata rows survive but their PTYs are gone — the operator must explicitly close orphaned rows.
- A binary frame format. Output is base64 inside JSON; a binary slice is future work.

#### Endpoints

- **Endpoint**: `POST /api/v1/terminal-sessions`. Request body unchanged from the metadata-only slice (`{ "server_profile_id", "cols"?, "rows"? }`, dims clamped to `1..=4096`). The route resolves the trio scoped to the caller, refuses with `409 conflict { entity: "host_key" }` if no trusted pin exists, decrypts the identity inside the vault (`503 service_unavailable` if the vault is disabled), writes the `terminal_sessions` row in `starting`, then hands the decrypted PEM and the active accept-pin set to the SSH PTY bridge. On success the row is transitioned to `active` and a live runtime entry is bound to the manager. On failure the row is transitioned to `closed` and the typed error is returned.
- **Create response (201)**: `{ id, server_profile_id, status, cols, rows, created_at, last_seen_at, closed_at, message, pty_live }`. `status` is `active` on a live response. `message` is the static string `"ssh pty started; replay across reconnects is not yet implemented"`. `pty_live` is `true`. The response carries NO host-key fingerprint, NO key material, NO peer banner, NO `owner_id`.
- **Endpoint**: `GET /api/v1/terminal-sessions/:id/ws`. Behavior unchanged at the lifecycle layer (same pre-upgrade ownership / closed-session gating). When a live PTY is bound:
  - The first server frame is `session_attached` with `status: "active"` and a static `message: "attached to live RelayTerm session; replay across reconnects is not yet implemented"`.
  - Server emits `output { seq, data }` frames where `data` is base64-encoded raw PTY bytes (renderer-neutral; the renderer decodes via `output_data_decode` / `decodeOutputData`).
  - Client `input { data }` frames forward the UTF-8 string bytes to the remote PTY's stdin. The payload is NEVER reflected back, logged, or echoed.
  - Client `resize { cols, rows }` frames apply both the metadata-only resize event and a `window_change` on the SSH channel.
- The legacy stub status (`attached_stub`) is still emitted for sessions that have no live PTY (e.g. a row whose runtime was lost across a restart).

#### Wire-stable error codes added

- `pty_not_live` — input/resize attempted on a session whose live runtime is not present (startup failed, PTY exited, or row was created without a bridge).
- `ssh_start_failed` — surfaced over the WebSocket if a live PTY tears down mid-session and the manager surfaces an SSH bridge error.
- `pty_not_implemented` — retained as a legacy code so existing clients keep decoding; new deployments emit `pty_not_live` instead.

#### Lifecycle event behaviour

The bridge slice does **not** introduce any new `SessionEventKind` and does **not** write `replay_started` on PTY start (the existing skeleton SPEC forbids that until the replay buffer lands). The audit trail for a live session is:

- `created` on row insert (existing).
- `attached` on each successful WS attach (existing).
- `resized` on each successful resize (existing).
- `detached` on each clean detach or socket-drop cleanup (existing).
- `closed` on row close, with `reason` distinguishing `client_requested` (user/operator close), `pty_teardown` + `category` (remote shell exit, transport error, local close), and `ssh_start_failed` + `category` (create-time bridge failure).

A precise `live_pty_started` event variant (and matching migration to the `session_events_kind_chk` CHECK constraint) is future work.

#### HTTP error mapping for create-time failures

| Bridge outcome | API status | Closed event `category` |
|---|---|---|
| `InvalidIdentity` | 500 (static body) | `invalid_identity` |
| `Transport(_)` | 502 `bad_gateway` | `transport` |
| `HostKeyNotTrusted` | 409 `host_key` | `host_key_not_trusted` |
| `AuthenticationFailed` | 409 `ssh_auth` | `authentication_failed` |
| `PtyStartFailed` (channel/pty/shell) | 502 `bad_gateway` | `pty_alloc` |
| Vault disabled | 503 (static body) | n/a — refused before bridge call |

The wire body for 4xx/5xx is always the static `code/message` envelope; peer banners, russh error text, SQL fragments, encrypted blobs, and PEM markers NEVER leak. Decrypted private-key bytes live only inside `SshPtyTarget` and the russh internal parse — both wipe on drop.

#### Detach / close semantics for this slice (load-bearing)

The current live-PTY persistence policy is **conservative**: until a replay buffer + TTL/reaper for detached live sessions exists, RelayTerm MUST NOT leave a live SSH PTY running with zero attached clients. This rule is what keeps the slice safe to ship without a background sweep job — every PTY has a deterministic teardown owner.

The orchestrator's [`TerminalSessionManager::detach_attachment`] is the single lifecycle entry point for any detach (explicit `Detach` frame or socket-drop cleanup tail). Its policy:

1. Detach the attachment first — `detach_session` is COALESCE-on-`detached_at` so the first call wins on the row, the `detached` event fires exactly once, and the runtime entry is removed.
2. **If this was the last attachment of a live PTY, also close the session.** The manager calls `close_session`, which transitions the row to `closed`, writes the `closed` event, and via `LiveRuntime`'s `Drop` aborts both the orchestrator's forwarder task and the SSH bridge's driver task.
3. If the session has no live PTY (stub session, or PTY already torn down by the forwarder when the remote shell exited) the manager does NOT auto-close.
4. If other attachments remain (multi-client read attach is future work, but the registry is shaped for it) the manager does NOT auto-close.
5. If the detach observed `already_detached == true`, the manager does NOT auto-close — that's the path that runs when an explicit `Detach` frame and the WebSocket cleanup tail both fire. `close_session` is itself idempotent, but the early skip guarantees a single `Closed` event under the race.

Wire-side behaviour:

- Client `Detach` on the last attachment of a live session: server emits `SessionDetached`, then `SessionClosed`, then closes the WebSocket. The client's state machine MUST treat this sequence as terminal — neither `Detach` nor `Close` will produce any further frames.
- Client `Close`: server emits `SessionClosed`, closes the WebSocket. Idempotent at the route layer (`POST /:id/close` returns `already_closed = true` on a second call) and at the manager.
- Socket drop without an explicit `Detach`/`Close`: the cleanup tail fires `detach_attachment`, which writes the `detached` event AND auto-closes if this was the last live attachment. A reattach to the auto-closed session id returns `409 conflict { entity: "terminal_session" }` from the upgrade gate.
- Race coverage: explicit `Detach` followed by socket-drop cleanup tail writes exactly one `Detached` and exactly one `Closed` event. Duplicate calls to `close_session` from any source still write exactly one `Closed` event.

Future work (still explicit out-of-scope): a replay ring buffer + sequence-number-based resume, plus a TTL/reaper that lets a detached live PTY linger for a bounded window, will replace this conservative policy with the SPEC's original "detached PTY survives until inactivity timeout" contract. The ring buffer is the load-bearing precondition — without it, a reconnecting client cannot pick up where it left off.

#### Logging and reflection prohibitions (re-affirmed)

- Raw `input` payloads MUST NEVER appear in logs, panic messages, error responses, or any server-emitted frame. Both the protocol's `Debug` impl and the SSH bridge's `Debug` impl mask these bytes at the type level.
- Raw `output` PTY bytes MUST NEVER appear in logs at any level. The bridge → orchestrator → fanout path forwards `Vec<u8>` end-to-end with no `Debug`/`Display` rendering of the payload.
- `error` frames carry only the typed wire-stable `code` plus a short, static, public `message`.

#### Diagnostic UI

`apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` is the dev-only end-to-end lab for a live SSH PTY session. It is NOT the production terminal UI — it exists to manually exercise the data path so the production UI slice can build on a validated seam. Contracts:

- Gated behind `import.meta.env.DEV`. The production bundle drops the JS branch via Vite dead-code elimination; only the xterm.css side-effect (~3KB) lands in the prod CSS bundle as the documented compromise.
- Renderer-neutral: the lab consumes `XtermRenderer` only through the `TerminalRenderer` adapter from `@relayterm/terminal-xterm`. No file under `apps/web/` imports `@xterm/xterm` directly.
- Output decode is centralised in `@relayterm/terminal-core` via `decodeOutputData`; the lab wraps it with `safeDecodeOutput` so a malformed base64 payload collapses to a typed log line WITHOUT echoing the payload. Tests in `apps/web/tests/labLog.test.ts` and `packages/terminal-core/tests/protocol.test.ts` pin the rule.
- Input redaction: the diagnostic event log NEVER carries raw input bytes. The log line is `input sent <redacted>, bytes=N`, where `N` is the UTF-8 byte length computed via `inputByteLength` (matching what the wire frame would carry — JS string `.length` would disagree on non-ASCII). The redaction helper's signature deliberately does not accept the payload at all.
- Output redaction: the log line for an inbound `output` frame is `output seq=S, bytes=N` only — the rendered bytes go to the terminal grid (that is the whole point), but the diagnostic log never carries them.
- Cell-grid validation: cols/rows are clamped to `1..=4096` client-side via `validateCellGrid` before any resize frame is sent; the backend's `invalid_input` rejection is a defense-in-depth.
- Resize is driven through the renderer: a manual "apply resize" button calls `renderer.resize(cols, rows)`, xterm fires `onResize` synchronously, and the subscriber is the single place that calls `client.sendResize` — no duplicate wire frames.

#### Dev workbench launcher

`apps/web/src/lib/dev/DevTerminalWorkbench.svelte` pairs `POST /api/v1/terminal-sessions` with the existing live-terminal lab so an operator can go from "no session" to "live PTY rendered in xterm" without leaving the browser. It is also strictly diagnostic — the production terminal UI (host/profile picker, polished workspace, replay-aware reconnect) is a separate, deliberate slice and does NOT live here.

- Gated behind `import.meta.env.DEV` at the call site (`App.svelte`). The launcher and the bare lab share the same dead-code-elimination story: the prod JS bundle is unchanged in size when this component is added.
- Manual `server_profile_id` entry only. Listing or filtering host/profile rows is **not** implemented in this slice — the launcher refuses to expand into CRUD UI. The backend's `404 not_found` collapse for foreign-owned ids is the only access check the operator sees.
- Validation runs client-side via `validateCreateRequest` (`apps/web/src/lib/api/terminalSessions.ts`) BEFORE any wire round-trip. Cols/rows are clamped to `1..=4096` and `server_profile_id` must be non-empty; the backend's `invalid_input` is defense-in-depth.
- The typed helper `createTerminalSession` issues the POST and parses the response into a small typed shape. Unknown fields in the response are ignored (forward-compat); a missing required field collapses to `malformed_response`. The helper's status summary is `code/status` only — the wire `message` field is never echoed into the launcher's status line, and operator-facing detail (already redacted to static strings server-side) is dropped at the helper boundary as defense-in-depth.
- On a successful create the launcher remounts the lab via `{#key launchId}` with `initialSessionId` / `initialCols` / `initialRows` / `autoConnect=true`. Each create yields a fresh `TerminalSessionClient` + `XtermRenderer` pair — no client/renderer state survives a relaunch. The lab still owns its own session id state after first render so the operator can edit and reconnect manually.
- Status states: `idle` (no session created yet), `creating` (POST in flight, create button disabled), `created { session, launchId }` (lab auto-attaches), `error { summary }` (typed safe summary, lab is unmounted to its baseline form). `clear status` returns to `idle`; the lab continues to expose its own `dispose renderer + client` for live cleanup.
- Tests in `apps/web/tests/terminalSessionsApi.test.ts` pin the helper's validation, request shaping, response parsing, and error mapping. The redaction sentinel pattern from `labLog.test.ts` is reused — no operator-facing string surfaces a wire message field.

#### Future work (explicit out-of-scope for this slice)

Replay buffer + sequence-number-based resume across reconnects; multi-client collaborative attach UX; binary frame format for `Output`; backend-restart recovery for `active` rows; per-session inactivity-timeout reaping for detached PTYs; password-bootstrap / `ssh-copy-id` flow; user-uploaded private keys; SFTP / file-browser surface; session recording; production terminal UI (host/profile picker, polished workspace, theme/preferences persistence); listing / filtering existing sessions in the launcher. Each is a separate, deliberate slice.

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
- libghostty-vt state engine swap (planned; xterm.js drives the baseline). The xterm.js baseline adapter (`@relayterm/terminal-xterm`) has landed; ghostty-web / restty / wterm are future siblings under `packages/terminal-<name>/`.

## Open questions

TODO — known ambiguities for the owner to resolve. Each: question, options considered, current default if any.

- Replay buffer policy: fixed bytes vs fixed events vs time-window? Default: TODO.
- How long does a `detached` session linger before auto-close? Default: TODO.
- Should the renderer choice be per-session or per-device? Default: per-device.

---

> When implementation diverges from this spec, run `/agents spec-sync` to surface the drift. Don't update SPEC.md without intent — the spec leading code is the point.
