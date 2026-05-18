# RelayTerm v1 release checklist

> Operator-facing checklist that turns the v1 production-readiness
> cutline into a practical, step-by-step release gate. Walk this top
> to bottom before tagging and deploying v1.
>
> **Status as of 2026-05-17:** drafted on `docs/v1-release-checklist`
> against the snapshot of `main` at commit `a9425b1`
> ("docs(deployment): record inventory edit delete UI smoke"). This
> doc composes existing primitives — it does NOT redefine any
> contract in `AGENTS.md`, `SPEC.md`, `docs/spec/*`, or
> `docs/deployment/production-runbook.md`. Where this doc and any
> upstream contract disagree, the upstream contract wins and this
> doc is the bug.

## 1. Purpose

This checklist is the final release gate for a **single
self-hosted v1 deploy**. It is the operator's "did I miss
anything?" page for the day they cut the v1 tag and stand up the
first production instance.

It composes:

- [`docs/v1-production-readiness.md`](v1-production-readiness.md)
  §9 (the cutline punch list).
- [`docs/deployment/production-runbook.md`](deployment/production-runbook.md)
  §4 (first deploy), §10 (post-deploy smoke), §11 (secret
  rotation).
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  (staging evidence the v1 cutline draws from).

This doc is NOT a replacement for those sources. Treat it as the
operator's single page of checkboxes; chase each `→` link to the
load-bearing detail when you actually walk a row.

## 2. Release posture

State these out loud before starting; they pin operator
expectations and prevent scope creep into v1.

- **xterm.js is the production default renderer on every
  surface.** No renderer default flips at v1. The experimental
  renderers (`ghostty-web`, `wterm`, `restty`) remain dev-only
  and reach the production shell ONLY through the gated lazy
  loader at `apps/web/src/lib/app/terminal/rendererLoader.ts`,
  via dynamic `import()`, AND ONLY when the operator has flipped
  the `experimentalRendererEvaluationEnabled` gate in Settings.
  → `docs/spec/terminal-adapters.md`.
- **Experimental renderers are not part of v1 readiness.** No
  renderer-evaluation row gates this release. The renderer track
  continues independently per
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  and the snapshot
  [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md).
