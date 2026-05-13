# SPEC.md

> Product specification for RelayTerm. Defines what the system does, who uses it, and the data + behavior contracts.
> AGENTS.md governs *how* code is written; SPEC.md governs *what* it should do.
>
> Keep this in sync with implementation via the `spec-sync` sub-agent (`/agents spec-sync`).
>
> SPEC.md is the index and holds the load-bearing invariants, data model, behavior contracts, inventory lifecycle policy, integration points, out-of-scope list, and open questions. Per-surface contract detail (terminal, auth, inventory, recording, web shell) lives in [`docs/spec/`](docs/spec/) — see [`docs/spec/README.md`](docs/spec/README.md) for the area index.

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
- **user_password** / **user_session** (auth tables) — see "Production authentication" below; full schema in [`docs/spec/auth.md`](docs/spec/auth.md). `user_passwords` carries one row per user with the Argon2id PHC string. `user_sessions` carries one row per active opaque session token, keyed by SHA-256 `token_hash`; the plaintext `session_token` is never stored.
- **terminal_recording_chunks** / **terminal_recording_markers** — append-only durable recording of PTY OUTPUT bytes plus metadata markers. Chunk `payload` is opaque renderer-neutral bytes; full schema and redaction rules in [`docs/spec/recording.md`](docs/spec/recording.md) and the binding [`docs/terminal-recording.md`](docs/terminal-recording.md).

### ER diagram

TODO: Mermaid ER diagram. Update when entities change.

## Surfaces

This index points to the per-surface long-form contract under [`docs/spec/`](docs/spec/). Drift between this index and the area docs is a spec bug — file a doc fix.

### SSH credential and trust surfaces — see [`docs/spec/auth.md`](docs/spec/auth.md)

- **Credential creation contract** — `POST /api/v1/ssh-identities` generates an Ed25519 keypair backend-side; response carries the public OpenSSH line + SHA-256 fingerprint, never `encrypted_private_key`, never plaintext PEM, never vault internals. Failure modes 400/401/503 (static body); plaintext private bytes only ever exist inside `VaultService::generate_ssh_identity` and are wiped before return.
- **Host-key preflight + known-host trust contract** — `POST /api/v1/server-profiles/:id/host-key-preflight` captures the host key during KEX and disconnects WITHOUT attempting authentication (no client signature is ever sent to an unverified peer). `POST /api/v1/server-profiles/:id/trust-host-key` requires the caller's `expected_fingerprint` to match the freshly-captured fingerprint AND refuses to silently re-trust a revoked entry. Wire-stable status set: `unknown`, `trusted`, `changed`. Idempotent. The original incident analysis is in [`docs/agent/encountered-lessons.md`](docs/agent/encountered-lessons.md) (2026-04-29).
- **Authenticated SSH credential check contract** — `POST /api/v1/server-profiles/:id/auth-check` confirms a configured `ssh_identity` actually authenticates against the target. Wire-stable status: `authentication_succeeded`, `authentication_failed`, `host_key_unknown`, `host_key_changed`, `connection_failed`. The check NEVER opens a PTY, runs a shell, or executes a command. Hard outer timeout (default 25s) and a process-wide concurrency semaphore (default 4) bound outbound network exposure.

### Terminal lifecycle, transport, renderers, and UI — see [`docs/spec/terminal.md`](docs/spec/terminal.md) (renderer adapter contracts in [`docs/spec/terminal-adapters.md`](docs/spec/terminal-adapters.md))

- **Terminal-session lifecycle** — `POST /api/v1/terminal-sessions` creates metadata in `starting`; the live PTY-bearing implementation transitions to `active` / `detached` / `closed`. Owner-scoped; foreign / missing ids collapse to a byte-identical 404. `409 conflict { entity: "host_key" }` if no trusted pin exists for the profile's host. Idempotent close.
- **Terminal WebSocket attach/detach contract** — `GET /api/v1/terminal-sessions/:id/ws` runs through `AuthenticatedUser`; control plane is JSON, hot terminal data plane is the binary `RTB1` envelope (`Output` server→client, `Input` client→server, ≤1 MiB payload). Wire-stable JSON message types and error codes append-never-renumber. Detached attachment cleanup writes `detached_at` even on abrupt socket exit.
- **Live SSH PTY bridge contract** — describes how the orchestrator binds `russh::Channel` to the wire, the `DETACHED_LIVE_PTY_TTL` reconnect window, the dev workbench launcher, and the renderer comparison diagnostic surface.
- **Output sequence + in-memory replay buffer contract** — every `Output` frame carries a monotonic per-session `seq` starting at 1. Bounded ring (default `max_frames = 1024` AND `max_bytes = 1 MiB`), FIFO eviction, the most recent frame is always retained. Replay handshake on attach: `replay_start` → buffered `output` → `replay_end`, or a single `replay_window_lost` if the bookmark predates the buffer.
- **Frontend `terminal-core` contract** — TS mirror of the wire protocol; renderer-neutral `TerminalRenderer` interface; `TerminalSessionClient` lifecycle state machine. Renderers are interchangeable adapters (xterm baseline + experimental ghostty-web / restty / wterm).
- **Renderer adapters** (full contracts in [`docs/spec/terminal-adapters.md`](docs/spec/terminal-adapters.md)) — four adapters, each documented with package layout, contract, neutrality re-affirmation, dev-lab UI, and production-bundle tree-shaking behavior. xterm.js is the production baseline; ghostty-web / restty / wterm are dev-only experimental and tree-shaken from production. Renderer-specific knobs go behind a local `<renderer>Only` escape hatch.
- **Production terminal UI** — launch UI; sessions list/status; terminal settings foundation; viewport controls; **paste safety** (shape-based policy: `safe` / `confirm` / `blocked`; metadata-only panels; tested with sentinel-string redaction); active-terminal local recovery via `(session_id, last_seen_seq)` `sessionStorage` pointer; status refresh and stale-session handling.

