# RelayTerm — VPS staging deployment & HTTPS smoke

> Operator-facing checklist for the **`relayterm-staging.js-node.cc`** slot
> on the `js-node.cc` VPS. This is a production-LIKE staging slot — same
> images, same env shape, same auth envelope as production — running on
> a deliberately distinct hostname behind the host's existing Traefik.
>
> This runbook does NOT cover production. Production deploys follow
> [`docs/deployment/production-runbook.md`](./production-runbook.md).
> The two are intentionally separate: staging is a smoke surface for
> the next production deploy, not a stand-in for it.

---

## 1. Scope and non-goals

What this slot is for:

- Smoke-test the published `:main` images end-to-end against real HTTPS
  through Traefik (so Origin / cookie / WebSocket behaviour matches
  what production will see).
- Walk the Tauri desktop bundled-shell remote-web handoff against a
  real HTTPS origin (path A — see
  [`docs/spec/tauri-runtime-backend-url.md`](../spec/tauri-runtime-backend-url.md)).
- Catch reverse-proxy / middleware / cert-resolver problems before
  they reach production.

Explicit non-goals:

- Not a production deployment. The hostname, the bootstrap user, and
  any SSH identities used for an attach smoke MUST all be throwaway.
- Not a permanent environment. Tear it down between smoke windows
  (§9).
- Not multi-tenant. v1 is single-user; the bootstrap user owns
  everything in this slot.

The hostname is **`relayterm-staging.js-node.cc`** — distinct from
whatever the future production hostname will be. Do not promote this
slot to production by changing the hostname; spin a new slot instead.

---

## 2. Prerequisites on the VPS

Already in place on the host (NOT managed by this repo):

- Traefik running with:
  - an `https` entrypoint terminating TLS,
  - the `cloudflare` certresolver (DNS-01 for `*.js-node.cc`),
  - a `secure-chain@file` middleware (HSTS / sane defaults) defined
    in the file provider,
  - an external docker network named `proxy` that Traefik watches.
- DNS for `relayterm-staging.js-node.cc` resolves to the VPS public IP
  (apex covered by the existing Cloudflare DNS-01 wildcard).
- Forgejo PAT with `read:package` scope on the host (the CI's
  `write:package` token is NOT reused here).

What this runbook adds:

- A staging Compose stack at
  `/home/ubuntu/docker-compose/relayterm-staging/`.
- Persistent state at `/home/ubuntu/docker/relayterm-staging/` (only
  needed if you bind-mount Postgres; the template uses a named volume
  by default and bind-mounts are optional).

---

## 3. Directory layout (on the VPS)

```
/home/ubuntu/docker-compose/relayterm-staging/
├── docker-compose.yml          # copy of deploy/docker-compose.traefik-staging.example.yml
└── .env                        # secrets only; chmod 600; NEVER committed

/home/ubuntu/docker/relayterm-staging/
└── pgdata/                     # OPTIONAL — only if you switch to a bind-mount;
                                # the template uses a named volume by default
```

The `compose-name` in the template is `relayterm-staging`, which
prefixes container, network, and volume names with `relayterm-staging_*`
so this slot does not collide with a future production slot on the
same host.

---

## 4. `.env` template (no real secrets)

Create `/home/ubuntu/docker-compose/relayterm-staging/.env`. Replace
every `CHANGE_ME_*` placeholder with a real value generated on a
trusted machine. The file holds the session signing key, the vault
master key, the database password, and the bootstrap token — `chmod 600`
is load-bearing.

```env
# ---- image pin (staging tracks the branch tip) -----------------------
RELAYTERM_IMAGE_TAG=main

# ---- Postgres --------------------------------------------------------
POSTGRES_USER=relayterm
POSTGRES_PASSWORD=CHANGE_ME_postgres_password
POSTGRES_DB=relayterm

# ---- backend: auth (production envelope) -----------------------------
RELAYTERM_AUTH__MODE=production
RELAYTERM_AUTH__COOKIE_SECURE=true
# byte-equality vs the browser's Origin header. Lowercase, no path,
# no trailing slash. The CSRF guard does no normalisation.
RELAYTERM_AUTH__ALLOWED_ORIGINS=https://relayterm-staging.js-node.cc

# 32 random bytes, base64. CHANGE THIS — and keep it different from
# the vault master key.
#   openssl rand -base64 32
RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64=CHANGE_ME_base64_32_bytes

# Throwaway bootstrap token for the FIRST user only. Unset and
# restart after bootstrap (§7.5).
#   openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN=CHANGE_ME_throwaway_bootstrap_token

# ---- backend: vault --------------------------------------------------
RELAYTERM_VAULT__ENABLED=true
# 32 random bytes, base64. MUST differ from the session signing key.
#   openssl rand -base64 32
RELAYTERM_VAULT__MASTER_KEY_B64=CHANGE_ME_base64_32_bytes

# ---- backend: terminal recording ------------------------------------
# Off in staging unless explicitly testing the recording surface.
RELAYTERM_TERMINAL_RECORDING__ENABLED=false
RELAYTERM_TERMINAL_RECORDING__CLEANUP__ENABLED=true
RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED=true
RELAYTERM_TERMINAL_RECORDING__CLEANUP__SWEEP_INTERVAL_SECONDS=3600
RELAYTERM_TERMINAL_RECORDING__CLEANUP__BATCH_SIZE=250

# ---- tracing ---------------------------------------------------------
RUST_LOG=relayterm=info,axum=info,sqlx=warn,info
```

Generate the secrets locally, paste them in, then:

```sh
chmod 600 /home/ubuntu/docker-compose/relayterm-staging/.env
```

Suppress shell history while generating secrets (see
`docs/deployment/production-runbook.md` §4.3 — same rules apply).

---

## 5. Pull the images

This slot uses the published images from the Forgejo container
registry (`git.js-node.cc/jsprague/...`). You do NOT build on the VPS.

```sh
cd /home/ubuntu/docker-compose/relayterm-staging
docker login git.js-node.cc      # one time, with read:package PAT
docker compose config            # sanity-check env interpolation
docker compose pull              # fetch backend + migrate + web at :main
```

`docker compose config` errors loudly if any required env placeholder
is missing — every `${...:?}` in the template fires a hint with the
exact variable name.

The three images pulled at this tag MUST be in lockstep — they are
all built from the same commit by CI. Mixing tags (`:main` for one,
`:vX.Y.Z` for another) is unsupported and will surface as 500s on
schema-touching routes.

---

## 6. Apply migrations

The backend does NOT auto-migrate. Run the one-shot migrate
container BEFORE starting the backend:

```sh
docker compose --profile migrate run --rm relayterm-migrate
```

Idempotent. Re-running on an unchanged schema is a no-op.

---

## 7. Start and bootstrap

### 7.1 Bring the stack up

```sh
docker compose up -d postgres relayterm-backend relayterm-web
docker compose ps
```

All three should reach `running`; `postgres`, `relayterm-backend`,
and `relayterm-web` should reach `healthy`.

### 7.2 Confirm Traefik picked up the router

```sh
# Watch Traefik's container logs for the staging router resolving;
# adjust container name / log location to whatever the host uses.
docker logs --tail=200 traefik 2>&1 | grep -i relayterm-staging || true
```

A clean log shows the router `relayterm-staging@docker` registered on
the `https` entrypoint with the `cloudflare` cert. A `403`/`404` from
the host below typically means the router never came up; check the
labels block of the `relayterm-web` service against §10.1.

### 7.3 HTTPS health checks (from any workstation)

The three checks below are the gate before walking any UI smoke.

```sh
# (a) SPA reachable, HTTPS termination correct.
curl -I https://relayterm-staging.js-node.cc/
# Expect: HTTP/2 200, Content-Type: text/html, HSTS header from
# secure-chain@file. NO `Set-Cookie`. NO redirect to a different host.

# (b) Auth gate from outside the LAN.
curl -i https://relayterm-staging.js-node.cc/api/v1/auth/me
# Expect: HTTP/2 401, JSON body { "error": "unauthorized", ... }.
# This proves the API is reachable AND that the cookie gate is on.

# (c) Backend health passthrough.
curl -i https://relayterm-staging.js-node.cc/healthz
# Expect: HTTP/2 200, JSON body { "status": "ok" }.
```

Failure modes worth flagging before continuing:

- `curl: (60) SSL certificate problem` → cert resolver did not
  finalise; check Traefik logs for the `cloudflare` resolver. STOP.
- `404 page not found` (Traefik default) → router rule did not
  match; double-check `Host(\`relayterm-staging.js-node.cc\`)`. STOP.
- `/api/v1/auth/me` returns `403` instead of `401` → CSRF / Origin
  guard rejected the request. Re-check
  `RELAYTERM_AUTH__ALLOWED_ORIGINS` byte-for-byte vs the URL above.
  STOP.
- `/api/v1/auth/me` returns `502` → Traefik routed but the web
  container is not yet healthy. Wait, then re-check
  `docker compose ps`.

If the production envelope (`RELAYTERM_AUTH__MODE=production`,
`RELAYTERM_AUTH__COOKIE_SECURE=true`, etc.) prevents the backend from
booting cleanly because a production-auth gate is unsatisfied (empty
allow-list, missing signing key, key reuse, etc.), STOP and report.
Do NOT silently downgrade to `dev` mode without owner approval.

### 7.4 Bootstrap the throwaway staging user

With the bootstrap token from §4 set in `.env`, from any
HTTPS-reachable workstation:

```sh
# Read the token from a file the operator owns. Do NOT paste a real
# bootstrap token into a shell history.
curl -fsS -X POST \
  -H 'Content-Type: application/json' \
  -H 'Origin: https://relayterm-staging.js-node.cc' \
  --data-binary @- \
  https://relayterm-staging.js-node.cc/api/v1/auth/bootstrap <<'JSON'
{
  "bootstrap_token": "<read from a file you own>",
  "email": "staging+throwaway@example.com",
  "display_name": "staging-throwaway",
  "password": "<a long random throwaway password>"
}
JSON
```

A `201 Created` with the new user record means bootstrap succeeded.
This user is throwaway — never use a real production identity here.

### 7.5 Close the bootstrap window

```sh
sed -i '/^RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN=/d' .env
docker compose up -d --no-deps relayterm-backend
```

A subsequent `POST /api/v1/auth/bootstrap` now rejects: with the
token unset and the backend recreated, the observed reject is `401
unauthorized` (any submitted token cannot match an unset
configuration). With the token still set but a first user already
present, the reject is `409 already_bootstrapped`. `503` shows up
if the route can't reach the bootstrap service at all (token
unset *and* misconfigured). All three are safe — the route is no
longer mintable by a stranger.

---

## 8. Tauri desktop smoke (path A bundled handoff)

This is the headline reason staging exists today: the Tauri desktop
shell's bundled remote-web handoff (path A — see
[`docs/spec/tauri-runtime-backend-url.md`](../spec/tauri-runtime-backend-url.md))
is now confirmed locally end-to-end (terminal attach via a throwaway
SSH target). Re-run the same flow against the HTTPS staging origin to
catch anything specific to TLS, Traefik, the cert chain, or the cookie
posture under `Secure;`.

Do this on a developer workstation, not on the VPS.

1. Build the desktop shell from the latest `main`:

   ```sh
   pnpm install
   pnpm --filter @relayterm/desktop tauri:build
   # AppImage strip incompatibility on Arch / CachyOS hosts:
   #   NO_STRIP=true pnpm --filter @relayterm/desktop tauri:build
   # See docs/deployment/tauri-local-build.md.
   ```

2. (Recommended on the same workstation that ran a previous
   local-stack smoke against `:main`.) Evict the per-app WebKit HTTP
   cache so the desktop WebView re-fetches the just-deployed staging
   bundle instead of replaying a hot-swapped local-stack bundle from
   cache. This is the WebKitGTK cache + nginx `Cache-Control:
   immutable` gotcha (Encountered Lesson 2026-05-09). Optional if
   you are launching from a workstation that has never built or run
   the desktop shell against another origin:

   ```sh
   # Caches only — preserves localStorage so the "Change Server"
   # affordance in step 4 has a saved config to switch FROM.
   rm -rf ~/.local/share/cc.js-node.relayterm.desktop/{WebKitCache,CacheStorage}
   ```

