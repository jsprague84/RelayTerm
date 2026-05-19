# RelayTerm — First production deployment plan

> Short, operator-specific plan for the **first** RelayTerm v1
> personal production deploy. Composes the production Compose
> template, the v1 release-day checklist, the production runbook,
> and the backup / restore runbook into a single tight page the
> operator can walk in one sitting.
>
> This is a **planning document, not a runbook**. It says what to
> decide, what to deploy, what to check, and what counts as
> "ready for personal use." The load-bearing procedure detail
> stays in the upstream runbooks; chase each `→` link when you
> walk a step.

## 1. Status

- **Document status:** PLANNING. Drafted on
  `docs/first-production-deploy-plan` against the snapshot of
  `main` at commit `065af12` ("feat(deploy): add production
  compose template").
- **No production deployment has been executed yet.** No
  production hostname has been chosen, no `.env` populated, no
  image pulled, no migration run, no health probe hit. v1 cutline
  blockers B2 (production smoke) and B3 (mobile portrait sanity)
  both remain **PENDING** — see
  [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  §5.
- **Intended use.** The operator (single self-hosted user) reads
  this once before the first personal production deploy, decides
  the §2 items, executes the §4 outline against the production
  Compose template, walks the §5 smoke, records the result in
  [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
  (which simultaneously resolves B2).
- **What this doc does NOT do.** It does not introduce new code,
  schema, migrations, routes, CSP changes, CI changes, renderer
  promotion, or any source change. It does not re-derive
  procedure that is already in
  [`production-runbook.md`](production-runbook.md),
  [`backup-restore-runbook.md`](backup-restore-runbook.md), or
  [`v1-release-checklist.md`](../v1-release-checklist.md) — it
  composes them.
- **Upstream wins on disagreement.** Where this plan and any
  upstream contract (`AGENTS.md`, `SPEC.md`, `docs/spec/*`, the
  runbooks above, or
  [`v1-release-notes.md`](../v1-release-notes.md)) disagree, the
  upstream contract wins and this plan is the bug.

## 2. Decisions to make before deploy

Walk this top to bottom. Every row needs an answer before §4
starts. Defaults are conservative; flip only with a reason.

- [ ] **Production hostname.** The byte-equality value the
  browser will use (e.g. `https://relay.example.com`). Drives
  `RELAYTERM_AUTH__ALLOWED_ORIGINS`, the outer reverse-proxy
  routing rule, and the TLS cert. Lower-case; no trailing slash;
  no path.
- [ ] **Deploy host.** The box that runs Docker Compose
  (VPS / homelab / NAS / laptop you trust). Must be reachable
  from the operator workstation. Recommend a host the operator
  already keeps backed up + patched.
- [ ] **Compose / project path on the deploy host.** Conventional
  value: `/srv/relayterm/`. Anywhere is fine as long as the
  operator account owns it and `.env` lives there with
  `chmod 600`.
- [ ] **Public internet vs VPN-only.** Either is supported. Public
  internet means TLS-terminating outer proxy + a public DNS
  record + likely Let's Encrypt; VPN-only / WireGuard-only avoids
  the TLS-on-public-DNS surface but you still need TLS on the
  outer proxy because the production envelope requires
  `cookie_secure = true`.
- [ ] **Reverse proxy choice.** Traefik / Caddy / outer nginx —
  pick the one the operator already runs. The production Compose
  template ships **without** routing hardcoded; the loopback port
  mapping (`127.0.0.1:8081`) is the only ingress and the outer
  proxy terminates TLS for the public origin. → runbook §9 +
  [production Compose template](../../deploy/docker-compose.production.example.yml)
  comment block.
- [ ] **Image tag / digest to deploy.** A release `vX.Y.Z` if one
  exists; otherwise an immutable `sha-<short>`. `:main` is for
  staging only. `:latest` does not exist. Record the digest too —
  it is the unambiguous rollback target. → runbook §3.
- [ ] **Recording on/off.** Default and strongly recommended for
  the first deploy is **OFF**
  (`RELAYTERM_TERMINAL_RECORDING__ENABLED=false`). If the
  operator intends to flip it ON, re-read
  [`docs/terminal-recording.md`](../terminal-recording.md)
  end-to-end first — chunk bytes carry full terminal output and
  are plaintext at rest at v1.
- [ ] **Generate-only identities acceptable for v1?** Default is
  **yes** (generate-only). Private-key import is wired (Ed25519
  unencrypted OpenSSH only) but generating fresh keys for the
  first deploy avoids importing a personal / production-critical
  key while you are still smoke-testing the stack. → cutline §10
  "Open questions"; [`docs/private-key-import.md`](../private-key-import.md).
- [ ] **Backup storage location.** Off-host path or object key
  the pre-deploy `pg_dump -Fc` and the `.env` / Compose archive
  will land on. **Not** inside the compose-dir; **not** inside
  the `relayterm-pgdata` volume; **not** on the same single
  failure domain as the database when feasible. →
  [`backup-restore-runbook.md`](backup-restore-runbook.md) §3 +
  §4.7.
- [ ] **Rollback tag.** The previous `vX.Y.Z` AND `sha-<short>`
  (and matching image digest) you would re-pin if the new image
  misbehaves. On a true *first* deploy there is no in-place
  rollback — recovery is "restore from the dump you took before
  bootstrap, redeploy clean." Record that explicitly. → runbook
  §6.1.
- [ ] **Mobile use required on day one?** Default: **no**. v1
  cutline B3 (mobile portrait sanity) is deliberately deferred to
  production-use-driven evidence
  (recorded operator preference: "Defer v1 B2/B3 manual walks").
  The Android Chrome SPA is expected to work because the staging
  resmokes have it green, but the first production deploy is
  desktop-only unless the operator opts in.

## 3. Recommended first-deploy posture

Opinionated defaults for the first personal production deploy.
Each is set to "minimum-blast-radius" until real usage proves it
out.

- **Default xterm only.** xterm.js is the v1 production default
  on every surface. Do not flip the renderer default. → cutline
  §8; [`docs/spec/terminal-adapters.md`](../spec/terminal-adapters.md).
- **Experimental renderer gate OFF.** Leave
  `experimentalRendererEvaluationEnabled` (Settings) **off** on
  the production deploy. The gated lazy loader is for personal
  evaluation, not for first-deploy stability. → release-checklist
  §4 last bullet.
- **Recording OFF unless explicitly needed.** Default state of
  `RELAYTERM_TERMINAL_RECORDING__ENABLED`. The retention worker
  may stay on for housekeeping (it is a no-op when there are no
  chunks). → release-checklist §4.
- **Public hostname goes live only after TLS / reverse proxy /
  Origin / CSRF are correct.** Walk the §5 smoke against the
  loopback first; do not publish DNS until `Origin` preservation,
  WS upgrade headers, and long `proxy_read_timeout` are verified
  → runbook §9.
- **Generate a fresh RelayTerm SSH identity for the first
  smoke.** Do not import a personal / production-critical private
  key just to validate the launch path. Use a throwaway Ed25519
  identity created via `IdentitiesView.svelte`.
- **Safe throwaway SSH target first, then real personal hosts.**
  Easiest: stand up a one-shot
  `lscr.io/linuxserver/openssh-server` (or equivalent) container
  the production backend can reach, smoke against it, tear it
  down. Real personal hosts come next — use `whoami` / `pwd`
  only, no destructive commands, during the first walk. → v1
  production smoke §3 "Safe SSH target strategy".
- **Take a DB / config backup before any meaningful use.** After
  the bootstrap + first xterm smoke pass, but BEFORE you add
  real hosts / identities you cannot easily recreate, run
  [`backup-restore-runbook.md`](backup-restore-runbook.md) §4
  end-to-end. The first dump is the recovery floor for everything
  that follows.

## 4. Step-by-step deploy outline

High level + executable. Each step links to the upstream runbook
detail for the exact command and the upstream caveat.

1. **Create the deploy directory** the operator account owns
   (example: `/srv/relayterm/`). → runbook §4.1.
2. **Copy the production Compose template** to
   `<compose-dir>/docker-compose.yml`. Source:
   [`deploy/docker-compose.production.example.yml`](../../deploy/docker-compose.production.example.yml).
   Do NOT use the staging Traefik template on a production host
   (hardcoded staging hostname + `secure-chain@file` middleware +
   external `proxy` network — all staging-specific).
3. **Create the production `.env`** from
   [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example),
   `chmod 600`. → runbook §4.2.
4. **Fill every required secret; no `CHANGE_ME` placeholders.**
   Generate session signing key (32B base64), vault master key
   (32B base64; **must differ** from session signing key),
   bootstrap token (URL-safe base64), Postgres password.
   Suppress shell history while generating + pasting. Production
   mode **refuses to boot** if any secret-shaped field still
   contains the literal `CHANGE_ME` substring — do NOT work
   around the refusal. → runbook §4.3 + §4.4;
   `apps/backend/src/config.rs::Config::validate_production_secrets`.
5. **Set `RELAYTERM_IMAGE_TAG`** to the immutable pin decided in
   §2 (`vX.Y.Z` or `sha-<short>`; digest pin if you want
   reproducibility). → runbook §3 + §4.5.
6. **Set the trusted origin / hostname.**
   `RELAYTERM_AUTH__ALLOWED_ORIGINS = https://<production-host>`
   byte-for-byte. `RELAYTERM_AUTH__MODE = production`.
   `RELAYTERM_AUTH__COOKIE_SECURE = true`. → release-checklist
   §4.
7. **Configure the reverse proxy.** Outer Traefik / Caddy / nginx
   terminates TLS for the public origin and forwards to
   `127.0.0.1:8081`. Required posture: `Origin` passes through
   unmodified (nginx needs `proxy_set_header Origin
   $http_origin;`); `Upgrade` / `Connection` headers honoured for
   `/api/`; `proxy_read_timeout 3600s` (and matching
   `proxy_send_timeout`) on the `/api/` location;
   `proxy_buffering off` on the WS location. Production CSP stays
   strict `default-src 'self'` — do **not** copy the staging
   `'wasm-unsafe-eval'` relaxation. → runbook §9 +
   release-checklist §4.
8. **Log in to the registry (if private) and pull the pinned
   images.** Render once with `docker compose config` to confirm
   env interpolation is clean (every `${...:?}` placeholder fails
   loudly here if a required env var is missing), then
   `docker compose pull`. → runbook §4.6 + §4.7.
   (Postgres starts automatically via Compose `depends_on:
   service_healthy` when the next step or step 10 runs — there is
   no explicit "start Postgres first" action; the guard
   serialises boot order against the `pg_isready` healthcheck.)
9. **Apply migrations via the profile-gated one-shot.**
   `docker compose --profile migrate run --rm relayterm-migrate`.
   Exit code 0; final line shows the applied migration ID. The
   backend does NOT auto-migrate on boot. → runbook §4.8 + §7.
10. **Bring up backend + web.** `docker compose up -d postgres
    relayterm-backend relayterm-web`. `docker compose ps` should
    show all three at `running` / `(healthy)`. Note `(healthy)`
    on `relayterm-backend` is process-alive only; the `postgres`
    row is DB-side liveness. → runbook §4.9 + §10.
11. **Check health endpoints.** From the loopback:
    `curl -sf http://127.0.0.1:8081/_web_health` → `ok`;
    `curl -sf http://127.0.0.1:8081/healthz` →
    `{"status":"ok"}`; `curl -i
    http://127.0.0.1:8081/api/v1/auth/me` (unauthenticated) →
    `401`. **`401` is the expected pass condition** — a `2xx`
    here is a security regression and means the auth gate is
    missing. → release-checklist §5 + runbook §10.
12. **Bootstrap the first user.** `POST
    /api/v1/auth/bootstrap` with the configured token through
    the public origin. Returns `201`. → runbook §4.10.
13. **Close the bootstrap window.** Unset
    `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` in `.env` and
    restart the backend (`docker compose up -d --no-deps
    relayterm-backend`). A subsequent bootstrap POST is then
    refused. → runbook §4.11.
14. **Login through the SPA.** Confirm dashboard loads.
15. **Open the Operational Status panel.** Settings →
    Operational Status
    (`[data-testid="settings-operational-status"]`). Surfaces
    backend reachability, terminal session counts by status, the
    effective deployment quotas, experimental-renderer gate
    posture (should read OFF), and the read-only "next session
    will mount X" line (should read xterm). The panel does NOT
    substitute for the §5 smoke. → cutline §7 honourable mention
    "feat/operator-status-page".

## 5. First smoke after deploy

Maps to the v1 production smoke entry template at
[`v1-production-smoke.md`](v1-production-smoke.md) §5. Walking
this section AGAINST the real production hostname and recording
the result there is exactly what resolves cutline blocker B2.

Walk these rows in order. STOP at the first failure; do not paper
over with retries. The rows below are the key paths; the
full per-row matrix (including the CSRF / `Origin` negative check,
session list / revoke, window resize, and launch timing
diagnostics) lives in
[`v1-production-smoke.md`](v1-production-smoke.md) §5 — fill in
every row of that template, not only the ones inlined here, before
committing the entry that resolves B2.

- **`/healthz` returns `200 {"status":"ok"}`** from both loopback
  and the public origin. Static body; not DB readiness.
- **`/_web_health` returns `200 ok`** from both loopback and the
  public origin. Nginx-static; does not reach the backend.
- **`/api/v1/auth/me` without cookie returns `401`** — expected
  PASS condition; a `2xx` is the failure case. → release-checklist
  §5.
- **Login / logout / `/me`** through the SPA. After login, `/me`
  returns the user record; after logout, `/me` returns `401`;
  re-login for the rest.
- **Create an SSH identity** (generate Ed25519, do NOT import a
  personal key). Confirm the identity detail panel shows NO
  `private_key`, NO `encrypted_private_key`, NO raw PEM, NO
  `BEGIN OPENSSH PRIVATE KEY` substring. Public-key + SHA-256
  fingerprint only. → release-checklist §7.
- **Create a host + server profile** binding them. Use the
  throwaway SSH target as the host. → release-checklist §7.
- **Host-key preflight + trust.** `HostKeyPanel.svelte`. The
  trust step writes one audit row with public-key fingerprint
  metadata only — no key bytes, no peer banners, no russh error
  text.
- **Launch the xterm session** against the trusted,
  auth-checked profile. Prompt appears
  (`production-terminal-viewport`). `data-renderer` resolves to
  `xterm`; `data-renderer-fallback` is empty.
- **Type `whoami`, `pwd`, `echo relayterm-v1-prod-smoke`** —
  output matches; the echo string serves as a deterministic
  positive sentinel for the redaction sweep below. →
  v1-production-smoke §5 row I'.
- **Detach / reconnect inside `DETACHED_LIVE_PTY_TTL`.** Type
  `echo relayterm-v1-before-detach`, detach via the UI, reconnect
  from the Sessions list within the TTL (default 30s), type
  `echo relayterm-v1-after-reconnect`. Reattach replays missed
  output; no `replay_window_lost` marker. → v1-production-smoke
  §5 row K.
- **Close the session.** Idempotent; second close call is a
  no-op. Row transitions to `closed` and stays visible as
  historical metadata.
- **Redaction / log sweep.** Walk the v1-production-smoke §5.3
  sweep query templates against the backend log, the nginx
  access log, and the audit-payload over the smoke window.
  Expected: `ok: no leakage sentinels found` on the log greps,
  `count = 0` on the audit query. **One hit blocks PASS.** The
  canonical sentinel set (mirrors
  `AUDIT_FORBIDDEN_SUBSTRINGS` + cookie pattern + smoke positive
  sentinels) lives at
  [`v1-production-smoke.md`](v1-production-smoke.md) §5.1.

When the rows above all pass: copy
[`v1-production-smoke.md`](v1-production-smoke.md) §5 into a new
dated entry under that file's §6, fill in every row, commit. That
commit resolves cutline B2.

## 6. Backup and rollback before real use

Do this after the §5 smoke passes and BEFORE you add personal
hosts / identities you cannot easily recreate. The first dump is
the recovery floor for everything that follows.

- **Take a DB + config backup.** Walk
  [`backup-restore-runbook.md`](backup-restore-runbook.md) §4
  end-to-end: §4.4 `pg_dump -Fc` to the off-host location
  decided in §2; §4.6 optional SHA-256; §4.7 `.env` + Compose
  file (+ reverse-proxy config) into a `config-<tag>-<ts>/`
  archive with `chmod 600` preserved. Record the path in a
  deploy log entry.
- **Record image tag AND digest** for `relayterm-backend`,
  `relayterm-web`, and `relayterm-backend-migrate` (the
  `docker compose images` output is the source of truth). Record
  the final applied migration ID from
  `docker compose --profile migrate run --rm relayterm-migrate
  migrate info`. → backup-restore-runbook §4.2 + §4.3.
- **Record the rollback command verbatim.** `sed -i
  's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=<previous-tag>/'
  .env` → `docker compose pull` → `docker compose up -d
  --no-deps relayterm-backend relayterm-web`. This is the cheap
  rollback path. → runbook §6.1.
- **Do not rely on rollback through the old app if migrations
  are incompatible.** Migrations are forward-only by default at
  v1. Backward-incompatible schema means image rollback alone
  leaves the DB ahead of the code; the supported path is
  restore-from-backup + image-tag rollback. → runbook §6.2;
  backup-restore-runbook §6.2 + §7.
- **Keep `.env` and vault key backup protected.** The vault
  master key AEAD-wraps every stored SSH private key — its
  backup carries the same blast radius as a Postgres dump.
  Backup + vault key on the same single failure domain = full
  disclosure of every stored identity. → backup-restore-runbook
  §3.
- **The first dump is for self-recovery, not yet a rehearsed
  restore.** The restore-from-backup rehearsal slice
  (`docs/backup-restore-rehearsal-run` — see
  [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
  §12) is independent of this first deploy. The first production
  deploy may proceed without a recorded rehearsal — the rehearsal
  is **recommended**, not required, per v1 release-checklist §10
  / §12. The operator may elevate it to "required before first
  production deploy" at any time; doing so is a release-day
  judgement.

## 7. What counts as "ready for personal use"

Practical threshold. Every bullet must be true before treating
the deploy as the operator's day-to-day SSH terminal.

- **First user can log in.** Bootstrap succeeded, bootstrap
  token was unset, SPA login lands on the dashboard, `/me`
  returns the user.
- **Identity / host / profile flow works.** A throwaway Ed25519
  identity was created; a host was created; a server profile was
  created binding them; host-key trust + auth-check both
  succeeded.
- **xterm launches against at least one safe host.** Prompt
  appears in the terminal viewport; `whoami` + `pwd` return
  expected values; output reflows on resize.
- **Reconnect works within `DETACHED_LIVE_PTY_TTL`.** Detach,
  reattach within the TTL; missed output replays from the
  in-memory ring; no `replay_window_lost` marker on the
  reconnect.
- **Logs do not leak secrets.** The §5 redaction sweep returned
  `ok: no leakage sentinels found` on backend + nginx logs and
  `count = 0` on the audit payload query.
- **Backup exists.** The §6 `pg_dump -Fc` + config archive is
  written, sized non-zero, on off-host storage; the path is
  recorded.
- **Rollback tag is known.** The previous `vX.Y.Z` /
  `sha-<short>` + digest are recorded next to the backup
  artifact. (On a true first deploy this is a recovery anchor,
  not an in-place rollback — see §6.)
- **Operator accepts the known caveats.** §8 below; nothing on
  that list is surprising. If anything on §8 is unacceptable for
  the operator's threat model, fix it BEFORE relying on the
  deploy.

## 8. Known caveats accepted for first use

Stated plainly. None of these are "we will fix it before you
deploy"; all of them are post-v1 or
production-use-driven evidence and are the conscious posture for
the first personal deploy.

- **B2 production smoke becomes the first deploy record.** The
  §5 smoke walked against the real production hostname AND
  committed under
  [`v1-production-smoke.md`](v1-production-smoke.md) §6 IS the
  evidence that resolves B2. There is no separate "B2 smoke" run
  before the first deploy.
- **B3 mobile portrait sanity can happen during real use.** The
  default xterm path attaches cleanly under real Android Chrome
  in staging; the v1 cutline accepts production-use-driven
  evidence for the first deploy
  (recorded operator preference: "Defer v1 B2/B3 manual walks").
  Polish past "usable" is post-v1.
- **No signed Tauri release.** The desktop and Android shells
  exist as Tauri v2 scaffolds and can be built locally per
  [`tauri-local-build.md`](tauri-local-build.md). CI / signing /
  Play Store / App Store bundling are deferred per
  [`tauri-ci-release-plan.md`](tauri-ci-release-plan.md). The
  first production deploy is the web SPA only.
- **No renderer promotion.** xterm.js is the v1 default on every
  surface. Experimental renderers (`ghostty-web`, `wterm`,
  `restty`) remain dev-lab-only and reach the production shell
  only through the gated lazy loader; the gate stays OFF.
- **No restore rehearsal yet unless the operator chooses to run
  it first.** The restore-from-backup runbook is shipped and the
  rehearsal template exists; the operator-walked Case R-B
  rehearsal (`docs/backup-restore-rehearsal-run`) is
  **recommended before relying on production** but not required
  for the v1 ship gate. → release-checklist §10 +
  [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
  §11.
- **No multi-user / RBAC.** v1 is single-tenant by design. The
  bootstrap user owns everything; there is no admin role and no
  per-user permissions. → cutline §2.
- **Recording UI not part of day-one use.** Recording stays OFF
  by default. The writer, replay viewer, retention worker, and
  audit kinds are landed but enabling recording is a config +
  restart action; there is no in-product enable / disable /
  export / search UI in v1. → release-notes §3.

## 9. Actual deployment log template

Copy this block into the deploy log (suggested file:
`/srv/relayterm/deploy.log` on the deploy host, or a per-operator
ops journal) once the §5 smoke passes. This is intentionally a
**short** template — the rich evidence track is the
[`v1-production-smoke.md`](v1-production-smoke.md) §5 entry the
operator commits next to it.

```
## YYYY-MM-DD · RelayTerm first production deploy

- Date / time UTC      : YYYY-MM-DDThh:mm:ssZ
- Production hostname  : https://<origin>
- Deploy host          : <hostname / FQDN>
- Compose-dir path     : /srv/relayterm/   (or operator-chosen)
- Image tag deployed   : <vX.Y.Z>   (commit sha-<short>)
- Image digests        : relayterm-backend          sha256:<…>
                         relayterm-web              sha256:<…>
                         relayterm-backend-migrate  sha256:<…>
- Config choices       : recording=<off|on>
                         experimental gate=<off|on>
                         allowed_origins=https://<origin>
                         cookie_secure=true
                         vault_enabled=true
                         detached_pty_ttl_seconds=<value>
- Migration result     : OK   (final applied migration ID: <id>)
- Bootstrap result     : 201   (token unset + backend restarted at hh:mm:ssZ)
- First xterm smoke    : PASS   (xterm; whoami + pwd + echo sentinel ok;
                                 detach/reconnect within TTL ok;
                                 idempotent close ok)
- Redaction sweep      : ok: no leakage sentinels found  (backend log,
                         nginx access log, audit payload)
- Backup file          : <off-host path / object key>
                         sha256:<hex>   size:<N> bytes
- Config archive       : <off-host path / object key>   (chmod 600)
- Rollback tag         : <vX.Y.(Z-1)>   (commit sha-<short>;
                                          digest sha256:<…>)
- Operator             : <name / handle>

Decision:  USE  /  HOLD
Notes:     <free text>
```

The matching v1 production smoke entry under
[`v1-production-smoke.md`](v1-production-smoke.md) §6 carries
the per-row PASS / FAIL gates and the full redaction sweep
evidence; this short block here is the "what was deployed, when,
and what is the rollback anchor" record.

## 10. Next actions after this doc

Ranked. Each is a discrete operator action; none introduces new
code.

1. **Choose the production hostname and the deploy host.** §2
   first two rows. Until both are picked the §4 outline cannot
   start.
2. **Create the production `.env` outside the repo** on the
   chosen deploy host. Generate the three independent random
   secrets per §4 step 4 with shell history suppressed. Do not
   paste secrets into chat, screenshots, or memory.
3. **Deploy using the production Compose template.** Walk §4
   steps 1–15 against the production host. Stop and ask if any
   step surprises you — the runbook and the template are the
   load-bearing source.
4. **Record the real result.** Copy the
   [`v1-production-smoke.md`](v1-production-smoke.md) §5 entry
   template into a new dated section under §6 of that file. Walk
   every row. Commit. That commit + the §9 deploy log block here
   is the v1 cutline B2 evidence.
5. **Fix any issues found through personal use** as discrete
   slices; do not retrofit them into this plan. Real-usage gaps
   either land as a runbook update (load-bearing detail), an
   `Encountered Lessons` row (one-off gotcha), or a follow-on
   slice (real implementation gap). The cutline B3 mobile
   portrait sanity walk lands as `docs/v1-mobile-portrait-sanity-smoke`
   when the operator chooses to record it.

---

## See also

- [`docs/deployment/production-runbook.md`](production-runbook.md)
  — load-bearing operator runbook. §3 tag policy, §4 first
  deploy, §6 rollback, §7 migration, §8 backup / restore, §9
  reverse proxy, §10 post-deploy smoke, §11 secret rotation.
- [`docs/deployment/backup-restore-runbook.md`](backup-restore-runbook.md)
  — operator-facing backup / restore / rollback procedure. §3
  sensitive-material warning, §4 pre-upgrade backup, §5 restore,
  §6 rollback, §7 migration caveat, §10 rehearsal record short
  form.
- [`docs/deployment/backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
  — operator-recorded rehearsal log (template at §5; §10
  verification log seeded NOT RUN).
- [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
  — v1 production smoke log; §5 template skeleton; §5.1
  canonical sentinel set; §5.3 sweep query templates. The §5
  entry copied into a new dated section under §6 is exactly what
  resolves cutline blocker B2.
- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  — release-day operator checklist; §3 repo checks, §4
  configuration, §5 deployment, §6 first-user / auth, §7
  inventory, §8 terminal, §9 mobile portrait sanity, §10 backup
  / restore / rollback, §11 redaction sweep, §12 decision table,
  §13 sign-off template.
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  — v1 cutline. §5 blockers B2 / B3; §9 deployment cutline
  punch list; §10 open operator questions this plan answers
  with defaults.
- [`docs/v1-release-notes.md`](../v1-release-notes.md) — draft
  v1 release notes; §3 known caveats this plan inherits; §7
  security notes; §11 release-log sign-off template (paired with
  §9 above).
- [`deploy/docker-compose.production.example.yml`](../../deploy/docker-compose.production.example.yml)
  — the production Compose template §4 step 2 copies onto the
  deploy host; ships with the upgrade / rollback / backup comments
  inline.
- [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
  — env contract; §4 step 4 fills every secret-shaped field
  here.
- [`docs/spec/terminal-adapters.md`](../spec/terminal-adapters.md)
  — xterm-as-default rule and experimental-renderer gate
  contract §3 rests on.
- [`docs/terminal-recording.md`](../terminal-recording.md) —
  off-by-default recording posture §2 + §3 require re-reading
  end-to-end if the operator flips recording ON.
- [`docs/private-key-import.md`](../private-key-import.md) — v1
  Ed25519 unencrypted OpenSSH import constraint relevant if §2
  "generate-only" is flipped.
- [`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md) — SPA
  smoke runbook; § D is the renderer-fair input methodology used
  by the §5 terminal rows.
- [`AGENTS.md`](../../AGENTS.md) — agent-facing conventions and
  the architectural invariants every step here rests on.