### Inventory views, dashboard, and lifecycle implementation — see [`docs/spec/inventory.md`](docs/spec/inventory.md)

- **Read-only inventory views** for hosts, SSH identities, server profiles; detail panels; client-side search + tag filters.
- **Setup-action UIs** — SSH identity generation; host & server-profile creation; host-key preflight & trust UI; SSH auth-check UI.
- **Dashboard summary + recent activity** — at-a-glance counts, an honest setup checklist, and the current-user audit feed (`actor_id = caller`, NULL-actor pre-auth events deliberately excluded).
- **Server profile disable / enable** — backend routes, lifecycle audit (`server_profile_created` / `server_profile_disabled` / `server_profile_enabled`), current-user audit-events read API, frontend UI. Implementation status, route shapes, and audit payload contracts live in the area doc; the normative policy stays inline below.

### Production web app shell — see [`docs/spec/web-shell.md`](docs/spec/web-shell.md)

- Sidebar / topbar / view chrome; `AppShell.svelte` view dispatch; `AppViewId` discriminator; URL-driven view routing helpers. Production shell components MUST NOT import from `lib/dev/` or any experimental renderer adapter; isolation is pinned by `tests/appShellIsolation.test.ts`.

### Durable terminal recording and replay — see [`docs/spec/recording.md`](docs/spec/recording.md) (and the binding [`docs/terminal-recording.md`](docs/terminal-recording.md))

- Recording is OFF by default and config-gated; production-mode boot refuses recording-enabled with no recording master key (the recording master key is SEPARATE from the SSH-identity vault master key). Output bytes only; input is NOT recorded in v1. Persisted format is renderer-neutral. Owner-scoped reads; chunk bytes cross the wire only as `data_b64`. Retention sweep (Stage A startup-only + Stage B periodic advisory-locked worker) emits `recording_purged` audit rows with `actor_id = NULL` and public-only payloads.

## Behavior contracts

- **Reconnect replay**: when a client reconnects with `(session_id, last_seen_seq)`, the backend MUST send all events with `seq > last_seen_seq` from the ring buffer in order, then resume live streaming. If `last_seen_seq` is older than the ring buffer's tail, the backend returns a `replay_window_lost` error and the client must request a full re-render or close the session. **Status:** the in-memory replay buffer is in place and the wire path is live. See [`docs/spec/terminal.md`](docs/spec/terminal.md) → "Output sequence + in-memory replay buffer contract" for the per-frame contract, the bounded buffer policy, and the explicit non-durability guarantees.
- **Renderer swap**: the user MAY change the active renderer for a session at any time. The new renderer subscribes from the current sequence number; no replay is required.
- **Session lifecycle**: a session enters `detached` immediately on client drop, NOT after a timeout. A `detached` session continues to receive PTY output and append to the ring buffer until the `DETACHED_LIVE_PTY_TTL` window expires or an explicit close arrives. Reconnect inside the window resumes via `last_seen_seq`. Audit log records every state transition. See [`docs/spec/terminal.md`](docs/spec/terminal.md) → "Detached-session TTL contract" for the full policy. The broader roadmap for "what *durable persistent sessions* should mean for RelayTerm" beyond the in-memory TTL window — including the staged plan toward durable display reconstruction (recording + VT snapshot) and host-side multiplexer-backed live-shell persistence — is the design doc [`docs/persistent-sessions.md`](docs/persistent-sessions.md). The current in-memory TTL model is the baseline of that roadmap; nothing in the roadmap weakens the invariants in this section.
- **Host-key change**: on `check_server_key` mismatch, the backend rejects the connection, logs an `audit_event`, and surfaces the mismatch to the user; it does NOT silently update the known_hosts entry. The preflight + trust-host-key endpoints (see [`docs/spec/auth.md`](docs/spec/auth.md) → "Host-key preflight + known-host trust contract") implement this for the pre-session probe; the same rule applies to live sessions once they land.
- **Key vault access**: the encrypted private key is decrypted only inside the SSH session task. Decrypted bytes never cross a boundary (no log, no IPC payload, no DB write).

## Inventory lifecycle and destructive-action policy

This section is normative. It defines the safe lifecycle for every inventory entity and the rules every destructive surface (delete, disable, archive, revoke) MUST follow. Drift from these rules is a spec bug, not an implementation freedom.

**Status today (load-bearing — read before adding any destructive surface).** The lifecycle moves wired today are:

