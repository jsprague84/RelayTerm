# Session quota policy — design

> **Status (2026-05-11):** design draft, partially landed.
> Phase 1B of the durable persistent-sessions roadmap
> ([`docs/persistent-sessions.md`](persistent-sessions.md) § 8 Phase 1).
> This document defines the quota and limit model that bounds RelayTerm's
> in-memory detached-live-PTY sessions.
>
> **Implementation status:**
>
> - **Phase 1B.1 (landed 2026-05-11; staging-verified 2026-05-11):**
>   `max_live_pty_sessions_per_user` — per-user live PTY ceiling,
>   default `8`, bound `1..=256`. Wire shape: 429
>   `too_many_sessions`. Exposed via
>   `GET /api/v1/config/session-policy` as
>   `max_live_pty_sessions_per_user`. SPA renders parameterised
>   refusal copy via `describeMaxLivePtyPerUser`. No new audit kind,
>   no DB row, no `Retry-After`. Enforcement order: after ownership
>   + host-key gates, before vault decrypt / SSH side effects.
>   End-to-end smoke against the HTTPS staging slot (cap temporarily
>   lowered to `1`) confirmed the 429 envelope, no DB row, no audit
>   row, safe operator warn line, and slot-freeing on close — see
>   [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
>   § 12 "Per-user live PTY quota (Phase 1B.1, cap=1) staging
>   smoke".
> - **Phase 1B.2a (landed 2026-05-11):**
>   `max_starting_sessions_per_user` — per-user starting-burst
>   ceiling, default `4`, bound `1..=32`. Wire shape: 429
>   `too_many_starting_sessions`. Exposed via
>   `GET /api/v1/config/session-policy` as
>   `max_starting_sessions_per_user`. SPA renders parameterised
>   refusal copy via `describeMaxStartingPerUser`. Counts the
>   disjoint set of `Starting` placeholders that have NOT yet bound
>   a live PTY (so the live and starting quotas never double-count).
>   Same redaction posture as 1B.1 — no new audit kind, no DB row,
>   no `Retry-After`. Enforcement order: same as 1B.1, immediately
>   after the live-cap check.
> - **Phase 1B.2 (NOT landed):** deployment-wide quota
>   (`max_live_pty_sessions_per_deployment`), operator dashboard
>   tile.
> - **Phase 1B.3 (NOT landed):** production-default tuning.
>
> **Related normative documents:**
>
> - [`SPEC.md`](../SPEC.md) — architectural invariants, behaviour
>   contracts, inventory-lifecycle and destructive-action policy.
> - [`docs/spec/terminal.md`](spec/terminal.md) → "Detached-session TTL
>   contract", "Output sequence + in-memory replay buffer contract" —
>   current TTL and replay model.
> - [`docs/persistent-sessions.md`](persistent-sessions.md) § 8 Phase 1
>   — staged plan that this slice fits into.
> - [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) §§ 1, 4,
>   7, 8, 9 — redaction backstops referenced by the quota error /
>   logging policy below.
> - [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
>   → 2026-05-10 Long-TTL reconnect smoke — empirical baseline for the
>   reaper / TTL behaviour quotas interact with.

## 1. Current behaviour and why quotas are needed

Every live RelayTerm terminal session today is a TUPLE of resources held
on the running backend:

- one `russh::Channel` to the target host
- one allocated PTY on the target host
- one bounded broadcast + replay ring buffer
  ([`relayterm_terminal::manager::LiveRuntime`](../crates/relayterm-terminal/src/manager.rs))
- one TTL close task (when `detached`)
- one or more `terminal_session_attachments` rows
  while a WebSocket is attached
- (optional) one recording chunk-writer task

The orchestrator's in-memory registry
(`TerminalSessionManager::runtimes`) is the authoritative tracker of
live PTYs. There is no upper bound on how many entries a single user, or
the deployment as a whole, can have at once. The 2026-05-10 long-TTL
smoke (`docs/deployment/vps-staging-smoke.md`) demonstrated 1800 s
detached-PTY survival end-to-end — empirically correct, but at scale
the same path becomes a resource pileup with no operator-defensible
ceiling.

Phase 1A landed the wire-observable TTL: the backend exposes the
configured `DETACHED_LIVE_PTY_TTL` via
`GET /api/v1/config/session-policy` and the SPA renders honest copy
parameterised on that value. Phase 1B's job is to put a defensible
operator-tunable ceiling on the resource pile before any later phase
(VT snapshots, multiplexer integration) widens what "live" means.

The roadmap document (`docs/persistent-sessions.md` § 8 "Phase 1 —
Tighten the current TTL model") explicitly lists per-user / per-deployment
caps as the Phase 1 deliverable. This is that design.

## 2. Definitions

The same word means different things in different parts of the stack;
this document uses these terms exclusively.

- **Live PTY.** A `TerminalSessionManager` runtime entry whose
  `RuntimeEntry.live` is `Some` — i.e. a real `russh::Channel` is open
  AND a forwarder task is draining its output. The orchestrator's
  authoritative count of live PTYs is `count_live_pty()` on the
  manager. Today this is the test-only `runtime_count()` accessor
  minus `Starting` entries that never bound a PTY.
- **Active session.** `terminal_sessions.status = 'active'`. A live
  PTY with at least one attached client.
- **Detached session.** `terminal_sessions.status = 'detached'`. A live
  PTY with zero attached clients, surviving in the bounded
  `DETACHED_LIVE_PTY_TTL` reconnect window.
- **Starting session.** `terminal_sessions.status = 'starting'`. A row
  exists; either no PTY has been bound yet OR PTY startup is in flight.
  Most rows in this state are transient (< 5 s); a stuck `starting` row
  is a burst-protection concern.
- **Closed session.** `terminal_sessions.status = 'closed'`. A terminal
  historical state — no live resources, never deleted from the user UI
  (SPEC.md "Inventory lifecycle policy" rule 4).
- **Attached client.** One open WebSocket bound to one
  `terminal_session_attachments` row. Today the WS handler is single-
  attachment-per-session in practice; the registry shape allows for
  future fanout but no surface uses it.
- **Owner.** The `user_id` recorded on `terminal_sessions.owner_id`.
  Every quota in this document is owner-scoped at the API boundary
  (SPEC.md architectural invariant: owner-scope reads + byte-identical
  404 for foreign ids).
- **Deployment-wide.** Counted against the single running backend
  process's in-memory registry; the value is exact for single-instance
  deployments and best-effort for any future multi-instance topology
  (see § 10).
- **Quota refusal.** A typed 429 wire envelope returned from
  `POST /api/v1/terminal-sessions` when a cap would be exceeded by the
  request. Never modifies state, never appends an audit row (see § 9).

The term **"quota"** in this document means **"in-memory ceiling on
concurrent resources held by the running backend"**. It does NOT mean
a rate (creations per minute) unless explicitly qualified as
**"burst rate"**.

## 3. Non-goals

These are explicitly out of scope for Phase 1B. Each is named so a
later slice can pick them up without re-arguing the boundary.

- **No durable-quota state.** Quotas are config + in-memory only. Zero
  new tables, zero new columns, zero new migrations. A backend restart
  resets every counter — that is correct because a restart also
  reaps every live PTY (`docs/spec/terminal.md` → startup
  reconciliation), so the counters and the resources track each other.
- **No multi-instance coordination.** Single-instance deployments are
  exact; any future multi-instance topology gets best-effort
  per-instance enforcement, called out honestly in operator copy and
  in the dashboard (§ 10). A real cross-instance quota would need a
  shared coordination layer (Postgres advisory locks or Redis or a
  leader-elected counter service) and is its own design slice.
- **No new audit-event kinds.** Quota refusals are operational, not
  security-relevant. The login throttler (which IS security-relevant
  via the probe channel) deliberately does not audit either; this
  policy mirrors it (`docs/agent/redaction-rules.md` § 9).
- **No admin / cross-user quota inspection.** The dashboard surface
  (§ 10.2) shows the caller's OWN counts only. An admin / cross-user
  quota view sits with the broader "admin surface" v1 deferral
  ([`SPEC.md`](../SPEC.md) "Out of scope (v1)").
- **No per-user override surface.** Every quota is per-deployment. A
  future `user_quotas` table (or `users.max_*` columns) is a separate
  design slice.
- **No quota on `terminal_session_attachments` rows.** The Phase 1B
  goal is bounding live PTYs (the heavy resources). Attachment-row
  bounding is its own concern when (a) the WS handler grows real
  multi-attach fanout, and (b) operator-observable churn rate
  warrants it. See § 13 Open question 6.
- **No rate quota in Phase 1B.** Burst-creation protection (creates
  per minute) is a separate axis from "concurrent ceiling". Phase 1B
  starts with concurrent ceilings; § 5.4 names the rate axis but
  defers it to Phase 1C or later.
- **No changes to the existing TTL knob.** The
  `terminal_sessions.detached_live_pty_ttl_seconds` config and the
  `5..=86_400` validator bound are unchanged. Phase 1's tightening of
  the production default upper bound (Phase 1 § 13 open question 1 in
  `docs/persistent-sessions.md`) is a documentation change inside the
  production example TOML, not a code change here.
- **No durable persistence claim.** Quotas exist on top of the
  in-memory TTL model. They do NOT change what survives a backend
  restart, and they do NOT enable a longer effective TTL. The roadmap
  for true durable persistence
  (`docs/persistent-sessions.md` § 8 Phase 2 / 3) is unchanged by
  Phase 1B.

## 4. Recommended initial quota model

The smallest useful set that puts a defensible ceiling on resource use
without growing the storage or coordination surface.

### 4.1 Per-user quotas (load-bearing)

| Quota | Counted as | Default | Bound at API boundary |
|---|---|---|---|
| `max_live_pty_sessions_per_user` | Owner's runtime-registry entries with `live = Some` (equivalently: `snapshot.status == RuntimeSessionStatus::Live`) | `8` | `POST /api/v1/terminal-sessions` create path |

The `live = Some` test is the load-bearing definition: `start_live_pty`
atomically sets both `entry.live = Some(...)` AND
`entry.snapshot.status = RuntimeSessionStatus::Live` under the same
write-lock guard (see `crates/relayterm-terminal/src/manager.rs`
around the `runtimes.write()` block in `start_live_pty`), and no
other path ever produces the combination `live = Some` with a
non-`Live` snapshot status. The DB-side distinction between
`active` and `detached` is IRRELEVANT to this counter — both are
`RuntimeSessionStatus::Live` in the registry, both hold the same
resource tuple, and the per-user quota is the sum of both. The
separate starting-burst quota in § 4.3 counts the disjoint
`live = None AND snapshot.status == Starting` set; the two quotas
never double-count.

A single quota for "any live PTY this user owns" is sufficient for
Phase 1B because each live PTY consumes the same shape of resource
(channel + PTY + buffer + tasks). Splitting it into separate
`active` / `detached` quotas adds operator complexity and a UX
discontinuity (a user who detaches a session would suddenly find they
can create another, even though no resource was freed) for no
defensible Phase 1B benefit. § 13 Open question 2 records the
revisit-point if observed usage shows the distinction matters.

Counted **against the in-memory registry**, not the DB, for three
reasons:

1. The registry is the authoritative tracker of resources actually
   held by THIS process. Counting from DB risks a small but real
   skew (e.g. an `active` row whose runtime registry entry was
   torn down by a forwarder exit but whose DB transition has not yet
   landed) and would let a user create more PTYs than the process
   can actually hold.
2. Startup reconciliation already collapses every pre-restart
   `starting`/`active`/`detached` row to `closed` BEFORE the HTTP
   listener binds (`docs/spec/terminal.md` → startup reconciliation;
   `crates/relayterm-db/src/repositories/terminal_session.rs` →
   `reconcile_orphaned_on_startup`), so the registry and the DB
   agree on "what's live" at every moment the API is reachable.
3. The registry-side count is O(N) over only this user's entries
   under the existing `RwLock`; no new index, no new query, no new
   schema concern.

### 4.2 Per-deployment quota

| Quota | Counted as | Default | Bound at API boundary |
|---|---|---|---|
| `max_live_pty_sessions_per_deployment` | Total runtime-registry entries with `live = Some` | `64` | `POST /api/v1/terminal-sessions` create path |

A deployment-wide ceiling on simultaneous live PTYs. Defends against
the "one user multiplies their per-user quota across N profiles"
shape AND the (currently theoretical) "many users, each near their
per-user quota" shape, before either becomes a problem.

The value `64` is conservative for a single-tenant self-hosted
deployment (the SPEC.md "Out of scope (v1)" v1 default). Operators
running a multi-user homelab can raise it; the upper bound the
validator accepts is `4096` so a configuration mistake does not
produce a silent unbounded ceiling.

### 4.3 Per-user starting-burst quota (defensive)

| Quota | Counted as | Default | Bound at API boundary |
|---|---|---|---|
| `max_starting_sessions_per_user` | Owner's runtime entries with `snapshot.status = Starting` AND no `live` bound yet | `4` | `POST /api/v1/terminal-sessions` create path |

This is defence in depth against a runaway client that POSTs many
sessions in flight without waiting for the PTY-start round-trip to
complete. Without it, the per-user `live` quota's effective ceiling
during a burst is `max_live + max_starting` (the burst can stack
before any in-flight PTY completes). The default of `4` is enough for
honest UI burst behaviour (a SPA navigation that opens a few sessions
in parallel) but rejects a tight POST loop.

This quota is intentionally separate from the `max_live` one because
they bound different resource shapes (in-flight SSH handshakes versus
completed PTY tuples) and a single combined number would be hard to
tune.

### 4.4 Quotas explicitly NOT in the initial set

| Considered quota | Why deferred |
|---|---|
| `max_detached_sessions_per_user` | Detached sessions are already bounded by the TTL; a separate ceiling adds operator complexity without a defensible Phase 1B win. The unified `max_live_pty_sessions_per_user` already caps it implicitly. Reconsider if usage shows a real difference. |
| `max_active_sessions_per_user` | Same reason — `max_live` already implicitly caps actives. Splitting introduces a UX discontinuity at detach. |
| `max_attachments_per_session` | Today the WS handler is effectively single-attachment-per-session. Multi-attach fanout is a future surface, and its quota lands with it. |
| `max_sessions_created_per_minute` | Rate is a different axis from concurrent ceiling. Phase 1B is concurrent only. The `LoginThrottler` shape (`crates/relayterm-auth/src/throttle.rs`) is the template the rate quota would borrow from, IF needed; per § 13 open question 4 it is plausibly never needed for a single-tenant self-hosted deployment. |
| `max_sessions_per_server_profile` | A reasonable shape for the "one profile, many tabs" pattern, but ties into the still-unfinished inventory destructive-action policy (SPEC.md) — defer until the destructive-action design slice lands. |
| `max_recording_chunk_writers_per_user` | Recording is OFF by default; per-session 64 MiB cap + retention sweep already bound storage. A per-user concurrent-writer count is meaningful only after recording is widely opt-in. |

## 5. Configuration surface

### 5.1 New config fields

Phase 1B adds three fields to the existing `TerminalSessionsConfig`
([`apps/backend/src/config.rs`](../apps/backend/src/config.rs)) struct.
The TOML / env mirror follows the established
`RELAYTERM_TERMINAL_SESSIONS__*` convention.

```toml
[terminal_sessions]
# Already exists — unchanged.
detached_live_pty_ttl_seconds = 30

# NEW (Phase 1B).
max_live_pty_sessions_per_user = 8
max_starting_sessions_per_user = 4
max_live_pty_sessions_per_deployment = 64
```

Env mirrors (existing convention):

- `RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER`
- `RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER`
- `RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT`

### 5.2 Validation envelope

The existing `Config::validate_terminal_sessions` adds three new
bounds. Each rejected value is a typed boot failure rather than a
silent fall-through (consistent with the existing TTL validator).

| Field | Validator bound | Rationale |
|---|---|---|
| `max_live_pty_sessions_per_user` | `1..=256` | `0` means "no sessions ever" — a config mistake. `256` per user single-tenant is far past the practical ceiling. |
| `max_starting_sessions_per_user` | `1..=32` | `0` deadlocks every create. `32` is well past any honest burst pattern. |
| `max_live_pty_sessions_per_deployment` | `1..=4096` | `0` disables the backend. `4096` is past the kernel-side FD ceiling on most single-host deployments. |

The validator MUST also reject the configuration `max_live_pty_sessions_per_deployment < max_live_pty_sessions_per_user` — a per-user ceiling
above the deployment ceiling is a contradiction. The error names both
fields explicitly so the operator can fix the right one.

The validator SHOULD additionally reject
`max_live_pty_sessions_per_deployment < max_starting_sessions_per_user`
for the same reason — a starting-burst cap above the deployment
ceiling would let one user's burst exhaust the deployment-wide
slot before any session promotes to `Live`. The error names both
fields explicitly. (This is "SHOULD" rather than "MUST" because
the deployment quota will still refuse the burst correctly at the
enforcement layer; the validator rejection is operator
ergonomics — surfacing the contradiction at boot rather than at
first refusal.)

### 5.3 Deployment plumbing — all three Compose templates + both TOML examples

Per the 2026-05-09 lesson in AGENTS.md, **every** new operator env knob
MUST be wired into all three Compose templates AND both worked-example
TOMLs AND the `scripts/check-doc-contracts.sh` § 9 matrix. The
Phase 1B implementation slice MUST extend each of:

- `deploy/relayterm.env.example`
- `deploy/docker-compose.example.yml`
- `deploy/docker-compose.images.example.yml`
- `deploy/docker-compose.traefik-staging.example.yml`
- `docs/config-examples/relayterm.dev.example.toml`
- `docs/config-examples/relayterm.production.example.toml`
- `scripts/check-doc-contracts.sh` § 9 env-var × file matrix

with the three new knobs. Per-file intentional omissions (if any
template intentionally omits a knob) MUST be encoded explicitly in
the matrix loop with a justifying comment.

### 5.4 Public surface — extend `/api/v1/config/session-policy`

The Phase 1A `SessionPolicyResponse`
([`crates/relayterm-api/src/dto/session_policy.rs`](../crates/relayterm-api/src/dto/session_policy.rs))
carries only `detached_live_pty_ttl_seconds`. Phase 1B widens it with
the per-user limits (only — NEVER the deployment-wide cap; see § 10
multi-instance):

```json
{
  "detached_live_pty_ttl_seconds": 30,
  "max_live_pty_sessions_per_user": 8,
  "max_starting_sessions_per_user": 4
}
```

The SPA uses these values to render quota-refusal copy parameterised
on the deployment's configured value, the same way the TTL window is
parameterised today. The deployment-wide ceiling is operator-only
and stays server-side — exposing it would give every authenticated
caller a deployment-fingerprint signal with zero benefit to honest
UX copy.

The frontend `parseSessionPolicy`
([`apps/web/src/lib/api/sessionPolicy.ts`](../apps/web/src/lib/api/sessionPolicy.ts))
parses field-by-field and rejects out-of-range values; Phase 1B
extends it with two new integer fields and reuses the same
range-rejection backstop. The sentinel sweep in
`apps/web/tests/sessionPolicy.test.ts` MUST be extended so a stray
secret-shaped sibling cannot piggyback the wire body.

### 5.5 Production default vs dev default

The dev example TOML keeps the loose defaults (`8 / 4 / 64`) so a
local homelab "just works." The production example TOML keeps the
same defaults — Phase 1B does not tighten them per-deployment; a
later slice (Phase 1C) can do that based on observed usage at the
production smoke. Phase 1B SHIPS the knobs and the wire shape; it
does not opine on production-specific values beyond the defensible
defaults above.

## 6. API / enforcement points

### 6.1 Single enforcement point: `POST /api/v1/terminal-sessions`

The only place in the orchestrator that allocates a new live PTY is
the create route
([`crates/relayterm-api/src/routes/v1/terminal_sessions.rs::create`](../crates/relayterm-api/src/routes/v1/terminal_sessions.rs)).
Every other lifecycle move — attach, detach, reattach, resize,
close — either reuses an existing PTY OR frees one. So the quota
check belongs in exactly one place.

### 6.2 Ordering inside `create()`

The check sits between the existing host-key precondition and the
vault decrypt, BEFORE any outbound network or cryptographic work:

```
0. CsrfGuard               ← first, before any state-touch (SPEC.md CSRF)
1. AuthenticatedUser
2. Resolve (profile, host, identity) trio  ← owner-scope
3. Reject `server_profile disabled`        ← existing
4. Resolve host-key accept pins            ← existing
5. Reject `host_key not trusted`           ← existing
6. ── QUOTA CHECK ──                       ← NEW (Phase 1B)
7. Vault.decrypt_private_key()             ← existing
8. SshPtyConfig + bridge.start()           ← existing
9. terminal_sessions.start_live_pty()      ← existing
```

The check happens AFTER ownership + host-key gating so a quota
refusal cannot be used to probe whether a foreign / disabled /
untrusted profile exists. Foreign profiles still collapse to a 404,
disabled / untrusted profiles still surface their typed 409, BEFORE
the quota check runs. Quota refusals therefore only fire for
combinations the caller would otherwise have been allowed to launch.

The check happens BEFORE vault decrypt + SSH connect so a rejected
request does no outbound work, no decryption cycle, and no
target-host probe.

### 6.3 Counter primitives on the manager

Phase 1B adds three new accessors on `TerminalSessionManager`:

```rust
pub fn count_live_pty_for_user(&self, owner_id: UserId) -> usize;
pub fn count_starting_for_user(&self, owner_id: UserId) -> usize;
pub fn count_live_pty_total(&self) -> usize;
```

Each iterates the existing `runtimes` map under its existing
`RwLock<HashMap>`; no new lock, no new index. The cost is O(N) over
the registry, which is bounded by the deployment-wide cap (§ 4.2,
default 64) — every call traverses a tiny structure.

The existing `runtime_count()` accessor on the manager is `pub` and
marked `#[must_use]` with a doc-comment "test-only convenience"; it
is not gated by `#[cfg(test)]`. Phase 1B adds the three typed
counters above. The existing `runtime_count()` MAY stay (it is a
trivial wrapper) or MAY be removed — the implementation slice
decides at code-review time. No compiler-level cfg attribute is
involved.

### 6.4 No DB-side count primitive

Phase 1B deliberately does NOT add `count_live_for_user` or similar
to the repository trait. The registry is authoritative for "what's
live right now", and a DB count would race the registry without
delivering an additional correctness guarantee on a single-instance
deployment. If multi-instance coordination ever lands, the
coordination layer (Postgres advisory lock + per-user counter table
OR a distinct coordination service) is its own design.

### 6.5 Attach / reattach / detach / close

Unchanged. Attach to an existing session does not allocate a new PTY,
so it does not trip the quota. Reattach inside the TTL window
transitions an existing `detached` to `active`; the live-PTY count is
unchanged. Detach reduces the attached-client count, never the
live-PTY count. Close frees the live-PTY slot the next create can
fill.

### 6.6 Reaper / cleanup interactions

The detach-TTL reaper (`expire_detach_close` on the manager) closes
a session whose TTL elapsed and frees its registry entry. From the
quota's point of view, this is identical to a user-initiated close —
the slot frees automatically and the next create succeeds without
operator action. The reaper does NOT emit any quota-related event;
it never touches the quota counters except by removing its own
runtime entry.

The forwarder-exit path (remote shell exits → manager observes
`broadcast::error::RecvError::Closed`) similarly frees the slot.

The startup reconciliation pass that runs BEFORE the HTTP listener
binds writes its `closed` rows under transaction; the in-memory
registry is empty when the listener starts taking traffic, so the
first create on a fresh boot sees a zero count.

## 7. Error semantics and UI copy

### 7.1 Wire envelope

The recommended shape is a typed 429 with a stable wire `code`:

```http
HTTP/1.1 429 Too Many Requests
Content-Type: application/json

{
  "error": {
    "code": "too_many_sessions",
    "message": "too many terminal sessions"
  }
}
```

A second typed code distinguishes the per-user from the
deployment-wide ceiling:

```http
HTTP/1.1 429 Too Many Requests
Content-Type: application/json

{
  "error": {
    "code": "too_many_sessions_deployment",
    "message": "deployment session capacity reached"
  }
}
```

And a third for the starting-burst ceiling:

```http
HTTP/1.1 429 Too Many Requests
Content-Type: application/json

{
  "error": {
    "code": "too_many_starting_sessions",
    "message": "too many sessions starting"
  }
}
```

Mapping to `ApiError`
([`crates/relayterm-api/src/error.rs`](../crates/relayterm-api/src/error.rs)):

```rust
// New variants — append to the existing ApiError enum.
ApiError::TooManySessions { scope: QuotaScope }

pub enum QuotaScope {
    PerUserLive,        // → code "too_many_sessions"
    PerUserStarting,    // → code "too_many_starting_sessions"
    Deployment,         // → code "too_many_sessions_deployment"
}
```

This requires a corresponding extension of the existing `ErrorCode`
enum in `error.rs`. The existing pattern is one-to-one: each
`ApiError` variant maps to exactly one `ErrorCode`, which maps to
exactly one wire `code` string. Phase 1B has three distinct wire
codes, so the implementation MUST add three new `ErrorCode`
variants (`TooManySessions`, `TooManyStartingSessions`,
`TooManySessionsDeployment`) alongside the new `ApiError` variant.
The `ApiError::parts()` arm for `TooManySessions { scope }` then
matches on `scope` and emits the corresponding `ErrorCode`. The
existing `ErrorCode::TooManyRequests` stays scoped to the login
throttler — overloading it with the quota refusal would conflate
two different wire contracts (the login-throttle 429 deliberately
collapses to a single static body; the quota 429 deliberately
distinguishes three causes for SPA copy).

The wire body comes from the existing `ApiError::parts()` mapping;
the operator-side detail (current count, cap, owner id) lives ONLY
in the `warn!` line, never on the wire.

Wire alternative considered: a single `ApiError::Conflict { entity:
"terminal_session", reason: "too_many" }`. This collapses the three
scopes into one code and forces the caller to interpret the
`message` string for tuning UX. It also conflicts with the existing
`409 conflict { entity: "terminal_session" }` semantic ("session
closed") and would muddy that contract. The roadmap in
`docs/persistent-sessions.md` § 8 Phase 1 also explicitly named
`429 too_many_sessions`. Recommendation: 429 with the three typed
codes above; § 13 Open question 1 records the revisit point.

### 7.2 No `Retry-After` header

Quota refusal does NOT carry a `Retry-After` header. The user cannot
retry productively against a wall-clock — they need to act (close an
existing session, or wait for a detached session's TTL to elapse,
which is a different wait per session). Adding a `Retry-After` value
the SPA would only ignore is misleading.

This matches the existing `LoginThrottler` posture
(`docs/agent/redaction-rules.md` § 9 — login 429 has no
`Retry-After` either).

### 7.3 No information leaks in the refusal

The 429 body MUST NOT contain:

- the current per-user count (telegraphs how many sessions the user
  has, which a multi-device user already knows but exposing it
  through the wire normalises future leakage)
- the cap (telegraphs deployment configuration to every caller)
- any session id, profile id, host id, identity id, owner id
- any operator-side detail string

The wire `message` is one of three static strings (§ 7.1). The
caller-side cap (and only the per-user one) is available via
`/api/v1/config/session-policy` — which already requires
`AuthenticatedUser` and is wire-stable. This is the same pattern as
the detached-TTL value today.

### 7.4 Frontend mapping (`apps/web/src/lib/api/terminalSessions.ts`)

The existing typed-error helper (see
`apps/web/src/lib/api/apiErrors.ts::describeLoadError`) does NOT echo
the wire `message`. Phase 1B adds three new reason discriminators to
`terminalSessions.ts`'s create-error union:

```ts
type CreateTerminalSessionError =
  | { reason: "too_many_sessions" }
  | { reason: "too_many_starting_sessions" }
  | { reason: "too_many_sessions_deployment" }
  | /* existing reasons */
  ;
```

Mapping rule: on HTTP 429 with one of the three codes above, return
the matching discriminator; on any other 429 collapse to the
existing generic-throttle reason; on any non-429 fall through to
existing error handling.

The mapping NEVER inspects the `message` field; the typed `code`
drives every branch. This mirrors the existing
`describeAuthError`-on-`code`-only pattern.

### 7.5 SPA copy

Pinned text the SPA renders for each refusal. Each is parameterised
on the wire-observed per-user cap (`max_live_pty_sessions_per_user`
from `/api/v1/config/session-policy`) AND, where it embeds a duration
fragment, the `formatDetachedTtl(...)` helper from
`apps/web/src/lib/api/sessionPolicy.ts` (which returns the inline
fragment `"about 30 seconds"`). Do NOT call `describeDetachedTtl`
inline — that helper returns the load-bearing two-sentence
persistence-disclaimer paragraph and is pinned by the existing
`sessionPolicy.test.ts` tests; reducing or reflowing it for a
different callsite would regress the persistence-honesty contract.
Each string is sentinel-tested in
`apps/web/tests/sessionStatus.test.ts` (or a new
`tests/sessionQuotas.test.ts` peer test file) with the same harness
as the detached-copy honesty checks.

**Per-user live ceiling reached (`too_many_sessions`):**

> "You're at the limit of N concurrent terminal sessions. Close a
> session from the Sessions list before starting another. Detached
> sessions count toward this limit and free up automatically after
> their reconnect window
> (`<formatDetachedTtl(ttl_seconds)>` — `"about 30 seconds"` by default)."

`N` is read from the configured per-user cap; the TTL window
fragment is the inline-duration return of `formatDetachedTtl`, not
the two-sentence `describeDetachedTtl`.

**Per-user starting ceiling reached (`too_many_starting_sessions`):**

> "Too many terminal sessions are starting at once. Wait a moment
> for the in-flight starts to complete, then try again."

No cap value in this copy — burst protection is a lower-volume
operator-side concern.

**Deployment ceiling reached (`too_many_sessions_deployment`):**

> "The server is at its terminal-session capacity. Try again in a
> few moments, or contact the operator if this persists."

Honest about the multi-user shape without saying "another user has
too many sessions". The advice ("try again in a few moments")
covers the eventual close that frees a slot.

**Anti-overclaim register** for quota copy. None of these substrings
may appear in any of the three SPA copies (extend the existing
forbidden-substring sweep in
`apps/web/tests/sessionStatus.test.ts`):

- "your session quota"          (overclaims that the quota is per-user-personalised)
- "we're rate-limiting you"     (this is not rate-limiting)
- "please slow down"            (not the user's fault)
- "queue"                       (no queue exists; refusal is immediate)
- "wait <N> seconds"            (no Retry-After contract; never quote a wall clock)

### 7.6 What the dev lab does

The dev workbench launcher
(`apps/web/src/lib/dev/`) is OUT of scope for the quota refusal copy
— it deliberately stays self-contained for the renderer-comparison
surface (see SPEC.md "Renderer adapters → Production terminal UI"
isolation rule). The dev lab can refuse on the same 429 with a
typed error frame; it does NOT need the parameterised production
copy.

## 8. Session events, audit, and logging policy

### 8.1 No new `session_events` kind

The `SessionEventKind` enum
([`crates/relayterm-core/src/session_event.rs`](../crates/relayterm-core/src/session_event.rs))
already carries `created`, `attached`, `detached`, `reattached`,
`resized`, `replay_started`, `replay_completed`, `closed`. Phase 1B
does NOT add a `quota_refused` kind because the refusal happens
BEFORE the row insert — there is no `terminal_sessions` row to
attach a session event to. A session that DOES exist already gets
its lifecycle events; nothing about quota changes that surface.

### 8.2 No new `audit_events` kind

The shared redaction backstop is `AUDIT_FORBIDDEN_SUBSTRINGS` in
`crates/relayterm-api/tests/api.rs` plus the
`audit_events_kind_chk` constraint. Phase 1B touches neither, by
design:

- Quota refusals are operational, not security-relevant. The
  login throttler — which IS security-relevant via the user-existence
  probe channel — also does not audit (`docs/agent/redaction-rules.md`
  § 9). The session-quota throttle has no equivalent probe surface
  (the user already authenticated to reach this route).
- Auditing every quota refusal would flood the log with no signal,
  since a single misbehaving client can easily produce thousands of
  refusals.
- The single audit-row-per-successful-state-transition rule from
  `docs/agent/redaction-rules.md` § 2 explicitly excludes "redundant
  / idempotent" no-ops; a refusal is the strongest form of "no-op".

### 8.3 Operator-side logging policy

Each quota refusal emits ONE `tracing::warn!` line with public
metadata only:

```rust
warn!(
    user_id = %user.user_id(),
    scope = %scope.as_str(),       // "per_user_live" | "per_user_starting" | "deployment"
    current_count = current,        // u64
    cap = cap,                      // u64
    "terminal session quota refused"
);
```

Forbidden in the line: any session id, attachment id, profile id,
host id, identity id, IP address, User-Agent, or wire message. The
`current_count` and `cap` are public-shape integers — they describe
deployment state, not user content. The `user_id` is the
authenticated caller and is already in many existing operator log
lines (`AuthenticatedUser` extraction itself emits user_id).

**Volume concern.** A misbehaving client that tight-loops `POST
/api/v1/terminal-sessions` can produce a high-volume `warn!` stream.
Phase 1B does NOT introduce a token-bucket coalescer for this line,
because (a) the per-user starting-burst cap (§ 4.3) already bounds
the rate at ~4 in-flight starts before refusal takes over, and
(b) the existing `LoginThrottler` does not coalesce either. If
operator observation shows runaway volume in practice, a follow-up
slice can move the line to `info!` or add a per-`(user_id, scope)`
coalescer; until then, the simpler one-warn-per-refusal shape stays.

### 8.4 Metrics / dashboard plumbing (deferred)

A future Prometheus-style metrics surface (a `relayterm_quota_*`
counter family) is desirable but out of scope for Phase 1B — the
metrics primitives don't exist in the codebase yet. The operator
dashboard surface (§ 10.2; see the smoke recipe in § 11) reads the
same counters the enforcement path uses through a new authenticated
read endpoint;
that read endpoint is the implementation seam where Prometheus
metrics could later land without re-plumbing.

## 9. Multi-instance limitations

Single-instance and multi-instance behave differently. This is
called out honestly in three places:

1. **This document** (§ 4.2 deployment-wide quota): the value is
   exact for one instance, best-effort per-instance for any future
   multi-instance topology.
2. **The operator dashboard tile** (§ 11 implementation roadmap):
   the dashboard renders the per-instance value with a tooltip
   that names the per-instance scope when more than one instance
   is configured.
3. **The production runbook**
   (`docs/deployment/production-runbook.md`): adds one paragraph
   under "scaling considerations" naming the per-instance quota
   semantics. The exact text to land is:

   > **Session quotas are per-instance.** RelayTerm enforces
   > `terminal_sessions.max_live_pty_sessions_per_user` and
   > `max_live_pty_sessions_per_deployment` against each backend
   > instance's in-memory registry. A deployment running N
   > instances behind a load balancer has an effective
   > deployment-wide ceiling of `N × max_live_pty_sessions_per_deployment`,
   > not the per-instance value. Quotas are a resource-pile
   > defence, not a tenant-isolation primitive. Single-instance
   > deployments (the v1 default) are exact.

   Phase 1B's implementation slice ADDS this paragraph; the
   runbook update lands alongside the code.

The honest claim is:

> Phase 1B quotas are enforced against the running backend's
> in-memory registry. Each instance enforces independently. A
> deployment running N instances behind a load balancer has an
> effective deployment-wide ceiling of `N × max_live_pty_sessions_per_deployment`,
> not the per-instance value. Quotas are NOT a tenant-isolation
> primitive; they are a resource-pile defence.

A genuine cross-instance quota would need a coordination layer
(Postgres advisory lock per (owner_id, "quota"), OR a redis-backed
counter, OR a leader-elected counter service). Each adds enough
complexity that it belongs in its own design slice, gated by an
operator request for multi-instance.

The single-tenant v1 default
([`SPEC.md`](../SPEC.md) "Out of scope (v1)") is single-instance,
so this limitation does not affect any v1 deployment.

## 10. Implementation roadmap (staged follow-up slices)

Phase 1B is one design document; the implementation lands across
two small slices so each is reviewable end-to-end and the production
default values can be validated before they ship.

### 10.1 Slice 1B.1 — per-user live ceiling

**Goal.** First and most important quota: `max_live_pty_sessions_per_user`.

**In scope.**

- Config: `terminal_sessions.max_live_pty_sessions_per_user`,
  validator bound `1..=256`, default `8`.
- Manager: `count_live_pty_for_user(owner_id) -> usize`.
- API: `ApiError::TooManySessions { scope: QuotaScope::PerUserLive }`
  variant, wire code `too_many_sessions`.
- API: enforcement step in `create()` (§ 6.2 ordering).
- Public DTO: `SessionPolicyResponse.max_live_pty_sessions_per_user`.
- Frontend: `parseSessionPolicy` extension; `terminalSessions.ts`
  typed-error mapping; sentinel-test extension; ProductionTerminal
  copy.
- Plumbing: all three Compose templates, both worked-example TOMLs,
  `scripts/check-doc-contracts.sh` § 9 matrix.
- Tests:
  - integration: refusal under cap, success at cap-after-close,
    refusal does not write any DB row or session event, refusal
    does NOT echo any forbidden substring, refusal works against
    the existing dev-mode CSRF posture (this is a `POST` with
    `CsrfGuard`).
  - manager: `count_live_pty_for_user` correctness across attach /
    detach / reattach / close / TTL-reaper.
  - frontend: typed-error mapping, parameterised copy at default
    + raised cap, forbidden substring sweep.

**Out of scope (1B.1).** Deployment-wide quota, starting-burst
quota. Dashboard tile.

**Smoke.** Extend the 2026-05-10 long-TTL smoke recipe with an
explicit refusal step: launch the configured cap of sessions, POST
one more, observe the 429 + no DB row + no audit row + no log line
echo of any session id.

### 10.2 Slice 1B.2 — deployment + starting-burst ceilings

**Goal.** Round out the quota set: `max_live_pty_sessions_per_deployment`
and `max_starting_sessions_per_user`. Land the operator dashboard
tile.

**In scope.**

- Config: two new fields, paired validator bounds, paired plumbing
  across all six deploy/docs files + `check-doc-contracts.sh` §9.
- Manager: `count_live_pty_total()`, `count_starting_for_user(...)`.
- API: two new `QuotaScope` variants, two new wire codes
  (`too_many_sessions_deployment`, `too_many_starting_sessions`).
- API: enforcement in `create()` AFTER the per-user check (so
  per-user violations don't get masked by deployment-wide ones).
- Public DTO: `SessionPolicyResponse.max_starting_sessions_per_user`.
  Deployment-wide value stays server-side.
- Frontend: two new typed-error reasons + matching SPA copies.
- Operator dashboard tile (Settings → Sessions): renders the
  caller's live count, the per-user cap, and the TTL. Single
  authenticated GET endpoint returns the integers; mirrors
  `/api/v1/config/session-policy` shape but is scoped to the
  caller's counts. Tile is read-only; no quota override surface.
- Tests:
  - integration: per-user 429 fires BEFORE deployment 429 when
    both would apply; deployment 429 fires when only deployment
    is over.
  - frontend: tile renders honestly when policy fetch fails (uses
    the same fallback contract as the existing TTL copy).

**Out of scope (1B.2).** Per-user override surface, cross-instance
coordination, rate quota, attachment-row quota.

**Smoke.** Extend the same long-TTL recipe with the deployment
refusal step (configure `max_deployment = 2`, launch as two distinct
users, refuse on user C). Confirm the operator dashboard tile
reflects each user's own count and never leaks foreign counts.

### 10.3 Slice 1B.3 — production-default tuning (optional follow-up)

After 1B.2 has run in staging for a real workload, revisit the
production-example TOML's defaults. This slice is documentation +
TOML only; no code change, no migration, no schema. May not be
needed if the 1B.1 / 1B.2 defaults are correct.

## 11. Smoke / verification plan

Each slice gets one smoke recipe entry in
`docs/deployment/vps-staging-smoke.md`. The recipes follow the
existing contract (throwaway inventory, no real-secret reuse, post-run
cleanup, matrix-style log of every observed `session_events` and
`audit_events` row, redaction sentinel sweep).

| Slice | Smoke entries |
|---|---|
| 1B.1 | (a) per-user refusal at cap; (b) close-then-success at cap-after-close; (c) refusal AFTER startup reconciliation (the registry is empty so the first cap+1 creates land); (d) refusal redaction sentinel sweep (the response body has no session id, no profile id, no `current_count`, no `cap`). |
| 1B.2 | (e) deployment refusal with two users; (f) starting-burst refusal via a tight POST loop; (g) dashboard tile renders user-own counts only. |

Each entry uses the existing `RELAYTERM_AUTH__ALLOWED_ORIGINS` +
loopback caveat (per the 2026-05-09 lesson in AGENTS.md) and the
desktop WebKit cache caveat. The redaction sentinel sweep extends
`AUDIT_FORBIDDEN_SUBSTRINGS` in `crates/relayterm-api/tests/api.rs`
with `too_many_sessions` / `too_many_sessions_deployment` /
`too_many_starting_sessions` as ALLOWED wire codes; the existing
sweep continues to reject every other potentially-leaky shape.

## 12. Pinned UX-copy contract (anti-overclaim)

The strings in § 7.5 are normative. Any implementation slice that
uses different wording MUST update this section first. Each string
is pinned by a sentinel test, the same way the detached-copy honesty
checks are pinned today.

Forbidden substrings on the quota-refusal SPA copies (extend the
existing sweep in `apps/web/tests/sessionStatus.test.ts`):

- (case-insensitive) "your session quota"
- (case-insensitive) "we're rate-limiting you"
- (case-insensitive) "please slow down"
- (case-insensitive) "queue"
- (case-insensitive) "wait \\d+ seconds"
- (case-insensitive) "always available"
- (case-insensitive) "persistent across restart"

The list does NOT duplicate the credential / token / vault sentinels
in `AUDIT_FORBIDDEN_SUBSTRINGS`; those sweeps stay focused on their
domain. The persistence-overclaim sentinels in
`docs/persistent-sessions.md` § 11.7 also continue to apply.

## 13. Open questions

Each is an explicit ambiguity for the owner to resolve before the
matching slice can start.

1. **Wire shape: 429 with typed codes vs 409 conflict with reason.**
   The recommendation is 429 with three typed codes (§ 7.1) because
   (a) `docs/persistent-sessions.md` § 8 Phase 1 canonically named
   `429 too_many_sessions` and (b) 409 conflicts in this codebase
   are entity-state contradictions, not quantity ceilings. The
   alternative is one `ApiError::Conflict { entity:
   "terminal_session", reason: "too_many_live"|"too_many_starting"|
   "deployment_full" }` which reuses existing machinery without new
   `ErrorCode` variants. Resolve at slice 1B.1 design review.

2. **Should `max_active` and `max_detached` be separate per-user
   quotas?** The recommendation is a single combined
   `max_live_pty_sessions_per_user`. If observed usage shows
   detached-session accumulation is a distinct problem (e.g.
   operators want to allow many active but few detached), splitting
   the quota into two is a clean follow-up — both are runtime-
   registry reads under the same lock. Defer the decision until
   real usage is observed.

3. **Production default for `max_live_pty_sessions_per_user`.** The
   proposed default `8` is conservative for solo homelab use and
   defensible for a small multi-user deployment. A real homelab
   user might want `4`; a small team might want `16`. Defer to slice
   1B.3 tuning AFTER staging observation.

4. **Do we need a rate quota at all?** The Phase 1B set has only
   concurrent ceilings. A burst-creation rate quota
   (`max_creates_per_minute`) could be added later. The current
   defence-in-depth shape (`max_starting + max_live` together) bounds
   the total in-flight footprint; the marginal benefit of an
   additional time-windowed bucket is unclear for a single-tenant
   deployment. Open question. Resolve only if real usage shows it.

5. **Should the operator dashboard tile show the deployment-wide
   count?** The recommendation (§ 7.4 + § 10.2) is NO for v1 — the
   tile shows the caller's own counts only. A future admin view
   would surface the deployment-wide value. The single-tenant v1
   shape makes the per-user view equivalent to the deployment view
   for the homelab operator. Reconsider if multi-user lands.

6. **Should the quota set include a `max_attachments_per_session`?**
   Today the WS handler is effectively single-attachment-per-session;
   the registry is shaped for multi-attach but no production surface
   uses it. Defer until multi-attach lands (a separate Phase X slice).

7. **Should the per-user starting-burst cap be combined with the
   per-user live cap?** The recommendation (§ 4.3) keeps them
   separate because they bound different resource shapes. A single
   combined cap would be hard to tune (the live ceiling caps
   long-lived resources, the starting ceiling caps in-flight
   crypto / network work). Open for a future refactor only.

8. **Should the manager track per-user counts incrementally (a
   `HashMap<UserId, AtomicUsize>`) instead of O(N) scans of the
   registry?** At the default bounds (per-user `8`, deployment
   `64`), a full scan is two-digit operations under an existing
   `RwLock` read — incremental tracking adds complexity for no
   measurable benefit. Reconsider only if the deployment-wide
   ceiling rises above ~1024.

---

End of design document. No implementation work follows from this
document directly; each slice opens its own design review and code
review.
