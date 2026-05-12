# Durable persistent sessions — design

> **Status (2026-05-10):** design draft, NOT a slice.
> This document defines what "durable persistent sessions" should mean for
> RelayTerm and the staged roadmap that gets us there. It does NOT change any
> code, schema, route, frontend behaviour, CI, or staging configuration.
> Drift in any later implementation slice goes through this document first.
>
> **Related normative documents:**
>
> - [`SPEC.md`](../SPEC.md) — architectural invariants, behavior contracts.
> - [`docs/spec/terminal.md`](spec/terminal.md) → "Detached-session TTL contract", "Output sequence + in-memory replay buffer contract" — current TTL model.
> - [`docs/spec/recording.md`](spec/recording.md) + [`docs/terminal-recording.md`](terminal-recording.md) — durable display-history recording (output-only). The recording doc's § 9 ("Backend restart recovery") is the canonical statement that recording captures display history, NOT a live PTY, and § 9.4 ("Future work: live PTY persistence") is the seam this document expands.
> - [`docs/agent/redaction-rules.md`](agent/redaction-rules.md) — the long form of the redaction contracts referenced below.
> - [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) → 2026-05-10 Long-TTL (1800 s) reconnect smoke — empirical baseline for the current model.

## 1. Current behavior summary

What ships today, named precisely so the staged plan can build on it without
overclaiming.

- **Backend-owned SSH session.** The live `russh::Channel`, the libghostty-vt
  parser state (when the future observer lands), and all replay state belong
  to the orchestrator (`relayterm_terminal::TerminalSessionManager`). The
  browser/Tauri client never holds the channel or any private key. ([`SPEC.md`](../SPEC.md) → Architectural invariants 1–5.)
- **In-memory replay buffer.** Every PTY `Output` frame carries a monotonic
  per-session `seq` starting at 1. A bounded ring (default `max_frames = 1024`
  AND `max_bytes = 1 MiB`, FIFO, most-recent-frame always retained) lives
  inside the live runtime entry. On reattach with `last_seen_seq`, the wire
  replays `replay_start` → buffered `output` → `replay_end`, or emits a
  single `replay_window_lost` if the bookmark predates the buffer.
  ([`docs/spec/terminal.md`](spec/terminal.md) → "Output sequence + in-memory replay buffer contract".)
- **Detached live-PTY TTL.** When the last client detaches, the manager
  transitions the row to `detached`, schedules a `tokio::sleep(TTL)` task
  that calls `close_session` on wake, and keeps the PTY + broadcast channel
  + replay buffer alive for the duration of the window. Reattach within the
  window cancels the timer and resumes from the bookmark. The TTL is
  `relayterm_terminal::DETACHED_LIVE_PTY_TTL` (default 30 s), tunable
  per-deployment via `terminal_sessions.detached_live_pty_ttl_seconds`
  (env `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`,
  bounded `5..=86400`). The 1800 s long-lived smoke on 2026-05-10
  empirically confirmed two within-window reconnects, replay delivery of
  detached output, and the reaper firing at the configured deadline; staging
  was reverted to the default 30 s afterward.
- **No restart survival of live PTYs.** A backend restart drops every entry
  in the in-memory runtime registry — the live `russh::Channel`, the
  detach-TTL timer, and the replay buffer all evaporate. Pre-restart rows in
  `starting` / `active` / `detached` are transitioned to `closed` by a
  startup reconciliation pass that runs AFTER the DB pool is ready and
  BEFORE the HTTP listener binds; the pass writes one
  `session_events { kind: closed, payload: { reason: "startup_reconciliation",
  previous_status, reconciled_at } }` row per session, and — for any
  reconciled session that has at least one chunk row — one
  `terminal_recording_markers { kind: closed, seq: MAX(seq_end), payload:
  { same } }` row in the same transaction. Zero `audit_events` rows are
  written. ([`docs/spec/terminal.md`](spec/terminal.md) → startup recovery; [`docs/terminal-recording.md`](terminal-recording.md) § 9.)
- **Durable terminal recording (display history).** OFF by default,
  config-gated. When enabled with a SEPARATE recording master key, the
  orchestrator writes output bytes as opaque chunks
  (`terminal_recording_chunks`) plus lifecycle metadata markers
  (`terminal_recording_markers`). Owner-scoped read API returns chunk bytes
  ONLY as `data_b64`; the closed-session replay viewer mounts a
  stdin-disabled `XtermRenderer`. Retention sweep (Stage A startup + Stage B
  periodic, advisory-locked, fail-soft) emits a `recording_purged` audit row
  with `actor_id = NULL`. Recording is **display history**, NOT live-PTY
  persistence. ([`docs/spec/recording.md`](spec/recording.md), [`docs/terminal-recording.md`](terminal-recording.md).)
- **Operator-visible state set.** `terminal_sessions.status` is
  `starting | active | detached | closed`. Closed sessions are NEVER deleted
  from the user UI; they remain a permanent historical record. Recording
  rows live in their own table family and cascade `RESTRICT` against the
  session row.

## 2. Definitions / terminology

The same word ("persistent", "resume") means different things to different
parts of the stack. This document uses the following terms exclusively.

- **Client reconnect (in-memory).** Browser/Tauri client drops its
  WebSocket, then re-attaches to the SAME live PTY within
  `DETACHED_LIVE_PTY_TTL`. Replay is in-memory only. ✓ Implemented.
- **Backend reconnect window.** The total time after a client drop during
  which a still-live PTY remains reattachable; identical to the TTL above
  on today's backend. ✓ Implemented.
- **Display reconstruction.** Re-rendering what the terminal previously
  printed (output bytes), without resuming an interactive shell. Required
  surface today: closed-session replay viewer. ✓ Implemented (read-only).
- **Live shell persistence.** Keeping a real, interactive remote shell
  process running after the RelayTerm backend goes away (restart, crash,
  redeploy). NOT implemented today. Recording does NOT deliver this; it
  records what was printed, not the running process. (Recording doc § 9.4.)
- **Host-side multiplexer persistence.** Live shell persistence delivered
  by running the remote shell inside `tmux` / `screen` / a user-systemd
  unit on the SSH target host. RelayTerm's backend re-attaches the
  multiplexer session over a fresh SSH transport. NOT implemented today.
- **Managed agent persistence.** A RelayTerm-owned process running on a
  managed host that owns the shell lifecycle and re-exposes it to the
  backend on demand. NOT implemented today.

The word **"durable"** in this document means **"survives RelayTerm backend
restart"** unless qualified ("durable display-history" = recording bytes
survive a restart; "durable shell" = the interactive shell process survives
a restart, which today requires host-side or agent-side persistence).

