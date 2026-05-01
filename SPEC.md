# SPEC.md

> Product specification for RelayTerm. Defines what the system does, who uses it, and the data + behavior contracts.
> AGENTS.md governs *how* code is written; SPEC.md governs *what* it should do.
>
> Keep this in sync with implementation via the `spec-sync` sub-agent (`/agents spec-sync`).

## Overview

RelayTerm is a web/mobile SSH terminal where SSH sessions live on a Rust backend, the browser/Tauri client only renders, and the terminal state is owned by a session orchestrator that survives client disconnects. Clients can detach and reconnect arbitrarily; on reconnect the backend replays missed output by sequence number. The terminal renderer is intentionally pluggable (xterm.js baseline, plus ghostty-web / restty / wterm experiments) so renderer choice doesn't affect session correctness.

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
- **terminal_session** (`terminal_sessions`) — long-lived SSH session METADATA only. The live `russh::Channel`, replay ring buffer, libghostty-vt parser state, and PTY descriptors are owned by the orchestrator at runtime and are NEVER persisted. `cols`/`rows` are the last requested PTY size — purely a hint for resume. Status is one of `starting`, `active`, `detached`, `closed`; `starting` is the placeholder set on `POST /api/v1/terminal-sessions` BEFORE a real PTY exists. `active` is set when a live PTY runtime is bound; `detached` is set during the bounded `DETACHED_LIVE_PTY_TTL` reconnect window after the last client leaves.
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

The protocol uses **two parallel WebSocket frame shapes**: structured JSON messages for the control plane (attach/detach/resize/replay control, errors, lifecycle), and a small binary envelope (`RTB1`, see "Terminal data plane: binary envelope" below) for the hot terminal data path (`output` and `input`). Both shapes share the same socket; they're distinguished by the WebSocket frame type (text vs binary). Message tags (`type`) and `code` strings on the JSON plane are wire-stable; new variants/codes append, never renumber.

**Client → server** ([`relayterm_protocol::ClientMsg`]):

| `type`     | Fields                                       | Server reply                                                                                                                                                                                                |
|------------|----------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `ping`     | none                                         | `pong`.                                                                                                                                                                                                     |
| `attach`   | `session_id?`, `last_seen_seq?`, `client_id?` | Drives the replay handshake. The first `attach` after upgrade is accepted: with `last_seen_seq` set the server emits `replay_start` → buffered `output` frames → `replay_end` (or a single `replay_window_lost` if the bookmark predates the buffer); without it, the loop continues straight to live fanout. A SECOND explicit `attach` is a protocol violation and is rejected with `error { code: "invalid_message", message: "already attached" }`. See "Output sequence + in-memory replay buffer contract" for the per-message wire shape. |
| `input`    | `data: string`                               | **Legacy fallback.** Carries one PTY input chunk as a UTF-8 string. The default wire shape for input is the binary `Input` frame (see "Terminal data plane: binary envelope"); the JSON form remains accepted by the backend for backwards compatibility and dev/diagnostic flows. Forwarded to the live PTY's stdin when bound; rejected with `error { code: "pty_not_live" }` otherwise. The payload is NEVER reflected back or logged.                                 |
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
| `output`            | `seq: u64`, `data: string`                                                                              | **Legacy fallback only.** The protocol still defines this base64-inside-JSON shape so a debug client (or a JSON-only consumer) can reason about it; the backend currently emits PTY output exclusively as binary `Output` frames (see "Terminal data plane: binary envelope"). `seq` semantics are identical between forms — monotonic per session starting at `1`; replayed and live frames share the same variant. See "Output sequence + in-memory replay buffer contract." |
| `replay_start`      | `from_seq: u64`, `to_seq: u64`                                                                          | Marks the start of a replay handshake. Emitted at most once per attach, BEFORE the first replayed `output` frame. Skipped when the snapshot is empty.          |
| `replay_end`        | `latest_seq: u64`                                                                                       | Marks the end of replay; live `output` frames resume at `latest_seq + 1`.                                                                                       |
| `replay_window_lost`| `requested_seq: u64`, `oldest_available_seq: u64?`, `latest_seq: u64`                                   | The bookmark predates the bounded replay buffer. The handler continues live attach AFTER emitting this — the renderer is expected to reset its grid.            |
| `error`             | `code: ErrorCode`, `message: string`                                                                    | `code` is one of `invalid_message`, `invalid_input`, `pty_not_implemented`, `internal`. `message` is short and static — never echoes input or operator detail. |

#### Terminal data plane: binary envelope

The hot terminal data path — PTY output server→client and renderer keystrokes client→server — flows on **binary** WebSocket frames carrying the RelayTerm v1 envelope (`RTB1`). The control plane stays JSON; only `Output` and `Input` payload bytes ride the binary surface.

**Envelope layout (big-endian, fixed 20-byte header):**

| offset | size | field                                                                          |
|-------:|-----:|--------------------------------------------------------------------------------|
|     0  |   4  | magic `b"RTB1"` (0x52 0x54 0x42 0x31)                                          |
|     4  |   1  | kind: `0x01 = Output`, `0x02 = Input`                                          |
|     5  |   1  | flags (reserved, MUST be `0` in v1; readers ignore unknown bits)               |
|     6  |   2  | reserved (MUST be `0`; readers ignore)                                         |
|     8  |   8  | `seq` u64 (Output: orchestrator-stamped seq; Input: `0`, ignored on receive)   |
|    16  |   4  | `payload_len` u32                                                              |
|    20  |   N  | payload bytes                                                                  |

**Payload limits.** A single binary frame carries at most **1 MiB** of payload. The decoder enforces the cap *before* allocating, so a malicious peer cannot OOM the process by stamping `0xFFFFFFFF` in `payload_len`. The encoder refuses to put an oversized frame on the wire.

**Versioning.** The magic carries a `1` suffix. A future revision (`b"RTB2"` etc.) appends a new magic and may run side-by-side; readers MUST reject any magic they don't recognise. Unknown `kind` bytes likewise reject as `invalid_message` so an unrelated wire format cannot be silently misinterpreted as a v1 frame.

**Compatibility decision (this slice).** The backend prefers binary frames for live and replayed `Output`. JSON `Output { seq, data }` remains *defined* in the protocol as a debug/legacy shape — its renderer-side decoder is kept so a JSON-only consumer can still reason about the surface — but the backend does not currently emit it. Inbound `Input` is accepted as either shape: clients SHOULD send the binary `Input` frame; the JSON `input` frame remains accepted for backwards compatibility and dev/diagnostic flows. The replay control frames (`replay_start`, `replay_end`, `replay_window_lost`) and every other lifecycle/error message stay JSON.

**Logging and reflection prohibitions (binary plane).** The same redaction rule the JSON plane enforces applies in full to the binary plane:

- The raw payload bytes of any binary `Input` or `Output` frame MUST NEVER appear in tracing logs, panic messages, error responses, or any frame the server emits. The Rust [`BinaryFrame`] type's `Debug` impl masks the payload at the type level as a last line of defense; the TS decoder's structured `BinaryDecodeFailure` shape carries only a classifier, never bytes.
- A malformed binary frame (bad magic, unknown kind, length mismatch, oversize claim, truncated header, non-zero reserved) MUST be rejected with the static JSON `error { code: "invalid_message", message: "invalid binary frame" }`. The handler does not echo any portion of the offending bytes.
- A client that sends a binary `Output` frame is malformed (Output is server→client only) and MUST receive `error { code: "invalid_message", message: "client must not send output frames" }`. The frame's bytes are dropped without inspection.

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

Multi-client (collaborative) attach behavior; backend-restart recovery for `starting` rows; and `ConnectInfo`-based `remote_addr` capture all belong to later, deliberate slices. (PTY allocation, `Input` forwarding, sequence-numbered `output` frames, the in-memory replay ring buffer, and the binary `RTB1` envelope for the terminal data plane have all landed in the live SSH PTY bridge slice, the replay buffer slice that follows it, and the binary-envelope slice on top of those.)

### Frontend terminal-core contract

The browser/Tauri client never talks to the WebSocket directly — it goes through the `@relayterm/terminal-core` package. That package owns the wire protocol, the transport, and the per-attachment state machine. **Renderers (xterm.js, libghostty-vt, restty, wterm) plug in through a renderer-neutral interface; none are dependencies of the core.** This load-bearing separation is what lets a renderer swap (or a future native Tauri renderer) drop in without changing protocol or state-machine code.

**Scope (load-bearing — this slice).** A successful `terminal-core` integration attests ONLY to the typed protocol envelope, transport lifecycle, and a renderer-neutral plug interface. It does **NOT** include replay-buffer behavior, real PTY byte streaming on the wire, or any reconnect policy beyond the explicit `attach`/`detach` lifecycle. (The xterm.js baseline renderer adapter — `@relayterm/terminal-xterm` — landed as a separate slice; see "xterm.js baseline renderer adapter" below.) Each remaining capability is a separate, deliberate slice.

#### Package layout

`packages/terminal-core/` exports four orthogonal layers — protocol, transport, renderer interface, session client — from one barrel `index.ts`. No file in this package may import an xterm/ghostty/restty/wterm package; if a future component needs renderer-specific behavior it lives in `packages/terminal-<name>/` and consumes `@relayterm/terminal-core` as a peer.

| Module       | Owns                                                                                          |
|--------------|-----------------------------------------------------------------------------------------------|
| `protocol`   | TS mirrors of `relayterm_protocol::{ClientMsg, ServerMsg, ErrorCode, AckKind, SessionAttachStatus}` plus a non-throwing `decodeServerMsg` that returns `{ok, message}` or `{ok:false, failure}`. |
| `transport`  | `TerminalTransport` interface and `WebSocketTerminalTransport` impl. Encodes outbound frames, decodes inbound text frames, surfaces close/error events. |
| `renderer`   | `TerminalRenderer` interface (mount/write/focus/resize/dispose/onInput). |
| `rendererOptions` | Renderer-neutral option/theme/cursor types: `BaseTerminalRendererOptions`, `RendererTheme`, `RendererThemeAnsi`, `RendererCursorStyle`. Adapter packages extend `BaseTerminalRendererOptions` with a local `<renderer>Only` escape hatch. Configuration shape only — not persisted user preferences. |
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
| `output`                       | `OutputMsg`                       | Live or replayed PTY output; carries `seq` and base64 `data`.                          |
| `replay_start`                 | `ReplayStartMsg`                  | Bracketing frame emitted before replayed `output` frames.                              |
| `replay_end`                   | `ReplayEndMsg`                    | Bracketing frame emitted after replayed `output` frames; carries `latest_seq`.         |
| `replay_window_lost`           | `ReplayWindowLostMsg`             | The client's `lastSeenSeq` predated the buffer's window; carries seq metadata only.    |
| `error`                        | `TerminalClientError`             | Transport, decode, unexpected-first-frame, server `error`, or send-while-not-attached. |
| `input_rejected_or_stubbed`    | `{reason, attempted}`             | See "Send guards" above; never includes payload bytes.                                 |

#### `TerminalRenderer` interface

A renderer is a class implementing `mount(element)`, `write(data)`, `focus()`, `resize(cols, rows)`, `dispose()`, and `onInput(cb)` — plus an optional `onResize(cb)` for renderers that own their own cell-grid measurement. The interface is deliberately minimal:

- `write(data)` accepts `string | Uint8Array` so a future binary-frame slice doesn't have to widen the type.
- The renderer never sees the protocol. The session client decides what bytes to call `write()` with; today that is nothing (no PTY).
- `dispose()` MUST release every listener and DOM/WebGL resource. Renderers MUST NOT carry state across a `dispose`/`mount` cycle — the orchestrator owns reconnect/replay.
- Renderer-specific config (xterm option names, ghostty-vt parser flags) does NOT belong in this interface. It lives in the adapter package's own constructor / config surface, behind an adapter-local `<renderer>Only` escape hatch (`xtermOnly`, `ghosttyOnly`, `resttyOnly`, `wtermOnly`). The renderer-neutral option/theme/cursor types (`BaseTerminalRendererOptions`, `RendererTheme`, `RendererThemeAnsi`, `RendererCursorStyle`) live in `terminal-core`'s `rendererOptions.ts` so every adapter package speaks the same surface; each adapter's `<Renderer>RendererOptions` is `BaseTerminalRendererOptions & { <renderer>Only?: ... }`. Cosmetic fields a given renderer cannot honour are accepted on the neutral surface and silently dropped during the adapter's option mapping (see per-adapter notes for which fields apply). This is configuration shape only — persistence/storage of user preferences is a separate, deliberate slice.

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
- `src/options.ts` — `XtermRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). The shared `RendererTheme` shape (background/foreground/cursor/selectionBackground + 16 named ANSI slots) lives in `terminal-core/src/rendererOptions.ts`. A local `xtermOnly` escape hatch passes raw `ITerminalOptions` through and is documented as **non-portable**.
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

### ghostty-web experimental renderer adapter

`@relayterm/terminal-ghostty-web` is the second concrete `TerminalRenderer` implementation. It is **experimental** — xterm.js remains the compatibility baseline. The adapter wraps `ghostty-web`, which embeds Ghostty's libghostty-vt parser via WebAssembly and exposes an xterm.js-API-compatible `Terminal` class. Landing this adapter proves the renderer-neutral seam holds end-to-end without backend protocol or `terminal-core` changes.

**Scope (load-bearing — this slice).** A successful integration attests ONLY to:

1. The same `TerminalRenderer` interface from `@relayterm/terminal-core` (`mount` / `write` / `focus` / `resize` / `dispose` / `onInput` / `onResize`) bridges ghostty-web bidirectionally, with the renderer's WASM `init()` resolved before `Terminal` construction.
2. `apps/web`'s dev-only live terminal lab can switch between xterm baseline and ghostty-web experimental at runtime; switching disposes the previous renderer and remounts the new one without tearing down the `TerminalSessionClient` or the wire protocol.
3. The backend protocol, the session client, and `terminal-core` remain unchanged and renderer-neutral.

It does **NOT** yet:

- Replace xterm as the production renderer. The production terminal UI is still not built; the dev lab is the only consumer.
- Persist a per-renderer preference. The lab defaults to xterm on every page load.
- Validate ghostty-web behavior in jsdom. Vitest exercises the adapter against a mocked `ghostty-web` module — the real WASM runtime is verified only in a browser dev session. The mock pins option mapping, init memoization, the pre-mount write queue, idempotent dispose, the dispose-during-pending-mount cancellation path, and the input-redaction rule.

#### Package layout

`packages/terminal-ghostty-web/` is a workspace package alongside `terminal-core` and `terminal-xterm`. Keys:

- `src/GhosttyWebRenderer.ts` — the only file in the repo that imports `ghostty-web`. Implements `TerminalRenderer`. `mount` is async because ghostty-web's one-time `init()` loads a shared WASM module before any `Terminal` can be constructed; the promise is memoized at module scope so multiple renderer instances share one load.
- `src/options.ts` — `GhosttyWebRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). `lineHeight` has no analogue in ghostty-web's `ITerminalOptions` and is silently dropped during the option mapping; this is documented adapter behavior, not a regression. A local `ghosttyOnly` escape hatch passes raw ghostty-web options through and is documented as **non-portable**.
- `package.json` declares `"sideEffects": false`. ghostty-web inlines its WASM payload as a base64 data URL inside its shipped JS bundle (no separate asset wiring is required for Vite consumers); combined with the `sideEffects: false` marker on this adapter, the production `apps/web` bundle tree-shakes both ghostty-web and this adapter when the dev lab is dead-code-eliminated.

#### Adapter contract

- `GhosttyWebRenderer` is the **only** `ghostty-web` consumer in the repo. `terminal-core` does not depend on `ghostty-web`. `terminal-xterm` does not depend on `ghostty-web`. `apps/web` depends on `@relayterm/terminal-ghostty-web` (workspace) — never directly on `ghostty-web`.
- Constructor takes `GhosttyWebRendererOptions` only; the underlying `Terminal` instance is private.
- `mount` is `async`. Calling it more than once on a live renderer rejects with `already mounted`. Calling it after `dispose` rejects with `cannot mount after dispose`. A synchronous `dispose()` issued **during** the awaited `init()` cancels the open silently — no `Terminal` is constructed and no DOM is touched after disposal.
- `write` before `mount` queues; the queue is flushed on `mount` resolution. `write` after `dispose` is a silent no-op.
- `dispose` is synchronous and idempotent. It tears down the WASM-backed `Terminal`, the `onData`/`onResize` subscriptions, the pre-mount write queue, and the listener sets. The shared `init()` WASM module stays loaded — re-disposing it would tear it out from under any other live `Terminal` on the page.
- A throwing user listener inside `onInput` is caught and dropped, identical to `XtermRenderer` — it MUST NOT interrupt sibling listeners or surface the input bytes through the error envelope. `tests/ghosttyWebRenderer.test.ts` pins the redaction rule with the same sentinel-string approach as the xterm adapter.

#### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `ghostty-web` (or `@xterm/*`).
- `GhosttyWebRendererOptions` is shape-compatible with `XtermRendererOptions` for the portable knobs, so an app can swap renderers by changing only the import. Renderer-only escape-hatch fields (`xtermOnly`, `ghosttyOnly`) are explicitly NOT promised to behave the same across adapters.
- The wire protocol stays RelayTerm-shaped. A live PTY's `Output` bytes hand identical payloads to either renderer; `Input` flows back through the same `TerminalSessionClient`.

#### Diagnostic UI

The dev-only live terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — exposes a `renderer:` radio group switching between xterm baseline (default) and ghostty-web experimental. Switching while attached tears down the current renderer and `TerminalSessionClient` and immediately reconnects with the new renderer; switching while idle records the choice for the next `connect()`. The event log records ONLY the renderer name on switch — no payload bytes. The redaction rules pinned by `apps/web/tests/labLog.test.ts`, `tests/xtermRenderer.test.ts`, and `tests/ghosttyWebRenderer.test.ts` continue to hold across renderer switches.

#### Production bundle behavior

The production `apps/web` build (`pnpm -r build`) emits a ~28 KB JS bundle. The dev lab is gated behind `import.meta.env.DEV`, which Vite inlines as a constant; Rollup eliminates the dead branch, which makes the `apps/web` imports of `@relayterm/terminal-xterm` and `@relayterm/terminal-ghostty-web` unreachable. Both adapter packages declare `sideEffects: false` (xterm pins only `./src/styles.ts` and `**/*.css` as side-effectful), so Rollup drops the wrappers, which in turn drops the underlying libraries — xterm.js's parser/renderer and ghostty-web's ~400 KB inlined WASM data URL. Only the xterm CSS side-effect import remains in the prod CSS bundle; ghostty-web ships no CSS so its adapter contributes nothing to the styles bundle. Caveat: ghostty-web 0.4.0 itself does not declare `sideEffects` in its `package.json`, so if a future code change made the adapter reachable from a non-dev path, the WASM data URL would land in the prod JS bundle.

#### Future work (explicit out-of-scope for this slice)

Production terminal UI; persistent per-renderer preference; renderer benchmarking harness; mobile/Tauri shell integration of the experimental renderer; jsdom/headless-browser verification of the real ghostty-web WASM runtime. Each is a separate, deliberate slice.

### restty experimental renderer adapter

`@relayterm/terminal-restty` is the third concrete `TerminalRenderer` implementation. It is **experimental** — xterm.js remains the compatibility baseline; `@relayterm/terminal-ghostty-web` remains the libghostty-vt-via-WASM experiment; this adapter wraps `restty` (npm `restty@0.1.x`), a more ambitious modern renderer powered by libghostty-vt (WASM), WebGPU/WebGL2, and TypeScript text shaping. Landing this adapter proves a substantively different renderer experiment can drop in behind the renderer-neutral seam without backend protocol or `terminal-core` changes.

**Scope (load-bearing — this slice).** A successful integration attests ONLY to:

1. The same `TerminalRenderer` interface from `@relayterm/terminal-core` (`mount` / `write` / `focus` / `resize` / `dispose` / `onInput` / `onResize`) bridges restty's `restty/xterm` compatibility shim bidirectionally.
2. `apps/web`'s dev-only live terminal lab can switch between xterm baseline (default), ghostty-web experimental, and restty experimental at runtime; switching disposes the previous renderer and remounts the new one without tearing down the wire protocol.
3. The backend protocol, the session client, and `terminal-core` remain unchanged and renderer-neutral.

It does **NOT** yet:

- Replace xterm as the production renderer. The production terminal UI is still not built; the dev lab is the only consumer.
- Persist a per-renderer preference. The lab defaults to xterm on every page load.
- Validate restty behavior in jsdom. Vitest exercises the adapter against a mocked `restty/xterm` module — the real WASM/WebGPU runtime is verified only in a browser dev session. The mock pins option mapping, the pre-mount write queue, idempotent dispose, the dispose-during-pending-mount cancellation path, the UTF-8 decode of `Uint8Array` writes, and the input-redaction rule.
- Honor restty's native pane / plugin / shader-stage surface. The adapter binds to the focused `restty/xterm` compatibility shim, not the full `Restty` class. Promoting any of those surfaces is future work.

#### Package layout

`packages/terminal-restty/` is a workspace package alongside `terminal-core`, `terminal-xterm`, and `terminal-ghostty-web`. Keys:

- `src/ResttyRenderer.ts` — the only file in the repo that imports from `restty`. Implements `TerminalRenderer`. Binds against `restty/xterm`'s `Terminal` class for shape-parity with the existing adapters; restty's WASM/WebGPU runtime initializes lazily inside the underlying `Restty` instance the first time `Terminal.open` is called. `mount` is `async` for parity with the ghostty-web adapter and to give restty room to grow into a future async init step without changing the adapter contract.
- `src/options.ts` — `ResttyRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). The `restty/xterm` shim does not interpret these cosmetic knobs (the underlying `Restty` exposes `setFontSize` / `setLigatures` / `applyTheme` etc. as native APIs); the adapter accepts them on the neutral surface for cross-adapter shape-parity and silently drops them during the option mapping. Honoring them via `Restty`'s native APIs is future work. A local `resttyOnly` escape hatch passes raw restty-compat option keys through and is documented as **non-portable**. An optional `cols` / `rows` initial cell grid is accepted on the constructor and forwarded into the restty `Terminal`.
- `package.json` declares `"sideEffects": false`. restty ships a sizeable WASM/WebGPU payload (~3 MB JS plus an inlined WASM binary); combined with the `sideEffects: false` marker the production `apps/web` bundle tree-shakes both restty and this adapter when the dev lab is dead-code-eliminated. Caveat: restty 0.1.x itself does not declare `sideEffects` in its `package.json`, so if a future code change made the adapter reachable from a non-dev path the WASM payload would land in the prod JS bundle.

#### Adapter contract

- `ResttyRenderer` is the **only** `restty` consumer in the repo. `terminal-core` does not depend on `restty`. `terminal-xterm` and `terminal-ghostty-web` do not depend on `restty`. `apps/web` depends on `@relayterm/terminal-restty` (workspace) — never directly on `restty`.
- Constructor takes `ResttyRendererCtorOptions` (the neutral options plus optional `cols` / `rows`); the underlying restty `Terminal` instance is private.
- `mount` is `async`. Calling it more than once on a live renderer rejects with `already mounted`. Calling it after `dispose` rejects with `cannot mount after dispose`. A synchronous `dispose()` issued **during** the awaited microtask cancels the open silently — no `Terminal` is constructed and no DOM is touched after disposal.
- `write` accepts `string | Uint8Array`. `restty/xterm`'s `Terminal.write(data: string)` accepts strings only; the adapter UTF-8-decodes `Uint8Array` payloads with replacement-on-error before forwarding. UTF-8 is the correct decoding for SSH PTY output; a future binary frame format is out of scope here. `write` before `mount` queues; the queue is flushed on `mount` resolution. `write` after `dispose` is a silent no-op.
- `dispose` is synchronous and idempotent. It tears down the underlying `Restty` instance via `Terminal.dispose()` (canvas, IME input, render loop, pane manager), the `onData`/`onResize` subscriptions, the pre-mount write queue, and the listener sets. The restty WASM module itself stays loaded for the page.
- A throwing user listener inside `onInput` is caught and dropped, identical to `XtermRenderer` and `GhosttyWebRenderer` — it MUST NOT interrupt sibling listeners or surface the input bytes through the error envelope. `tests/resttyRenderer.test.ts` pins the redaction rule with the same sentinel-string approach as the sibling adapters.

#### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `restty` (or `@xterm/*` / `ghostty-web`).
- `ResttyRendererOptions` is shape-compatible with `XtermRendererOptions` and `GhosttyWebRendererOptions` for the portable knobs, so an app can swap renderers by changing only the import. Renderer-only escape-hatch fields (`xtermOnly`, `ghosttyOnly`, `resttyOnly`) are explicitly NOT promised to behave the same across adapters. Cosmetic knobs (font, cursor, theme, scrollback) are accepted by `ResttyRendererOptions` for shape-parity but silently dropped during the mapping — see "Package layout."
- The wire protocol stays RelayTerm-shaped. A live PTY's `Output` bytes hand identical payloads to all three renderers; `Input` flows back through the same `TerminalSessionClient`.

#### Diagnostic UI

The dev-only live terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — exposes a `renderer:` radio group switching between xterm baseline (default), ghostty-web experimental, and restty experimental. Switching while attached tears down the current renderer and `TerminalSessionClient` and immediately reconnects with the new renderer; switching while idle records the choice for the next `connect()`. The event log records ONLY the renderer name on switch — no payload bytes. The redaction rules pinned by `apps/web/tests/labLog.test.ts`, `tests/xtermRenderer.test.ts`, `tests/ghosttyWebRenderer.test.ts`, and `tests/resttyRenderer.test.ts` continue to hold across renderer switches.

#### Production bundle behavior

The dev lab is gated behind `import.meta.env.DEV`, which Vite inlines as a constant; Rollup eliminates the dead branch, which makes the `apps/web` imports of `@relayterm/terminal-xterm`, `@relayterm/terminal-ghostty-web`, and `@relayterm/terminal-restty` unreachable. All three adapter packages declare `sideEffects: false` (xterm pins only `./src/styles.ts` and `**/*.css` as side-effectful), so Rollup drops the wrappers, which in turn drops the underlying libraries — xterm.js's parser/renderer, ghostty-web's WASM data URL, and restty's WASM/WebGPU payload. Only the xterm CSS side-effect import remains in the prod CSS bundle; ghostty-web and restty ship no CSS so their adapters contribute nothing to the styles bundle. Caveat: neither ghostty-web 0.4.0 nor restty 0.1.x declares `sideEffects` in its own `package.json`, so if a future code change made either adapter reachable from a non-dev path, the corresponding WASM payload would land in the prod JS bundle.

#### Future work (explicit out-of-scope for this slice)

Production terminal UI; persistent per-renderer preference; renderer benchmarking harness; mobile/Tauri shell integration of the experimental renderer; jsdom/headless-browser verification of the real restty WASM/WebGPU runtime; honoring the neutral cosmetic knobs (font, cursor, theme, scrollback) via `Restty`'s native APIs; restty pane / plugin / shader-stage surface integration. Each is a separate, deliberate slice.

### wterm experimental renderer adapter

`@relayterm/terminal-wterm` is the fourth concrete `TerminalRenderer` implementation. It is **experimental** — xterm.js remains the compatibility baseline; `@relayterm/terminal-ghostty-web` and `@relayterm/terminal-restty` remain the two libghostty-vt-based experiments; this adapter wraps `@wterm/dom` (npm `@wterm/dom@0.2.x`, depending transitively on `@wterm/core@0.2.x`), a DOM-rendered terminal emulator with a Zig+WASM core. The adapter is the **DOM/mobile/accessibility-oriented** experiment in the renderer lineup: text selection, copy, paste, IME composition, and mobile soft keyboards flow through the platform's native text-handling primitives because the cell grid renders into ordinary DOM nodes (`.term-row > span`), not a canvas/WebGPU surface. Landing this adapter proves a substantively different rendering style can drop in behind the renderer-neutral seam without backend protocol or `terminal-core` changes.

#### Adapter contract

1. The same `TerminalRenderer` interface from `@relayterm/terminal-core` (`mount` / `write` / `focus` / `resize` / `dispose` / `onInput` / `onResize`) bridges wterm's `WTerm` orchestrator bidirectionally.
2. `apps/web`'s dev-only live terminal lab can switch between xterm baseline (default), ghostty-web experimental, restty experimental, and wterm experimental at runtime; switching disposes the previous renderer and remounts the new one without tearing down the wire protocol.
3. The same redaction rule pinned by the sibling adapters (`tests/xtermRenderer.test.ts`, `tests/ghosttyWebRenderer.test.ts`, `tests/resttyRenderer.test.ts`) holds verbatim — no `console.*` in the adapter, no payload bytes inside thrown errors, no neutral-knob echo into the underlying constructor's options blob. `tests/wtermRenderer.test.ts` pins the rule with the same sentinel-string approach.

What this slice does NOT promise:

- A polished terminal UI. The wterm adapter is wired up only inside the dev lab; production builds tree-shake it out.
- Full theming parity with `XtermRenderer`. wterm consumes typography/theme via CSS custom properties on the `.wterm` host element (see `@wterm/dom/src/terminal.css`), not via `WTermOptions`; the adapter accepts the neutral cosmetic knobs (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `scrollbackLines`, `theme`) for cross-renderer shape-parity and silently drops them during the option mapping. `cursorBlink` is the one cosmetic knob that flows through to the `WTerm` constructor.
- Validation of wterm behavior in jsdom. Vitest exercises the adapter against a mocked `@wterm/dom` module — the real WASM/DOM runtime is verified only in a browser dev session. The mock pins option mapping, the pre-mount write queue, the pre-mount latest-resize cache, idempotent dispose, the dispose-during-pending-init cancellation path (which destroys the just-constructed `WTerm` instead of leaking a render loop), the static init-failure error message, and the input-redaction rule.
- Honoring `WTerm`'s `onTitle` callback. Title-change is not a channel on the renderer-neutral interface; the adapter does not wire it. Adding it later is a deliberate change.
- Surfacing wterm's `DebugAdapter` in the dev lab UI. The `wtermOnly.debug` knob passes through to `WTermOptions.debug` for adapter-local experimentation, but enabling it makes wterm's own `DebugAdapter` log render-path traces (including bytes the bridge processed) outside the adapter's redaction surface. The dev lab UI does NOT expose a debug checkbox today; if a future slice adds one, it must NOT be wired into any path that captures real terminal input or output, and the adapter test suite must continue to pin that the adapter itself surfaces zero console output regardless of `debug` value.

#### Package layout

`packages/terminal-wterm/` is a workspace package alongside `terminal-core`, `terminal-xterm`, `terminal-ghostty-web`, and `terminal-restty`. Keys:

- `src/WtermRenderer.ts` — the only file in the repo that imports `@wterm/dom`. Implements `TerminalRenderer`. `mount` is async because `WTerm.init()` loads the WASM bridge before the renderer can write or render. The adapter constructs the `WTerm` synchronously inside `mount(element)` (because `WTerm`'s constructor takes the host element and immediately mutates it — appending a child grid div and adding the `.wterm` class) and then awaits `init()` before flushing the pre-mount write queue. A synchronous `dispose()` issued during the awaited `init()` destroys the just-constructed `WTerm` and skips the queue flush.
- `src/options.ts` — `WtermRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). `cursorBlink` is forwarded to the `WTerm` constructor (it toggles a CSS class on the host); the rest are accepted on the neutral surface for cross-adapter shape-parity and silently dropped during the option mapping. Theming/typography for wterm is documented as going through CSS variables on the `.wterm` host (`--term-fg`, `--term-bg`, `--term-color-{0..15}`, `--term-font-family`, `--term-font-size`, `--term-line-height`, `--term-row-height`) rather than constructor arguments. A local `wtermOnly` escape hatch carries adapter-local knobs (`autoResize`, `wasmUrl`, `debug`) and is documented as **non-portable**. The `autoResize` default flips from wterm's own `true` to `false` on the adapter, so the caller drives sizing explicitly via `renderer.resize(cols, rows)` for parity with xterm/ghostty-web/restty; opt back into wterm's `ResizeObserver`-driven auto-fit by setting `wtermOnly.autoResize: true`. An optional `cols` / `rows` initial cell grid is accepted on the constructor and forwarded into the `WTerm` constructor.
- `package.json` declares `"sideEffects": false`. `@wterm/core@0.2.x` inlines its WASM payload as a base64 module inside the shipped JS (`wasm-inline.js`, ~17 KB), so no separate asset wiring is required for Vite consumers; combined with the `sideEffects: false` marker on this adapter, the production `apps/web` bundle tree-shakes both `@wterm/dom`/`@wterm/core` and this adapter when the dev lab is dead-code-eliminated. Caveat: `@wterm/dom` does not declare `sideEffects` in its own `package.json`, so if a future code change made the adapter reachable from a non-dev path the WASM payload would land in the prod JS bundle.

#### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `@wterm/*` (or `@xterm/*`, `ghostty-web`, `restty`).
- `WtermRenderer` is the **only** `@wterm/dom` consumer in the repo. `terminal-core` does not depend on `@wterm/dom`. `terminal-xterm`, `terminal-ghostty-web`, and `terminal-restty` do not depend on `@wterm/dom`. `apps/web` depends on `@relayterm/terminal-wterm` (workspace) — never directly on `@wterm/dom`.
- Constructor takes `WtermRendererCtorOptions` (the neutral options plus optional `cols` / `rows`); the underlying `WTerm` instance is private.
- `write` accepts `string | Uint8Array`. `WTerm.write(data)` accepts both directly via the `WasmBridge` (`writeString` UTF-8-encodes; `writeRaw` takes bytes), so the adapter forwards both shapes unchanged — no UTF-8 decode step inside the adapter (unlike `restty/xterm`). `write` before `mount` queues; the queue is flushed on `mount` resolution. `write` after `dispose` is a silent no-op.
- `dispose` is synchronous and idempotent. It tears down the underlying `WTerm` via `destroy()` (which clears the host element's `innerHTML`, detaches the click listener, disconnects the optional internal `ResizeObserver`, and tears down the `InputHandler`), the pre-mount write queue, the cached pre-mount resize, and the listener sets. The `@wterm/core` WASM module itself stays loaded for the page; that's intentional — re-initialising it would tear shared state out from under any future renderer instance.
- The wire protocol stays RelayTerm-shaped. A live PTY's `Output` bytes hand identical payloads to all four renderers; `Input` flows back through the same `TerminalSessionClient`.

#### Diagnostic UI

The dev-only live terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — adds a `wterm experimental` choice to the `renderer:` radio group. Switching while attached tears down the current renderer and `TerminalSessionClient` and immediately reconnects with the new renderer; switching while idle records the choice for the next `connect()`. The event log records ONLY the renderer name on switch — no payload bytes. The redaction rules pinned by `apps/web/tests/labLog.test.ts`, `tests/xtermRenderer.test.ts`, `tests/ghosttyWebRenderer.test.ts`, `tests/resttyRenderer.test.ts`, and `tests/wtermRenderer.test.ts` continue to hold across renderer switches. The lab's helper text calls out that wterm's DOM rendering changes the selection / copy-paste / IME / mobile-keyboard model relative to canvas/WebGPU adapters.

#### Production bundle behavior

The dev lab is gated behind `import.meta.env.DEV`, which Vite inlines as a constant; Rollup eliminates the dead branch, which makes the `apps/web` imports of `@relayterm/terminal-xterm`, `@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`, and `@relayterm/terminal-wterm` unreachable. The xterm and wterm adapter packages pin `./src/styles.ts` and `**/*.css` as side-effectful (because they re-export an upstream CSS file via a dedicated `/styles` entry); ghostty-web and restty declare `sideEffects: false` outright. So Rollup drops the JS wrappers, which in turn drops the underlying libraries — xterm.js's parser/renderer, ghostty-web's WASM data URL, restty's WASM/WebGPU payload, and `@wterm/dom`/`@wterm/core`'s DOM/WASM bundle — and a check of the production bundle shows zero JS references to `WTerm`/`WasmBridge`. The `@relayterm/terminal-wterm/styles` side-effect import is the same documented compromise xterm has: routing the CSS through the adapter package (rather than `@wterm/dom/css` directly) is necessary because pnpm's strict resolver refuses an `apps/web` import of an undeclared transitive dep, and the CSS side-effect itself is not eliminated by Rollup the way the JS branch is. Both xterm's grid sheet and wterm's `.wterm` host stylesheet land in the prod CSS bundle today; ghostty-web and restty ship no CSS so their adapters contribute nothing. Caveat: none of `@xterm/xterm`, ghostty-web 0.4.0, restty 0.1.x, or `@wterm/dom` 0.2.x declare `sideEffects` in their own `package.json`, so if a future code change made any of these adapters reachable from a non-dev path the corresponding payload would land in the prod JS bundle.

#### Future work (explicit out-of-scope for this slice)

Production terminal UI; persistent per-renderer preference; renderer benchmarking harness; mobile/Tauri shell integration of the experimental renderer; jsdom/headless-browser verification of the real wterm WASM/DOM runtime; honoring the neutral cosmetic knobs (font, cursor, theme, scrollback) via wterm's CSS custom properties; surfacing wterm's `onTitle` channel; wiring wterm's `DebugAdapter` into the dev lab. Each is a separate, deliberate slice.

### Production web app shell

The production-facing web app has a real shell now. The shell is layout, navigation, and dev/prod gating only — it is not the production terminal workspace, not real CRUD UI, and not real auth UI. Each of those is a deliberate later slice.

**Scope (load-bearing — this slice).**

1. The shell renders in production (`vite build` / preview) AND in development (`vite dev`).
2. Navigation is a small local view-state model — no router. The discriminator (`AppViewId`) is `dashboard | terminal | sessions | servers | identities | settings`.
3. Each non-dashboard view is a placeholder. Placeholder copy is honest: "not implemented yet", "future work", and a short bullet list of what currently exists on the backend. **Placeholders MUST NOT show fake data, mock secret values, or a `private_key` / `encrypted_private_key` field.** The SSH-identities placeholder explicitly does not surface secrets.
4. Dev-lab tools (`TerminalProtocolLab`, `DevTerminalWorkbench`, the per-renderer lab and renderer diagnostics) stay dev-only. They are reachable only via the "Developer tools" section of the shell, which is gated by `import.meta.env.DEV` AND a `devTools` snippet passed from `App.svelte`. Vite's dead-code elimination drops the dev branch — and the dev-lab imports it pulls in — from the production bundle.
5. The dashboard exposes a one-shot backend health probe (`GET /healthz`) via `lib/api/health.ts`. The probe does NOT poll, does NOT retry, and does NOT surface transport-error detail. Failure collapses to `down`; the underlying error is dropped on the floor (liveness probe, not diagnostic).

**Architecture rule.** Production shell components (`lib/app/`) MUST NOT import anything from `lib/dev/` or any renderer adapter (`@relayterm/terminal-{xterm,ghostty-web,restty,wterm}`). Renderer packages stay dev-lab-only until the production terminal workspace lands. This is enforced by `appShellIsolation.test.ts`.

**Package layout.**

```
apps/web/src/lib/app/
├─ AppShell.svelte         # composes sidebar + topbar + view + (dev) tools
├─ SidebarNav.svelte
├─ TopBar.svelte
├─ StatusBadge.svelte
├─ navigation.ts           # NAV_ITEMS, AppViewId, DEFAULT_VIEW, findNavItem
└─ views/
   ├─ DashboardView.svelte    # backend health probe
   ├─ TerminalView.svelte     # placeholder
   ├─ SessionsView.svelte     # placeholder
   ├─ ServersView.svelte      # placeholder
   ├─ IdentitiesView.svelte   # placeholder, no secrets
   ├─ SettingsView.svelte     # placeholder
   └─ PlaceholderView.svelte  # shared layout for non-functional views
apps/web/src/lib/api/
├─ apiErrors.ts                # shared LoadError, fetchJsonList, readErrorEnvelope, describeLoadError
├─ health.ts                   # checkHealth() helper
├─ hosts.ts                    # listHosts() + parseHost()
├─ serverProfiles.ts           # listServerProfiles() + parseServerProfile() + resolveProfileLinks()
└─ sshIdentities.ts            # listSshIdentities() + parseSshIdentity() + publicKeyPreview() + createSshIdentity()
```

**Future work (explicit out-of-scope for this slice).**

Production terminal workspace; production renderer selector; renderer-preference persistence; server / profile / identity CRUD UI; real auth UI (passkey enrollment, session list); mobile/Tauri shell integration; password bootstrap; private-key import; durable session-recording UI; a real router (URL-driven routes, deep-linking). Each is a separate slice.

### Production inventory read-only views

The Servers and Identities views are display-only inventories of `hosts`, `server_profiles`, and `ssh_identities`. They prove the production shell can fetch real backend data through typed, redaction-safe helpers without pulling in the dev lab or any renderer adapter. Create / edit / delete UI, terminal launch, and SSH identity deletion remain future work. (Host-key trust, auth-check, and SSH identity generation are now wired — see the per-flow sections below.)

**Scope (load-bearing — this slice).**

1. **Servers view** (`apps/web/src/lib/app/views/ServersView.svelte`) renders two grouped sections: a Hosts list (display name, hostname, port, default username) and a Profiles list (name, linked host summary if resolvable from the fetched hosts, effective username with explicit "(host default)" / "(override)" attribution, tags, and last-connected timestamp). Hosts and profiles are fetched in parallel via `Promise.all`; either failure collapses the whole view to a single safe error summary keyed off the first failed resource.
2. **Identities view** (`apps/web/src/lib/app/views/IdentitiesView.svelte`) renders one row per identity with name, key type, full SHA-256 fingerprint, a one-line public-key preview (`publicKeyPreview` truncates the base64 body to keep tables tight), created-at, last-used-at, and a "Copy public key" button. The button copies ONLY `identity.public_key` — never the fingerprint, never any other field. Clipboard failures collapse to a static `Copy failed` label without echoing origin/permission detail.
3. **Dashboard counts** (`DashboardView.svelte`) shows `hosts` / `profiles` / `identities` cardinality using the same helpers. The counts are nice-to-have: any failure collapses to an unobtrusive `—` placeholder so the per-view error surface stays the canonical triage path. No polling.
4. **No secret material is rendered.** `SshIdentity` (TypeScript DTO) does not declare an `encrypted_private_key` or `private_key` field. The runtime parser in `parseSshIdentity` builds the DTO field-by-field, so a backend bug or hostile fixture that includes those keys cannot smuggle them onto the parsed object. `tests/inventoryApi.test.ts` pins this with sentinel strings asserted absent from the parsed object, the serialized JSON, and the formatted preview.
5. **Loading / empty / error states are honest.** Loading states render an unobtrusive "Loading…" placeholder. Empty states say "CRUD UI is not implemented yet — created through the backend API today." Error states render the formatted summary and nothing else (no retry-storm, no auto-reload).
6. **Architecture rule preserved.** The new helpers and views live entirely under `lib/app/` and `lib/api/`; no import touches `lib/dev/` or any renderer adapter. `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- The shared error reader (`readErrorEnvelope` in `apiErrors.ts`) extracts ONLY `code` and `message` from the backend's `{ error: { code, message } }` envelope; sibling fields (including any future `operator_detail`) are dropped.
- `describeLoadError` formats the UI summary as a function of `kind` + `status` + `code` only — it never echoes the wire `message` of an HTTP error or the thrown message of a transport error. The typed error object preserves both so programmatic callers can branch, but the formatter is the single point that reaches the UI.
- The helpers do NOT log raw response bodies. `inventoryApi.test.ts` pins `console.log/warn/error` as untouched on success and on transport failure.
- The OpenSSH public-key preview is a pure string operation on the supplied argument; nothing in the helper looks up a private-key field by side channel.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/`).**

- `Host` — `{ id, display_name, hostname, port, default_username, created_at, updated_at }`. The parser rejects ports outside `1..=65535` or non-integer values; unknown extra fields are silently dropped so a future safe addition does not break older clients.
- `ServerProfile` — `{ id, name, host_id, ssh_identity_id, username_override, tags[], created_at, updated_at, last_connected_at }`. Parser rejects non-string tag entries. `resolveProfileLinks(profile, hosts)` produces `{ host, effectiveUsername, inheritedFromHost }` — the join is done on the client; a missing `host_id` is rendered honestly as "host not in your inventory" and `effectiveUsername` falls back to `null` when neither override nor host default is reachable.
- `SshIdentity` — `{ id, name, key_type, public_key, fingerprint_sha256, created_at, last_used_at }`. `key_type` is constrained to the wire-stable `ed25519 | rsa | ecdsa_p256 | ecdsa_p384 | ecdsa_p521` set; unknown algorithm tags collapse to `malformed_response`.

**Future work (explicit out-of-scope for this slice).**

CRUD forms (create / edit / delete) for hosts and profiles; SSH identity deletion / rename; private-key import; terminal session launch from the production shell; per-row "view details" / `get_by_id` panels; password bootstrap / `ssh-copy-id`; durable session-recording UI; real auth UI; mobile/Tauri shell integration. Each is a separate slice. (SSH identity generation, host-key preflight + trust UI, and auth-check UI are now wired — see the per-flow sections below.)

### Production SSH identity generation UI

The first production-safe write flow on the Identities view: an operator can ask the backend to generate a fresh keypair, see only the public metadata, and copy the OpenSSH public key for manual installation on the target server. No private material is ever rendered, copied, logged, or returned over the wire.

**Scope (load-bearing — this slice).**