- `POST /api/v1/terminal-sessions/:id/close` — terminal sessions reach the `closed` terminal state.
- `POST /api/v1/server-profiles/:id/disable` and `POST /api/v1/server-profiles/:id/enable` — Stamp / clear `server_profiles.disabled_at`. Disabled profiles refuse new launches, auth-check, host-key preflight / trust. Existing live sessions are unaffected. Each successful create / enabled→disabled / disabled→enabled transition appends one `audit_events` row (`server_profile_created` / `server_profile_disabled` / `server_profile_enabled`). Frontend UI for disable / enable has landed.
- `PATCH /api/v1/hosts/:id` and `DELETE /api/v1/hosts/:id` — owner-scoped partial update + hard-delete on hosts. DELETE refuses with `409 conflict { entity: "host", reason: "referenced" }` when **either** an owned `server_profiles` row **or** any `known_host_entries` row references the host (the route-level guard is the single short-circuit OR; see "Reference / integrity policy" below). Neither route emits an audit row today — the `host_*` audit kinds are deliberately absent from the `audit_events_kind_chk` schema constraint.
- `PATCH /api/v1/ssh-identities/:id` (rename only) and `DELETE /api/v1/ssh-identities/:id`. DELETE refuses with `409 conflict { entity: "ssh_identity", reason: "referenced" }` when any owned `server_profiles` row references the identity. On success a `ssh_identity_deleted` audit row is written BEFORE the row is removed; the row's `encrypted_private_key` column is hard-deleted with the row (the only allowed path to removal of vault-encrypted private-key bytes from durable storage). Rename emits no audit (no `ssh_identity_updated` kind in the schema).
- `PATCH /api/v1/server-profiles/:id` and `DELETE /api/v1/server-profiles/:id`. PATCH writes a `server_profile_updated` audit row on success. DELETE refuses with `409 conflict { entity: "server_profile", reason: "referenced" }` when any `terminal_sessions` row (live OR closed) references the profile, and writes a `server_profile_deleted` audit row BEFORE the DELETE on success. The recommended alternative for profiles with session history is the disable flow above.
- `known_host_entries.revoked_at` — column exists; no route or UI yet writes it. The trust route already refuses to silently re-trust a revoked fingerprint (two-layer guard: route check + `record_trusted` SQL `WHERE revoked_at IS NULL`).

Implementation detail (route shapes, payload contract, idempotency rules, fail-closed audit-failure policy, current-user audit-events read API, frontend UI surfaces) lives in [`docs/spec/inventory.md`](docs/spec/inventory.md). The inventory-management mutation routes were exercised end-to-end against staging on 2026-05-12 — see [`docs/deployment/vps-staging-smoke.md`](docs/deployment/vps-staging-smoke.md) "Inventory management mutations" entry. The smoke was API-driven (curl + cookie jar), not browser-driven; the SPA-side error formatters and parsers are covered by `apps/web/tests/inventoryMutationsApi.test.ts`. Real browser drive of the SPA mutation flows remains deferred.

`terminal_sessions` (outside `close`), `audit_events`, `session_events`, and `known_host_entries` have no destructive surface. The schema enforces FK `RESTRICT` on the load-bearing references; an attempt to delete a referenced row from the DB layer would already fail. The policy below is what every destructive route MUST satisfy — the routes above were built against it.

### Per-entity lifecycle states

| Entity | States today | Future states | Destructive surface today | FK to children |
|---|---|---|---|---|
| `users` | `active` (no other state) | none planned in v1 | none | `hosts` (CASCADE), `ssh_identities` (CASCADE), `server_profiles` (CASCADE), `terminal_sessions` (CASCADE), `audit_events.actor_id` (SET NULL) |
| `hosts` | `active` (no flag column) | `active` only — delete-when-zero-references | `PATCH /:id`, `DELETE /:id` (refuses 409 when an owned `server_profiles` row OR any `known_host_entries` row references the host) | `server_profiles.host_id` (RESTRICT), `known_host_entries.host_id` (CASCADE — schema-level; the route refuses delete before it can fire) |
| `ssh_identities` | `active` (no flag column) | `active` only — delete-when-zero-references | `PATCH /:id` (rename only), `DELETE /:id` (refuses 409 when an owned `server_profiles` row references the identity) | `server_profiles.ssh_identity_id` (RESTRICT) |
| `server_profiles` | `active` \| `disabled` (`disabled_at` column) | unchanged; delete refused while any `terminal_sessions` row references the profile | `POST /:id/disable`, `POST /:id/enable`, `PATCH /:id`, `DELETE /:id` (refuses 409 on any session reference) | `terminal_sessions.server_profile_id` (RESTRICT) |
| `known_host_entries` | `unknown` (no `trusted_at`), `trusted` (`trusted_at` set, `revoked_at IS NULL`), `revoked` (`revoked_at` set) | unchanged; explicit operator-only unrevoke much later | column-level `revoked_at` only — no route yet | none |
| `terminal_sessions` | `starting`, `active`, `detached`, `closed` (CHECK constraint) | unchanged | `POST /:id/close` — idempotent, terminal | `terminal_session_attachments.session_id` (CASCADE), `session_events.session_id` (CASCADE) |
| `terminal_session_attachments` | open (`detached_at IS NULL`), detached (`detached_at` set) | unchanged | row update on detach (manager-internal); never deleted via UI | none |
| `session_events`, `audit_events` | append-only | unchanged | none — immutable | none |

`users` deletion is intentionally out of scope for v1. The `ON DELETE CASCADE` shape exists for operator/test use only; no API surface accepts a user delete.

### Delete vs disable / archive policy