## 3. Non-goals

- **No promise of live-shell survival across backend restart in v1.** The
  recording doc § 9 and SPEC's "Detached-session TTL contract" are explicit
  that the `russh::Channel` is unrecoverable across a restart. This
  document does NOT change that contract; Phase 3+ describes how a future
  slice could externalise the shell-bearing process.
- **No SCP/SFTP, file transfer, or shared/collaborative sessions.** Those
  are tracked separately in SPEC's "Out of scope (v1)".
- **No multi-user / cross-user resume.** Persistence is owner-scoped; an
  operator only resumes their own sessions, the same way they only read
  their own recordings today.
- **No input recording, ever, as a side effect of persistence.** Recording
  in v1 captures output bytes only. An opt-in keystroke-audit surface — if
  it ever lands — is its own slice with its own UI warnings (recording
  doc § 4.3). Persistence MUST NOT broaden the redaction surface.
- **No silent host-key trust, ever, as a side effect of persistence.** A
  Phase 3 multiplexer reattach is a fresh SSH transport with a full
  `check_server_key` against the known_hosts vault. Recovery never bypasses
  the pin.
- **No durable storage of secrets.** Decrypted private-key bytes live only
  inside the SSH session task (vault decrypts only into ephemeral memory
  and zeroizes on drop). Persistence MUST NOT add a second copy.
- **No removal of the existing in-memory TTL model.** Phase 1 hardens it;
  later phases ADD durable display reconstruction and (eventually) live
  shell persistence on top.

## 4. User-visible semantics

### 4.1 What should the user expect by disconnect duration

| Disconnect duration | What survives today (in-memory) | What should survive in the staged plan |
|---|---|---|
| **< TTL (default 30 s; tunable up to 24 h)** | Live PTY + replay buffer + cursor state | Same — this is the load-bearing reconnect window. Phase 1 hardens its UX and quotas. |
| **TTL elapsed, < retention window (default 30 days)** | Nothing — the PTY is reaped; session is `closed`; **only** the `closed` row + (if enabled) recording history survive. | After Phase 2: same as today, plus the closed-session replay viewer reproduces what was on screen up to the close. After Phase 3 (host-side multiplexer), a separate `Resume from host multiplexer` action MAY relaunch into a still-live `tmux`/`screen` window on the target host. |
| **Backend restart, regardless of TTL** | Nothing live — `starting`/`active`/`detached` rows are reconciled to `closed`; the live `russh::Channel` is gone forever. Recording bytes that landed before the restart are still readable. | Same. Live-shell survival across a RelayTerm backend restart requires Phase 3 (host-side multiplexer) or Phase 4 (managed agent) — it is **not** delivered by recording. Phase 2 lets the user reconstruct the display state at close. |
| **Days / weeks** | Closed metadata row. Recording (if enabled) until retention sweeps. | Same. Long-term storage is `recording_purged` after `retention_days` (default 30, per-session 64 MiB cap). Live-shell persistence at this scale is host-side or agent-side only. |

### 4.2 Verb semantics (UX copy contract)

The Sessions list and the terminal workspace MUST use these verbs
consistently. The current production copy (`apps/web/src/lib/app/terminal/
sessionStatus.ts`, `SessionsView.svelte`, `ProductionTerminal.svelte`)
already follows this convention; this document pins it.

| Verb | Meaning | Enabled when |
|---|---|---|
| **Open** (a.k.a. **Reconnect**) | Attach to the SAME live PTY. Replay missed output via the in-memory ring if `last_seen_seq` is inside the window; otherwise the renderer resets and resumes from the live cursor. | `status ∈ {active, detached}`. |
| **Start new session** | `POST /api/v1/terminal-sessions` → fresh PTY, new `terminal_sessions` row. | Always, from the server-profile launch UI. |
| **View recording** | Open the read-only `RecordingReplayView`. Streams chunks into a stdin-disabled `XtermRenderer`. NEVER attaches a live wire. | `status != starting` AND the recording metadata gate honestly returns `has_recording = true`. |
| **Resume** (Phase 3+; NOT today) | Attach to a host-side multiplexer window (`tmux`/`screen`) the user previously launched RelayTerm into. Fresh `terminal_sessions` row + fresh `russh::Channel`; the remote shell process is the one that survived. | Phase 3+ only; `status = closed` AND a host-side multiplexer reattach handle has been recorded for the user/profile. Until Phase 3 ships, this verb MUST NOT appear in production UI. |
| **End session** | `POST /api/v1/terminal-sessions/:id/close`. Idempotent. Bypasses the TTL. | `status ∈ {starting, active, detached}` (operator-driven close). |

### 4.3 Wording that MUST be avoided (anti-overclaim)

The current SPEC, the recording doc, and the production UI all say the
same things; persistence work MUST NOT regress this language.

- **Never** say "your session is saved" or "your session is preserved"
  about the in-memory TTL model. The PTY survives only as long as the
  RelayTerm backend keeps running AND the TTL has not elapsed.
- **Never** say "resume your shell" or "reconnect to your shell" about
  the closed-session recording viewer. The viewer plays back display
  history; it is not an interactive shell.
- **Never** say "persistent across restart" or "always available" about
  any v1 surface. A restart drops live PTYs. The only thing that
  durably survives a restart today is `terminal_sessions` metadata,
  `session_events`, `audit_events`, and (if enabled) recording chunks/markers.
- **Never** say "your session reconnects automatically" if the wire
  cannot prove it. The Sessions list explicitly does not pre-supply
  `last_seen_seq` on a navigation-driven reconnect (`SessionsView.svelte`
  lines 297–298, 441–443) — the operator clicks Reconnect; the workspace
  attempts replay; the renderer resets on `replay_window_lost`.

## 5. Architecture options

Each option is scored on: **benefits**, **risks**, **implementation
complexity**, **security implications**, **failure modes**. Where an
option requires breaking a current invariant from
[`SPEC.md`](../SPEC.md), that is called out explicitly.

### Option A — Extend live PTY TTL only, with stricter quotas

Keep everything in-memory; raise the upper bound on
`detached_live_pty_ttl_seconds` (already `5..=86400`); add per-user /
per-deployment quotas to bound resource use; harden the closed/detached
copy in the UI.

- **Benefits.** Smallest possible delta. No schema change, no protocol
  change, no new dependency. The 1800 s long-lived smoke already proved
  the path empirically. Bug-fixable in days.
