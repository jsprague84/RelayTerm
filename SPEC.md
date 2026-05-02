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

**Architecture rule.** Production shell components (`lib/app/`) MUST NOT import anything from `lib/dev/`. The renderer-adapter rule was relaxed once the production terminal workspace landed: `@relayterm/terminal-core` (renderer-neutral) and `@relayterm/terminal-xterm` (the production baseline + its CSS side-effect entry) are allowed in the production shell; the experimental adapters `@relayterm/terminal-{ghostty-web,restty,wterm}` remain banned. This is enforced by `appShellIsolation.test.ts`.

**Package layout.**

```
apps/web/src/lib/app/
├─ AppShell.svelte         # composes sidebar + topbar + view + (dev) tools
├─ SidebarNav.svelte
├─ TopBar.svelte
├─ StatusBadge.svelte
├─ navigation.ts           # NAV_ITEMS, AppViewId, DEFAULT_VIEW, findNavItem
├─ routing.ts              # URL <-> AppViewId helpers (viewForPath, pathForView, ...)
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

Production terminal workspace; production renderer selector; renderer-preference persistence; server / profile / identity CRUD UI; real auth UI (passkey enrollment, session list); mobile/Tauri shell integration; password bootstrap; private-key import; durable session-recording UI. URL routing for the top-level production views is now wired — see "URL-driven production view routing" below for the foundation slice; route parameters, deep-linking, auth routes, and nested routes remain future work. Each is a separate slice.

### Production inventory read-only views

The Servers and Identities views are display-only inventories of `hosts`, `server_profiles`, and `ssh_identities`. They prove the production shell can fetch real backend data through typed, redaction-safe helpers without pulling in the dev lab or any renderer adapter. Create / edit / delete UI and SSH identity deletion remain future work. (Host-key trust, auth-check, SSH identity generation, and terminal launch are now wired — see the per-flow sections below.)

**Scope (load-bearing — this slice).**

1. **Servers view** (`apps/web/src/lib/app/views/ServersView.svelte`) renders two grouped sections: a Hosts list (display name, hostname, port, default username) and a Profiles list (name, linked host summary if resolvable from the fetched hosts, effective username with explicit "(host default)" / "(override)" attribution, tags, and last-connected timestamp). Hosts and profiles are fetched in parallel via `Promise.all`; either failure collapses the whole view to a single safe error summary keyed off the first failed resource.
2. **Identities view** (`apps/web/src/lib/app/views/IdentitiesView.svelte`) renders one row per identity with name, key type, full SHA-256 fingerprint, a one-line public-key preview (`publicKeyPreview` truncates the base64 body to keep tables tight), created-at, last-used-at, and a "Copy public key" button. The button copies ONLY `identity.public_key` — never the fingerprint, never any other field. Clipboard failures collapse to a static `Copy failed` label without echoing origin/permission detail.
3. **Dashboard counts** (`DashboardView.svelte`) shows `hosts` / `profiles` / `identities` / `sessions` cardinality using the same helpers (the dashboard surface is described in full under "Production dashboard summary"). The counts are nice-to-have: any failure collapses to an unobtrusive `—` placeholder so the per-view error surface stays the canonical triage path. No polling.
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

CRUD forms (create / edit / delete) for hosts and profiles; SSH identity deletion / rename; private-key import; terminal session launch from the production shell; route-param detail pages and `get_by_id` round-trips; password bootstrap / `ssh-copy-id`; durable session-recording UI; real auth UI; mobile/Tauri shell integration. Each is a separate slice. (SSH identity generation, host-key preflight + trust UI, auth-check UI, and read-only inventory detail panels are now wired — see the per-flow sections below.)

### Production read-only inventory detail panels

A click-to-select detail panel sits next to each inventory list (Hosts, Server profiles, SSH identities). The panel is read-only — it surfaces fields the list query already loaded, joined client-side against the other lists already on screen. The panel does NOT introduce edit / delete UI, route-param detail pages, `get_by_id` round-trips, or any new backend surface.

**Scope (load-bearing — this slice).**

1. **Selection model.** Each list row exposes a real `<button>` with a stable `data-testid` (`host-row-select`, `profile-row-select`, `identity-row-select`). Clicking the same row twice closes the panel; selecting a different row swaps the panel content. On the Servers view, host and profile selection are mutually exclusive — only one detail panel is visible at a time. Identity selection is independent of the generation panel; the existing "Generate SSH identity" surface is unchanged. The select button carries `aria-expanded` (the disclosure-widget semantic — the button opens a sibling panel; `aria-pressed` is deliberately not also set so screen readers do not see two contradictory ARIA roles) and renders a visible ring when active. A "Close" button (with `data-testid` `host-detail-close` / `profile-detail-close` / `identity-detail-close`) dismisses the panel.
2. **Host detail panel** (`host-detail-panel`) shows display name, hostname, port, default username, created-at, updated-at, and a short-id of the row. It also renders the count and ordered list of profiles whose `host_id` matches — joined entirely from the already-loaded `view.profiles`; no new fetch. Honest copy explicitly states host details do not prove reachability and connection readiness depends on a server profile + host-key trust + auth-check.
3. **Profile detail panel** (`profile-detail-panel`) shows name, the linked-host summary if resolvable from the loaded hosts (otherwise an honest "host not in your inventory"), the linked-identity summary (id + name + key type + fingerprint, joined from the already-loaded identities — no new fetch; honest "identity not in your inventory — metadata available in the SSH Identities view" otherwise), the effective username with explicit "(host default)" / "(override)" attribution, tags, last-connected, created-at, updated-at, and a short-id. The panel does NOT call `get_by_id`, does NOT fetch the linked identity individually, and does NOT infer host-key trust / auth-check / terminal readiness — those facts live on the per-profile state stores already rendered alongside the row.
4. **Identity detail panel** (`identity-detail-panel`) shows name, key type, full SHA-256 fingerprint, a one-line public-key preview (`publicKeyPreview` — the same helper used in the row), created-at, last-used-at (or "never"), short-id, and a `<pre>`-rendered full OpenSSH public key with a deliberate "Copy public key" button. The full key reaches the DOM through exactly one path — the `<pre>` block — and the copy action's value is the typed `public_key` field; `title=` / `aria-*` tooltips are not used to surface key material. The button label collapses to "Copy failed" without echoing browser detail when the clipboard API is unavailable.
5. **Helpers.** `apps/web/src/lib/app/inventory/inventoryDetails.ts` carries the pure projections: `shortId`, `safeDisplayValue`, `hostProfileCount`, `relatedProfilesForHost`, `identitySummary`, `resolveProfileDetail`, `describeReadinessFromKnownState`, `identityPublicDetail`, `publicKeyCopyValue`. Each helper is field-by-field — the SSH-identity helpers re-assert the redaction discipline established by `parseSshIdentity` (`encrypted_private_key` / `private_key` are not declared on the projection types AND cannot smuggle through onto the returned object even when present on the input). `tests/inventoryDetails.test.ts` pins this with redaction sentinels asserted absent from the returned projections, the JSON-stringified projections, and the deliberate copy value.
6. **Architecture rule preserved.** The new module lives entirely under `lib/app/`; no import touches `lib/dev/` or any renderer adapter. `appShellIsolation.test.ts` continues to pass.

**Honesty rules (load-bearing).**

- The detail panel surfaces ONLY data the list query already returned. No `get_by_id` round-trip is added; a future detail-route slice can replace the panel without changing the helper contract.
- Related-object summaries are joins over the supplied list arrays. An unresolved link is rendered honestly ("host not in your inventory" / "identity not in your inventory — metadata available in the SSH Identities view") — the helpers never synthesise a placeholder host, identity, or fingerprint.
- The profile detail panel does NOT imply host-key trust, auth-check success, or terminal-launch readiness from the existence of the profile or the resolution of its links. The advisory line (`describeReadinessFromKnownState`) explicitly names host-key trust and auth-check as separate, still-required steps; it never claims "ready", "trusted", "verified", or "passed".
- Selection state is local to the view component (no URL / route-param coupling, no global store). A page reload, a refresh, or a navigation event resets the detail panel to closed — the URL never carries an inventory id.

**Redaction posture (load-bearing).**

- `IdentitySummary` and `IdentityPublicDetail` (the projection types) do not declare `encrypted_private_key` or `private_key`. The helpers build them field-by-field from typed `SshIdentity` input, so a backend bug or hostile fixture that smuggled a private-key field onto the input cannot reach the projection or any string the panel renders. Sentinel tests pin this against future regressions.
- The full OpenSSH public key reaches the DOM through exactly one path per panel — the `<pre>` block. `publicKeyPreview` is used in the in-card summary so the full key cannot leak through an incidental hover surface, `title=` attribute, or `aria-*` description.
- The copy action's value is the typed `public_key` field; the helper that yields it (`publicKeyCopyValue`) is pure and cannot read or echo any private-key field, even when one is present on the input. Clipboard failure collapses to a static `Copy failed` label without echoing origin / permission detail.
- No helper logs, throws, or formats raw response bodies. The panels never echo wire `message` fields; advisory copy is composed from the helper's own enum-shaped state.

**Future work (explicit out-of-scope for this slice).**

Route-param detail pages (e.g. `/servers/:id`); full-page detail routes; `get_by_id` round-trips and per-detail backend calls; edit / delete / rename UI for any inventory entity; private-key import; password bootstrap / `ssh-copy-id`; live host-key trust / auth-check status surfaced inside the detail panel beyond what the row already renders; multi-tab workspace with sticky detail; mobile/Tauri-specific detail layout; pagination over the inventory list. (Client-side search and filters are now wired — see "Production inventory client-side search & filters" below.) Each is a separate slice.

### Production inventory client-side search & filters

A usability layer over the existing read-only inventory views. Servers and Identities each gain a small filter toolbar above the list that narrows what is rendered. The filter is in-memory only over already-loaded data — no backend search, no pagination, no URL or local-storage persistence.

**Scope (load-bearing — this slice).**

1. **Pure helpers** (`apps/web/src/lib/app/inventory/inventoryFilters.ts`): `normalizeSearchText(input)` (trims, lowercases, collapses internal whitespace runs to a single space; non-string and empty inputs collapse to `""`), `filterHosts(hosts, query)`, `filterProfiles(profiles, hosts, identities, filters)`, `filterIdentities(identities, filters)`, `collectProfileTags(profiles)` (sorted, deduped, case-insensitive), and `countFilteredResults(visible, total, singular, plural?)` (renders a "Showing X of Y <noun>" string, or a shorter "Y <noun>" form when no filter is active). Helpers are field-by-field and never mutate their inputs; an empty filter returns a NEW shallow copy of the source list so callers can rely on result-array ownership.
2. **Servers view filter toolbar** (`servers-filter-toolbar`) renders three controls: `Search hosts` (input matching display name, hostname, port-as-decimal, default username), `Search profiles` (input matching profile name, tags, username override, effective username, linked-host display name + hostname, linked-identity name + fingerprint + key type), and `Profile tag` (select pre-populated with the unique tags currently in use; auto-resets to "All tags" when the active tag disappears from the loaded inventory). A `Clear filters` button is enabled only while at least one Servers filter is active. The Hosts and Profiles result-count badges (`hosts-count` / `profiles-count`) flip to the "Showing X of Y" form when the corresponding filter is active.
3. **Identities view filter toolbar** (`identities-filter-toolbar`) renders one or two controls: `Search identities` (input matching name, fingerprint, and key type) and a `Key type` select (rendered ONLY when more than one key type appears in the loaded list). A `Clear filters` button is enabled only while at least one identity filter is active. The Identities result-count badge (`identities-count`) flips to the "Showing X of Y" form when the corresponding filter is active.
4. **Empty-filter states.** Hosts list renders `hosts-filter-empty` ("No hosts match this filter."), profiles list renders `profiles-filter-empty` ("No profiles match this filter."), identities list renders `identities-filter-empty` ("No identities match this filter."). These are distinct from the existing zero-rows empty states (`hosts-empty` / `profiles-empty` / `identities-empty`) so the operator can tell "you have nothing" apart from "your filter excluded everything."
5. **Detail-panel coexistence.** Selecting a row that is later filtered out of the list keeps the detail panel open; the panel renders an honest banner (`host-detail-hidden-by-filter` / `profile-detail-hidden-by-filter` / `identity-detail-hidden-by-filter`) telling the operator the row is hidden by the active filter and pointing at the controls that brought it there. Clearing the relevant filter brings the row back into the list without re-selecting it.
6. **Architecture rule preserved.** The new helpers and toolbars live entirely under `lib/app/inventory/` and the two view components; no import touches `lib/dev/` or any renderer adapter. `appShellIsolation.test.ts` continues to pass.

**Redaction posture (load-bearing).**

- The matching haystack for an SSH identity (in `filterIdentities` AND in `filterProfiles` for a profile's linked identity) is built field-by-field from `name`, `fingerprint_sha256`, and `key_type` only. The OpenSSH `public_key` body is deliberately NOT in the haystack — substring matching against a 400-char base64 string is rarely useful and would invite a future preview surface that echoes the matched fragment. Sentinel tests in `tests/inventoryFilters.test.ts` pin that a hostile public-key body cannot drive a match.
- A hostile fixture that smuggles `private_key`, `encrypted_private_key`, `session_output`, or `access_token` onto an SshIdentity input cannot reach the matching haystack — the haystack reads only typed properties. The result array still references the input object (the helpers are pure, not deep-clones), but the helpers do not surface those fields through any computed string. The redaction-sentinel tests pin that a query against any of those sentinel substrings returns the empty array.
- Helpers do NOT log search queries. The search inputs are user-typed UI text; no path here writes them to the console, throws them inside an Error, or echoes them through a wire body.
- The filter toolbars do NOT alter the redaction posture of the existing detail panels — `private_key` / `encrypted_private_key` remain undeclared on the typed DTOs, and the filter helpers do not declare them either.

**Stable selectors.** New `data-testid` hooks: `servers-filter-toolbar`, `servers-host-search`, `servers-profile-search`, `servers-profile-tag-filter`, `servers-clear-filters`, `hosts-filter-empty`, `profiles-filter-empty`, `host-detail-hidden-by-filter`, `profile-detail-hidden-by-filter`; `identities-filter-toolbar`, `identities-search`, `identities-key-type-filter`, `identities-clear-filters`, `identities-filter-empty`, `identity-detail-hidden-by-filter`. The existing `hosts-count` / `profiles-count` / `identities-count` selectors continue to identify the result-count badges (now the count-string is sourced from `countFilteredResults`).

**Future work (explicit out-of-scope for this slice).**

Backend-side search / filtering; pagination over inventory lists; URL query-string state for filters (deep-linking a filtered view); saved / starred filters; saved per-user view preferences; multi-tag AND/OR composition (today the tag select is a single exact match); free-text search over deeper fields (e.g. created-at ranges); regex / fuzzy matching; saving a search as a "smart group". Each is a separate slice.

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

### Production terminal launch UI

After a host key has been pinned and trusted AND auth-check has confirmed credentials, an operator can launch a live terminal session from a server profile. This slice ships the FIRST production terminal workspace; it is intentionally minimal — one session at a time, one renderer (xterm baseline), no multi-tab UI, no durable session list, no renderer selector.

**Scope (load-bearing — this slice).**

1. **Per-profile "Launch terminal" action** lives on each profile row in `ServersView.svelte`. The button is enabled by default; the underlying SSH/host-key/auth preconditions are enforced server-side and surface as a typed launch error if missing. Adjacent operator-facing copy explicitly tells the operator to run host-key trust + auth-check first if launch is refused. Per-row launch state (`submitting`, `error`) is keyed on `profile.id` so a launch on one row does not freeze every other row's button.
2. **Production terminal workspace** — `apps/web/src/lib/app/views/TerminalView.svelte` + `apps/web/src/lib/app/terminal/ProductionTerminal.svelte`. The view is a thin wrapper that either renders the workspace component (keyed by `sessionId` so a fresh launch tears down the prior renderer + client cleanly) or shows an honest empty state pointing the operator at the Server profiles view.
3. **Renderer is xterm baseline only.** The production shell imports `@relayterm/terminal-core` and `@relayterm/terminal-xterm` (plus its CSS side-effect via `@relayterm/terminal-xterm/styles`). The experimental adapters — ghostty-web, restty, wterm — remain dev-lab-only; `appShellIsolation.test.ts` is updated to ban only the experimentals (terminal-xterm and terminal-core are explicitly allowed). A production build's tree-shaking confirms zero references to the experimental adapters in the JS bundle.
4. **Cross-view active-launch state lives on the shell.** `AppShell.svelte` owns `activeLaunch: ActiveLaunch | null`. Pressing "Launch terminal" on a profile row calls `createTerminalSession` and, on success, hands the result back to the shell which switches `selected = "terminal"` and stores the new launch. The Terminal view's "Back to servers" button clears `activeLaunch` and switches back to `selected = "servers"`. There is no router (the shell still uses local view-state).
5. **Lifecycle behaviour.**
   - On mount of `ProductionTerminal`: build an `XtermRenderer`, mount it, build a `TerminalSessionClient` over `WebSocketTerminalTransport`, attach via the canonical `wss?://<host>/api/v1/terminal-sessions/:id/ws` URL.
   - Output frames decode through `decodeOutputData` inside a `try/catch`; malformed frames are dropped silently — `m.data` is NEVER logged or echoed.
   - Renderer `onInput` forwards directly to `client.sendInput`; renderer `onResize` forwards directly to `client.sendResize`. xterm's `onResize` fires synchronously inside `Terminal.resize`, so the workspace's manual resize entry points (none in this slice) MUST NOT also call `client.sendResize` — the AGENTS.md "Encountered Lessons" double-emit rule still holds.
   - On unmount: tear down client + renderer without sending a wire `Close` frame. The session enters the bounded detached-TTL window so a re-mount within ~30s can resume from the captured `lastSeenSeq`.
   - **Detach** sends the wire `Detach` frame (TTL window starts).
   - **End session** sends the wire `Close` frame (PTY ends immediately, no TTL).
   - **Reconnect** tears the client down and re-attaches with `last_seen_seq` for replay; disabled until the bookmark is positive.
   - **Disconnect** tears down the local client + renderer without changing the session row (operator-facing equivalent of closing the browser tab).
6. **No backend changes.** The existing `POST /api/v1/terminal-sessions` and `GET /api/v1/terminal-sessions/:id/ws` routes already carry the wire shape this slice consumes; the slice is purely a frontend addition.

**Redaction posture (load-bearing).**

- Raw input bytes (renderer keystrokes / paste) flow straight from `XtermRenderer.onInput` to `client.sendInput`. They are NEVER logged, stashed in any error message, or surfaced through the workspace status line. The redaction rule mirrors the dev lab's pin in `packages/terminal-xterm/tests/xtermRenderer.test.ts`.
- Raw output bytes are decoded only inside the `output` event handler and forwarded to `renderer.write`. The status line shows metadata only — phase, last_seen_seq, and a profile/session label.
- `describeLaunchError` is a function of `kind` + `status` + `code` ONLY. Sentinel-string tests pin that it never echoes the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure, across every error variant (`validation`, `http`, `transport`, `malformed_response`).
- `describeWorkspaceError` is a function of `kind` (and `code` for server errors) ONLY. The wire `ServerMsg::Error.message` is intentionally dropped at the formatter boundary; the backend's static `message` field is wire-stable today, but a future widening must not leak through this surface. Sentinel tests pin this against every `TerminalClientError.kind`.
- Helpers do NOT log raw response bodies, request bodies, or any payload. The workspace component does not `console.*` at all.
- The `data-testid` selectors carry `sessionId` only on `production-terminal[data-session-id]` (a UUID — operator-visible by design); they NEVER carry input/output bytes, decoded text, or any payload-correlated value beyond `last_seen_seq`.

**Architecture rule update.** The "production shell MUST NOT import any `@relayterm/terminal-*` adapter" rule (`appShellIsolation.test.ts`) is relaxed: `@relayterm/terminal-core` is renderer-neutral and is allowed; `@relayterm/terminal-xterm` is the production baseline and is allowed; `@relayterm/terminal-{ghostty-web,restty,wterm}` remain banned in the production shell. The same rule lives in AGENTS.md "Things to avoid" / "Folder conventions".

**TTL and replay limitations (load-bearing copy).**