3. Launch the bundled binary. Two valid entry states from here — the
   right one depends on what the bundled shell already has saved:

   - **Has a saved config** (e.g. a previous local-stack smoke saved
     `relayterm.backend-config.v1`): the shell renders the
     `Connecting…` splash and auto-hands off to that prior origin.
     Click **Change Server** on the splash before the navigation
     fires (the click cancels the pending navigation timer and
     clears the saved config — pinned by the `Change Server reset
     flow` block in `apps/web/tests/backendHandoff.test.ts`). The
     picker re-renders. **If the auto-navigation fires before you
     click** (the timer is short and the saved origin may be a
     now-dead local-stack URL like `http://localhost:8081`, in
     which case the WebView lands on a "Could not connect to
     127.0.0.1: Connection refused" page): kill the shell, also
     wipe `~/.local/share/cc.js-node.relayterm.desktop/localstorage`
     so the saved config is gone, and relaunch — the picker now
     renders directly with no race.
   - **No saved config** (fresh install, or you also wiped
     `~/.local/share/cc.js-node.relayterm.desktop/localstorage` in
     step 2): the picker renders directly. There is no `Connecting…`
     splash and no **Change Server** button to look for.

4. In the picker input ("Connect to RelayTerm Server"), enter
   `https://relayterm-staging.js-node.cc` and press **Connect**.

5. The handoff navigates the WebView to that origin. Expect the SPA
   to load, the configured-backend gate to pass (the picker's
   `localStorage` is now stamped on the remote origin), and the login
   screen to render.

6. Sign in with the throwaway staging user from §7.4.

7. (Optional, only with explicit approval) Add a throwaway SSH target
   and run a terminal-attach smoke. The SSH identity used here MUST
   be a throwaway keypair generated for the smoke; do NOT paste a
   real production private key into the staging vault.

8. Sweep the backend logs for redaction sentinels — same rule as
   production:

   ```sh
   ssh ubuntu@vps 'cd /home/ubuntu/docker-compose/relayterm-staging \
     && docker compose logs --tail=2000 relayterm-backend \
     | grep -E '\''relayterm_session=[A-Za-z0-9_-]{20,}|encrypted_private_key|data_b64'\'' \
     || echo ok'
   ```

   Treat any hit as a release-blocking regression and stop.

What to specifically watch for under HTTPS that the local-stack smoke
does not exercise:

- The session cookie carries `Secure;` (browser DevTools → Application
  → Cookies → `relayterm_session`).
- WebSocket upgrade for `/api/v1/terminal-sessions/:id/ws` survives
  Traefik + the `secure-chain@file` middleware. If terminal attach
  hangs on "connecting" with no error and the SPA's network panel
  shows the upgrade request with status `200`/`502`/`426`, STOP and
  report — do NOT alter `secure-chain@file` to "make it work" without
  owner sign-off.
- The handoff's same-origin short-circuit fires correctly on the
  remote origin. After step 5 succeeds, reload the SPA: the bootstrap
  picker MUST NOT reappear (Encountered Lesson 2026-05-09 — `decideHandoff`
  must short-circuit on `currentOrigin === backendOrigin`).

---

## 9. Teardown / rollback

### 9.1 Stop the slot, keep the data

```sh
cd /home/ubuntu/docker-compose/relayterm-staging
docker compose stop
```

The named volume `relayterm-staging_relayterm-staging-pgdata` (and the
optional bind-mount, if used) survive. Restart with
`docker compose up -d` to pick up where you stopped.

### 9.2 Tear down the slot completely (destroys all staging data)

```sh
cd /home/ubuntu/docker-compose/relayterm-staging
docker compose down -v
# `-v` removes the named volume. Postgres data, the throwaway user,
# any throwaway SSH identities, and any terminal recordings are all
# gone. This is the intended end state between smoke windows.
```

The external `proxy` network is host-owned and is NOT removed by
`compose down`.

### 9.3 Roll back to a previous build

Staging tracks `:main` by default. To pin a known-good earlier
commit while you investigate a `:main` regression:

```sh
# Stop the running backend BEFORE applying migrations, so the live
# backend never queries against an in-progress schema swap. Postgres
# stays up.
docker compose stop relayterm-backend relayterm-web
sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=sha-abc1234/' .env
docker compose pull
docker compose --profile migrate run --rm relayterm-migrate
docker compose up -d --no-deps relayterm-backend relayterm-web
```

If the rollback crosses a backward-incompatible migration, fall back
to §6.2 of `production-runbook.md` (restore from a pre-upgrade
backup); this slot does not have a separate plan for that.

---

## 10. What to STOP and report

Production-mode and Traefik-side surprises. None of these get
silently worked around:

1. **Production envelope refuses to boot.** A `production`-mode auth
   gate (empty allow-list, missing signing key, vault key equals
   signing key, `cookie_secure=false`) rejects boot. Stop. Do NOT
   downgrade to `dev` without owner approval — the whole point of
   staging is to exercise the production envelope.
2. **WebSocket attach fails behind Traefik.** Terminal attach hangs
   or the upgrade handshake errors only behind the proxy. Stop. Do
   NOT remove `secure-chain@file` or rewrite the middleware chain
   without owner approval; document the symptom (status code, headers
   sent, headers received) first.
3. **`secure-chain@file` strips the `Origin` header.** The CSRF
   guard returns `403 csrf_origin_mismatch` on every state-change
   even with the cookie present. Stop. Confirm the offending header
   (Traefik-side debug log or `curl -v`) before changing config.
4. **Cert resolver loop.** `cloudflare` resolver retries forever and
   never finalises. Stop and read the Traefik logs end-to-end —
   this is usually a DNS-01 / API token issue, not a RelayTerm issue.
5. **Cookie missing `Secure;`.** The browser shows
   `relayterm_session` without `Secure`. Stop — it means
   `RELAYTERM_AUTH__COOKIE_SECURE` did not propagate, and any session
   minted in this state is unsafe to keep around.

---

## 11. Security checklist (pinned)

A staging deploy that violates any of these MUST be torn down (§9.2)
and re-stood-up correctly:

- [ ] No secrets in git. The committed Compose template carries
      placeholders only; the real `.env` lives at
      `/home/ubuntu/docker-compose/relayterm-staging/.env` with
      `chmod 600` and is NOT in any repo.
- [ ] `RELAYTERM_AUTH__MODE=production` and
      `RELAYTERM_AUTH__COOKIE_SECURE=true`.
- [ ] `RELAYTERM_AUTH__ALLOWED_ORIGINS` is exactly
      `https://relayterm-staging.js-node.cc` — byte-for-byte, no
      trailing slash, no path. Distinct from the future production
      origin.
- [ ] Postgres is NOT on the `proxy` network and has NO host port
      mapping. `ss -lntp` on the VPS does NOT show port 5432
      bound on a public interface.
- [ ] `relayterm-backend` is NOT on the `proxy` network and has NO
      host port mapping. The only public ingress is Traefik →
      `relayterm-web`.
- [ ] The bootstrap user is throwaway, the password is throwaway,
      the bootstrap token is unset post-bootstrap, and any SSH
      identities used for an attach smoke are throwaway keypairs.
- [ ] The hostname is `relayterm-staging.js-node.cc` — never a
      production hostname, never a user-facing domain.
- [ ] Backend logs greppable for redaction sentinels return clean
      (§8 step 8).

---

## 12. Verification log

A short, dated record of when this runbook was actually walked
end-to-end against the live VPS slot. Each entry pins what was
verified, what was deferred, and any drift between the runbook and
observed behaviour worth folding into the next iteration. Existing
entries are not edited after the fact; later entries cross-reference
and supersede earlier ones explicitly when needed. Ordering within
the section is best-effort grouped by date, with same-date runs
sometimes prepended above earlier runs of the same date when a
later run depends on an earlier run and a top-down read benefits
from seeing the dependent context first — readers should rely on
the explicit `2026-MM-DD · <slug>` headings and the inter-entry
cross-references (`entry above`, `entry below`) rather than on a
strict overall ordering rule.

### 2026-05-10 · Closed-session reconnect empty-state UX smoke + follow-up fix

Picks up from the 2026-05-09 desktop reconnect smoke entry below
(same VPS slot `relayterm-staging`, same hostname
`relayterm-staging.js-node.cc`, same throwaway bootstrap user, same
managed `smoke-id` ed25519 identity reused). The starting goal was a
quick verification that commit
`0804083 Fix closed session reconnect affordance` resolved the
operator-visible "End session → Reconnect → connection error" UX
bug on staging. The smoke surfaced that the helper fix is correct in
its narrow scope but does not in fact address the operator-visible
path, and the run was pivoted into a focused follow-up source fix in
the same branch.

This entry is **smoke + scoped follow-up fix**, not a product feature
expansion. No backend changes. No session-lifecycle, schema,
WebSocket-protocol, or auth-envelope changes. No Tauri-shell or CI
changes. The follow-up is two files in the production web shell plus
one regression-pin test.

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Image tag:** `:main`, refreshed from
`sha256:a904f55473…` (built `2026-05-10T01:39:28Z`, predates fix
`0804083` by ~47 min) to
`sha256:da78580...` (built `2026-05-10T02:55:34Z`, ~28 min after
`0804083` committed). Migrate run was a no-op. Postgres untouched.
**Desktop binary:** existing `target/release/relayterm-desktop` from
the 2026-05-08 build (no rebuild — the bundled SPA is only used
pre-handoff; the post-handoff SPA is fetched fresh from staging).
**Throwaway SSH target:** `linuxserver/openssh-server:latest`
container `relayterm-staging-smoke-ssh` joined to
`relayterm-staging_relayterm-staging-internal` (key-auth-only,
`PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`, port `2222`, no host
port published, user `smoke`, throwaway, torn down at end of run);
ed25519 host fingerprint
`SHA256:K4QL+yWXpMUGcf8gUbwLdDBIQ9ouiDHSuTH179XTKCU`, byte-identical
to `docker exec ... ssh-keygen -lf
/config/ssh_host_keys/ssh_host_ed25519_key.pub`.

**Inventory:** brand-new host `smoke-ssh-uxsmoke-v2` and brand-new
profile `ux-smoke-profile-v2` bound to the existing reused
`smoke-id` ed25519 identity. Existing `smoke-ssh-desktop` /
`desktop-smoke-profile` (from the 2026-05-09 reconnect smoke) and
the Android-smoke inventory were left intact per the AGENTS.md
"Inventory lifecycle and destructive-action policy" — re-using the
prior profile would have failed host-key preflight against the new
container's freshly-generated keys (RelayTerm refuses to silently
overwrite a pinned key, and there is no operator route to clear
`known_host_entries` on purpose; the supported flow is "create a
new host + profile").

What the original-fix smoke verified:

- `0804083`'s helper-level invariants are sound. The commit's
  `computeWorkspaceEnablement` change makes the `production-terminal-reconnect`
  button disabled when `phase === "closed"`, and `classifyReconnectAttempt`
  blocks a `closed`-phase click with a non-technical message. Both are
  pinned by `apps/web/tests/terminalLaunch.test.ts`.

What the smoke discovered the original fix does NOT cover:

- After `End session`, `ProductionTerminal.svelte` fires
  `onSessionClosed?.()`, which `AppShell.svelte`'s
  `handleSessionClosed` services by `clearActiveSession() +
  activeLaunch = null`. This unmounts `ProductionTerminal` and
  re-renders `TerminalView`'s `{:else}` (empty-state) branch —
  before the operator can interact with the now-disabled
  workspace-pane Reconnect button. The fix's intended UX is
  therefore essentially never visible in production.
- The empty-state branch carries a separate "Reconnect last session"
  affordance gated by `let saved = $state<ActiveSessionRecord |
  null>(loadActiveSession())`. That `$state` initializer runs once
  at mount and is never re-read while the component stays mounted.
  `TerminalView` is NOT unmounted by AppShell on a launch
  transition; both `{#if launch}` and `{:else}` branches live inside
  the same component. So `saved` stays cached pointing at the
  just-closed session id — and the operator clicks "Reconnect last
  session", which routes back to the workspace, opens a doomed
  WebSocket attach, and surfaces the generic "connection error"
  copy. That is the original 2026-05-09 staging-smoke complaint;
  it persists on staging through the empty-state path even with
  `0804083` shipped. Verified that `clearActiveSession()` did
  durably remove the entry from the WebView's
  `localstorage/https_relayterm-staging.js-node.cc_0.localstorage`
  SQLite (only `relayterm.backend-config.v1` remained), so the
  staleness is purely an in-memory `$state` cache that never
  re-syncs.

Follow-up source fix (this branch):

- `apps/web/src/lib/app/AppShell.svelte`: wrap `<TerminalView>` in
  `{#key activeLaunch?.sessionId ?? "empty"}` so every launch
  transition (non-null → null on wire-close, null → some-id on
  launch, id → different-id on reconnect-from-Sessions) unmounts
  and remounts `TerminalView`. `saved` is then always reflective of
  current localStorage at the moment the empty state renders.
- `apps/web/src/lib/app/views/TerminalView.svelte`: corrected the
  inline comment that previously asserted "the AppShell unmounts
  and remounts this view across launch transitions" (which was
  aspirational, not actual). The comment now points at the new
  AppShell wrapper and the new regression pin.
- `apps/web/tests/appShellIsolation.test.ts`: added a static-text
  regression scan that asserts `AppShell.svelte` wraps
  `<TerminalView>` in `{#key activeLaunch?.sessionId ?? "empty"}`.
  Red-green verified: the assertion fails with the wrapper removed
  and passes with the wrapper restored. No Svelte component test
  harness is wired into the workspace today; the static scan is the
  smallest maintainable pin given that constraint.

Verified end-to-end on staging:

- HTTPS reachability gate (§7.3) re-checked from the workstation
  before any UI action: `/` → 200, `/healthz` → 200,
  `/api/v1/auth/me` → 401. Staging Compose stack carried over from
  the prior 2026-05-09 entries; only `relayterm-backend` and
  `relayterm-web` were `up -d --force-recreate`-d to pick up the
  refreshed `:main` images. Postgres was not touched.
- Desktop bundled-shell handoff to staging worked unchanged (saved
  bootstrap config in `localstorage/https_relayterm-staging.js-node.cc_0.localstorage`
  short-circuited the picker per the prior 2026-05-09 same-origin
  short-circuit lesson).
- Original-fix smoke against the refreshed `:main` (commit
  `0804083` in the served bundle, verified by `grep -a -c "closed
  and cannot be reconnected"` on the served `assets/index-*.js` —
  count `1`): launch + echo + End. Empty-state still surfaced a
  clickable "Reconnect last session" pointing at the closed session;
  click produced "connection error". Bug reproduced on the original
  fix, scoped to the empty-state path as analysed above.