- **Risks.** Still **not durable across backend restart** — the in-memory
  runtime registry is the same one that evaporates on redeploy. A TTL of
  24 h gives the operator the illusion of persistence without delivering it.
- **Complexity.** Trivial. Most of the work is UX copy + quotas.
- **Security.** No new attack surface; the existing redaction rules
  apply unchanged. Quota work must avoid leaking the offending session
  id across user boundaries (use byte-identical 404 + 429 patterns).
- **Failure modes.** Backend OOM if quotas are too generous (live PTY
  state + replay buffer per session × many users); unbounded reaper
  task accumulation if the TTL knob is misused. Both are deployment-side.
- **Stops short.** Does not solve "backend restart drops everything",
  does not solve "I disconnected my laptop, want to reattach in 3 hours".

### Option B — Durable display reconstruction (recording + optional VT snapshot)

The recording subsystem already lands output bytes durably. Extend it so
that on reconnect-after-restart, the workspace can reconstruct what was
on screen (read-only, no live shell). Add the deferred
`terminal_vt_snapshots` table (recording doc § 5.3) so the renderer can
fast-forward to the nearest snapshot instead of replaying the entire
chunk stream byte-by-byte.

- **Benefits.** Honest about what's possible: the operator opens the
  workspace after a restart and sees the rendered grid the previous
  session ended at. No new architectural primitive; recording already
  proves the schema + retention + redaction story. VT snapshots are
  an OPTIMISATION on top of chunk replay, not a replacement (recording
  doc § 4.4, § 5.3).
- **Risks.** Easy to overclaim. Users may expect display reconstruction
  to imply they can type — must be policed in copy + by stdin-disabling
  the renderer (already enforced on the closed-session viewer).
- **Complexity.** Moderate. VT snapshot writer + reader is one new
  table + one observer task. The display-reconstruction frontend is a
  variant of the existing `RecordingReplayView`. Backend changes ride
  the existing recording slice's redaction harness.
- **Security.** Same as recording today. Recording master key is
  SEPARATE from the SSH-identity vault master key (recording doc § 6.3).
  Display reconstruction NEVER opens a wire to the target.
- **Failure modes.** Recording disabled → no display history → falls
  back to the closed metadata only. Per-session 64 MiB cap → `replay_gap`
  marker → renderer surfaces `replay_window_lost`. Both are pinned today.
- **Stops short.** Still no live shell after restart — the recording
  doc § 9.4 already names this explicitly.

### Option C — Backend-managed host-side multiplexer (`tmux` / `screen` / systemd user units)

The RelayTerm orchestrator launches every shell inside `tmux new-session
-A -s <name>` (or `screen -dRR`) on the target host, records the
multiplexer session name on the `terminal_sessions` row, and on resume
opens a fresh `russh::Channel` that runs `tmux attach -t <name>`
(or `screen -r`). The shell process belongs to the SSH target host, not
RelayTerm; a RelayTerm restart re-attaches over a fresh transport.

- **Benefits.** First option that delivers real live-shell persistence
  across backend restart, network outage, laptop suspend, and even a
  multi-day disconnect — bounded only by what the target host keeps
  alive. Standard sysadmin pattern; users already understand `tmux`.
- **Risks.** Significant new surface. The host must have `tmux`/`screen`
  installed (or a fallback is needed). Multiplexer session names become
  durable state RelayTerm tracks; orphaned multiplexer windows on the
  target host become a host-side resource leak RelayTerm must reason
  about. The shell now persists OUTSIDE RelayTerm's audit boundary —
  any command run inside it is observed only via the recording
  subsystem, NOT via "the live wire was open at the time".
- **Complexity.** High. New per-profile capability flag (multiplexer
  preferred kind: `tmux` / `screen` / none). New backend-side probe
  for multiplexer availability (out-of-band exec on connect; collapse
  to "none" on probe failure). New `terminal_sessions` columns
  (`multiplexer_kind`, `multiplexer_session_name`) under a new
  migration. New `Resume` route + UI verb. New quotas for orphaned
  multiplexer windows per user.
- **Security.**
  - **Host-key boundary unchanged.** Every reattach is a fresh
    `russh::Channel` against the same `server_profile_id`; the
    existing `check_server_key` path runs against the known_hosts
    vault with no relaxation. A revoked or changed host key blocks
    `Resume` exactly as it blocks `Open`.
  - **Auth boundary unchanged.** `Resume` requires
    `AuthenticatedUser` + `CsrfGuard` (write route).
  - **No silent multiplexer write to disk.** The target host's
    `tmux server` is the persistence layer; RelayTerm writes only the
    multiplexer session name (a non-secret string) to its own DB.
    Multiplexer session names MUST NOT be leaked across user
    boundaries; they are owner-scoped on `terminal_sessions`.
  - **No keystroke leak into the multiplexer log path.** `tmux`'s
    own `history-limit` is an in-multiplexer scrollback, NOT a
    keystroke log. The recording subsystem's "output bytes only"
    rule is unaffected.
  - **Audit must cover the new lifecycle verb.** Phase 3 adds a
    `terminal_session_resumed` audit kind (owner-scoped) and emits
    it on every successful `Resume`. Failed resumes that look like
    probing (cross-user 404 burst) follow the same
    `recent_for_actor`-invisible posture as `recording_purged`.
- **Failure modes.**
  - **Multiplexer not installed on target.** `Resume` falls back to
    `Start new session`; the workspace surfaces a typed error
    (`409 conflict { entity: "terminal_session", reason: "multiplexer_unavailable" }`).
  - **Orphaned multiplexer windows.** Without bounds, a user with
    many resumable sessions accumulates many `tmux` windows on the
    target host. Phase 4 owns the operator-side cleanup contract;
    Phase 3 ships with a strict per-(user, profile) cap and a
    "drop oldest" eviction.
  - **Multiplexer kill from the target side.** A target sysadmin can
    `tmux kill-server`; RelayTerm's resume fails with
    `409 conflict { reason: "multiplexer_window_gone" }`. The user
    sees the same recording history they had before.
  - **Race with `Open`.** `Open` (live PTY) and `Resume` (multiplexer
    attach) on the same `terminal_sessions` row are mutually
    exclusive — the manager guards under the same lock that today
    cancels the TTL timer on reattach.

### Option D — RelayTerm remote agent on managed hosts

A RelayTerm-built process (`relayterm-agent`) installed on a target
host. Listens on a localhost socket on the target, owns the shell
lifecycle (`fork()` + PTY allocation), and re-exposes that PTY to the
backend over a fresh authenticated channel. Functionally a self-built
multiplexer with auth and audit primitives the backend already speaks.