- Detached sessions survive only briefly — ~30s — pinned to `relayterm_terminal::DETACHED_LIVE_PTY_TTL`. The workspace copy says "~{`DETACHED_TTL_MS / 1000`}s" and labels the local TTL countdown as `approximate, local clock` (the backend's true remaining TTL is not on the wire).
- Replay is the bounded in-memory ring buffer on the backend. A bookmark older than the buffer surfaces as `replay_window_lost`; the workspace renders no special UI for this beyond resuming the live stream.
- A backend restart drops every detached PTY AND its replay buffer. The workspace explicitly does NOT promise resume-across-restart, durable session recording, or backend-side terminal state observation. These are out of scope.

**Stable selectors.** `production-terminal` (root, carries `data-session-id` and `data-phase`), `production-terminal-phase`, `production-terminal-detach`, `production-terminal-close`, `production-terminal-reconnect`, `production-terminal-dispose`, `production-terminal-back`, `production-terminal-ttl-hint`, `production-terminal-closed`, `production-terminal-error`, `production-terminal-viewport`. ServersView gains `profile-launch-terminal` and `profile-launch-error` (with sibling `profile-launch-error-dismiss`) on each profile row.

**UX copy (load-bearing).**

- Workspace status line: `Status <phase>` + `last_seen_seq <n>` only. No raw payload references.
- Detach hint: "Detached. The remote PTY remains alive only briefly (~30s) — reconnect within that window or the session is reaped. Replay is in-memory and not durable across a backend restart."
- Closed hint: "Session ended. Return to the server profile to launch a new one."
- Empty Terminal view: "Launch a terminal from a server profile." plus a 3-bullet honest disclaimer (where to launch from, host-key/auth precondition, TTL/replay limitation).
- ServersView per-row launch button hint: "Launch is enabled by host-key trust and SSH auth-check — run those above first if the launch is refused."

**Future work (explicit out-of-scope for this slice).**

Multi-tab terminal workspace; durable / polished session list; renderer selector in production; renderer-preference persistence; backend VT observer / `libghostty-vt` snapshot for resume-across-restart; durable session recording UI; mobile/Tauri shell integration; password bootstrap / `ssh-copy-id`; private-key import UI; edit / delete UI for hosts and profiles; URL-driven routes / deep-linking; resume-from-detached on browser tab restore. Each is a separate slice.

### Production terminal sessions list/status UI

After the production terminal launch UI shipped, an operator could create and re-attach a single session via the AppShell active-launch state but had no inventory view of their existing sessions. This slice replaces the Terminal Sessions placeholder with a read-only/session-control view that surfaces the live + detached + closed rows the backend already owns and lets the operator reconnect or close any non-terminal row.

**Scope (load-bearing — this slice).**

1. **`SessionsView.svelte`** — production view at `apps/web/src/lib/app/views/SessionsView.svelte`. Replaces the prior `PlaceholderView` shim. Loads sessions on mount and on explicit Refresh; renders explicit loading / empty / error / list states. No polling, no auto-retry storms.
2. **API helpers** — `apps/web/src/lib/api/terminalSessions.ts` gains `listTerminalSessions()`, `closeTerminalSession(sessionId)`, the `TerminalSession` DTO, `parseTerminalSession()`, and the `describeSessionLoadError` / `describeCloseSessionError` formatters. The list helper reuses the shared `fetchJsonList` envelope; the close helper reuses `postJsonItem`. Both compose against `apiErrors.ts` so the redaction rule matches the rest of the inventory surface.
3. **Status helpers** — pure module at `apps/web/src/lib/app/terminal/sessionStatus.ts`. Exports `statusLabel`, `statusTone`, `describeSessionStatus`, `canReconnect`, `canClose`, `showsTtlHint`. Tests live in `apps/web/tests/sessionStatus.test.ts`.
4. **Per-row actions.**
   - **Open** (a.k.a. Reconnect) — enabled when `canReconnect(status)` is true (`active` or `detached`). Clicking calls `onReconnect` which the AppShell wires to `handleLaunch`; the shell sets the existing `ActiveLaunch` and switches `selected = "terminal"`. Reconnect from this list does NOT pre-supply a `lastSeenSeq` — that bookmark only exists when the current frontend has been attached to the same session in the current page lifetime, which `ProductionTerminal` tracks locally via its `lastSeenSeq` state. Honouring the bookmark across navigations would require shell-level persistence (out of scope).
   - **Close** — enabled when `canClose(status)` is true (anything but `closed`). Calls `closeTerminalSession(id)`; on success the row is replaced in place with the parsed close response (no full list refetch — that would steal focus and reset scroll). Per-row close error state is dismissable; the formatter never echoes the wire `message` or transport `Error.message`.
   - **Refresh** — explicit reload button; the only non-mount entry point.
5. **Reconnect handoff.** AppShell already owns `activeLaunch: ActiveLaunch | null`. The Sessions view receives `onReconnect` AND `activeSessionId` (the current `activeLaunch?.sessionId`). If the row's id matches `activeSessionId`, the action button shows "Attached" and is disabled — clicking through would tear down and rebuild the existing attachment, which is a footgun. The Terminal view's `{#key launch.sessionId}` block already remounts cleanly on a different session.
6. **No backend changes.** `GET /api/v1/terminal-sessions`, `POST /api/v1/terminal-sessions/:id/close`, and the WebSocket attach route all already carry the wire shape this slice consumes.

**Honesty rules (load-bearing).**

- **Closed sessions cannot be reconnected.** `canReconnect("closed")` is `false`. The Open button is disabled and the per-status copy says "Session ended … cannot be reconnected. Launch a new session from the originating server profile." Test pin: `sessionStatus.test.ts` asserts the literal "cannot be reconnected" substring.
- **Detached sessions are TTL-bounded.** When `showsTtlHint(status)` is true (only for `detached`), the row renders a disclaimer that names the ~30s TTL, the in-memory replay constraint, and that a backend restart drops everything. Test pin: the helper copy contains both the `30s` window AND the literal `in-memory` substring.
- **Starting sessions are not yet reconnectable.** `canReconnect("starting")` is `false`. The runtime is not yet bound; attaching would race the create call. Per-status copy says reconnect becomes available once the orchestrator promotes the row to active.
- **Active sessions point at the close action without overpromising.** Per-status copy is short: "Session is live on the backend. Open it to attach, or close it to end the PTY immediately."
- **One active terminal at a time.** This is the same limitation the production terminal launch UI already pinned: the AppShell holds a single `ActiveLaunch`. Reconnect from this list overwrites it. Multi-tab is explicit future work.

**Redaction posture (load-bearing).**

- The `TerminalSession` interface declares only safe public fields (`id`, `server_profile_id`, `status`, `cols`, `rows`, `created_at`, `last_seen_at`, `closed_at`). It does NOT declare `private_key` or `encrypted_private_key` — and the parser builds the DTO field-by-field so a stray field on the wire body cannot reach the parsed object. Sentinel test `parseTerminalSession does NOT carry private_key or encrypted_private_key onto the parsed DTO` pins this.
- `describeSessionLoadError` and `describeCloseSessionError` are functions of `kind` + `status` + `code` ONLY. They NEVER echo the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure. Sentinel tests pin both formatters against URL-bearing transport messages and operator-detail-bearing wire bodies.
- The view never shows raw terminal output, replay buffer contents, or any field that could reconstruct input/output. Only safe metadata (id, profile id/name, status, cols/rows, timestamps) reaches the DOM. Helpers do NOT log raw response bodies, request bodies, or any payload.

**Profile name resolution.** The list view fetches `listServerProfiles()` in parallel with the sessions list and maps `server_profile_id → profile.name` for a friendlier label. Profiles-list failure is silent: the view falls back to the short id form (first 8 chars) and renders the sessions anyway. Surfacing the profile error here would be misleading — the page is about terminal sessions.

**Stable selectors.** `production-view-sessions` (root), `sessions-refresh-button`, `sessions-loading`, `sessions-error`, `sessions-empty`, `sessions-list`, `sessions-row` (carries `data-session-id` and `data-status`), `sessions-row-status`, `sessions-row-description`, `sessions-row-ttl-hint`, `sessions-row-reconnect`, `sessions-row-close`, `sessions-row-close-error`.

**Architecture rule.** The view imports only from `lib/app/`, `lib/api/`, and `lib/app/terminal/sessionStatus.ts`. It does NOT import any `@relayterm/terminal-*` adapter — terminal rendering remains the Terminal view's concern. `appShellIsolation.test.ts` continues to enforce this; no rule change.

**Future work (explicit out-of-scope for this slice).**

Multi-tab workspace; durable / persistent session listing across browser sessions; durable session-recording UI / replay player; backend VT observer / `libghostty-vt` snapshot for resume-across-restart; production renderer selector; mobile/Tauri shell integration; URL-driven routes / deep-linking; auto-refresh / live status updates (today the operator presses Refresh); session filtering / search / pagination; admin cross-user view. Each is a separate slice.

### Production terminal settings foundation

The first production-safe local preferences UI for the terminal workspace. An operator can pick a font, font size, line height, cursor shape/blink, scrollback depth, and a small theme preset; the production xterm-baseline workspace honours those preferences on the next session it launches. This is the **local-only** foundation — there is no backend settings API, no per-user/account persistence, no per-server-profile override surface, and no production renderer selector in this slice.

**Scope (load-bearing — this slice).**

1. **Settings model** — `apps/web/src/lib/app/settings/terminalSettings.ts`. The `TerminalSettings` shape is renderer-neutral and maps cleanly onto `BaseTerminalRendererOptions` from `@relayterm/terminal-core`: `fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, plus a `themePresetId` that resolves to a curated `RendererTheme`. The module exports `defaultTerminalSettings`, `parseTerminalSettings`, `normalizeTerminalSettings`, `serializeSettings`, `loadTerminalSettings`, `saveTerminalSettings`, `clearTerminalSettings`, `resolveTheme`, and `settingsToRendererOptions`. Validators clamp / reject rather than throw so a corrupted entry can never lock the operator out of the terminal.
2. **Theme presets** — `apps/web/src/lib/app/settings/themePresets.ts`. Curated set: `relayterm-dark` (the default; visually identical to the pre-slice inline defaults), `alacritty-ish-dark` (deliberately labelled "ish" — NO claim of byte-for-byte parity with upstream Alacritty), `high-contrast`, `solarized-dark`. Each preset is a plain `RendererTheme`. The set is intentionally small until per-profile theming lands.
3. **localStorage-only persistence** — single key `relayterm.terminal-settings.v1`. Adding fields is a breaking change relative to existing entries: bump the key (`v2`) and migrate. The loader collapses every failure path (missing key, JSON parse error, schema mismatch, hostile fixture, storage unavailable) to defaults silently — no `console.*` noise. Out-of-range numerics are clamped; unknown theme ids fall back to the default. Unknown / extra fields are dropped: a hostile fixture that injects `private_key` / `encrypted_private_key` / `session_output` cannot smuggle them onto the parsed object.
4. **Production terminal wiring** — `apps/web/src/lib/app/terminal/ProductionTerminal.svelte` reads settings via `loadTerminalSettings()` once per attach and constructs `XtermRenderer` with `settingsToRendererOptions(settings)`. Mid-session live-updates (re-fit, atlas reset, palette swap on a mounted xterm) are explicit future work; the slice ships "applies on next session" semantics, and the Settings UI says so. The dev lab is unchanged — it has its own `XtermLiveTerminalLab` controls and is not driven by these preferences.
5. **Settings view** — `apps/web/src/lib/app/views/SettingsView.svelte`. Replaces the prior `PlaceholderView` shim. Two-way bindings build a draft `TerminalSettings`; the Save button calls `saveTerminalSettings(normalized)`; the Reset button restores `defaultTerminalSettings()` and persists the defaults. A small inline preview card renders sample shell output using the selected theme, font, and line-height so the operator sees the change before applying it.

**No backend changes.** The slice is purely a frontend addition. No new routes, no schema, no new wire shapes.

**Architecture rule preserved.** The new module lives entirely under `lib/app/settings/`. It does NOT import anything under `lib/dev/` and does NOT import any experimental renderer adapter; only `@relayterm/terminal-core` (renderer-neutral types) is imported. `appShellIsolation.test.ts` continues to enforce both bans.

**Redaction posture (load-bearing).**

- `serializeSettings` writes ONLY the seven documented fields. Sentinel-string tests in `tests/terminalSettings.test.ts` pin that a hostile draft carrying `private_key`, `encrypted_private_key`, `session_output`, or `access_token` cannot reach the persisted JSON — the keys are absent, the values are absent, and the JSON does not match the corresponding key names.
- `parseTerminalSettings` reads ONLY the documented keys; `__proto__` is not honoured for prototype mutation.
- The loader / saver / reset paths NEVER `console.log/warn/error`. Tests pin this against future regressions.
- Settings carry no secrets, no host/profile/identity references, no session ids, and no terminal output. The slice is purely cosmetic preferences.

**Validation bounds (load-bearing copy).**

- `fontSize`: integer 8–32; non-integers rounded; non-finite collapses to the default.
- `lineHeight`: 0.8–2.5, rounded to two decimals (so `1.4 - 0.1` does not drift into `1.299999…`).
- `scrollbackLines`: integer 0–100,000; truncated; non-finite collapses to the default. This is the renderer's visible scrollback only, NOT the backend replay buffer (the operator UI states this explicitly).
- `cursorStyle`: closed set `block | underline | bar`. Anything else collapses to `block`.
- `fontFamily`: stripped of ASCII control characters and trimmed; falls back to the default if empty after stripping; clipped to 256 chars.
- `themePresetId`: must match an entry in `TERMINAL_THEME_PRESETS`; unknown ids collapse to `relayterm-dark`.

**Stable selectors.** `production-view-settings` (root), `settings-terminal-appearance`, `settings-font-family`, `settings-font-size`, `settings-line-height`, `settings-scrollback-lines`, `settings-cursor-style`, `settings-cursor-blink`, `settings-theme-preset`, `settings-preview`, `settings-apply`, `settings-reset`, `settings-status-saved`, `settings-status-failed`. The settings view also hosts the recent-audit panel — see "Current-user audit events read API (landed)" below for `settings-recent-activity*` selectors.

**UX copy (load-bearing).**

- View summary: "Local terminal preferences for this browser. Stored in localStorage only — there is no backend / account settings yet, and these preferences do not sync to other devices. Changes apply to the next terminal session you launch."
- Save success: "Saved locally. Applies to the next terminal session."
- Save failure: "Couldn't save to local storage. Settings stayed in memory only."
- Footer note: "Per-server-profile preferences, custom palettes, keybinding editor, copy/paste policy, production renderer selection, and mobile/Tauri settings are deliberate later slices. Today's settings are stored locally in this browser only."

**Future work (explicit out-of-scope for this slice).**

Backend / account settings persistence; per-server-profile preferences; live-update of an attached terminal (re-fit / atlas reset / palette swap); production renderer selector and per-renderer preferences; keybinding editor; copy/paste policy UI; theme import/export; Alacritty config import; durable session-recording settings; mobile/Tauri-specific settings; custom 16-slot palette editor; theme marketplace. Each is a separate slice.

### Production terminal viewport controls

Polish slice that layers small UX affordances onto the existing production terminal workspace. No backend changes, no new wire messages, no renderer additions: the slice adds renderer-driven controls (focus, fit, local-viewport-clear), a post-attach focus, and stable production copy for "appearance settings apply on the next session" and "copy/paste lives at the browser level for now". The production renderer remains xterm baseline only.

**Scope (load-bearing — this slice).**

1. **Renderer surface — `clear()`.** `XtermRenderer` gains a `clear()` method that delegates to xterm's `Terminal.clear()`. This method is **not** added to the renderer-neutral `TerminalRenderer` interface in `@relayterm/terminal-core` — it is xterm-specific today. The production workspace happily talks to the concrete `XtermRenderer` since it is the only allowed renderer in the production shell; promoting `clear` to the neutral contract is deferred until a second renderer needs it. Tests pin idempotency, pre-mount safety, and post-dispose safety.
2. **Renderer surface — `fit()` already exists.** No interface change. The production workspace calls the existing `XtermRenderer.fit()`; xterm's fit addon synchronously fans out the new dims to the renderer's `onResize` listeners, and the workspace's existing `onResize` subscriber drives the wire `resize` frame (the AGENTS.md "Encountered Lessons" double-emit rule still holds — the Fit button does NOT call `client.sendResize` itself).
3. **Workspace controls** — `apps/web/src/lib/app/terminal/ProductionTerminal.svelte` gains three compact buttons in the existing button row:
   - **Focus terminal** — calls `safeFocus(renderer)` so the renderer takes keyboard focus.
   - **Fit** — calls `safeFit(renderer)`; the renderer's own resize fanout drives the wire `resize`.
   - **Clear local viewport** — calls `safeClearViewport(renderer)`. Local-only; never sends a wire frame, never mutates the backend replay buffer, never asks the remote shell to run `clear`.
   The existing **Detach / End session / Reconnect / Disconnect / Back to servers** buttons are unchanged.
4. **Post-attach focus.** When the client transitions into `attached`, the workspace pulls focus into the renderer via `safeFocus(...)` so an operator can start typing immediately. The call happens once per attach inside the existing `state_change` handler; the `myGen !== generation` guard already protects against dispose races, and `safeFocus` swallows any synchronous throw from a torn-down renderer. An additional focus is also issued immediately after `mount` so the viewport is keyboard-accessible during the brief `connecting` phase before the socket flips to `attached` — pre-existing behaviour preserved.
5. **UX copy module.** `terminalLaunch.ts` exports `TERMINAL_UX_COPY` — a frozen `{ settingsApplyNote, copyPasteNote }` map. The production terminal workspace, the empty-state Terminal view, and the Settings view all consume the same strings so the wording stays aligned.
   - `settingsApplyNote`: "Appearance settings apply to new terminal sessions. Save preferences in the Settings view, then launch (or reconnect) the session to see them."
   - `copyPasteNote`: "Use your browser's selection + clipboard shortcuts (Ctrl/Cmd+C / Ctrl/Cmd+V, or right-click Paste). Bracketed-paste confirmation, OSC 52, and a clipboard policy editor are future work."
6. **Pure helpers.** `safeFocus(renderer)`, `safeFit(renderer)`, `safeClearViewport(renderer)` live in `terminalLaunch.ts`. Each tolerates a `null`/`undefined` renderer and absorbs a synchronous throw (dispose race) without logging — the redaction posture forbids surfacing renderer internals through error strings. The helpers take a structural-typed renderer surface (`FocusableRenderer`, `FittableRenderer`, `ClearableRenderer`) so vitest can exercise the contract against a stub. The structural type for `safeClearViewport` deliberately excludes any session-client / transport surface — adding a wire-side call would require widening the type, which would trip review.
7. **`computeWorkspaceEnablement` extended.** The enablement object grows three new booleans: `focus`, `fit`, `clear`. Each is `true` only while live (`attached` or `replaying`). `idle`, `creating`, `connecting`, `detached`, `closed`, `error` all keep them disabled — the affordance hides rather than no-ops on a torn-down renderer.

**No backend changes.** The slice is purely a frontend polish slice. No new routes, no schema, no new wire messages, no protocol changes.

**Architecture rule preserved.** The production shell still imports only `@relayterm/terminal-core` and `@relayterm/terminal-xterm`; the experimental adapters remain dev-lab-only. `appShellIsolation.test.ts` is unchanged. The `clear()` method is xterm-specific and lives on `XtermRenderer` only; it does NOT pollute `TerminalRenderer` or `terminal-core`.

**Local-clear viewport semantics (load-bearing).**

- The Clear button calls `XtermRenderer.clear()`, which delegates to xterm's `Terminal.clear()`. Per xterm's contract this clears the visible viewport AND the scrollback ring within xterm.
- It does NOT send any wire frame. It does NOT call `client.sendInput` with a `clear` command, an `ESC[2J`, or any other terminal control sequence. The remote shell is unaware of the operation.
- It does NOT mutate the backend's replay buffer, the backend's sequence counter, or any audit-log event. Reconnect with `last_seen_seq` after a Clear will resume the live stream from the same bookmark — Clear is a renderer-only concern.
- The button is enabled only while live. Pressing it on a detached / closed / error workspace is impossible (the affordance is disabled), but the helper still tolerates a missing renderer for forward-safety.

**Fit/reflow semantics (load-bearing).**

- The Fit button calls `XtermRenderer.fit()`. That call delegates to the xterm fit addon, which sets the cell-grid dims AND synchronously fans out an `onResize` event.
- The workspace's existing `onResize` subscriber is the single place that calls `client.sendResize(...)`. The Fit button does NOT call `client.sendResize` itself — re-pinning the AGENTS.md "Encountered Lessons" double-emit rule.
- A renderer that has not been mounted (no live `Terminal`) returns `null` from `fit()`; the helper treats that as a clean no-op and the wire stays silent.

**Settings application copy (load-bearing).**

- Production terminal settings still apply on **next attach / next session** only. Live re-fit, atlas reset, and palette swap on a mounted xterm are explicit future work — the slice deliberately does not introduce hot-reload of an active session.
- The same `settingsApplyNote` string is rendered in three places: the production terminal workspace (next to the viewport), the empty Terminal view (when no launch is active), and the Settings view (next to Save / Reset). Centralising the copy means a SPEC drift trips a unit test rather than a UI smoke.

**Copy/paste policy notes (load-bearing).**

- The same `copyPasteNote` string is rendered in the production terminal workspace and the Settings view. The note explicitly names browser shortcuts as the current path and flags bracketed-paste / OSC 52 / clipboard-policy as future work. The slice does NOT implement any clipboard automation.
- Browser clipboard semantics depend on the browser, OS, and (in some embeddings) page focus. The note is intentionally short — operator behaviour, not a specification.

**Redaction posture (load-bearing).**

- The new buttons NEVER read terminal output, NEVER read input bytes, and NEVER include any payload in their `data-testid`, `title`, or click-handler argument. They are static-copy controls.
- `safeFocus` / `safeFit` / `safeClearViewport` swallow synchronous throws WITHOUT logging — a renderer-dispose race could otherwise produce an `Error` whose `message` describes internal state. Tests pin that the swallowed-throw branch returns the documented sentinel value (`false` / `null`).
- `TERMINAL_UX_COPY` is frozen by structure (a `const` object literal) and a sentinel test asserts none of its values contain `private_key`, `encrypted_private_key`, `BEGIN OPENSSH`, `session_output`, or the launch-summary redaction sentinel.

**Stable selectors (additions only).** `production-terminal-focus`, `production-terminal-fit`, `production-terminal-clear`, `production-terminal-settings-note`, `production-terminal-copy-paste-note`, `terminal-empty-settings-note`, `terminal-empty-copy-paste-note`, `settings-apply-note`, `settings-copy-paste-note`. The pre-existing `production-terminal-*` selectors are unchanged.

**Future work (explicit out-of-scope for this slice).**

Production renderer selector; ghostty-web / restty / wterm in production; multi-tab workspace; durable session recording UI; backend VT observer / `libghostty-vt` snapshot; profile-specific terminal preferences; live hot-reload of font / theme on a mounted xterm; mobile / Tauri keyboard UI; custom keybinding editor; OSC 52 clipboard automation; bracketed-paste confirmation / multiline-paste preview; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI. Each is a separate slice.

### Production active terminal local recovery

After the production terminal launch UI shipped, an operator who navigated away from the Terminal view (or did a full-page reload) had no way to find their way back to a still-alive backend session: the AppShell's `activeLaunch` was an in-memory pointer, not a stored one. Detached sessions survive the bounded ~30-second TTL window on the backend, but the operator had to copy the session id by hand and reconnect from the Sessions list. This slice adds a **local-only browser convenience pointer** at the most-recent terminal session so the empty-state Terminal view can offer an explicit "Reconnect last session" affordance.

This is **not** multi-tab workspace, **not** durable recording, **not** backend-restart recovery, and **not** automatic reconnect. The backend remains authoritative on every lifecycle decision; the local pointer is a UX shortcut.

**Scope (load-bearing — this slice).**

1. **Active session store** — `apps/web/src/lib/app/terminal/activeSessionStore.ts`. localStorage-backed helper module. Storage key: `relayterm.active-terminal.v1`. Persists ONLY safe public metadata (`session_id`, optional `profile_label`, optional `cols`/`rows`, optional non-negative `last_seen_seq`, optional cached `status_hint`, required `saved_at` ISO timestamp). Public surface: `parseActiveSession`, `serializeActiveSession`, `loadActiveSession`, `saveActiveSession`, `updateActiveSessionSeq`, `clearActiveSession`, `activeSessionFromLaunch`, `buildReconnectAttempt`. Tests live in `apps/web/tests/activeSessionStore.test.ts`.
2. **AppShell wiring** — `apps/web/src/lib/app/AppShell.svelte` writes the saved record on `handleLaunch` (covers fresh profile-row launches AND Sessions-list reconnects). On wire-confirmed close it clears the record (`handleSessionClosed`). On detach / `onDestroy` it refreshes the saved record's `last_seen_seq` (`handleLastSeenSeqUpdate`). The "Back to servers" exit deliberately does NOT clear — the operator may want to reconnect within the detached-TTL window.
3. **Empty-state Terminal view affordance** — `apps/web/src/lib/app/views/TerminalView.svelte` reads `loadActiveSession()` once per mount. When a record exists AND it does not match the currently-active launch, the empty state renders a "Reconnect last session" button alongside a "Forget saved session" affordance. Clicking Reconnect routes through `AppShell.handleLaunch` (the same path a profile-row launch takes); clicking Forget calls `clearActiveSession` and removes the affordance from the view.
4. **Production terminal seeded resume** — `apps/web/src/lib/app/terminal/ProductionTerminal.svelte` accepts `initialLastSeenSeq?: number`. When set and positive, the workspace seeds its local `lastSeenSeq` state and passes it to the very first `client.attach` so the backend's replay handshake covers the gap. Zero / missing values collapse to "no resume" — identical to a fresh attach. The workspace also fires two new optional callbacks: `onSessionClosed()` on the wire-confirmed `closed` lifecycle edge (debounced via a local `closeNotified` flag), and `onLastSeenSeqUpdate(seq)` on the `detached` lifecycle edge AND on `onDestroy` when `lastSeenSeq > 0`.
5. **`ActiveLaunch.lastSeenSeq` extension** — the cross-view launch struct gains an optional `lastSeenSeq?: number`. The local store is the single producer; profile-row launches and Sessions-list reconnects continue to leave it unset.

**No backend changes.** The slice is purely a frontend UX polish slice. No new routes, no schema, no new wire messages, no protocol changes. The local store is browser-local and per-device; clearing browser storage drops the record.

**Reconnect attempt contract (load-bearing).**

- `buildReconnectAttempt(record)` returns an `ActiveLaunch` whose `lastSeenSeq` is set ONLY when the saved value is a strictly positive integer. Zero, missing, and any malformed value collapse to "no resume bookmark" — the wire `attach` then skips the replay request and the operator gets a fresh attach.
- Cell-grid dims fall back to the standard 80×24 if the saved record omitted them.
- The reconnect attempt is gated by an explicit user action (the "Reconnect last session" button click). The view does NOT auto-reconnect on mount.

**Stale handling (load-bearing).**

- Wire-confirmed `closed` is the canonical "this session is gone" signal. When `ProductionTerminal` observes `clientState === "closed"` it calls `onSessionClosed()` once; `AppShell` clears the local pointer. This covers both the explicit "End session" path and a server-side close (e.g. PTY exited, TTL elapsed, operator closed the row from the Sessions list).
- Reconnect against a stale record routes through the same path: the wire returns `closed` quickly and the local pointer is dropped.
- A pure transport failure (WebSocket open rejected with no `closed` lifecycle signal) leaves the record alone — the failure could be transient. The operator can press "Forget saved session" manually.

**Redaction posture (load-bearing).**

- The persisted record stores ONLY safe public metadata. It MUST NOT carry secrets, terminal input, terminal output, replay frames, public/private keys, `encrypted_private_key`, host fingerprints, peer banners, or `session_event` payloads.
- `parseActiveSession` builds the record field-by-field. A hostile fixture that injects `private_key`, `encrypted_private_key`, `session_output`, `access_token`, or `replay_buffer` cannot smuggle those keys onto the parsed object — the parser explicitly drops anything outside the seven documented fields. Sentinel-string tests pin this in `tests/activeSessionStore.test.ts` against the parsed object, the serialized JSON, the persisted localStorage value, and the `activeSessionFromLaunch` projection.
- Parse failures (missing key, malformed JSON, wrong schema, hostile fixture, oversized fields) collapse to `null` silently. Save failures (storage unavailable, quota exceeded, unnormalisable draft) return `false`. NEITHER branch logs.
- The empty-state UI does NOT echo wire-side error detail. It surfaces the backend's lifecycle signal indirectly via `onSessionClosed` (which the shell uses to clear the record) — never via a wire `message` string.

**Stable selectors (additions only).** `terminal-empty-saved` (carries `data-saved-session-id`), `terminal-empty-reconnect-last`, `terminal-empty-forget-last`. The pre-existing `production-terminal-*` and `terminal-empty-*-note` selectors are unchanged.

**TTL / runtime limitation (load-bearing).** The local pointer is a *pointer*, not a runtime. Reconnect succeeds only while the backend's bounded detached-TTL window is still open AND the in-memory replay buffer survives in the live runtime. A backend restart drops every PTY and every replay buffer; the local pointer would still resolve, but the wire `attach` would receive a `closed` lifecycle and the pointer would be cleared. The empty-state copy says this in operator language ("Reconnect only succeeds while the backend runtime is still alive — replay is in-memory and does not survive a backend restart").

**Future work (explicit out-of-scope for this slice).**

Multi-tab workspace; durable session recording UI; backend VT observer / `libghostty-vt` snapshot for backend-restart recovery; cross-device sync of the saved pointer; per-profile recovery hints; auto-reconnect on page load; smarter stale-detection (poll the Sessions list before offering recovery); a local "saved sessions history" beyond the most-recent one. Each is a separate slice.

### Production session status refresh and stale-session handling

Polish slice that layers honest manual-refresh + stale-detection behavior onto the production Terminal Sessions list AND the empty-state Terminal view's "Reconnect last session" affordance. No backend changes, no new wire messages, no polling, no auto-refresh, no live list updates. The slice exists because two failure modes were silently misleading the operator:

1. The Sessions list could show an `active` / `detached` row that the backend had since closed (e.g. PTY exited; close from another tab; TTL elapsed). Clicking Open would hand off to the Terminal view, which would attach a WebSocket, which would fail seconds later with a `closed` lifecycle.
2. The empty-state Terminal view always offered the "Reconnect last session" affordance whenever a saved local pointer existed. The pointer is local; the backend may have dropped the runtime hours ago. Clicking Reconnect would attach to a stale id and fail.

This slice catches both BEFORE the WebSocket hand-off, using the existing `GET /api/v1/terminal-sessions/:id` route. It is explicit, single-shot, user-triggered (Sessions list) or once-per-mount (Terminal view); it never polls.

**Scope (load-bearing — this slice).**

1. **API helpers** — `apps/web/src/lib/api/terminalSessions.ts` gains:
   - `getTerminalSession(sessionId, options?)` — typed GET wrapper around `/api/v1/terminal-sessions/:id`. Reuses the existing `parseTerminalSession` (the GET route emits the same `TerminalSessionResponse` shape as the list route), so a future smuggled `private_key` / `encrypted_private_key` cannot reach the parsed object via this surface either. Path-unsafe ids are URL-encoded (test pin: `a/b c → /api/v1/terminal-sessions/a%2Fb%20c`).
   - `isSessionReconnectable(session)` — convenience predicate over a `Pick<TerminalSession, "status">`; mirrors `sessionStatus.canReconnect(status)` for callers that already hold a DTO.
   - `validateSavedSession(sessionId, options?)` — composes the GET + the predicate into a typed `SavedSessionValidation` decision: `reconnectable | stale (closed | not_found) | uncertain (starting | transport | http | malformed)`. Each variant carries a pre-formatted `summary` so the UI does not need a second formatter.
   - `describeSessionGetError(err)` — one-line UI summary for the GET error envelope. Same redaction posture as `describeSessionLoadError` / `describeCloseSessionError`: a function of `kind` + `status` + `code` ONLY.
2. **Sessions list pre-handoff verification** — `SessionsView.svelte` now validates the row against the backend BEFORE handing off to the Terminal view. The Open button transitions through a brief `Verifying…` state while `validateSavedSession` runs. Outcome:
   - `reconnectable` → refresh the row in place from the verify response (so the post-handoff scroll-back into the list shows fresh status), then call `onReconnect`.
   - `uncertain (transport | http | malformed)` → proceed with handoff. Don't punish the operator for a network blip; the WebSocket attach will surface its own failure if applicable.
   - `uncertain (starting)` → refuse handoff, surface the reason inline, and trigger a full list reload (`load()`) so every row re-syncs against the backend.
   - `stale (closed | not_found)` → refuse handoff, surface the safe summary (`"Saved session is no longer available."`) inline, and trigger a full list reload.
3. **Sessions list "honesty note"** — the Refresh affordance now sits next to a static `data-testid="sessions-refresh-note"` line stating that "Refresh re-fetches the current backend state. There is no auto-refresh or live update yet — closed sessions cannot be recovered from this view." The note is not a bug surface; it is a TTL/limitation reminder.
4. **Terminal view saved-session validation** — `TerminalView.svelte` runs `validateSavedSession(saved.session_id)` once per mount via `$effect` (cancellation flag pins the unmount race). The empty-state affordance now has four progressive states:
   - `idle` — no saved record; nothing rendered (existing behavior).
   - `checking` — request in flight; the existing affordance renders with a `Checking saved session against the backend…` line and the Reconnect button disabled.
   - `reconnectable` — the existing affordance renders with no caveat.
   - `uncertain` — the existing affordance renders with a cautious message: "{summary} You can still try the reconnect — the saved pointer was kept because the failure may be transient." The Reconnect button is enabled.
   - `stale` — the affordance is REPLACED by a small notice block (selector `terminal-empty-saved-stale`) that names the dropped session and tells the operator to launch a new session. The local pointer is dropped via `onForgetLastSession` (the shell clears localStorage); the in-memory `saved` is intentionally NOT nulled until next mount so the notice can show the dropped record's metadata.
5. **No backend changes.** The slice is purely a frontend reliability slice. No new routes, no schema, no new wire messages, no protocol changes. The pre-handoff verify uses the existing `GET /api/v1/terminal-sessions/:id` route.

**Honesty rules (load-bearing).**

- **No polling, no auto-refresh, no live list updates.** Refresh in the Sessions view is explicit; the Terminal view's mount-time validation is a single shot. Both views document the limitation in their copy.
- **Refresh does not recover closed sessions.** The honesty note next to the Refresh button says so; the underlying GET / list endpoints surface the backend's authoritative state, which never re-promotes a closed row.
- **Transport failure does not drop the saved pointer.** A backend blip is exactly the time when the operator most wants the pointer to survive. `validateSavedSession` returns `uncertain (transport)` and the UI keeps the Reconnect button + shows a cautious message. Test pin: `validateSavedSession transport` test asserts `kind: "uncertain"`, `reason: "transport"`, and that the formatter does NOT echo the thrown `Error.message`.
- **A 404 IS a stale signal.** The backend treats "doesn't exist" and "not yours" as the same 404 (the right redaction). The validator collapses both to `stale (not_found)` and the UI clears the pointer.
- **A 200 + closed status IS a stale signal.** The runtime is gone and cannot be reconnected. The validator collapses this to `stale (closed)` and the UI clears the pointer with the same operator-facing copy.
- **Saved-pointer copy never echoes wire detail.** `describeSessionGetError`, the `summary` strings on `SavedSessionValidation`, and the inline notice copy are all functions of `kind` + `status` + `code` ONLY. Sentinel-string tests pin this in `terminalSessionsApi.test.ts` against a URL-bearing transport message and operator-detail-bearing wire envelopes.

**Redaction posture (load-bearing).**

- `getTerminalSession` reuses `parseTerminalSession`. The DTO is built field-by-field; a stray `private_key` / `encrypted_private_key` / `peer_banner` smuggled onto the wire body cannot reach the parsed object. Sentinel test `parseTerminalSession does NOT carry private_key or encrypted_private_key onto the parsed DTO` already pins the parser; the new `validateSavedSession` tests additionally pin that `JSON.stringify(result)` does not contain the operator-detail sentinel after a stale-by-closed or stale-by-404 outcome.
- `SavedSessionValidation` summaries and `describeSessionGetError` are functions of `kind` + `status` + `code` ONLY. The wire `message` field of an HTTP error and the thrown `Error.message` of a transport failure are NEVER echoed in any user-facing string. Tests pin both.
- The new SessionsView "open error" inline message uses the same `summary` strings; no new redaction surface was introduced.
- The Terminal view validation effect does NOT log the validation result and does NOT log the dropped pointer. A console-noise regression would trip the existing redaction posture for the active-session store.

**Stable selectors (additions only).** `sessions-refresh-note`, `sessions-row-open-error`, `terminal-empty-saved-stale`, `terminal-empty-saved-checking`, `terminal-empty-saved-uncertain`. The pre-existing `sessions-*` and `terminal-empty-*` selectors are unchanged. The `terminal-empty-saved` block now also carries a `data-validation` attribute (one of `idle`/`checking`/`reconnectable`/`uncertain`) for smoke-test targeting.

**Architecture rule preserved.** The new helpers live in `lib/api/terminalSessions.ts`; the new view wiring lives in `lib/app/views/`. No imports from `lib/dev/` and no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans; no rule change.

**Future work (explicit out-of-scope for this slice).**

Background polling / auto-refresh; WebSocket-driven live list updates; multi-tab workspace; URL-driven routes / deep-linking; durable session recording UI / replay player; backend VT observer / `libghostty-vt` snapshot for backend-restart recovery; production renderer selector; mobile / Tauri shell integration; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI; auto-reconnect on page load. Each is a separate slice.

### URL-driven production view routing

Replaces the purely local view-state model with stable URLs for every production view. No routing library — the shell mirrors `selected` to `window.location.pathname` via `history.pushState` and listens for `popstate`. Foundation slice; route params, nested routes, deep-link launch, and auth routes remain future work.

**Scope (load-bearing — this slice).**

1. **Stable path per production view.** The production-shell route table:

   | Path | View |
   |---|---|
   | `/` | Dashboard (canonical landing alias) |
   | `/dashboard` | Dashboard |
   | `/terminal` | Terminal workspace |
   | `/sessions` | Terminal Sessions |
   | `/servers` | Server profiles |
   | `/identities` | SSH identities |
   | `/settings` | Settings |

2. **Pure helper module** — `apps/web/src/lib/app/routing.ts` exports `viewForPath`, `pathForView`, `normalizeAppPath`, `isKnownAppPath`, and the `AppRoutePath` union. All functions are pure and `window`-free; no helper throws on user-supplied input.
3. **Browser back/forward** — `AppShell.svelte` listens for `popstate` and updates `selected` from `window.location.pathname`. Nav clicks call `history.pushState` so back/forward step through in-app history without a full page reload. Cross-view transitions (`Launch terminal` from Servers, `Open` from Sessions, `Back to servers` from the Terminal view) route through the same `navigate(id)` helper so the URL stays in sync.
4. **Initial mount canonicalization.** On first paint, an unknown pathname (`/whatever`, `/servers/abc`, `/dashboard/extra`) collapses to the default view AND `replaceState`s the canonical path in place — the unknown URL never enters history. Known paths (including `/`) are left untouched.
5. **Dev tools have no route.** The dev-tools toggle and lab live under the same route as the surrounding view; they are not URL-addressable and remain gated by `import.meta.env.DEV` plus the `devTools` snippet from `App.svelte`.

**No secrets in URLs (load-bearing).**

- Terminal session ids, identity ids, profile ids, host ids, fingerprints, and any other backend-issued identifier MUST NOT appear in the URL in this slice. The active-launch hand-off continues to flow through shell-local state, NOT the URL.
- The router has no concept of route parameters. A path that *contains* a session-id-shaped segment (e.g. `/terminal/01HZK...`) collapses to the dashboard fallback — `viewForPath` rejects it and `normalizeAppPath` returns `null`. Sentinel test pin: `routing.test.ts` "redaction posture".
- `normalizeAppPath` strips a trailing `?query` or `#hash` before matching but never echoes their content; nothing the helper returns retains query parameters.

**Deployment requirement (load-bearing).**

The production host MUST serve `index.html` for every app route (`/`, `/dashboard`, `/terminal`, `/sessions`, `/servers`, `/identities`, `/settings`). Vite's dev server already does this; a static deployment without an SPA fallback will 404 on direct loads of any non-root route. Documented here so the deploy slice configures it explicitly.

**Architecture rule preserved.** The new helper lives in `lib/app/`; no imports from `lib/dev/`, no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans.

**Future work (explicit out-of-scope for this slice).**

Route parameters / detail pages (`/servers/:id`, `/identities/:id`); auth routes (`/login`, passkey enrollment, session list); deep-link terminal-session launch; route-based data preloading; nested routes; multi-tab workspace; URL-driven renderer selection; URL-driven settings deep-links; shareable URLs that include any backend identifier. Each is a separate slice.

### Production dashboard summary

The Dashboard view is now a real read-only summary instead of a single health badge. It composes existing API helpers — `checkHealth`, `listHosts`, `listServerProfiles`, `listSshIdentities`, and `listTerminalSessions` — into summary cards, a session-status breakdown, a connection-flow checklist, and a fixed set of internal navigation buttons. No new backend route, no new wire shape, no analytics, no polling.

**Scope (load-bearing — this slice).**

1. **Summary cards** render backend health, hosts count, server-profile count, SSH-identity count, and terminal-session count. Each card load is independent — one failure collapses to the card's `unavailable` state, but it does NOT poison the other cards. Counts that are still loading render as a `—` placeholder; failed loads ALSO render as `—` plus an honest "Unavailable" badge so a zero count is never confused with a failure.
2. **Sessions-by-status breakdown** sums the existing list-endpoint rows by `TerminalSessionStatus` (`active`, `detached`, `starting`, `closed`). The breakdown is reused list data — no new endpoint, no extra round-trip. A list failure collapses the breakdown to a single `Unavailable` line; an empty list renders all-zeros.
3. **Connection-flow checklist** renders seven steps in a stable order:
   1. Generate an SSH identity
   2. Install the public key on the target server
   3. Create a host
   4. Create a server profile
   5. Run host-key preflight and trust the result
   6. Run the auth-check
   7. Launch a terminal

   Steps that the inventory counts can prove (`generate-identity`, `create-host`, `create-profile`, `launch-terminal`) flip to `complete` when their underlying count is `> 0`; otherwise they stay `incomplete`. The remaining three steps — `install-public-key`, `host-key-trust`, `auth-check` — are explicitly `manual`. The dashboard does NOT have per-row state to prove a key was installed, a host key was trusted, or an auth-check passed; the checklist row tells the operator to verify from the per-resource view rather than pretending to know.
4. **Manual refresh only.** A single `Refresh` button drives both the health probe and the four inventory loads in parallel. There is no polling, no auto-refresh, no exponential backoff. The dashboard is a snapshot — operator triage stays on the per-resource views.
5. **Quick-action navigation.** A small fixed table of in-app navigation buttons (Manage servers, Manage SSH identities, Open terminal, View sessions, Configure terminal) routes through the existing AppShell `navigate(id)` helper — pure pushState, no full page reload, no route parameters. The view targets are pinned against `routing.ts` in `dashboardSummary.test.ts` so dashboard CTAs and the production route table cannot drift out of sync.
6. **No backend changes.** No new HTTP route, no new WebSocket frame, no new DTO. The slice is purely a frontend addition on top of the existing inventory + health surface.

**Architecture rule preserved.** The new helper module lives at `apps/web/src/lib/app/dashboard/dashboardSummary.ts`; the view stays at `apps/web/src/lib/app/views/DashboardView.svelte`. No imports from `lib/dev/` and no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans.

**Redaction posture (load-bearing).**

- The helper consumes already-typed DTOs (`Host`, `ServerProfile`, `SshIdentity`, `TerminalSession`) — never raw wire bodies. The DTO parsers in `lib/api/` already drop `private_key` / `encrypted_private_key` / unknown fields; the dashboard helper cannot reintroduce them because nothing copies fields off `unknown`.
- The dashboard does NOT echo the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure. Per-card failure surfaces as a static `Unavailable` label only.
- The helper does NOT log raw response bodies. The `Refresh` button is the single user-visible signal that a load happened.
- The checklist's manual-row copy is asserted in `dashboardSummary.test.ts` against banned phrases ("host-key trusted", "auth-check passed", "key installed", "ready to launch") so a future copy edit cannot smuggle an implication that the dashboard cannot prove.

**Stable selectors (additions only).** `dashboard-refresh`, `dashboard-summary-cards`, `dashboard-card-{health,hosts,profiles,identities,sessions}`, `dashboard-card-{...}-status`, `dashboard-card-{...}-cta`, `dashboard-count-{hosts,profiles,identities,sessions}`, `dashboard-health-probe`, `dashboard-session-breakdown`, `dashboard-session-status-{active,detached,starting,closed}`, `dashboard-session-loading`, `dashboard-session-unavailable`, `dashboard-sessions-cta`, `dashboard-setup-checklist`, `dashboard-checklist-{generate-identity,install-public-key,create-host,create-profile,host-key-trust,auth-check,launch-terminal}`, `dashboard-checklist-{...}-status`, `dashboard-checklist-{...}-cta`, `dashboard-nav-actions`, `dashboard-nav-{manage-servers,manage-identities,open-terminal,view-sessions,configure-terminal}`. The pre-existing `dashboard-inventory-counts` / `dashboard-counts-refresh` / `dashboard-counts-error` selectors are removed by this slice — the new card grid replaces the legacy three-column inventory tile.

**Checklist limitations (load-bearing — operators read this).**

- A `complete` mark on the count-inferable rows proves only that the corresponding row exists in your inventory. It does NOT prove that the host is reachable, that the SSH identity matches the target, or that the next terminal launch will succeed.
- The `manual` rows do NOT reflect any state the dashboard can observe today. The dashboard cannot tell whether a public key was installed, whether a host-key fingerprint is trusted, or whether the most recent auth-check passed. Future API surfaces may expose that state — at which point the relevant row would graduate from `manual` to count- or flag-inferable; this slice does not anticipate the schema.
- A `launch-terminal` mark of `complete` means a terminal session has been launched at least once. It is NOT a readiness signal for a new launch.

**Future work (explicit out-of-scope for this slice).**

Backend exposure of host-key trust state and last-auth-check outcome on the profile DTO; a checklist that promotes those rows from `manual` to inferable; charts / time-series widgets; admin / cross-user reporting; auto-refresh / polling; mobile-specific dashboard layout; setup-wizard UX (step-by-step flow); terminal launch directly from dashboard; host-key trust / auth-check directly from dashboard; URL-driven dashboard parameters. Each is a separate slice.

### Dashboard recent activity

The Dashboard view also surfaces a compact **Recent activity** section that reuses the existing read-only current-user audit feed (`GET /api/v1/audit-events/recent`). It is a snapshot designed to make the most recent server-profile lifecycle events (and any other current-user audit events) visible from the landing view without forcing the operator into Settings. No new backend route, no new DTO, no admin / cross-user view.

**Scope (load-bearing — this slice).**

1. **Source.** The section reuses `listRecentAuditEvents` from `apps/web/src/lib/api/auditEvents.ts` with `limit: 5`. The frontend never exposes the raw payload JSON; events are rendered through the same `summarizeAuditEvent` helper as the Settings panel. Unknown wire kinds collapse to a generic "Audit event" line.
2. **Bounded count.** The dashboard caps the rendered list at `DASHBOARD_RECENT_ACTIVITY_LIMIT = 5` (pinned in `dashboardSummary.test.ts`). The Settings `RecentActivityPanel` continues to request `limit: 20` — the dashboard intentionally renders fewer rows so it stays a snapshot, not a feed.
3. **Independent failure.** The audit fetch is its own load slot. A 401 / transport blip on the audit feed must NOT poison the inventory cards or the health probe. The section renders one of `loading` / `ready` (with rows or empty-state) / `error` and nothing else.
4. **Manual refresh only.** Two refresh paths exist: (a) the dashboard-wide `Refresh` button drives the health probe, the four inventory loads, AND the audit fetch in parallel; (b) the section's own `Refresh` affordance re-fetches activity alone, leaving the rest of the dashboard untouched. There is no polling, no auto-refresh, no retry storm.
5. **Navigation to Settings.** A `View all →` button uses the existing AppShell `onNavigate(AppViewId)` path to jump to the Settings view, which hosts the fuller `RecentActivityPanel`. The dashboard does NOT introduce route parameters and does NOT trigger a full-page reload. The target is pinned against `routing.ts` so the link cannot silently drift to a placeholder.
6. **No backend changes.** No new route, no new DTO, no new audit kind. The slice is purely a frontend composition on top of the existing audit read API.
7. **No admin / cross-user view.** This section, like the existing Settings panel, is the current-user audit feed only. Cross-user / admin reporting, search, filter, export, and audit-payload detail panes are deliberate later slices.

**Architecture rule preserved.** The helper module is `apps/web/src/lib/app/dashboard/dashboardSummary.ts` (extended with `summarizeRecentActivity`, `activitySectionFromLoad`, `DASHBOARD_RECENT_ACTIVITY_LIMIT`, and `RecentActivitySection` / `RecentActivityLine` types). The view stays at `apps/web/src/lib/app/views/DashboardView.svelte`. No imports from `lib/dev/` and no imports from any `@relayterm/terminal-*` adapter package. `appShellIsolation.test.ts` continues to enforce both bans.

**Redaction posture (load-bearing).**

- The dashboard renders only fields that have already passed through `parseAuditEvent` (which builds the structured `AuditPayloadSummary` field-by-field). Smuggled `private_key` / `encrypted_private_key` / `client_info` / `remote_addr` / `user_agent` / `session_output` / `access_token` keys cannot survive — pinned by sentinel-string tests in `dashboardSummary.test.ts` against the rendered `RecentActivityLine` JSON.
- The dashboard does NOT show actor identifiers (`actor_id`), remote addresses, user-agent strings, or any raw payload JSON. The visible row carries: a safe summary string (kind label + lifecycle profile name when present) and a formatted timestamp.
- Error states use `describeLoadError("audit events", err)` only — the helper never echoes the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure. Pinned with a sentinel string in `activitySectionFromLoad` tests.
- The helper does NOT log raw response bodies. Operator detail belongs in server logs, not the browser console.

**Stable selectors (additions).** `dashboard-recent-activity`, `dashboard-recent-activity-refresh`, `dashboard-recent-activity-view-all`, `dashboard-recent-activity-loading`, `dashboard-recent-activity-error`, `dashboard-recent-activity-empty`, `dashboard-recent-activity-list`, `dashboard-recent-activity-row` (carries `data-kind` set to the wire `AuditEventKind` tag).

**Future work (explicit out-of-scope for this slice).**

Cross-user / admin audit views, audit search / filter / export, audit-payload detail modals, raw-payload expansion, polling / auto-refresh, charts / time-series widgets, audit-by-resource drill-downs, mobile-specific dashboard layout, route-parameter-driven activity filtering, and audit pagination. Each is a separate slice.

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

## Inventory lifecycle and destructive-action policy

This section is normative. It defines the safe lifecycle for every inventory entity and the rules a future destructive surface (delete, disable, archive, revoke) MUST follow. Drift from these rules is a spec bug, not an implementation freedom.

**Status today (load-bearing — read before adding any destructive surface).** No production route or UI **deletes** or archives any inventory record. The lifecycle moves wired today are:

- `POST /api/v1/terminal-sessions/:id/close` — terminal sessions reach the `closed` terminal state.
- `POST /api/v1/server-profiles/:id/disable` and `POST /api/v1/server-profiles/:id/enable` — Stamp / clear `server_profiles.disabled_at`. Disabled profiles refuse new launches, auth-check, host-key preflight / trust. Existing live sessions are unaffected. Each successful create / enabled→disabled / disabled→enabled transition appends one `audit_events` row (`server_profile_created` / `server_profile_disabled` / `server_profile_enabled`). Frontend UI for disable / enable has landed; see "Server profile disable / enable UI (landed)". An audit viewer remains future work. See "Server profile disable / enable backend" and "Server profile lifecycle audit" for the full backend contract.
- `known_host_entries.revoked_at` — column exists; no route or UI yet writes it. The trust route already refuses to silently re-trust a revoked fingerprint (two-layer guard: route check + `record_trusted` SQL `WHERE revoked_at IS NULL`).

Everything else (`hosts`, `ssh_identities`, `server_profiles` deletion, `terminal_sessions` outside `close`, audit/session events) has no destructive surface. The schema enforces FK `RESTRICT` on the load-bearing references; an attempt to delete a referenced row from the DB layer would already fail. The policy below is what MUST be true *before* any destructive route or UI lands.

### Per-entity lifecycle states

| Entity | States today | Future states | Destructive surface today | FK to children |
|---|---|---|---|---|
| `users` | `active` (no other state) | none planned in v1 | none | `hosts` (CASCADE), `ssh_identities` (CASCADE), `server_profiles` (CASCADE), `terminal_sessions` (CASCADE), `audit_events.actor_id` (SET NULL) |
| `hosts` | `active` (no flag column) | `active` only — delete-when-zero-references | none | `server_profiles.host_id` (RESTRICT), `known_host_entries.host_id` (CASCADE) |
| `ssh_identities` | `active` (no flag column) | `active` only — delete-when-zero-references | none | `server_profiles.ssh_identity_id` (RESTRICT) |
| `server_profiles` | `active` \| `disabled` (`disabled_at` column) | unchanged; delete only after disable AND zero session references | `POST /:id/disable`, `POST /:id/enable` (backend-only today; no UI) | `terminal_sessions.server_profile_id` (RESTRICT) |
| `known_host_entries` | `unknown` (no `trusted_at`), `trusted` (`trusted_at` set, `revoked_at IS NULL`), `revoked` (`revoked_at` set) | unchanged; explicit operator-only unrevoke much later | column-level `revoked_at` only — no route yet | none |
| `terminal_sessions` | `starting`, `active`, `detached`, `closed` (CHECK constraint) | unchanged | `POST /:id/close` — idempotent, terminal | `terminal_session_attachments.session_id` (CASCADE), `session_events.session_id` (CASCADE) |
| `terminal_session_attachments` | open (`detached_at IS NULL`), detached (`detached_at` set) | unchanged | row update on detach (manager-internal); never deleted via UI | none |
| `session_events`, `audit_events` | append-only | unchanged | none — immutable | none |

`users` deletion is intentionally out of scope for v1. The `ON DELETE CASCADE` shape exists for operator/test use only; no API surface accepts a user delete.

### Delete vs disable / archive policy

1. **Default user-facing destructive action for `server_profiles` is `disable`, not delete.** Disable blocks NEW launches; existing live sessions keep running until they close on their own (operator close, remote shell exit, or PTY teardown). A re-enable returns the profile to launchable.
2. **`hosts` and `ssh_identities` are deletable only when zero `server_profiles` reference them.** This matches the schema's FK `RESTRICT`. The route MUST classify the refusal at the application layer — a clean 409 BEFORE attempting the DELETE — so the client gets a typed error (`409 conflict { entity: "server_profile", count: N }`) instead of a generic constraint violation. Production UI MUST surface "remove the N referencing profiles first" rather than "try again."
3. **`server_profiles` are deletable only when zero `terminal_sessions` reference them, AND the profile was already `disabled`** (preferred). Closed sessions count toward the reference total — closed-session metadata is historical and protective. If hard-delete on a referenced profile is ever needed, it is admin-only, not a user-facing action.
4. **`terminal_sessions` are NEVER deleted from the user UI.** Once `closed`, they are historical metadata. The user lists, views, and audits them. Any cascade or sweep that drops session rows is admin-only, future-only, and explicit. Inventory deletion (host/identity/profile) MUST NOT cascade-delete sessions — `RESTRICT` is the policy and the schema agrees.
5. **`known_host_entries` are revoked, never hard-deleted from user UI.** Hard delete is admin-only future work. Revoke is non-recoverable from the user surface; an explicit operator unrevoke flow may land later as a separate, deliberate slice (see "Encountered Lessons" 2026-04-29 in AGENTS.md).
6. **`session_events` and `audit_events` are never deleted from any surface.** They are append-only forensic logs; an admin retention sweep is future work and out of scope for v1.

### Reference / integrity policy

- **Host delete**: requires `0` `server_profiles` referencing the host. Cascade-deletes `known_host_entries` for the host (DB-level `ON DELETE CASCADE`). This is intentional — pins live with the host and a deleted host's pins have no meaning.
- **SSH identity delete**: requires `0` `server_profiles` referencing the identity. The encrypted private-key bytes are wiped at the DB layer when the row is removed; no copy of `encrypted_private_key` exists outside the row (vault decrypts only into ephemeral memory in the SSH session / preflight task and zeroizes on drop).
- **Server profile disable**: no reference check needed. Existing live `terminal_sessions` are unaffected. The launch route refuses to start a new session against a disabled profile with `409 conflict { entity: "server_profile", reason: "disabled" }`.
- **Server profile delete**: requires `disabled` AND `0` `terminal_sessions` referencing the profile (any status). Two-layer policy: the route emits a clean 409 BEFORE attempting DELETE; the schema's `RESTRICT` is the second-line backstop.
- **Typed-409 entity field convention**: the wire `entity` value on a `409 conflict` envelope uses the singular table-row form (`server_profile`, `terminal_session`, `host_key`). This matches the existing `409 conflict { entity: "host_key" }` and `409 conflict { entity: "terminal_session" }` shapes in the host-key-trust and terminal-session-create contracts. New destructive routes MUST follow this form so client error handling stays uniform.
- **Active session at the moment of profile disable**: the live session continues. Disable is a launch-time gate, not a runtime kill switch. Operator-driven session kill remains `POST /api/v1/terminal-sessions/:id/close`.
- **`audit_events.actor_id` orphans to `NULL`** when a user is deleted (schema `ON DELETE SET NULL`). Audit history survives user deletion, with the actor anonymised. This is the only inventory action that nullifies a reference; everything else uses `RESTRICT` or `CASCADE` deliberately.

### Session-history policy

- A `closed` `terminal_session` row is a permanent historical record. Users can list and view it but cannot delete it.
- The row's `server_profile_id` and `owner_id` references must remain stable for the row's lifetime. This is why the schema uses `RESTRICT` on `server_profile_id` and `CASCADE` on `owner_id` — the row dies only with its owner.
- When a profile is disabled or deleted, historical session rows that reference it stay readable. The list UI MUST handle a session whose underlying profile is gone (post-delete) without crashing — render a stable session id, status, timestamps, and a "(profile removed)" placeholder for the profile name.
- `terminal_session_attachments` and `session_events` cascade-delete with their session row. This is correct: they are per-session telemetry and have no meaning detached from the session. They are NOT exposed as their own deletable surface.

### Known-host revocation policy

- The state machine is `unknown → trusted → revoked` (with `unknown` returning to itself if the operator never confirms). `revoked` is reachable only via a deliberate operator action; the production UI does NOT surface revoke today.
- A revoked entry is **never silently re-trusted**. The trust route enforces this with two layers (route guard + `record_trusted` SQL), and the classifier filters revoked rows out of the `trusted` / `changed` classification (a revoked-and-reappearing key surfaces as `unknown`, not `trusted`). See AGENTS.md "Encountered Lessons" 2026-04-29 for the original incident analysis.
- Recovery from `revoked` is an explicit operator workflow that does NOT exist in v1. A future "unrevoke" route MUST be admin-only, audit-logged, and require an explicit fingerprint match — no convenience UX that lets an operator click through revocation.
- `known_host_entries` cascade-delete with their host (schema `ON DELETE CASCADE`). This is correct: pins are scoped to the host and have no meaning after the host is gone.
- Hard delete of a known-host entry without deleting its host is admin-only future work.

### Audit-event expectations

The `audit_events.kind` enum already anticipates `server_profile_created`, `server_profile_updated`, `server_profile_disabled`, `server_profile_enabled`, `server_profile_deleted`, `ssh_identity_created`, `ssh_identity_deleted`, `host_key_accepted`, `host_key_mismatch`, and `host_key_revoked`. New destructive routes MUST extend the enum (with a paired migration to the `audit_events_kind_chk` CHECK and the `AuditEventKind` Rust enum) when they introduce a new lifecycle action.

`server_profile_created`, `server_profile_disabled`, and `server_profile_enabled` are the only kinds wired today. See "Server profile lifecycle audit" below for the payload contract, idempotency rules, and fail-closed failure policy.

The currently-missing kinds are:

- `host_created`, `host_updated` — neither variant exists yet. The host CRUD routes do not write audit events today; that is its own gap. When host create/update lands, add the matching kinds (and corresponding `host_deleted`).
- `host_deleted` — required when host delete lands.
- (`host_key_revoked` already exists; reuse it for the revoke route.)

The auth-related kinds `login_succeeded`, `login_failed`, and `logout_succeeded` ARE already present in `audit_events_kind_chk` (per the original `20260428000009_audit_events.sql` migration) but no route emits them today. The forthcoming auth slice MUST NOT add a duplicate migration for these names. The auth slice DOES add new kinds (`first_user_created`, `password_changed`, `session_revoked`) — see "Production authentication architecture" → "Audit events" for the full list and the paired migration requirement.

Rules every destructive lifecycle action MUST follow:

1. **Successful destructive action writes exactly one audit event** with `actor_id = caller`, an appropriate `kind`, and a payload containing the target id and target kind. The `target_id` field on the payload is required so cross-entity audit queries are tractable.
2. **Failed destructive attempts that are security-relevant SHOULD audit.** A revoke-then-trust attempt, a cross-user delete (which already collapses to a 404 to avoid existence leak), and a delete refused for FK reasons in a context that suggests probing (large burst, repeated unknowns) are candidates. Routine 409s (delete blocked by visible references in the caller's own inventory) MAY skip audit to keep the log signal-rich.
3. **Audit payloads contain public metadata only.** Allowed: target id, target kind, caller id, fingerprints (public), `key_type`, `name`, timestamps, reference counts (e.g. `referencing_profile_count`), reason codes. **Forbidden:** `encrypted_private_key`, plaintext private-key bytes, raw russh error text, peer banners, vault internals (master key, nonce, version byte), terminal I/O (input keystrokes, output bytes), full URLs with query strings that could carry secrets, the `client_info` blob from `terminal_session_attachments` (operator-supplied User-Agent — reference attachments by `id` only).
4. **For `ssh_identity_deleted`** the payload MAY retain `name`, `key_type`, `fingerprint_sha256`, and `created_at` so the audit row remains readable after the underlying identity row is gone. The `encrypted_private_key` bytes are NEVER copied into audit.
5. **For `host_deleted`** the payload MAY retain `display_name`, `hostname`, `port`, `default_username`, and a count of cascaded `known_host_entries` so audit history records the operation precisely.
6. **For `server_profile_disabled` / `server_profile_enabled`** the payload includes `target_id` and the new state. No reason field is required in v1; an optional operator-supplied note is future work.
7. **`session_events` are not a substitute for `audit_events`.** Session events are per-session lifecycle telemetry and stay scoped to that session row. Audit events span the system and survive cascade-delete of session telemetry.

### UI implications

- No edit / delete / archive UI exists today outside the terminal-session close path. The production app shell renders read-only inventory detail panels by design except for the server-profile disable / enable controls described in "Server profile disable / enable UI (landed)" below.
- Future destructive UI MUST be **explicit, confirmable, and auditable**. A confirmation dialog is required for every destructive action; the confirmation MUST name the target (display name + id suffix), the action verb, and the consequence ("this profile will stop accepting new launches; existing live sessions are unaffected").
- Confirmation dialogs and audit views render **public metadata only**. The redaction rule from `lib/api/` parsers extends here — no `private_key` / `encrypted_private_key` field appears in any DOM string, formatted preview, or copy string. The existing sentinel-string redaction tests in the SSH-identity views are the pattern to follow.
- Routing rule (already established): no secret material, terminal data, or session payloads in URLs. This applies to confirmation dialog hashes too — destructive confirmation goes through component state, not URL params.
- Disabled `server_profiles` render with a clear `disabled` badge in the inventory list and detail panel. The Launch button is rendered disabled with an honest tooltip ("this profile is disabled; enable it to launch a new terminal"). The dashboard checklist's `launch-terminal` row stays count-inferable — disabled profiles do NOT change the count semantics.
- Closed terminal sessions remain visible and read-only in the sessions list. The list MUST handle a session whose `server_profile_id` no longer resolves (post-delete) without crashing — render a stable session id, status, timestamps, and a "(profile removed)" placeholder.
- A session whose underlying profile is `disabled` (but still resolvable) renders the profile name with a `(disabled)` suffix in the sessions list and detail panel. The session itself is unaffected by disable — `active`/`detached` sessions keep streaming and the operator may still close them — so the UI signals the disabled-profile context without implying the session has stopped. Re-enabling the profile clears the suffix on the next refresh.

### Server profile disable / enable backend (landed)

**Status:** schema, repository, API, and launch / setup-action guards are wired. Audit-event emission is intentionally deferred — see "Audit gap (deferred)" below. Frontend disable / enable UI remains future work and is unchanged today; the production shell still renders inventory read-only.

**Schema.** `server_profiles.disabled_at TIMESTAMPTZ NULL`, no default (migration `20260501000011_server_profiles_disabled_at.sql`). Existing rows are enabled (NULL). Column is **not** indexed in this slice — list filtering by `disabled_at` is not yet a hot path.

**Domain + DTO.** `relayterm_core::server_profile::ServerProfile.disabled_at: Option<DateTime<Utc>>` plus `is_disabled() -> bool`. `ServerProfileResponse.disabled_at` is **always serialised** (`null` when absent) so clients can rely on the field's presence. Frontend `parseServerProfile` accepts a string or `null`, treats a missing field as `null` for forward compatibility, and rejects wrong-shape values to prevent silent drift.

**Endpoints.**

- `POST /api/v1/server-profiles/:id/disable` — stamps `disabled_at = NOW()`. Owner-scoped; foreign / missing ids return a byte-identical 404. Idempotent: a redundant disable returns the existing row unchanged (the original `disabled_at` is preserved — bumping it on a no-op call would be misleading).
- `POST /api/v1/server-profiles/:id/enable` — clears `disabled_at`. Same ownership / idempotency contract.
- Both routes return the updated `ServerProfileResponse` body. Neither route accepts a request body in this slice.

**Failure modes.** `401 unauthorized` when dev-auth is disabled (extractor 401 short-circuits). `404 not_found` for a missing or foreign-owned profile (cross-user 404 is byte-identical to a genuine 404). `500 internal_error` for repository / database failures (static body, never echoes SQL).

**Setup-action and launch-time guards.** A profile with `disabled_at IS NOT NULL` refuses these dependent actions with `409 conflict` and the wire message `"server_profile disabled"` (the `code` stays `conflict`):

- `POST /api/v1/terminal-sessions` (launch). The wire `entity` reads `server_profile`; `reason` reads `disabled`. Existing live sessions are unaffected — see "Active session at the moment of profile disable" in the policy section above.
- `POST /api/v1/server-profiles/:id/auth-check`.
- `POST /api/v1/server-profiles/:id/host-key-preflight`.
- `POST /api/v1/server-profiles/:id/trust-host-key`.

Preflight refuses (rather than allowing a read-only probe) so the disabled state is uniformly closed across every dependent action; re-enabling the profile is the explicit return path. The trust route additionally guards against a sneaky bypass where a disabled profile is "re-blessed" without an explicit enable.

**WebSocket attach.** `GET /api/v1/terminal-sessions/:id/ws` does **not** re-check the underlying profile's `disabled_at`. An already-created session row is reachable until it closes via the standard lifecycle paths (operator close, remote shell exit, PTY teardown, TTL expiry). Disable is a launch-time gate, not a runtime kill switch; reapplying it across the live wire would surprise an active operator and serve no security purpose (the SSH transport is already pinned to the credentials in flight).

**Audit emission (landed).** See "Server profile lifecycle audit" below for the full contract. Server profile **create** and the **disable** / **enable** *transitions* each append one row to `audit_events` with public metadata only. The `update` and `delete` routes do not exist yet and therefore do not audit; when they land, they MUST follow the same payload contract and idempotency rules.

**ApiError shape.** `ApiError::Conflict` now carries `entity: &'static str` AND `reason: Option<&'static str>`. The wire envelope still uses `code: "conflict"`; when `reason` is `Some(r)` the message becomes `"{entity} {r}"`. When `reason` is `None` the message keeps the historical `"{entity} conflict"` form so existing clients (and pinned tests for `host_key conflict`, `terminal_session conflict`, etc.) continue to parse byte-identically.

### Server profile lifecycle audit

**Status:** schema, domain, and API emission landed. The kinds emitted today are `server_profile_created`, `server_profile_disabled`, and `server_profile_enabled`. `server_profile_updated` and `server_profile_deleted` remain pending — the routes themselves do not exist yet.

**Schema.** Migration `20260501000012_audit_events_lifecycle_kinds.sql` extends the `audit_events_kind_chk` CHECK with `server_profile_disabled` and `server_profile_enabled` (strict superset; no rows invalidated). The matching variants land on `relayterm_core::audit_event::AuditEventKind` with snake_case wire tags pinned by unit tests in `audit_event.rs`.

**Emission points.**

- `POST /api/v1/server-profiles` — on a successful create, appends one `server_profile_created` row.
- `POST /api/v1/server-profiles/:id/disable` — appends one `server_profile_disabled` row **only on the enabled → disabled transition**. A redundant disable (already-disabled row) returns the existing row unchanged and writes NO audit event.
- `POST /api/v1/server-profiles/:id/enable` — appends one `server_profile_enabled` row **only on the disabled → enabled transition**. A redundant enable returns the existing row unchanged and writes NO audit event.
- 401 / 404 paths (cross-user / missing id) write NO audit event. Otherwise the audit log would expose existence by id.

**Payload contract (security-critical).** The JSON object on every emitted row is built field-by-field from a single helper (`write_lifecycle_audit` in `routes/v1/server_profiles.rs`) and contains only public metadata:

```jsonc
{
    "server_profile_id": "<uuid>",
    "name":              "<profile name>",
    "host_id":           "<uuid>",
    "ssh_identity_id":   "<uuid>",
    "disabled_at":       "<rfc3339 timestamp> | null"
}
```

The payload MUST NOT contain: `private_key`, `encrypted_private_key`, plaintext key bytes, public-key bytes, terminal I/O (input keystrokes, output bytes, replay frames), the `client_info` blob from `terminal_session_attachments`, peer banners, raw russh error text, vault internals, or DB error text. Sentinel-string redaction tests in `crates/relayterm-api/tests/api.rs` (the `AUDIT_FORBIDDEN_SUBSTRINGS` helper) pin this on every emission path.

**Failure policy: fail-closed.** If the audit insert fails after the lifecycle row write, the route returns `500 internal_error` to the caller. The wire body is the static `internal error` message; the underlying SQL / driver detail is logged operator-side only and never echoed to the client. The lifecycle row state (the `server_profiles` insert / the `disabled_at` stamp / clear) is already committed by the time the audit insert runs — this matches the partial-success shape documented for `create_session` in AGENTS.md (2026-04-29 lesson). The orphan `server_profiles` row is operator-visible and reconcilable; the audit gap cannot be reconstructed after the fact, so surfacing the failure is preferable to silently dropping it.

**`remote_addr`.** The `audit_events.remote_addr` column is intentionally `NULL` for these rows in this slice. Client IP / user-agent capture across the API surface is its own deferred refactor (see "Out of scope (v1)") — this slice does not introduce a one-off route-level capture path.

**Reasoning.** Lifecycle audit rows are forensic primitives. Their value depends on `(actor, kind, target_id, recorded_at)` being trustworthy and free of secret-shaped fields. The payload deliberately avoids `tags`, `username_override`, host bag-of-fields, and identity public-key bytes — all of which are reachable via standard inventory queries scoped to the `actor_id`. Audit history is not a denormalised inventory snapshot; it is a transition log.

### Current-user audit events read API (landed)

**Status:** read-only `GET /api/v1/audit-events/recent` route plus a small "Recent activity" panel on the production Settings view. This slice is deliberately **not** an admin / cross-user audit viewer; admin tooling, search, filtering, export, retention, and payload-detail expansion remain future work.

**Scope (load-bearing).**

- **Current-user only.** Rows are filtered at the SQL layer by `actor_id = caller` via `AuditEventRepository::recent_for_actor`. There is no `actor_id` query parameter, no admin route, no aggregation surface.
- **NULL-actor exclusion.** Pre-auth events with `actor_id IS NULL` (failed-login attempts, unauthenticated probes) are NOT visible on this route. An admin surface that wants those uses `AuditEventRepository::recent` directly when it lands; this route MUST NOT relax the SQL filter.
- **Limit clamping.** `?limit=N` is clamped to `1..=100`; default is `20`. Out-of-range values are clamped silently rather than 400'd — the limit is a UI hint, not load-bearing input. The clamp is in `routes/v1/audit_events::clamp_limit` with a unit-test table.
- **No raw payload.** Responses go through `AuditEventResponse::from_event` (`crates/relayterm-api/src/dto/audit_event.rs`), which maps each known `AuditEventKind` onto a closed allow-list of safe public fields. Unknown kinds collapse to a generic summary that carries no payload data at all.
- **`actor_id` and `remote_addr` are dropped from the wire.** The caller IS the actor; re-emitting `actor_id` would invite a future drift where a cross-user row leaks via copy-paste. `remote_addr` exposure is a separate slice (client IP / user-agent capture across the API surface).

**Wire shape.** `AuditEventResponse`:

```jsonc
{
    "id":          "<uuid>",
    "kind":        "<snake_case AuditEventKind tag>",
    "recorded_at": "<rfc3339 timestamp>",
    "summary": {
        "kind": "server_profile_lifecycle",
        "server_profile_id": "<uuid> | null",
        "name":              "<string> | null",
        "host_id":           "<uuid> | null",
        "ssh_identity_id":   "<uuid> | null",
        "disabled_at":       "<rfc3339> | null"
    }
}
```

For audit kinds without an explicit sanitizer arm, `summary` collapses to `{ "kind": "generic" }` with no other fields. Per-kind sanitizer arms are added explicitly: each new kind that grows a public surface must (1) extend `AuditPayloadSummary`, (2) wire it in `sanitize_payload`, and (3) add a redaction-sentinel test that constructs an event whose payload contains every name in `AUDIT_FORBIDDEN_SUBSTRINGS` and asserts the serialised DTO contains none of them.

**Redaction contract (security-critical).** The DTO MUST NOT carry `private_key`, `encrypted_private_key`, plaintext PEM bytes, public-key bytes, terminal I/O, replay frames, peer banners, raw russh / transport / SQL error text, vault internals, `client_info` blobs, `remote_addr`, `user_agent`, or any payload field not explicitly allow-listed. Sentinel-string tests at three layers pin this:

1. `crates/relayterm-api/src/dto/audit_event.rs` — sanitizer-level tests serialise the DTO and assert no forbidden substring appears.
2. `crates/relayterm-api/tests/api.rs::audit_events_recent_redacts_secret_shaped_payload_fields` — route-level test that constructs an audit row whose payload smuggles every forbidden name and asserts the response body strips them.
3. `apps/web/tests/auditApi.test.ts` — frontend `parseAuditEvent` drops top-level smuggled fields, falls back to a `generic` summary on unknown summary variants (forward-compatibility for a backend that ships a new sanitizer arm before the frontend updates), and rejects malformed top-level shape.

**Unauthorized.** A request without a valid `relayterm_session` cookie is rejected by the `AuthenticatedUser` extractor before the route runs. Pinned by `audit_events_recent_unauthorized_without_session_cookie`.

**Empty list semantics.** A user with no audit history sees `200 []` (not `404`). Empty is the steady state for a fresh account.

**Frontend surface.** `apps/web/src/lib/api/auditEvents.ts` exposes `listRecentAuditEvents({ limit? })`, `parseAuditEvent`, `describeAuditEventKind`, and `summarizeAuditEvent`. The "Recent activity" panel (`apps/web/src/lib/app/views/RecentActivityPanel.svelte`) renders inside `SettingsView` with explicit loading / empty / error / ready states and a manual `Refresh` button. There is no polling, no auto-retry, and no payload-expansion affordance. Errors collapse through `describeLoadError("audit events", err)` so transport / operator detail cannot leak into the rendered string.

**Stable selectors (additions only).** `settings-recent-activity` (root article), `settings-recent-activity-refresh` (manual refresh button), `settings-recent-activity-loading`, `settings-recent-activity-error`, `settings-recent-activity-empty`, `settings-recent-activity-list` (the `<ul>` once events have loaded), `settings-recent-activity-row` (each `<li>`). Each row also carries a `data-kind` attribute set to the wire `kind` tag for smoke targeting; the value is a public taxonomy label and contains no operator data.

**Out of scope for this slice.** Admin / cross-user audit view, audit search, audit filtering, audit export, retention / sweeper, raw JSON payload expansion, client IP / user-agent capture refactor, payload sanitizers for kinds beyond the server-profile lifecycle trio.

### Server profile disable / enable UI (landed)

**Status:** wired in `apps/web/src/lib/app/views/ServersView.svelte`. The disable / enable surface is the first user-driven destructive-side action in the production shell; the rest of the inventory (hosts, SSH identities, known-hosts) remains read-only with no destructive UI.

**Scope.** Disable an enabled `server_profile` AND re-enable a disabled one. NOT in scope for this slice: delete UI for any inventory entity, host disable/delete UI, SSH identity disable/delete UI, known-host revoke UI, an audit viewer, admin tooling, multi-tab workspace, or any backend behavior change.

**API surface.** `disableServerProfile(profileId)` and `enableServerProfile(profileId)` in `apps/web/src/lib/api/serverProfiles.ts` POST to the existing backend routes. Both reuse `parseServerProfile` so `disabled_at` parsing stays centralised. Errors are formatted via `describeLifecycleError(action, err)` — a function of `kind` + `status` + `code` only; never echoes wire `message` or transport `Error.message`. Redaction-sentinel tests in `tests/profileLifecycle.test.ts` pin that a 200 response carrying a `private_key` / `encrypted_private_key` field cannot reach the parsed `ServerProfile` object.

**List badge + detail panel.** Each row in the Servers profile list emits a `data-profile-disabled` attribute and a small `disabled` badge next to the name when `disabled_at` is non-null. The detail panel renders a `Lifecycle` row carrying an `enabled` / `disabled` badge plus the `disabled_at` timestamp on disabled profiles, AND an inline disabled-profile note that names the gate ("New terminal launches, host-key preflight / trust, and auth-check are blocked. Existing live sessions are unaffected."). Disabled profiles are NOT hidden by default; the existing client-side search and tag filters continue to include them.

**Disable controls.** An enabled profile renders a `Disable profile` button in its row. Clicking opens a confirmation panel that:

- States the gate explicitly (new launches blocked, host-key preflight / trust / auth-check blocked, existing live sessions unaffected).
- Requires the operator to type the profile name verbatim before the disable submit becomes enabled (`disableConfirmationMatches` from `lib/app/inventory/profileLifecycle.ts`). The comparison is strict — case- and whitespace-sensitive — so the confirmation is deliberate but lightweight.
- Carries a `Cancel` button so the operator can back out without firing a request.
- Submits via `disableServerProfile` and replaces the matching row in the in-memory list from the parsed response. No automatic refetch is required; the backend response is the canonical post-disable shape.

The confirmation copy is static and never interpolates profile-specific data, so a hostile profile name cannot reach the rendered paragraph; sentinel-string tests pin this.

**Enable controls.** A disabled profile renders an `Enable profile` button gated only by an explicit click and a static reminder ("Enabling permits setup and launch attempts again. It does NOT prove host-key trust or auth readiness — re-run preflight, trust the host key, and re-run auth-check before launching."). On submit, the row is replaced in-memory from the parsed response and the disabled badge clears.

**Setup-action gating in UI.** While a profile is disabled:

- The `Launch terminal` button is rendered disabled with an honest tooltip and the inline copy switches to "Re-enable this profile to start a new terminal session." This mirrors the backend's `409 conflict { entity: "server_profile", reason: "disabled" }` and prevents the operator from racing into a rejected POST.
- `HostKeyPanel` accepts a `disabled` prop, renders an inline `Profile is disabled. Host-key preflight and trust are blocked until the profile is re-enabled.` notice, AND keeps the preflight button disabled. The same pattern applies to `AuthCheckPanel`.
- These guards are local to the UI and not relied on for security — the backend remains authoritative. The UI mirror exists so a disabled profile never offers an action the backend will refuse.

**Existing live sessions.** Disabling a profile does NOT close, kill, or otherwise touch its existing `terminal_sessions`. The UI copy names this guarantee in the disable confirmation, the row notice, and the detail panel note. The Sessions view continues to render live sessions whose underlying profile has been disabled (see "Sessions view list & per-row state").

**Idempotency.** A redundant disable on an already-disabled row (or enable on an already-enabled row) returns the same row from the backend; the UI replaces it in place and clears the lifecycle state. Concurrent UI clicks are guarded by the per-row `submitting` lifecycle state.

**Errors.** `describeLifecycleError` collapses 404 to `"server profile not found"`, 401 to `"not authenticated"`, transport failures to `"transport error"`, and parse failures to `"malformed response"`. The error banner is dismissable via a per-row `Dismiss` button so the operator can retry from a clean state without reloading.

**Future work this slice does NOT do.** No delete UI, no host or SSH identity lifecycle UI, no known-host revoke UI, no audit viewer, no terminal-session kill on profile disable, no admin tooling. Those remain as separate slices per the policy section above.

### Future implementation order

This is the recommended staged plan. Each item is its own slice; do not bundle. Earlier items unblock later items.

1. **~~Add `disabled_at TIMESTAMPTZ NULL` to `server_profiles`~~ (LANDED).** Migration, domain model, DTO, and frontend parser all carry the field. See "Server profile disable / enable backend (landed)" above. The "third state" guidance still applies: graduate to a `status` text column only if a third state (e.g. `archived`) becomes necessary.
2. **~~Backend route `POST /api/v1/server-profiles/:id/disable` (and paired `:id/enable`)~~ (LANDED).** Idempotent, owner-scoped, dev-auth gated. ~~Audit-event emission is deferred~~ Audit-event emission landed alongside `server_profile_created`; see "Server profile lifecycle audit" above for the kinds, payload contract, idempotency rules, and fail-closed failure policy.
3. **~~Launch-time guard on `POST /api/v1/terminal-sessions`~~ (LANDED).** Plus parallel guards on `auth-check`, `host-key-preflight`, and `trust-host-key`. Existing live sessions keep running. WebSocket attach is intentionally not gated — disable is a launch-time gate, not a runtime kill switch.
4. **~~Frontend disable / enable UI~~ (LANDED).** Server-profile lifecycle controls live on the Servers view: per-row `Disable profile` / `Enable profile` actions, name-echo confirmation for disable, inline disabled badge on row + detail panel, gated launch / preflight / trust / auth-check affordances, safe error formatter, and redaction-sentinel tests on the API path AND the static confirmation copy. See "Server profile disable / enable UI (landed)" above. An audit viewer remains future work — the backend already emits the rows, but no read surface exists.
5. **Backend route `DELETE /api/v1/server-profiles/:id`** ONLY after disable is in place. Refuses with `409 conflict { entity: "terminal_session", count: N }` when any session references the profile (any status). Owner-scoped 404 collapses cross-user existence checks. Writes `server_profile_deleted` audit event (kind already exists).
6. **Backend route `DELETE /api/v1/hosts/:id`.** Refuses with `409 conflict { entity: "server_profile", count: N }` when any profile references the host. Cascade-deletes `known_host_entries` (already enforced by schema). Writes `host_deleted` audit event (NEW kind — paired migration required).
7. **Backend route `DELETE /api/v1/ssh-identities/:id`.** Refuses with `409 conflict { entity: "server_profile", count: N }` when any profile references the identity. Writes `ssh_identity_deleted` audit event (kind already exists). The encrypted private-key bytes go away with the row; no separate wipe step is needed.
8. **Backend known-host revoke route** (e.g. `POST /api/v1/hosts/:id/known-hosts/:entry_id/revoke`). Stamps `revoked_at`. Writes `host_key_revoked` audit event (kind already exists). Owner-scoped via the host's `owner_id`.
9. **Stale-row sweepers and admin tooling** — operator surface for `starting` rows that survived a backend restart, orphaned attachments, and very-old closed sessions. Explicit, audit-logged, never silent. Out of scope for v1.
10. **Operator unrevoke and admin hard-delete** of `known_host_entries` / closed `terminal_sessions` — admin-only, audit-logged, deliberately later.

Each step's "definition of done" inherits the standard checklist (tests, sqlx prepare on schema change, audit event reachable, owner-scoping, redaction posture). When the first destructive route lands, append an "Encountered Lessons" entry in AGENTS.md if any non-obvious gotcha emerged (FK ordering, audit-payload surface, dialog redaction).

## Production authentication architecture

This section is normative. It defines the production authentication model and the security invariants every future auth slice MUST satisfy. Drift from these rules is a spec bug, not an implementation freedom.

**Operator guide.** For the deployment-side view — required env vars / TOML keys, secret generation, the first-user bootstrap flow, reverse-proxy / HTTPS notes, startup failure modes, and recovery paths — see `docs/production-auth.md`. Worked configuration templates ship at `docs/config-examples/relayterm.production.example.toml` and `docs/config-examples/relayterm.dev.example.toml`. The guide is the operator entry point; this section remains the normative spec.

**Status today (load-bearing — read before adding any auth code).** Real cookie-backed authentication is wired up across every protected `/api/v1/*` route AND `auth.mode = production` boots cleanly when the configuration envelope is satisfied (signing key, non-empty `allowed_origins`, `cookie_secure = true`). The legacy `DevUser` extractor, the `AppState::dev_user_id` field, the `DevAuthConfig` struct, and the `dev@relayterm.local` startup bootstrap are gone — both modes now run the same real-auth code path; only the boot-time validation envelope differs. The API surface is gated by the cookie-backed `AuthenticatedUser` extractor (`crates/relayterm-api/src/auth/user.rs`); a request without a valid `relayterm_session` cookie collapses to a static `401 unauthorized`. State-changing browser-write routes additionally take the shared `_csrf: CsrfGuard` extractor (`crates/relayterm-api/src/auth/csrf.rs`) so a missing or non-allowlisted `Origin` rejects with `403 csrf_origin_mismatch` BEFORE the body is parsed and BEFORE any DB / auth / lifecycle work runs. The `users` table carries `id`, `email`, `display_name`, `created_at`, `last_login_at`. The `audit_events_kind_chk` CHECK lists every auth event kind needed today (`first_user_created`, `login_succeeded`, `login_failed`, `logout_succeeded`, `password_changed`, `session_revoked`). The login route now runs an in-memory `LoginThrottler` keyed on the normalized email (5 failures / 15-minute window → 15-minute block; SPEC step 8 below); a multi-instance deploy still SHOULD layer reverse-proxy rate-limiting on top per `docs/production-auth.md` until a distributed limiter lands. The `AuthenticatedUser` extractor now stamps `user_sessions.last_seen_at` on every successful extraction (best-effort, error-tolerant — a repository failure logs `warn!` with the session id only and the request still succeeds; failed / expired / revoked / unknown extractions never reach the touch). What still does NOT ship in v1 (deliberate scope): passkeys / WebAuthn, password reset, session-management UI (the `last_seen_at` column is now populated for that future surface to consume), IP-aware / distributed throttling, and admin / RBAC tooling — see "Out of scope (v1)" and the implementation order below.

### Auth mode model

The backend runs in exactly one of two modes at any time. The mode is decided at boot from typed config and is fail-fast if misconfigured. **Both modes route requests through the same real-auth code path** — `AuthenticatedUser` is the sole identity source regardless of mode. The mode selects the boot-time validation envelope only.

| Mode | `auth.mode` | Behavior |
|---|---|---|
| `dev` | `dev` | Boot-time validation is permissive: insecure cookies (`cookie_secure = false`), missing signing key, and an empty `allowed_origins` list are all accepted (the latter still rejects every browser-write at the CSRF guard — that is the secure default; populate `allowed_origins` to actually serve a write surface). Same `AuthenticatedUser` extractor, same login / bootstrap / logout / me routes, same cookie shape. The mode flag does not change handler behaviour. |
| `production` | `production` | Boot-time validation enforces: exactly one of `auth.session_signing_key_b64` / `auth.session_signing_key_file` is set; `auth.allowed_origins` is non-empty; `auth.cookie_secure = true`. After the DB connect, an additional runtime gate refuses to start when no first user exists AND `auth.first_user_bootstrap_token` is unset (the operator has no path to create one). |

Rules every auth-mode change MUST follow:

1. **No silent fallback.** A request that fails real-auth MUST return 401 — never a fabricated identity. There is no longer any concept of a "dev user" stamped onto unauthenticated requests; the `dev_user_id` field is gone from `AppState` and any future replacement that smuggles a fixed identity would be a regression.
2. **Startup is fail-fast.** Production-mode misconfiguration (missing key, ambiguous key sources, empty allow-list, insecure cookie, or missing-token-and-no-user) MUST refuse to boot. Error messages name the failing input but never echo a value (same redaction posture as the vault master key — see `apps/backend/src/config.rs`).
3. **Config keys follow the existing convention.** Keys are nested under `auth` and read from `RELAYTERM_AUTH__*` env vars (double-underscore = nesting). Reserved names: `auth.mode`, `auth.session_signing_key_b64`, `auth.session_signing_key_file`, `auth.first_user_bootstrap_token`, `auth.cookie_secure`, `auth.cookie_domain`, `auth.allowed_origins`. The legacy `RELAYTERM_DEV_AUTH__ENABLED` env var and `[dev_auth]` TOML section are silently ignored (an operator with stale config does not see a hard load failure).
4. **`Debug` for any auth config struct redacts secret-shaped fields.** `session_signing_key_*` and `first_user_bootstrap_token` MUST be `Debug`-redacted to `_set: bool` markers, mirroring `VaultConfig` and `FileVaultConfig`.
5. **The session-signing key requirement in production is forward-looking.** The v1 session model is hashed-opaque-token (32 random bytes, SHA-256-stored), so the signing key is not consumed by any code path today. Requiring it at boot follows SPEC "Security properties to test" property 1 and reserves the operational discipline (rotation, redaction, file-vs-env sourcing) for the signed-CSRF / signed-cookie variants that come later.

A `disabled` / `maintenance` third mode is intentionally NOT introduced. If maintenance is needed, the existing operator path (stop the process, run migrations, restart) is sufficient and avoids a third code path.

### User model and first-user bootstrap

- **Existing `users` table is sufficient as the identity row.** Auth credentials are layered on top in additional tables (`user_passwords`, `user_passkeys`, `user_sessions`). The `users` row is referenced by `owner_id` from every inventory entity AND from `audit_events.actor_id` (SET NULL on delete). New auth tables MUST reference `users(id)` with `ON DELETE CASCADE` so a user delete (admin-only, future) cleans up their credentials atomically. The session table CASCADEs as well — orphan sessions after user delete would be a logout-bypass.
- **First-user bootstrap is required and one-shot.** A self-hosted deployment MUST be able to create its first user without an existing user to authenticate as. The chosen mechanism is a one-shot bootstrap token: the operator supplies `auth.first_user_bootstrap_token` (env or config); the backend prints / logs nothing about its value but exposes `POST /api/v1/auth/bootstrap` accepting `{ token, email, display_name, password }`. The route succeeds exactly once — once any row exists in `users` AND a corresponding `user_passwords` row exists, the bootstrap route returns `409 conflict { entity: "user", reason: "already_bootstrapped" }`.
- **The first-user record is a normal user row.** No `is_admin` column, no role table in v1. RelayTerm is single-user/self-hosted first; multi-user invites and admin/operator roles are deferred. The bootstrap user owns every subsequently-created inventory entity (one-user model) until multi-user lands.
- **Bootstrap path writes audit events.** The first-user bootstrap success appends one `audit_events` row with a NEW kind `first_user_created` (paired migration to extend the `audit_events_kind_chk` CHECK and the `AuditEventKind` enum). The bootstrap row's `actor_id` MUST be the newly-created user's id (NOT NULL), since the user IS the actor. A failed bootstrap (wrong token, malformed input) writes a `login_failed` row with `actor_id = NULL` and a payload that names ONLY the failure category (`bad_token`, `invalid_email`, `weak_password`) — no token bytes, no email if it was rejected as malformed, no password material at all.
- **Legacy `dev@relayterm.local` fixture.** The hard-coded dev-fixture user that the old `DevUser` shim bootstrapped at startup is gone — `apps/backend/src/main.rs` no longer creates it on boot, and the `bootstrap_dev_user_for_unimplemented_auth` helper has been deleted. Existing rows in deployed databases stay (no migration drops the row); they are now treated as a normal user, but a password row was never written so the row cannot log in. An operator who wants to keep that account uses the standard `set_password` path (no UI yet — direct DB or future admin tooling), and an operator who wants it gone deletes the row through the same path inventory cleanup follows.
- **Multi-user / team support is explicitly out of scope for the first auth milestone.** No invite flow, no admin role, no per-user permissions table. The single-user model lets the first auth slice land without RBAC scope creep.

### Password authentication (v1)

The first auth milestone uses password authentication. Passkeys/WebAuthn are deferred — see "Passkey/WebAuthn stance."

- **Hashing.** Argon2id with parameters chosen for ~250 ms verify on a typical server (`m=19456` (kibibytes, ~19 MiB — the Argon2 `m` parameter is *already* expressed in kibibytes; do NOT multiply by 1024), `t=2`, `p=1` is the OWASP 2023 minimum and a sane v1 default). Parameters live in a typed config struct so they can be tuned without a migration. Hashes are stored in a new `user_passwords` table — not as a column on `users` — so the password row CASCADEs cleanly with the user, and so future credential variants (one-time-recovery, passkey) can sit alongside without bloating `users`.
- **Schema sketch (NOT TO BE IMPLEMENTED IN THIS SLICE).**
  ```sql
  CREATE TABLE user_passwords (
      user_id        UUID        PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
      hash           TEXT        NOT NULL,            -- PHC string (`$argon2id$...`)
      algo_version   SMALLINT    NOT NULL,            -- bump on parameter change
      updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
  );
  ```
  PHC-string storage means future parameter upgrades verify the old hash, then re-hash and update on next successful login. No bespoke salt column; the salt is part of the PHC string.
- **Password policy.** v1 enforces only a minimum length (12 chars). Maximum length (e.g. 1024) is enforced at the boundary so a maliciously huge password cannot DoS the hasher. No mandatory complexity rules (a length floor + Argon2id is more effective). A future password-reset flow MAY introduce additional rules; the boundary parser is the single place to extend.
- **Throttling.** ✅ **Landed (foundation — email-keyed only; IP-aware / distributed deferred).** `POST /api/v1/auth/login` runs the in-memory `LoginThrottler` (`crates/relayterm-auth::throttle`) before the user lookup. The throttle key is the **normalized email** (lower-cased + trimmed); IP-aware keying is deferred until `ConnectInfo` is plumbed through the listener (see `terminal-sessions` reconnect notes for the same caveat). Policy v1 default: 5 failures within a 15-minute sliding window trigger a 15-minute block. Unknown-email AND wrong-password failures BOTH increment the same key — that is the probe-resistance contract: a probe cannot distinguish "user does not exist" from "user exists, wrong password" through the throttle channel any more than through the wire response or audit row. A throttled attempt does NOT spend an Argon2id verify or a DB query — the route returns `429 too_many_requests` immediately, with the operator-side detail logged at `warn!` and the wire body collapsed to the static `too many requests` string. The wire response carries no `Retry-After` header in v1 — exposing the remaining block duration would leak throttle-key telemetry to a probe; if a future SPA UI needs it, plumb a separate `last_attempt_remaining_seconds` field on `/api/v1/auth/me` rather than the 429 response. A successful login clears the bucket via `record_success` so a typo'd attempt under threshold does not linger. The map is bounded at 10,000 distinct keys via opportunistic cleanup; once at capacity new keys are silently dropped (fail-open under saturation rather than refusing service to the rest of the user base). State is **local-process only** — a multi-instance deploy SHOULD additionally rate-limit at the reverse-proxy layer (Traefik middleware, nginx `limit_req`, Cloudflare, etc.) per `docs/production-auth.md`. A Redis-backed distributed limiter is a follow-up. CSRF-rejected logins (bad `Origin`) NEVER touch the throttle map — the `CsrfGuard` extractor short-circuits before the route runs, so a third-party origin cannot lock out a legitimate user by triggering 403s against their email.
- **Recovery.** No password reset / "forgot password" flow in the first milestone. A self-hosted operator who locks themselves out has DB-level recourse (admin command-line tool, future). Building email-based reset is its own slice and would drag in mail transport scope. Documented as deferred in "Out of scope (v1)."
- **Login-event audit.**
  - Successful login → `login_succeeded` with payload `{ user_id, login_at, method: "password" }`. `actor_id = user_id`.
  - Failed login (wrong password, unknown email, throttled) → `login_failed` with `actor_id = NULL`, payload `{ method: "password", reason: "bad_credentials" | "throttled" }`. The reason set is exactly these two values for v1 — `"bad_credentials"` covers wrong-password AND unknown-email AND any other authentication-time refusal so a probe cannot distinguish "user does not exist" from "user exists but password is wrong" via the audit row OR the wire response. The payload MUST NOT carry the attempted email, the password, the password hash, or the request body. NULL-actor exclusion (see "Current-user audit events read API") keeps these rows out of any user-facing audit feed; an admin surface that wants them uses the unscoped `recent` query.
  - Password change → `password_changed` (NEW kind, paired migration). Payload: `{ user_id, changed_at }`. Never the new or old hash, never the new or old password.
- **Logging prohibitions.** Plaintext passwords MUST NEVER appear in any log line, error message, audit payload, panic message, span field, or HTTP response body. Argon2id hashes MUST NEVER appear in any audit payload, log line, or HTTP response body. The session-signing key MUST NEVER appear in any log line or `Debug` output. Every new module that handles password material grows a sentinel-string redaction test mirroring `AUDIT_FORBIDDEN_SUBSTRINGS` (see `crates/relayterm-api/tests/api.rs`).

### Passkey/WebAuthn stance

- **Deferred to a later milestone.** v1 ships password-only. The decision is pragmatic: passkeys-only would require a working multi-factor recovery story for a self-hosted deployment, and a passkey-or-password implementation doubles the surface of the first auth slice for marginal benefit on a single-user product.
- **Forward compatibility.** The `users` table and `user_sessions` table (defined below) are passkey-ready: the session is independent of the credential mechanism that minted it. A future `user_passkeys` table sits alongside `user_passwords`; a future login route variant accepts a WebAuthn assertion and (on success) issues the same opaque session cookie shape password login does.
- **What a future passkey slice would add.** (Sketch, NOT load-bearing.) `user_passkeys (user_id, credential_id, public_key, sign_count, transports, friendly_name, created_at, last_used_at)`; relying-party-id config under `auth.webauthn.rp_id`; a registration challenge endpoint; an authentication challenge endpoint. The session and audit shapes do not change. `login_succeeded.payload.method` becomes `"passkey"`; everything else flows through unchanged.
- **Anti-goal.** Do not introduce a passkey extractor or shim before the password milestone is green. Mixing the two in the first slice is how scope dies.

### Session model

Sessions are server-side opaque tokens persisted in Postgres and bound to an HTTP-only cookie. JWTs are NOT used for the browser surface. This matches the existing rule in `AGENTS.md`: "Sessions over JWTs."

- **`user_sessions` table sketch (NOT IN THIS SLICE).**
  ```sql
  CREATE TABLE user_sessions (
      id               UUID        PRIMARY KEY,         -- not the cookie value
      user_id          UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
      token_hash       BYTEA       NOT NULL UNIQUE,      -- SHA-256 of the cookie token
      created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
      expires_at       TIMESTAMPTZ NOT NULL,
      last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
      revoked_at       TIMESTAMPTZ,
      remote_addr      TEXT,                             -- last-seen, NOT join condition
      user_agent       TEXT                              -- last-seen, NOT join condition
  );
  CREATE INDEX user_sessions_user_id_idx ON user_sessions (user_id);
  CREATE INDEX user_sessions_expires_at_idx ON user_sessions (expires_at);
  ```
  - **Token generation.** 32 bytes from `rand::rngs::OsRng`, base64url-encoded for the cookie value. The DB stores ONLY `token_hash = SHA-256(token_bytes)` — never the plaintext token, never an HMAC keyed with a per-server key (a non-keyed hash is sufficient when tokens are 32 random bytes; a keyed MAC adds key-rotation cost without security benefit). A DB dump never contains a usable token.
  - **Lookup is constant-time-ish.** Indexed `UNIQUE` on `token_hash`. Token comparison happens via the index; an attacker cannot time-side-channel which `(prefix)` matches because the index lookup is on the full hash.
  - **Expiry.** Default `expires_at = created_at + 30 days`. A single-purpose `last_seen_at` column tracks idle activity; the expiry sliding-window policy (re-issue on activity? hard expire?) is `hard_expire` for v1 — simpler, no rolling-expiry edge cases — and revisited if UX demands it. The login route refuses to reuse expired rows.
  - **Logout / revoke.** `POST /api/v1/auth/logout` stamps `revoked_at = NOW()` on the matching row (looked up by `token_hash` from the cookie) AND returns a `Set-Cookie` that immediately expires the cookie. A revoked or expired row 401s every subsequent request. Revoked rows are kept for audit / "active sessions" listing; a sweeper deletes them after `expires_at + 30 days` (sweeper is future-only — see Implementation order).
  - **Active sessions list (later).** `GET /api/v1/auth/sessions` returns the caller's non-revoked, non-expired rows with `id`, `created_at`, `last_seen_at`, `remote_addr`, `user_agent` (sanitised). The user can revoke a specific session id from the list. The plaintext token is NEVER returned. Out of scope for the first milestone — listed here so the schema accommodates it.
  - **`remote_addr` / `user_agent`.** Stored last-seen for the session list. They are NEVER used as a join / equality predicate for auth (an IP change does not invalidate the session — that breaks mobile networks, see the project's reconnect rules). Do NOT add `WHERE user_agent = $1` or `WHERE remote_addr = $1` predicates anywhere — they are display metadata, not auth inputs. The auth extractor does NOT consult them; lookup goes through `token_hash` only.
- **Cookie configuration.**
  - **Name.** `relayterm_session`. Stable wire name; clients (the Tauri shells) MUST NOT depend on the name being changeable per environment.
  - **Flags.** `HttpOnly; Secure; SameSite=Strict; Path=/`. `Domain` is omitted by default (host-only cookie); a deploy that runs the API and the SPA on different subdomains MUST set `auth.cookie_domain` and accept the cross-subdomain SameSite implications.
  - **Lifetime.** Cookie `Max-Age` matches `user_sessions.expires_at`. The session is the source of truth; the cookie is a hint. A user clearing cookies invalidates their browser session immediately; the DB row stays until the sweeper runs.
  - **Dev-mode permissiveness.** In `auth.mode = dev`, `auth.cookie_secure = false` is accepted unconditionally — there is no second env-var gate, no startup `warn!`, and no operational ceremony around opting out of `Secure`. Local development over plain HTTP is the load-bearing use case; making the operator pass through two flags would add friction without security benefit (a dev box already has nobody else routing to it). `Config::validate_auth` enforces the asymmetry: production rejects `cookie_secure = false` at boot; dev does not.
  - **Rotation on login.** Every successful login mints a fresh `user_sessions` row and a fresh cookie value. The previous cookie/session of the same user is NOT revoked automatically — multiple devices coexist. A "log out everywhere" affordance is future work.
  - **No rotation on each request.** v1 does not rotate the session token on activity. Rotation introduces a race window where the old cookie is briefly valid; the marginal benefit is small for a 30-day expiry and the implementation complexity is real.
- **Session-signing key.** Reserved as `auth.session_signing_key_b64` / `auth.session_signing_key_file` for future use (e.g. signed CSRF tokens, signed cookies if the model changes). NOT used in the v1 hashed-opaque-token model — the random 32 bytes are their own entropy source. The key is loaded at boot using the same fail-fast / no-silent-fallback discipline as `vault.master_key_*` (see `Config::vault_master_key`); in v1 the key is simply unused.
- **Boundaries.** `user_sessions` rows live in the same Postgres database as inventory. No Redis dependency in v1. The sqlx repository for `user_sessions` follows the existing repository pattern (`relayterm-core::repository`).

### CSRF posture

Cookie-bearing browser requests are vulnerable to CSRF. v1 defends in three layers:

1. **`SameSite=Strict` on the session cookie.** First line of defense. Strict means the cookie is not sent on cross-site requests AT ALL — including top-level navigations from third-party sites. This blocks the classic CSRF vectors (image src, form post, top-frame nav). Strict is acceptable for RelayTerm because there is no "click a link from gmail and stay logged in" UX expectation; the user opens the SPA tab directly.
2. **`Origin` header validation on every state-changing request.** Every non-GET / non-HEAD / non-OPTIONS request MUST carry an `Origin` header that matches the configured `auth.allowed_origins` list (single entry by default — the SPA's own origin). A missing or mismatched `Origin` returns `403 forbidden { code: "csrf_origin_mismatch" }` BEFORE the auth extractor runs. This catches misconfigured browsers and (more importantly) SOPped XHR from third parties that forget to set `Origin`. The shared guard is the [`CsrfGuard`] axum extractor + [`check_origin`] helper in `crates/relayterm-api/src/auth/csrf.rs`; every browser-write route places `_csrf: CsrfGuard` ahead of `Json<...>` so the rejection happens before body parsing AND before any DB or auth work runs (the ordering documents intent — axum 0.8 runs every `FromRequestParts` extractor before the single `FromRequest` body extractor regardless of source order, so the guarantee holds even on a re-ordered signature).
3. **Double-submit token for the SPA (added once a non-same-origin client lands).** A short-lived CSRF token is issued in a non-`HttpOnly` cookie + a header on every state-changing request; the middleware checks they match. v1 ships ONLY layers 1 and 2 — same-origin SPA + Strict cookie is sufficient. Layer 3 lands when the Tauri shells move to a different origin from the API, or when an admin embeds RelayTerm somewhere.
- **Exempt routes.** GET/HEAD/OPTIONS are exempt from CSRF middleware (idempotent reads, browser preflight). The login, logout, and bootstrap routes are NOT exempt — they require `Origin` to match. The WebSocket upgrade route is NOT exempt; the upgrade handshake carries `Origin` and the middleware enforces it BEFORE the upgrade completes (a same-origin SameSite-Strict cookie is the only way the cookie reaches the upgrade in the first place, but defense in depth costs nothing here).
- **API-client / non-browser clients.** A future API token surface (e.g. for CI / scripts) issues a separate `Authorization: Bearer <token>` mechanism with its own table; bearer tokens are not subject to CSRF (no ambient credential). Out of scope for v1; named here so the v1 CSRF layer doesn't block its design.
- **No CSRF cookie in v1.** Avoiding the second cookie keeps the cookie surface small and the headers minimal. The Origin-header check is sufficient given Strict.

### Auth extractor and route migration

- **New extractor: `AuthenticatedUser`.** ✅ **Landed.** Lives in `crates/relayterm-api/src/auth/user.rs` (the HTTP-layer glue stays in `relayterm-api`; the crypto and persistence primitives live in `relayterm-auth`, and a thin shared cookie helper lives at `relayterm-api::auth::cookie`). The handler-facing surface is:
  ```rust,ignore
  pub struct AuthenticatedUser { /* private */ }

  impl AuthenticatedUser {
      pub fn user_id(&self) -> UserId;
      pub fn user(&self) -> &User;
      pub fn into_user(self) -> User;
  }

  impl<S> FromRequestParts<S> for AuthenticatedUser
  where
      S: Send + Sync,
      AppState: FromRef<S>,
  {
      type Rejection = ApiError;
      async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
          // 1. Parse the `Cookie:` header via the shared
          //    `auth::cookie::extract_session_cookie` helper. Missing /
          //    non-UTF-8 / empty-value / prefix-named / suffix-named
          //    cookies all return 401 with `missing session cookie`.
          // 2. `AuthService::validate_session_token(token, now)` — the
          //    service hashes the token with SHA-256 internally, looks
          //    up by `token_hash`, and surfaces `SessionInvalid` /
          //    `SessionExpired` / `SessionRevoked`. Each maps to 401.
          // 3. `db.users().get(session.user_id)` — a missing user row
          //    (CASCADE should make it unreachable) returns 401 with
          //    `session references missing user`.
          // 4. `db.user_sessions().touch_last_seen(session.id, now)` —
          //    best-effort, awaited inline. A repository failure logs
          //    at `warn!` with the session id only (never the cookie
          //    value, the token hash, or the repository internals)
          //    and the request still succeeds. The touch runs ONLY
          //    after the validate + user-load have both succeeded —
          //    failed / expired / revoked / missing-user paths above
          //    already returned, so a row is never touched outside
          //    the happy-path. No `tokio::spawn`-and-forget per the
          //    AGENTS.md concurrency rule — the await is inline so
          //    one failing call cannot accumulate orphaned futures.
          // 5. Return `AuthenticatedUser { user }`.
      }
  }
  ```
  Failures collapse to a single `ApiError::Unauthorized` on the wire — a missing cookie, a revoked row, an expired row, an unknown row, and a missing user are byte-identical (`code: "unauthorized"`). The operator-side detail names the category (logged at `warn!` in `error.rs::IntoResponse`) but never the token bytes or hash. The session token, the token hash, and the session row are NEVER reached by the handler — only the resolved `UserId` and `User` are.
- **Route migration shape.** ✅ **Complete.** Every protected `/api/v1/*` handler takes `AuthenticatedUser` and binds the caller's id via `user.user_id()`. The `owner_id = caller` repository filter, the `into_create(owner)` DTO methods, and the audit `actor_id` all take a bare `UserId`. The legacy `DevUser` extractor and the `AppState::dev_user_id` field are gone — no handler in the codebase has a path back to a fabricated identity.
- **Test fakes.** Integration tests bootstrap a real session via `bootstrap_test_session(&auth, user_id)` and attach the cookie on the test request (Origin too, for POSTs). The fixture helpers (`setup`, `setup_full`, `setup_with_first_user`, `setup_production_first_user`, etc.) all return the cookie token as the last tuple element. There is no DEV-only `bypass_auth = true` test mode — the production path is the test path.
- **Boundary discipline (re-affirmed).** Every protected handler MUST take the auth extractor as a parameter, not pull the user out of state inside the handler body. The extractor IS the gate; a handler that "guards itself" is a future bug.

### Frontend authentication UI plan

The frontend gains a login surface. The terminal renderer surface and the WebSocket reconnect contract do NOT change — auth lives in the AppShell, not in `terminal-core` or any renderer adapter.

- **Phase 1 — gate the AppShell on `getCurrentUser()`.** Add `apps/web/src/lib/api/auth.ts` exposing `getCurrentUser()` that calls `GET /api/v1/auth/me`. `AppShell.svelte` short-circuits to a `LoadingSplash` while the call is in flight, to a `LoginView` on 401, and to the existing nav+view tree on 200. The same code path runs in dev and production — only the boot-time configuration envelope differs.
- **Phase 2 — `LoginView` (password).** New `apps/web/src/lib/app/views/LoginView.svelte`. Single form: `email`, `password`, submit. Submits to `POST /api/v1/auth/login` with `credentials: "include"` and an explicit `Content-Type: application/json`. On 200, calls `getCurrentUser()` and (on success) re-renders the AppShell. On 401, shows a generic "sign-in failed" message — never echoes the wire `message` of the response. On 5xx / network failure, formats via the existing `describeLoadError("sign in", err)` helper. Adds stable selectors `auth-login-form`, `auth-login-email`, `auth-login-password`, `auth-login-submit`, `auth-login-error`.
- **Phase 3 — `BootstrapView` (first user).** Reachable only when `GET /api/v1/auth/me` returns `409 conflict { entity: "user", reason: "not_bootstrapped" }`. Form: `bootstrap_token`, `email`, `display_name`, `password`. Submits to `POST /api/v1/auth/bootstrap`. The bootstrap route's job is **user creation only — it does NOT mint a session or set a cookie**. On 200, the SPA immediately POSTs the same `email` + `password` to `/api/v1/auth/login` to obtain the session cookie, then re-renders the AppShell. Splitting bootstrap and login keeps session minting on a single route (the login route) — bootstrapping does not become a second unauthenticated session-issuing surface. Selectors: `auth-bootstrap-*`. The bootstrap form's `bootstrap_token` field is a normal `<input type="password">` — never logged, never echoed, never persisted to local storage.
- **Phase 4 — logout.** Add a "Sign out" affordance to the existing `Settings` view (or the AppShell header). Submits `POST /api/v1/auth/logout`; on 200 (or 401 — already invalidated is fine) the SPA re-renders to `LoginView`. **Active terminal state MUST be preserved across the wire only.** On logout, the SPA closes any open terminal WebSocket, clears any in-memory terminal-renderer state, AND clears the local-storage `active_terminal_session_id` recovery pointer (the existing `apps/web/src/lib/app/views/active terminal local recovery` slice). The backend `terminal_sessions` row stays in `detached` until the TTL expires or an explicit close — that is the existing contract; logout does NOT auto-close sessions, and re-login within the TTL CAN re-attach. Documenting this is the load-bearing part: an operator who logs out and back in within 30s of detach SHOULD recover their terminal.
- **Phase 5 — session-expired handling.** Any `/api/v1/*` response with status `401` that is NOT itself the auth surface MUST cause the SPA to drop to `LoginView`. The error envelope's `code: "unauthorized"` is the trigger; transient network errors do NOT trigger logout (they re-render the existing per-view error state). A small `lib/api/authState.ts` store holds the current-user resource; the 401 handler in `fetchJson` notifies it.
- **Phase 6 — route guarding.** URL-driven views (the existing `lib/app/navigation.ts` machinery) MUST refuse to render the inventory/terminal routes when `currentUser` is null. The router shows `LoginView` regardless of the URL path until login succeeds; the originally-requested path is preserved in component state and restored after login. This avoids a flash of empty inventory between login complete and `currentUser` resolved.
- **No client-side persistence of auth material.** The session cookie is the only auth state the SPA holds. Local storage MUST NOT carry the session token, the password, the bootstrap token, or any decoded session payload. Stable selector and redaction-sentinel tests pin this — `apps/web/tests/authPersistence.test.ts` (NEW) asserts no auth-shaped string ever reaches `localStorage` or `sessionStorage`.
- **Tauri shells.** Desktop and mobile shells inherit the cookie via the WebView. Production deploys MUST configure `auth.allowed_origins` to include the Tauri custom scheme (`tauri://localhost` or the platform-specific equivalent) — otherwise the Origin-header CSRF check rejects the SPA's writes. Mobile-network reconnects continue to work because the session is server-side; a flaky network just retries the request, the cookie travels with it.

### Audit events

The auth surface emits exactly the kinds enumerated in `audit_events_kind_chk`. New kinds REQUIRE a paired migration extending the CHECK + the `AuditEventKind` Rust enum in lockstep (see "Audit-event expectations").

| Event | Kind | `actor_id` | Allowed payload fields |
|---|---|---|---|
| Successful password login | `login_succeeded` | logged-in `user_id` | `user_id`, `method: "password"`, `login_at` |
| Failed login (any reason) | `login_failed` | `NULL` | `method: "password"`, `reason: "bad_credentials" \| "throttled"` |
| Logout | `logout_succeeded` | logged-out `user_id` | `user_id`, `session_id` (= `user_sessions.id`, the UUID primary key — NEVER the cookie token bytes or its hash), `logout_at` |
| Session revoked (admin / list-revoke) | `session_revoked` (NEW) | revoking `user_id` | `user_id`, `revoked_session_id` (= `user_sessions.id`, NOT the cookie token), `reason` |
| First user created | `first_user_created` (NEW) | the new `user_id` | `user_id`, `created_at` (`email` and `display_name` are deliberately excluded — they are PII reachable via a normal `users` query scoped to `actor_id`, and including them in audit `payload` would survive the `audit_events.actor_id ON DELETE SET NULL` anonymisation contract from "Reference / integrity policy") |
| Password changed | `password_changed` (NEW) | changing `user_id` | `user_id`, `changed_at` |
| Bootstrap-token misuse | `login_failed` | `NULL` | `method: "bootstrap"`, `reason: "bad_token" \| "already_bootstrapped"` |

Forbidden in EVERY auth payload (per the existing audit redaction contract): plaintext passwords, password hashes, session tokens, session token hashes, bootstrap token bytes, the session-signing key, the `client_info` blob, raw russh / DB error text, peer banners. Sentinel-string tests in `crates/relayterm-api/tests/api.rs` MUST extend `AUDIT_FORBIDDEN_SUBSTRINGS` with the new auth-shaped names (`bootstrap_token`, `password_hash`, `session_token`, `argon2id`).

NULL-actor exclusion (see "Current-user audit events read API") keeps `login_failed` rows (which always carry `actor_id = NULL`) out of the per-user audit feed; admin tooling that wants them uses the unscoped `recent` query when it lands.

User-visibility of auth events on the `recent_for_actor` feed:

- `login_succeeded`, `logout_succeeded`, `password_changed`, and `first_user_created` carry the user's own id as `actor_id` and ARE visible on the per-user feed. A user seeing their own sign-ins is the load-bearing UX of an audit feed.
- `session_revoked` carries the revoking user's id as `actor_id`. When a user revokes one of their OWN sessions from the (future) active-sessions list, the row IS visible on their feed (they are both actor and target). When an admin (future) revokes another user's session, the `actor_id` is the admin's id and the target user does NOT see the row on their per-user feed — that is the intended NULL-actor-style isolation. A future "events about me" admin surface (`target_user_id` audit query) is its own slice and is NOT mixed into `recent_for_actor`.
- `login_failed` is `actor_id = NULL` and never appears on any per-user feed.

The frontend `parseAuditEvent` already collapses unknown summary kinds to `generic`, so the new kinds are forward-compatible — the per-kind sanitizer arms can land in a follow-up without breaking the frontend.

### Security properties to test

The first auth slice MUST ship with tests covering each property below. Tests are the spec's enforcement; a property without a test is a property that drifts.

1. **Boot fail-fast.** `auth.mode = production` with no `session_signing_key_*` source ALWAYS refuses to start, regardless of whether a first user already exists — a missing key is a misconfiguration, not a transient state. `auth.mode = production` with both `session_signing_key_*` sources set is rejected as ambiguous. `auth.mode = production` with an empty `auth.allowed_origins` list refuses to start (every browser-write would be 403'd at the CSRF guard otherwise). `auth.mode = production` with `auth.cookie_secure = false` refuses to start (production cookies must carry `Secure`). After the DB connect, `auth.mode = production` with no first user AND no `auth.first_user_bootstrap_token` configured refuses to start (the operator has no path to create a user). `auth.mode = dev` validates with permissive settings (insecure cookies, missing key, empty allow-list are all accepted). Mode-mismatch errors name the failing input but never echo a value.
2. **Password verify is correct.** Argon2id parameters round-trip: a hash produced at parameter `v1` verifies at parameter `v1`; on a successful verify under an older `algo_version` the hash is re-issued at the current parameters.
3. **Session token storage is hashed.** A direct `SELECT * FROM user_sessions WHERE token_hash = $1` with the plaintext token in `$1` returns zero rows. Only `SHA-256(token)` matches. A cookie-replay test confirms the plaintext token cookie still authenticates.
4. **Cookie flags are set.** `Set-Cookie` from a successful login carries `HttpOnly`, `Secure`, `SameSite=Strict`, and a `Max-Age` matching `expires_at`. Production-mode test asserts `Secure`; dev-insecure-mode test asserts `Secure` is absent AND that the dev warning was logged.
5. **CSRF rejects bad Origin.** A POST with a missing `Origin` header → 403. A POST with an `Origin` outside `auth.allowed_origins` → 403. A POST with an exact match → passes the CSRF gate (whether it then 200s or 401s depends on auth, which is a separate test). GETs are exempt.
6. **Every protected route requires auth.** A direct GET / POST to every `/api/v1/*` route WITHOUT a session cookie returns `401 unauthorized` with the static `unauthorized` body. The list of routes is enumerated explicitly in the test so a future route is forced to opt in.
7. **Cross-user isolation.** User A's session cookie cannot read User B's hosts, profiles, identities, terminal sessions, or audit events. A foreign id collapses to `404 not_found` byte-identical to a genuine 404 (the existing `get_by_id` ownership-filter rule, see AGENTS.md "Encountered Lessons" 2026-04-28).
8. **Logout invalidates.** A logout response stamps `revoked_at`; the next request with the same cookie returns 401. A second logout with the same cookie ALSO returns 401 (idempotent — a revoked session is indistinguishable from a missing one).
9. **Audit redaction.** A login attempt whose payload would otherwise smuggle a password, hash, or session token into `login_succeeded` / `login_failed` finds none of those substrings in the persisted row OR in the `audit_events_recent` response. Sentinel tests at the route layer AND the DTO layer.
10. **DevUser is unreachable everywhere.** The `DevUser` extractor and the `AppState::dev_user_id` field have been deleted from the codebase. Every route takes `AuthenticatedUser` and returns 401 to a missing cookie regardless of `auth.mode`; there is no remaining path that can stamp a fabricated identity onto a request. A grep for `DevUser` / `dev_user_id` in the source tree returns zero hits.
11. **Throttling triggers and clears.** N+1 failed logins against the same normalized email return `429 too_many_requests` (`ApiError::TooManyRequests`, wire `code: "too_many_requests"`); a successful login clears the bucket. Unknown-email and wrong-password share the bucket (probe-resistance); CSRF-rejected attempts do NOT touch the throttler (lockout-by-third-party prevention); the throttled response carries no `Retry-After` header in v1. The threshold is injectable per-test (`Arc<LoginThrottler>` on `AppState`) so the suite drives a tight bucket without waiting on the production 15-minute window. **IP-aware keying remains deferred** — until `ConnectInfo` is plumbed through the listener, the throttle key is the normalized email only; multi-instance deploys layer reverse-proxy rate-limiting on top per `docs/production-auth.md`.
12. **Bootstrap is one-shot.** A successful bootstrap flips the route to `409 conflict { entity: "user", reason: "already_bootstrapped" }` for every subsequent attempt, including with the same token, a different token, and a malformed token.

### Implementation order

This is the recommended staged plan. Each item is its own slice; do not bundle. Earlier items unblock later items and the cutover to production auth.

1. **Auth-mode + config plumbing.** ✅ **Landed.** `apps/backend/src/config.rs` carries `AuthConfig { mode, session_signing_key_b64, session_signing_key_file, first_user_bootstrap_token, cookie_secure, cookie_domain, allowed_origins }` with the same `Debug`-redaction posture as `VaultConfig` / `FileVaultConfig` (the secret-shaped fields render as `_set: bool` markers; `FileAuthConfig` mirrors the redaction so the deserialized intermediate cannot re-introduce the leak). `Config::validate_auth` runs in `apps/backend/src/main.rs` BEFORE any irreversible work (db connect, ssh services, listener bind). Policy: `auth.mode = dev` → permissive (insecure cookies, missing signing key, empty allow-list all accepted); `auth.mode = production` → enforce exactly-one-signing-key-source, non-empty `allowed_origins`, `cookie_secure = true`. After the DB connect, `main.rs` adds a runtime gate: production with no first user AND no `first_user_bootstrap_token` → `bail!`. The default mode is `dev`. Reserved keys are read from `RELAYTERM_AUTH__MODE`, `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64`, `RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE`, `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN`, `RELAYTERM_AUTH__COOKIE_SECURE`, `RELAYTERM_AUTH__COOKIE_DOMAIN`, and `RELAYTERM_AUTH__ALLOWED_ORIGINS` (comma-separated). The `[auth]` TOML section mirrors the same names. Unknown `auth.mode` values are rejected at parse time (TOML via serde rename, env via `AuthMode::from_str`) — never silently coerced. The legacy `RELAYTERM_DEV_AUTH__ENABLED` env var and `[dev_auth]` TOML section are silently ignored (legacy operator config does not block a load). Property-1 tests landed in the same module as the existing vault redaction tests and pin every production-mode failure mode (missing key, both keys, empty allow-list, cookie_secure=false) plus the redaction posture across all four error paths.
2. **Schema migrations + repositories foundation.** ✅ **Landed (partial — passwords + sessions only).** Two migrations are in place: `20260501000013_user_passwords.sql` and `20260501000014_user_sessions.sql`. The audit-kind extension (`first_user_created`, `password_changed`, `session_revoked`) is paired with step 4 (the route slice that emits them) so the migration and the emitter ship together; until then the kinds are documented above but not in the CHECK constraint. `relayterm-core` carries `PasswordCredential`, `UserSession`, the `UserSessionId` newtype, the `CreatePasswordCredential` / `CreateUserSession` inputs, and the `PasswordCredentialRepository` / `UserSessionRepository` traits. `relayterm-db` provides `PgPasswordCredentialRepository` and `PgUserSessionRepository`, both reachable via `Db::password_credentials()` / `Db::user_sessions()`. No routes, no extractor, no auth-service wrapper yet — the schema is reachable only through the repositories.

   - **`user_passwords` columns.** `user_id UUID PK REFERENCES users(id) ON DELETE CASCADE` (the original sketch's choice; an orphan password row would never be reachable, so cascade is the only sensible behavior); `password_hash TEXT NOT NULL` (Argon2id PHC string — the algorithm and parameters are encoded in the string itself, which is why no separate `algo_version` column was added); `password_changed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`; `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`; `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`. Upsert (the only mutation) overwrites `password_hash` and bumps `updated_at` and `password_changed_at`; `created_at` is preserved across upserts. A future re-hash-on-parameter-upgrade flow uses the same `upsert_for_user` call site — bumping `password_changed_at` on every re-hash is acceptable because the audit `password_changed` event is what carries semantic intent; the timestamp is metadata.
   - **`user_sessions` columns.** `id UUID PK` (NOT the cookie value — the stable session identifier referenced by `logout_succeeded.session_id` and `session_revoked.revoked_session_id` audit payloads); `user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE` (orphan sessions after a user delete would be a logout-bypass); `token_hash BYTEA NOT NULL UNIQUE` (SHA-256 digest of the random cookie token); `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`; `last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`; `expires_at TIMESTAMPTZ NOT NULL`; `revoked_at TIMESTAMPTZ NULL`; `revoked_reason TEXT NULL` (a short free-form code such as `"logout"` / `"admin_revoke"`; display metadata only, never an auth input). Indexes: the unique on `token_hash` (constraint name `user_sessions_token_hash_key`), one on `user_id` (for revoke-all-for-user and the future active-sessions list), one on `expires_at` (for the future sweeper). `remote_addr` and `user_agent` are intentionally deferred — the active-sessions list is the only consumer and that surface lands later. Adding the columns now without a writer would normalize empty / NULL display metadata into the rows from day one and lock that shape in.
   - **Repository contract: passwords.** `upsert_for_user(input)` is the only mutation; `get_for_user(user_id)` returns the row or `None`. There is no `delete_for_user` — a password row's lifecycle is tied to its user via `ON DELETE CASCADE`, and a "remove password without deleting the user" surface does not exist in v1. A foreign-key failure (no matching `users.id`) is mapped to `RepositoryError::Database` via the underlying constraint — the auth service is the only caller and is expected to ensure the user row exists first.
   - **Repository contract: sessions.** `create(input)` inserts a fresh row (duplicate `token_hash` → `RepositoryError::Conflict { entity: "user_session", constraint: "user_sessions_token_hash_key" }`; the constraint name never echoes the digest bytes). `get_by_token_hash(&[u8])` is the only auth-extractor lookup; `get(id)` is for management code that already knows the row. Neither method filters on `revoked_at` / `expires_at` — the auth service / extractor is the single place that enforces the policy, so the SQL stays trivial and there is no second source of truth to drift. `touch_last_seen(id, at)` updates the timestamp; `revoke(id, at, reason)` is idempotent (a second call against an already-revoked row preserves the original `revoked_at` and `revoked_reason` so the audit trail remains honest); `revoke_all_for_user(user_id, at, reason)` returns the number of rows transitioned from non-revoked to revoked so the caller can decide whether to write any audit events. An unknown id on `touch_last_seen` / `revoke` returns `RepositoryError::NotFound { entity: "user_session" }`; an unknown user on `revoke_all_for_user` returns `0`.
   - **Redaction contract (load-bearing — sentinel-tested).** `PasswordCredential::Debug` redacts `password_hash` to `<redacted: N chars>`. `UserSession::Debug` redacts `token_hash` to `<redacted: N bytes>`. `CreatePasswordCredential::Debug` redacts `password_hash`. `CreateUserSession::Debug` redacts `token_hash`. The private SQLx row structs (`PasswordCredentialRow`, `UserSessionRow`) deliberately do NOT derive `Debug` — the redacting domain types are the only thing reachable to a formatter outside the row module. `RepositoryError::Conflict` strings carry the schema constraint name only (no digest, no hash, no user input, no SQL fragment); the existing sentinel-string tests at the route layer (`AUDIT_FORBIDDEN_SUBSTRINGS` in `crates/relayterm-api/tests/api.rs`) extend to cover `password_hash`, `argon2id`, `session_token`, and `bootstrap_token` once step 4 lands and these substrings are reachable through a route.
   - **What plaintext NEVER reaches this layer.** Plaintext passwords are not modeled at the domain or repository level — the auth service hashes them before constructing `CreatePasswordCredential`. Plaintext cookie tokens are not modeled here either — the auth service generates the token, SHA-256-hashes it, and passes only the digest as `CreateUserSession::token_hash`. Any future caller that inverts this — e.g. a "store the password and hash it on the way out" helper — is a spec bug, not a refactor. There are no API surfaces that read or return password material or session-token material from this layer.

   `cargo sqlx prepare --workspace` is intentionally NOT required by this slice: the project uses the runtime SQLx API (`sqlx::query` / `sqlx::query_as::<_, RowType>`) rather than the compile-time-checked macros, as documented in `crates/relayterm-db/src/lib.rs`. Step 4 (the route slice) decides whether to migrate hot queries to the macros at the same time it adds them.
3. **Auth service.** ✅ **Landed (service primitives only — no routes).** The repository traits and sqlx impls (`relayterm-core::repository::{PasswordCredentialRepository, UserSessionRepository}` + `relayterm-db::{PgPasswordCredentialRepository, PgUserSessionRepository}`) already landed in step 2. This step adds `relayterm-auth::AuthService` plus the password and session-token primitives it composes; no HTTP routes, cookie wiring, CSRF middleware, or extractor changes ship in this slice (those land in steps 4–6).

   - **`relayterm-auth::password`.** `PasswordHasher` wraps `argon2 = "0.5"` (Argon2id). Default parameters are `PasswordHasherConfig::OWASP_2023` (`m=19456 KiB`, `t=2`, `p=1`) — the OWASP 2023 baseline; tests pin the constant so a future PR cannot silently weaken it. `hash_password(&str) -> Result<String, PasswordHashingError>` produces a fresh-salt `$argon2id$...` PHC string; `verify_password(&str, &str) -> Result<bool, PasswordHashingError>` returns `Ok(true)` on match, `Ok(false)` on a structurally-valid wrong-password verify, and `Err(InvalidStoredHash)` only when the stored value is not a PHC string at all (the service collapses this to `InvalidCredentials` so a probe cannot distinguish "your password is wrong" from "the row is corrupt"). `PasswordHasher`, `PasswordHasherConfig`, and `PasswordHashingError` all redact in `Debug` (parameter numerics never appear in formatter output, and a malformed-hash error never echoes either input). Tests injected a tuned-down hasher (`t=1`) so the suite runs in well under a second; production callers use `PasswordHasher::default()`.
   - **`relayterm-auth::session_token`.** `SessionToken::generate()` reads 32 bytes from `OsRng` and URL-safe-base64-encodes them with no padding — the resulting cookie value is exactly 43 ASCII characters from the URL-safe alphabet (`A-Za-z0-9-_`). `SessionToken` exposes the encoded bytes only via `expose() -> &str` (the single legitimate caller is the future `Set-Cookie` writer in step 4); there is no `Display`, no `serde`, and `Debug` redacts to `<redacted: N chars>`. The wrapper zeroizes on drop. `hash_session_token(&str) -> SessionTokenHash` is a free function so the future auth extractor can hash the cookie value without instantiating a service. `SessionTokenHash` is a `[u8; 32]` newtype with `as_bytes` / `into_bytes` constructors for the repository's `get_by_token_hash(&[u8])` and `CreateUserSession::token_hash` fields respectively; it also redacts in `Debug`. The plaintext token crosses the service boundary exactly once — as the `token` field of `CreatedSession` returned from `AuthService::create_session`.
   - **`relayterm-auth::AuthService`.** Composes `Arc<dyn PasswordCredentialRepository>` + `Arc<dyn UserSessionRepository>` + a `PasswordHasher`. Methods (all async): `set_password(user_id, plaintext)`, `verify_password(user_id, plaintext)`, `create_session(user_id, ttl, now) -> CreatedSession`, `validate_session_token(plaintext_token, now) -> UserSession`, `revoke_session(id, now, reason)`, `revoke_all_for_user(user_id, now, reason) -> u64`. Time is passed in as `DateTime<Utc>` rather than read from a clock trait — the surface stays small and tests stay literal. `verify_password` collapses every failure shape (no row, wrong password, corrupt stored hash) into a single `InvalidCredentials` so a probe cannot distinguish them. `validate_session_token` keeps `SessionInvalid` / `SessionExpired` / `SessionRevoked` distinct internally but the route layer (step 4) MUST collapse them to one 401 body on the wire. `validate_session_token` does NOT touch `last_seen_at` — that is the future extractor's responsibility (best-effort, error-tolerant — see "Auth extractor and route migration"). `revoke_session` returns `SessionInvalid` for an unknown id (not `Repository`) so a probe cannot distinguish "your id is unknown" from "your session was already revoked"; idempotent re-revoke is a no-op that preserves the original `revoked_at` and `revoked_reason` so the audit trail stays honest.
   - **Error posture (sentinel-tested).** `AuthServiceError` variants are structural: `InvalidCredentials`, `SessionInvalid`, `SessionExpired`, `SessionRevoked`, `Repository(String)`, `Crypto`. `Display` and `Debug` for any error never echo the offered password, the stored hash, the offered token, or the stored digest. The `Repository` variant wraps the upstream `RepositoryError`'s `Display` — that string is already redaction-safe per the repository contract. `Crypto` deliberately drops the wrapped `PasswordHashingError` detail so the public string is fixed (the audit-substring tests in step 4 pin this).
   - **Dependencies added (workspace).** `argon2 = "0.5"` (with the `std` feature; defaults already include `password-hash` + `rand`). `password-hash` is consumed transitively via `argon2::password_hash::*` and is NOT a separate workspace entry. `rand`, `sha2`, `zeroize`, and `base64` were already in the workspace for the vault.
   - **What this slice intentionally does NOT do.** No `bootstrap` / `login` / `logout` / `me` routes; no cookie reading or writing; no CSRF middleware; no `AuthenticatedUser` extractor; no frontend auth UI; no passkeys; no production-auth enablement; no audit-event emission of `login_succeeded` / `login_failed` / `logout_succeeded` / `password_changed` / `session_revoked` / `first_user_created`. The audit-kind extension migration is still paired with step 4. Property 8 (logout invalidates at the wire layer) and properties 4, 5, 9, 10, 11, 12 are still route-level tests deferred to later steps.
4. **Login / logout / bootstrap / me API plus inline CSRF guard.** ✅ **Landed.** `POST /api/v1/auth/bootstrap`, `POST /api/v1/auth/login`, `POST /api/v1/auth/logout`, `GET /api/v1/auth/me`. Cookie set / clear in `axum`. Audit-event emission. **Because step 6 has not landed yet, every state-changing auth route carries an INLINE Origin-header check (the small per-route helper [`AuthRoutesConfig::check_origin`], not shared middleware) so login/logout/bootstrap are not CSRF-vulnerable in the gap between steps 4 and 6.** When step 6 lands, the inline check is removed in the same commit that wires the shared middleware so there is no gap and no double-check.

   - **Routes.** All four routes are mounted under `/api/v1/auth/*` in `crates/relayterm-api/src/routes/v1/auth.rs`. Bootstrap creates the first user only (no session minted; the SPA calls `/auth/login` next). Login mints a 30-day session via [`AuthService::create_session`] and emits the cookie. Logout reads the cookie, revokes the matching session row through [`AuthService::revoke_session`] (idempotent at the repository), and writes a clear-cookie header. `GET /auth/me` validates the cookie via [`AuthService::validate_session_token`] and returns the safe `UserResponse` DTO.
   - **Cookie wire shape.** `relayterm_session=<43-char URL-safe-base64 token>; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`. `Secure` is appended when `auth.cookie_secure = true`. `Domain=<...>` is appended when `auth.cookie_domain` is set. The plaintext token is the only byte sequence that crosses the boundary unhashed; `crates/relayterm-auth::session_token::SessionToken::expose` is the single legitimate caller of the cookie writer per AGENTS.md ("Don't ... stash, log, or pass-around the plaintext value of a `SessionToken`").
   - **Origin guard policy.** Both a missing `Origin` header AND an `Origin` value not present in `auth.allowed_origins` produce `403 forbidden { code: "csrf_origin_mismatch" }`. An empty `allowed_origins` list rejects every write — that is the secure default; tests / dev populate it explicitly. `GET /auth/me` is exempt from the inline guard (idempotent read; same exemption SPEC step 6's middleware will preserve).
   - **Audit emission.** `first_user_created` (paired migration `20260501000015_audit_events_first_user_created_kind.sql` extends the CHECK; the matching `AuditEventKind::FirstUserCreated` lands in lockstep), `login_succeeded`, `login_failed`, and `logout_succeeded` (all three were already in the CHECK from a prior slice). Failure-path audits on bootstrap (`bad_token`, `already_bootstrapped`) and login (`bad_credentials`) reuse `login_failed` with `actor_id = NULL` and a `payload.method` discriminator — `"bootstrap"` vs `"password"`. Audit failures on probe / failure paths are best-effort (a transient DB failure on the audit append does not turn a 401 into a 500); audit failures on the success paths (bootstrap → `first_user_created`, login → `login_succeeded`, logout → `logout_succeeded`) are fail-closed and surface as 500 to the caller, mirroring the partial-success contract documented for `create_session` and the server-profile lifecycle audit. Payloads contain public metadata only — sentinel-string redaction tests pin that no `password` / `password_hash` / `session_token` / `token_hash` / `bootstrap_token` / `argon2id` value reaches a persisted row.
   - **Production-auth enablement (status at the time of this slice).** Was still fail-fast at boot — `Config::validate_auth` rejected `auth.mode = production` until the route migration and shared CSRF middleware landed. Step 10 retired the gate; production now boots when the configuration envelope is satisfied.

   Tests:
   - DTO redaction unit tests pin that `BootstrapRequest` / `LoginRequest` `Debug` redacts the bootstrap token and password to length-only markers; that `UserResponse` serialization carries no secret-shaped names; that the validation error paths never echo the offered token, password, or email value.
   - Auth-route module unit tests pin the cookie format (`HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`; `Secure` only when configured), the Origin-guard's allow / deny / missing / empty-allowlist / non-UTF-8 cases, the `AuthRoutesConfig::Debug` bootstrap-token redaction, the constant-time bootstrap-token compare, and the `Cookie:` header parser.
   - Postgres-backed integration tests (in `crates/relayterm-api/tests/api.rs`, `postgres-tests` feature) cover: bootstrap creates the first user (and does NOT mint a session cookie); bootstrap rejects a wrong token without echoing the attempted value; bootstrap is one-shot (the second call returns `409 conflict { reason: "already_bootstrapped" }`); bootstrap returns 503 when `auth.first_user_bootstrap_token` is unset; login succeeds and sets a `HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000` cookie; login wrong-password returns 401 and writes a `login_failed` audit row that does NOT echo the password; login unknown-email is byte-identical to login wrong-password (probe resistance); `GET /auth/me` returns the user for a valid cookie and 401s a missing / unknown / revoked cookie; logout revokes and clears the cookie; logout is idempotent for missing / unknown cookies (no `logout_succeeded` row is written for the no-op paths); the inline Origin guard rejects missing and disallowed origins on the write routes; `GET /auth/me` is exempt from the Origin guard (no Origin → 401, not 403). Property 8 (logout invalidates at the wire layer) and properties 4, 6 (partial — for the four new auth routes), 9, and 12 are exercised at this layer; properties 5 and 10 still belong to later slices.
5. **`AuthenticatedUser` extractor.** ✅ **Landed.** `crates/relayterm-api/src/auth/` ships the cookie-backed `AuthenticatedUser` extractor (`auth/user.rs`) plus a shared cookie helper (`auth/cookie.rs`) consumed both by the extractor and by the `/api/v1/auth/*` routes. The extractor parses the `Cookie:` header (exact-match on `relayterm_session`; missing / non-UTF-8 / empty-value / prefix-named / suffix-named cookies all collapse to a single 401 indistinguishable from "no cookie"), hashes the token via `relayterm_auth::hash_session_token`, validates it through `AuthService::validate_session_token` (revoked → 401, expired → 401, unknown → 401), then loads the `User` row by id (missing → 401). Failures collapse on the wire to the static `unauthorized` envelope; operator-side detail (`missing session cookie` / `session invalid` / `session expired` / `session revoked` / `session references missing user`) survives in the `warn!` line in `error.rs::IntoResponse`. The handler surface exposes `user_id() -> UserId`, `user() -> &User`, and `into_user() -> User`. The session token, the token hash, and the session row are NEVER reached by the handler — only the resolved `UserId` and `User` are. **`last_seen_at` is stamped on every successful extraction** via `db.user_sessions().touch_last_seen(session.id, now)`, awaited inline (no `tokio::spawn`-and-forget per the AGENTS.md concurrency rule). The touch is best-effort: a repository failure logs at `warn!` with the session id only (never the cookie, token hash, or repository internals) and the request still succeeds. Failed / expired / revoked / unknown extractions never reach the touch — the early returns above guarantee `last_seen_at` is updated only on the happy-path. The future session-management UI consumes this column. Tests: `auth::cookie::tests` pins the parser's exact-match policy across single / multiple / prefix-named / suffix-named / empty-value / non-UTF-8 / no-equals / duplicate / whitespace-padded fixtures (11 unit cases). Postgres-backed integration tests in `crates/relayterm-api/tests/api.rs` cover: `me` returns the user for a valid cookie via the extractor, `me` rejects a missing cookie (401), `me` rejects an unknown cookie (401), `me` rejects an expired session (row inserted with `expires_at` in the past; 401), `me` rejects a revoked session (revoked at the repository; 401), `me` rejects a prefix-confusion cookie (`relayterm_session_other=<real-token>`; 401), the `/me` 200 response carries no `password` / `password_hash` / `session_token` / `token_hash` / `bootstrap_token` / `argon2id` substring AND no sentinel-shaped string from the test password / token / bootstrap secret, AND the `last_seen_at` touch contract: a successful `/auth/me` advances `last_seen_at`, a successful protected `/api/v1/hosts` GET advances it (proves the touch rides on the shared extractor), an expired session does NOT advance it, a revoked session does NOT advance it, and an unknown-token request creates no row AND leaves any pre-existing legitimate session's `last_seen_at` untouched. The auth crate graduated from `NotImplemented` in step 3.
6. **Shared CSRF / `Origin` guard foundation.** ✅ **Landed.** `crates/relayterm-api/src/auth/csrf.rs` ships the shared helper [`check_origin(&HeaderMap, &[String]) -> Result<(), ApiError>`] and the [`CsrfGuard`] axum extractor (`FromRequestParts`) that wraps it. Every browser-write route takes `_csrf: CsrfGuard` as its first extractor — placed ahead of `Json<...>` so the rejection happens before request bytes are parsed and before any DB or auth work runs. (Ordering note: axum 0.8 runs every `FromRequestParts` extractor before the single `FromRequest` body extractor regardless of source order, so the rejection-before-body-parse guarantee is enforced by axum's invariant; the "ahead of `Json<...>`" placement is convention that keeps the call site self-explanatory and is pinned by the integration tests — not a load-bearing source-order requirement.) Wire policy: missing / non-UTF-8 / non-allowlisted `Origin` → `403 forbidden { code: "csrf_origin_mismatch" }`; empty `allowed_origins` rejects every write; `GET /auth/me` and the WebSocket attach route are exempt. The wrapped operator-side detail strings (`missing Origin header` / `Origin header is not valid UTF-8` / `Origin not in allowed_origins`) are deliberately classified — they never echo the offered `Origin` value. Comparison is **case-sensitive byte equality**; a case-insensitive variant is deferred (handling internationalised hostnames safely is its own slice). **Out of scope (deliberate).** No double-submit token (deferred until a non-same-origin client lands per "CSRF posture"); no route-wide `tower` middleware (the extractor approach gives per-route scope without a global allow-list of GET routes). **Tests.** `auth::csrf::tests` pins ten unit cases (allow / deny / missing / empty-allowlist / non-UTF-8 / case-sensitivity / trailing-slash / multi-origin allow / two distinct sentinel-Origin redaction cases). Postgres-backed integration tests in `crates/relayterm-api/tests/api.rs` cover: bad Origin rejects BEFORE body parsing (a malformed JSON body paired with a disallowed Origin returns 403 not 400); a CSRF-rejected login does NOT write a `login_failed` audit row (no auth work runs); a CSRF-rejected bootstrap creates no user row AND emits zero auth audit rows; the same shape applies to every other browser-write route (`create_host_bad_origin_returns_403_before_body_parse`, `disable_with_bad_origin_returns_403_and_writes_no_audit`, etc.).
7. **Route migration (Phase B).** ✅ **Landed.** Every protected `/api/v1/*` app route takes `AuthenticatedUser`. The migrated surfaces are: `hosts` (create/list/get), `ssh-identities` (create/list/get), `server-profiles` (create/list/get + disable/enable + host-key-preflight + trust-host-key + auth-check), `terminal-sessions` (create/list/get/close + the WebSocket attach route), and `audit-events/recent`. Browser-write routes (`POST` / state-changing handlers) additionally take the shared `_csrf: CsrfGuard` extractor as their first parameter so a missing or non-allowlisted `Origin` header rejects with `403 csrf_origin_mismatch` BEFORE the body is parsed AND BEFORE any DB / auth / lifecycle work runs. The WebSocket attach route is `GET` and therefore exempt from `CsrfGuard`; its auth gate is the cookie-backed `AuthenticatedUser` extractor which short-circuits BEFORE the upgrade handshake completes (clients see a clean HTTP 401, not an opened-then-closed socket). Ownership filtering shape: handlers extract `UserId` via `user.user_id()`, repository queries stay scoped to `owner_id = caller`, foreign-vs-missing collapses to a byte-identical 404. The `into_create(owner_id: UserId)` DTO methods on `CreateHostRequest` / `CreateServerProfileRequest` take a bare `UserId`; audit lifecycle helpers (`write_lifecycle_audit`, `resolve_owned_profile`) likewise. Tests: every fixture (`setup`, `setup_with_probe`, `setup_full`, `setup_with_auth_check_service`, `setup_with_full_state`, `setup_with_full_state_short_ttl`, `setup_with_fake_probe`, `setup_with_fake_auth_checker`, `setup_with_pty_bridge`, `setup_with_first_user`, `setup_production_first_user`) bootstraps a real `AuthService` session via `bootstrap_test_session(&auth, user_id)` and returns the cookie-token plaintext as the last tuple element. The `json_post(uri, body, cookie)` and `get(uri, cookie)` request builders attach the cookie + Origin (POST only) automatically; `json_post_no_auth` / `get_no_auth` cover the missing-cookie 401 paths; `json_post_with_origin(uri, body, cookie, origin)` covers the bad-Origin 403 paths. WebSocket helpers (`open_ws`, `open_ws_attached`, `ws_handshake_status`) take the cookie token and attach it to the upgrade handshake. Integration tests cover: `protected_hosts_routes_return_401_without_session_cookie` (GET + POST `/hosts` reject when no cookie), `post_ssh_identity_returns_401_without_session_cookie`, `auth_check_returns_401_without_session_cookie`, `terminal_session_routes_return_401_without_session_cookie` (covers create / list / get / close on the v1 surface), `ws_attach_returns_401_without_session_cookie` (the WebSocket handshake fails BEFORE upgrade), `audit_events_recent_unauthorized_without_session_cookie`, `create_host_bad_origin_returns_403_before_body_parse` (malformed JSON body + disallowed Origin → 403, not 400; no row written), `create_host_missing_origin_returns_403`, and `disable_with_bad_origin_returns_403_and_writes_no_audit` (lifecycle audit row count stays 0 on the bad-Origin path). Property 5 (CSRF rejects bad Origin) is covered for at least one representative route per surface; property 7 (cross-user 404 indistinguishable) and property 10 (no fabricated identity) are exercised against the extractor.
8. **Throttling.** ✅ **Landed (foundation — email-keyed only).** `crates/relayterm-auth::throttle` ships `LoginThrottler` (in-memory map behind a `std::sync::Mutex`; no I/O under the lock so safe to share across an async runtime), `LoginThrottleConfig` (v1 default: 5 failures / 15-min window → 15-min block), `ThrottleDecision { Allowed, Throttled { retry_after_seconds } }`, and `normalize_login_identifier` (lower-case + trim). Wired into `AppState::login_throttler` and consumed by `POST /api/v1/auth/login` ahead of the user lookup; the route emits `ApiError::TooManyRequests` on a hit (new variant, wire `code: "too_many_requests"`, status 429, static `"too many requests"` body). Audit emits `login_failed` with `reason = "throttled"` (best-effort, mirroring the bad-credentials path). `record_failure` runs on BOTH the wrong-password and unknown-email branches so the throttle channel preserves the same probe resistance the wire response does. `record_success` clears the bucket on a correct login. Bounded at 10,000 keys via opportunistic cleanup; full-map insert silently no-ops (fail-open under saturation). **What this slice does NOT include.** IP-aware keying (deferred until `ConnectInfo` is plumbed through the listener); distributed / Redis-backed limiter (multi-instance deploy still relies on reverse-proxy rate-limiting per `docs/production-auth.md`); `Retry-After` header (would leak throttle-key telemetry — re-evaluate if/when a SPA needs the countdown UI); a configurable policy on `AuthConfig` (constants in code for v1 — bumping the policy is a code change). Property 11 is exercised by `login_throttle_blocks_after_threshold_with_safe_response`, `login_throttle_unknown_user_shares_bucket_with_known_user`, `login_failed_audit_reasons_split_bad_credentials_and_throttled`, `successful_login_clears_throttle_bucket`, `bad_origin_login_does_not_engage_throttler`, and `login_throttle_is_keyed_on_normalized_email` in `crates/relayterm-api/tests/api.rs`, plus 13 deterministic unit tests in `crates/relayterm-auth/src/throttle.rs::tests`.
9. **Frontend auth phases 1–6.** ✅ **Landed (foundation — phases 1–4; phases 5–6 still deferred).** `apps/web/src/lib/api/auth.ts` ships typed helpers for `getCurrentUser`, `login`, `logout`, and `bootstrap` plus the field-by-field `parseCurrentUser` parser, the `describeAuthError` formatter (function of `kind` + `status` + `code` only), and frontend-side `validateLoginForm` / `validateBootstrapForm` mirrors of the backend bounds. Every helper sets `credentials: "include"` so the browser ships the `relayterm_session` cookie; nothing in the SPA reads, writes, or echoes the cookie value. The `Origin` header is never set from JS — the browser controls it on POSTs and the backend's CSRF guard is appeased by the browser-attached value. `apps/web/src/lib/app/auth/AuthGate.svelte` mounts at the top of `App.svelte`, calls `getCurrentUser()` on mount, and short-circuits the rest of the SPA: a small `auth-loading` splash while in flight, `auth-error-screen` (with explicit retry; no auto-retry storm) on transport / 5xx / malformed, `LoginView` on HTTP 401, and the existing `AppShell` view tree on a parsed user. `LoginView` (`auth-login-*` selectors) submits to `POST /api/v1/auth/login` and collapses the wire 401 to a generic "invalid credentials" line — the copy never reveals whether the offered email belongs to a known account. A "First-time setup" affordance switches the unauthenticated screen to `BootstrapView` (`auth-bootstrap-*` selectors); bootstrap creates the user, shows "Account created. Please sign in.", and routes back to `LoginView` (no auto-login — keeping session minting on the login route only). The `TopBar`'s `auth-sign-out` button calls `POST /api/v1/auth/logout` and ALWAYS runs local cleanup afterwards (clears the active-terminal pointer and `activeLaunch`, drops the gate to the login screen) regardless of the wire outcome — a flaky network can never trap an operator in a logged-in UI state. The bootstrap form's `bootstrap_token` is a `<input type="password">`; the SPA does not persist the token, the password, the session token, or any decoded session payload to local storage. The redaction posture is sentinel-tested in `apps/web/tests/authApi.test.ts`: `parseCurrentUser` drops smuggled `password_hash` / `session_token` / `token_hash` / `bootstrap_token` / `private_key` / `encrypted_private_key` / `access_token` / `session_output` field-by-field; `describeAuthError` never echoes the wire `message` or transport detail; `login` / `bootstrap` request inputs (offered password / bootstrap token) never reach an error string or `console.*`. **Phases still deferred.** Phase 5 (a `lib/api/authState.ts` store + a `fetchJson` 401 interceptor that drops the SPA to `LoginView` from any protected `/api/v1/*` 401) is NOT in this slice — protected views still surface their own 401 via the per-view error formatter, which is acceptable until we have a richer story for "session expired mid-flow." Phase 6 (URL-based route guarding that preserves the originally-requested path across login) is also deferred — `AuthGate` short-circuits the entire view tree when unauthenticated, so the gate IS a guard; the "preserve and restore the requested path" affordance is its own slice.
10. **DevUser retirement (Phase C) + production-auth enablement.** ✅ **Landed.** Deleted `crates/relayterm-api/src/dev_user.rs`, dropped `AppState::dev_user_id` and the `FromRef<AppState> for Option<UserId>` impl, dropped `DevAuthConfig` and the `dev_auth` config field, dropped the `bootstrap_dev_user_for_unimplemented_auth` startup call and the `DEV_USER_EMAIL` / `DEV_USER_DISPLAY_NAME` constants. `Config::validate_auth` no longer hard-rejects `auth.mode = production` — it now enforces the production envelope (signing key, allow-list, Secure cookies) and accepts on success. `apps/backend/src/main.rs` adds a runtime gate after the DB connect: production with no first user AND no `first_user_bootstrap_token` configured → `bail!`. The legacy `dev@relayterm.local` user row is no longer auto-bootstrapped at startup; existing rows in deployed databases are not touched (no migration drops them) and behave as a normal user without a password row. Tests: every fixture sets up real cookie-backed auth (no `dev_user_id` field on `AppState` to set); `production_login_sets_secure_cookie_and_authenticates_protected_route` is the wire-level proof that a production-shaped `AuthRoutesConfig` (cookie_secure=true, populated allow-list) mints a `Secure` cookie, that cookie authenticates a former `DevUser`-only route, AND a no-cookie request to the same router still returns 401 (production does not silently bypass auth). `production_no_first_user_no_token_runtime_gate` mirrors the exact predicate the main.rs gate runs and pins all three operator states (no users + no token blocks; no users + token-set proceeds; first-user-exists proceeds regardless of token). `auth_mode_production_with_valid_config_validates`, `auth_mode_production_missing_signing_key_fails_fast`, `auth_mode_production_both_signing_key_sources_set_is_ambiguous`, `auth_mode_production_empty_allowed_origins_fails_fast`, `auth_mode_production_cookie_secure_false_fails_fast`, `auth_mode_production_with_signing_key_file_only_validates`, `auth_mode_production_with_optional_bootstrap_token_validates`, `dev_auth_env_var_is_silently_ignored`, and `legacy_dev_auth_toml_section_is_silently_ignored` pin the new validate_auth policy. The `auth_validation_errors_do_not_echo_secret_env_values` test exercises every reachable production-mode failure path against a sentinel-shaped bootstrap token and signing key.
11. **Optional: passkeys.** New `user_passkeys` table, registration / authentication endpoints, `LoginView` adds the alternate path. Same session shape, same audit shape. Out of scope for the first milestone.
12. **Optional: active sessions list.** `GET /api/v1/auth/sessions`, `POST /api/v1/auth/sessions/:id/revoke`. Same session table, no schema change. Useful once a deployment has multiple devices per user.
13. **Optional: password reset.** Email transport scope; deliberately deferred. Self-hosted operators have DB-level recovery in v1.
14. **Optional: admin / multi-user.** Roles table, invite flow, per-route role check. Deliberately deferred — single-user/self-hosted is the v1 target.

Each step's "definition of done" inherits the standard checklist (tests, sqlx prepare on schema change, audit event reachable, owner-scoping, redaction posture). When the first auth route lands, append an "Encountered Lessons" entry in AGENTS.md if any non-obvious gotcha emerged (cookie flag interaction, middleware ordering, CSRF preflight surprises).

## Integration points

- **PostgreSQL** — primary store for users, sessions, audit log, key vault. sqlx connection pool; `runtime-tokio-rustls`.
- **Vault master key** — 32-byte secret loaded once at boot, supplied via `vault.master_key_b64` (config / `RELAYTERM_VAULT__MASTER_KEY_B64` env) or `vault.master_key_file`. Exactly one source must resolve, or the backend refuses to start. There is no fallback to a randomly generated key — that would orphan all previously stored ciphertext after a restart. Setting `vault.enabled = false` disables backend-generated identities (the POST route returns 503) and lets the rest of the API run.
- **Traefik** — reverse proxy in front of the backend; terminates TLS; routes `/api/*` and `/ws/*`.
- **WireGuard** (optional) — used only when the backend lives on a remote box and SSH targets are reachable only via the WireGuard mesh.
- **Object storage** — TODO if the project ever needs file upload/download via SCP/SFTP. Out of scope for v1.
- **Passkeys / WebAuthn** — deferred. v1 ships password-only authentication with opaque server-side sessions in Postgres bound to an `HttpOnly; Secure; SameSite=Strict` cookie. Real cookie-backed auth runs in both `auth.mode = dev` and `auth.mode = production`; see "Production authentication architecture" above for the validation envelope each mode enforces and the forward-compatible session shape that lets passkeys land later without a session-shape change.

## Out of scope (v1)

TODO — explicit list of features deferred so the agent doesn't "helpfully" implement them. Likely:

- SCP/SFTP file transfer surface.
- Multi-user shared sessions / "screen-share."
- Public-cloud-hosted multi-tenant deployment (v1 is single-tenant Docker Compose).
- iOS Tauri build (Android first; iOS later).
- libghostty-vt state engine swap (planned; xterm.js drives the baseline). The xterm.js baseline adapter (`@relayterm/terminal-xterm`) and the experimental ghostty-web (`@relayterm/terminal-ghostty-web`), restty (`@relayterm/terminal-restty`), and wterm (`@relayterm/terminal-wterm`) adapters have all landed under `packages/terminal-<name>/`.
- Multi-user / team authentication, role-based access control, and an admin / operator surface. v1 is single-user self-hosted; see "Production authentication architecture" for the rationale.
- Email-based password reset / "forgot password" flow. Self-hosted operators have DB-level recovery in v1; mail transport is its own scope.
- Passkey / WebAuthn registration and authentication. Forward-compatible with the v1 session shape; deliberately deferred.
- Active sessions list and per-session revoke UI. Schema accommodates it; no route or UI in v1.

## Open questions

TODO — known ambiguities for the owner to resolve. Each: question, options considered, current default if any.

- Replay buffer policy: fixed bytes vs fixed events vs time-window? Default: TODO.
- How long does a `detached` session linger before auto-close? **Default**: `relayterm_terminal::DETACHED_LIVE_PTY_TTL = 30s`. In-memory only (lost on backend restart). See "Detached-session TTL contract" for the full lifecycle.
- Should the renderer choice be per-session or per-device? Default: per-device.
- Session expiry policy: hard-expire vs sliding-window? **Default (v1)**: `hard_expire` at `created_at + 30 days`. Reconsider only if UX demands it; sliding-window introduces a re-issue race for marginal benefit. See "Production authentication architecture" → "Session model".
- Login throttle thresholds: bucket size and refill rate per `(remote_addr, email)` and per `email`. **Resolved (v1)**: `LoginThrottleConfig::V1_DEFAULT` = 5 failures / 15-minute sliding window → 15-minute block, keyed on the **normalized email only** (IP-aware keying deferred until `ConnectInfo` is plumbed). Constants live in code (`crates/relayterm-auth/src/throttle.rs`); a config knob is deliberately not added in v1 — the policy is single-tenant defensible and any tuning is a deploy redeploy. The test rig drives a tight bucket via per-test `Arc<LoginThrottler>` injection on `AppState`.

---

> When implementation diverges from this spec, run `/agents spec-sync` to surface the drift. Don't update SPEC.md without intent — the spec leading code is the point.