1. **Default user-facing destructive action for `server_profiles` is `disable`, not delete.** Disable blocks NEW launches; existing live sessions keep running until they close on their own (operator close, remote shell exit, or PTY teardown). A re-enable returns the profile to launchable.
2. **`hosts` and `ssh_identities` are deletable only when zero `server_profiles` reference them.** This matches the schema's FK `RESTRICT`. The route classifies the refusal at the application layer — a clean 409 BEFORE attempting the DELETE — so the client gets a typed error (`409 conflict { entity: "host" | "ssh_identity", reason: "referenced" }`) instead of a generic constraint violation. Production UI MUST surface "remove the N referencing profiles first" rather than "try again." (For `hosts`, the same 409 also fires when any `known_host_entries` row references the host — see "Reference / integrity policy" below.)
3. **`server_profiles` are deletable only when zero `terminal_sessions` reference them** (live OR closed; closed-session metadata is historical and protective). The implemented `DELETE /api/v1/server-profiles/:id` route does NOT require the profile to be `disabled` first — disable remains the *recommended* user-facing destructive action and the UI should still steer the operator there for profiles with history, but the backend does not enforce it. If hard-delete on a referenced profile is ever needed, it is admin-only, not a user-facing action.
4. **`terminal_sessions` are NEVER deleted from the user UI.** Once `closed`, they are historical metadata. The user lists, views, and audits them. Any cascade or sweep that drops session rows is admin-only, future-only, and explicit. Inventory deletion (host/identity/profile) MUST NOT cascade-delete sessions — `RESTRICT` is the policy and the schema agrees.
5. **`known_host_entries` are revoked, never hard-deleted from user UI.** Hard delete is admin-only future work. Revoke is non-recoverable from the user surface; an explicit operator unrevoke flow may land later as a separate, deliberate slice (see `docs/agent/encountered-lessons.md` 2026-04-29).
6. **`session_events` and `audit_events` are never deleted from any surface.** They are append-only forensic logs; an admin retention sweep is future work and out of scope for v1.

### Reference / integrity policy

- **Host delete**: requires `0` owned `server_profiles` AND `0` `known_host_entries` referencing the host. The route-layer `any_dependents_for_user` predicate is a single short-circuit OR across both refs and returns the same `409 conflict { entity: "host", reason: "referenced" }` envelope on either branch. The schema FK `known_host_entries.host_id ON DELETE CASCADE` exists but is intentionally unreachable from the user-facing surface: refusing the host delete preserves pinned-trust history as a deliberate user-facing safety property (see "Things to avoid" in AGENTS.md). Hard delete of a host with pinned-trust history is admin-only future work — the production UI must surface "revoke or remove the N pinned host keys first" rather than cascading them.
- **SSH identity delete**: requires `0` owned `server_profiles` referencing the identity. The encrypted private-key bytes are wiped at the DB layer when the row is removed; no copy of `encrypted_private_key` exists outside the row (vault decrypts only into ephemeral memory in the SSH session / preflight task and zeroizes on drop). A `ssh_identity_deleted` audit row is written BEFORE the DELETE so the audit row exists even if the DELETE later fails.
- **Server profile disable**: no reference check needed. Existing live `terminal_sessions` are unaffected. The launch route refuses to start a new session against a disabled profile with `409 conflict { entity: "server_profile", reason: "disabled" }`.
- **Server profile delete**: requires `0` `terminal_sessions` referencing the profile (any status). The route does NOT enforce a `disabled` precondition — disable is the *recommended* user-facing flow (preserves history and audit; blocks future launches) but is not a delete prerequisite. Two-layer policy: the route emits a clean 409 BEFORE attempting DELETE; the schema's `RESTRICT` is the second-line backstop.
- **Typed-409 entity field convention**: the wire `entity` value on a `409 conflict` envelope uses the singular table-row form (`host`, `ssh_identity`, `server_profile`, `terminal_session`, `host_key`). Wire message shape: `ApiError::Conflict` carries `entity` plus an optional `reason`; when `reason` is `Some(r)` the message is `"{entity} {r}"` (the inventory-management routes emit `host referenced` / `ssh_identity referenced` / `server_profile referenced`; the profile-disable launch guard emits `server_profile disabled`); when `reason` is `None` the message keeps the older `"{entity} conflict"` form (host-key-trust emits `host_key conflict`; terminal-session-create emits `terminal_session conflict`). New destructive routes MUST set an explicit `reason` so client error handling stays uniform.
- **Active session at the moment of profile disable**: the live session continues. Disable is a launch-time gate, not a runtime kill switch. Operator-driven session kill remains `POST /api/v1/terminal-sessions/:id/close`.
- **`audit_events.actor_id` orphans to `NULL`** when a user is deleted (schema `ON DELETE SET NULL`). Audit history survives user deletion, with the actor anonymised. This is the only inventory action that nullifies a reference; everything else uses `RESTRICT` or `CASCADE` deliberately.

### Session-history policy

- A `closed` `terminal_session` row is a permanent historical record. Users can list and view it but cannot delete it.
- The row's `server_profile_id` and `owner_id` references must remain stable for the row's lifetime. This is why the schema uses `RESTRICT` on `server_profile_id` and `CASCADE` on `owner_id` — the row dies only with its owner.
- When a profile is disabled or deleted, historical session rows that reference it stay readable. The list UI MUST handle a session whose underlying profile is gone (post-delete) without crashing — render a stable session id, status, timestamps, and a "(profile removed)" placeholder for the profile name.
- `terminal_session_attachments` and `session_events` cascade-delete with their session row. This is correct: they are per-session telemetry and have no meaning detached from the session. They are NOT exposed as their own deletable surface.