- **Benefits.** Tightest end-to-end control. Same auth model as the
  rest of RelayTerm (the agent can speak the same `RTB1` envelope and
  reuse `AuthenticatedUser` semantics). Audit and redaction primitives
  are RelayTerm's, not `tmux`'s. Works on hosts where installing
  `tmux`/`screen` is policy-forbidden.
- **Risks.** Largest new surface by far. Installing software on every
  target is a deployment policy decision per operator. The agent is
  a new long-lived process under RelayTerm's care, with its own
  CVE / upgrade / supervision cycle. Cross-platform support (Linux,
  macOS, BSDs) becomes RelayTerm's burden.
- **Complexity.** Very high. A new crate, a new release artefact, a
  new install path, a new auth handshake, a new test matrix, a new
  threat model. None of the existing redaction or audit harnesses
  cover the agent — they have to be rebuilt for it.
- **Security.** Hardest scope to keep narrow. The agent runs as the
  user on the target; a vulnerability in the agent is a local-privilege
  surface RelayTerm did not previously own. The auth handshake between
  backend and agent is a new credential boundary (cannot reuse the
  user-facing session cookie). On the upside: every audit event has
  a typed source (`agent`, version, host fingerprint) and the agent
  can enforce per-command policy at the source rather than after the
  fact.
- **Failure modes.** Agent crash → shell dies (back to host-side
  recovery story, which RelayTerm now owns). Agent upgrade required
  → operator interaction across the fleet. Agent CVE → coordinated
  patch + per-host audit. Each is operationally heavier than `tmux`.

### Option E — Hybrid staged approach (RECOMMENDED, see § 7)

Combine A (tighten TTL + UX + quotas), B (durable display
reconstruction with optional VT snapshot), and C (host-side
multiplexer for live shell persistence) in that order. Option D
remains future work and is NOT recommended for v1 — the install
surface alone is bigger than the entire current backend.

## 6. Comparison table

The same dimensions as § 5, flattened into one view.

| Dimension | A · Tune TTL + quotas | B · Recording + VT snapshot | C · `tmux`/`screen` | D · RelayTerm agent | E · A→B→C (recommended) |
|---|---|---|---|---|---|
| Live shell survives backend restart | ✗ | ✗ | ✓ | ✓ | After C ✓ |
| Display history survives backend restart | ✗ | ✓ | ✓ (via B) | ✓ (via B) | After B ✓ |
| New schema / migration | none | 1 new table (`terminal_vt_snapshots`) | 2 columns on `terminal_sessions` | many | additive per phase |
| New protocol shape | none | optional `Snapshot` marker on the existing recording wire | new `Resume` verb; new `409 multiplexer_unavailable` code | new agent handshake | per phase |
| Host-side dependency | none | none | `tmux` OR `screen` (probed) | new agent binary | only Phase 3+ |
| Security boundary expansion | TTL knob + quotas | recording master key (already separate) | multiplexer audit kind; ZERO host-key/auth relaxation | new agent credential boundary | minimal per phase |
| Operator runbook impact | minor | minor | new (probe, cleanup, quota) | major (install/upgrade fleet) | additive |
| Reversibility if approach is wrong | trivial | moderate | high (drop the verb, keep schema) | low | high per phase |
| Realistic v1 ship distance | 1 slice | 2–3 slices | 4–6 slices | 10+ slices | A: 1, B: 2–3, C: 4–6 |

## 7. Recommendation

**Adopt Option E.** Take the staged path A → B → C, gated by smoke
sign-off between phases. Defer D unless an operator explicitly asks for
a managed-host fleet (and then re-design — D is not a Phase 5).

Reasoning:

1. **Be honest about what we ship.** The recording doc § 9.4 already
   says recording does not deliver live PTY recovery across a restart;
   we will not pretend otherwise. Phase 2 gives users
   reconstruction-of-display without claiming a live shell.
2. **Live-shell persistence is fundamentally host-side.** The shell
   process belongs to the SSH target, not to RelayTerm. The cleanest
   way to keep it alive across a RelayTerm restart is to host it in
   `tmux`/`screen`, which sysadmins already use for the same reason.
3. **Each phase has a small, testable delta.** A is a UX/quotas slice.
   B is a snapshot-table slice on top of recording. C is a multiplexer
   integration slice with a probe + a new verb. None requires a
   backend-wide rewrite.
4. **Each phase preserves every load-bearing invariant from
   [`SPEC.md`](../SPEC.md):** backend-owned session, no client-held
   private keys, server-side host-key verification, output-only
   recording, owner-scoped reads, byte-identical 404 collapse,
   field-by-field audit payloads.
5. **Each phase is reversible.** If C turns out to be the wrong shape
   (e.g. operators want only managed agents), the multiplexer feature
   can be feature-flagged off without dropping B. If B turns out to be
   too expensive (VT snapshot storage), the snapshot writer can be
   disabled without touching A or C.

## 8. Staged implementation plan

Each phase below is a self-contained slice plan, NOT a slice. Each gets
its own design doc and code-review round when its time comes.

### Phase 1 — Tighten the current TTL model (smallest safe next slice)

**Goal.** Make the current in-memory TTL model honest, observable, and
bounded by quotas.

> **Phase 1A landed (2026-05-11):** `GET /api/v1/config/session-policy`
> exposes the effective `detached_live_pty_ttl_seconds`; the SPA renders
> honest parameterised copy via `describeDetachedTtl`. Phase 1B is the
> quota model on top of the wire-observable TTL — full design in
> [`docs/session-quotas.md`](session-quotas.md). The quota slices land as
> 1B.1 (per-user live ceiling — landed 2026-05-11) → 1B.2a (per-user
> starting-burst ceiling — landed 2026-05-11) → 1B.2b (deployment-wide
> live ceiling — design refined, NOT landed) → 1B.2c (operator dashboard
> tile — deferred until 1B.2b is observed in staging) → 1B.3 (optional
> production-default tuning). Zero schema, zero migration, zero new audit
> kinds; design is deliberately conservative so a later Phase 2 / 3 can
> build on it.

**In scope.**

- Inventory of every UX string that names the TTL window or
  in-memory replay; one pass to align them on
  [`docs/spec/terminal.md`](spec/terminal.md) → "Detached-session TTL
  contract" wording (esp. the `~30s` / `~Ns` literal where
  `DETACHED_LIVE_PTY_TTL` is dynamic).