1. **"Generate SSH identity" panel** lives on `IdentitiesView.svelte`, opened by a button in the view header. The form has a name input (≤ {`MAX_IDENTITY_NAME_LEN` = 64} characters, no surrounding whitespace, no control characters) and a key-type select bound to `SUPPORTED_GENERATION_KEY_TYPES`. The submit button is disabled while a request is in flight or while the trimmed name is empty. The "Close" button is disabled while submitting so an in-flight request cannot be orphaned.
2. **`createSshIdentity(request, options)`** in `lib/api/sshIdentities.ts` is the single client entry point. It client-side-validates the request (mirrors the backend's `CreateSshIdentityRequest::validate` rules), POSTs `{ name, key_type }` to `/api/v1/ssh-identities`, parses the response with `parseSshIdentity` (which already drops `private_key`/`encrypted_private_key`), and returns a typed `CreateSshIdentityResult`. It does not throw, does not log raw response bodies, and does not echo wire / transport detail through any user-facing string.
3. **Supported key types** are gated by `SUPPORTED_GENERATION_KEY_TYPES` — currently `["ed25519"]`, the deliberate intersection of the wire-stable `SshKeyType` union (which has to decode legacy rows) and what the backend vault can actually generate today. A test pins this against drift.
4. **Success UI** renders name, key type, SHA-256 fingerprint, created-at, the full OpenSSH public key in a `<pre>`, and a "Copy public key" button (re-uses the existing `copyPublicKey` helper). The success card stays visible until the user closes the panel; the new identity is also prepended to the inventory list (or a refresh is triggered if the list was loading/errored).
5. **Error UI** renders one line from `describeCreateSshIdentityError` and nothing else. The summary is a function of `kind` + `status` + `code` (and the validation `reason` enum) only — never the wire `message`, never the transport `Error.message`. A 503 `service_unavailable` is collapsed to a friendly "backend vault is not configured" hint so an operator running without a master key sees an actionable message; every other HTTP error keeps the raw `HTTP <status> <code>` form.
6. **No backend changes.** The existing `POST /api/v1/ssh-identities` route already returns the wire shape the inventory parser consumes; the slice is purely a frontend addition.
7. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- The `SshIdentity` TypeScript DTO does not declare `encrypted_private_key` or `private_key`. `parseSshIdentity` constructs the DTO field-by-field, so a backend bug or hostile fixture that includes those keys on a 201 response cannot smuggle them onto the returned object. Sentinel tests in `tests/inventoryApi.test.ts` pin this for both the parser and the `createSshIdentity` happy path.
- `describeCreateSshIdentityError` is the only formatter that reaches the UI. It never echoes the wire `message` or transport `Error.message`. Sentinel tests pin this against future regressions for `http`, `transport`, and `service_unavailable` kinds.
- The success card surfaces the OpenSSH public key in exactly two places: a `<pre>` for inspection and the "Copy public key" button (which copies `identity.public_key` only). The key never appears in `title=` / `aria-*` tooltips, console output, or any data attribute.
- Generation surface mirrors the existing list/copy redaction discipline: helpers do NOT log raw response bodies; tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/ssh_identity.rs`).**

- Request: `{ name: string, key_type?: "ed25519" }`. The backend accepts the broader `SshKeyType` union as a string but the client gates it to `SUPPORTED_GENERATION_KEY_TYPES` so a UI typo cannot reach the boundary.
- Response (`201 Created`): the same `SshIdentity` shape used by `listSshIdentities` — `{ id, name, key_type, public_key, fingerprint_sha256, created_at, last_used_at }`. No private-key field exists on the wire.

**UX copy (load-bearing).**

- The panel intro states that RelayTerm generated the keypair on the backend, the private key is encrypted at rest with the master key and never reaches the browser, and that copy/install on `~/.ssh/authorized_keys` is currently manual.
- The success card explicitly tells the operator to append the public key to the target server. It does NOT imply the key is already installed, that the identity can already authenticate against any host, or that the private key can be recovered from the UI.
- The footer note carries the future-work list: deletion, rename, private-key import, password bootstrap, and `ssh-copy-id` automation are deliberate later slices.

**Future work (explicit out-of-scope for this slice).**

Identity deletion and rename; private-key import (BYOK); editing the name after creation; password bootstrap and `ssh-copy-id` to automate `authorized_keys` install; per-identity audit log surface; multi-vault key rotation. Each is a separate slice.

### Production host & server-profile creation UI

The next production-safe write flows on the Servers view: an operator can create a `host` (a reachable target definition) and a `server_profile` (a binding of a host to an SSH identity). Both flows are metadata-only — they do NOT trust a host key, do NOT verify SSH authentication, and do NOT confirm the public key is installed on the target.

**Scope (load-bearing — this slice).**

1. **"Create host" panel** lives on `ServersView.svelte`, opened by a button in the view header. The form has `display_name` (≤ 128 chars, no surrounding whitespace, no control chars), `hostname` (≤ 253 chars, no whitespace, no control chars, only ASCII alphanumerics + `-`, `.`, `:`, `[`, `]`, `_`), `port` (integer 1..=65535, defaults to 22), and `default_username` (≤ 64 chars, leading letter/`_`, ASCII alphanumerics + `-`, `_`, `.` thereafter). Submit is disabled while a request is in flight or while any required text field is empty after trim.
2. **"Create server profile" panel** lives on the same view. The form has `name` (≤ 64 chars, no surrounding whitespace, no control chars), a `host` select (from the caller's existing hosts), an `ssh_identity` select (from the caller's existing identities), an optional `username_override` (same shape as host username), and an optional `tags` input parsed from a comma-separated string (≤ 32 tags, each ≤ 32 chars, ASCII alphanumerics + `-`/`_`, no duplicates). The "Create server profile" button is **disabled at the toolbar** when the caller has zero hosts OR zero identities — `canSubmitServerProfile(hostCount, identityCount)` returns a typed reason (`no_hosts | no_identities | no_hosts_or_identities | ok`) and the UI renders an honest empty-state hint without ever opening the form.
3. **`createHost(request, options)` and `createServerProfile(request, options)`** in `lib/api/hosts.ts` and `lib/api/serverProfiles.ts` are the single client entry points. Each client-side-validates the request (mirrors the backend's validators in `crates/relayterm-core/src/validation.rs`), POSTs to the relevant endpoint via the shared `postJsonItem` helper in `apiErrors.ts`, parses the response with the existing `parseHost` / `parseServerProfile`, and returns a typed result. Neither helper throws, logs raw response bodies, or echoes wire / transport detail through any user-facing string.
4. **Success UI** for hosts shows the new display name, `hostname:port`, and default user, with an explicit "Reachability and host-key trust are not verified by this action." disclaimer. Success UI for profiles shows the new name and an explicit "The host key is not yet trusted and SSH authentication has not been verified for this profile." disclaimer. The newly-created row is also prepended to the inventory list (or a refresh is triggered if the list was loading/errored).
5. **Error UI** renders one line from `describeCreateHostError` / `describeCreateServerProfileError` and nothing else. Both formatters stay a function of `kind` + `status` + `code` (and the validation `reason` enum) only — never the wire `message`, never the transport `Error.message`. The server-profile formatter collapses `404 not_found` to a friendly "linked host or SSH identity not found" hint so a stale-reference race shows an actionable message.
6. **No backend changes.** The existing `POST /api/v1/hosts` and `POST /api/v1/server-profiles` routes already accept the wire shapes the new helpers send; the slice is purely a frontend addition.
7. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- Both formatters never echo wire / transport detail. Sentinel-string tests in `tests/createApi.test.ts` pin this for `http`, `transport`, and `validation` kinds across both helpers, including the `404 not_found` collapse on profile create.
- `parseServerProfile` already constructs the DTO field-by-field, so a backend bug or hostile fixture that includes `private_key` / `encrypted_private_key` on a 201 response cannot smuggle them onto the parsed object. A redaction-sentinel test pins this for the `createServerProfile` happy path.
- Helpers do NOT log raw response bodies. Tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure for both `createHost` and `createServerProfile`.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/`).**