- Follow-up-fix smoke against the locally-built bundle hot-replaced
  into the running `relayterm-staging-relayterm-web-1` nginx (per
  the 2026-05-09 WebKit-cache lesson the bundle filename
  necessarily changes when the source changes, so the
  `index-*.js` URL changes and nginx's content-hash-immutable cache
  strategy invalidates cleanly; WebKit cache + CacheStorage were
  also wiped before relaunching the desktop binary to be safe).
  Bundle hash `index-BADxlpqn.js`; `grep -a -oE "empty\""` against
  the served bundle returned a hit confirming the `{#key}` wrapper
  compiled in. Re-smoke flow: relaunch desktop fresh-context → log
  in (already had a session cookie) → empty-state validation
  effect ran on the lingering pre-fix saved pointer, validated
  against the backend, returned `stale (closed)`, ran
  `onForgetLastSession` and the localStorage pointer was cleared →
  fresh launch via `ux-smoke-profile-v2` → echo round-trip
  (one cold-start nudge per the documented race) → `End session`
  → empty state rendered cleanly with NO saved-session affordance
  and the only action surface being "Launch a terminal from a
  server profile". `localstorage/https_relayterm-staging.js-node.cc_0.localstorage`
  inspected post-end; only `relayterm.backend-config.v1` remained.
  Operator-visible "End → Reconnect → connection error" UX bug
  is no longer reachable through the empty-state path. The
  workspace-pane closed-session guard from `0804083` remains in
  place as defence in depth (its narrow-scope unit-test invariants
  still pass).

Workstation checks before stop-before-commit:

- `pnpm -r check` (svelte-check + tsc): clean.
- `pnpm -r test`: 948 tests pass (incl. the new regression pin in
  `tests/appShellIsolation.test.ts`).
- `pnpm -r build`: clean; web bundle `index-BADxlpqn.js` produced.
- `pnpm run check:docs-contracts`: clean.
- `git diff --check`: clean.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets --all-features -- -D
  warnings`: clean.
- `cargo test --workspace`: clean.

Drift worth folding back later (intentional non-goals for this run):

- **Promote `:main` post-merge.** Staging is currently serving the
  follow-up-fix bundle via `docker cp` hot-replace into the running
  web container. That is a smoke convenience only; the durable
  shape is `cargo / pnpm green → merge to main → CI publishes a
  fresh `:main` → re-pull on the VPS slot`. A follow-up entry will
  pin the post-merge re-pull was clean.
- **No host-key-revoke route.** Re-using the prior smoke profile
  was blocked by RelayTerm's "refuse to silently overwrite a pinned
  host key" policy and there is no `DELETE /known-host-entries/:id`
  surface to clear stale trust. The supported flow is "create a new
  host + profile", which is what this run did. Operator UX is
  acceptable for now (single-user staging) but the cost will grow
  with multi-user / production reuse — call out in a future SPEC
  slice if the operator UX becomes a friction point.
- **No Svelte component test harness.** The regression-pin is a
  static-text scan rather than a behavioural test of TerminalView's
  remount-on-launch-transition. Wiring `@testing-library/svelte` (or
  the workspace-preferred equivalent) is a useful future investment;
  the static scan is the smallest maintainable pin until then.

### 2026-05-10 · Closed-session empty-state reconnect fix verified against published `:main` web image

Closes the **"Drift worth folding back later — Promote `:main`
post-merge"** follow-up explicitly called out in the entry above.
The follow-up branch landed as
`fc80b5a Fix closed session empty-state reconnect`; this run pins
that the registry-published `:main` web image emitted by the post-merge
Forgejo Actions run contains the fix and that staging serves it
cleanly without the prior `docker cp` hot-replace. Same VPS slot
`relayterm-staging`, same hostname
`relayterm-staging.js-node.cc`, same throwaway bootstrap user, same
managed `smoke-id` ed25519 identity reused. Postgres untouched.

This entry is **smoke + docs-only**. No source changes. No backend,
session-lifecycle, schema, WebSocket-protocol, auth-envelope, Tauri
shell, or CI changes.

**Verification path.** Driven via Playwright MCP against
`https://relayterm-staging.js-node.cc` directly, NOT through the
Tauri desktop WebView. Rationale: the closed-session empty-state
regression lives in the server-served SPA after handoff, so the
post-handoff UI path is the same code path the Tauri shell would
load from this URL. This entry verifies the **published web UI
path**; it does NOT re-verify Tauri WebView post-handoff behaviour.
The 2026-05-10 entry above already covered the Tauri handoff +
behavioural regression-pin during the follow-up-fix iteration.

**Forgejo CI publish (from `git.js-node.cc/api/v1/repos/jsprague/RelayTerm/actions/tasks`):**
the `publish images (forgejo registry)` workflow for head SHA
`fc80b5a1a6b33335b58a979e5612713418811564` ran as workflow_id 504,
status `success`, completed `2026-05-10T00:49:22-05:00` (UTC
`2026-05-10T05:49:22Z`). The four upstream gates (`rust checks`,
`web checks`, `docker build`, `desktop linux build`, `tauri android
build`) all green for the same SHA.

**Registry digests at `:main` (Forgejo container registry,
post-publish):**

```
git.js-node.cc/jsprague/relayterm-backend:main
  sha256:5971ab3a74985466f1c25f5000df71dbc7e96d4217489b04e217dcbb102cf215

git.js-node.cc/jsprague/relayterm-backend-migrate:main
  sha256:a0b77846c3f984806737c255a6ad83289b5cc70242500bef537e8e3ece98e4bf

git.js-node.cc/jsprague/relayterm-web:main
  sha256:5b38fbf3c1c06ae549e4763eef7ee645472ca10da79b145246a3ce6bc2580cad
```

These three digests are the manifest-blob digests as advertised by
the registry's `docker-content-digest` response header for
`HEAD /v2/jsprague/<repo>/manifests/main` (Bearer-token auth scoped
`repository:<repo>:pull`); they match byte-identically what `docker
images --digests …:main` reports after the host-side pull.

**Pre-refresh staging state (snapshot before the post-merge
re-pull):**