- Per-user / per-deployment caps on:
  - max simultaneous `detached` sessions per user;
  - max `detached_live_pty_ttl_seconds` exposed to operators (today
    `5..=86400`; consider tightening per-deployment in production
    config without changing the env-var bound);
  - max simultaneous `starting` rows per user (stale-row burst
    protection, paired with a backend-side prune that DOES NOT delete
    `terminal_sessions` rows — it transitions them to `closed` via the
    existing close path, matching the SPEC's "never delete from user UI"
    rule).
- A small operator dashboard (Settings → Sessions or Dashboard) that
  surfaces the active count, the cap, and the configured TTL.

**Out of scope.** Recording changes. Schema changes (the new caps are
config + in-memory). Any verb beyond `Open` / `End session`.

**Smoke.** Extend the 2026-05-10 long-TTL smoke recipe to cover the
new cap-rejection path (`429 too_many_sessions` envelope; wire body
carries `code` + static `message` only — no user / session id leaked).

**Deliverables.**

1. Design doc: [`docs/session-quotas.md`](session-quotas.md).
2. Config additions in `apps/backend/src/config.rs` (typed,
   `RELAYTERM_TERMINAL_SESSIONS__*` env mirrors; validation envelope
   matches the existing `terminal_recording.cleanup` pattern).
3. Plumbing into all THREE Compose templates + both TOML examples +
   the `scripts/check-doc-contracts.sh` matrix (lesson from 2026-05-09).
4. Audit kinds: NONE new in Phase 1 (cap rejection is operational, not
   security-relevant; matches the existing detach-TTL path).

### Phase 2 — Durable display reconstruction (recording + VT snapshot)

**Goal.** After a backend restart (or after `Close`), the user can open
a session and see the rendered grid the previous session ended at, NOT
a blank screen. Read-only.

**In scope.**

- Land `terminal_vt_snapshots` per recording doc § 5.3. The
  authoritative column set is defined there; this design doc does NOT
  re-define the schema. The load-bearing columns are
  `terminal_session_id` (FK `ON DELETE RESTRICT`), `seq` (the chunk seq
  AT which the snapshot was taken — the next chunk to apply has
  `seq_start = seq + 1`), `cols` / `rows`, `snapshot_blob` (opaque
  renderer-neutral bytes; format owned by the libghostty-vt crate),
  `format_version`, `byte_len`, `encryption`, `created_at`. Same
  redaction rules as `terminal_recording_chunks.payload`:
  `snapshot_blob` is `Debug`-redacted to `byte_len` only and never
  appears in logs, errors, audit, or any non-recording wire surface.
  Phase 2 ships `encryption = 'none'` only; the encryption-required
  writer rides the same future slice as chunk encryption (recording
  doc § 6.3).
- VT snapshot writer (recording doc step 7). Lives in the
  orchestrator. Emits a snapshot every N chunks OR every M output
  bytes (whichever first); bounded; drop-on-overflow; never blocks
  the live wire. Snapshot is the libghostty-vt parser's serialised
  grid state.
- Owner-scoped read API:
  `GET /api/v1/terminal-sessions/:id/recording/snapshots?from_seq=&limit=`.
  The wire response shape mirrors the per-column set above; concrete
  field names are decided at Phase 2 implementation review against the
  recording doc § 10 pattern, but in any case the snapshot bytes cross
  the wire as `snapshot_b64` only (mirroring the `data_b64` rule for
  chunks — never as raw bytes, never decoded server-side into a
  structural form). `cols`, `rows`, `seq`, `format_version`,
  `byte_len`, `encryption`, and `created_at` are public metadata.
  Foreign / unknown → byte-identical 404. Empty → `200 []`.
- Replay-viewer enhancement: on open, fetch the nearest snapshot
  with `seq <= last_chunk_seq`, apply it to the renderer, then play
  chunks past it. No new verbs; the existing `View recording`
  affordance benefits transparently.
- Retention sweep extends to snapshot rows (same `purge_for_retention`
  primitive; one extra `DELETE FROM terminal_vt_snapshots WHERE ...`
  inside the same transaction; `recording_purged` audit row's payload
  gains a `snapshot_count` field; `bytes_purged` aggregates snapshot
  byte_len too).

**Out of scope.** Any live-shell change. The viewer remains
stdin-disabled. Encryption-required mode for snapshots rides the same
encryption-required slice as chunks (recording doc § 6.3) — Phase 2
ships `encryption.mode = disabled` only, matching the current chunk
writer.

**Smoke.** Re-record the closed-session smoke entries with snapshots
enabled (extend
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)).
Verify: snapshot write under load doesn't block the live wire; replay
viewer applies snapshot then replays past it; retention sweep deletes
snapshot rows alongside chunks; `recording_purged` payload contains
`snapshot_count`.

**Deliverables.**

1. Design doc: `docs/persistent-sessions-phase2-snapshots.md` (or a
   new section in `docs/terminal-recording.md` § 5.3 / § 9.4 that
   promotes the deferred design to landing).
2. Migration: `<timestamp>_terminal_vt_snapshots.sql` +
   `cargo sqlx prepare --workspace`.
3. Repository surface + redaction tests + audit-payload field
   addition (`snapshot_count` on `recording_purged`).
4. Frontend: `RecordingReplayView.svelte` snapshot-aware bootstrap;
   no new verb.

### Phase 3 — Host-side multiplexer prototype (live shell persistence on throwaway hosts)

**Goal.** On opt-in profiles, a RelayTerm-launched shell survives a
backend restart because it runs inside a `tmux` (or `screen`) window
on the target host. The operator's `Resume` opens a fresh
`russh::Channel` that reattaches the multiplexer.

**In scope.**

- New `server_profiles` columns: `multiplexer_kind` (enum:
  `none | tmux | screen`; default `none`), `multiplexer_session_pattern`
  (string template, default `relayterm-<terminal_session_id>`). Both
  are operator opt-ins per profile; default behaviour is unchanged
  from today.
- Probe phase. On profile create OR on every fresh launch (whichever
  is cheaper), the backend runs an out-of-band `tmux -V` /
  `screen -v` exec over a one-shot SSH channel (NOT the PTY). Probe
  result is cached on the profile; failure collapses the profile to
  `multiplexer_kind = none` with a typed UI hint
  ("`tmux` not available on this host; sessions remain in-memory only").
- Launch path. When `multiplexer_kind != none` AND the probe
  succeeded, the orchestrator runs the user's shell inside
  `tmux new-session -A -s <session_name>` (or `screen -dRR -S <name>`)
  instead of bare. The multiplexer session name is recorded on the
  `terminal_sessions` row.