- **Staging smokes are evidence but production hostname smoke is
  still required.** Every `[x]` in
  [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  is real evidence, but the v1 cutline requires at least one
  end-to-end pass against a real production hostname (cutline
  blocker B2) and at least one operator-recorded mobile portrait
  sanity walk against the default xterm production path (cutline
  blocker B3).
- **No claim of multi-user / team readiness.** v1 is
  single-tenant, single-self-hosted-operator. RBAC, admin, team
  features, and shared throttler / shared session orchestrator
  across instances are explicitly post-v1.
- **No claim of full mobile app readiness.** v1 ships the web
  SPA; "usable on mobile Chrome" is the bar (B3 records the
  proof). Tauri Android shell builds locally per
  [`docs/deployment/tauri-local-build.md`](deployment/tauri-local-build.md)
  but is NOT a v1 release artifact; no Play Store / App Store
  bundling, no signed CI release.
- **No production CSP widening in v1.** The staging-only
  `'wasm-unsafe-eval'` CSP relaxation does NOT migrate to
  production. The production CSP keeps the strict
  `default-src 'self'` posture that the xterm baseline relies
  on.

## 3. Pre-release repository checks

Before tagging.

- [ ] On `main`, clean working tree: `git status --short --branch`
  shows `## main...origin/main` with no diff.
- [ ] Local `main` is fast-forwarded to `origin/main`:
  `git pull --ff-only` is a no-op.
- [ ] CI green on the chosen commit (Forgejo Actions: workspace
  checks + image-publish workflow both green).
- [ ] Image-publish workflow has produced the immutable tags you
  intend to deploy (`vX.Y.Z` and `sha-<short>`) for each of
  `relayterm-backend`, `relayterm-backend-migrate`,
  `relayterm-web`. → runbook §3 (Tag policy).
- [ ] Doc contracts pass locally:
  `bash scripts/check-doc-contracts.sh` returns success.
- [ ] Backend baseline green:
  `cargo check --workspace --all-targets` and
  `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Frontend baseline green: `pnpm -r check` and `pnpm -r lint`.
- [ ] Unit tests pass: `cargo test --workspace` and `pnpm -r test`.
- [ ] Release tag / version decision recorded (operator picks
  `vX.Y.Z`; `:main` and unpinned `:latest` are NEVER the
  release tag).
- [ ] Rollback tag recorded — the previous `vX.Y.Z` or
  `sha-<short>` you would fall back to if the deploy goes wrong.
  → runbook §6.1.

## 4. Configuration checks

The operator's `.env` and the reverse-proxy config. Every row
maps to a row in `deploy/relayterm.env.example`.

- [ ] `RELAYTERM_AUTH__MODE=production` (production envelope; boot
  refuses to start without it).
- [ ] `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64` set to a freshly
  generated 32-byte base64 value. NOT the placeholder
  `CHANGE_ME_base64_32_bytes`.
- [ ] `RELAYTERM_VAULT__MASTER_KEY_B64` set to a SEPARATE,
  freshly generated 32-byte base64 value. **Must differ from**
  `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64`. Vault key compromise
  ≠ session key compromise; do not collapse them.
- [ ] `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` set to a
  fresh URL-safe base64 value, NON-EMPTY for the first deploy.
  The example file ships this intentionally empty
  (`RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN=`); the
  operator MUST fill it in before first boot, then unset it
  after bootstrap (see §6). → runbook §4.10.
- [ ] `RELAYTERM_AUTH__COOKIE_SECURE=true` (mandatory in
  production; the session cookie is `HttpOnly; SameSite=Strict;
  Secure` only when this is `true`).
- [ ] `RELAYTERM_AUTH__ALLOWED_ORIGINS` is a byte-equality list
  containing exactly the operator's production origin (e.g.
  `https://relay.example.com`). No trailing slash. No staging or
  attacker-test origin left in by accident.
- [ ] `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` set;
  `POSTGRES_PASSWORD` is NOT the placeholder
  `CHANGE_ME_postgres_password`.
- [ ] Same-origin contract holds: SPA and `/api/` are served on
  the SAME public origin. The CSRF guard rests on this. → runbook
  §9.
- [ ] **Terminal recording decision recorded.** Default and
  recommended for v1 is OFF:
  - `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`.
  - The retention worker
    (`RELAYTERM_TERMINAL_RECORDING__CLEANUP__*`) is independent
    of recording-enabled and may stay on for housekeeping; the
    defaults
    (`RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED=true`,
    `RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS=3600`,
    `RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE=250`) are
    operator-acceptable.
  - If the operator opts recording IN at v1 (NOT the cutline
    default), re-read
    [`docs/terminal-recording.md`](terminal-recording.md)
    end-to-end before flipping it on. The recording vault key
    SHOULD be a separate key from
    `RELAYTERM_VAULT__MASTER_KEY_B64` once that surface lands;
    chunk-encryption is post-v1 per cutline §6, so the v1
    posture is OFF.
- [ ] Per-user / per-deployment terminal session quotas reviewed:
  the
  `RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER`
  / `..._STARTING_..._PER_USER` /
  `..._LIVE_PTY_..._PER_DEPLOYMENT` knobs are optional, default
  sensible for a single-operator deploy, and left at the
  example file's defaults unless the operator has a reason to
  change them.
- [ ] `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`
  decision recorded (default 30s; operator may extend per
  taste). v1 reconnect window is bounded by this value;
  backend-restart durable resume is POST-V1.
- [ ] `RUST_LOG` set to a non-debug value
  (`relayterm=info,axum=info,sqlx=warn,info` from the example
  file is the recommended baseline). Operator must avoid
  `debug` / `trace` in production — debug logging widens the
  redaction-sentinel risk surface.
- [ ] Reverse proxy / TLS hostname configured. → runbook §9.
  Specifically:
  - HTTPS on the production origin; no plain HTTP exposure.
  - `Origin` header passes through unmodified (nginx needs
    `proxy_set_header Origin $http_origin;` explicitly).
  - WebSocket upgrade headers (`Upgrade`, `Connection`) pass
    through for `/api/v1/terminal-sessions/<id>/ws`.
  - `proxy_read_timeout 3600s` (and matching `proxy_send_timeout
    3600s`) on the API location.
  - `proxy_buffering off` on the WS location.
- [ ] **Staging-only assumptions NOT copied to production.** The
  staging `'wasm-unsafe-eval'` CSP relaxation is for
  experimental-renderer evaluation against the throwaway staging
  slot only. The production CSP MUST remain the strict
  `default-src 'self'` posture. If you copied a `nginx.conf`
  from `deploy/docker-compose.traefik-staging.example.yml`,
  re-walk the CSP block.
- [ ] **Experimental renderer gate default is OFF.** Operators
  may flip the `experimentalRendererEvaluationEnabled` gate in
  Settings post-deploy for personal evaluation. The cutline
  recommendation for v1 is **leave it OFF** on the production
  deploy; xterm is the production default.

## 5. Deployment checks

Walk these on the production host. Every command line is the
exact form from runbook §4 / §7 / §10.

- [ ] DB backup BEFORE the upgrade / install (if this is an
  upgrade, not a fresh install):
  `docker compose exec -T postgres pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc > "/srv/relayterm/backups/relayterm-pre-vX.Y.Z-$(date -u +%Y%m%dT%H%M%SZ).dump"`.
  → runbook §8.
- [ ] Config backup: copy the live `.env` to an off-host
  location with `chmod 600` preserved.
- [ ] `docker compose config` returns clean env interpolation
  (no `WARN`, no missing variables).
- [ ] Image pull at the selected pinned tag:
  `docker compose pull`.
- [ ] Migrations applied:
  `docker compose --profile migrate run --rm relayterm-migrate`.
  Exit code 0; final line shows the applied migration ID.
  → runbook §7.
- [ ] Stack up:
  `docker compose up -d postgres relayterm-backend relayterm-web`.
- [ ] Healthchecks reach healthy on all three services:
  `docker compose ps` shows `(healthy)` for `postgres`,
  `relayterm-backend`, and `relayterm-web`.
- [ ] Backend health endpoint OK from the host loopback:
  `curl -sf http://127.0.0.1:8081/healthz`.
- [ ] Web health endpoint OK from the host loopback:
  `curl -sf http://127.0.0.1:8081/_web_health`.
- [ ] Public origin reachable from the operator workstation:
  `curl -sf https://<origin>/_web_health`.
- [ ] Rollback tag IDs known and recorded next to the deploy
  log entry. Both `vX.Y.Z` (the deploy you are upgrading FROM)
  AND `sha-<short>` (the exact commit) so a rollback is
  unambiguous. → runbook §6.1.
- [ ] Log redaction sanity (early — before user data lands):
  ```sh
  docker compose logs --tail=2000 relayterm-backend | \
    grep -E 'relayterm_session=[A-Za-z0-9_-]{20,}|encrypted_private_key|data_b64' \
    || echo "ok: no leakage sentinels found"
  ```
  Must print `ok: no leakage sentinels found`. Any hit is a
  security regression — STOP, investigate, do not proceed to §6.
  → runbook §10 (last bullet). This is the early-signal grep;
  the comprehensive sentinel sweep with the full pattern set
  (adds `BEGIN OPENSSH PRIVATE KEY` and `token_hash`) is in §11.

## 6. First-user / auth checks

The first operator can log in and the auth envelope is intact.

- [ ] Bootstrap-only-when-no-user-exists holds. Run the bootstrap
  POST exactly once with the configured token:
  ```sh
  curl -fsS -X POST \
    -H 'Content-Type: application/json' \
    -H "Origin: https://<origin>" \
    --data-binary @- \
    https://<origin>/api/v1/auth/bootstrap <<'JSON'
  { "token": "<RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN>", "email": "...", "password": "..." }
  JSON
  ```
  Returns `201`. → runbook §4.10.
- [ ] After bootstrap succeeds, **unset** the bootstrap token in
  `.env` and restart the backend:
  `sed -i '/^RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN=/d' .env`
  then
  `docker compose up -d --no-deps relayterm-backend`.
  → runbook §4.11.
- [ ] A second bootstrap attempt now returns a clean rejection
  (no token configured / user already exists). The backend
  refuses to mint a second first-user via this route.
- [ ] Login via the SPA reaches the dashboard. The session
  cookie set in the browser is `HttpOnly; Secure;
  SameSite=Strict` (verify in the browser devtools cookie
  inspector — DO NOT screenshot the cookie value).
- [ ] `GET /api/v1/auth/me` returns the bootstrapped user.
- [ ] Password change works:
  `POST /api/v1/auth/change-password` via the
  `PasswordPanel.svelte` UI succeeds. OTHER sessions are
  revoked; the calling session stays valid.
- [ ] Session list (`AuthSessionsPanel.svelte`) shows the
  current session.
- [ ] Revoke-other-sessions affordance works; current cookie
  stays valid afterwards.
- [ ] Logout clears the cookie and a subsequent
  `GET /api/v1/auth/me` returns `401`.
- [ ] **CSRF / `Origin` negative check.** From the operator
  workstation:
  ```sh
  curl -i -X POST \
    -H 'Content-Type: application/json' \
    -H 'Origin: https://attacker.example.com' \
    -b cookies.txt -d '{}' \
    https://<origin>/api/v1/hosts
  ```
  returns `403 csrf_origin_mismatch`. The body MUST NOT echo
  the offered `Origin` value. → runbook §10.

## 7. Inventory checks

Walk the inventory rows end-to-end in the SPA. Every test-id
in parentheses is a stable hook from `apps/web/e2e/SMOKE.md`.

- [ ] Create an SSH identity. Either:
  - Generate (Ed25519) via `IdentitiesView.svelte`, OR
  - Import (Ed25519, unencrypted OpenSSH) per
    [`docs/private-key-import.md`](private-key-import.md). v1
    supports Ed25519 unencrypted ONLY; encrypted /
    RSA / ECDSA / DSA private-key import is post-v1.
- [ ] After identity creation, confirm the identity detail
  panel shows NO `private_key`, NO `encrypted_private_key`, NO
  raw PEM bytes, NO `BEGIN OPENSSH PRIVATE KEY` substring
  anywhere on the page. Only public-key metadata / fingerprint.
- [ ] Create a host. (`servers-create-host-*`).
- [ ] Edit the host. (`host-detail-edit-open`,
  `host-detail-edit-{display-name,hostname,port,username}`).
  Verify the change persists.
- [ ] Host-key preflight + trust against the target.
  (`HostKeyPanel.svelte`). The trust step writes one audit row
  with public-key fingerprint metadata only — no key bytes, no
  peer banners, no russh error text.
- [ ] Create a server profile binding the host + identity.
  (`servers-create-profile-*`).
- [ ] Edit the profile metadata. (`profile-detail-edit-open`,
  `profile-detail-edit-{name,host,identity,
  username-override,tags}`). An empty save surfaces "change at
  least one field" — the safe-formatter, not an error echo.
- [ ] Auth-check (`AuthCheckPanel.svelte`) succeeds against the
  target.
- [ ] **Delete an unused host.** (`host-detail-delete-open`,
  `host-detail-delete-confirm-input`,
  `host-detail-delete-confirm-submit`). Succeeds.
- [ ] **Delete an unused server profile.**
  (`profile-detail-delete-open`,
  `profile-detail-delete-confirm-input`,
  `profile-detail-delete-confirm-submit`). Succeeds.
- [ ] **Referenced-conflict copy is correct.** Attempt a host
  delete while a server profile references it: returns
  `409 referenced`; the SPA renders the safe-formatter copy
  from `describeDeleteHostError` ("still used by a saved server
  profile or has trusted host keys"). Attempt a server-profile
  delete while terminal-session history references it: returns
  `409 referenced`; the SPA renders the safe-formatter copy
  from `describeDeleteServerProfileError` (routes the operator
  to "disable it instead"). Wire `message` MUST NOT appear in
  the UI verbatim.
- [ ] **Disable a server profile that has session history.**
  Profile transitions to `disabled`. A subsequent launch
  attempt against the disabled profile is refused at the API
  boundary with a clean error. Re-enable returns the profile to
  `active`; launches succeed again.
- [ ] **Audit rows carry public metadata only.** Open
  `RecentActivityPanel.svelte`. For every lifecycle event from
  the rows above, the payload is the field-by-field metadata
  shape — no `encrypted_private_key`, no plaintext PEM, no raw
  russh / DB error text, no peer banners, no terminal I/O, no
  `client_info` blobs, no `data_b64`. → AGENTS.md "Things to
  avoid" rows on `audit_events.payload`.

> Cross-reference for B1: every row above was already walked
> against the throwaway staging slot on 2026-05-17 — see
> [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
> § "2026-05-17 · `docs/inventory-edit-delete-ui-staging-smoke`"
> rows A + B + C + D + E + F + G. The §7 ticks here re-walk
> the same rows against the operator's real production
> hostname.

## 8. Terminal checks

Default xterm path only. Renderer evaluation is POST-V1.

- [ ] Launch an xterm session against the trusted, auth-checked
  profile from §7. Target either a real production host the
  operator owns OR a safe throwaway host reachable from the
  production backend. → `apps/web/e2e/SMOKE.md` § D for the
  renderer-fair input methodology.
- [ ] Prompt appears in the terminal viewport.
  (`production-terminal-viewport`).
- [ ] Focus the terminal
  (`production-terminal-focus`); the focus marker
  `data-renderer-input="marked"` is on the viewport's helper
  input target.
- [ ] Run `whoami` — output matches the configured username.
- [ ] Run `pwd` — output matches the expected home directory.
- [ ] Window resize works. Resize the SPA window; output reflows;
  no replay-window-lost frame appears.
- [ ] **Detach / reconnect inside `DETACHED_LIVE_PTY_TTL`.**
  Close the SPA tab; reopen within the configured TTL (default
  30s). Session reattaches; missed output is replayed from the
  in-memory ring (`emit_replay_range`); no `replay_window_lost`
  marker.
- [ ] Idempotent close. `SessionsView.svelte` close affordance
  transitions the row to `closed`. A second close call against
  the same `terminal_session_id` succeeds (idempotent), no
  audit row duplication.
- [ ] Sessions list (`SessionsView.svelte`) shows the closed
  session in `closed` state with public metadata only (no
  payload, no `client_info`).
- [ ] **Launch timing diagnostics sanity.** With the SPA debug
  panel open during launch, the diagnostics counters increment
  (POST→WS-open→attach phase markers). No timing field carries
  terminal payload bytes. → commit `ee89764`; verified across 5
  surfaces, latest 2026-05-17 multi-run resmoke.
- [ ] **No terminal payload in localStorage / sessionStorage /
  logs / audit.** In the browser devtools:
  - `localStorage` and `sessionStorage` contain no terminal
    output bytes, no `data_b64`, no PTY chunks.
  Back on the host:
  ```sh
  docker compose logs --tail=2000 relayterm-backend | \
    grep -E 'data_b64|relayterm_session=[A-Za-z0-9_-]{20,}' \
    || echo "ok: no leakage sentinels found"
  ```
  Recording is OFF by default at v1, so the recording-disabled
  path is also exercised — no `terminal_recording_chunks`
  inserts (verify in DB if curious; not a release blocker).

## 9. Mobile portrait sanity (B3)

Short operator checklist against the production xterm path on a
real Android phone. This row maps directly to cutline blocker B3.
**Do NOT extend into the wterm / mobile-renderer matrix** — that
is POST-V1.

Setup:

- A real Android phone with up-to-date Chrome.
- The production hostname from §3 reachable from the phone's
  network (over LTE/Wi-Fi; not via the operator's laptop CDP
  tunnel — that channel is for diagnostic capture, not a
  cutline smoke).

Walk:

- [ ] Open the production hostname (`https://<origin>`) in
  Android Chrome.
- [ ] Login via the SPA login form. Soft keyboard pops; email +
  password entry succeed; dashboard loads.
- [ ] Open the mobile navigation drawer
  (`mobileNavOpen` toggle); navigate `Servers` → server-profile
  list shows; navigate `Sessions` → session list shows.
- [ ] From the server profile chosen in §7, **launch the default
  xterm session.** No renderer-fallback panel; data-renderer
  resolves to `xterm`.
- [ ] Tap the terminal viewport; soft keyboard opens.
- [ ] Type `whoami` only; output appears. Do NOT type secrets,
  passwords, or anything you would not paste into a chat log —
  the goal is reachability, not load testing.
- [ ] (If practical) Background the tab briefly, return inside
  `DETACHED_LIVE_PTY_TTL`; session reattaches with replay. Skip
  if the phone aggressively suspends the tab.
- [ ] No catastrophic layout blocker: the terminal viewport is
  visible above the soft keyboard; the nav drawer closes; the
  sessions list row is tappable.
- [ ] Record any polish issues (typography, paste affordance,
  soft-keyboard sizing, scrolling glitches) as a deferred
  post-v1 list, NOT as a v1 blocker. Polish past "usable" is
  explicitly POST-V1 per cutline §6.

> **Privacy gotcha for screenshots and remote-DOM reads.** If you
> use Android Chrome's USB DevTools to capture evidence, filter
> `/json/list` strictly to the RelayTerm hostname before forwarding
> the output anywhere — see `apps/web/e2e/SMOKE.md` § D → "Real-
> phone DOM read via USB DevTools (CDP attach)" → "Privacy
> gotchas". Do NOT capture the tab switcher, password-manager
> overlay, or non-RelayTerm tabs.

## 10. Backup / restore / rollback checks

The operator can take a backup, restore from one, and roll back
the image tag without ad-hoc improvisation.

- [ ] **Pre-deploy backup recorded.** The pre-deploy
  `pg_dump -Fc` from §5 is written to off-host storage. Record
  the path / object key next to the deploy log entry.
- [ ] Config (`/.env` + reverse-proxy config) backed up to the
  same off-host location with `chmod 600` preserved.
- [ ] Image-tag rollback target identified. Both the previous
  `vX.Y.Z` AND a `sha-<short>` so the rollback is unambiguous.
  Rollback procedure: → runbook §6.1
  (`sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=sha-abc1234/' .env`
  then `docker compose pull` then
  `docker compose up -d --no-deps relayterm-backend relayterm-web`).
- [ ] **Migration rollback caveat acknowledged.** Backward-
  compatible migrations roll back by image tag alone. For a
  backward-incompatible migration, the documented path is
  restore-from-backup; no automated `migrate down`. → runbook
  §6.2.
- [ ] Restore-from-backup procedure read end-to-end. → runbook
  §8 + §6.2, plus the operator-facing manual procedure in
  [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md)
  §5 (restore) + §6 (rollback). If a restore rehearsal has not
  been run against a throwaway Postgres at least once, mark
  this as a **REQUIRED POST-V1 FOLLOW-UP** in the §12 decision
  table (slice: `docs/backup-restore-rehearsal-record`).
  Restoring from a never-tested backup on a real incident is
  not acceptable for v1 hygiene; rehearse before relying on it.
- [ ] Backup location recorded in the §13 sign-off template.

The full operator-facing backup, restore, and rollback procedure
(including the protect-list, the sensitive-material warning, the
recording-specific caveat, the rehearsal record template, and
the v1 / post-v1 non-goal split) lives in
[`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md).
The runbook is docs-only; it composes runbook §4 / §6 / §7 / §8
and this checklist's §10 / §11 into a single page. If you have
not walked it once before today's deploy, walk it now — at
minimum read §4 (pre-upgrade backup) and §5 (restore) end-to-end.

## 11. Security / redaction checks

The release gate on payload leakage. Run these AFTER §6–§9 (so
real user / terminal / session data has crossed the system).

- [ ] **Bounded backend log sweep.** From the host:
  ```sh
  docker compose logs --tail=2000 relayterm-backend | \
    grep -E 'relayterm_session=[A-Za-z0-9_-]{20,}|encrypted_private_key|data_b64|BEGIN OPENSSH PRIVATE KEY|token_hash' \
    || echo "ok: no leakage sentinels found"
  ```
  MUST print `ok: no leakage sentinels found`. Any hit is a
  security regression — STOP, do not declare v1 shipped.
- [ ] **Bounded web log sweep** (nginx access log inside the
  `relayterm-web` container). Same redaction sentinels; same
  decision rule.
- [ ] **Audit-payload sweep.** Query
  `audit_events.payload` for the §6 + §7 + §8 window:
  ```sh
  docker compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c \
    "SELECT id, kind FROM audit_events WHERE created_at > now() - interval '1 hour' \
     AND (payload::text ~ 'encrypted_private_key|private_key_openssh|BEGIN OPENSSH PRIVATE KEY|data_b64|token_hash|client_info') \
     ORDER BY id;"
  ```
  Expected output: `(0 rows)`. Any row is a leak — `payload`
  must carry public metadata only. → AGENTS.md "Things to
  avoid" `audit_events.payload` row;
  `docs/agent/redaction-rules.md` §§ 1, 4, 5, 10, 11, 12.
- [ ] **No `private_key_openssh` substring** anywhere in any
  HTTP response body the operator hit during §6–§9 (Network
  panel inspection in browser devtools; saved JSON responses).
- [ ] **No `encrypted_private_key` substring** in any response
  body. The vault-encrypted bytes are durable in
  `ssh_identities.encrypted_private_key` but MUST NOT cross the
  API boundary for read or list calls.
- [ ] **No `BEGIN OPENSSH PRIVATE KEY` substring** anywhere on
  any page in the SPA. Open the browser devtools `Elements`
  panel, search the rendered DOM tree.
- [ ] **No session-token plaintext / no cookie value / no
  `token_hash` / no password substring** in any log / Error /
  response / UI cell / `data-*` attribute. The session token
  plaintext lives in the cookie ONLY; storage and lookup are
  by SHA-256 `token_hash`.
- [ ] **No `data_b64` substring** in any log / audit row /
  response body / DOM. Terminal-recording chunks cross the
  wire ONLY through `TerminalRecordingChunkResponse::data_b64`
  and are NEVER logged.
- [ ] **No terminal payload in the recording-disabled path.**
  Because recording is OFF by default at v1 (per §4), no
  `terminal_recording_chunks` rows should have been inserted
  during the §8 walk. Spot-check:
  ```sh
  docker compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c \
    "SELECT count(*) FROM terminal_recording_chunks WHERE created_at > now() - interval '1 hour';"
  ```
  Expected: `0`. (Non-zero is only acceptable if the operator
  has explicitly opted recording IN per §4 — re-read
  `docs/terminal-recording.md` before deciding the row passes.)
- [ ] **Screenshot privacy caution recorded.** Any screenshots
  taken during the §6–§9 walk MUST be reviewed for: session
  cookie values, terminal output (filenames, hostnames you do
  not want public), `Authorization` headers in Network panel,
  Android tab switcher (which can leak non-RelayTerm tab
  titles per the encountered lesson on 2026-05-16e). When in
  doubt: do not commit the screenshot. Privacy gotcha for
  Android USB DevTools attach: `apps/web/e2e/SMOKE.md` § D →
  "Privacy gotchas".

## 12. Release decision table

Each gate is one row. Status values:

| Status | Meaning |
|---|---|
| **PASS** | Walked end-to-end; evidence committed. |
| **PENDING** | Required for v1; not yet walked / recorded. |
| **BLOCKED** | Required for v1; a dependency is missing or failed. |
| **POST-V1** | Deliberately deferred; not on the v1 critical path. |

| Gate | Required evidence | Status | Blocking? | Link to evidence |
|---|---|---|---|---|
| Repo pre-release checks (§3) | CI green; baseline checks pass; doc contracts pass | PENDING | Yes | This release log entry |
| Configuration checks (§4) | `.env` review, reverse-proxy review | PENDING | Yes | This release log entry |
| Deployment checks (§5) | Compose stack healthy on production host | PENDING | Yes | This release log entry |
| First-user / auth checks (§6) | Bootstrap → login → me → password change → session list → revoke → logout → CSRF negative | PENDING | Yes | This release log entry |
| Inventory walk on staging (B1, cutline §9) | Operator-walked staging UI smoke 2026-05-17 (rows A–I) | **PASS** | No (staging evidence; production re-walk happens in §7) | [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) § "2026-05-17 · `docs/inventory-edit-delete-ui-staging-smoke`" |
| Inventory walk on production (§7) | Production-host re-walk of B1 rows | PENDING | Yes | This release log entry |
| Terminal walk on production (§8) | Production-host xterm launch / I/O / reattach / close | PENDING | Yes | This release log entry |
| Production-walked end-to-end smoke (B2, cutline blocker) | Operator-recorded production smoke entry against a real production hostname | **PENDING** | Yes | Template skeleton landed 2026-05-17 at [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md) (NOT EXECUTED — template only); next slice walks §5 of that file against a real production hostname (see §14) |
| Mobile portrait sanity on default xterm (B3, cutline blocker) | Operator-recorded Android Chrome walk per §9 | **PENDING** | Yes | Next slice: `docs/v1-mobile-portrait-sanity-smoke` (see §14) |
| Backup / restore / rollback (§10) | Pre-deploy `pg_dump` + config backup off-host; rollback tag known; backup/restore runbook walked | PENDING | Yes | This release log entry; runbook at [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md) |
| Restore-from-backup rehearsal | Operator-recorded restore against a throwaway Postgres at least once (runbook §5 Case R-B; template at runbook §10) | **PENDING** | Yes (minimum: backup/restore runbook committed — DONE 2026-05-17 at [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md); rehearsal still pending) | Next slice: `docs/backup-restore-rehearsal-record` (see §14) |
| Security / redaction sweep (§11) | Log + audit-payload sentinels return zero hits | PENDING | Yes | This release log entry |
| Experimental renderer promotion | Any flip of the production default away from xterm | **POST-V1** | No | [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md) |
| Production renderer selector UI | Operator-selectable renderer in production | **POST-V1** | No | Cutline §6 |
| Tauri desktop / Android release packaging | Signed CI release artifacts, Play Store / App Store bundling | **POST-V1** | No | [`docs/deployment/tauri-ci-release-plan.md`](deployment/tauri-ci-release-plan.md) |
| Recording UI toggle / export / search | In-product enable / disable / export / search UI | **POST-V1** | No | [`docs/terminal-recording.md`](terminal-recording.md) |
| Passkeys / WebAuthn, password reset, IP-aware throttling | Auth surface beyond v1 envelope | **POST-V1** | No | [`docs/spec/auth.md`](spec/auth.md) |
| Multi-user / admin / RBAC | Beyond single-tenant | **POST-V1** | No | Cutline §2 |
| Multi-instance / HA / Kubernetes deploy | Shared throttler / shared orchestrator | **POST-V1** | No | Cutline §2 |

The release is **shippable when every "Blocking? = Yes" row is
PASS**. POST-V1 rows are deliberately deferred; they do not gate
the release.

## 13. Final sign-off template

Copy / paste into the release log entry (suggested location:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
sibling, or a new `docs/deployment/production-deploys.md`).

```
## YYYY-MM-DD · v1 release sign-off

- Production hostname: https://<origin>
- Image tag deployed:  vX.Y.Z   (commit sha-<short>)
- Migration version:   <latest applied migration ID>
- Rollback tag:        vX.Y.(Z-1)   (commit sha-<short>)
- Smoke date / time:   YYYY-MM-DDThh:mm:ssZ
- Operator:            <name / handle>
- Pre-deploy backup:   <off-host path / object key>
- Config backup:       <off-host path / object key>
- Recording enabled:   no   (or: yes — and re-read docs/terminal-recording.md)
- Experimental renderer gate: off   (default)

Checks walked:
- [ ] §3 Repo pre-release          PASS
- [ ] §4 Configuration             PASS
- [ ] §5 Deployment                PASS
- [ ] §6 First-user / auth         PASS
- [ ] §7 Inventory (production)    PASS
- [ ] §8 Terminal (production)     PASS
- [ ] §9 Mobile portrait sanity    PASS         (resolves B3)
- [ ] §10 Backup / restore / rollback   PASS
- [ ] §11 Security / redaction     PASS

Known caveats (post-v1, not blocking):
-
-

Decision: SHIP  /  HOLD
Notes:
```

Commit this entry to the release log on the same day as the
deploy; do not let it sit uncommitted.

## 14. Next slices

Concrete, actionable follow-on slices, ranked by what most
moves the needle. Order matches cutline §7.

1. **`docs/v1-production-smoke-record`** (resolves B2). The
   operator-recorded production-walked smoke entry against a
   real production hostname. The template skeleton landed
   2026-05-17 at
   [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
   (NOT EXECUTED; status NOT YET EXECUTED — operator had not
   yet chosen a production hostname). The next slice copies §5
   of that file into a new dated entry and walks §3–§8 + §10–
   §11 of this checklist against the real production
   hostname. The staging template is the 2026-05-17 entry in
   [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
   — do NOT co-mingle the staging log with the production
   log.
2. **`docs/v1-mobile-portrait-sanity-smoke`** (resolves B3).
   One operator-recorded run on a real Android phone against
   the default xterm production path using §9 of this
   checklist + `apps/web/e2e/SMOKE.md` § D as the input
   methodology. NOT a wterm / mobile-renderer matrix.
3. ~~**`docs/backup-restore-runbook`**~~ **DONE — 2026-05-17.**
   Landed on `docs/backup-restore-runbook` as
   [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md).
   Full operator-facing backup / restore / rollback procedure;
   composes runbook §4 / §6 / §7 / §8 and this checklist's
   §10 / §11 into a single page (protect-list, sensitive-
   material warning, pre-upgrade backup, restore, rollback,
   migration caveat, recording caveat, verification checklist,
   rehearsal record template, v1 / post-v1 non-goal split).
   Docs-only; no code / schema / deploy change.
   **Pair (still pending):** the operator-recorded
   `docs/backup-restore-rehearsal-record` (Case R-B restore
   against a throwaway Postgres, per the new runbook §5.0 +
   §10 template). The runbook closes the documentation gap;
   the rehearsal closes the verification gap. The
   release-checklist §10 / §12 row "Restore-from-backup
   rehearsal" stays PENDING until the rehearsal entry lands.
4. **`docs/v1-release-notes-draft`** (or equivalent). The
   user-facing changelog for the v1 tag: what is in v1, the
   explicit post-v1 list, the upgrade caveats, the operator
   smoke summary. Drafted against this checklist's §12
   decision table.

Honourable mentions (not v1-critical):

- `feat/operator-status-page` — a small operations page in
  Settings surfacing healthcheck status, effective quotas, and
  recording on / off. Handy at deploy time but not v1-required.

Deliberately NOT recommended as next slices:

- Any renderer-promotion or experimental-renderer matrix
  smoke.
- Tauri release-packaging work.
- Recording UI toggle / export.
- Any auth / multi-user / RBAC expansion.

---

## See also

- [`docs/v1-production-readiness.md`](v1-production-readiness.md)
  — the v1 cutline this checklist composes into a release
  gate.
- [`docs/deployment/production-runbook.md`](deployment/production-runbook.md)
  — the load-bearing operator runbook. §3 (tag policy), §4
  (first deploy), §6 (rollback), §7 (migration), §8 (backup /
  restore), §9 (reverse proxy), §10 (post-deploy smoke), §11
  (secret rotation).
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — staging smoke history; the 2026-05-17 inventory edit /
  delete entry is the B1 evidence cited in §12.
- [`docs/deployment/v1-production-smoke.md`](deployment/v1-production-smoke.md)
  — v1 production smoke log; template skeleton landed
  2026-05-17 (NOT EXECUTED). Future production-walked smoke
  entries land here as dated sections; entries here are the
  B2 evidence the §12 decision table is gated on.
- [`docs/deployment/backup-restore-runbook.md`](deployment/backup-restore-runbook.md)
  — operator-facing backup, restore, and rollback procedure;
  closes the §10 "minimum manual process" gap. Pair with the
  future `docs/backup-restore-rehearsal-record` slice.
- [`docs/deployment/docker-compose.md`](deployment/docker-compose.md)
  — Compose stack reference, env contract, reverse-proxy
  notes.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) — the
  SPA smoke runbook; §D is the input methodology for §8 + §9.
- [`docs/spec/inventory.md`](spec/inventory.md) — destructive-
  action policy (host / profile / identity / session); the
  rules the §7 walk relies on.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
  — xterm-as-default rule and the experimental-renderer gate
  contract referenced in §2 + §4.
- [`docs/private-key-import.md`](private-key-import.md) — v1
  Ed25519 unencrypted OpenSSH constraint cited in §7.
- [`docs/terminal-recording.md`](terminal-recording.md) — the
  off-by-default recording posture cited in §4 + §11.
- [`docs/production-auth.md`](production-auth.md) — auth
  envelope detail; the "lost the only password" recovery path
  cited indirectly via runbook §11.