### Known-host revocation policy

- The state machine is `unknown → trusted → revoked` (with `unknown` returning to itself if the operator never confirms). `revoked` is reachable only via a deliberate operator action; the production UI does NOT surface revoke today.
- A revoked entry is **never silently re-trusted**. The trust route enforces this with two layers (route guard + `record_trusted` SQL), and the classifier filters revoked rows out of the `trusted` / `changed` classification (a revoked-and-reappearing key surfaces as `unknown`, not `trusted`). See `docs/agent/encountered-lessons.md` 2026-04-29 for the original incident analysis.
- Recovery from `revoked` is an explicit operator workflow that does NOT exist in v1. A future "unrevoke" route MUST be admin-only, audit-logged, and require an explicit fingerprint match — no convenience UX that lets an operator click through revocation.
- `known_host_entries.host_id ON DELETE CASCADE` exists at the schema level, but the host-delete route refuses to fire it: any `known_host_entries` row for the host trips the same `409 host referenced` envelope as a referencing `server_profiles` row. Pins are retained as forensic / trust history; removing them is a deliberate operator action, not an implicit side effect of removing the host. AGENTS.md "Things to avoid" carries the matching one-liner ("do not hard-delete `known_host_entries`"); the route's `any_dependents_for_user` predicate is the enforcement point.
- Hard delete of a known-host entry without deleting its host is admin-only future work. Revoke (`revoked_at` stamp) is the user-facing trust-removal surface and remains unimplemented today.

### Audit-event expectations

The `audit_events.kind` enum (mirrored by the `audit_events_kind_chk` CHECK constraint) carries `server_profile_created`, `server_profile_updated`, `server_profile_disabled`, `server_profile_enabled`, `server_profile_deleted`, `ssh_identity_created`, `ssh_identity_deleted`, `host_key_accepted`, `host_key_mismatch`, and `host_key_revoked`. New destructive routes MUST extend the enum (with a paired migration to the `audit_events_kind_chk` CHECK and the `AuditEventKind` Rust enum) when they introduce a new lifecycle action.

Kinds wired today:

- `server_profile_created` (POST), `server_profile_updated` (PATCH), `server_profile_disabled` / `server_profile_enabled` (POST disable/enable), `server_profile_deleted` (DELETE).
- `ssh_identity_deleted` (DELETE).

See [`docs/spec/inventory.md`](docs/spec/inventory.md) → "Server profile lifecycle audit" for the payload contract, idempotency rules, and fail-closed failure policy. The `ssh_identity_deleted` payload contract is documented in the same area doc.

The currently-missing kinds are:

- `host_created`, `host_updated`, `host_deleted` — no `host_*` variant exists in the schema. The host create / update / delete routes are wired but deliberately do NOT audit; adding host_* kinds is a separate slice (would require a CHECK-constraint migration). Inventory-metadata mutations on hosts are considered low-risk relative to the security-critical surfaces that already audit (server-profile lifecycle, SSH-identity delete, host-key trust/revoke).
- `ssh_identity_updated` — also intentionally absent from the schema. SSH-identity rename is inventory metadata; no audit row is written.
- (`host_key_revoked` already exists; reuse it for the revoke route when it lands.)

The auth-related kinds `login_succeeded`, `login_failed`, and `logout_succeeded` ARE already present in `audit_events_kind_chk` (per the original `20260428000009_audit_events.sql` migration) but no route emits them today. The forthcoming auth slice MUST NOT add a duplicate migration for these names. The auth slice DOES add new kinds (`first_user_created`, `password_changed`, `session_revoked`, `sessions_revoked`) — see [`docs/spec/auth.md`](docs/spec/auth.md) → "Audit events" for the full list and the paired migration requirement.

Rules every destructive lifecycle action MUST follow:

1. **Successful destructive action writes exactly one audit event** with `actor_id = caller`, an appropriate `kind`, and a payload containing the target id and target kind. The `target_id` field on the payload is required so cross-entity audit queries are tractable.
2. **Failed destructive attempts that are security-relevant SHOULD audit.** A revoke-then-trust attempt, a cross-user delete (which already collapses to a 404 to avoid existence leak), and a delete refused for FK reasons in a context that suggests probing (large burst, repeated unknowns) are candidates. Routine 409s (delete blocked by visible references in the caller's own inventory) MAY skip audit to keep the log signal-rich.
3. **Audit payloads contain public metadata only.** Allowed: target id, target kind, caller id, fingerprints (public), `key_type`, `name`, timestamps, reference counts (e.g. `referencing_profile_count`), reason codes. **Forbidden:** `encrypted_private_key`, plaintext private-key bytes, raw russh error text, peer banners, vault internals (master key, nonce, version byte), terminal I/O (input keystrokes, output bytes), full URLs with query strings that could carry secrets, the `client_info` blob from `terminal_session_attachments` (operator-supplied User-Agent — reference attachments by `id` only).
4. **For `ssh_identity_deleted`** the payload MAY retain `name`, `key_type`, `fingerprint_sha256`, and `created_at` so the audit row remains readable after the underlying identity row is gone. The `encrypted_private_key` bytes are NEVER copied into audit.
5. **`host_deleted` is not currently emitted** — the host DELETE route writes no audit. If a future slice adds the `host_*` audit kinds, the `host_deleted` payload MAY retain `display_name`, `hostname`, `port`, `default_username`, and counts of any cascaded telemetry. Today no cascade fires from the user-facing host delete (the route refuses on `known_host_entries` references), so a future `host_deleted` payload would not need a `cascaded_known_hosts_count` field unless admin tooling later introduces a cascading admin-only path.
6. **For `server_profile_disabled` / `server_profile_enabled`** the payload includes `target_id` and the new state. No reason field is required in v1; an optional operator-supplied note is future work.
7. **`session_events` are not a substitute for `audit_events`.** Session events are per-session lifecycle telemetry and stay scoped to that session row. Audit events span the system and survive cascade-delete of session telemetry.

### UI implications

- User-facing destructive / mutation UI today: server-profile disable / enable (Servers view; see [`docs/spec/inventory.md`](docs/spec/inventory.md) → "Server profile disable / enable UI (landed)") and SSH-identity rename + delete (Identities view; wired to `PATCH /api/v1/ssh-identities/:id` and `DELETE /api/v1/ssh-identities/:id`). Inventory-management API helpers for host edit / delete and server-profile edit / delete (`apps/web/src/lib/api/hosts.ts`, `apps/web/src/lib/api/serverProfiles.ts`) are landed with typed error formatters and unit tests but are not yet called from any production view — the Servers view still renders hosts and server-profiles read-only for everything outside disable / enable. The remaining inventory entities (`known_host_entries` revoke, hard-delete) have neither API nor UI surface.
- Future destructive UI MUST be **explicit, confirmable, and auditable**. A confirmation dialog is required for every destructive action; the confirmation MUST name the target (display name + id suffix), the action verb, and the consequence ("this profile will stop accepting new launches; existing live sessions are unaffected").
- Confirmation dialogs and audit views render **public metadata only**. The redaction rule from `lib/api/` parsers extends here — no `private_key` / `encrypted_private_key` field appears in any DOM string, formatted preview, or copy string. The existing sentinel-string redaction tests in the SSH-identity views are the pattern to follow.
- Routing rule (already established): no secret material, terminal data, or session payloads in URLs. This applies to confirmation dialog hashes too — destructive confirmation goes through component state, not URL params.
- Disabled `server_profiles` render with a clear `disabled` badge in the inventory list and detail panel. The Launch button is rendered disabled with an honest tooltip ("this profile is disabled; enable it to launch a new terminal"). The dashboard checklist's `launch-terminal` row stays count-inferable — disabled profiles do NOT change the count semantics.
- Closed terminal sessions remain visible and read-only in the sessions list. The list MUST handle a session whose `server_profile_id` no longer resolves (post-delete) without crashing — render a stable session id, status, timestamps, and a "(profile removed)" placeholder.
- A session whose underlying profile is `disabled` (but still resolvable) renders the profile name with a `(disabled)` suffix in the sessions list and detail panel. The session itself is unaffected by disable — `active`/`detached` sessions keep streaming and the operator may still close them — so the UI signals the disabled-profile context without implying the session has stopped. Re-enabling the profile clears the suffix on the next refresh.

### Implementation status and order

Current implementation status (server-profile disable/enable backend + audit emission + read API + frontend UI; inventory-management PATCH/DELETE routes for hosts / server-profiles / SSH identities API-smoked against staging on 2026-05-12; SSH-identity rename + delete wired into the Identities view) and the staged plan for the remaining destructive surfaces (host / server-profile edit-delete UI wiring, known-host revoke route + UI, stale-row sweepers, admin tooling, operator unrevoke + admin hard-delete) are tracked in [`docs/spec/inventory.md`](docs/spec/inventory.md) → "Future implementation order". Each numbered step inherits the standard "definition of done" checklist (tests, sqlx prepare on schema change, audit event reachable, owner-scoping, redaction posture).

## Production authentication

> Full normative contract: [`docs/spec/auth.md`](docs/spec/auth.md). Operator deployment guide: [`docs/production-auth.md`](docs/production-auth.md). Smoke procedure: [`docs/auth-smoke.md`](docs/auth-smoke.md). Worked config templates: [`docs/config-examples/relayterm.dev.example.toml`](docs/config-examples/relayterm.dev.example.toml) and [`docs/config-examples/relayterm.production.example.toml`](docs/config-examples/relayterm.production.example.toml).

The summary below is load-bearing: any change must keep all of these invariants true. Drift goes through `docs/spec/auth.md` first, never through code.

- **Auth mode model.** `auth.mode = production` and `auth.mode = dev` run the same real-auth code path. Mode selects only the boot-time validation envelope: production REQUIRES a configured session signing key source, non-empty `auth.allowed_origins`, and `auth.cookie_secure = true`; dev relaxes all three. There is no runtime "skip auth" branch — the legacy `DevUser` extractor and `dev@relayterm.local` fixture user are gone.
- **Opaque server-side sessions.** The wire never carries a JWT. Every authenticated request is gated by an opaque random `session_token` (32 bytes, URL-safe-no-pad base64 → 43 ASCII chars) bound to a `user_sessions` row. The plaintext `session_token` is generated once at session creation, written to a `Set-Cookie` header, and dropped. Storage and lookup are by SHA-256 `token_hash` only — `user_sessions.token_hash` is the load-bearing column, and the plaintext is treated like vault private-key bytes (visible on exactly one wire surface, never persisted, never logged).
- **Cookie-backed browser auth.** The cookie is `relayterm_session`, `HttpOnly; SameSite=Strict; Secure` (in production), with optional `Domain=` for subdomain sharing. Default expiry is `created_at + 30 days` hard-expire. Logout / revoke writes a `session_revoked` audit row and removes the row from `user_sessions`.
- **CSRF / `Origin` guard for browser writes.** Every state-changing browser-write route runs the shared `CsrfGuard` extractor (`relayterm_api::CsrfGuard`) BEFORE any DB / auth / body work. Wire policy is `403 csrf_origin_mismatch`; `GET`s are exempt. Allowed origins are a byte-equality allow-list configured per-deployment. The handler MUST NEVER echo the offered `Origin` value in the wire body OR the operator-side `warn!` line.
- **First-user bootstrap.** Production refuses to start with no row in `user_passwords` AND no `auth.first_user_bootstrap_token`. With both: `POST /api/v1/auth/bootstrap` accepts the bootstrap token + email + password, creates the first user, hashes the password with Argon2id at `OWASP_2023` parameters (`m=19456`, `t=2`, `p=1`), writes a `first_user_created` audit row, and the operator unsets the bootstrap token on next deploy. There is no second-user bootstrap path; admin / multi-user is out of scope for v1.
- **Password authentication.** `POST /api/v1/auth/login` runs through CSRF guard + payload validation + the in-memory `LoginThrottler` (5 failures / 15-minute window → 15-minute block, keyed on `normalize_login_identifier(&email)` only — no `Retry-After`, no IP keying yet) BEFORE the user lookup. Both unknown-email AND wrong-password branches `record_failure`. Success records `record_success`, mints a session, writes a `login_succeeded` audit row.
- **Password and session management surfaces.**
  - `POST /api/v1/auth/change-password` — verifies current password, persists a fresh Argon2id PHC hash, revokes every OTHER session for the caller (the current cookie stays valid), emits `password_changed` audit (payload carries `revoked_other_sessions: u64` only — never the plaintext password, never any hash, never any cookie value).
  - `GET /api/v1/auth/sessions` — current-user-scoped at the SQL layer; returns only safe metadata (`id`, `created_at`, `last_seen_at`, `expires_at`, optional truncated `user_agent`). Never carries `session_token`, `token_hash`, or `client_info` blobs.
  - `POST /api/v1/auth/sessions/:id/revoke` and `POST /api/v1/auth/sessions/revoke-all-except-current` — owner-scoped; emit `session_revoked` / `sessions_revoked` audit rows; the second route deliberately keeps the caller's current session alive.
  - `last_seen_at` is stamped inline on every successful `AuthenticatedUser` extraction; failure logs `warn!` with the session id only — never the cookie value, the `session_token`, the `token_hash`, the password hash, or repository internals.
- **Redaction boundary (security-critical).** The plaintext `session_token`, the `token_hash`, the password (clear and PHC-hashed), and the `relayterm_session` cookie value MUST never appear in:
  - `tracing::*` lines (any level — including `warn!` / `error!`).
  - `audit_events.payload` rows.
  - `Display` / `Serialize` impls of session / password types.
  - Thrown `Error.message` / panic strings / any HTTP response body.
  - `data-*` attributes / frontend `localStorage` / `sessionStorage` / any DOM string.
  - The current-user audit feed (`recent_for_actor`) — pre-auth events with `actor_id IS NULL` are ALSO excluded by SQL filter so a probe pattern cannot leak via the user-facing feed.
  - Sentinel-string tests in `crates/relayterm-api/tests/api.rs` (the `AUDIT_FORBIDDEN_SUBSTRINGS` helper) are the redaction backstop on every emission path.
- **Frontend auth UI plan.** Login / logout / password-change / session-management panels live in the Settings view; no auth UI shows the plaintext `session_token`, `token_hash`, or any password-shaped field. Errors collapse through `describeLoadError` so transport / operator detail cannot leak into the rendered string.
- **Forward compatibility.** The session shape carries an enum-tagged credential type so passkeys / WebAuthn can land later without a session-shape migration. Passkeys, password reset, IP-aware throttling, and admin / cross-user session views are deliberately out of scope for v1.

The full per-route contract, audit-payload schema, security-properties test list, and implementation order live in [`docs/spec/auth.md`](docs/spec/auth.md). The boot-time validator (`Config::validate_auth` in `apps/backend/src/config.rs`) is the executable contract for the validation envelope; the integration tests in `crates/relayterm-api/tests/api.rs` are the executable contract for the runtime surface.

## Integration points

- **PostgreSQL** — primary store for users, sessions, audit log, key vault. sqlx connection pool; `runtime-tokio-rustls`.
- **Vault master key** — 32-byte secret loaded once at boot, supplied via `vault.master_key_b64` (config / `RELAYTERM_VAULT__MASTER_KEY_B64` env) or `vault.master_key_file`. Exactly one source must resolve, or the backend refuses to start. There is no fallback to a randomly generated key — that would orphan all previously stored ciphertext after a restart. Setting `vault.enabled = false` disables backend-generated identities (the POST route returns 503) and lets the rest of the API run.
- **Traefik** — reverse proxy in front of the backend; terminates TLS; routes `/api/*` and `/ws/*`. Self-hosted Compose deployment shape lives in [`docs/deployment/docker-compose.md`](docs/deployment/docker-compose.md) and the operator runbook lives in [`docs/deployment/production-runbook.md`](docs/deployment/production-runbook.md).
- **WireGuard** (optional) — used only when the backend lives on a remote box and SSH targets are reachable only via the WireGuard mesh.
- **Object storage** — TODO if the project ever needs file upload/download via SCP/SFTP. Out of scope for v1.
- **Passkeys / WebAuthn** — deferred. v1 ships password-only authentication with opaque server-side sessions in Postgres bound to an `HttpOnly; Secure; SameSite=Strict` cookie. Real cookie-backed auth runs in both `auth.mode = dev` and `auth.mode = production`; see [`docs/spec/auth.md`](docs/spec/auth.md) for the validation envelope each mode enforces and the forward-compatible session shape that lets passkeys land later without a session-shape change.
- **Tauri shells (desktop + mobile)** — separate release tracks under `apps/desktop/` and `apps/mobile/`. Both shells consume the built `apps/web` SPA. v1 has no automated build/release pipeline for either shell; iOS is explicitly later than Android. The runtime-backend-URL design for built (non-dev) shells is closed in [`docs/spec/tauri-runtime-backend-url.md`](docs/spec/tauri-runtime-backend-url.md) (path A — remote web shell — recommended; path B — cross-origin bundled SPA — deferred because it would weaken the same-site cookie / `CsrfGuard` posture). See "Out of scope (v1)" below for the full list of deferred Tauri / mobile work.

## Out of scope (v1)

TODO — explicit list of features deferred so the agent doesn't "helpfully" implement them. Likely:

- SCP/SFTP file transfer surface.
- Multi-user shared sessions / "screen-share."
- Public-cloud-hosted multi-tenant deployment (v1 is single-tenant Docker Compose).
- iOS Tauri build (Android first; iOS later). The Tauri desktop and mobile shells (`apps/desktop/`, `apps/mobile/`) ship with no automated CI/build pipeline yet. (Staged plan: [`docs/deployment/tauri-ci-release-plan.md`](docs/deployment/tauri-ci-release-plan.md).)
- libghostty-vt state engine swap (planned; xterm.js drives the baseline). The xterm.js baseline adapter (`@relayterm/terminal-xterm`) and the experimental ghostty-web (`@relayterm/terminal-ghostty-web`), restty (`@relayterm/terminal-restty`), and wterm (`@relayterm/terminal-wterm`) adapters have all landed under `packages/terminal-<name>/`.
- Multi-user / team authentication, role-based access control, and an admin / operator surface. v1 is single-user self-hosted; see [`docs/spec/auth.md`](docs/spec/auth.md) for the rationale.
- Email-based password reset / "forgot password" flow. Self-hosted operators have DB-level recovery in v1; mail transport is its own scope.
- Passkey / WebAuthn registration and authentication. Forward-compatible with the v1 session shape; deliberately deferred.
- Admin / cross-user session view (`/auth/sessions` is current-user only by design). The current-user Settings session-management UI has now landed — see [`docs/spec/auth.md`](docs/spec/auth.md) → "Implementation order".
- Kubernetes / Helm / Nomad, multi-node HA, zero-downtime rolling deploys, image signing, SBOM / vulnerability scanning, registry retention automation, multi-arch images, managed-secrets integrations, or backup automation — see the [`docs/deployment/production-runbook.md`](docs/deployment/production-runbook.md) deferred-work ledger.

## Open questions

TODO — known ambiguities for the owner to resolve. Each: question, options considered, current default if any.

- Replay buffer policy: fixed bytes vs fixed events vs time-window? Default: TODO. (Live default: `max_frames = 1024` AND `max_bytes = 1 MiB`; see [`docs/spec/terminal.md`](docs/spec/terminal.md) → "Output sequence + in-memory replay buffer contract".)
- How long does a `detached` session linger before auto-close? **Default**: `relayterm_terminal::DETACHED_LIVE_PTY_TTL = 30s`. Operator-tunable per-deployment via `terminal_sessions.detached_live_pty_ttl_seconds` (env `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`, bounded `5..=86_400`); the SPA reads the effective value via `GET /api/v1/config/session-policy` (`{ detached_live_pty_ttl_seconds: u64 }`, `AuthenticatedUser`-only, no CSRF) so production UX copy never hardcodes the legacy literal. In-memory only (lost on backend restart). See [`docs/spec/terminal.md`](docs/spec/terminal.md) → "Detached-session TTL contract" for the full lifecycle.
- Should the renderer choice be per-session or per-device? Default: per-device.
- Session expiry policy: hard-expire vs sliding-window? **Default (v1)**: `hard_expire` at `created_at + 30 days`. Reconsider only if UX demands it; sliding-window introduces a re-issue race for marginal benefit. See [`docs/spec/auth.md`](docs/spec/auth.md) → "Session model".
- Login throttle thresholds: bucket size and refill rate per `(remote_addr, email)` and per `email`. **Resolved (v1)**: `LoginThrottleConfig::V1_DEFAULT` = 5 failures / 15-minute sliding window → 15-minute block, keyed on the **normalized email only** (IP-aware keying deferred until `ConnectInfo` is plumbed). Constants live in code (`crates/relayterm-auth/src/throttle.rs`); a config knob is deliberately not added in v1 — the policy is single-tenant defensible and any tuning is a deploy redeploy. The test rig drives a tight bucket via per-test `Arc<LoginThrottler>` injection on `AppState`.

---
