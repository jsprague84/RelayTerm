# RelayTerm v1 production-readiness cutline

> A concrete definition of what RelayTerm needs to be "first deployable
> for a single self-hosted operator," what is already there, what truly
> blocks v1, and what is explicitly post-v1. This is a planning
> document — no source, schema, deploy, or CI changes accompany it.
>
> **Status as of 2026-05-17:** drafted on
> `docs/v1-production-readiness-cutline` against the snapshot of `main`
> at commit `1813552` ("docs(testing): record Android multi-run launch
> timing resmoke"). Updated 2026-05-17 on
> `docs/inventory-edit-delete-ui-staging-smoke` to record the
> operator-visible UI walk that resolves B1 — see § 5 row B1 and the
> 2026-05-17 entry in
> [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md).
>
> This doc supersedes nothing. It does NOT redefine the architectural
> invariants in `AGENTS.md` or the data / behavior contracts in
> `SPEC.md`; it composes them into a release cutline. Where the cutline
> and the spec disagree, the spec wins and this doc is the bug.

## 1. Purpose — what "v1 production deployable" means

RelayTerm v1 is the first version that an operator can deploy on a
single host they own and use as their personal SSH terminal. Concretely:

- **Single self-hosted operator.** One person (the bootstrap user)
  installing the published images on a VPS or homelab box they
  administer. Multi-user / admin / RBAC is explicitly out of scope.
- **Secure enough for personal / VPS use.** The production auth
  envelope is required (cookie `Secure`, `SameSite=Strict`,
  `HttpOnly`; CSRF `Origin` allow-list; Argon2id-OWASP_2023 password
  hashing; opaque server-side sessions; vault-encrypted SSH private
  keys at rest). v1 does not claim resistance to dedicated attackers
  with code-execution on the host.
- **Stable default xterm terminal access.** xterm.js is the production
  default renderer on every surface. Experimental renderers
  (`ghostty-web`, `wterm`, `restty`) remain dev-only and reach the
  production shell only through the gated lazy loader. v1 does NOT
  flip any renderer default.
- **Basic host / profile / credential management.** Create / list /
  read / detail / lifecycle for hosts, server profiles, and SSH
  identities. Generate or import (Ed25519 only) an SSH identity.
  Preflight + trust a host key. Auth-check a profile. Launch a live
  terminal. List + close sessions.
- **Documented install / upgrade / rollback.** The operator can do a
  fresh install, walk a post-deploy smoke, upgrade by image tag, roll
  back, take a backup, restore from one, and rotate the secrets they
  are expected to rotate — all from a single runbook.
- **No claim that experimental renderers are production-default
  ready.** Renderer evaluation continues post-v1. v1 ships xterm.

This cutline is calibrated for "the operator can rely on this for their
own daily SSH work" — not for production traffic in a paid service.

## 2. V1 non-goals (explicitly deferred)

Every item below is real future work that this cutline deliberately
does NOT block on. Each is captured in the relevant SPEC or design
doc; the rationale lives there. The cutline's job is to keep them off
the critical path.

- **Renderer promotion / default flip.** No flip of the production
  default away from xterm. The evaluation lane in
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  and the scorecard in
  [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  continue independently.
- **Production renderer selector UI.** Operator-selectable renderer
  in production is post-v1. The dev lab keeps the swap mechanism;
  production stays xterm-only.
- **Full mobile app polish.** The SPA loads and is usable on mobile
  Chrome (the existing `mobileNavOpen` drawer is wired and the
  production-shell terminal mounts), but a deliberate mobile-portrait
  polish pass is post-v1. See §5 row "Mobile portrait usability."
- **Tauri desktop / mobile release packaging.** The shells exist as
  Tauri v2 scaffolds under `apps/desktop/` and `apps/mobile/` (NOT
  stubs — real `src-tauri/`, `gen/`, `capabilities/`). Local build
  commands are documented in
  [`docs/deployment/tauri-local-build.md`](deployment/tauri-local-build.md).
  CI workflows, code signing, Play Store / App Store bundling, and a
  formal release artifact track stay deferred per
  [`docs/deployment/tauri-ci-release-plan.md`](deployment/tauri-ci-release-plan.md).
  v1 is the web SPA behind a reverse proxy.
- **iOS Tauri shell.** Android-first; iOS later.
- **Multi-user / admin / RBAC.** Single-tenant by design. See
  [`docs/spec/auth.md`](spec/auth.md) "Passkey/WebAuthn stance" for
  the rationale.
- **Passkeys / WebAuthn.** Forward-compatible with the v1 session
  shape; deferred.
- **Email password reset / "forgot password."** Self-hosted operators
  have DB-level recovery via
  [`docs/production-auth.md`](production-auth.md) §8. Mail transport
  is its own scope.
- **`tmux` / `screen`-style host-side persistent shells.** The
  in-memory `DETACHED_LIVE_PTY_TTL` reconnect window (default 30s,
  operator-tunable) is the v1 reconnect surface. The longer roadmap
  is [`docs/persistent-sessions.md`](persistent-sessions.md).
- **Recording UI toggle / export / search.** Recording infrastructure
  is landed end-to-end (writer, durable read API, replay viewer,
  retention worker, audit) but is OFF by default and toggled by
  config + restart. An in-product enable / disable / export / search
  UI is post-v1. See
  [`docs/terminal-recording.md`](terminal-recording.md).
- **Performance / renderer benchmark automation.** No throughput /
  reflow / memory harness in v1.
- **Restty viability decision.** Restty needs broader CSP / font /
  WebGPU choices before it can even be matrix-evaluated. Deferred per
  scorecard §7.
- **Production CSP widening for experimental renderers.** Production
  CSP stays strict. The staging-only `'wasm-unsafe-eval'` relaxation
  is NOT extended to production in v1.
- **Multi-instance / HA / Kubernetes deploy.** Single-instance Docker
  Compose only. The in-memory login throttler and the in-memory
  session orchestrator are single-instance-correct; coordinating them
  across instances is post-v1.
- **Backup / off-site replication automation, signed images, SBOM,
  managed-secrets integration.** Operator-side concerns per
  [`docs/deployment/production-runbook.md`](deployment/production-runbook.md)
  §13.

## 3. Must-have v1 capabilities

What must be true before calling v1 deployable, grouped by area.

### 3.1 Auth & security

- First-user bootstrap (`POST /api/v1/auth/bootstrap`, one-shot
  token + production envelope refuses to start without it).
- Login (`POST /api/v1/auth/login`) with email-normalised in-memory
  throttle; both unknown-email and wrong-password branches recorded.
- `/me`, logout, change-password.
- Session list, single-session revoke, revoke-all-except-current.
- CSRF / `Origin` guard on every browser-write route, ahead of the
  body extractor.
- Production envelope enforced at boot: signing key configured,
  non-empty `allowed_origins`, `cookie_secure=true`.
- Cookie posture: `HttpOnly; SameSite=Strict; Secure; 30-day
  hard-expire`.
- Argon2id OWASP_2023 password hashing.
- Redaction backstop: sentinel-string `AUDIT_FORBIDDEN_SUBSTRINGS`
  test sweep over audit / log / response bodies.

### 3.2 Inventory

- Hosts, server profiles, and SSH identities: list / read / create /
  detail panels, with the parser-level redaction backstops landed.
- Server-profile lifecycle: create / disable / enable / delete with
  the audit kinds and the `409 referenced` envelope. UI for disable
  / enable / create exists; **UI for edit / delete on hosts and
  server profiles is API-only today** — see §5 blocker B1.
- SSH-identity rename + delete UI landed; private-key import
  (Ed25519, unencrypted OpenSSH) landed end-to-end including the
  staging smoke recorded 2026-05-13.
- Host-key preflight + trust UI.
- Host-key replace (atomic revoke + re-trust): backend route AND
  SPA UI landed (`HostKeyPanel.svelte` wires the modal with reason
  selector, fingerprint confirmation, and dual-audit submit).
- Auth-check UI against the configured identity.

### 3.3 Terminal

- Launch a default xterm session against a trusted, auth-checked
  profile.
- Live russh PTY bridge (not a stub) with attach / detach /
  reconnect inside `DETACHED_LIVE_PTY_TTL` via sequence-replay from
  the in-memory ring.
- Window resize honored.
- Idempotent session close.
- Sessions list + status UI with refresh + close affordance.
- Per-user live / starting / per-deployment quotas enforced.
- Renderer-neutral autofit (operator opt-in via Settings, off by
  default).
- Launch timing diagnostics in the SPA (payload-free).
- Paste safety pipeline (`safe` / `confirm` / `blocked` metadata-only
  panels) and the sentinel-string redaction tests around it.
- No payload leakage: no terminal I/O, no recording bytes, no
  session token plaintext in any log / audit / Error / DOM / `data-*`.

### 3.4 Deployment

- Published OCI images for `relayterm-backend`,
  `relayterm-backend-migrate`, and `relayterm-web` at the Forgejo
  registry. Immutable `vX.Y.Z` and `sha-<short>` tags; `:main` is for
  staging only.
- `deploy/docker-compose.example.yml` /
  `deploy/docker-compose.images.example.yml` plus
  `deploy/relayterm.env.example` with every required env variable
  annotated and boot-validated.
- Migrations as a profile-gated one-shot
  (`docker compose --profile migrate run --rm relayterm-migrate`).
- Healthchecks defined on `postgres`, `relayterm-backend`,
  `relayterm-web` (and exercised by the post-deploy smoke).
- Reverse-proxy guidance for Traefik / Caddy / outer nginx
  (`Origin` preservation, WebSocket upgrade, long-lived `proxy_*`
  timeouts, no plain HTTP exposure).
- Same-origin contract between SPA and API (CSRF guard rests on it).
- TLS termination at the outer proxy; no plaintext on the public
  surface.

### 3.5 Operations

- Production-runbook checklist for: first deploy, upgrade, rollback
  (backward-compatible and backward-incompatible schema paths),
  migration order, backup (`pg_dump -Fc` to off-host),
  restore-from-backup procedure, reverse-proxy contract, secret
  rotation notes (signing key, vault key, bootstrap token, registry
  token, Postgres password), post-deploy smoke.
- Staging-smoke runbook
  ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md))
  for the throwaway slot at `relayterm-staging.js-node.cc`.