- Host create request: `{ display_name, hostname, port?, default_username }`. The validator normalizes `port` to `DEFAULT_SSH_PORT` (22) when omitted before sending so the wire body is always explicit. Response (`201 Created`): the same `Host` shape used by `listHosts`.
- Server-profile create request: `{ name, host_id, ssh_identity_id, username_override?, tags? }`. `username_override` is included on the wire ONLY when non-null and non-empty (matches the existing integration-test shape so the backend's "omitted == null" behavior is exercised). `tags` is always sent (defaulting to `[]`). Response (`201 Created`): the same `ServerProfile` shape used by `listServerProfiles`.

**UX copy (load-bearing).**

- Host panel intro: "A host is a metadata-only target definition" and "No SSH connection is attempted. Host-key trust and auth-check happen per-profile (panels appear under each profile row after creation)."
- Profile panel intro: "A server profile binds a host, a username, and an SSH identity into a single connect target" and "Creating a profile does NOT trust the host key, does NOT verify SSH authentication, and does NOT install the public key on the target server. Run host-key trust and then auth-check on the new profile row after it appears."
- The view header and footer are updated with the same load-bearing claim: creation here does not imply trust or reachability.

**Stable selectors.** New `data-testid` hooks: `servers-create-host-{open,close,panel,form,display-name,hostname,port,username,submit,error,success}` and `servers-create-profile-{open,close,panel,form,name,host,identity,username-override,tags,submit,error,success,blocked}`.

**Future work (explicit out-of-scope for this slice).**

Edit / delete forms for hosts and profiles; terminal session launch from the production shell; password bootstrap / `ssh-copy-id`; `username_override` / `tags` editing on existing profiles; per-row "view details" / `get_by_id` panels; mobile/Tauri shell integration. Each is a separate slice. (Host-key preflight + trust UI and auth-check UI are now wired — see "Production host-key preflight & trust UI" and "Production SSH auth-check UI" below.)

### Production host-key preflight & trust UI

The next production-safe security flow on the Servers view: an operator can run `host-key-preflight` for a server profile, see the captured fingerprint and trust classification, and explicitly trust an unknown key by confirming the fingerprint. This is NOT auth-check, NOT terminal launch, and NOT automatic trust-on-first-use.

**Scope (load-bearing — this slice).**

1. **Per-profile "Host key" panel** is rendered inside each profile row on `ServersView.svelte` via the `HostKeyPanel.svelte` component. The panel exposes a "Run host-key preflight" button, a status badge (`Not trusted` / `Trusted` / `Changed`), the captured key type, the captured `SHA256:<base64>` fingerprint (selectable / copyable), and — only for the `unknown` outcome — a fingerprint-confirmation input + "Trust this host key" button. The panel holds local Svelte state ONLY (no global stores, no router, no polling, no auto-retry).
2. **`hostKeyPreflight(profileId, options)` and `trustHostKey(profileId, expectedFingerprint, options)`** in `lib/api/serverProfiles.ts` are the single client entry points. Each parses the response with a field-by-field DTO guard (`parseHostKeyPreflightResponse` / `parseTrustHostKeyResponse`) so a stray `private_key` / `encrypted_private_key` smuggled onto a wire body cannot reach the parsed object. Neither helper throws, logs raw response bodies, or echoes wire / transport detail through any user-facing string.
3. **Trust is NEVER auto-issued.** The "Trust this host key" button is enabled only when ALL of: (a) the most recent preflight returned `unknown`; (b) the captured fingerprint is non-empty AND passes the local `isValidFingerprintShape` shape check; (c) the operator has typed the captured fingerprint into the confirmation input AND it matches the captured value byte-exactly. `trustGateForPreflight(preflight)` is the pure function that decides this; `fingerprintConfirmationMatches(captured, confirmation)` is the pure function that compares the strings (case-significant — base64 is case-significant).
4. **`changed` and `revoked` outcomes refuse trust.** `changed` is a wire status and the UI surfaces it as a non-actionable refusal. `revoked` is NOT a wire status today — the backend collapses revoked-and-reappearing keys to `unknown`, then refuses the trust request with `409 conflict { entity: "host_key" }`. The UI treats `revoked` ONLY as a derived trust-rejection reason, deferred to the trust-error formatter, never as a parsed-status value. The trust-error formatter collapses `409` to a single deliberately conservative message ("the host key changed, was revoked, or no longer matches the fingerprint shown — re-run preflight before trying again") because the wire body cannot distinguish the three sub-cases.
5. **Client-side fingerprint shape check.** `isValidFingerprintShape(fp)` mirrors the backend's `validated_expected_fingerprint` (`crates/relayterm-api/src/dto/preflight.rs`): must start with `SHA256:`, length 8..=128, no whitespace or control characters. The `trustHostKey` helper refuses a malformed fingerprint with `{ kind: "validation", reason: "invalid_fingerprint_shape" }` BEFORE any wire round-trip. Backend remains authoritative.
6. **No backend changes.** The existing `POST /api/v1/server-profiles/:id/host-key-preflight` and `POST /api/v1/server-profiles/:id/trust-host-key` routes already return the wire shapes the new helpers parse; the slice is purely a frontend addition.
7. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- `parseHostKeyPreflightResponse` and `parseTrustHostKeyResponse` build their DTOs field-by-field, so a backend bug or hostile fixture that includes `private_key` / `encrypted_private_key` on a 200 response cannot smuggle them onto the parsed object. Sentinel-string redaction tests in `tests/hostKeyApi.test.ts` pin this for both parsers (the parsed object, `JSON.stringify` of the parsed object).
- `describePreflightError` and `describeTrustHostKeyError` are functions of `kind` + `status` + `code` ONLY. Sentinel-string tests pin that they NEVER echo the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure, across `400`, `401`, `404`, `409`, `502`, `503`, and unknown statuses, plus `transport` and `malformed_response`.
- Helpers do NOT log raw response bodies. Tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure for both `hostKeyPreflight` and `trustHostKey`.
- Host-key fingerprints are public-ish security metadata; they are deliberately rendered. Identity-side material (encrypted blob, decrypted PEM) is never on the wire for either route, never declared on either DTO, and never reachable from the panel.

**Wire shapes (mirror of `crates/relayterm-api/src/dto/preflight.rs`).**

- Preflight request: empty body. Response (`200 OK`): `{ profile_id, host_id, hostname, port, host_key_status: "unknown" | "trusted" | "changed", host_key_type, host_key_fingerprint, message }`.
- Trust request: `{ "expected_fingerprint": "SHA256:<base64>" }`. Response (`200 OK`): `{ known_host_entry_id, host_id, host_key_type, host_key_fingerprint, trusted_at }`.

**UX copy (load-bearing).**

- Preflight disclaimer (`PREFLIGHT_DISCLAIMER`): "Preflight verifies the server's host key during SSH key exchange. It does not authenticate, does not open a terminal, and does not install your public key."
- Trust disclaimer (`TRUST_DISCLAIMER`): "Only trust if the fingerprint matches what you expect for the server. RelayTerm will not overwrite a changed or revoked host key automatically."
- `unknown` description: "Host key was captured during SSH key exchange, but no pinned entry matches it. Verify the fingerprint matches what you expect for this server before trusting it."
- `trusted` description: "Host key matches an active pinned entry. SSH authentication and terminal launch are still future work."
- `changed` description: "Host key differs from the pinned entry for this host. RelayTerm will not overwrite a pinned key automatically. This may indicate server reinstallation, key rotation, or a possible man-in-the-middle."
- The success message after a trust action explicitly disclaims auth and terminal launch: "Host key pinned. … SSH authentication and terminal launch are still future work."

**Stable selectors.** New `data-testid` hooks on `HostKeyPanel.svelte`: `host-key-panel`, `host-key-preflight-button`, `host-key-idle`, `host-key-preflighting`, `host-key-preflight-error`, `host-key-status-badge` (with `data-status` attribute), `host-key-status-description`, `host-key-fingerprint`, `host-key-already-trusted`, `host-key-changed-refused`, `host-key-confirm-input`, `host-key-confirm-mismatch`, `host-key-trust-button`, `host-key-trust-error`, `host-key-trusted-success`. The panel root carries `data-profile-id` for per-row targeting.

**Future work (explicit out-of-scope for this slice).**

Terminal session launch from the production shell; changed-host-key override / re-pin UI; revoked-entry recovery UI; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI; mobile/Tauri shell integration; backend VT observer. Each is a separate slice. (SSH auth-check UI is now wired — see "Production SSH auth-check UI" below.)

### Production SSH auth-check UI

After a host key has been pinned and trusted (preceding section), an operator can run an SSH auth-check from the production Servers view to confirm the configured `ssh_identity` actually authenticates against the target. This is NOT a terminal launch, NOT a password bootstrap, NOT a private-key import, and NOT a real auth/user-login UI.

**Scope (load-bearing — this slice).**

1. **Per-profile "Auth-check" panel** is rendered inside each profile row on `ServersView.svelte` via the `AuthCheckPanel.svelte` component, immediately below the existing `HostKeyPanel`. The panel exposes a single "Run auth-check" button, a loading indicator, a status badge keyed off the wire status (`Authenticated` / `Auth rejected` / `Host key not trusted` / `Host key changed` / `Connection failed`), a one-line operator-facing description, the `checked_at` timestamp, and — only on `authentication_succeeded` — a static success footnote that explicitly disclaims terminal launch. The panel holds local Svelte state ONLY (no global stores, no router, no polling, no auto-retry).
2. **`authCheckServerProfile(profileId, options)`** in `lib/api/serverProfiles.ts` is the single client entry point. It posts an empty JSON body to `POST /api/v1/server-profiles/:id/auth-check`, parses the response with `parseAuthCheckResponse` (a field-by-field DTO guard), and returns either `{ ok: true, check }` or `{ ok: false, error }`. It does NOT throw, does NOT log raw response bodies, and does NOT echo wire / transport detail through any user-facing string.
3. **Auth-check NEVER opens a PTY, runs a shell, executes a command, persists a terminal session, or installs the public key.** The success copy explicitly disclaims that scope so the operator cannot mistake "credentials work" for "terminal ready". `terminalLaunchWouldBeAllowed(status)` is the single pure helper that names the (currently empty) bridge to a future terminal-launch slice — it returns `true` only on `authentication_succeeded` and is advisory, not a gate.
4. **Trusted host key is a precondition, surfaced as a diagnostic outcome.** The wire `host_key_unknown` and `host_key_changed` statuses arrive as 200-OK typed `status` values, NOT HTTP errors. The UI renders them as "trust the host key first" / "the host key changed; investigate before continuing" — never as an internal error and never as a generic failure. The host-key panel above continues to be the single deliberate trust-issuance surface; auth-check never auto-runs preflight or auto-trusts.
5. **No backend changes.** The existing `POST /api/v1/server-profiles/:id/auth-check` route already returns the wire shape the new helper parses; the slice is purely a frontend addition.
6. **Architecture rule preserved.** No import added under `lib/app/` touches `lib/dev/` or any renderer adapter; `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- `parseAuthCheckResponse` builds its DTO field-by-field, so a backend bug or hostile fixture that includes `private_key` / `encrypted_private_key` on a 200 response cannot smuggle them onto the parsed object. Sentinel-string redaction tests in `tests/authCheckApi.test.ts` pin this on the parsed object and on `JSON.stringify` of the parsed object.
- `describeAuthCheckError` is a function of `kind` + `status` + `code` ONLY. Sentinel-string tests pin that it NEVER echoes the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure, across `401`, `404`, `500`, `503`, and unknown statuses, plus `transport` and `malformed_response`.
- The helper does NOT log raw response bodies. Tests pin `console.log/warn/error` as untouched across success, HTTP failure, and transport failure.
- The UI status formatters (`authCheckStatusLabel`, `authCheckStatusDescription`, `authCheckStatusTone`, `terminalLaunchWouldBeAllowed`, `AUTH_CHECK_DISCLAIMER`, `AUTH_CHECK_SUCCESS_FOOTNOTE`) are pure functions of `status` only — no I/O, no Svelte state, no side effects. Tests pin that none of them mention `private_key` / `encrypted_private_key` and that the success copy never implies a PTY, shell, command execution, or terminal/session readiness.

**Wire shape (mirror of `crates/relayterm-api/src/dto/auth_check.rs`).**

- Auth-check request: empty body. Response (`200 OK`): `{ profile_id, host_id, ssh_identity_id, status, message, checked_at }`. `status` ∈ `authentication_succeeded | authentication_failed | host_key_unknown | host_key_changed | connection_failed`. `message` is a static, server-supplied string keyed off `status`; the UI may render it but does not depend on its exact wording (the local `authCheckStatusDescription` helper is the single source of truth for rendered status copy).

**UX copy (load-bearing).**

- Auth-check disclaimer (`AUTH_CHECK_DISCLAIMER`): "Auth-check verifies that the configured SSH identity authenticates against the server. It requires a trusted host key first. It does not open a terminal, does not run commands, and does not install your public key."
- Success footnote (`AUTH_CHECK_SUCCESS_FOOTNOTE`): "Credentials worked for SSH authentication. Terminal launch is still a separate action and is not yet implemented in the production shell."
- `authentication_succeeded` description: explicitly disclaims PTY allocation, command execution, and terminal-launch. Phrasing: "SSH public-key authentication succeeded for the configured username. No PTY was allocated and no command was executed. Terminal launch is a separate, deliberate action."
- `authentication_failed` description: names the wrong-key / wrong-user / `authorized_keys`-not-installed diagnostic without surfacing peer banner detail.
- `host_key_unknown` description: surfaces the trust-host-key precondition explicitly ("Run host-key preflight and trust the captured fingerprint above before re-running auth-check") and never implies authentication was attempted.
- `host_key_changed` description: warns about server reinstallation, key rotation, or man-in-the-middle, and explicitly states auth was not attempted.
- `connection_failed` description: names the SSH-transport-layer cause (refused, timeout, unreachable) without leaking peer detail.
- The host-key panel's `trusted` description and `trusted` success message now point operators to the auth-check panel below: "Run auth-check below to confirm the configured SSH identity authenticates. Terminal launch is still future work."
- The Servers view header and the bottom "future work" footer are updated in lockstep so neither still claims auth-check is future work.

**Stable selectors.** New `data-testid` hooks on `AuthCheckPanel.svelte`: `auth-check-panel` (root, also carries `data-profile-id`), `auth-check-run-button`, `auth-check-idle`, `auth-check-checking`, `auth-check-error`, `auth-check-status-badge` (with `data-status` and `data-tone` attributes), `auth-check-checked-at`, `auth-check-status-description`, `auth-check-success-footnote`.

**Future work (explicit out-of-scope for this slice).**

Terminal session launch from the production shell; changed-host-key override / re-pin UI; revoked-entry recovery UI; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI; mobile/Tauri shell integration; backend VT observer; auth-check history / audit-log surfacing in the UI. Each is a separate slice.

### Live SSH PTY bridge contract

After the host key is pinned and trusted (preceding section), an operator may open a `terminal_session` that is backed by a **live SSH PTY**. The create flow does the metadata write AND starts the PTY in one shot; if any precondition fails the row is transitioned to `closed` with a `closed { reason: ssh_start_failed, category }` event.

**Scope (load-bearing — this slice).** A successful create + attach attests ONLY to:

1. The (server_profile, host, ssh_identity) trio resolves and is owned by the caller.
2. The host has at least one active, trusted, non-revoked `known_host_entries` row.
3. The vault decrypted the identity's `encrypted_private_key` to a valid OpenSSH PEM.
4. The SSH transport reached the target, the captured host key matched an accept-pin in `check_server_key`, public-key authentication succeeded, an interactive PTY was allocated, and the user's default login shell started.
5. WebSocket attachments stream raw PTY bytes (base64-encoded inside the JSON `output` frame) from the remote shell, and forward `input`/`resize` to the SSH PTY.

It does **NOT** yet provide:

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

#### Detached-session TTL contract (load-bearing)

A live SSH PTY survives the **last** client detach for a bounded TTL window so a brief reconnect can pick up where it left off without losing the remote shell. The current policy:

- **TTL duration**: `relayterm_terminal::DETACHED_LIVE_PTY_TTL = 30s`. Pinned in the manager crate; tests inject a sub-second value via `TerminalSessionManager::with_detach_ttl`. Any change goes through this constant — there is no per-session override on the wire.
- **Scope**: in-memory only. The replay buffer, the TTL timer task, and the live `russh::Channel` all live in the orchestrator's runtime registry. Postgres holds the `terminal_sessions` row and the lifecycle event log; it does NOT hold any of the runtime state.
- **Backend restart**: drops every detached session along with the rest of the runtime registry. A pre-restart `detached` row is operator-visible until it's explicitly closed via `POST /:id/close`. A restart is therefore equivalent to an immediate TTL expiry from the client's perspective.
- **No durable terminal output**: PTY bytes are mirrored only into the in-memory replay buffer alongside the broadcast channel; nothing about TTL persistence puts any byte into Postgres.

The orchestrator's [`TerminalSessionManager::detach_attachment`] is the single lifecycle entry point for any detach (explicit `Detach` frame or socket-drop cleanup tail). Its policy:

1. Detach the attachment first — `detach_session` is COALESCE-on-`detached_at` so the first call wins on the row, the `detached` event fires exactly once, and the runtime entry is removed.
2. **If this was the last attachment of a live PTY, schedule a TTL close**. The manager transitions the row to `detached`, spawns a `tokio::sleep(DETACHED_LIVE_PTY_TTL)` task that calls `close_session` on wake, and stores the `JoinHandle` on the live runtime so it can be aborted later. The PTY, the broadcast channel, and the replay buffer all stay alive.
3. If the session has no live PTY (stub session, or PTY already torn down by the forwarder when the remote shell exited) the manager does NOT schedule a close — there's nothing live to reap.
4. If other attachments remain (multi-client read attach is future work, but the registry is shaped for it) the manager does NOT schedule a close.
5. If the detach observed `already_detached == true`, the manager does NOT install a second timer — that's the path that runs when an explicit `Detach` frame and the WebSocket cleanup tail both fire. The original deadline is preserved so the close doesn't drift forward on every cleanup-tail run.

Reattach within the TTL window: [`TerminalSessionManager::attach_session`] cancels the pending close task BEFORE writing the new attachment row, transitions the row from `detached` back to `active`, and appends a `reattached` lifecycle event. The client sees the standard `SessionAttached(Active)` frame and can issue an `Attach { last_seen_seq: n }` to drive the replay handshake.

TTL expiry: the spawned task wakes after `DETACHED_LIVE_PTY_TTL`, re-checks under lock that no reattach happened in the meantime (a reattach would have aborted the handle), and calls `close_session`. `close_session` is idempotent so a racing wake-up after an explicit close still produces exactly one `Closed` event.

Wire-side behaviour:

- Client `Detach`: server emits `SessionDetached`, then closes the WebSocket. **No** `SessionClosed` is sent — the session enters the TTL window. The client SHOULD expose a "reconnect with `last_seen_seq`" affordance for the duration of the window.
- Client `Close`: server emits `SessionClosed`, closes the WebSocket, and cancels any pending TTL task. Idempotent at the route layer (`POST /:id/close` returns `already_closed = true` on a second call) and at the manager.
- Socket drop without an explicit `Detach`/`Close`: the cleanup tail fires `detach_attachment`, which writes the `detached` event AND schedules the TTL close if this was the last live attachment. The PTY survives the bounded reconnect window.
- Reattach during the TTL window: the WebSocket upgrade succeeds, `SessionAttached { status: Active }` lands, and the row transitions back to `active` with a `reattached` event.
- Reattach AFTER the TTL window: the upgrade gate sees a `closed` row and returns `409 conflict { entity: "terminal_session" }`.
- Race coverage: explicit `Detach` followed by socket-drop cleanup tail writes exactly one `Detached` event and installs exactly one TTL timer. Duplicate calls to `close_session` from any source (explicit close + late TTL wake) still write exactly one `Closed` event.

Replay interaction during reconnect: the in-memory replay buffer is held inside the live runtime entry — it survives the detach alongside the PTY for the TTL window. A reattach with a bookmark inside the buffer's window receives the standard `replay_start` → buffered `output` → `replay_end` handshake. A bookmark older than the buffer's oldest retained frame surfaces `replay_window_lost` and the handler continues live attach. The replay window is identical to the TTL: lose the PTY, lose the buffer.

Future-VT-snapshot relationship: when the libghostty-vt observer slice lands, the snapshot will become an additional resume surface for clients that want a structured grid (e.g. for fast-forward), but the byte-replay contract above is the load-bearing one for renderer-neutral catch-up. The TTL window is the same for both — the snapshot lives inside the same live runtime entry.

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

##### Dev reconnect UX (load-bearing — diagnostic surface)

The lab surfaces the detached-TTL window, the replay handshake, and the explicit-close path so an operator can manually validate the contracts in "Detached-session TTL contract" and "Output sequence + in-memory replay buffer contract" without leaving the browser. State, button enablement, TTL text, and replay-event formatting are pure helpers in `apps/web/src/lib/dev/liveTerminalState.ts`; the Svelte component holds only the imperative glue. Tests in `apps/web/tests/liveTerminalState.test.ts` pin every rule named below.

- **Lab phase model**: `idle | connecting | attached | replaying | detached | reconnecting | closed | expired | error`. `replaying` is a sub-state of `attached` (between `replay_start` and `replay_end`/`replay_window_lost`); `detached` covers BOTH server-frame `session_detached` AND a local disconnect-without-close while the local TTL clock is unexpired; `expired` means the local TTL clock has elapsed (the server may or may not have closed — only a reconnect attempt distinguishes); `reconnecting` is the brief window between `teardown()` and the next `attach` resolving. The phase is the lab's only operator-visible status badge — the underlying `TerminalSessionClient.state` is shown alongside it for cross-reference but is not the load-bearing label.
- **TTL clarity**: an `~Ns remaining` countdown is shown only when the lab has observed a detach (server frame OR local disconnect-no-close). The text is ALWAYS labelled `approximate (local clock)` because the backend's exact remaining TTL is not on the wire — `describeTtlWindow` never claims server authority. The label flips to `TTL elapsed locally; reattach may 409 (server-truth, not local)` once the local clock crosses the deadline. The visible countdown clamps to a 1-second floor — `0s` would imply a server-confirmed close the lab cannot prove. The TTL constant in `liveTerminalState.ts` (`DETACHED_TTL_MS = 30_000`) is duplicated from `relayterm_terminal::DETACHED_LIVE_PTY_TTL` deliberately; backend value isn't on the wire and we don't poll for it.
- **Reconnect controls**: four affordances cover the manual-test surface — `disconnect (no close)` drops the WebSocket without sending `Close` so the server enters its TTL window; `close` sends the wire `Close` frame so the PTY ends immediately and the TTL is bypassed; `reconnect with last_seen_seq` re-attaches with the lab's tracked bookmark to exercise replay; `reconnect without bookmark` re-attaches with `last_seen_seq: null` to exercise the brand-new-attach path (no replay request). Manual session id entry remains supported throughout — the workbench-driven auto-attach is just one path into the same controls. Button enablement is centralised in `computeEnablement` and pinned by tests; wire-frame buttons (ping, applyResize, detach, close, disconnect-no-close) require an attached client; `connect` requires a fresh phase (`idle | closed | expired | error`) AND a non-empty session id; reconnect buttons disable while the wire is already live or a reconnect is in flight.
- **Replay event visibility**: `replay_start`, `replay_end`, and `replay_window_lost` log lines are seq-metadata-only. `formatReplayWindowLost` carries `requested_seq`, `oldest_available_seq` (`null` preserved for an empty buffer), and `latest_seq` only — the missed bytes are unrecoverable from this surface by design and the formatter never hints at them. Replayed `output` frames continue to log via `outputLogText (seq + bytes count only)` — same redaction as live frames, and the renderer is still the only surface that writes the bytes.
- **Closed / expired-session behavior**: `closed` is reached on a server `session_closed` frame; `expired` is reached when the local TTL clock crosses the deadline without a reconnect. Both are terminal-on-this-session phases; the lab disables wire-frame buttons in either, but `connect`, `reconnect with last_seen_seq` (when the bookmark is positive), `reconnect without bookmark`, and `dispose` remain enabled — operator-driven reconnect attempts are how the lab teaches the wire signal back. A reattach against a server-side-closed session surfaces as a 409 from the upgrade gate; a reattach against a session whose live PTY runtime is gone surfaces via the standard error path. The lab does NOT poll for server status because the contract is "let the wire signal teach you."
- **Logging and reflection prohibitions** (re-affirmed for the dev surface): the lab's event log NEVER carries raw input bytes (`redactInputLogText` doesn't even accept the payload), NEVER carries raw `output` bytes (length only via `outputLogText`), NEVER decodes-and-echoes a malformed base64 frame (`safeDecodeOutput` returns a typed `invalid_base64` failure with no payload), and NEVER includes payload-shaped fields in replay-event formatters. The redaction sentinel pattern from `labLog.test.ts` is reused in `liveTerminalState.test.ts` as defence-in-depth.
- **Production UI is still future work**. The lab is a manual-validation surface for the reconnect/replay/TTL contracts; the polished terminal workspace (host/profile picker, theme/preferences persistence, mobile-first reconnect UX) is a separate, deliberate slice. Production builds drop the lab via dead-code elimination — `import.meta.env.DEV` gates every call site.

##### Renderer comparison diagnostics (load-bearing — diagnostic surface)

The lab gains a renderer/session diagnostics panel so xterm and ghostty-web can be compared meaningfully WHILE exercising the same RelayTerm protocol path. The panel is dev tooling — NOT a benchmark suite — and the contract is honest about that. State and update functions live in pure helpers in `apps/web/src/lib/dev/rendererDiagnostics.ts`; the Svelte component holds only imperative glue. Tests in `apps/web/tests/rendererDiagnostics.test.ts` pin every rule named below.

- **Same protocol path**: the diagnostics surface intercepts events at the lab's existing seam — input/output/replay/lifecycle handlers, renderer mount/dispose — and never asks the backend or `terminal-core` for anything different. Comparing renderers exercises the SAME `TerminalSessionClient`, the SAME wire frames (binary `RTB1` for input/output, JSON for control), and the SAME replay handshake. Any divergence between renderers is therefore a renderer-side observation, not a protocol fork.
- **Diagnostics tracked**: currently selected renderer + label; renderer mount start/end timestamps and duration; mount count and dispose count; input frames + bytes (UTF-8 byte count via `inputByteLength`); output frames + bytes (decoded byte length); resize sends + acks; ping + pong counts; `replay_start` / `replay_end` / `replay_window_lost` counts; attach / detach / close counts; error count; highest observed `output` seq; mirrored `last_seen_seq`; latest `TerminalSessionState`. All values are diagnostic and approximate — clock skew, browser focus, GC pauses, and renderer mounting strategy all affect what the operator sees.
- **Honest measurement wording**: the panel header carries the disclaimer `dev diagnostics, not a benchmark — browser/machine/renderer/font/workload all affect numbers`. The same string is the `disclaimer` field on the JSON summary so a copied summary cannot be quoted as a benchmark out of context. Mount duration is reported in milliseconds with no claim of statistical rigour; the lab makes no claim about steady-state throughput, frame budget, or per-glyph latency.
- **Logging and reflection prohibitions** (re-affirmed for the diagnostics surface): `recordInput` and `recordOutput` accept ONLY a byte count (and seq for output); the function signatures cannot accept the payload at all. The JSON summary returned by `summarizeDiagnostics` is metadata-only by construction — no payload-shaped fields, no base64 strings, no `data` / `payload` / `bytes_b64` keys. The redaction sentinel pattern from `labLog.test.ts` is reused in `rendererDiagnostics.test.ts` as defence-in-depth.
- **Reset and copy affordances**: `reset diagnostics` zeroes counters, mount/dispose telemetry, and the diagnostics window's started-at clock — but PRESERVES the currently selected renderer and the most recently observed client state, so a reset mid-session does not surprise-flip the panel. `copy diagnostics JSON` copies a `summarizeDiagnosticsAsJson` snapshot to the clipboard; if `navigator.clipboard.writeText` is unavailable or denied, the lab falls back to writing the same JSON into the event log so the operator can copy it from there. The JSON is metadata-only, so the fallback log line still satisfies the redaction rule.
- **Renderer scrollback is NOT preserved across renderer switches** in this slice. Switching renderer disposes the previous adapter and mounts the new one against the same `TerminalSessionClient`; the previous renderer's grid/scrollback is dropped. The lab states this inline in the diagnostics panel so an operator does not mistake the cleared grid for a session bug. Persisting scrollback across renderer changes is future work.
- **Production bundle behavior**: the diagnostics module sits behind `import.meta.env.DEV`, like the rest of the lab. Vite inlines the constant; Rollup eliminates the dead branch; the production `apps/web` JS bundle does not contain `rendererDiagnostics` symbols, the `relayterm.dev.renderer-diagnostics.v1` schema string, or the disclaimer. Verified empirically — `pnpm -r build` continues to emit a ~28-29 KB JS bundle.

#### Dev workbench launcher

`apps/web/src/lib/dev/DevTerminalWorkbench.svelte` pairs `POST /api/v1/terminal-sessions` with the existing live-terminal lab so an operator can go from "no session" to "live PTY rendered in xterm" without leaving the browser. It is also strictly diagnostic — the production terminal UI (host/profile picker, polished workspace, replay-aware reconnect) is a separate, deliberate slice and does NOT live here.

- Gated behind `import.meta.env.DEV` at the call site (`App.svelte`). The launcher and the bare lab share the same dead-code-elimination story: the prod JS bundle is unchanged in size when this component is added.
- Manual `server_profile_id` entry only. Listing or filtering host/profile rows is **not** implemented in this slice — the launcher refuses to expand into CRUD UI. The backend's `404 not_found` collapse for foreign-owned ids is the only access check the operator sees.
- Validation runs client-side via `validateCreateRequest` (`apps/web/src/lib/api/terminalSessions.ts`) BEFORE any wire round-trip. Cols/rows are clamped to `1..=4096` and `server_profile_id` must be non-empty; the backend's `invalid_input` is defense-in-depth.
- The typed helper `createTerminalSession` issues the POST and parses the response into a small typed shape. Unknown fields in the response are ignored (forward-compat); a missing required field collapses to `malformed_response`. The helper's status summary is `code/status` only — the wire `message` field is never echoed into the launcher's status line, and operator-facing detail (already redacted to static strings server-side) is dropped at the helper boundary as defense-in-depth.
- On a successful create the launcher remounts the lab via `{#key launchId}` with `initialSessionId` / `initialCols` / `initialRows` / `autoConnect=true`. Each create yields a fresh `TerminalSessionClient` + `XtermRenderer` pair — no client/renderer state survives a relaunch. The lab still owns its own session id state after first render so the operator can edit and reconnect manually.
- Status states: `idle` (no session created yet), `creating` (POST in flight, create button disabled), `created { session, launchId }` (lab auto-attaches), `error { summary }` (typed safe summary, lab is unmounted to its baseline form). `clear status` returns to `idle`; the lab continues to expose its own `dispose renderer + client` for live cleanup.
- Tests in `apps/web/tests/terminalSessionsApi.test.ts` pin the helper's validation, request shaping, response parsing, and error mapping. The redaction sentinel pattern from `labLog.test.ts` is reused — no operator-facing string surfaces a wire message field.

#### Dev renderer smoke verification (manual, MCP-driven)

`apps/web/e2e/SMOKE.md` documents a small browser-level smoke procedure for the dev renderer lab AND the production app shell. It is **manual**, driven by the Playwright MCP server, NOT a committed `@playwright/test` runner — adding committed browsers + a config + a CI surface is more churn than this slice warrants, and the dev lab is intentionally gated out of production. The smoke proves three things and nothing else:

- Under `vite dev`, the production shell renders (sidebar nav, top bar, dashboard view), the dev-mode badge and dev-tools toggle are present, and clicking the toggle reveals the dev workbench / live terminal lab / renderer selector / diagnostics panel with all four renderer options (`xterm`, `ghostty-web`, `restty`, `wterm`). xterm is the default-checked option.
- Selecting each renderer in idle does not crash the page, mirrors the choice into the diagnostics panel's `renderer` cell, and emits a single `[info] renderer set to <label> (idle)` line into the event log.
- Under `vite preview` of the production bundle, the production shell renders (sidebar nav, top bar, dashboard view), AND every dev-only surface is absent: dev-mode badge, dev-tools toggle, dev-tools panel, dev workbench, live terminal lab, renderer selector, renderer options, and diagnostics panel are ALL gone.

Stable selectors are pinned via `data-testid` on the production shell (`app-shell-main`, `top-bar-title`, `nav-{dashboard,terminal,sessions,servers,identities,settings}`, `production-view-dashboard`), the dev-only shell affordances (`dev-mode-badge`, `nav-devtools-toggle`, `dev-tools-panel`), and the dev lab (`dev-terminal-workbench`, `xterm-live-terminal-lab`, `renderer-selector`, `renderer-option-{xterm,ghostty-web,restty,wterm}`, `renderer-diagnostics`, `lab-event-log`). The `idle` choice flip eagerly mirrors `setRenderer(diagnostics, choice)` so the panel reflects the operator's selection without needing a live attach — the docstring on `RendererDiagnosticsState.rendererId` already named this field "currently selected renderer."

The smoke does NOT cover: a real SSH end-to-end browser test (no PTY bytes flow; no backend is required); renderer-specific WASM/WebGPU/DOM behavior (no `mount()` is exercised because no session is attached); benchmarks or perf claims; mobile / Tauri shell; visual regression; persistent renderer preference. Each is a separate, deliberate slice.

#### Future work (explicit out-of-scope for this slice)

Replay buffer + sequence-number-based resume across reconnects; multi-client collaborative attach UX; binary frame format for `Output`; backend-restart recovery for `active` rows; per-session inactivity-timeout reaping for detached PTYs; password-bootstrap / `ssh-copy-id` flow; user-uploaded private keys; SFTP / file-browser surface; session recording; production terminal UI (host/profile picker, polished workspace, theme/preferences persistence); listing / filtering existing sessions in the launcher; committed Playwright runner / CI integration of the dev renderer smoke; real-SSH browser end-to-end test; renderer-specific WASM/WebGPU/DOM smoke. Each is a separate, deliberate slice.

### Output sequence + in-memory replay buffer contract

After the live SSH PTY bridge slice, every PTY output frame the orchestrator forwards over the WebSocket carries a monotonic per-session `seq`. A bounded **in-memory** replay buffer mirrors the same frames so a client that briefly disconnects can resume without losing scrollback. This section defines the wire contract, the bounds, and the explicit non-durability guarantees.

**Scope (load-bearing — this slice).** A client that supplies `last_seen_seq` on its `Attach` frame and whose bookmark is still inside the buffer's window WILL receive every missed `output` frame, in order, before live fanout resumes. It does **NOT** mean:

- Replay survives a backend restart. The buffer is process memory only; a restart drops it and any reconnect after restart that supplies a `last_seen_seq` will surface as `replay_window_lost`.
- Replay survives a session close. The bounded buffer is held inside the live runtime entry; closing the session (explicit `Close`, TTL expiry on the detached-session window, or PTY teardown) drops it alongside the runtime.
- The PTY survives "leave the page, come back hours later" intervals. The detached-session TTL slice gives the PTY a bounded **`DETACHED_LIVE_PTY_TTL = 30s`** window after the last detach (see "Detached-session TTL contract"); reconnect within that window resumes via the in-memory replay buffer, reconnect after it produces a `409` from the upgrade gate. Long-running tmux/screen-style persistence is still future work — the TTL is sized for short disconnects (network blip, tab focus jitter, race between explicit detach and reattach by another tab).
- Multi-writer / collaborative replay semantics. Today only one WS attachment per session is exercised; the buffer is shaped for future fan-in but not promised.

#### Sequence number contract

- The first PTY output frame for a freshly created session has `seq = 1`. Every subsequent frame increments by exactly one (`AtomicU64::fetch_add(1, SeqCst)`). The orchestrator's PTY forwarder is the SINGLE place that assigns `seq`; clients cannot inject an `output` frame.
- Replayed frames re-use the original `seq` they were stamped with on the live wire — they are NOT renumbered. A client that bridges replayed frames into live frames sees one continuous, gap-free stream.
- A gap in `seq` on the live wire signals one of two recoverable conditions: (a) the per-attachment broadcast's bounded fanout queue lagged and dropped frames (the renderer can request replay on the next reconnect); or (b) a `replay_window_lost` frame was emitted and the renderer was told to reset its grid.
- `seq` is per-session: closing and re-creating a session yields a fresh counter starting at `1`.

#### Replay buffer policy

- **Bounds**: default cap is `1024` frames OR `1 MiB` of payload bytes, whichever bound is hit first. Eviction is FIFO from the front after every push. The single most recent frame is always retained even when it overshoots `max_bytes` — dropping it would leave nothing to replay.
- **Storage**: in-process memory only, behind a `Mutex<ReplayBuffer>` shared with the WS handler. The buffer is created when the live PTY runtime is bound and dropped when the runtime is dropped (`close_session`, detached-TTL expiry, or forwarder exit).
- **Privacy invariants** (re-asserted): the buffer stores raw PTY bytes; `Debug` redacts payload bytes to `seq + len` only; the buffer is NEVER mirrored to Postgres, disk, or any log surface; `tracing` macros that format an `OutputFrame` cannot leak the bytes.
- **Input is never buffered**: only PTY OUTPUT frames are written to the buffer. Client `Input` bytes flow straight to the SSH PTY and are never echoed via the broadcast or the buffer.

#### Wire-stable replay messages added

The protocol adds three `ServerMsg` variants that bracket replay:

- `replay_start { from_seq, to_seq }` — emitted once at the start of a successful replay handshake, BEFORE the first replayed `output` frame. Skipped when the snapshot is empty.
- `output { seq, data }` — replayed frames use the SAME variant as live frames, carrying their original `seq`.
- `replay_end { latest_seq }` — emitted once after the last replayed frame. `latest_seq` is the highest `seq` the orchestrator has stamped at the moment replay finished; the next live frame will be `latest_seq + 1`.
- `replay_window_lost { requested_seq, oldest_available_seq, latest_seq }` — emitted exactly once when the client's `last_seen_seq` predates the bounded buffer's oldest retained frame. `oldest_available_seq` is `null` when the buffer is empty. The handler then continues live attach — a lost replay window is NOT a session-fatal error; the renderer is expected to reset its grid before live frames resume.

The `output` frame itself is unchanged on the wire — the existing `seq + data` shape covers both replayed and live frames.

#### Replay handshake on attach

The WebSocket route already attaches the session on upgrade (writes the attachment row, registers the runtime entry, emits `session_attached`). The replay handshake hangs off the FIRST `Attach` frame the client sends:

- `Attach { last_seen_seq: null }` — no replay request, the loop continues straight to live fanout. This is the brand-new-attach path; the server NEVER dumps pre-attach scrollback to a fresh client (that's product policy, not a missing feature).
- `Attach { last_seen_seq: Some(n) }` where `n + 1 >= oldest_available_seq` — the server snapshots the buffer, emits `replay_start` → buffered `output` frames → `replay_end`, then continues live fanout. To avoid double-delivery of frames the broadcast subscriber queued in parallel during attach, the handler tracks a `min_live_seq` floor and drops any incoming live frame whose `seq <= floor`.
- `Attach { last_seen_seq: Some(n) }` where the bookmark predates the buffer's window — the server emits `replay_window_lost { requested_seq: n, oldest_available_seq, latest_seq }` and continues live attach. The floor is raised to `latest_seq` so the renderer doesn't see a frame it was just told it missed.
- A SECOND explicit `Attach` frame on the same socket is a protocol violation and is rejected with `error { code: invalid_message, message: "already attached" }`. The socket stays open.
- An `Attach` against a session whose live PTY runtime is gone (stub session, post-teardown) is treated as a no-op — the bookmark refers to a vanished PTY.

#### Frontend (`@relayterm/terminal-core`) changes

- `TerminalSessionClient.lastSeenSeq` is a public getter that mirrors the highest `seq` observed on the wire (replayed or live). Clients pass it back into the next `attach({ lastSeenSeq })` call to request replay.
- New typed events: `replay_start`, `replay_end`, `replay_window_lost`. The redaction rule still applies — `replay_*` event payloads carry only seq metadata and never the missed bytes.
- The `output` event's payload shape is unchanged (`OutputMsg`); the renderer doesn't need to special-case replayed vs live frames.

#### Diagnostic UI

`apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` surfaces the bookmark in its status header (`last_seen_seq: N`) and gains a "reconnect with last_seen_seq" button that tears down + reconnects with the captured bookmark. The event log gains lines for `replay_start`, `replay_end`, `replay_window_lost` with seq metadata only — never the missed bytes.

#### Logging and reflection prohibitions (re-affirmed)

- The replay buffer's `Debug` impl redacts payload bytes (`<redacted pty output>`); `ReplayRange::Debug` formats `frame_count + latest_seq` only.
- `replay_window_lost` errors carry seq metadata only; the missed bytes are unrecoverable from this surface by design.
- `replay_start` / `replay_end` wire frames are metadata only; the bytes ride on the standard `output` frame variant.

#### Future work (explicit out-of-scope for this slice)

Backend VT observer / `libghostty-vt` snapshot engine (the future replacement for the byte-level replay buffer when a renderer needs structured grid state); durable session recording in Postgres; binary `output` frame format; multi-writer collaborative replay; long-running tmux/screen-style detached-PTY persistence beyond the bounded `DETACHED_LIVE_PTY_TTL`; full mobile/Tauri reconnect UX; renderer-driven replay visualisation (e.g. fast-forward of a long replay buffer).

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

- **Reconnect replay**: when a client reconnects with `(session_id, last_seen_seq)`, the backend MUST send all events with `seq > last_seen_seq` from the ring buffer in order, then resume live streaming. If `last_seen_seq` is older than the ring buffer's tail, the backend returns a `replay_window_lost` error and the client must request a full re-render or close the session. **Status (this slice):** the in-memory replay buffer is in place and the wire path is live. See "Output sequence + in-memory replay buffer contract" for the per-frame contract, the bounded buffer policy, and the explicit non-durability guarantees.
- **Renderer swap**: the user MAY change the active renderer for a session at any time. The new renderer subscribes from the current sequence number; no replay is required.
- **Session lifecycle**: a session enters `detached` immediately on client drop, NOT after a timeout. A `detached` session continues to receive PTY output and append to the ring buffer until the `DETACHED_LIVE_PTY_TTL` window expires or an explicit close arrives. Reconnect inside the window resumes via `last_seen_seq`. Audit log records every state transition. See "Detached-session TTL contract" for the full policy.
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
- libghostty-vt state engine swap (planned; xterm.js drives the baseline). The xterm.js baseline adapter (`@relayterm/terminal-xterm`) and the experimental ghostty-web (`@relayterm/terminal-ghostty-web`), restty (`@relayterm/terminal-restty`), and wterm (`@relayterm/terminal-wterm`) adapters have all landed under `packages/terminal-<name>/`.

## Open questions

TODO — known ambiguities for the owner to resolve. Each: question, options considered, current default if any.

- Replay buffer policy: fixed bytes vs fixed events vs time-window? Default: TODO.
- How long does a `detached` session linger before auto-close? **Default**: `relayterm_terminal::DETACHED_LIVE_PTY_TTL = 30s`. In-memory only (lost on backend restart). See "Detached-session TTL contract" for the full lifecycle.
- Should the renderer choice be per-session or per-device? Default: per-device.

---

> When implementation diverges from this spec, run `/agents spec-sync` to surface the drift. Don't update SPEC.md without intent — the spec leading code is the point.