- New verb. `POST /api/v1/terminal-sessions/:id/resume` opens a fresh
  `russh::Channel` against the same profile, runs
  `tmux attach -t <name>` (or `screen -r`), and reattaches the
  WebSocket bridge as if it were a new live PTY. The `terminal_sessions`
  row transitions back from `closed` to `active`; one
  `session_events { kind: resumed }` row is appended; one
  `audit_events { kind: terminal_session_resumed, actor_id = caller,
  payload = { target_id, target_kind: "terminal_session", multiplexer_kind } }`
  row is appended.
- Quotas. Per-(user, profile) cap on resumable multiplexer windows.
  Exceeding the cap evicts oldest-first and emits a typed UI hint.
  Eviction is "drop the multiplexer window on the target host AND
  transition the `terminal_sessions` row to `closed` via the normal
  close path" — no new schema state.
- Failure paths. `Resume` against a vanished multiplexer window
  surfaces `409 conflict { entity: "terminal_session",
  reason: "multiplexer_window_gone" }`. `Resume` against a profile
  whose probe has expired re-runs the probe; if it now reports
  unavailable, the row collapses to `closed` with a single
  `session_events { kind: resume_failed, payload: { reason: "multiplexer_unavailable" } }`
  row.
- UX. The Sessions list gains a `Resume` action on `closed` rows
  whose `terminal_sessions.multiplexer_session_name` is set AND
  whose profile still reports `multiplexer_kind != none`. The action
  is gated by an explicit confirmation dialog ("This will re-attach
  to a `tmux` window the previous session left running on the target
  host. The host-key pin still applies.") so the user understands
  the persistence layer.

**Out of scope.** Managed agents (Option D). Cross-profile resume
(the multiplexer name lives on the profile + session, never crosses
profiles). Operator UI for cleaning up arbitrary multiplexer windows
(Phase 4 territory).

**Smoke.** A throwaway VPS host with `tmux` installed. Launch a
session in a multiplexer profile, leave a `sleep 600` running, restart
the RelayTerm backend, resume from the Sessions list, observe the
`sleep` still counting. Tear down. Then repeat the same with the
target host's `tmux` killed mid-flight to confirm the
`multiplexer_window_gone` path.

**Deliverables.**

1. Design doc: `docs/persistent-sessions-phase3-multiplexer.md`.
2. Migration: `<timestamp>_server_profiles_multiplexer.sql` +
   `terminal_sessions.multiplexer_session_name` column +
   `cargo sqlx prepare --workspace`.
3. Audit kind: `terminal_session_resumed` (and migration to
   `audit_events_kind_chk`; matching `AuditEventKind` variant).
4. Frontend: `Resume` verb + confirmation dialog + Sessions-list
   gating + ServersView profile-edit field (multiplexer kind).
5. Operator runbook: extend
   [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
   with the throwaway-host smoke recipe.

### Phase 4 — Production-harden multiplexer + admin controls

**Goal.** Make Phase 3 safe to roll out to a self-hosted operator who
is not personally babysitting the target hosts.

**In scope.**

- Per-user / per-deployment quotas surfaced as both config
  (`RELAYTERM_TERMINAL_SESSIONS__MULTIPLEXER__*`) and operator dashboard
  reads. Eviction policy documented; eviction emits
  `terminal_session_multiplexer_evicted` audit kind.
- Backend-driven cleanup of orphaned multiplexer windows that no
  `terminal_sessions` row knows about (this only happens if the DB
  was rolled back or the user manually `tmux` 'd on the host). Sweep
  runs on a schedule similar to `recording_retention` (advisory-locked,
  fail-soft) and emits `terminal_session_multiplexer_orphan_swept`
  with `actor_id = NULL`.
- Operator surfaces for hard-revoke / kill of a specific multiplexer
  window without the user's involvement. This is admin-only / future
  work in the same posture as known-host unrevoke
  ([`SPEC.md`](../SPEC.md) → Known-host revocation policy).
- Multi-instance / staging clarity: the multiplexer session-name
  template includes the RelayTerm deployment id so two RelayTerm
  instances pointing at the same SSH target cannot stomp each
  other's windows. Pin the template format in
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md).

**Out of scope.** Managed agents (Option D). Any change to the
host-key vault or its policy.

**Smoke.** Two RelayTerm instances pointing at the same throwaway
SSH host with distinct deployment ids. Launch sessions on both;
confirm multiplexer windows do not collide. Restart instance A; pull
the rug on instance B (`docker compose down -v`); confirm instance A
can still resume its own windows AND that the orphan sweep eventually
reaps instance B's leftover windows on the target host without
touching instance A's.

## 9. Data model / API implications

Each phase's schema delta in one place so a future review can spot
which migrations belong to which phase.

### 9.1 `terminal_sessions` status model

Today: `starting | active | detached | closed`. The set does NOT
expand. Phase 3 adds a transition `closed → active` via the new
`Resume` route; this is explicitly NOT a new status. Disabled or
otherwise unsafe profiles continue to refuse new launches via the
existing `409 conflict { entity: "server_profile", reason: "disabled" }`
path; `Resume` inherits the same gate.

### 9.2 New columns / tables by phase

| Phase | Schema delta |
|---|---|
| 1 | None. New caps live in config + in-memory. |
| 2 | New table `terminal_vt_snapshots` per recording doc § 5.3 (authoritative column set lives there; the load-bearing payload column is named `snapshot_blob`, not `payload`). Same shape rules as `terminal_recording_chunks`: `snapshot_blob` opaque + `Debug` redacted to `byte_len` only, FK `ON DELETE RESTRICT`, owner-scoping at the API layer only. |
| 3 | `server_profiles.multiplexer_kind` enum (`none | tmux | screen`), `server_profiles.multiplexer_session_pattern` text, `server_profiles.multiplexer_probe_at` timestamptz, `terminal_sessions.multiplexer_session_name` text nullable. `audit_events_kind_chk` extends with `terminal_session_resumed`. |
| 4 | `audit_events_kind_chk` extends with `terminal_session_multiplexer_evicted`, `terminal_session_multiplexer_orphan_swept`. No new columns. |

### 9.3 `session_events` additions

`session_events.kind` already carries `created`, `attached`,
`detached`, `reattached`, `resized`, `replay_started`,
`replay_completed`, `closed`. Phase 3 adds:

- `resumed` — payload `{ multiplexer_kind, multiplexer_session_name }`. **Note** (acknowledged for Phase 3 review): this is the first `session_events` payload that carries a free-form host-side string (the multiplexer window label). The value is owner-scoped via `terminal_sessions.owner_id`, is a non-secret operational label by construction (it is the string the target host's `tmux server` uses to address the window, not a credential), and is never copied into `audit_events.payload` or any log. The Phase 3 design slice MUST re-confirm this choice against the redaction-rules backstop.
- `resume_failed` — payload `{ reason: "multiplexer_window_gone" | "multiplexer_unavailable" | "host_key_changed" | "auth_failed" }`.
  Reason codes are wire-stable; new variants append, never renumber.

### 9.4 `audit_events` additions

Phase 3: `terminal_session_resumed` (actor_id = caller, payload =
`{ target_id, target_kind: "terminal_session", multiplexer_kind }`).
Phase 4: `terminal_session_multiplexer_evicted` (actor_id = caller for
quota-driven eviction; actor_id = NULL for system-driven cleanup) and
`terminal_session_multiplexer_orphan_swept` (actor_id = NULL, payload
public-only). All new kinds extend `audit_events_kind_chk` via a
paired migration; the matching `AuditEventKind` Rust variant lands
with the migration.

### 9.5 Runtime registry / orchestrator

The in-memory runtime registry in `TerminalSessionManager` stays the
single source of truth for live PTYs. Phase 3 adds a per-session
"resumable from multiplexer" flag that lives ONLY on the
`terminal_sessions` row (`multiplexer_session_name IS NOT NULL`),
NOT on the registry. The registry is rebuilt on every backend boot;
the new flag survives because it's in Postgres.

### 9.6 Multi-instance / staging

Phase 4 pins the multiplexer session-name template to include a
deployment id (operator-supplied via config; defaults to a hash of
`(database_url, public_origin)` so two `docker compose up` deployments
pointing at the same SSH target do not collide). Pre-Phase-4
deployments MUST NOT enable multiplexer kinds against a shared SSH
target — the runbook documents this explicitly.

## 10. Security boundaries

Each boundary below is a load-bearing rule. Persistence work MUST NOT
weaken any of them. Each links to the canonical contract; this
document does NOT re-state the contracts in full.

1. **No client-held private keys.** Unchanged across all phases.
   `apps/web` and the Tauri shells never see `private_key` /
   `encrypted_private_key`. ([`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 14.)
2. **No terminal input recording by default.** Unchanged. Phase 2
   adds VT snapshots, which are POST-parse renderer state — they do
   NOT contain input bytes by construction. ([`docs/terminal-recording.md`](terminal-recording.md) § 4.3.)
3. **No terminal payload in audit / logs / errors / wire bodies.**
   Unchanged. New audit kinds in Phase 3 / 4 carry the same field-by-field
   public-metadata-only payload shape as `recording_purged`.
   ([`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 1, § 11, § 12.)
4. **No silent host-key overwrite.** Unchanged. Phase 3 `Resume` runs
   the full `check_server_key` against the known_hosts vault on
   every fresh `russh::Channel`; a mismatch surfaces
   `host_key_changed` exactly as today's launch path does, with a
   matching `audit_events { kind: host_key_mismatch }` row.
   ([`SPEC.md`](../SPEC.md) → Host-key change behavior contract.)
5. **No session-token / cookie leakage beyond existing safe
   metadata.** Unchanged. The new `Resume` route uses the same
   `AuthenticatedUser` + `CsrfGuard` extractors as every other
   browser-write route. ([`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 4 — § 9.)
6. **Host-side `tmux`/`screen` MUST NOT bypass host-key, auth, or
   audit boundaries.** Phase 3 puts the multiplexer process inside
   the user's SSH session — RelayTerm's auth boundary is the same
   russh boundary it always was. The multiplexer is a target-host
   userland process; its scrollback is a target-host concern. The
   recording subsystem captures the bytes the wire delivered, which
   is what RelayTerm has always recorded.
7. **Quotas MUST prevent runaway detached shells.** Phase 1 introduces
   per-user / per-deployment caps on simultaneous `detached` rows.
   Phase 3 extends the same accounting to multiplexer windows. Both
   rejections are owner-scoped 429s with static wire bodies; neither
   leaks the offending session id across user boundaries.
8. **Probe failures collapse to a typed UI hint, not an information
   leak.** Phase 3 probe failures (`tmux -V` returning non-zero) MUST
   NOT echo the target host's stderr or banner. The probe wraps the
   exec result and returns `unavailable` to the caller; the typed UI
   hint is static. ([`docs/agent/redaction-rules.md`](agent/redaction-rules.md) § 11 sets the precedent.)
9. **No new path lets a user write to another user's multiplexer
   window.** Phase 3 / 4: multiplexer session names are owner-scoped
   via `terminal_sessions.owner_id`; `Resume` requires the caller's
   `user.user_id()` to match. Foreign-owned ids collapse to
   byte-identical 404 BEFORE the multiplexer probe runs.

## 11. UX copy requirements

The strings below are the load-bearing copy that the staged plan will
write. They are normative: an implementation slice that uses different
wording MUST update this section first. Every string is pinned with a
sentinel-string test the same way the auth-check status strings and
the paste-policy strings are pinned today.

### 11.1 Detached session (Phase 1 — parameterise the literal TTL)

Today's production copy lives in three places and uses a hardcoded
`~30s` literal (matching the type-level
`DETACHED_LIVE_PTY_TTL = Duration::from_secs(30)`):

- `apps/web/src/lib/app/terminal/sessionStatus.ts` line 57:
  > "No client is attached. The remote PTY only survives briefly
  > (~30s) after the last detach — reconnect within that window or
  > the session is reaped. Replay is in-memory and not durable
  > across a backend restart."
- `apps/web/src/lib/app/views/SessionsView.svelte` lines 441–443
  ("Detached. The remote PTY remains alive only briefly (~30s) — …").
- `apps/web/src/lib/app/terminal/ProductionTerminal.svelte` lines
  646–649 (same shape, dynamic `~{Math.round(DETACHED_TTL_MS / 1000)}s`
  via the dev-lab constant).

The honesty failure today is the literal `30s` in the first two
files — when a deployment configures the env var to e.g. `1800 s`,
the copy lies by a factor of 60. Phase 1's UX work parameterises
the literal on the wire-observable value. Open question: the
backend's exact remaining TTL is NOT on the wire today (the dev-lab
copy explicitly labels its countdown `approximate (local clock)`).
The Phase 1 design slice MUST resolve whether to put the configured
TTL on the wire as part of the `SessionAttached` envelope, or to
keep the copy approximate and re-label it (`"only survives briefly
(typically ~30 s; up to a few hours in some deployments)"`). The
target copy after Phase 1 lands MUST satisfy the same anti-overclaim
register as § 11.7 below.

### 11.2 Closed session, recording available (Phase 2 alignment)

> "This session is closed and cannot be reconnected. The recording
> below replays what was on screen up to the close. The recording is
> read-only — typing is disabled."

Pinned in tests against `RecordingReplayView.svelte`.

### 11.3 Closed session, no recording (today's copy)

Unchanged:

> "This session is closed and cannot be reconnected. Launch a new
> session from the originating profile."

### 11.4 Resume from multiplexer (Phase 3, NEW)

The verb appears only after Phase 3 lands. The button copy is
`Resume`; the confirmation-dialog body MUST be:

> "Resume re-attaches to a `tmux` window the previous session left
> running on `<host display name>`. The host-key pin still applies
> — if the host's key has changed, the resume will refuse. Existing
> recordings of this session remain available."

Substitute `screen` when the profile's `multiplexer_kind = screen`.
The host display name is the same string the launch UI shows; never
the raw `hostname:port` and never the SSH banner.

### 11.5 Multiplexer unavailable on profile (Phase 3, NEW)

> "This profile cannot keep your session running across a backend
> restart — `tmux` or `screen` is not installed on the target host.
> The session will use the in-memory reconnect window (~{TTL}s) only."

### 11.6 Multiplexer window gone (Phase 3, NEW)

> "The `tmux` window for this session is no longer on the target
> host. Existing recordings remain available; start a new session
> from the originating profile to keep working."

### 11.7 Wording NEVER to use (anti-overclaim register)

Pinned in tests as forbidden substrings on the production UI. The
canonical home is `apps/web/tests/sessionStatus.test.ts` (the existing
detached-copy honesty checks already live there; new entries extend
the same harness). A test in that file MUST assert that none of the
forbidden substrings appears in any of the four UX-copy sources named
in § 11.1, § 11.2, § 11.4, § 11.5, § 11.6 — same shape as the
existing `AUDIT_FORBIDDEN_SUBSTRINGS` harness in
`crates/relayterm-api/tests/api.rs`.

Forbidden substrings (case-insensitive):

- "your session is saved";
- "always available";
- "your shell will resume automatically";
- "persistent across restart" (without an immediate qualification);
- "session recovery";
- "your work is preserved" (recording captures display, not "work").

The list does NOT duplicate the credential / token / vault sentinels
in `AUDIT_FORBIDDEN_SUBSTRINGS` and `apps/web/tests/auditApi.test.ts`'s
`FORBIDDEN` list — those harnesses keep enforcing their domain;
this list adds the persistence-overclaim domain on top.

## 12. Smoke / verification plan

| Phase | Smoke recipe |
|---|---|
| 1 | Reuse the 2026-05-10 long-TTL smoke recipe; add a cap-rejection step (`429 too_many_sessions`); pin wire-body redaction with the existing `AUDIT_FORBIDDEN_SUBSTRINGS` harness. |
| 2 | Extend the 2026-05-10 closed-session-reconnect smoke: enable recording, launch a session, run a TUI (e.g. `htop`), close the session, restart the backend, open the recording viewer, confirm the renderer shows the htop grid at the close instant. Verify retention sweep deletes snapshot rows alongside chunks. |
| 3 | Throwaway VPS host with `tmux` installed. Launch in a multiplexer profile, leave `sleep 600` running, restart the backend, resume, observe `sleep` still counting. Kill `tmux server` on the target; confirm `multiplexer_window_gone` UI. |
| 4 | Two RelayTerm instances pointing at the same SSH target with distinct deployment ids. Confirm no collision. Pull the rug on instance B; confirm instance A's `Resume` works AND the orphan sweep eventually reaps instance B's windows. |

Every smoke entry follows the existing
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
contract: throwaway inventory, no real-secret reuse, post-run cleanup,
matrix-style log of every observed `session_events` and `audit_events`
row, redaction sentinel sweep.

## 13. Open questions

Each is an explicit ambiguity for the owner to resolve before the
matching phase's slice can start.

1. **TTL upper bound in production.** The env-var bound is `5..=86400`
   (24 h). Phase 1 should set a tighter production default upper bound
   (e.g. 4 h) in `docs/config-examples/relayterm.production.example.toml`
   while leaving the type-level bound at 24 h. The right value depends
   on the operator's tolerance for a single tab's PTY lingering after
   the user closed their laptop.
2. **Per-user `detached` cap in production.** A reasonable starting
   point is `min(8, max_sessions_per_user / 2)` but the right value
   depends on observed usage. Phase 1 ships the knob with a
   conservative default and a dashboard surface.
3. **VT snapshot cadence.** Phase 2 ships with `every N chunks OR every
   M output bytes`. Default values need to balance "snapshot replay
   gives a near-instant reconstruction" against "snapshot storage
   compounds on top of the existing 64 MiB per-session cap". Tentative
   defaults: snapshot every 64 chunks OR every 1 MiB; revisit after
   Phase 2 smoke.
4. **Multiplexer probe TTL.** Phase 3's probe result is cached on the
   profile. Cache TTL — 1 hour? 24 hours? Forever, invalidated on
   profile edit? A long TTL means a host that uninstalls `tmux` keeps
   launching into a doomed multiplexer flag until the next probe; a
   short TTL means more out-of-band exec round-trips. Default tentative:
   24 h, invalidate on profile edit.
5. **Naming.** Is the new verb `Resume` correct? `Reattach`, `Continue`,
   and `Reopen` are all candidates. `Resume` reads well next to `Open` /
   `End session` and is the standard `tmux` verb. Locked in this document
   unless Phase 3 review surfaces a better option.
6. **What happens when a user disables multiplexer kind on a profile
   that has live multiplexer windows?** Phase 3 default proposal: the
   existing `Resume` buttons disappear from the UI; the orphan sweep
   in Phase 4 eventually reaps the windows; until then they are
   target-host-side state RelayTerm acknowledges but cannot reattach.
   Confirm at Phase 3 design review.
7. **Cross-profile recording portability.** Today recordings live with
   their `terminal_sessions` row, which lives with its profile. If a
   profile is deleted (Phase 5+ destructive policy), the recording rows
   die with the session row (FK `RESTRICT`; today the session row
   cannot be deleted from the user UI either). This document does NOT
   change the policy. Open question: should the operator have an
   "export this recording" path before profile delete becomes
   surfaced? Defer to the destructive-action policy slice.

---

End of design document. No implementation work follows from this
document directly; each phase opens its own design slice and review.