- Auth-flow smoke runbook ([`docs/auth-smoke.md`](auth-smoke.md))
  and the SPA smoke runbook
  ([`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md)).
- A documented log / audit redaction sweep step in the post-deploy
  smoke (grep for sentinels; treat any hit as a security
  regression).
- A documented "lost the only password" recovery path
  ([`docs/production-auth.md`](production-auth.md) §8).

## 4. Current status table

Capability rollup. Each row is one of:

| Mark | Meaning |
|---|---|
| **DONE** | Implemented and exercised by a smoke or by integration tests. |
| **DONE / smoke** | Implemented; needs a deliberate final smoke against the v1 cutline before release. |
| **PARTIAL** | Some surface implemented; UI or wiring gap a v1 operator would hit. |
| **BLOCKER** | True v1 blocker (see §5). |
| **POST-V1** | Deliberately deferred. |

### 4.1 Auth & security

| Capability | Mark | Evidence |
|---|---|---|
| First-user bootstrap | DONE | `crates/relayterm-api/src/routes/v1/auth.rs`; tests in `crates/relayterm-api/tests/api.rs`; runbook §4.10 |
| Login / logout / `/me` | DONE | Same; smoked in [`docs/auth-smoke.md`](auth-smoke.md) |
| Password change | DONE | `PasswordPanel.svelte`; auth-smoke runbook |
| Session list + revoke | DONE | `AuthSessionsPanel.svelte`; routes wired |
| CSRF / `Origin` guard | DONE | `relayterm_api::CsrfGuard`; integration test `bad_origin_rejects_before_body_parsing` |
| Production envelope boot validator | DONE | `apps/backend/src/config.rs`; runbook §4.4 |
| Login throttle | DONE (single-instance only) | `crates/relayterm-auth/src/throttle.rs`; tests in `login_throttle_*` |
| Argon2id password hashing | DONE | `relayterm-auth::password`; pinned at OWASP_2023 |
| Audit sentinel redaction tests | DONE | `AUDIT_FORBIDDEN_SUBSTRINGS` in `crates/relayterm-api/tests/api.rs` |
| Passkeys / WebAuthn | POST-V1 | [`docs/spec/auth.md`](spec/auth.md) "Passkey/WebAuthn stance" |
| Email password reset | POST-V1 | Same |
| IP-aware throttling | POST-V1 | `docs/spec/auth.md` "Open questions" |

### 4.2 Inventory

| Capability | Mark | Evidence |
|---|---|---|
| Hosts read / detail / create UI | DONE | `ServersView.svelte`; `inventoryApi.test.ts` |
| Hosts edit / delete UI | DONE | Wired in `ServersView.svelte` host detail panel (`host-detail-edit-*`, `host-detail-delete-*` test ids); calls `updateHost` / `deleteHost` with typed `409 referenced` handling. Operator-walked staging UI smoke recorded 2026-05-17 in [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) § "2026-05-17 · `docs/inventory-edit-delete-ui-staging-smoke`" rows A + B + C |
| Server-profile read / detail / create UI | DONE | Same |
| Server-profile disable / enable UI | DONE | `profileLifecycle.ts`; runbook + staging smoke 2026-05-12 |
| Server-profile edit / delete UI | DONE | Wired in `ServersView.svelte` profile detail panel (`profile-detail-edit-*`, `profile-detail-delete-*` test ids); `describeDeleteServerProfileError` routes a `409 referenced` (session-history) refusal to "disable it instead". Operator-walked staging UI smoke recorded 2026-05-17 in [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) § "2026-05-17 · `docs/inventory-edit-delete-ui-staging-smoke`" rows D + E + F + G (disable + re-enable verified end-to-end against a profile with terminal-session history) |
| SSH identity generate UI | DONE | `IdentitiesView.svelte` |
| SSH identity import (Ed25519 OpenSSH, unencrypted) | DONE | Landed in `feat/private-key-import-v1`; staging-smoked 2026-05-13 ([`docs/private-key-import.md`](private-key-import.md) banner) |
| SSH identity rename + delete UI | DONE | `IdentitiesView.svelte` |
| Host-key preflight + trust UI | DONE | `HostKeyPanel.svelte` |
| Host-key replace (atomic revoke + re-trust) backend route | DONE | `crates/relayterm-api/src/routes/v1/server_profiles.rs::replace_host_key` |
| Host-key replace UI | DONE | `apps/web/src/lib/app/views/HostKeyPanel.svelte` wires `replaceHostKey` with modal, reason-code select, fingerprint confirmation, and error surface (`host-key-replace-{button,modal,submit,…}` test ids) |
| Known-host pure revoke route + UI | POST-V1 | Schema column exists (`revoked_at`); replace-host-key covers the practical rotation flow; standalone revoke without re-trust is rarely needed |
| Auth-check UI | DONE | `AuthCheckPanel.svelte` |
| Inventory filters / search | DONE | `inventoryFilters.ts` |
| Audit-event read API | DONE | `recent_for_actor`; surfaced in `RecentActivityPanel.svelte` |

### 4.3 Terminal

| Capability | Mark | Evidence |
|---|---|---|
| Live russh PTY bridge | DONE | `crates/relayterm-ssh/src/russh_pty.rs`; `crates/relayterm-terminal/src/manager.rs` |
| Launch xterm session from production shell | DONE | `apps/web/src/lib/app/terminal/terminalLaunch.ts`; `ProductionTerminal.svelte`; staging smoke 2026-05-13 "Deployable-baseline end-to-end" |
| Attach / detach / reconnect with sequence replay (in-TTL ring) | DONE | In-memory ring (1024 frames / 1 MiB) wired in `crates/relayterm-terminal/src/replay.rs`; the WS attach handler at `crates/relayterm-api/src/routes/v1/terminal_sessions.rs:800` calls `replay_since` and emits `emit_replay_range` / `emit_replay_window_lost`. **Note:** the wire-stable `LIVE_PTY_{CREATE,ATTACH}_MESSAGE` strings ("replay across reconnects is not yet implemented") in `crates/relayterm-terminal/src/manager.rs` are pinned conservative copy and pre-date the wired ring path — they MUST stay until the next wire-message revision; they do NOT contradict the DONE mark for in-TTL ring replay. Backend-restart durable resume remains POST-V1 |
| Idempotent session close | DONE | `POST /terminal-sessions/:id/close` |
| Window resize | DONE | `Resize` wire message; russh `window_change` |
| Renderer-neutral autofit (operator opt-in) | DONE | [`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md); staging resmoke 2026-05-15 |
| Sessions list + close UI | DONE | `SessionsView.svelte` |
| Per-user / per-deployment quotas | DONE | `RELAYTERM_TERMINAL_SESSIONS__MAX_*` knobs in `relayterm.env.example` |
| Launch timing diagnostics | DONE | commit `ee89764`; verified across 5 surfaces, latest 2026-05-17 multi-run resmoke |
| Paste safety pipeline | DONE | sentinel-string tests; metadata-only panels |
| Renderer evaluation (ghostty-web / wterm / restty) | POST-V1 | xterm stays default; gated lazy loader for experiments only |
| Multi-tab / multi-pane terminal workspace | POST-V1 | Single launch surface in v1 |
| Backend-restart durable session resume | POST-V1 | Out of scope; see [`docs/persistent-sessions.md`](persistent-sessions.md) |

### 4.4 Deployment

| Capability | Mark | Evidence |
|---|---|---|
| Docker Compose production stack (images-mode) | DONE | `deploy/docker-compose.images.example.yml` |
| Compose stack (build-mode for dev) | DONE | `deploy/docker-compose.example.yml` |
| Env example with every variable annotated | DONE | `deploy/relayterm.env.example` |
| Migration as one-shot profile-gated container | DONE | Runbook §4.8 |
| Healthchecks on postgres / backend / web | DONE | Compose example |
| Reverse-proxy guidance (Traefik / Caddy / nginx) | DONE | Runbook §9; `docker-compose.md` §3 |
| TLS at outer proxy | DONE (operator-side) | Runbook §9.1 + §9.5 |
| CI image publish | DONE | `docker-compose.md` §6.4; smoked 2026-05-08 staging |
| Image rollback by immutable tag | DONE | Runbook §6.1 |
| Migration revert on backward-incompatible upgrade | DONE / smoke | Runbook §6.2 — restore from backup is the documented path; no formal automated revert. Pre-rehearsal recommended |
| Staging-smoke runbook | DONE | [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) |
| Production-walked end-to-end smoke | **BLOCKER (B2)** | Staging smoke is extensive; an explicit v1-cutline production smoke against a real production hostname (not the throwaway staging slot) has not been recorded |
| Backup procedure (`pg_dump -Fc` + off-site reminder) | DONE | Runbook §8; operator-facing manual procedure at [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md) §4 |
| Restore-from-backup procedure | DONE | Runbook §8 + §6.2; operator-facing manual procedure at [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md) §5 + §6 |
| Restore-test rehearsal | DONE / runbook + rehearsal template exist; actual rehearsal pending | Runbook §8.3 + the [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md) §10 short-form rehearsal template + the canonical [`docs/deployment/backup-restore-rehearsal-record.md`](deployment/backup-restore-rehearsal-record.md) (§5 template, §6/§7/§8 verification checklists, §9 redaction sweep, §10 verification log seeded NOT RUN); recommended quarterly; the still-pending successor slice `docs/backup-restore-rehearsal-run` is the operator-walked Case R-B entry that closes verification |
| Secret rotation (signing key / vault key / bootstrap / registry / DB) | DONE | Runbook §11 — with explicit caveats on vault-key rotation (no re-encryption pass in v1) |
| Tauri desktop / mobile release packaging | POST-V1 | Scaffolds + local-build docs only; no CI |

### 4.5 Operations

| Capability | Mark | Evidence |
|---|---|---|
| Production-runbook (first deploy / upgrade / rollback) | DONE | [`docs/deployment/production-runbook.md`](deployment/production-runbook.md) |
| Post-deploy smoke checklist | DONE | Runbook §10 |
| Production-walked redaction sweep | DONE | Runbook §10 last bullet |
| Inspect sessions without leaking payloads | DONE | `recent_for_actor`; redaction rules `docs/agent/redaction-rules.md` §§ 1, 11 |
| Lost-password operator recovery | DONE | [`docs/production-auth.md`](production-auth.md) §8 |
| Compose env-contract guard | DONE | `docker-compose.md` §4.6 |
| Mobile portrait usability on default xterm | **DONE / smoke (B3 caveat)** | `mobileNavOpen` drawer + mobile-aware components landed; recent real-Android-Chrome multi-run resmoke 2026-05-17 reached `attached` cleanly; an explicit operator-recorded mobile-portrait sanity smoke against a non-throwaway target is the remaining v1 gate (see §5 row B3). |
| Backup automation, off-site replication, SBOM, signing | POST-V1 | Runbook §13 |

## 5. Blockers to v1 (evidence-based)

These are the true blockers — concrete missing surfaces an operator
running v1 would hit, with the evidence behind each.

- ~~**B1. Frontend edit / delete UI for hosts and server profiles.**~~
  **DONE — 2026-05-17.** Implementation landed earlier in commit
  `f1f0691` ("feat(api): add inventory management mutations",
  2026-05-12); the operator-visible staging UI walk that the v1
  cutline required was recorded 2026-05-17 in
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § "2026-05-17 · `docs/inventory-edit-delete-ui-staging-smoke`"
  (rows A–I; B1-relevant rows are A host edit, B host delete, C host
  delete refused by referenced profile, D profile edit, E profile
  delete, F profile delete refused by terminal-session history, G
  profile disable + re-enable on a profile with history, H 390 × 844
  mobile reachability of all edit / delete / disable controls, I
  redaction sweep returning zero sentinel hits across backend log,
  nginx access log, AND every `audit_events.payload` written in the
  smoke window). The error copy on the two 409-refusal rows is
  byte-exact the safe-formatter strings in `describeDeleteHostError`
  / `describeDeleteServerProfileError` (no wire `message` echo). The
  cutline drafted on `1813552` flagged this as a blocker on the
  premise that no production view called the `updateHost` /
  `deleteHost` / `updateServerProfile` / `deleteServerProfile`
  helpers; that was a stale reading of `ServersView.svelte`. For
  reference (now historical), the UI lives inside the host + server-
  profile detail panels:
  - Host detail panel: `host-detail-edit-open` opens the edit form
    (`host-detail-edit-{display-name,hostname,port,username}`),
    `host-detail-delete-open` opens a typed-name confirmation
    (`host-detail-delete-confirm-input`,
    `host-detail-delete-confirm-submit`), the `409 referenced` reason
    is mapped by `describeDeleteHostError` to the "still used by a
    saved server profile or has trusted host keys" copy.
  - Profile detail panel: `profile-detail-edit-open` opens the edit
    form (`profile-detail-edit-{name,host,identity,
    username-override,tags}` with delta-build so an empty save
    surfaces "change at least one field"), `profile-detail-delete-
    open` opens a typed-name confirmation
    (`profile-detail-delete-confirm-input`,
    `profile-detail-delete-confirm-submit`), the `409 referenced`
    reason is mapped by `describeDeleteServerProfileError` to "it
    has terminal session history — disable it instead to keep the
    history while blocking new launches" — routing the operator to
    the existing disable flow exactly as the spec prescribes.
  Helpers + describers are exhaustively unit-tested in
  `apps/web/tests/inventoryMutationsApi.test.ts` (52 tests, includes
  redaction-sentinel sweeps). Disable confirmation logic is pinned
  by `apps/web/tests/profileLifecycle.test.ts` (24 tests). The
  operator-walked staging UI smoke that the cutline previously named
  as the remaining v1 gate now lives at
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § 2026-05-17.

- **B2. No production-walked end-to-end smoke recorded.** The
  staging smoke history at
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  is extensive against the throwaway `relayterm-staging.js-node.cc`
  slot, but the v1 release should be gated on at least one
  end-to-end pass against a real production hostname using the
  cutline in §9 of this doc and the runbook's §10 smoke. Scope:
  pick the operator's first production hostname, walk §9, record
  the result. Docs-only deliverable, no code. **Template
  skeleton landed 2026-05-17** on
  `docs/v1-production-smoke-record` at
  [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
  (status NOT EXECUTED — operator had not yet chosen a
  production hostname); the §5 entry template + §3
  prerequisites checklist there is what a successor slice
  copies and walks against the real production hostname. **B2
  remains PENDING** until that successor entry records PASS;
  the template is not evidence.

- **B3. Mobile portrait sanity smoke against the production xterm
  path.** The default xterm renderer is the production v1 surface;
  the recent real-Android-Chrome 2026-05-17 multi-run resmoke
  ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § 2026-05-17) attached cleanly across three sequential launches.
  An operator-recorded portrait sanity walk — launch, basic I/O,
  detach, reconnect, close — against a non-throwaway-shaped target
  closes the loop and unblocks the "web app is usable on a phone"
  claim. Anything more (typography polish, soft-keyboard tuning,
  paste affordance tuning) is post-v1.

Notable items that initially LOOK like blockers but are not:

- *Recording UI toggle* — recording is OFF by default, optional, and
  enabling it requires reading
  [`docs/terminal-recording.md`](terminal-recording.md) end-to-end.
  An in-product toggle is post-v1; recording is not on the v1
  critical path.
- *Standalone known-host revoke UI* — replace-host-key (B2 above)
  covers the rotation flow operators actually need. Pure-revoke
  without re-trust can stay post-v1.
- *Backend-restart durable session resume* — out of scope per
  [`docs/persistent-sessions.md`](persistent-sessions.md); the v1
  reconnect contract is bounded by `DETACHED_LIVE_PTY_TTL`.
- *Renderer evaluation* — explicitly post-v1; xterm baseline already
  proved-out as the production default.

## 6. Non-blocking but important post-v1

Track these without letting them creep into the v1 cutline. Each has a
home doc; this list is the consolidated post-v1 backlog from a v1
perspective.

- wterm / ghostty-web / restty production evaluation
  ([`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md),
  [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)).
- Production renderer selector UI.
- Tauri desktop & Android CI / signing / release pipeline
  ([`docs/deployment/tauri-ci-release-plan.md`](deployment/tauri-ci-release-plan.md)).
- iOS Tauri shell.
- Mobile portrait polish past "usable" — soft-keyboard affordances,
  typography pass, paste-UX pass.
- Restty viability decision (CSP / font / WebGPU choice).
- Performance / renderer benchmark harness.
- Recording in-product toggle, export, and search UI.
- Per-chunk recording encryption.
- Tmux-like host-side multiplexing
  ([`docs/persistent-sessions.md`](persistent-sessions.md)).
- VT-snapshot observer / display reconstruction beyond ring replay.
- Passkeys / WebAuthn, password reset, IP-aware throttling.
- Multi-user / admin / RBAC.
- Backup automation, off-site replication, SBOM, signed images.
- Multi-instance / HA / Kubernetes deploy story (shared throttler,
  shared session orchestrator).
- Pure known-host-revoke UI (without re-trust).
- Production CSP relaxation for experimental renderers (would only
  follow a renderer-promotion decision).

## 7. Recommended next implementation slices (ranked)

Three docs / features ordered by "what most moves the needle toward a
shippable v1" — prefer production readiness over renderer experiments.
(`feat/inventory-edit-delete-ui` was the originally-listed top slice;
it resolved as docs-only on 2026-05-17 once the audit caught that the
UI had already landed in commit `f1f0691` — see B1 above. The
2026-05-17 staging UI smoke walked the cutline §9 inventory rows
end-to-end, so B1 is fully DONE; the next-most-impactful slices are
B2 / release checklist / B3 in that order.)

1. ~~**`docs/v1-release-checklist`**~~ **DONE — 2026-05-17.** Landed
   on `docs/v1-release-checklist` as
   [`docs/v1-release-checklist.md`](v1-release-checklist.md). The
   release-day operator checklist composes runbook §4 (first
   deploy), §10 (post-deploy smoke), §11 (secret rotation hygiene),
   and §9 of this doc (cutline smoke) into a single page with a
   §12 decision table and a §13 sign-off template. B1 is recorded
   as PASS (staging evidence) and B2 / B3 stay PENDING, matching
   this cutline. The two cutline blockers remaining are now
   surfaced as the explicit next-slice candidates §7 row 2 and
   row 3 below.
2. **`docs/v1-production-smoke-record`** (resolves B2). The
   operator-recorded production-walked smoke entry. **Template
   skeleton landed 2026-05-17 (status NOT EXECUTED)** at
   [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
   — the slice was re-scoped to template-only because the
   operator had not yet chosen a production hostname / deploy
   host / image tag. The successor slice copies §5 of that
   file into a new dated entry and walks every row against
   the real production hostname; the entry should include the
   inventory edit + delete + delete-refused-by-history walks
   now that B1 is fully DONE (the 2026-05-17 staging entry is
   the row format; the production walk re-runs the same rows
   against the operator's real production hostname). The new
   production smoke log is a dedicated sibling at
   `docs/deployment/v1-production-smoke.md`, NOT an append to
   the staging log — staging vs production must not be
   co-mingled.
3. **`docs/v1-mobile-portrait-sanity-smoke`** (resolves B3). One
   operator-recorded run on a real Android phone against the default
   xterm production path, using the existing renderer-fair smoke
   methodology from `apps/web/e2e/SMOKE.md` § D as the input rules.

Honourable mentions (would help but not v1-critical):

- ~~**`docs/backup-restore-runbook`**~~ **DONE — 2026-05-17.**
  Landed on `docs/backup-restore-runbook` as
  [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md).
  Operator-facing manual backup / restore / rollback runbook;
  closes the §4.4 "DONE / smoke" doc gap on restore procedure
  ergonomics. Pair-with slice below remains pending.
- ~~**`docs/backup-restore-rehearsal-record`**~~ **DONE
  (template) — 2026-05-18.** Landed on
  `docs/backup-restore-rehearsal-record` as
  [`docs/deployment/backup-restore-rehearsal-record.md`](deployment/backup-restore-rehearsal-record.md).
  Operator-recorded rehearsal log skeleton — §1 status, §2
  scope (T1–T4), §3 preconditions, §4 safety rules, §5 record
  template, §6/§7/§8 verification checklists for backup /
  restore / rollback, §9 redaction sweep (sentinels mirror
  v1-production-smoke §5.1), §10 verification log seeded NOT
  RUN. Closes the template gap; the still-pending successor
  slice **`docs/backup-restore-rehearsal-run`** is the
  operator-walked Case R-B entry that closes the remaining
  verification gap on the §4.4 "Restore-test rehearsal" row.
- ~~**`feat/operator-status-page`**~~ **DONE — 2026-05-17.** Landed
  on `feat/v1-operational-status-page` as the Operational Status
  panel inside the Settings view
  (`apps/web/src/lib/app/views/OperationalStatusPanel.svelte` +
  pure helpers at
  `apps/web/src/lib/app/settings/operationalStatus.ts`). Surfaces
  backend reachability (`/healthz`), browser session counts,
  terminal session counts by status, the deployment-configured
  detached-PTY TTL and per-user quotas (from the existing
  `/api/v1/config/session-policy` endpoint), the local
  experimental-renderer gate posture, the local autofit posture,
  the read-only "next session will mount X" copy, and three
  production-readiness reminders that point to the backup runbook
  and the v1 release checklist. **No new backend endpoints; no
  secrets / env values / DB URLs exposed.** Sentinel-tested in
  `apps/web/tests/operationalStatus.test.ts` (41 tests); SMOKE.md
  selector vocabulary documents `settings-operational-status-*`
  test ids and the §5b dev-smoke row asserts panel presence
  without claiming specific counts. The panel does NOT substitute
  for B2 (production smoke) or B3 (mobile portrait sanity) — those
  rows remain operator-walked and the panel's readiness section
  surfaces that explicitly.

Deliberately NOT recommended as next slices:

- Any renderer-promotion or experimental-renderer matrix smoke.
- Tauri release work.
- Recording UI toggle / export.
- Any auth / multi-user / RBAC expansion.

## 8. Decision on renderer lane

- **xterm.js is the v1 default on every surface.** This is the
  load-bearing production compatibility baseline; the cutline does
  NOT propose any change to it.
- **wterm is promising for mobile / browser-native UX** but stays
  post-v1. The 2026-05-17 multi-run resmoke removed the "wterm broke
  mobile" reading by reclassifying the 2026-05-15c detach pattern as
  workspace-bound + transient (and the 2026-05-16 xterm-control
  resmoke also reproduced the same pattern under xterm before going
  green). wterm's product upside is real but not v1-blocking.
- **ghostty-web is the correctness candidate.** libghostty-vt parser
  via WASM. Post-v1; needs the resize / reflow Gate-1 decision and a
  v1-independent CSP conversation.
- **restty is a research track.** Post-v1; needs the viability
  decision per scorecard §7.
- **The gated lazy loader stays.** Operators can flip the
  `experimentalRendererEvaluationEnabled` gate in Settings and pick
  a non-default renderer for evaluation; the production default does
  not change.
- **No production CSP widening in v1.** The staging-only
  `'wasm-unsafe-eval'` relaxation does not migrate to production.
  This keeps the strict `default-src 'self'` posture intact for the
  v1 surface.
- **No renderer-evaluation slice is on the v1 critical path.**
  Renderer work continues in its own lane, governed by
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md);
  the cutline neither blocks it nor depends on it.

## 9. Deployment cutline — what must be smoked before calling v1 deployable

This is the punch list the operator walks against the v1 release tag.
Each row maps to existing infrastructure; nothing here is new code.

**Pre-flight.**

- [ ] Image tag selected (`vX.Y.Z` or `sha-<short>`; never `:main`).
- [ ] Public origin + TLS cert ready at the outer reverse proxy.
- [ ] `.env` populated per `relayterm.env.example`; session signing key
  ≠ vault master key; `chmod 600`.
- [ ] Reverse proxy preserves `Origin`, handles WS upgrade, sets
  `proxy_read_timeout 3600s`.

**Fresh install.**

- [ ] `docker compose --profile migrate run --rm relayterm-migrate`
  succeeds.
- [ ] `docker compose up -d postgres relayterm-backend relayterm-web`
  reaches healthy on all three.
- [ ] First-user bootstrap via `POST /api/v1/auth/bootstrap` returns
  201; bootstrap token is then unset and the backend restarted
  (runbook §4.11).

**Auth & session management.**

- [ ] Login via the SPA reaches the dashboard with the session cookie
  set (`HttpOnly; Secure; SameSite=Strict`).
- [ ] `/api/v1/auth/me` returns the bootstrapped user.
- [ ] `POST /api/v1/auth/change-password` succeeds; OTHER sessions
  revoked.
- [ ] Session list shows the current session; revoke-other works;
  current-cookie session stays valid.
- [ ] Logout clears the cookie.
- [ ] Bad-`Origin` write returns `403 csrf_origin_mismatch`.

**Inventory.**

- [ ] Create an SSH identity (generate or import Ed25519).
- [ ] Create a host.
- [ ] Create a server profile binding the host + identity.
- [x] (staging, 2026-05-17) Edit the profile metadata; delete an
  unused profile; attempt a delete that refuses with the disable
  guidance. (B1 resolved — see
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § 2026-05-17 rows A + B + C + D + E + F + G. **§9 [x] marks
  here record staging proof; the final production walk re-ticks
  each row against the operator's real production hostname per
  B2.**)
- [ ] Trust the host key via the preflight + trust UI.
- [ ] Auth-check succeeds against the target.
- [ ] Disable the profile; confirm launch is refused; re-enable.
- [ ] (Optional sanity) Trigger a host-key `changed` outcome against
  a re-keyed throwaway target and walk the `replace-host-key` modal
  through to a successful dual-audit transition. Skip if no
  rotation event is in scope for this release.

**Terminal.**

- [ ] Launch an xterm session against the trusted, auth-checked
  profile.
- [ ] Type a command; output appears; window resize works.
- [ ] Close the SPA tab; reopen within `DETACHED_LIVE_PTY_TTL`;
  session reattaches and replays missed output.
- [ ] Close the session via the sessions list; row transitions to
  `closed`.

**Operations.**

- [ ] Post-deploy smoke (runbook §10) walked top to bottom.
- [ ] Redaction sweep (the `grep -E` line in runbook §10 last bullet)
  returns "ok: no leakage sentinels found."
- [ ] At least one `pg_dump -Fc` backup written to off-host storage
  (procedure: [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md) §4).
- [ ] Restore-from-backup rehearsal: pre-rehearsed at least once
  against a throwaway Postgres (runbook §8.3; procedure
  [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md) §5 Case R-B,
  template §10).
- [ ] Rollback path identified for the current tag (which earlier
  `vX.Y.Z` / `sha-<short>` is the rollback target).
- [ ] (Once B2 records) the production-walked smoke entry committed.
- [ ] (Once B3 records) the mobile portrait sanity entry committed.

## 10. Open questions for the operator

These need the operator (you) to decide before v1 lands. The cutline's
default for each is conservative; flip only if the operator decides
otherwise.

- **Is private-key import required for v1, or can generate-only
  ship?** Default: import-included (it has already landed and
  staging-smoked). Flip only if the operator wants to defer
  import-related risk.
- **Is public internet deployment required at v1, or is
  VPN-only / WireGuard-only acceptable?** Default: public-internet
  deployment is supported and documented; VPN-only is the operator's
  choice.
- **Is mobile browser support required at v1, or is desktop-first
  acceptable?** Default: "usable on mobile Chrome" is required (and
  B3 records the proof); deliberate mobile polish stays post-v1.
- **Is recording enabled for v1, or optional / disabled?** Default:
  OFF (matches the shipped default and avoids the plaintext-at-rest
  question for v1).
- **Is multi-user out of scope for v1?** Default: yes, out of scope.
  v1 is single-user self-hosted.
- **Which v1 hostname is the production smoke (B2) walked against?**
  Open — operator picks.
- **Are the Tauri desktop / Android shells in scope for v1 release
  notes, or shipped only as "available, build-it-yourself"?**
  Default: "available, build-it-yourself via
  [`docs/deployment/tauri-local-build.md`](deployment/tauri-local-build.md)";
  no signed release artifact in v1.

---

## See also

- [`docs/v1-release-checklist.md`](v1-release-checklist.md) —
  operator-facing release-day checklist that turns this cutline
  into a step-by-step gate (§3–§11), a decision table (§12), and
  a sign-off template (§13).
- [`docs/deployment/first-production-deploy-plan.md`](deployment/first-production-deploy-plan.md)
  — short, opinionated planning page for the first personal
  production deploy; answers §10 "Open questions for the
  operator" with conservative defaults and points back at the
  release-day checklist for the row-by-row gate. Planning-only;
  does not change B2 / B3 status.
- [`docs/v1-release-notes.md`](v1-release-notes.md) — draft v1
  user-facing release notes (what v1 is, included features,
  caveats, post-v1 roadmap, sign-off template). Pairs with the
  release-checklist §13 sign-off and the v1 production-smoke §5
  entry header.
- [`AGENTS.md`](../AGENTS.md) — agent-facing conventions and the
  architectural invariants this cutline rests on.
- [`SPEC.md`](../SPEC.md) — product spec; the cutline does not
  redefine any contract here.
- [`docs/deployment/production-runbook.md`](deployment/production-runbook.md)
  — the load-bearing operator runbook §9 references.
- [`docs/deployment/docker-compose.md`](deployment/docker-compose.md)
  — Compose stack reference + CI image-publish detail.
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — the staging smoke history this cutline draws evidence from.
- [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
  — v1 production smoke log; template skeleton landed
  2026-05-17 (NOT EXECUTED). Operator-walked entries here are
  the B2 evidence track.
- [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md)
  — operator-facing manual backup / restore / rollback
  procedure that closes the §4.4 doc gap on restore ergonomics.
- [`docs/deployment/backup-restore-rehearsal-record.md`](deployment/backup-restore-rehearsal-record.md)
  — operator-recorded rehearsal log; §5 template + §10
  verification log seeded NOT RUN. Paired with the runbook
  above; the §4.4 "Restore-test rehearsal" row closes when the
  first dated entry under §10 there records PASS.
- [`docs/production-auth.md`](production-auth.md) and
  [`docs/auth-smoke.md`](auth-smoke.md) — auth operator surface.
- [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  and [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  — the renderer lane this cutline marks post-v1.
- [`docs/persistent-sessions.md`](persistent-sessions.md) — the
  longer-term reconnect roadmap the v1 TTL window sits inside.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) — operator
  runbook for SPA-level smokes.