- `relayterm-staging-relayterm-web-1` was running image-id
  `sha256:da785804b26827d4d1119486463a04f1664acf6a51cd34b4bbe9016945e7febc`,
  container created `2026-05-10T05:22:02Z` (~27 min before the
  publish workflow finished). The running container also carried the
  hot-replaced bundle from the entry above (a `docker cp` of
  `apps/web/dist/.` into the container's `/usr/share/nginx/html/`).

**Post-refresh staging state (after `compose pull` +
`up --force-recreate`):**

- `relayterm-staging-relayterm-web-1` image-id
  `sha256:5b38fbf3c1c06ae549e4763eef7ee645472ca10da79b145246a3ce6bc2580cad`,
  container created `2026-05-10T05:58:06Z` — byte-equal to the
  `:main` manifest config digest above.
- `relayterm-staging-relayterm-backend-1` image-id
  `sha256:5971ab3a74985466f1c25f5000df71dbc7e96d4217489b04e217dcbb102cf215`,
  container created `2026-05-10T05:58:06Z` — byte-equal to the
  `:main` manifest config digest above.
- `relayterm-staging-postgres-1` left untouched
  (`postgres:17-alpine`, up 13 h).
- Migrate ran via the `migrate` profile, exited code `0`
  (idempotent — no migrations to apply).

The only host-side commands that mutated container state were:

```sh
# On cloud-edge:
cd /home/ubuntu/docker-compose/relayterm-staging
docker compose --env-file /home/ubuntu/docker/relayterm-staging/.env \
  -p relayterm-staging pull postgres relayterm-backend relayterm-web
docker compose --env-file /home/ubuntu/docker/relayterm-staging/.env \
  -p relayterm-staging --profile migrate pull relayterm-migrate
docker compose --env-file /home/ubuntu/docker/relayterm-staging/.env \
  -p relayterm-staging --profile migrate up \
  --no-deps --abort-on-container-exit \
  --exit-code-from relayterm-migrate relayterm-migrate
docker compose --env-file /home/ubuntu/docker/relayterm-staging/.env \
  -p relayterm-staging up -d \
  --no-deps --force-recreate --pull never \
  relayterm-backend relayterm-web
```

`--force-recreate` is load-bearing: a plain `up -d` would have
preserved the running web container (since the env hash and image
tag both look unchanged at the compose-config level), and the
hot-replaced bundle would have stayed in its overlay layer. With
`--force-recreate` the old container is torn down and a fresh
one is constructed from the just-pulled image, evicting the
`docker cp` overlay.

**HTTPS reachability gate (§7.3) post-refresh:**

```
curl -I  https://relayterm-staging.js-node.cc/             → 200
curl -i  https://relayterm-staging.js-node.cc/healthz      → 200 {"status":"ok"}
curl -i  https://relayterm-staging.js-node.cc/api/v1/auth/me
                                                            → 401 unauthorized
```

`/`'s `last-modified` header is
`Sun, 10 May 2026 05:49:13 GMT` — exactly within the publish
workflow's run window
(`2026-05-10T00:44:44 → 00:49:22 -05:00` local =
`2026-05-10T05:44:44 → 05:49:22Z` UTC), confirming the served HTML
is the registry-published artifact and not the prior hot-replace.

**Bundle-string verification of the served JS:**

The single Vite-emitted bundle is `/assets/index-BADxlpqn.js`
(content-hash filename; immutable; `cache-control: public,
immutable`; `last-modified: 2026-05-10T05:49:13Z`). MD5 of the
served bytes (`c36b63a6ea3a805919e7bfad56e1603b`) matches MD5 of
`/usr/share/nginx/html/assets/index-BADxlpqn.js` inside
`relayterm-staging-relayterm-web-1` byte-for-byte. The bundle
contains both stable user-facing strings from the closed-session
fix series (Python `re.search` against the served bytes — bash
`grep -E` against the same file silently dropped both matches in
this shell, hence the Python crosscheck — both are present
exactly once):

- `"This session is closed and cannot be reconnected. Launch a
  new session from the originating server profile."`
  (`RECONNECT_CLOSED_MESSAGE` in
  `apps/web/src/lib/app/terminal/terminalLaunch.ts`, shipped in
  commit `0804083 Fix closed session reconnect affordance`).
- `"Reconnect is not available from the current state."`
  (`RECONNECT_INELIGIBLE_MESSAGE`, same file, same commit).

Comments are stripped by minification, so the `{#key
activeLaunch?.sessionId ?? "empty"}` wrapper from `fc80b5a` is not
directly grep-able by its source comment; the behavioural pin
below covers it.

**Inventory used for the UX smoke (Playwright MCP):**

- **Throwaway SSH target.** `linuxserver/openssh-server:latest`
  container `relayterm-staging-smoke-ssh` joined to
  `relayterm-staging_relayterm-staging-internal` (key-auth-only,
  `PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`, port `2222`, no
  host port published, user `smoke` with `/bin/bash`, throwaway).
  The previous container from the entry above was already torn
  down per its cleanup; this run started a fresh one whose
  ed25519 host fingerprint is
  `SHA256:uDf/HiRD80z22jUge0TGRKV1BejRuSixVn0rReMajLY`,
  byte-identical to `docker exec ... ssh-keygen -lf
  /config/ssh_host_keys/ssh_host_ed25519_key.pub`.
- **Inventory.** Brand-new host `smoke-ssh-published-uxsmoke`
  (`e0f9ae64-d3ce-49cf-ac52-2d55bfc901c3`,
  `relayterm-staging-smoke-ssh:2222 smoke`) and brand-new profile
  `published-uxsmoke-profile`
  (`07ddbfe7-9fa6-42e2-a3f4-6b9d38fbc953`) bound to the existing
  reused `smoke-id` ed25519 identity
  (`44b5e2be-29c2-4eb0-b6ac-3b4e25ca789d`). Existing
  `smoke-ssh-uxsmoke-v2` / `ux-smoke-profile-v2` (from the
  closed-session empty-state follow-up-fix entry above),
  `smoke-ssh-custom-ttl-desktop` / `custom-ttl-smoke-profile`,
  `smoke-ssh-desktop` / `desktop-smoke-profile`, and the
  Android-smoke inventory were left intact per the AGENTS.md
  "Inventory lifecycle and destructive-action policy" — re-using
  the prior `smoke-ssh-uxsmoke-v2` host would have failed
  host-key preflight against the new container's freshly-generated
  keys (RelayTerm refuses to silently overwrite a pinned key, and
  there is no operator route to clear `known_host_entries` on
  purpose; the supported flow is "create a new host + profile",
  consistent with the same observation in the entry above and the
  prior 2026-05-10 custom-TTL entry below).

Inventory CRUD (host + profile creation) was driven via
`fetch('/api/v1/...', { credentials: 'include' })` from inside
the Playwright-controlled page; the session cookie ridealong is
automatic and the same `CsrfGuard` / `Origin` checks apply as for
the production SPA. Public-key install onto the throwaway SSH
target was a `docker exec` into the container's
`/config/.ssh/authorized_keys`. Host-key preflight + trust +
auth-check were exercised through the SPA's normal buttons.

**End-to-end UX smoke (the load-bearing surface for this entry):**

The flow exercises the operator-visible regression and pins that it
is no longer reachable through the registry-published bundle.

1. Login via `/api/v1/auth/login` (Playwright UI fill of the
   `/login` form) succeeded — Dashboard rendered, banner showed
   `staging-throwaway-20260509173230`.
2. Inventory POSTs (host + profile) returned `201`.
3. Host-key preflight returned the new container's ed25519
   fingerprint
   `SHA256:uDf/HiRD80z22jUge0TGRKV1BejRuSixVn0rReMajLY`; pasted
   into the confirmation textbox; **Trust this host key** turned
   the row into `Trusted ed25519`.
4. Auth-check returned `Authenticated 2026-05-10T06:12:38.999288997Z`
   ("SSH public-key authentication succeeded for the configured
   username. No PTY was allocated and no command was executed.").
5. **Launch terminal** routed to `/terminal`; phase reached
   `attached`; xterm DOM rows present; `localStorage` had
   `relayterm.active-terminal.v1 =
   {"session_id":"50168a0f-209e-4576-8b5f-ee8cb0f3fccb",...,
   "profile_label":"published-uxsmoke-profile","cols":80,"rows":24}`.
6. Sent the harmless command via xterm's IME composition pathway
   (the `.xterm-helper-textarea` is hidden so a direct `fill`
   times out; dispatching a `compositionstart` /
   `input(isComposing=true)` / `compositionend` sequence is the
   well-supported xterm.js entry point for multi-char input on
   non-CJK keyboards):

   ```sh
   echo relayterm-published-web-closed-ux-smoke
   ```

   The remote shell echoed both the input line and the output
   line, then the prompt `relayterm-staging-smoke-ssh:~$`
   re-rendered.
7. Clicked **End session**. Network panel showed no further
   `POST /api/v1/...attach` and no fresh `wss://` open after the
   click — i.e. no doomed reconnect attempt was made.
8. Empty state rendered. The "Terminal workspace" pane showed the
   "Launch a terminal from a server profile." copy and the three
   bullet helpers (`Server profiles → pick → Launch terminal`,
   `host-key trust + auth-check first`, `~30s detached survival`)
   — and **NO "Reconnect last session" affordance anywhere** in
   the empty-state region. The DOM `Array.from(main.querySelectorAll('button')).map(b=>b.textContent)`
   contained no `Reconnect last session` button. Pre-fix this
   was a reproducible operator-visible regression on the
   empty-state path; post-fix it is gone.
9. `localStorage.getItem('relayterm.active-terminal.v1')` was
   `null` after End — confirming `clearActiveSession()` ran in
   `handleSessionClosed`. Combined with #8, this is the load-bearing
   evidence that AppShell's `{#key activeLaunch?.sessionId ?? "empty"}`
   wrapper around `<TerminalView>` is in the served bundle and
   takes effect: the launch transition `non-null → null` rotated
   the key value from the session id to `"empty"`, unmounted
   `TerminalView`, and the freshly-mounted instance re-read
   `loadActiveSession()` against the now-empty localStorage.

What this entry deliberately does NOT claim:

- It does NOT re-verify the Tauri desktop WebView post-handoff
  path. The SPA code path is the same in both surfaces, but the
  entry above already covered the Tauri shell against the same
  staging origin during the follow-up-fix iteration; this entry
  pins only the registry-published web bundle on the browser
  surface.
- It does NOT re-verify Android. The Android Tauri shell is on
  the same SPA bundle once it hands off, so the same conclusion
  follows by code-share — but this run did not exercise it.

Workstation checks before stop-before-commit:

- `git diff --check`: clean.
- `pnpm run check:docs-contracts`: clean.
- `pnpm -r check` (svelte-check + tsc): clean.

Drift worth folding back later (intentional non-goals for this
run):

- **WebSocket request panel filtering.** `mcp__playwright__browser_network_requests`
  with the regex `attach|wss` returned an empty list even with
  `static: true` — the panel may not include WebSocket frames
  in this version of the MCP server. The "no doomed reconnect
  attempt" claim above is grounded in the unfiltered HTTP
  request log (no `POST /attach` and no fresh `GET /ws/...`
  after End) plus the empty-state DOM lacking any Reconnect
  affordance to drive one. A Tauri-shell DevTools verification
  would catch this directly via the Network tab; defer until
  the next desktop-shell smoke that has reason to revisit the
  surface.
- **Single Vite bundle.** `apps/web/dist` ships exactly one JS
  asset (`index-BADxlpqn.js`) and one CSS asset
  (`index-Bl1jKMB5.css`). String greps like the one used here
  remain a good smoke-time backstop. If we later split the
  bundle (route-based chunks, vendor split), update this
  procedure to grep the chunk that owns `lib/app/terminal/*`
  rather than the entry bundle.

### 2026-05-10 · Desktop Tauri staging custom detached-live-PTY TTL smoke

Picks up from the 2026-05-09 desktop reconnect smoke entry below
(same VPS slot `relayterm-staging`, same hostname
`relayterm-staging.js-node.cc`, same throwaway bootstrap user, same
managed `smoke-id` ed25519 identity reused). Closes the "30 s TTL
vs. desktop kill+relaunch round-trip" follow-up from that entry's
"Drift worth folding back later" — the operator-configurable knob
is now reachable end-to-end through the staging Compose stack and
its runtime behaviour is verified against the desktop Tauri shell.

This entry is **smoke + plumbing-fix**, not a product feature
expansion: the configurable-TTL knob already shipped in commit
`e28b009 Make detached live PTY TTL configurable`. What this run
landed in addition to the smoke is a one-line wiring fix in the
Compose templates that the original commit missed.

**Pinned contract under test.** `terminal_sessions.detached_live_pty_ttl_seconds`
(env `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`),
default **30 s**, bounded **5..=86 400** (5 s..24 h), staging value
for this run **300 s**. Validator rejects 0 / out-of-range as a hard
boot failure. The knob is a *short-term reconnect grace window* on
a still-live PTY held by the running backend; it does NOT survive a
backend restart and is NOT durable shell persistence — see the
"SCOPE — read this before bumping the value" preamble in
[`docs/config-examples/relayterm.production.example.toml`](../config-examples/relayterm.production.example.toml).

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Image tag:** `:main`, lockstep upgrade from
`sha256:596d8c270d…` (built `2026-05-09T17:05:07Z`, predates
`e28b009` by ~5 h 26 min) to
`sha256:1f641be800…` (built `2026-05-09T22:58:54Z`, +27 min after
`e28b009` committed `2026-05-09T22:31:16Z`). Migrate run was a no-op
(21 in DB equals 21 in repo at this commit). Postgres untouched.
**Desktop binary:** existing
`target/release/relayterm-desktop` from the 2026-05-08 build (no
rebuild — the bundled SPA is only used pre-handoff; the post-handoff
SPA is fetched fresh from staging at the new web image's hash).
**Throwaway SSH target:** `linuxserver/openssh-server:latest`
container `relayterm-staging-smoke-ssh` joined to
`relayterm-staging_relayterm-staging-internal` (key-auth-only,
`PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`, port `2222`, no host
port published, user `smoke` with `/bin/bash`, throwaway, torn
down at end of run); ed25519 host fingerprint
`SHA256:5E9r10JlhWoS4zxPepLju3ooCnw1OA65tfqPeLw6QqU`,
byte-identical to `docker exec ... ssh-keygen -lf
/config/ssh_host_keys/ssh_host_ed25519_key.pub`.

**Inventory:** brand-new host `smoke-ssh-custom-ttl-desktop`
(`5eac7b62-1bb3-42ce-b556-6744a3b4af5e`,
`relayterm-staging-smoke-ssh:2222 smoke`) and brand-new profile
`custom-ttl-smoke-profile`
(`a250def7-be02-4492-bd44-8e33716b8181`) bound to the existing
reused `smoke-id` ed25519 identity
(`44b5e2be-29c2-4eb0-b6ac-3b4e25ca789d`). Existing
`smoke-ssh-desktop` host + `desktop-smoke-profile` profile (from
the 2026-05-09 reconnect smoke) and the prior Android-smoke
inventory were left intact per the AGENTS.md "Inventory lifecycle
and destructive-action policy" — see also the TOFU re-pin
observation under "Drift worth folding back later" below for why
the smoke pivoted to a new host/profile rather than re-using the
existing `desktop-smoke-profile` row.

Setup-API path drove from `cloud-edge` itself with the session
cookie persisted to `/tmp/relayterm-custom-ttl.cookie` (`chmod 600`,
never echoed). Bootstrap credentials sourced from
`/home/ubuntu/docker/relayterm-staging/.bootstrap-credentials`
(format: `email=… password=… created_utc=…`, parsed key-by-key into
shell vars and shipped as a JSON body via `python3 -m json` over
env interpolation, never echoed). Cookie file shredded at end of
run.

Plumbing fix landed on this branch:

- The configurable-TTL commit `e28b009` added
  `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS` to
  [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
  and to the TOML example
  [`docs/config-examples/relayterm.production.example.toml`](../config-examples/relayterm.production.example.toml),
  but did NOT add the matching `${VAR:-30}` interpolation row to
  either Compose template, so the knob could be set in `.env` but
  the container's `environment:` map never propagated it. Verified
  on the on-VPS stack: `docker exec ... env | grep
  ^RELAYTERM_TERMINAL_SESSIONS__` returned no match before the
  fix.
- Patched
  [`deploy/docker-compose.traefik-staging.example.yml`](../../deploy/docker-compose.traefik-staging.example.yml)
  and
  [`deploy/docker-compose.example.yml`](../../deploy/docker-compose.example.yml)
  with one line each, mirroring the existing recording-knob shape:
  `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS:
  "${RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS:-30}"`.
  The default `30` matches the historical hard-coded value, so any
  existing operator who does NOT override in `.env` sees no change.
- Mirrored on the on-VPS file
  `/home/ubuntu/docker-compose/relayterm-staging/docker-compose.yml`
  (with a `.bak.<unix-ts>` snapshot first); subsequent
  `docker compose up -d --no-deps --force-recreate relayterm-backend`
  surfaced the env in the container, and the upgraded backend image
  added the value to its first-line startup log:
  `relayterm-backend starting addr=0.0.0.0:8080 auth_mode="production"
  recording_enabled=false detached_live_pty_ttl_seconds=300`.

Verified:

- HTTPS reachability gate (§7.3) re-checked from the workstation
  before any container action: `/` → 200, `/healthz` → 200 JSON,
  `/api/v1/auth/me` → 401 JSON. Re-checked AGAIN post lockstep
  pull + recreate: same three responses, byte-identical headers
  (HSTS / CSP / referrer-policy from `secure-chain@file`).
- Force-recreate of `relayterm-backend` after the env-var
  appendage to `.env` initially appeared to succeed but
  `docker exec ... env` still showed the new key absent — the
  Compose template's `environment:` block did not reference it
  (root cause of the plumbing fix above). After the
  one-line patch + a second `--force-recreate` cycle, the env
  var landed in the container; backend reached `(healthy)` in
  ~10 s; container image at this point was still
  `sha256:596d8c270d…` and predates `e28b009`, so the configured
  300 was accepted by the deserializer but the runtime kept the
  hard-coded 30 s TTL. Lockstep `docker compose pull` + idempotent
  migrate (no-op at 21 / 21) + `docker compose up -d --no-deps
  --force-recreate relayterm-backend relayterm-web` brought both
  containers to `sha256:1f641be800…` /
  `sha256:<new web>` ; the new backend's startup log carried the
  `detached_live_pty_ttl_seconds=300` field.
- Inventory CRUD: `POST /api/v1/hosts` → 201 (host id above);
  `POST /api/v1/server-profiles` → 201 (profile id above) bound to
  the reused identity. `POST .../host-key-preflight` returned
  `host_key_status: "unknown"` with the new container's ed25519
  fingerprint (matches the host-side `ssh-keygen -lf`); subsequent
  `POST .../trust-host-key` with the same `expected_fingerprint`
  returned a fresh `known_host_entry_id`
  (`f4a91f89-e9fd-4d69-8e76-3a8d29f59249`); subsequent
  `POST .../auth-check` returned `status:
  "authentication_succeeded"`, `message: "ssh public-key
  authentication succeeded; no PTY was allocated and no command
  was executed"`. Public key was injected into
  `/config/.ssh/authorized_keys` inside the container with
  `smoke:users` ownership and mode `600` between preflight and
  auth-check.
- **Case A — short detach/reconnect within the configured TTL
  window** (session `aa7d2809-b539-4a2f-95eb-91cd59325d4c`).
  Operator launched the production terminal from
  `custom-ttl-smoke-profile` via `Sidebar → Server profiles →
  custom-ttl-smoke-profile → Launch terminal session`, ran
  `echo relayterm-custom-ttl-start`, Detach → wait → Reconnect →
  `echo relayterm-custom-ttl-resumed` round-tripped. `session_events`
  ground truth: `attached 00:05:41.026 → detached 00:06:04.567 →
  attached 00:07:08.801 → reattached 00:07:08.803`. Detach gap =
  **64.234 s**, comfortably above the old 30 s default (would have
  reaped) and below the new 300 s configured TTL (still alive);
  `reattached` event fired within **2.4 ms** of `attached` proving
  the cancel-pending-close + transition-to-Active path on
  `crates/relayterm-terminal/src/manager.rs:914-919, 956-970`
  fired correctly with a non-default TTL.
- **Case C — replay during detach** (same session
  `aa7d2809-…`). Operator ran
  `( for i in 1 2 3 4 5; do sleep 1; echo "relayterm-custom-ttl-replay-$i"; done ) &`,
  Detached, waited with the client torn down so all 5 ticks emitted
  while no client was rendering, then Reconnected; **all five lines
  `relayterm-custom-ttl-replay-1` through `…-5` rendered on the
  canvas after Reconnect and before any new input** — the
  in-memory replay ring buffer emitted held frames through the
  `replay_start → buffered output → replay_end` handshake on the
  new attachment. `session_events` for the replay portion: `detached
  00:08:15.500 → attached 00:11:19.173 → reattached 00:11:19.175`,
  detach gap = **183.7 s** (≈ 6 × the old default; would have
  reaped 5 × over). A second tighter cycle (`detached 00:11:51.396
  → attached 00:12:10.967 → reattached 00:12:10.969`, gap **19.6 s**)
  exercised the same code path. Replay is a *resume-the-live-stream*
  primitive — the renderer `dispose()` on Detach destroys the
  xterm.js grid; the live stream's reattachment replays output
  past `lastSeenSeq` only, not local scrollback. Same documented
  behaviour as the 2026-05-09 entry.
- **Case D — beyond-configured-TTL reaper, observed inadvertently**
  (same session `aa7d2809-…`). After the Case A and Case C cycles
  the operator detached and the discussion of Case B's tighter
  retry path ran longer than the 300 s window. `session_events`:
  `detached 00:13:11.065 → closed 00:18:11.077`, exactly
  **300.012 s** later. The reaper fires at the configured TTL,
  not the old default — strong end-to-end evidence the knob is
  load-bearing on both the reattach-cancel and the
  reaper-schedule paths. A subsequent **Reconnect** click in the
  desktop UI for this session id surfaced the
  `apps/web/src/lib/app/terminal/sessionStatus.ts` "cannot be
  reconnected" helper text — same documented limitation as the
  2026-05-09 entry, just observed at the new TTL boundary.
  No explicit "wait 5 minutes" was scheduled for this case.
- **Case B — desktop kill + relaunch reconnect within configured
  TTL** (fresh session
  `b67717e5-875d-499c-9f2c-0f4c3ae7f6f3`, second attempt). The
  first Case-B attempt overshot the window: original session
  `aa7d2809-…` was reaped by the configured TTL before the
  operator's `kill+relaunch+handoff+AuthGate+Sessions-list-render`
  round-trip completed (same operator-UX bottleneck the 2026-05-09
  entry noted at 30 s). Retry path used a fresh session against
  `custom-ttl-smoke-profile` plus tighter cadence: operator
  Detached → workstation `pkill -f
  /home/jsprague/dev/RelayTerm/target/release/relayterm-desktop$` +
  `nohup … &` (PID `1503908` → `1511854`, kill+relaunch elapsed
  **5 s** wall-clock) → operator navigated splash → AuthGate-cookie
  → AppShell → Sidebar Sessions → Open. `session_events`:
  `attached 00:31:47.312 → detached 00:32:10.428 → attached
  00:32:47.254 → reattached 00:32:47.256`, detach gap = **36.8 s**
  including the desktop process restart, `reattached` within
  **2 ms** of `attached`. The cookie persisted across the kill
  (storage at
  `~/.local/share/cc.js-node.relayterm.desktop/cookies` survives
  process death; same observation as the 2026-05-09 Case D
  reattempt) so AuthGate skipped LoginView. Sessions-list
  cross-navigation reconnect succeeded — the prior smoke's "Case
  D" finding (kill+relaunch+nav round-trip exceeds 30 s default)
  is now **resolved at 300 s** for the typical desktop, while
  remaining a real consideration for any deployment that keeps the
  default. Operators tuning this knob for restart-recovery should
  pick a value ≥ "operator's slowest expected
  bootstrap+nav round-trip" + a margin, not just "longer than 30 s".
- Backend log sweep over **2 hours** of `relayterm-backend`
  output covering pre-upgrade + post-upgrade + Cases A through D
  (workstation `ssh ubuntu@cloud-edge ... docker compose logs
  --since 2h`): zero hits across the full redaction sentinel set
  (`csrf_origin_mismatch`, `relayterm_session=[A-Za-z0-9_-]{20,}`,
  `encrypted_private_key`, `data_b64`, `REDACT-MARKER`,
  `password=`, `"password"`, `BEGIN OPENSSH`, `BEGIN PRIVATE`).
  Zero `WARN` lines and zero `ERROR` lines in the same window
  — the binary `RTB1` frame path through Traefik to the desktop
  WebView is silent on errors during normal detach / reconnect /
  replay / reaper / process-restart-and-reattach behaviour at
  the configured TTL. Source-of-truth disambiguation on every
  TTL-after-reattach question used a bounded read-only Postgres
  query inside the staging Postgres container scoped to one
  `session_id` at a time, returning only `kind` + `recorded_at`
  (no `payload` dump — same defensive default as the 2026-05-09
  entry).

Deferred (intentional non-goals for this run):

- **Durable long-term session persistence** (`tmux`/`screen`-style
  resurrection across backend restart). The configurable knob is
  a *short-term reconnect grace window* on a still-live PTY held
  by the running backend; a backend restart drops every detached
  PTY AND its replay buffer per
  [`docs/spec/terminal.md`](../spec/terminal.md) § "Output
  sequence + in-memory replay buffer contract". Long-term
  persistent sessions remain a separate, future architecture and
  are explicitly NOT delivered by this knob.
- **Backend restart survival.** Not exercised in this slice; the
  staging stack was recreated mid-smoke (twice — once for the
  initial env-var attempt, once for the lockstep image upgrade)
  but the recreate happened BEFORE Cases A–D and left the
  pre-existing terminal sessions dormant; each Case used a fresh
  session.
- **Android reconnect / Android handoff after backend restart.**
  Step 7's optional Android handoff/auth sanity was deferred —
  the desktop smoke covered the configurable-TTL knob in full,
  and nothing in this slice changed any Android-relevant surface
  beyond the lockstep image refresh which has the same shape on
  desktop and mobile. Android terminal attach + Android mobile
  background-foreground / network-flap remain deferred from
  prior runs.
- **Mobile portrait sidebar UX** — same deferred-mobile-UX row
  as the prior 2026-05-09 Android entries; out of scope for
  desktop and out of scope here.
- **Production hostname / production credentials / real production
  SSH identities** — staging is throwaway by construction (§1).
- **Tauri release-channel signing / Play Store / AAB / AppImage
  release notes** — Phase 4+ in
  [`docs/deployment/tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).
- **Recording surface.** `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`
  on this slot per `.env`; recording chunks did not exist for any
  smoke session.
- **Alternate renderer adapters** — only
  `@relayterm/terminal-xterm` baseline was exercised; the
  experimental ghostty-web / restty / wterm adapters were not.
- **Multi-tab / multi-client collaborative attach** — single
  session at a time throughout.
- **Operator-initiated TOFU re-pin / revoke-and-replace surface**
  (see "Drift worth folding back later" below).
- **CI / signing / auth / CORS / CSRF behaviour changes** — none
  in scope, none made.

Drift worth folding back later (non-blocking):

- **Compose template plumbing for new env knobs.** This run's
  load-bearing finding: the configurable-TTL commit `e28b009`
  added the env var to the `.env` and TOML examples but missed
  both Compose templates AND the implicitly-distributed runbook
  Compose. The template-template gap is a recurring shape (every
  `RELAYTERM_…` env knob requires THREE coordinated edits — the
  Rust config struct, the `.env`/TOML examples, AND the
  `${VAR:-default}` interpolation row in
  [`deploy/docker-compose.example.yml`](../../deploy/docker-compose.example.yml)
  and
  [`deploy/docker-compose.traefik-staging.example.yml`](../../deploy/docker-compose.traefik-staging.example.yml));
  consider a future CI / pre-commit check that grep-cross-references
  the three to guarantee they stay aligned. Out of scope for this
  smoke; flagged for a future tooling slice. The on-VPS Compose
  drift is operator-side, but if the released `:main` deploy
  templates ship correctly, the on-VPS file mostly follows from
  copy-and-`docker compose pull` discipline at upgrade time.
- **Operator-initiated TOFU re-pin / revoke-and-replace.** When a
  throwaway SSH target is recreated and the existing
  `desktop-smoke-profile` saw `host_key_status: "changed"` against
  the new fingerprint, `POST .../trust-host-key` correctly returned
  **409 `host_key_conflict`** and refused to overwrite the active
  pin (per the route handler at
  [`crates/relayterm-api/src/routes/v1/server_profiles.rs:454-459`](../../crates/relayterm-api/src/routes/v1/server_profiles.rs):
  *"an active pin exists with a different fingerprint, which we
  never auto-overwrite"*; same handler's source comment notes
  *"Recovery from a revoked entry is a separate, deliberate
  operator action that does not exist yet"*). The smoke pivoted
  to a fresh host + fresh profile bound to the same identity
  rather than weakening TOFU; existing inventory rows were left
  intact per AGENTS.md "Inventory lifecycle and destructive-action
  policy". A purpose-built `POST
  /api/v1/server-profiles/:id/revoke-and-replace-host-key` (or
  similar) operator surface would let routine re-pinning happen
  without inventory clutter, with explicit "I acknowledge the
  fingerprint changed and accept the new one" intent recorded as
  a `host_key_replaced` audit row carrying public metadata only.
  Candidate edit for a future product slice; out of scope here.
  **Design landed (no implementation yet):** the operator
  revoke-and-replace flow is now specified in
  [`docs/spec/host-key-replace.md`](../spec/host-key-replace.md)
  on branch `docs/host-key-repin-design`. The route name
  `revoke-and-replace-host-key` and the audit kind
  `host_key_replaced` proposed earlier in this paragraph are
  superseded by that design — the agreed shape is `POST
  /api/v1/server-profiles/:id/replace-host-key` plus a paired
  audit emission of `host_key_revoked` + `host_key_accepted`
  (both kinds already exist in `audit_events_kind_chk`, so no
  enum migration is needed). The design also commits to the
  schema additions (`revoked_by` / `revoked_reason_code` /
  `replaced_by_id` on `known_host_entries`) and the
  typed-`REPLACE` modal UX. Backend route, repository primitive,
  and SPA wiring will land in the rollout PRs enumerated in that
  doc; this smoke entry will pin the staging-side verification
  on the final PR. **Update (Phase 4 ready):** Phases 1-4 of the
  design are now landed (schema, route, API helpers, UI).
  `HostKeyPanel.svelte` carries the `Replace trusted host key…`
  affordance + typed-`REPLACE` modal end-to-end against the live
  backend route. The next staging smoke (Phase 5) is the
  remaining deferred work — pin the operator-initiated
  revoke-and-replace flow against a recreated throwaway target
  and confirm the paired audit pair shows up cleanly in the
  audit feed.
- **Image-tag-vs-commit drift visibility at smoke start.** This
  run discovered ~halfway through that the running staging
  backend image (`sha256:596d8c270d…`, built `2026-05-09T17:05:07Z`)
  predated the configurable-TTL commit `e28b009` (committed
  `2026-05-09T22:31:16Z`) by ~5 h 26 min. The runbook's §5 "Pull
  the images" assumes the operator pulls before bringing up; on a
  long-lived staging slot, the running image can drift behind the
  branch tip without any surface signal. A simple
  `docker compose pull` + image-digest-printed pre-flight before
  every smoke (or a runbook §7.3 addition documenting the digest
  check) would catch this. The runbook §10 lockstep rule already
  pins the discipline; surfacing it in the smoke gate is the
  candidate edit.
- **Default detached-PTY TTL value — keep at 30 s in the
  templates, override per-deployment.** The `:-30` default in
  the new template lines matches the historical hard-coded
  value. The 30 s default is still sane for a default-posture
  deployment (lower backend RAM / fd / SSH-PTY-budget consumption,
  faster reaper-of-orphan); 300 s should be a *slot-specific
  override* for slots that need restart-recovery, not a global
  default-bump. This is the explicit posture in
  `docs/config-examples/relayterm.production.example.toml`'s
  "SCOPE — read this before bumping the value" preamble; calling
  it out here so future operators reading this entry don't
  conclude "300 s should be the default" from the smoke results.
- **Restart-recovery TTL sizing rule of thumb.** The Case-B
  retry's 36.8 s detach gap (including `pkill` + relaunch +
  splash + AuthGate + Sessions-render + Open click) suggests a
  practical floor of ~60–120 s for desktop restart-recovery. The
  300 s value used here is generous; a deployment that wants
  restart-recovery without paying the full 300 s of held PTY +
  fd budget could pick 60–120 s safely on a warm-cookie path. A
  longer value makes sense when LoginView round-trips on an
  expired cookie are routine. The two example configs already give
  range guidance in their preambles
  ([`docs/config-examples/relayterm.production.example.toml`](../config-examples/relayterm.production.example.toml)
  cites `600–1800s`;
  [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
  cites `300–1800s`); a future tooling slice could reconcile the
  two and fold in this slice's measured 36.8 s "kill+relaunch+nav"
  observation as a sizing rationale. Not in scope here.

### 2026-05-09 · Desktop Tauri staging reconnect / detach / replay smoke

Picks up from the 2026-05-09 first end-to-end staging entry below
and the 2026-05-09 Android terminal-attach entry above (same VPS
slot, same image tag `:main`, same throwaway bootstrap user, no
teardown between runs). Closes the "long-lived reconnect /
replay-buffer correctness" deferred row from the first 2026-05-09
entry by walking the production-terminal Detach / Reconnect
lifecycle on desktop against HTTPS staging — short-detach reconnect
within the TTL window, replay handshake when output arrives during
a detach gap, beyond-TTL reaper behaviour, and desktop kill+restart
via the Sessions-list cross-navigation reconnect. The slice is
explicitly NOT a session-persistence-across-restart slice: the
verified behaviour is the in-memory replay ring buffer covering a
brief gap, not durable session resume.

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Desktop binary:** existing
`target/release/relayterm-desktop` from the 2026-05-08 build (no
rebuild — the bundled SPA is only used pre-handoff; the post-handoff
SPA is fetched fresh from staging). **Throwaway SSH target:**
`linuxserver/openssh-server:latest` container
`relayterm-staging-smoke-ssh` joined to
`relayterm-staging_relayterm-staging-internal` (key-auth-only,
`PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`, listens on **port
`2222`**, no host port published; user `smoke` with shell
`/bin/bash`; auto-generated host keys at first start; throwaway —
torn down at end of run). Same shape as the prior 2026-05-09
Android terminal-attach entry above; ed25519 host fingerprint
`SHA256:sF9pMtVqW9pgXfyUd/9of6SEdFUkbLanb8ZgobbX05g`,
byte-identical to `docker exec ... ssh-keygen -lf
/config/ssh_host_keys/ssh_host_ed25519_key.pub`. Inventory
created fresh per the AGENTS.md "Inventory lifecycle and
destructive-action policy" — Android-smoke `smoke-ssh-android`
host + `android-smoke-profile` left untouched; created **new**
host `smoke-ssh-desktop`
(`802fc0c0-dde6-4e64-babf-7913b0d82b05`,
`relayterm-staging-smoke-ssh:2222 smoke`) and **new** profile
`desktop-smoke-profile`
(`14bfb3d9-141f-4ada-83c8-33ede1217ba3`) bound to the existing
reused `smoke-id` ed25519 identity
(`44b5e2be-29c2-4eb0-b6ac-3b4e25ca789d`).

**Pinned contract under test.** The detached-PTY TTL is
`Duration::from_secs(30)` per
`crates/relayterm-terminal/src/manager.rs:94 — pub const
DETACHED_LIVE_PTY_TTL`. Reconnect within that window is documented
to cancel the scheduled close
(`crates/relayterm-terminal/src/manager.rs:914-919, 956-970`:
`cancel_pending_close` runs first, then the row is set back to
`Active`, then a `Reattached` event is appended). Replay covers
output frames *after* the client's `last_seen_seq` bookmark; it does
NOT preserve the renderer's local scrollback — the renderer
`dispose()` on Detach destroys the xterm.js grid and `mount()` on
Reconnect creates a fresh canvas
(`docs/spec/web-shell.md` § "TTL and replay limitations"). A
backend restart drops every detached PTY AND its replay buffer
(`docs/spec/terminal.md` § "Output sequence + in-memory replay
buffer contract").

Setup path mirrored the 2026-05-09 Android terminal-attach entry
above: inventory CRUD + host-key preflight + trust + auth-check
driven from the workstation against the staging API with the
session cookie held in `chmod 600` `/tmp/...cookie`, never echoed
in any tool output, log, or doc. Bootstrap credentials sourced
from `/home/ubuntu/docker/relayterm-staging/.bootstrap-credentials`
on `cloud-edge` and copied to a `chmod 600` local `/tmp/...creds`
that was `shred -u`'d at end of run. Cookie file shredded at end
of run.

Verified:

- HTTPS reachability gate (§7.3) re-checked from the workstation
  before any container or device action: `/` → 200, `/healthz` →
  200, `/api/v1/auth/me` → 401 JSON. Staging stack carried over
  without restart from the prior 2026-05-09 entries (`docker
  compose ps` showed all three services `Up 3 hours (healthy)`).
- Setup-API path: `POST /api/v1/auth/login` set
  `relayterm_session` cookie; subsequent
  `POST /api/v1/hosts` (display_name=`smoke-ssh-desktop`),
  `POST /api/v1/server-profiles` (name=`desktop-smoke-profile`,
  bound to `smoke-id`), `POST .../host-key-preflight`
  (returned `host_key_status: "unknown"` with the new container's
  ed25519 fingerprint), public-key injection into
  `/config/.ssh/authorized_keys` (`smoke:users 600`), and
  `POST .../trust-host-key` (returned `known_host_entry_id
  7f7d6473-6f61-45c1-8fb8-599c3262c015`) all succeeded.
  `POST .../auth-check` returned `status: "authentication_succeeded"`,
  `message: "ssh public-key authentication succeeded; no PTY was
  allocated and no command was executed"`.
- **Case A — short detach/reconnect within the TTL window
  (session `532ffb9b`).** Operator launched the production
  terminal from `desktop-smoke-profile` via `Sidebar → Server
  profiles → desktop-smoke-profile → Launch terminal session`,
  ran `echo relayterm-desktop-reconnect-smoke-start`, `whoami`,
  `pwd`. Detach → wait ~15s → Reconnect → `echo
  relayterm-desktop-reconnect-smoke-resumed` round-tripped: input
  → backend → SSH → bash → output → render. **Replay observation:**
  the renderer's prior visible state was NOT restored — the canvas
  was blank after Reconnect because the PTY was idle during the
  detach window so the replay handshake had zero new frames to
  emit (`replay_window_lost` was not surfaced; replay simply had
  nothing past `lastSeenSeq`). This is the documented behaviour:
  the renderer `dispose()` destroys the local grid; replay is a
  *resume-the-live-stream* primitive, not a *restore-the-canvas*
  primitive. Operator subsequently performed two more
  detach/reconnect cycles before the final detach;
  `session_events` ground truth (Postgres) confirmed the timing:
  `attached 20:35:18.570 → detached 20:37:17.389 → attached
  20:37:33.451 → reattached 20:37:33.454 → detached
  20:38:26.527 → attached 20:38:33.702 → reattached
  20:38:33.705 → detached 20:39:45.533 → closed 20:40:15.541`.
  Each `attached` event was followed within 4 ms by a `reattached`
  event proving the cancel-pending-close + transition-to-Active
  path on `manager.rs:919, 956-970` fired correctly. The
  `closed_at` landed at exactly 30 s after the FINAL detach, not
  the original detach — TTL cancel works as documented.
- **Case B — replay handshake observation
  (session `d44bc691`).** Operator launched a fresh session,
  backgrounded a 5-tick producer (`( for i in 1 2 3 4 5; do
  sleep 1; echo "relayterm-replay-tick-$i"; done ) &`),
  immediately Detached (within ~1 s, before any tick fired on
  screen), waited ~24 s with the client detached so all 5 ticks
  emitted to the PTY while the renderer was torn down, then
  Reconnected. **All five lines `relayterm-replay-tick-1`
  through `…-5` appeared on the canvas after Reconnect and
  before any new input** — the in-memory replay ring buffer
  emitted the held frames through the `replay_start → buffered
  output → replay_end` handshake on the new attachment.
  `session_events` confirmed: `attached 20:49:13.166 →
  detached 20:51:08.026 → attached 20:51:32.551 → reattached
  20:51:32.554 → detached 20:52:32.650 → closed 20:53:02.661`.
- **Case C — beyond-TTL reaper behaviour
  (session `d44bc691`, continued).** After the Case B
  observations the operator detached again (final detach
  20:52:32) and let the session sit. The 30 s TTL fired at
  20:53:02 (closed event). A subsequent **Reconnect** click on
  the production-terminal toolbar surfaced
  **"Connection error"** in the UI — the underlying backend
  session was already reaped, so the new WebSocket attach
  cannot land. Documented limitation per
  `docs/spec/web-shell.md` § "TTL and replay limitations" — not
  a bug.
- **Case D — desktop kill + restart with a still-live session
  (session `15c190c9`).** Operator launched a fresh session
  (`echo relayterm-desktop-restart-pre`); the desktop process
  (`PID 1399668`) was killed at the workstation; the binary
  was immediately re-spawned (`PID 1419606`); the bundled SPA
  re-executed the path-A handoff and landed at the staging
  origin; the existing same-origin session cookie survived
  the kill (cookie storage at
  `~/.local/share/cc.js-node.relayterm.desktop/cookies` is
  persisted across process death) so AuthGate ran
  `getCurrentUser()` and rendered AppShell directly with no
  re-login round-trip. Operator navigated `Sidebar → Sessions`
  to find the still-live row, but by the time the Sessions list
  rendered (cookie-attached `GET /api/v1/terminal-sessions`,
  list refresh, row scan) the session had been reaped: row
  showed `Closed` with **Open** disabled and the helper text
  **"cannot be reconnected"** (the spec-pinned literal from
  `apps/web/src/lib/app/terminal/sessionStatus.ts` /
  `apps/web/tests/sessionStatus.test.ts`). `session_events`
  ground truth: `attached 20:59:22.578 → detached
  21:00:44.412` (the WS dropped at desktop kill) `→ closed
  21:01:14.421` (TTL fired exactly 30 s later, no reattach
  events in between). **Operator-UX observation:** the 30 s
  detached-PTY TTL is shorter than a desktop kill + relaunch
  + AuthGate-resolve + Sidebar-Sessions-render round-trip
  (~30 s here, faster on a warm cache, slower on a cold
  re-login). Cross-navigation reconnect from the Sessions list
  is therefore an in-process recovery primitive, NOT a
  restart-recovery primitive. This is consistent with the
  spec's `docs/spec/terminal.md` § "TTL and replay
  limitations" — backend restart drops everything; for the
  desktop side, "process restart that exceeds 30 s" is
  effectively the same shape as backend restart from the
  client's perspective.
- Backend log sweep over 90 minutes of `relayterm-backend`
  output (workstation `ssh ubuntu@cloud-edge ... docker
  compose logs --since 90m`): zero hits across the full
  redaction sentinel set
  (`csrf_origin_mismatch`, `relayterm_session=[A-Za-z0-9_-]{20,}`,
  `encrypted_private_key`, `data_b64`, `REDACT-MARKER`,
  `password=`, `"password"`, `BEGIN OPENSSH`,
  `BEGIN PRIVATE`). The 8 WARN + 2 ERROR lines in the window
  were ALL emitted BEFORE the smoke proper (which started with
  Session A's launch at 20:35:18) and are accounted for: 4 ×
  `unauthorized request detail=missing session cookie` from
  pre-login `curl /api/v1/auth/me` reachability checks; 1 ×
  `Unknown server key` (19:40:11) from a leftover Android-smoke
  pre-trust auth-check; 3 × `Temporary failure in name
  resolution` (20:34:36–44 — pty start, preflight, auth-check)
  during the Docker DNS settling window after the new SSH
  container's `docker run`, each surfaced as a 502 Bad Gateway.
  **The smoke proper (Sessions A through D, 20:35 onward) emitted
  zero ERROR / WARN lines on the WebSocket data plane** — the
  binary `RTB1` frame path through Traefik to the desktop
  WebView is silent on errors during normal detach / reconnect
  / replay / reaper behaviour. Source-of-truth disambiguation
  on the TTL-after-reattach question used a bounded read-only
  Postgres query inside the staging Postgres container scoped
  to the four smoke `session_id`s, returning only `kind` +
  `recorded_at` (no `payload` dump, since `session_events.payload`
  can carry attachment metadata; the per-AGENTS.md redaction
  rule for audit payloads is "public metadata only" but the
  defensive default for ad-hoc smoke queries is to project
  away the column). The 28-row event timeline confirmed every
  reattach cycle wrote a `reattached` event within 4 ms of the
  matching `attached` event, and every `closed` event landed
  exactly 30 s after the immediately preceding `detached` (the
  FINAL detach, not the FIRST).

Deferred (intentional non-goals for this run):

- **Android reconnect / Android mobile background-foreground
  lifecycle / Android network-flap.** This slice was desktop
  only; Android terminal attach is verified separately above
  (2026-05-09 Android terminal attach entry).
- **Production hostname / production credentials / real
  production SSH identities** — staging is throwaway by
  construction (§1).
- **Long-lived multi-hour reconnect** — every test session
  here lived 1–5 minutes; the longest was Session A at ~5 min.
  Multi-hour cookie / session-cookie expiry / WebSocket
  keep-alive behaviour was not exercised.
- **Network flap / firewall / mid-session backend kill.**
  Detach was always client-initiated (Detach button or process
  kill); no simulated network drop, no `iptables` interference,
  no `docker compose stop relayterm-backend` mid-session.
  Whether the backend's TTL reaper survives a backend restart,
  whether replay survives a backend restart, are both
  documented as out-of-scope in the spec but not exercised
  here.
- **Backend-restart resume of a still-live session.** Per
  `docs/spec/terminal.md` § "Output sequence + in-memory
  replay buffer contract" the backend restart drops every
  detached PTY AND its replay buffer; this is documented but
  not exercised in this slice.
- **Alternate renderer adapters** — only
  `@relayterm/terminal-xterm` baseline was exercised; the
  experimental ghostty-web / restty / wterm adapters were not.
- **Recording surface.** `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`
  on this slot per `.env`; recording chunks did not exist for
  any smoke session.
- **Multi-tab / multi-client collaborative attach** — the
  `AppShell` holds a single `ActiveLaunch`; the entire smoke
  ran one session at a time.
- **Mobile portrait sidebar UX** — same deferred-mobile-UX
  row as the prior 2026-05-09 Android entries; out of scope
  for desktop.

Drift worth folding back later (non-blocking):

- **API status field vs. event timeline.** The
  `GET /api/v1/terminal-sessions` payload exposes `status` +
  `last_seen_at` + `closed_at` per row, but does NOT expose
  the per-attachment / per-event timeline. During this smoke
  the `status` field at any single poll showed the
  most-recent state (often `detached` because the operator
  was idle in detached state between cycles), and
  `last_seen_at` reflected the most-recent activity but did
  NOT chain backward through prior reattach cycles. Reading
  `last_seen_at` as "the original detach timestamp" without
  the event timeline produces a false read of "TTL didn't
  reset on reattach" — exactly the misread that triggered the
  bounded read-only Postgres-side disambiguation in this run.
  Surfacing per-attachment events on a `GET .../events` (or
  similar) authenticated route would let the operator close
  the loop without dropping into the database; until then,
  the documented disambiguation is "query `session_events`
  scoped to the session id, project `kind` + `recorded_at`
  only". Candidate edit for a future spec slice; not in scope
  here.
- **30 s TTL vs. desktop kill+relaunch round-trip.** Case D
  observed that the kill + relaunch + AuthGate-resolve +
  Sessions-list-render round-trip exceeds 30 s on a warm
  cache and a still-valid cookie — i.e. the absolute fastest
  path. Cross-navigation Sessions-list reconnect is therefore
  not a viable restart-recovery primitive at the **default**
  TTL. **Resolved (follow-up slice):** the detached-live-PTY
  TTL is now operator-configurable —
  `terminal_sessions.detached_live_pty_ttl_seconds` (env
  `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`),
  default **30 s**, bounded **5..=86 400** (5 s..24 h). Slots
  that need desktop / mobile reconnect after app restart can
  set 300–1800 s; resource-constrained slots can keep the
  default. **Important** — this is a *short-term reconnect
  grace window* on a still-live PTY held by the running
  backend; it does NOT make sessions survive a backend
  restart, and it is NOT durable shell persistence. Higher
  values consume backend RAM, file descriptors, and the SSH
  server's PTY budget for the full window. Long-term
  persistent sessions (`tmux`/`screen`-style resurrection
  across restarts) remain a separate, future architecture
  and are explicitly **not** delivered by this knob. See
  `docs/config-examples/relayterm.production.example.toml`,
  `deploy/relayterm.env.example`, and the
  "detached-live-PTY TTL is now operator-configurable"
  follow-up under
  `docs/spec/tauri-runtime-backend-url.md` § 11 Phase E.
- **Bundled-SPA cache vs. post-handoff SPA freshness.** The
  desktop binary used here was the 2026-05-08 build; the
  post-handoff SPA was fetched fresh from staging. The
  bundled SPA only renders the picker / Connecting splash,
  so its staleness was inert this run. The "WebKit HTTP cache
  + nginx immutable assets" Encountered Lesson (2026-05-09)
  applies only when the served bundle is hot-swapped in place
  on the staging nginx without a hash change — not the case
  here. No cache wipe was needed for this run; the staging
  stack and the served bundle had not changed since the
  previous 2026-05-09 entries.

### 2026-05-09 · first end-to-end staging smoke

**VPS host:** `cloud-edge` (`192.168.3.12`).
**Compose project:** `relayterm-staging`. **Image tag:** `:main`
(`relayterm-backend`, `relayterm-backend-migrate`, `relayterm-web`,
all built from the same CI commit).
**Origin:** `https://relayterm-staging.js-node.cc`. **Cert:** Cloudflare
DNS-01, valid; HTTP/2 termination via host Traefik.

Verified:

- `docker compose config` rendered clean (no published Postgres /
  backend ports; web on `proxy` + `relayterm-staging-internal`;
  Traefik labels target `Host(\`relayterm-staging.js-node.cc\`)` with
  `secure-chain@file` and port 80; auth envelope is `production`).
- 21 sqlx migrations applied via the one-shot `relayterm-migrate`
  container; no manual schema touch.
- HTTPS reachability gate (§7.3): `/` → 200, `/healthz` → 200 JSON,
  `/api/v1/auth/me` → 401 JSON, no redirect to a different host, no
  `Set-Cookie`, HSTS / CSP / referrer-policy headers all sourced from
  `secure-chain@file`.
- Throwaway bootstrap (§7.4) → `201 Created` user record (no
  `password`, `password_hash`, `encrypted_private_key`, or
  `private_key` field on the wire). Bootstrap window closed (§7.5).
- Tauri desktop bundled-shell path-A handoff (§8): picker accepted
  the staging URL, the WebView navigated, the SPA loaded, login
  rendered, and login with the throwaway user succeeded.
- Optional terminal-attach smoke (§8 step 7): a throwaway Alpine +
  openssh-server container on `relayterm-staging-internal` accepted
  a managed RelayTerm SSH identity (ed25519, generated in the vault).
  `host-key-preflight` captured the host key; `trust-host-key`
  pinned it; `auth-check` returned `authentication_succeeded`. A
  GUI attach from the desktop shell allocated a PTY through Traefik
  via the `/api/v1/terminal-sessions/{id}/ws` upgrade and ran
  `echo relayterm-vps-staging-smoke`, `whoami`, `pwd`. Detach was
  clean. Throwaway container was torn down at end of run.
- Redaction sentinel sweep across 4 000 lines of backend logs
  (`relayterm_session=…`, `encrypted_private_key`, `data_b64`,
  `REDACT-MARKER`): zero hits.

Deferred (intentional non-goals for this run):

- Production hostname / production credentials / real production
  SSH identities — staging is throwaway by construction (§1).
- Long-lived reconnect / replay-buffer correctness under network
  flap.
- Android staging smoke. Mobile shell did not exercise the staging
  origin in this window.
- Tauri release-channel signing / Play Store / AppImage. Desktop
  shell ran from the locally-built `target/release/relayterm-desktop`.
- Recording surface. `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`
  for this slot per `.env`.

Drift worth folding back later (non-blocking):

- §7.5 ("Close the bootstrap window") claims a subsequent
  `POST /api/v1/auth/bootstrap` returns `409 already_bootstrapped`
  or `503`. With the token unset in `.env` and the backend
  recreated, the observed reject code is `401 unauthorized` —
  still safe (a stranger cannot bootstrap), but the documented
  status set should include `401`. Address in the next runbook
  edit; not blocking for this run.
- §3 / §4 show the `.env` colocated with the Compose file. The
  VPS convention here split them: `docker-compose.yml` lives at
  `/home/ubuntu/docker-compose/relayterm-staging/`, `.env` at
  `/home/ubuntu/docker/relayterm-staging/`. All compose calls in
  this run used `--env-file /home/ubuntu/docker/relayterm-staging/.env`
  to bridge. The split mirrors the existing per-service convention
  on this host (compose defs vs persistent state). Folding the
  split into the runbook (or explicitly noting both layouts as
  acceptable) is a candidate edit.
- §8 step 2 recommends wiping `WebKitCache,CacheStorage` only,
  preserving `localStorage` so the **Change Server** affordance is
  exercisable. In practice, when the saved `relayterm.backend-config.v1`
  points at a now-dead local-stack origin (here:
  `http://localhost:8081`), the auto-handoff timer fires before a
  human can click **Change Server** and the WebView ends at a
  `Could not connect to 127.0.0.1: Connection refused` page. The
  recovery path (kill shell, also wipe `localstorage`, relaunch →
  picker renders directly) is the documented fallback in step 2's
  "No saved config" sub-bullet, but the timing race against the
  splash auto-navigation deserves a louder callout.

### 2026-05-09 · Android Tauri staging terminal attach smoke

Picks up from the 2026-05-09 Android handoff + login entry below
(same VPS slot, same image tag, same throwaway bootstrap user, same
device, no APK rebuild — the existing debug build from the handoff
slice was reused). Closes the Android-terminal-attach deferred row
that the 2026-05-09 handoff entry called out as "intentional non-goal
for this slice". Path A is now verified end-to-end on Android through
the binary terminal data plane: bundled-shell handoff → login →
identity / host / profile / preflight / trust / auth-check → WebSocket
attach → PTY → bash → harmless command round-trip.

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Device:** Samsung Galaxy S10e (`SM-G970U`, codename `beyond0q`,
serial `R38N500TY3E`) — same physical device + same installed debug
APK (`cc.js_node.relayterm.mobile.debug`, PID stayed alive across the
gap between slices).
**Throwaway SSH target:** `linuxserver/openssh-server:latest`
container `relayterm-staging-smoke-ssh` joined to
`relayterm-staging_relayterm-staging-internal` (the same internal
network the staging backend dials over). Hostname
`relayterm-staging-smoke-ssh`, sshd port `2222`, **no host port
published**. User `smoke` (`PUID=1000`, `PGID=1000`,
`USER_NAME=smoke`, `PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`).
Auto-generated host keys (RSA / ECDSA / ED25519) at first start; no
real SSH private key material from any production host. Throwaway
container; not committed to any image.

**Setup path.** The Android UI was used for the load-bearing surface
(WebSocket attach + PTY round-trip); the inventory / preflight /
trust / auth-check setup was driven from the workstation against the
staging API with a session cookie persisted to a `chmod 600` file
under `/tmp` and never echoed in any command output or log line.
Rationale: the inventory CRUD UIs are not Android-specific (the
desktop smoke already covered them) and copying a long
`ssh-ed25519 ...` public key off the phone screen onto the SSH
target's `authorized_keys` is mechanically impractical on a mobile
keyboard (mirrors the operator-UX caveat from the prior 2026-05-09
Android handoff entry on subaddressed-email typing). The terminal
attach itself remained entirely on the phone — that is the surface
this slice exists to verify.

Verified:

- HTTPS reachability gate (§7.3) re-checked from the workstation
  before any device or container action: `/` → 200, `/healthz` → 200,
  `/api/v1/auth/me` → 401 JSON. Staging stack carried over from the
  prior 2026-05-09 entries without restart.
- Throwaway SSH target started cleanly on the staging internal
  network (`docker run -d --name relayterm-staging-smoke-ssh
  --hostname relayterm-staging-smoke-ssh --network
  relayterm-staging_relayterm-staging-internal --restart no -e
  PUID=1000 -e PGID=1000 -e USER_NAME=smoke -e PASSWORD_ACCESS=false
  -e SUDO_ACCESS=false -e TZ=UTC linuxserver/openssh-server:latest`).
  Container reached `Up`; sshd listening on `0.0.0.0:2222` and
  `:::2222` per `netstat` inside the container; user `smoke` with
  home `/config` and shell `/bin/bash` provisioned by the image's
  `USER_NAME` env. Container reachability from the staging backend
  was deferred to RelayTerm's own host-key preflight (which is the
  only client that matters for the smoke); the staging backend's
  slim image has neither `nc` nor a `bash` with `/dev/tcp` so a
  direct shell-level reachability probe was not run.
- RelayTerm setup via the staging API, with the session cookie kept
  in `/tmp/relayterm-android-smoke.cookie` (`chmod 600`, never
  echoed): existing `smoke-id` ed25519 identity from the prior VPS
  smoke was reused; **new** host `smoke-ssh-android` created
  pointing at `relayterm-staging-smoke-ssh:2222` user `smoke`; **new**
  server profile `android-smoke-profile` bound the new host + reused
  identity. The previous `smoke-ssh` host + `smoke-profile` profile
  (pointing at the now-removed Alpine container from the first
  2026-05-09 entry) were left in place rather than mutated, per the
  AGENTS.md "Inventory lifecycle and destructive-action policy"
  (`server_profiles` default destructive action is **disable**, not
  delete; `hosts` delete is blocked while a profile references them).
  Host-key preflight captured the SSH target's ed25519 host key with
  fingerprint `SHA256:/Y3n454qkT0GFzN4PilNrfS1ljblIGn9l+nDnnkpfOU`
  (`host_key_status: "unknown"` as expected on a never-trusted host);
  fingerprint cross-verified **byte-identical** against the
  container's own `/config/ssh_host_keys/ssh_host_ed25519_key.pub`
  via `docker exec ... ssh-keygen -lf`. Trust pinned via
  `POST /api/v1/server-profiles/<id>/trust-host-key` with
  `expected_fingerprint` (the field name is `expected_fingerprint`,
  not `expected_host_key_fingerprint`); response carried
  `known_host_entry_id` `49804d24-d013-4746-a4d1-3d6bb1529129`.
  `auth-check` returned `status: "authentication_succeeded"`,
  `message: "ssh public-key authentication succeeded; no PTY was
  allocated and no command was executed"`. Public key was injected
  into `/config/.ssh/authorized_keys` inside the container with
  `smoke:users` ownership and mode `600` between the preflight and
  the auth-check steps (preflight does not need authentication;
  auth-check does).
- **Terminal session attach + PTY round-trip on Android.** Three
  successive terminal sessions were launched against
  `android-smoke-profile` from the Android UI (`Sidebar → Server
  profiles → android-smoke-profile → Launch terminal session`).
  The first session paint hit the same cold-start race documented for
  the desktop smoke ("the initial PS1 was emitted to the PTY before
  the WebSocket frame pump caught up so the first paint showed only a
  blinking cursor; one Enter triggered bash to redraw the prompt"
  — see [`docs/spec/tauri-runtime-backend-url.md`](../spec/tauri-runtime-backend-url.md)
  Phase E desktop terminal-attach row). Operator typed `ls` to nudge
  the prompt and got back the bash response (a single empty `ls`
  output line + the redrawn `relayterm-staging-smoke-ssh:~$ `
  prompt) — input → backend → SSH → bash → output → render proven
  end-to-end on Android. Session 1 closed after a screen-sleep + UI
  dump pause crossed the 30-second detach reaper window; session 2
  was launched but the workstation-side approval gate added enough
  latency that the canonical commands missed the reap window again
  ("Send attempted after session ended" toast on the SPA, audible
  in the API as `terminal-sessions[].status: "closed"`); session 3
  was launched + nudged + driven without an interleaved approval
  gate, and the three canonical commands all round-tripped:
  - `echo relayterm-android-staging-smoke` → `relayterm-android-staging-smoke`
  - `whoami` → `smoke`
  - `pwd` → `/config`

  Session 3's UI status row showed `Status: live` with a fresh
  `last_seen_at`; canvas rendered correctly in landscape mode at
  the device's native resolution; input was driven via
  `adb shell input text` + `KEYCODE_ENTER` (66) (mobile keyboard
  re-typing was bypassed for the same reason it was for the prior
  handoff slice's email field). Output was captured via
  `adb shell screencap` because the WebView's terminal canvas is
  HTML5 canvas, not native widgets, so `uiautomator dump` does not
  see terminal contents (a known mobile-smoke evidence-collection
  caveat — captured here so future runs reach for `screencap` first
  rather than `uiautomator dump`).
- Backend log sweep over 1 hour of `relayterm-backend` output
  (`csrf_origin_mismatch`, `relayterm_session=[A-Za-z0-9_-]{20,}`,
  `encrypted_private_key`, `data_b64`, `REDACT-MARKER`, `password=`,
  `ERROR`): zero hits. The 2 `WARN` lines in the window were both
  expected: one `russh connect failed during auth-check error=Unknown
  server key` from the very first auth-check call before host-key
  trust was pinned (later auth-check after trust returned
  `authentication_succeeded`), and one `unauthorized request
  detail=missing session cookie` from the workstation's
  unauthenticated `curl /api/v1/auth/me` in step 1. Both carried
  generic detail strings only — no email, password, token, IP, or
  correlated identifier in any payload. **The terminal-data-plane
  WebSocket attach / detach / re-attach cycles produced zero
  WARN / ERROR lines at all** — the binary `RTB1` frame path through
  Traefik to the Android Tauri WebView is silent on errors.
- Path A premise extends from desktop bundled handoff and Android
  bundled handoff (both 2026-05-09 against HTTPS staging) into the
  **Android binary terminal data plane** with **zero** backend / auth
  / CORS / CSRF / Tauri-capability code change. Same-origin Tauri
  Android WebView session-cookie / `Origin`-allow-list flow + the
  `/api/v1/terminal-sessions/{id}/ws` upgrade + `RTB1` frame pump +
  `russh` PTY allocation work end-to-end through Traefik HTTPS
  exactly as they do on desktop (per the prior desktop-vs-staging
  terminal-attach row in the first 2026-05-09 staging entry below).

Deferred (intentional non-goals for this run):

- **Mobile portrait sidebar / layout optimization.** Observed during
  this run that the production `AppShell.svelte` sidebar consumes
  most of the visible portrait viewport on the device, leaving the
  active view (server-profile detail, terminal canvas, identities
  list) cramped. The smoke remained focused on terminal attach /
  WebSocket behaviour; mobile-portrait layout work — sidebar collapse
  / drawer / responsive nav — is out of scope for this slice and is a
  separate UX pass.
- **Long-lived reconnect across mobile network changes** (Wi-Fi
  ↔ LTE handoff, deep sleep, doze, low-memory kill, push-driven
  wake). Three short sessions were observed to cross the 30-second
  detach reaper cleanly when left to idle; none was held open long
  enough to verify the reconnect / replay-buffer correctness path.
- **Production hostname / production credentials / real production
  SSH identities** — staging is throwaway by construction (§1).
- **Tauri release-channel signing / Play Store / AAB** — Phase 4+
  in [`docs/deployment/tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).
- **Recording surface.** `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`
  on this slot per `.env`.
- **Alternate renderer adapters on Android** (only
  `@relayterm/terminal-xterm` baseline was exercised; the
  experimental ghostty-web / restty / wterm adapters were not).
- **"Change server" runtime click on the Android shell** (still
  deferred from the prior 2026-05-09 Android handoff entry — the
  picker did not re-enter on this run because the saved config was
  already correct).

Drift worth folding back later (non-blocking):

- **Detach reaper window vs interactive-approval latency.** The
  30-second backend reap-after-detach window can collide with any
  workstation-side interactive approval prompt (here, an
  `AskUserQuestion` modal in the operator's IDE) when the operator
  is driving the phone over `adb`. Two sessions in this run were
  reaped before the canonical commands could land because the
  operator-side approval cycle exceeded the reap window. Mitigation
  for future Android terminal-attach smokes: either (a) batch the
  approval BEFORE the session is launched and fire commands
  immediately on attach, or (b) consider a runbook-time toggle to
  briefly extend the detach reap window during a smoke. The first is
  what this run actually used (third session). Not a code or backend
  bug — the 30-second window is correct for the production posture.
- **`uiautomator dump` does not see WebView canvas content.** All
  three terminal-attach evidence captures used `adb shell screencap
  -p` because `uiautomator dump`'s accessibility tree exposes only
  native widgets, not HTML5 canvas pixels. Worth surfacing in this
  runbook as the canonical mobile-smoke evidence pattern: dump for
  chrome (buttons, modals, status rows, EditText focus state) and
  screencap for terminal-canvas content.
- **First-paint cold-start race** is now confirmed identical
  on Android and desktop — same root cause (initial PS1 emitted
  before the WebSocket frame pump catches up), same workaround (one
  Enter keystroke nudges bash to redraw). The desktop terminal-attach
  row in the spec's Phase E log already documents this; folding the
  Android observation into a single shared note (rather than
  re-stating per-platform) is a candidate edit.

### 2026-05-09 · Android Tauri staging handoff + login smoke

Picks up from the 2026-05-09 entry above (same VPS slot, same image
tag, same throwaway bootstrap user — no teardown between runs).
Closes the "Android staging smoke" deferred item from that entry's
"Deferred" list for the handoff + login halves; terminal attach on
Android remains intentionally deferred (no §8-equivalent for
mobile yet, and this slice was scoped as docs-only with an explicit
approval gate before any device action).

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Device:** Samsung Galaxy S10e (model `SM-G970U`, codename
`beyond0q`, serial `R38N500TY3E`) — the same physical device used
for the 2026-05-08 Android local-launch smoke recorded in
[`docs/deployment/tauri-local-build.md`](./tauri-local-build.md).
**APK:** debug, unsigned, universal, built on the same CachyOS host
from the `docs/android-staging-handoff-smoke` branch via
`pnpm --filter @relayterm/mobile exec tauri android build --debug --apk --ci`
(≈ 548 MB universal at
`apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`,
all four ABIs with debuginfo + bundled SPA; tracks the existing
"≈ 437 MB on 2026-05-07" baseline shape per
[`docs/deployment/tauri-local-build.md`](./tauri-local-build.md)
"Verification performed", same scaffold).

Verified:

- HTTPS reachability gate (§7.3) re-checked from the workstation
  before any device action: `/` → 200 (RelayTerm SPA HTML, HSTS
  header, no `Set-Cookie`), `/healthz` → 200 JSON, `/api/v1/auth/me`
  → 401 JSON. Staging stack carried over from the 2026-05-09 run
  without restart.
- `adb install -r` of the debug APK reported `Performing Streamed
  Install` → `Success`; `monkey -p cc.js_node.relayterm.mobile.debug
  -c android.intent.category.LAUNCHER 1` injected one event;
  re-probed `pidof` (PID 28072), `ps -A`, and `dumpsys activity
  activities` confirmed `mResumedActivity:
  cc.js_node.relayterm.mobile.debug/cc.js_node.relayterm.mobile.MainActivity`
  with `mFocusedApp` matching. Bounded filtered `logcat -d -t 600`
  snapshot returned zero `crash` / `fatal` / `exception` / `ANR` /
  `signal 1[0-9]` / `libc:` lines.
- Bundled SPA rendered the **"Connect to RelayTerm Server"** picker
  directly on first launch (no `relayterm.backend-config.v1` in the
  WebView's `localStorage`, as expected for a fresh install of the
  debug `applicationId` `cc.js_node.relayterm.mobile.debug`).
  `https://relayterm-staging.js-node.cc` typed in, **Connect**
  tapped, the validator accepted the URL (HTTPS, bare origin, no
  path/query/fragment, no userinfo per
  [`docs/spec/tauri-runtime-backend-url.md`](../spec/tauri-runtime-backend-url.md)
  § 10), the handoff persisted the canonical origin and called
  `window.location.assign("https://relayterm-staging.js-node.cc/")`,
  and the WebView reloaded into the staging origin. Same SPA bundle
  ran again at the post-handoff origin and `ConfiguredBackendGate`
  short-circuited via `decideHandoff`'s `already_at_backend` branch
  (closes the same-origin short-circuit verification on a third
  surface — the `decideHandoff — same-origin short-circuit
  (already_at_backend)` block in
  [`apps/web/tests/backendHandoff.test.ts`](../../apps/web/tests/backendHandoff.test.ts)
  pins the unit-level behaviour). `AuthGate` then ran
  `getCurrentUser()` → 401 and the SPA rendered `LoginView`.
- Throwaway bootstrap user from the 2026-05-09 VPS smoke
  (`/home/ubuntu/docker/relayterm-staging/.bootstrap-credentials`)
  authenticated cleanly through the Tauri Android WebView.
  `POST /api/v1/auth/login` set the
  `relayterm_session` cookie with `HttpOnly; SameSite=Strict; Path=/;
  Max-Age=2592000; Secure` (separately confirmed via a `curl -i` to
  the same endpoint with `Origin: https://relayterm-staging.js-node.cc`),
  the cookie attached on the subsequent `GET /api/v1/auth/me`, and
  `AuthGate` flipped to `kind: "ready"`. The production
  `AppShell.svelte` rendered: sidebar with Dashboard / Terminal /
  Sessions / Server profiles / SSH identities / Settings; top-bar
  RelayTerm title + Sign-out; Dashboard view showed Backend =
  `online`, inventory tiles `HOSTS=1`, `SERVER PROFILES=1`,
  `SSH IDENTITIES=1`, `TERMINAL SESSIONS=1` (carry-over from the
  2026-05-09 VPS smoke), and recent-activity row
  `Sign-in succeeded`.
- Redaction sentinel sweep across 2 000 lines of backend logs over
  the auth-path window
  (`csrf_origin_mismatch`, `relayterm_session=[A-Za-z0-9_-]{20,}`,
  `encrypted_private_key`, `data_b64`, `REDACT-MARKER`, `password`,
  `ERROR`): zero hits. The 8 `WARN` lines in the window were all
  expected unauthenticated paths (`bad bootstrap token`,
  `missing session cookie`, `invalid credentials`) carrying only
  generic detail strings — no email, password, token, IP, or
  correlated identifier in any payload.
- Path A premise extends from desktop bundled handoff (verified
  2026-05-09 against a throwaway local Compose stack with the
  `already_at_backend` same-origin short-circuit row in
  [`docs/spec/tauri-runtime-backend-url.md`](../spec/tauri-runtime-backend-url.md)
  § "Phase E — verification log", and against HTTPS staging in the
  prior § 12 entry above this one) to **Android bundled handoff
  against HTTPS staging behind Traefik** with **zero** backend / auth
  / CORS / CSRF / Tauri-capability code change. Same-origin Tauri
  WebView cookie / `Origin` allow-list flow works for browser-style
  auth on Android exactly as it does on desktop.

Deferred (intentional non-goals for this run):

- **Android terminal session attach.** No `/api/v1/terminal-sessions`
  POST, no WebSocket attach, no PTY allocation against any SSH
  target from the Android device. The runbook step that gates this
  is the §8 "step 7" optional terminal-attach smoke; no Android
  equivalent runs by default and none was approved here.
- Production hostname / production credentials / real production
  SSH identities — staging is throwaway by construction (§1).
- Long-lived reconnect / replay-buffer correctness under network
  flap on mobile.
- Mobile background → foreground lifecycle, doze, low-memory kill,
  push-driven wake — `tauri:android:dev` and the `relayterm-mobile`
  background-session model remain unverified per
  [`docs/deployment/tauri-local-build.md`](./tauri-local-build.md)
  "Mobile / Android — runtime caveats".
- Tauri release-channel signing / Play Store / AAB — Phase 4+ in
  [`docs/deployment/tauri-ci-release-plan.md`](./tauri-ci-release-plan.md);
  the verified APK is the debug, unsigned, universal one only.
- Recording surface. `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`
  on this slot per `.env`.
- "Change server" runtime click on the Android shell (the affordance
  is shipped on the Connecting splash and pinned by the
  `Change Server reset flow` block in
  [`apps/web/tests/backendHandoff.test.ts`](../../apps/web/tests/backendHandoff.test.ts);
  the picker rendered directly on this fresh install so the
  Connecting splash + reset path was not exercised).

Drift worth folding back later (non-blocking):

- This runbook's §8 ("Tauri desktop smoke") is desktop-specific.
  Now that the Android bundled handoff + login halves are
  end-to-end-verified against HTTPS staging, an "§8.X — Tauri
  Android smoke" companion section (or an explicit "the §8 walk
  applies symmetrically on Android via `adb install -r` + the
  picker; same expectations, same redaction sweep") is a candidate
  edit. Not in scope for this run.
- **Operator-UX caveat: subaddressed bootstrap email + Android
  software keyboard.** The bootstrap user on this slot uses a `+`
  tag (`staging+throwaway-DATETIME@example.com`); on at least
  Samsung One UI's default keyboard, the `+` lives on a secondary
  symbols layer and is easy to miss / mistype as `-` while typing
  on a phone. First sign-in attempt from the device landed
  `staging-throwaway-...` and surfaced
  `Sign in failed: invalid credentials` (the runbook's expected
  reject for a wrong-credentials branch — the throttler key uses
  `normalize_login_identifier`, so a misspelled email keys a
  *different* bucket and the correct address is unaffected on the
  next attempt). Recovery used `adb shell input text` with
  `KEYCODE_PLUS` (81) and `KEYCODE_AT` (77) splits to feed the
  literal `+` and `@` past the IME's shell layer. Worth surfacing
  in a future runbook edit because every operator who types a
  `+`-tagged staging email on a phone will hit the same trap; a
  trivial mitigation is to bootstrap staging users without `+`
  subaddressing (keep `-` separator only). Not in scope for this
  run.

---

## See also

- [`deploy/docker-compose.traefik-staging.example.yml`](../../deploy/docker-compose.traefik-staging.example.yml)
  — the Compose template this runbook installs on the VPS.
- [`docs/deployment/production-runbook.md`](./production-runbook.md)
  — production deploy / upgrade / rollback / smoke (the long form
  many sections here defer to).
- [`docs/deployment/docker-compose.md`](./docker-compose.md) — Compose
  stack reference, env contract, reverse-proxy notes, same-origin
  contract.
- [`docs/spec/tauri-runtime-backend-url.md`](../spec/tauri-runtime-backend-url.md)
  — bootstrap-picker / backend-URL handoff contract.
- [`docs/deployment/tauri-local-build.md`](./tauri-local-build.md) —
  desktop / mobile local build, AppImage strip workaround, WebKitGTK
  cache caveat.
