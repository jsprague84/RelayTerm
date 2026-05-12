# SPEC — Terminal surface area

> Detailed contracts split out of `SPEC.md` for context efficiency. The
> top-level `SPEC.md` is the index; this file is the long form.
>
> AGENTS.md governs *how* code is written; this doc governs *what* the
> terminal-related surfaces do. Drift from any rule here is a spec bug.
>
> Live design that anchors retention/recording lives in
> `docs/terminal-recording.md`.

## Contents

- [Terminal-session lifecycle contract](#terminal-session-lifecycle-contract)
- [Terminal WebSocket attach/detach contract](#terminal-websocket-attachdetach-contract)
- [Frontend terminal-core contract](#frontend-terminal-core-contract)
- [Renderer adapters](#renderer-adapters) — compact summaries; full contracts in [`terminal-adapters.md`](terminal-adapters.md)
- [Production terminal launch UI](#production-terminal-launch-ui)
- [Production terminal sessions list/status UI](#production-terminal-sessions-liststatus-ui)
- [Production terminal settings foundation](#production-terminal-settings-foundation)
- [Production terminal viewport controls](#production-terminal-viewport-controls)
- [Production terminal paste safety](#production-terminal-paste-safety)
- [Production active terminal local recovery](#production-active-terminal-local-recovery)
- [Production session status refresh and stale-session handling](#production-session-status-refresh-and-stale-session-handling)
- [Live SSH PTY bridge contract](#live-ssh-pty-bridge-contract)
- [Output sequence + in-memory replay buffer contract](#output-sequence--in-memory-replay-buffer-contract)

---

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
- **Failure modes**: `400 invalid_input` for cols/rows out of `1..=4096`; `401 unauthorized` when the session cookie is missing or invalid; `404 not_found` for a missing or foreign-owned profile (create) or session (get/close); `409 conflict { entity: "host_key" }` when no trusted pin exists for the profile's host on create; `500 internal_error` for repository/database failures (static body, never echoes SQL). Responses NEVER contain encrypted private-key bytes, plaintext PEM, fingerprints, peer banners, or terminal I/O.
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

- **Endpoint**: `GET /api/v1/terminal-sessions/:id/ws`. The route resolves the session scoped to the caller's user; missing or foreign-owned ids collapse to a byte-identical `404 not_found` BEFORE the WebSocket handshake completes (no upgrade is performed). A session in `closed` state is rejected with `409 conflict { entity: "terminal_session" }`. A missing or invalid session cookie short-circuits to `401 unauthorized` at the `AuthenticatedUser` extractor — the upgrade never runs. `User-Agent` is captured (length-capped to 256 chars) and persisted on the attachment row as `client_info`; `remote_addr` is recorded as `NULL` until `ConnectInfo` is plumbed through the listener.

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

**Scope (load-bearing — this slice).** A successful `terminal-core` integration attests ONLY to the typed protocol envelope, transport lifecycle, and a renderer-neutral plug interface. It does **NOT** include replay-buffer behavior, real PTY byte streaming on the wire, or any reconnect policy beyond the explicit `attach`/`detach` lifecycle. (The xterm.js baseline renderer adapter — `@relayterm/terminal-xterm` — landed as a separate slice; see [`terminal-adapters.md`](terminal-adapters.md#xtermjs-baseline-renderer-adapter).) Each remaining capability is a separate, deliberate slice.

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

ghostty-web / restty / wterm renderer adapters; real PTY byte streaming through `output` frames; replay-buffer integration on reconnect; auth handshake on the WebSocket beyond the cookie-based `AuthenticatedUser` extractor; per-renderer preference persistence; mobile/Tauri shell integration of the lab UI. Each is a separate, deliberate slice.
### Renderer adapters

The four concrete `TerminalRenderer` adapter packages live under `packages/terminal-<name>/`. Their full contracts — package layout, adapter contract, renderer-neutrality re-affirmation, dev-lab UI, production-bundle tree-shaking, future work — moved to [`terminal-adapters.md`](terminal-adapters.md). The summaries below are the load-bearing facts a reader needs before deciding whether to follow the link.

**Production baseline / experimental rule (load-bearing).** xterm.js is the **production compatibility baseline**. The other three adapters (ghostty-web, restty, wterm) are **experimental and dev-only** — they are wired in the dev lab and tree-shaken out of the production `apps/web` bundle. Production shell components MUST NOT import any experimental renderer adapter (pinned by `apps/web/tests/appShellIsolation.test.ts`). The renderer-neutral rule (`terminal-core` imports nothing renderer-specific; the wire protocol stays RelayTerm-shaped) is stated above under "Frontend terminal-core contract" and re-affirmed per adapter in `terminal-adapters.md`.

| Adapter | Status | Package | Key contract |
|---|---|---|---|
| [xterm.js baseline](terminal-adapters.md#xtermjs-baseline-renderer-adapter) | **production baseline** | `@relayterm/terminal-xterm` | `TerminalRenderer` over xterm.js v5; sync `mount` (allowed once); `write` queues pre-mount; `XtermRendererOptions` is the **first** concrete shape future adapters honor 1:1 for portable knobs; `xtermOnly` escape hatch is non-portable. CSS side-effect import via `/styles`. |
| [ghostty-web experimental](terminal-adapters.md#ghostty-web-experimental-renderer-adapter) | dev-only / experimental | `@relayterm/terminal-ghostty-web` | `TerminalRenderer` over ghostty-web (libghostty-vt-via-WASM, xterm.js-API-compatible `Terminal`). `mount` is async — module-scoped memoized `init()` resolves before `Terminal` construction; `dispose` during pending `init()` cancels silently; `lineHeight` silently dropped; `ghosttyOnly` non-portable. |
| [restty experimental](terminal-adapters.md#restty-experimental-renderer-adapter) | dev-only / experimental | `@relayterm/terminal-restty` | `TerminalRenderer` over `restty/xterm` compat shim (libghostty-vt + WebGPU/WebGL2 + text shaper). Async `mount`; adapter UTF-8-decodes `Uint8Array` writes (the shim takes `string` only); cosmetic knobs accepted on the neutral surface and silently dropped; `resttyOnly` non-portable; honoring native pane / plugin / shader-stage surfaces is future work. |
| [wterm experimental](terminal-adapters.md#wterm-experimental-renderer-adapter) | dev-only / experimental | `@relayterm/terminal-wterm` | `TerminalRenderer` over `@wterm/dom` (Zig+WASM core, DOM-rendered cell grid). DOM/mobile/accessibility-oriented experiment — selection, copy/paste, IME, mobile keyboards flow through native text-handling primitives. `WTerm` constructor mutates the host element synchronously, so construction AND `await init()` are deferred to `mount(element)`; `write` accepts `string \| Uint8Array` directly (no UTF-8 decode); theming/typography go through CSS variables on the `.wterm` host, not options; `cursorBlink` is the one cosmetic knob honored; `wtermOnly` (`autoResize`/`wasmUrl`/`debug`) non-portable. |

Each adapter reaffirms in its own section that `terminal-core` imports nothing renderer-specific, the wire protocol stays RelayTerm-shaped, and the input-redaction rule (no `console.*`, no payload bytes in errors, no neutral-knob echo) is pinned by an adapter-specific test under `packages/terminal-<name>/tests/`. The dev lab (`apps/web/src/lib/dev/XtermLiveTerminalLab.svelte`) is the only consumer of all four adapters today; it gates on `import.meta.env.DEV` so Rollup tree-shakes the experimental adapters (and the xterm.js JS) out of the production bundle. The renderer comparison diagnostic surface is documented under "Live SSH PTY bridge contract → Diagnostic UI → Renderer comparison diagnostics" below.

Renderer-specific knobs go behind the local `<renderer>Only` escape hatch (`xtermOnly` / `ghosttyOnly` / `resttyOnly` / `wtermOnly`) — explicitly NOT promised to behave the same across adapters. The shared `BaseTerminalRendererOptions`, `RendererTheme`, `RendererThemeAnsi`, and `RendererCursorStyle` live in `@relayterm/terminal-core`'s `rendererOptions.ts`; redefining them inside an adapter package is forbidden by the AGENTS.md rule (and pinned by tests).

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
   - On unmount: tear down client + renderer without sending a wire `Close` frame. The session enters the bounded detached-TTL window (deployment-configured, default 30s — see "Detached-session TTL contract") so a re-mount within that window can resume from the captured `lastSeenSeq`.
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

- Detached sessions survive for the deployment's configured detach-TTL window — default 30s (`relayterm_terminal::DETACHED_LIVE_PTY_TTL`), operator-tunable per the "Detached-session TTL contract" below. The SPA reads the effective value at view-mount time via `GET /api/v1/config/session-policy` (module-cached in `apps/web/src/lib/api/sessionPolicy.ts`) and renders honest copy via `formatDetachedTtl` / `describeDetachedTtl`; on transport / HTTP / parse failure the helper falls back to the SPEC-pinned 30s default so the UI is never blocked on the fetch. The dev lab keeps its own independent `~Ns remaining` countdown labelled `approximate, local clock` because the backend's exact REMAINING TTL is not on the wire (only the configured BASE window is).
- Replay is the bounded in-memory ring buffer on the backend. A bookmark older than the buffer surfaces as `replay_window_lost`; the workspace renders no special UI for this beyond resuming the live stream.
- A backend restart drops every detached PTY AND its replay buffer. The workspace explicitly does NOT promise resume-across-restart, durable session recording, or backend-side terminal state observation. These are out of scope.

**Stable selectors.** `production-terminal` (root, carries `data-session-id` and `data-phase`), `production-terminal-phase`, `production-terminal-detach`, `production-terminal-close`, `production-terminal-reconnect`, `production-terminal-dispose`, `production-terminal-back`, `production-terminal-ttl-hint`, `production-terminal-closed`, `production-terminal-error`, `production-terminal-viewport`. ServersView gains `profile-launch-terminal` and `profile-launch-error` (with sibling `profile-launch-error-dismiss`) on each profile row.

**UX copy (load-bearing).**

- Workspace status line: `Status <phase>` + `last_seen_seq <n>` only. No raw payload references.
- Detach hint: parametised on the deployment's configured detach TTL — `describeDetachedTtl(seconds)` produces e.g. "Detached sessions stay reconnectable for about 30 seconds after the last client drop. Replay is in-memory and not durable across a backend restart." Operator-tuned deployments substitute the configured window (minutes / hours / days up to the 24 h validator cap). The persistence disclaimer (in-memory replay, no backend-restart survival) is load-bearing and pinned by the `PERSISTENCE_OVERCLAIM_FORBIDDEN_SUBSTRINGS` sweep in `apps/web/tests/sessionStatus.test.ts`.
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
- **Detached sessions are TTL-bounded.** When `showsTtlHint(status)` is true (only for `detached`), the row renders a disclaimer that names the deployment's configured detach-TTL window, the in-memory replay constraint, and that a backend restart drops everything. The window is parametised on the live value read from `GET /api/v1/config/session-policy` via `loadSessionPolicy`; on fetch failure the SPA falls back to the SPEC-pinned 30s default. Test pin: `sessionStatus.test.ts` asserts both that `describeSessionStatus("detached")` (default) contains the 30 s window AND that `describeSessionStatus("detached", 1800)` correctly renders 30 minutes. The persistence disclaimer (the literal `in-memory` substring AND the `backend restart` phrase) is pinned across every configured window in the same file.
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

Production renderer selector; ghostty-web / restty / wterm in production; multi-tab workspace; durable session recording UI; backend VT observer / `libghostty-vt` snapshot; profile-specific terminal preferences; live hot-reload of font / theme on a mounted xterm; mobile / Tauri keyboard UI; custom keybinding editor; OSC 52 clipboard automation; password bootstrap / `ssh-copy-id`; private-key import UI; real auth UI. Each is a separate slice. (Bracketed-paste confirmation / multiline-paste preview is now wired — see "Production terminal paste safety" below.)

### Production terminal paste safety

A frontend-only paste-safety policy that intercepts paste-candidate input at the renderer-input boundary and gates risky pastes behind explicit operator confirmation. The slice adds NO backend changes, NO new wire messages, NO renderer-interface additions, and NO durable storage of paste content. It is purely a UI gate between `XtermRenderer.onInput` and `client.sendInput`.

**Scope (load-bearing — this slice).**

1. **Pure paste policy module** — `apps/web/src/lib/app/terminal/pastePolicy.ts`. Exports the closed types `PasteRisk` (`safe` | `confirm` | `blocked`) and `PasteReasonCode` (`ok_empty` | `ok_keystroke` | `ok_single_line` | `multiline` | `large_payload` | `control_chars` | `bracketed_paste_markers` | `nul_byte` | `exceeds_hard_cap`); the public surface `isLikelyKeystroke`, `decidePaste`, `evaluatePaste`, `describePasteDecision`, `pasteByteLength`, `pasteLineCount`; and the threshold constants `KEYSTROKE_MAX_LENGTH = 8`, `PASTE_CONFIRM_BYTES = 4096` (4 KiB), `PASTE_HARD_CAP_BYTES = 65536` (64 KiB). The module is renderer-neutral (no `xterm` / `terminal-core` imports) and does no I/O. Tests live in `apps/web/tests/pastePolicy.test.ts`.
2. **`PasteDecision` shape (load-bearing redaction).** `{ risk, reasonCode, lineCount, byteLength, hasControlChars, hasBracketedPasteMarkers, safeUserMessage }`. The decision is METADATA only — it NEVER carries the original paste text or any fragment of it. `safeUserMessage` is a static string keyed off `reasonCode` (the `describePasteDecision` table). The decision is safe to put in Svelte state, JSON-stringify, or render to the DOM. Sentinel-string tests pin that no field of the decision (and no JSON / String form of it) contains a sentinel even when the input string carries one.
3. **Risk classification rules** (in `decidePaste` evaluation order):
   1. NUL byte present → `blocked`, reason `nul_byte`.
   2. `byteLength > PASTE_HARD_CAP_BYTES` → `blocked`, reason `exceeds_hard_cap`.
   3. Bracketed-paste markers (`ESC[200~` / `ESC[201~`) present in the text → `confirm`, reason `bracketed_paste_markers`.
   4. `lineCount > 1` → `confirm`, reason `multiline`.
   5. Risky control chars (any ASCII control < 0x20 except tab / LF / CR; or DEL 0x7f) → `confirm`, reason `control_chars`.
   6. `byteLength > PASTE_CONFIRM_BYTES` → `confirm`, reason `large_payload`.
   7. Otherwise → `safe`, reason `ok_single_line`.
4. **Keystroke short-circuit.** `isLikelyKeystroke(text)` returns `true` for empty strings, single characters (covers `\r` Enter, ESC, every printable), and 2..`KEYSTROKE_MAX_LENGTH`-char strings with no `\r` / `\n`. `evaluatePaste` applies this short-circuit FIRST and returns a `safe` decision with `reasonCode = "ok_keystroke"` for keystroke-likely input — bypassing the strict control-char check so arrow keys (`ESC[A`), function keys (`ESC OP`), and short IME commits do not trigger confirm UI. Empty strings get `reasonCode = "ok_empty"` for symmetry.
5. **ProductionTerminal integration** — `apps/web/src/lib/app/terminal/ProductionTerminal.svelte`. The `XtermRenderer.onInput` callback decodes the input to a string, calls `evaluatePaste`, and dispatches:
   - `safe` → forward immediately to `client.sendInput(text)` (the prior path).
   - `confirm` → hold the original text in a script-scoped `pendingPasteText` variable (NOT `$state`, NOT logged, NOT persisted), set `pendingPasteDecision` to the metadata-only decision, render the confirm panel.
   - `blocked` → drop the text, set `blockedPasteDecision` to the metadata-only decision, render the blocked panel.
6. **Confirm panel** (`production-terminal-paste-confirm`). Carries `data-paste-reason` plus a heading (the static `safeUserMessage`), a metadata line ("X line(s), Y byte(s)"), a static disclaimer ("This will send text directly to the remote shell. Review the source before continuing — RelayTerm does not inspect the paste content."), a "Send paste" button, and a "Cancel" button. The full pasted content is NEVER displayed; only the metadata. "Send paste" snapshots and immediately clears the `pendingPasteText` closure variable, then forwards the snapshot via `client.sendInput`. "Cancel" clears the closure variable and dismisses the panel.
7. **Blocked panel** (`production-terminal-paste-blocked`). Heading (the static `safeUserMessage`), metadata line ("Y byte(s) dropped. Nothing was sent to the remote shell."), and a "Dismiss" button. Same redaction posture as the confirm panel — no paste content reaches the DOM.
8. **Pending-paste teardown.** `teardownLocal` clears `pendingPasteText`, `pendingPasteDecision`, and `blockedPasteDecision` along with the client/renderer. This covers detach, dispose, reconnect (which calls teardown first), and `onDestroy`. A pending paste cannot survive a navigation away from the workspace.
9. **Wire-frame contract preserved.** `client.sendInput` is the only outbound surface. The Send-paste button calls it with the snapshotted text exactly once. The Cancel and Dismiss buttons NEVER call it. The blocked path NEVER calls it. The keystroke / safe-paste path is byte-identical to the prior implementation.

**No backend changes.** The slice is purely frontend. No new routes, no schema, no new wire messages, no protocol changes. The backend has no knowledge that paste safety happened — `client.sendInput` carries the same payload it always would, just gated on operator confirmation when risky.

**Architecture rule preserved.** The production shell still imports only `@relayterm/terminal-core` and `@relayterm/terminal-xterm`; the experimental adapters remain dev-lab-only. `appShellIsolation.test.ts` is unchanged. The `TerminalRenderer` interface in `terminal-core` is unchanged — paste interception lives entirely above the renderer at the `onInput → sendInput` boundary inside the production component.

**Redaction posture (load-bearing).**

- Paste content lives at exactly one place outside the input event: the script-scoped `pendingPasteText` variable on the workspace component, between `evaluatePaste` returning `confirm` and the operator's confirm/cancel click. It is NEVER assigned to `$state`, NEVER passed to `console.*`, NEVER serialized into a `data-*` attribute, NEVER persisted to localStorage / sessionStorage, NEVER routed through the audit-log surface, NEVER included in a thrown `Error.message`.
- The confirm panel and the blocked panel render METADATA only (line count, byte length, reason code, the static `safeUserMessage`). The full paste content is never displayed.
- `evaluatePaste` / `decidePaste` / `describePasteDecision` perform no I/O. They do not log, throw with payload bytes in the message, or persist anything. Sentinel tests in `pastePolicy.test.ts` pin that the decision object — across `safe` / `confirm` / `blocked` outcomes — never carries a sentinel string from its input through any field, JSON form, or String() form.

**Limitations and what this slice is NOT (load-bearing).**

- Frontend-only. The backend does not see the policy. A different client (or a future programmatic surface) that goes around the production component is not protected.
- No backend command inspection. RelayTerm does NOT parse or interpret the pasted text in any way. The policy is shape-based (newlines, size, control chars), not semantics-based. There is no allow-list, no deny-list, no shell-aware analysis.
- No durable terminal recording. The pending paste content is held only between `evaluatePaste` and the next confirm / cancel / teardown.
- No bracketed-paste-mode protocol handshake. The policy detects bracketed-paste markers in the text as a `confirm` signal; it does NOT track whether the remote shell enabled bracketed paste mode.
- No clipboard API access. The policy operates on whatever the renderer hands to `onInput`. The keystroke vs paste distinction is heuristic (length / newlines) rather than browser-paste-event-based; an explicit programmatic paste API (or OSC 52) is future work.
- No keybinding editor, no command palette, no global shortcut manager. Ctrl+Shift+V (or whatever the browser maps to paste) flows through xterm's `onData` and lands in the same `evaluatePaste` path as right-click paste; the policy's behaviour is paste-source-agnostic.
- The renderer-neutral `TerminalRenderer` interface is unchanged. A future adapter can plug into the same boundary without changes.

**Stable selectors (additions only).** `production-terminal-paste-confirm`, `production-terminal-paste-confirm-heading`, `production-terminal-paste-confirm-meta`, `production-terminal-paste-confirm-send`, `production-terminal-paste-confirm-cancel`, `production-terminal-paste-blocked`, `production-terminal-paste-blocked-heading`, `production-terminal-paste-blocked-meta`, `production-terminal-paste-blocked-dismiss`. The confirm/blocked panel containers also carry `data-paste-reason="<reasonCode>"` for smoke selectors to assert risk classification without depending on copy text.

**Future work (explicit out-of-scope for this slice).**

OSC 52 clipboard automation; right-click context menu with paste / paste-as-line / paste-without-newlines variants; programmatic paste API on the renderer adapter; bracketed-paste-mode protocol awareness (knowing when the remote shell enabled bracketed paste so the policy can present a richer UI); shell-aware command inspection / allow-list / deny-list; mobile-keyboard-specific paste UX; durable terminal recording / replay UI; backend-side paste inspection or audit; per-profile paste policy overrides; full keybinding editor; production renderer selector. Each is a separate slice.

### Production active terminal local recovery

After the production terminal launch UI shipped, an operator who navigated away from the Terminal view (or did a full-page reload) had no way to find their way back to a still-alive backend session: the AppShell's `activeLaunch` was an in-memory pointer, not a stored one. Detached sessions survive the bounded ~30-second TTL window on the backend, but the operator had to copy the session id by hand and reconnect from the Sessions list. This slice adds a **local-only browser convenience pointer** at the most-recent terminal session so the empty-state Terminal view can offer an explicit "Reconnect last session" affordance.
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
- Backend-restart recovery for live sessions. A restart drops the in-memory runtime registry and the live `russh::Channel` is unrecoverable. The orchestrator now runs a startup reconciliation pass at boot that transitions every `terminal_sessions` row in `starting`/`active`/`detached` to `closed` with a matching `closed { reason: "startup_reconciliation", previous_status, reconciled_at }` `session_events` row written in the same database transaction; for sessions with at least one durable chunk it also appends a single `terminal_recording_markers { kind: closed, seq: MAX(seq_end), payload: { reason: "startup_reconciliation", previous_status, reconciled_at } }` row in the same transaction so the replay viewer renders a clean terminator instead of a trailing chunk. The live PTY itself is not resurrected (recording captures display history, not the live shell). See "Durable terminal recording and replay architecture" for the full policy.
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

- **TTL duration**: default `relayterm_terminal::DETACHED_LIVE_PTY_TTL = 30s`, operator-tunable per-deployment via `terminal_sessions.detached_live_pty_ttl_seconds` (env `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`, bounded `5..=86_400`). Pinned in the manager crate; tests inject a sub-second value via `TerminalSessionManager::with_detach_ttl`. The orchestrator's live value (`TerminalSessionManager::detach_ttl()`) is exposed to authenticated callers via `GET /api/v1/config/session-policy` (`{ detached_live_pty_ttl_seconds: u64, max_live_pty_sessions_per_user: u32, max_starting_sessions_per_user: u32 }`, `AuthenticatedUser`-only, no CSRF — idempotent read) so the SPA can render honest UX copy without hardcoding the legacy `~30s` literal. There is still no per-session override on the wire — the value is per-deployment. The `max_live_pty_sessions_per_user` field is the Phase 1B.1 per-user live-PTY ceiling (see `docs/session-quotas.md`); it backs the SPA's `429 too_many_sessions` refusal copy. The `max_starting_sessions_per_user` field is the Phase 1B.2a per-user starting-burst ceiling (defaults to `4`, bounded `1..=32`); it backs the SPA's `429 too_many_starting_sessions` refusal copy. The Phase 1B.2b deployment-wide ceiling (`terminal_sessions.max_live_pty_sessions_per_deployment`, env `RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT`, default `64`, bounded `1..=4096`) is enforced at the create-route boundary with wire code `429 too_many_sessions_deployment`, but is deliberately NOT exposed on this surface — operator-only, fingerprinting risk; the SPA renders STATIC (non-parameterised) refusal copy. See `docs/session-quotas.md` § 4.2 / § 5.4 / § 7.1.
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
- **TTL clarity**: an `~Ns remaining` countdown is shown only when the lab has observed a detach (server frame OR local disconnect-no-close). The text is ALWAYS labelled `approximate (local clock)` because the backend's exact REMAINING TTL is not on the wire — `describeTtlWindow` never claims server authority. The label flips to `TTL elapsed locally; reattach may 409 (server-truth, not local)` once the local clock crosses the deadline. The visible countdown clamps to a 1-second floor — `0s` would imply a server-confirmed close the lab cannot prove. The TTL constant in `liveTerminalState.ts` (`DETACHED_TTL_MS = 30_000`) is the dev lab's own copy of the SPEC-pinned default; the production SPA reads the configured BASE window from `GET /api/v1/config/session-policy` but the lab does not (it stays self-contained for the renderer-comparison surface). Neither path polls for the REMAINING TTL — that is still not on the wire.
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
