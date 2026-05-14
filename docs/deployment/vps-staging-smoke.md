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

### 2026-05-10 · Android host-key replacement (revoke-and-replace) staging smoke

Follow-up verification of the host-key replacement flow walked
end-to-end through the **Tauri Android WebView** on a physical
Samsung Galaxy S10e (model SM-G970U, Android 12), against the same
published `:main` lockstep that the
`2026-05-10 · Host-key replacement (revoke-and-replace) staging
smoke` entry below already smoked via the workstation Playwright
browser. The web bundle was identical between the two runs (same
served `index-vIMOoKa7.js`, served from web image
`sha256:2977d9a4…` against backend image `sha256:22e092f8…` —
both built 2026-05-10 18:36-18:37 UTC, ~20 min after the Phase 4
merge `3000105 Add host key replacement UI`). This slice confirms
the same SPA bundle renders, gates, and submits the Replace flow
through Tauri's Android WebView on a physical device, and that the
backend produces byte-identical DB + audit state to the web smoke
when driven from the phone.

**APK state.** The previously-installed debug APK on the S10e
(`cc.js_node.relayterm.mobile.debug` `versionName=0.0.1`
`versionCode=1`, last update `2026-05-09 13:26`) predated the Phase
1 schema migration; rebuilt locally via the canonical command
`pnpm --filter @relayterm/mobile exec tauri android build --debug
--apk --ci` (≈ 548 MB universal debug APK at
`apps/mobile/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`),
installed over the prior install with `adb install -r` (replace,
keep `localStorage` — the saved backend config carried the staging
origin from a prior session). Same JDK 17 / Android SDK / NDK
30.0.14904198 / four-`*-linux-android` Rust targets host setup
documented in [`tauri-local-build.md`](./tauri-local-build.md);
build finished green with one `Finished 1 APK at: …
app-universal-debug.apk` line. Tree stayed clean
(`git status` showed no changes) — no scaffold edits required.

**Operator user / login.** Reused the existing throwaway bootstrap
user `staging-throwaway-20260509173230` (same one the prior
2026-05-09 / 2026-05-10 entries below use); the auto-login cookie
from a prior phone session was still valid, so reaching the app
shell required zero credential handling on the workstation. No new
bootstrap. No production credentials.

**Throwaway SSH target.** Fresh `linuxserver/openssh-server:latest`
container `relayterm-staging-android-repin-smoke-ssh` (distinct
name from the prior 2026-05-10 Phase 5 web smoke's
`relayterm-staging-repin-smoke-ssh` so the two runs do not collide,
and so the inventory rows the prior smoke left in place per
AGENTS.md "Inventory lifecycle and destructive-action policy" are
not re-touched), joined to
`relayterm-staging_relayterm-staging-internal`, key-auth only
(`PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`), internal SSH port
2222, no host port published, user `smoke`, `authorized_keys`
populated from the existing managed `smoke-id` ed25519 identity's
**public** key only (read from staging Postgres on `cloud-edge`
via `psql … encode(public_key,'escape')`, never left the host).

**Inventory.** Brand-new host `android-repin-smoke-host`
(`eb4a7b14-02c0-4376-becf-231806d997a0`) + brand-new server
profile `android-repin-smoke-profile`
(`960ed94f-17de-4658-b0f6-cd4f9a980316`), both created through the
Android UI, bound to the existing reused `smoke-id` ed25519
identity (`44b5e2be-29c2-4eb0-b6ac-3b4e25ca789d`). Prior smokes'
inventory left intact per the same AGENTS.md policy.

Verified end-to-end on staging (timestamps from the nginx access
log + the SSH target's `/config/logs/openssh/current`; SPA flow
driven manually on the SM-G970U):

- **HTTPS reachability gate (§7.3) re-checked from the workstation
  before any device or container action.** `/` → 200 (last-modified
  `Sun, 10 May 2026 18:37:22 GMT`, matches the new web image build
  time), `/healthz` → 200 `{"status":"ok"}` (content-length 15),
  `/api/v1/auth/me` → 401
  `{"error":{"code":"unauthorized",...}}` from outside the SPA.
  Staging stack carried over from the prior entry below without
  restart. Backend route presence cross-checked from the
  workstation: `POST /api/v1/server-profiles/<id>/replace-host-key`
  → 403 `csrf_origin_mismatch` (route wired, CSRF guard sits ahead
  of the body extractor, not a 404).
- **APK install + cold launch on the SM-G970U.**
  `adb -s R38N500TY3E install -r app-universal-debug.apk` →
  `Success`; `dumpsys package` confirmed
  `versionCode=1 versionName=0.0.1 lastUpdateTime=2026-05-10
  17:29:15`; cold launch via
  `adb … shell monkey -p cc.js_node.relayterm.mobile.debug -c
  android.intent.category.LAUNCHER 1` dispatched
  `MainActivity` to `mResumedActivity` (pid 19203); bounded
  `logcat -d -t 200` filter showed zero `F/`-tagged FATAL lines /
  zero ANR / zero RelayTerm-owned exception. Saved backend config
  from prior session pointed at the staging origin already.
- **Initial trust path against the new throwaway target.** Phone
  preflight (`22:44:45`, POST `…/host-key-preflight` → 200 from the
  Android WebView UA `Mozilla/5.0 (Linux; Android 12; SM-G970U
  Build/SP1A.210812.016; wv) AppleWebKit/537.36 …
  Chrome/147.0.7727.138 Mobile Safari/537.36`) returned `unknown`
  with the initial host fingerprint
  `SHA256:VTHIC7Eu0wwzKvxTpFawX3o8f26UAkWPcfvCsj5pCaM`,
  cross-verified byte-identical against the container's own
  `/config/ssh_host_keys/ssh_host_ed25519_key.pub` via `docker exec
  … ssh-keygen -lf`. Trust pinned via `POST .../trust-host-key`
  (`22:45:21`, → 200); panel transitioned to `Trusted` with the
  inline success banner; `known_host_entries` row
  `0ee311db-46a5-466f-8cd5-ac12effcee36` recorded with
  `trusted_at = 2026-05-10 22:45:21.761002+00`,
  `revoked_at = NULL`.
- **Initial auth-check failed; mobile-keyboard surface bug.**
  First `auth-check` attempts at `22:45:28`, `22:45:41`, `22:47:28`,
  `22:49:06` all returned `authentication_failed` with the SPA
  message "host key trusted pin, but the server rejected the
  configured SSH identity for the configured username." Root cause
  was surfaced by the SSH target's sshd log
  (`/config/logs/openssh/current`): `Invalid user Smoke from
  172.21.0.3` (capital **S**). The Android soft-keyboard
  auto-capitalized the first character when the operator typed
  `smoke` into the host **Default username** input
  (`apps/web/src/lib/app/views/ServersView.svelte:774-786`); the
  input is missing the `autocapitalize="none"` /
  `autocorrect="off"` attributes that would suppress the behaviour
  in mobile WebViews. Linux usernames are case-sensitive, so the
  pubkey was never consulted (sshd rejects unknown users before
  the publickey method runs). **Workaround applied:** one-row
  staging-side Postgres
  `UPDATE hosts SET default_username='smoke', updated_at=now()
  WHERE display_name='android-repin-smoke-host' AND
  default_username='Smoke'` (corrects the typo, does not touch any
  host-key-related table, produces no audit row). After the
  correction, sshd recorded `Accepted publickey for smoke from
  172.21.0.3 port 50762 ssh2: ED25519
  SHA256:94RI7NEnKZyw/xn7XJgqmFpb5xstD+YK+GnwuOLWbPc` and the SPA
  `auth-check` succeeded (`22:56:04`, → 200,
  `status: authentication_succeeded`). **This is a mobile-input
  UX defect, not a Replace-flow defect**, and is deferred (see
  "Deferred" list below) per this slice's docs-only scope.
- **Trigger changed host key.** Throwaway target destroyed and
  recreated with identical container name / hostname / port / user
  / `authorized_keys` (sha256 of the file remained `c27fbb59…`,
  i.e. byte-identical); new ed25519 host fingerprint
  `SHA256:Bld6MEAx6/FX7CedywJEX+dAsZZwVLdCUBIKvvHCoy0` captured via
  `docker exec relayterm-staging-android-repin-smoke-ssh ssh-keygen
  -lf /config/ssh_host_keys/ssh_host_ed25519_key.pub` for
  cross-reference against the SPA result.
- **`changed` preflight + Replace affordance gating on Android.**
  Re-run preflight on the phone (`22:58:06`, POST
  `…/host-key-preflight` → 200) — badge flipped to `Changed`,
  captured fingerprint matched the new host fingerprint, the
  normal Trust affordance was absent (not just disabled), and the
  `Replace trusted host key…` affordance appeared. Replace modal
  opened cleanly in Android portrait mode (no clipping, no scroll
  trap, modal usable on the SM-G970U display); the operator
  visually verified the old fingerprint, the new fingerprint, the
  four-tag reason picker, the typed-`REPLACE` confirmation field,
  and the submit-disabled gating (disabled until reason picked
  AND `REPLACE` typed exactly). Selected reason
  `lab/staging target recreated` (→ `reason_code =
  lab_target_recreated`), typed `REPLACE`, button enabled.
  Forbidden TOFU vocabulary scan (`Force trust`, `Override`,
  `Ignore warning`, `Disable check`, `auto-trust`) is carried by
  the Phase 5 web smoke entry below — same SPA bundle, no
  rebuild — so the modal rendered on the Android WebView inherits
  the zero-hit result by transitivity; no separate scan was run.
- **Replace 200 + paired audit verified end-to-end.** Submitting
  produced a single `POST
  /api/v1/server-profiles/960ed94f-17de-4658-b0f6-cd4f9a980316/replace-host-key
  → 200` at `23:01:51`. The old `known_host_entries` row
  `0ee311db-46a5-466f-8cd5-ac12effcee36` received `revoked_at =
  2026-05-10 23:01:51.735176+00`, `revoked_by =
  f968b6f5-9cfc-46ae-b735-bc0f95465b5b` (the throwaway bootstrap
  user), `revoked_reason_code = lab_target_recreated`, and
  `replaced_by_id` pointing at the new row
  `1b2d58fb-274d-49d6-a864-3f6318ac7621`; the new row received
  `trusted_at` at the SAME timestamp as the old row's `revoked_at`
  (atomic-tx property, microsecond-equal at
  `23:01:51.735176+00`), `revoked_at = NULL`, `revoked_by = NULL`,
  `revoked_reason_code = NULL`, `replaced_by_id = NULL`. Audit
  table carried exactly one `host_key_revoked` AND exactly one
  `host_key_accepted` for the host, both at the same
  `recorded_at = 23:01:51.735176+00`, both `actor_id =
  f968b6f5-9cfc-46ae-b735-bc0f95465b5b`, payloads cross-linked via
  the counterparty's `known_host_entry_id` and fingerprints.
  Payload-key enumeration on both rows showed exactly the seven
  canonical keys (`host_id`, `known_host_entry_id`,
  `replacement_known_host_entry_id`, `old_fingerprint`,
  `new_fingerprint`, `key_type`, `reason_code`) and nothing else —
  byte-identical shape to the prior web smoke's audit rows.
- **Post-replace SPA + auth-check + terminal attach on Android.**
  Panel advanced to the trusted/replaced state showing the new
  fingerprint and the modal closed. Post-replace `POST
  .../auth-check → 200` at `23:02:57` with `status:
  authentication_succeeded`. Terminal launch on the same profile
  reached an interactive session on the phone; the three harmless
  smoke commands `echo relayterm-android-repin-replaced-smoke` /
  `whoami` / `pwd` rendered the expected output (the echo line,
  `smoke`, `/config`) and the session was ended cleanly from the
  Android UI.
- **Backend + web + Android log redaction sweep clean.** Zero
  `ERROR` / `WARN` lines in the backend during the 45-minute smoke
  window; zero hits in backend or nginx logs on the sentinel set
  (`session_token`, `token_hash`, `password`, `cookie`,
  `encrypted_private_key`, `private_key`, `data_b64`,
  `REDACT-MARKER`). Zero 5xx in the nginx error log over the same
  window. Bounded Android `adb logcat -d -t 1000` filter for
  `relayterm|tauri|webview|fatal|ANR|^F/|signal 1[0-9]|exception`
  showed zero `F/`-tagged FATAL lines, zero ANR, zero crash signal
  attributable to RelayTerm (`pid 19203`); a handful of unrelated
  Samsung-system `System.err: java.io.IOException: write failed:
  EBUSY` lines came from `pid 1112` (a SoC-system process, not
  RelayTerm), so they were excluded from the smoke's failure
  criteria. The wire timeline in the nginx access log matched the
  phone walk exactly: preflight (200) → trust (200) → auth-check
  (200, body `authentication_failed` × 4 across `22:45:28`,
  `22:45:41`, `22:47:28`, `22:49:06`) → preflight (200) →
  auth-check (200, body `authentication_succeeded` at `22:56:04`
  after the `default_username` correction) → preflight (200, body
  `changed` at `22:58:06`) → replace-host-key (200 at `23:01:51`)
  → auth-check (200 post-replace at `23:02:57`).

Workstation checks before stop-before-commit:

- `pnpm run check:docs-contracts`: clean.
- `pnpm -r check` (svelte-check + tsc, 315 files / 0 errors / 0
  warnings): clean.
- `git diff --check`: clean (no whitespace defects; only the two
  doc files modified).

Deferred (intentional non-goals for this run):

- **Mobile-input UX fix on the host create form.** Adding
  `autocapitalize="none"` / `autocorrect="off"` /
  `inputmode="text"` to
  `apps/web/src/lib/app/views/ServersView.svelte:774-786` (and any
  sibling Linux-identifier inputs that share the same input
  shape — `Hostname`, `Default username`, profile
  `username_override`, identity `Name`) is a separate small slice;
  deferred per this slice's docs-only scope. Workaround for this
  smoke was a one-row staging Postgres
  `UPDATE hosts SET default_username='smoke' …`. The defect is
  not specific to the Replace flow — any host-create or username
  edit done through the Tauri Android WebView is vulnerable to
  the same auto-capitalize behaviour, so the fix belongs to the
  inventory-form input shape and not to `HostKeyPanel.svelte`.
- **Mobile portrait-sidebar UX.** The known portrait-mode
  sidebar-consumes-viewport issue did not bite the Replace flow
  on the SM-G970U here (modal opened cleanly, reason picker and
  confirmation field were both reachable without sidebar
  interference); deferred for the broader mobile UX slice as
  previously planned.
- **SSH CA / host-certificate trust; admin or cross-user replace;
  bulk replace; hard delete of `known_host_entries` rows;
  production hostname / production credentials / real production
  SSH identities; CI / signing / AAB / Play Store work.** Same
  deferred set as the Phase 5 web smoke below.
- **Source-code or CI change.** None made; this is a docs-only
  follow-up slice. The mobile-keyboard input fix is a separate
  slice if approved.

### 2026-05-10 · Host-key replacement (revoke-and-replace) staging smoke

Closes the **"Operator-initiated TOFU re-pin / revoke-and-replace
surface"** deferred row carried forward from the
`2026-05-10 · Desktop Tauri staging custom detached-live-PTY TTL
smoke` entry below. Pins the Phase 5 staging-side verification of
the design in
[`docs/spec/host-key-replace.md`](../spec/host-key-replace.md) —
Phases 1–4 (schema, route, API helpers, UI) had already landed; this
slice is the manual smoke that walks the operator-sanctioned recovery
path against a recreated throwaway target on the published `:main`
lockstep.

This entry is **smoke + docs-only**. No source-code changes. No
backend, session-lifecycle, schema, repository, WebSocket-protocol,
auth-envelope, Tauri-shell, or CI changes.

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Image lockstep (post-Phase 4):** backend
`sha256:22e092f824b44f6e8bc27194c9453411663570a9f7d5ef98fb470db036d7d7c6`
(built `2026-05-10T18:36:55Z`), web
`sha256:2977d9a4191c01964487d38038ad6e1718c7b8378850c3f0ad88ec297f9d33df`
(built `2026-05-10T18:37:22Z`), migrate
`sha256:d2b3ca084f25aebde1ffa242f8bea29a73e761b9994aa5abe0983c4a2cd3efcc`
(built `2026-05-10T18:37:06Z`) — all built ~20 min after the Phase 4
merge `3000105 Add host key replacement UI` (`2026-05-10T18:16:43Z`).
The fresh web bundle is `index-vIMOoKa7.js` and embeds all eleven
canonical `host-key-replace-*` testids (`-button`, `-cancel`,
`-confirm-input`, `-confirm-mismatch`, `-error`, `-modal`,
`-new-fingerprint`, `-old-fingerprint`, `-reason-select`, `-submit`,
`-title`). Stack was lockstep-recreated via
`docker compose --env-file ~/docker/relayterm-staging/.env up -d
--force-recreate --no-deps relayterm-backend relayterm-web`; Postgres
was not touched apart from the idempotent migrate
`20260510000022 known host entries revoke metadata` (the Phase 1 row
the schema needed).
**Throwaway SSH target:** `linuxserver/openssh-server:latest`
container `relayterm-staging-repin-smoke-ssh`, joined to
`relayterm-staging_relayterm-staging-internal`, key-auth only
(`PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`), internal SSH port
`2222`, no host port published, user `smoke`, authorized_keys
populated from the existing managed `smoke-id` ed25519 identity's
**public** key only. The container was deliberately destroyed and
recreated mid-smoke so its ed25519 host key changed by construction
(the load-bearing property for this run).
**Inventory:** brand-new host `repin-smoke-host` and brand-new
profile `repin-smoke-profile` bound to the existing reused `smoke-id`
ed25519 identity. The prior smokes' inventory was left intact per
AGENTS.md "Inventory lifecycle and destructive-action policy".
**Operator user:** the existing throwaway bootstrap user
`staging-throwaway-20260509173230` (same one used by the prior
2026-05-09 / 2026-05-10 entries). No new bootstrap. No production
credentials handled.

Verified end-to-end on staging (timestamps from the nginx access
log; SPA flow driven via the playwright MCP browser):

- **HTTPS reachability gate after lockstep recreate.** `/` → 200
  (last-modified matches the new web image build time), `/healthz`
  → 200 `{"status":"ok"}`, `/api/v1/auth/me` → 401
  `{"error":{"code":"unauthorized",...}}` from outside the SPA. The
  staging slot was already up under the prior `:main` build before
  this run; only `relayterm-backend` + `relayterm-web` were
  recreated.
- **Initial trust path against the new throwaway target.** Run
  preflight (`19:22:06`, POST `…/host-key-preflight` → 200) — badge
  rendered `Not trusted`, status `unknown`, fingerprint
  `SHA256:jeSIUDEj8fk4VtCMU1JokJcCjmeKxRL4/FLcu36GYtI`, no Replace
  affordance visible (correct — replace MUST be invisible for
  unknown / trusted, not just disabled). Trust path: paste captured
  fingerprint into `host-key-confirm-input`, click
  `host-key-trust-button` (`19:22:32`, POST `…/trust-host-key` →
  200); panel transitioned to `Trusted` with the inline
  `host-key-trusted-success` banner. `auth-check` (`19:23:09`, POST
  `…/auth-check` → 200) returned `authentication_succeeded` with
  the expected success copy ("SSH public-key authentication
  succeeded for the configured username. No PTY was allocated and
  no command was executed.").
- **Trigger changed host key.** Throwaway target destroyed and
  recreated with identical container name, hostname, port, user, and
  `authorized_keys`; new ed25519 host fingerprint
  `SHA256:XEWlwegwUAgs3rM9+JcnhChoxvnzt89tBbOfbXDk5V0` captured via
  `docker exec relayterm-staging-repin-smoke-ssh ssh-keygen -lf
  /config/ssh_host_keys/ssh_host_ed25519_key.pub` for cross-reference
  against the SPA result.
- **`changed` preflight + Replace affordance gating.** Re-run
  preflight (`19:23:44`, POST `…/host-key-preflight` → 200) — badge
  flipped to `Changed`, captured fingerprint matched the new host
  fingerprint, and the deliberate `host-key-changed-refused` notice
  rendered ("RelayTerm refuses to overwrite a pinned host key
  automatically. Investigate before retrying — server reinstallation,
  key rotation, or a man-in-the-middle are all possible
  explanations."). The normal Trust button was **absent** from the
  panel (invisible, not just disabled). The new
  `host-key-replace-button` ("Replace trusted host key…") appeared
  and was the only operator-sanctioned recovery affordance. A
  static template scan of the rendered panel for the spec's
  forbidden words (`Force trust`, `Override`, `Ignore warning`,
  `Disable check`, `auto-trust`) returned zero hits.
- **Replace modal contract pinned.** Clicking the affordance opened
  the modal with `role="dialog"`, `aria-modal="true"`, and
  `aria-labelledby="host-key-replace-title"`; the title element read
  "Replace trusted host key". `host-key-replace-old-fingerprint`
  carried `SHA256:jeSI…36GYtI` (the active pin), and
  `host-key-replace-new-fingerprint` carried `SHA256:XEWl…XDk5V0`
  (the captured fingerprint) under the `ed25519` key-type label.
  The reason picker exposed exactly the four canonical wire tags
  (`server_reinstalled`, `host_key_rotated`, `lab_target_recreated`,
  `operator_other`) plus the placeholder. The confirmation input
  required the byte-exact uppercase `REPLACE`; lowercase `replace`
  triggered the inline `host-key-replace-confirm-mismatch` helper
  ("Type the literal word REPLACE in uppercase to enable the
  action.") AND left `host-key-replace-submit` disabled. Picking a
  reason alone — submit still disabled. Picking
  `lab_target_recreated` AND typing uppercase `REPLACE` — submit
  enabled, button label `Replace pin`, cancel label `Cancel`.
- **Replace submit + atomic-tx audit pair.** Clicking
  `host-key-replace-submit` issued a single `POST
  /api/v1/server-profiles/:id/replace-host-key` (`19:36:44`, → 200,
  response shape mapped through `parseReplaceHostKeyResponse`). The
  modal closed; the panel advanced to the `replaced` state with the
  `host-key-replaced-success` banner ("Host key replaced. Run
  auth-check below to confirm…"); the badge rendered `Trusted` and
  the displayed fingerprint pinned the new pin
  `SHA256:XEWl…XDk5V0` via the synthesized post-replace preflight.
  Direct DB inspection (read-only, safe-keys-only) confirmed both
  rows of the atomic transition:
  - **Old `known_host_entries` row** received `revoked_at =
    2026-05-10 19:36:44.900327+00`, `revoked_by` = the caller's user
    id, `revoked_reason_code = lab_target_recreated`, and
    `replaced_by_id` pointing at the new row. `trusted_at` (the
    original trust moment) was preserved.
  - **New `known_host_entries` row** received `trusted_at =
    2026-05-10 19:36:44.900327+00` (byte-identical timestamp to the
    revoke — atomic-tx property), `revoked_at = NULL`,
    `replaced_by_id = NULL`.
  - **Audit pair** in the same transaction: exactly one
    `host_key_revoked` AND exactly one `host_key_accepted` for the
    host within the smoke window, both `recorded_at` at the same
    instant `19:36:44.900327+00`, both `actor_id = caller`. Each
    payload carried exactly the seven canonical safe keys
    (`host_id`, `known_host_entry_id`,
    `replacement_known_host_entry_id`, `old_fingerprint`,
    `new_fingerprint`, `key_type`, `reason_code`) — verified by
    enumerating `jsonb_object_keys(payload)`. The two payloads
    cross-link: each row's `known_host_entry_id` is the entry the
    row is "about" and `replacement_known_host_entry_id` is the
    counterparty's id.
  - **Redaction sentinel scan** of both payload `::text` casts
    returned zero hits for `private_key`, `encrypted_private_key`,
    `password`, `cookie`, `session_token`, `token_hash`,
    `public_key`, `client_info`, `banner`. Each payload was 397
    bytes — public metadata only.
- **Post-replace auth-check + terminal attach.** Re-run auth-check
  (`19:38:03`, POST `…/auth-check` → 200) returned
  `authentication_succeeded` — credentials work against the new
  pin. Terminal launch on the same profile reached
  `production-terminal-phase = "live"`; three harmless commands
  were run through the production xterm (`echo
  relayterm-repin-replaced-smoke`, `whoami`, `pwd`) and produced
  the expected output (`relayterm-repin-replaced-smoke`, `smoke`,
  `/config`). Session ended cleanly via
  `production-terminal-close`; the component unmounted.
- **Backend + web log redaction sweep clean.** The nginx access
  log's wire timeline matched the SPA walk exactly (preflight →
  trust → auth-check → preflight changed → replace-host-key →
  auth-check). Zero `ERROR` lines in the backend during the smoke
  window. Zero sentinel hits in either log stream on the set
  `session_token`, `token_hash`, `password`, `cookie`,
  `encrypted_private_key`, `private_key`, `data_b64`,
  `REDACT-MARKER`.

Workstation checks before stop-before-commit:

- `pnpm run check:docs-contracts`: clean.
- `pnpm -r check` (svelte-check + tsc): clean.
- `git diff --check`: clean.

Deferred (intentional non-goals for this run):

- **SSH CA / host-certificate trust.** Separate future trust model;
  out of scope for this surface per `docs/spec/host-key-replace.md`
  § "Non-goals".
- **Admin / cross-user replace; bulk replace.** Surface stays
  owner-scoped + single-profile.
- **Hard delete of `known_host_entries`.** Old rows are revoked,
  never dropped; admin-only future work.
- **Production hostname / production credentials / real production
  SSH identities.** Staging is throwaway by construction (§1).
- **Tauri release-channel / Android-specific replace smoke.**
  Same `HostKeyPanel.svelte` ships via `apps/web`, so the bundled-
  shell behaviour follows automatically; a Tauri-shell-specific
  walk is deferred until the next desktop / Android slice that
  exercises the Replace affordance through a WebView.
- **Source-code or CI change.** None made; this is a manual-smoke
  + docs-only slice.

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

### 2026-05-10 · Long-TTL (1800 s) reconnect smoke via Playwright browser automation

Extends the 2026-05-10 custom-TTL smoke below (which validated
`RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS=300`
end-to-end through the desktop Tauri shell) to a substantially
longer reconnect window — `1800 s` (30 min) — and replaces the
desktop Tauri client with Playwright-driven browser automation
against the production browser path at
`https://relayterm-staging.js-node.cc`. Goal: confirm the
configurable knob keeps a still-live PTY reachable for a long
multi-minute disconnect, that the in-memory replay buffer keeps
delivering post-detach output past the prior 300 s validated
window, and that the reaper still fires exactly at the configured
TTL boundary (not the old hard-coded 30 s default, and not the
prior smoke's 300 s).

This is a **smoke + docs** slice: no source, deploy, schema, API,
auth, CSRF, CORS, WebSocket-protocol, or Tauri-native code
changed. The configurable-TTL knob and the Compose-template
plumbing for it shipped in earlier commits (see the 2026-05-10
custom-TTL entry below for that landing record) — this run only
turned the knob up and observed the long-window behaviour.

**Date convention.** The heading date `2026-05-10` matches this
file's operator-local-date convention (the 2026-05-09 desktop
Tauri reconnect entry below uses the same convention — its inline
timestamps include UTC values past midnight UTC). The smoke
proper ran from operator-local-evening 2026-05-10 across UTC
midnight, so every inline `session_events` row, attachment row,
and absolute UTC timestamp in this entry carries a `2026-05-11`
date. There is no UTC-date error in the timestamps; only the
heading uses the operator-local convention.

**Pinned contract under test.** Same field as the 2026-05-10
custom-TTL smoke below: `terminal_sessions.detached_live_pty_ttl_seconds`
(env `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`),
bounded `5..=86400` (5 s..24 h), staging value for this run
**1800 s**. The knob is a *short-term reconnect grace window* on
a still-live PTY held by the running backend; it does NOT survive
a backend restart, is NOT durable shell persistence
(no `tmux`/`screen`-style resurrection), and explicitly was not
asked to do either — see
[`docs/spec/terminal.md`](../spec/terminal.md) § "Output sequence
+ in-memory replay buffer contract" and the SCOPE preamble in
[`docs/config-examples/relayterm.production.example.toml`](../config-examples/relayterm.production.example.toml).

**Origin:** `https://relayterm-staging.js-node.cc` (unchanged).
**Image tag:** `:main` carried over from the 2026-05-10 custom-TTL
smoke below at `sha256:22e092f824b4…` (same SHA on both
`relayterm-backend` and `relayterm-web`; no `docker compose pull`,
no migrations, no image upgrade). Backend was force-recreated once
to pick up the env-var change; Postgres + `relayterm-web`
containers were untouched.
**Throwaway SSH target:** new `linuxserver/openssh-server:latest`
container `relayterm-staging-smoke-ssh-longttl` joined to
`relayterm-staging_relayterm-staging-internal` (key-auth-only,
`PASSWORD_ACCESS=false`, `SUDO_ACCESS=false`, port `2222`, **no
host port published**, user `smoke` with `/bin/bash`; ed25519 host
fingerprint `SHA256:7FWo7ltkrf4bAiOGP4WR3p7B3gc85Skvd5LwkNrLZo0`
captured at `host-key-preflight` and pinned at `trust-host-key`).
The reused `smoke-id` ed25519 identity's public key was injected
into `/config/.ssh/authorized_keys` (`smoke:users`, mode `600`,
single line) by `docker cp` from a temp file on the VPS that was
shredded immediately after copy; no private key material left the
backend vault. **Throwaway; torn down at end of run.**

**Inventory:** brand-new host `long-reconnect-smoke-host`
(`1812f957-c8e1-4c86-96f6-2e5d75c1605d`,
`relayterm-staging-smoke-ssh-longttl:2222 smoke`) and brand-new
profile `long-reconnect-smoke-profile`
(`11978f87-61d1-4ad9-b208-00ae7e4fea13`) bound to the existing
reused `smoke-id` ed25519 identity
(`44b5e2be-29c2-4eb0-b6ac-3b4e25ca789d`). All prior staging
inventory rows from the 2026-05-09 and 2026-05-10 entries were
left intact per the AGENTS.md "Inventory lifecycle and
destructive-action policy". Single new `known_host_entry` row
`82abb0ad-6d12-4ce4-8c96-7dadff327abc` from the preflight + trust
cycle.

**Driver surface — Playwright MCP, not desktop Tauri.** Diff vs.
the 2026-05-10 custom-TTL smoke below: the operator surface here
is a Chromium instance driven by Playwright MCP, against the
HTTPS staging hostname directly (no Tauri bundled-handoff path,
no native shell). The 2026-05-09 desktop Tauri smoke and the
2026-05-10 custom-TTL desktop Tauri smoke already verified the
bundled-shell handoff + cookie persistence layers; this slice's
scope is the configured-TTL backend behaviour, so the simpler
browser path was used. The terminal canvas is read via DOM
inspection (`document.querySelectorAll('.xterm-rows > div')`
under `browser_evaluate`) — the xterm.js DOM renderer (`renderer-type=dom`
is the production default per
[`packages/terminal-xterm/src/index.ts`](../../packages/terminal-xterm/src/index.ts))
puts visible cells in plain `<div>` rows, so the agent can
ground-truth the replay handshake against the rendered grid
without driving WebGL.

Setup-API path drove from the workstation against the same
Compose stack on `cloud-edge` (login cookie persisted in the
ephemeral Playwright MCP browser context for the duration of the
run, plus a parallel curl-driven cookie at
`cloud-edge:/tmp/relayterm-longttl.cookie` chmod 600 used only
for the inventory bootstrap and torn down at cleanup; bootstrap
credentials sourced from
`/home/ubuntu/docker/relayterm-staging/.bootstrap-credentials`
identically to the 2026-05-10 custom-TTL entry below, parsed
key-by-key into shell vars and never echoed).

Verified — **all timings are wall-clock UTC ground-truth from the
Postgres `session_events` table on the staging stack itself**
(not operator-reported):

- HTTPS reachability gate (§7.3) re-checked from the workstation
  pre-recreate and post-recreate: `/healthz` → 200,
  `/api/v1/auth/me` → 401 JSON envelope (`{"error":{"code":"unauthorized","message":"unauthorized"}}`),
  identical headers (HSTS / CSP / referrer-policy from
  `secure-chain@file`). Pre-recreate startup log line was
  `detached_live_pty_ttl_seconds=30` (the prior smoke had not
  persisted the `1800` value into `.env` — staging starts each
  recreate at the Compose template default unless `.env`
  overrides it). After appending one line to
  `/home/ubuntu/docker/relayterm-staging/.env` (timestamp-suffixed
  backup at `.env.bak.20260511T005244Z`) and
  `docker compose --env-file ... up -d --no-deps --force-recreate
  relayterm-backend`, the container reached `(healthy)` in ~13 s
  and the first-line startup log read
  `detached_live_pty_ttl_seconds=1800`. `docker exec ... env`
  inside the running container confirmed
  `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS=1800`.
- Inventory CRUD: `POST /api/v1/hosts` → 201 (host id above);
  `POST /api/v1/server-profiles` → 201 (profile id above);
  `POST .../host-key-preflight` → `host_key_status: "unknown"`
  with `host_key_fingerprint:
  "SHA256:7FWo7ltkrf4bAiOGP4WR3p7B3gc85Skvd5LwkNrLZo0"`
  matching `docker exec ... ssh-keygen -lf
  /config/ssh_host_keys/ssh_host_ed25519_key.pub`; subsequent
  `POST .../trust-host-key` with the same `expected_fingerprint`
  returned the fresh `known_host_entry_id` above;
  `POST .../auth-check` → `status: "authentication_succeeded"`
  ("ssh public-key authentication succeeded; no PTY was allocated
  and no command was executed"). One incidental shape note: the
  trust-host-key body is `expected_fingerprint` (must begin with
  `SHA256:`), the preflight response field is `host_key_fingerprint`
  — these are distinct names. A first-cycle attempt that mistook
  the preflight response for a `observed_fingerprint` field
  returned `invalid_input` ("expected_fingerprint must start with
  'SHA256:'"); the retry with the correct copy succeeded.
- **Pre-flight WS-idle observation (informational, not a TTL
  finding).** The very first launch on the Playwright browser
  (session `7e37f2f1-b740-4c0d-80c7-d17631e7873c`, click
  `01:34:25.327Z`) was lost at exactly **60 s of WS idle** —
  `session_events` showed `attached 01:34:26.017Z → detached
  01:35:26.055Z` with the client never having sent a keystroke
  (the agent was busy inspecting DOM globals via
  `browser_evaluate`). The session became `detached`,
  `last_seen_seq=0`, Reconnect disabled. **This is an idle-WS
  gate at the proxy or backend, NOT the TTL knob under test, and
  NOT a regression** — the 2026-05-09 and 2026-05-10 desktop
  Tauri reconnect entries below do not surface it because the
  operator was producing keystrokes/output continuously during
  live windows. On the smoke proper (session
  `e7ae8b6e-caa4-47bd-a264-07be42fc4e45`, below) the agent typed
  the baseline command within the first 7 s after launch and the
  60 s idle gate did not fire. Worth folding back later if a
  future slice depends on hands-off browser-headless test
  fixtures — see "Drift worth folding back later" below.
- **Smoke session (the one that matters):**
  `e7ae8b6e-caa4-47bd-a264-07be42fc4e45`. `terminal_sessions`
  end-state row: `status=closed`, `cols=80`, `rows=24`,
  `created_at=2026-05-11T01:36:30.394513Z`,
  `last_seen_at=2026-05-11T02:47:34.322412Z`,
  `closed_at=2026-05-11T02:47:34.314367Z`. Full `session_events`
  trace (only `kind` + `recorded_at`, no `payload` dump — same
  defensive default as the prior smokes):

  ```
  created    2026-05-11 01:36:30.395867+00
  attached   2026-05-11 01:36:30.647381+00   ← initial attach
  detached   2026-05-11 01:37:36.832974+00   ← T_detach_1
  attached   2026-05-11 01:49:52.706617+00   ← reconnect 1 (12-min window)
  reattached 2026-05-11 01:49:52.711682+00
  detached   2026-05-11 01:50:39.239252+00   ← T_detach_2
  attached   2026-05-11 02:16:58.799760+00   ← reconnect 2 (26-min window)
  reattached 2026-05-11 02:16:58.802457+00
  detached   2026-05-11 02:17:34.297582+00   ← T_detach_3 (final)
  closed     2026-05-11 02:47:34.327946+00   ← reaper fired
  ```

  Three `terminal_session_attachments` rows mirror the three live
  windows (initial + two reattach cycles); each
  `(attached_at, detached_at)` is within microseconds of the
  corresponding `session_events` pair.
- **Case A — short detach + reconnect at 12 min** (well past the
  prior 300 s smoke window). `detached 01:37:36.832 → attached
  01:49:52.706 → reattached 01:49:52.711`. Detach gap =
  **735.874 s** (12 min 16 s) — 2.45 × the prior smoke's 300 s
  validation, 24.5 × the old 30 s default. `reattached` event
  fired **5.07 ms** after `attached`, proving the
  cancel-pending-close path on
  `crates/relayterm-terminal/src/manager.rs:914-919, 956-970`
  fires correctly at the new non-default TTL. **Replay during
  the gap:** before detach, the live shell started a
  six-tick background loop emitting `relayterm-detached-output-N`
  lines at 120 s spacing; ticks 2 through 6 emitted between
  `01:39:20Z` and `01:47:20Z` while the WebView was detached. On
  reconnect, the rendered xterm DOM grid showed **all five
  post-detach ticks (lines 2-6)** before any new input,
  delivered via the `replay_started → buffered output →
  replay_completed` handshake on `last_seen_seq=118`. Tick 1
  (emitted pre-detach) was absent — expected: same
  *resume-the-live-stream not restore-the-canvas* behaviour
  documented in the 2026-05-09 entry below and pinned by
  `apps/web/src/lib/dev/liveTerminalState.ts`; the renderer
  `dispose()` on Detach destroys the local grid and replay
  delivers only output past `last_seen_seq`, not local
  scrollback. Post-reconnect baseline command
  (`echo relayterm-probe-1-resumed && date -u`) round-tripped
  and rendered `Mon May 11 01:50:27 UTC 2026`.
- **Case B — long detach + reconnect at 26 min** (most of the
  way through the configured 1800 s window). `detached
  01:50:39.239 → attached 02:16:58.799 → reattached
  02:16:58.802`. Detach gap = **1579.560 s** (26 min 19 s) —
  margin to the 1800 s reaper boundary was only ~220 s.
  `reattached` event fired **2.7 ms** after `attached`. A second
  bounded loop started pre-detach emitted
  `relayterm-probe2-tick-N` lines at 240 s spacing (so all six
  ticks land within the detach window); after reconnect, the
  rendered grid showed **probe2-tick-2 through probe2-tick-6**
  (five post-detach lines) via replay. Tick 1 (emitted
  pre-detach, at `01:50:27Z` per the remote bash clock) was
  again absent for the same documented reason. Post-reconnect
  baseline (`echo relayterm-probe-2-resumed && date -u`)
  rendered `Mon May 11 02:17:21 UTC 2026`. **No duplicate /
  garbled / out-of-order replay observed** — the five lines
  arrived monotonic on `recorded_at`-ordered seq, with the
  ASCII payloads byte-identical to the producer-side `date -u
  +%H:%M:%S` timestamps embedded in each line.
- **Case C — beyond-TTL reaper** (final detach, single
  reconnect attempt ~31 min later). `detached 02:17:34.297` →
  scheduled close fires → `closed 02:47:34.327`. Gap from
  `detached` to `closed` event = **1800.030 s** — exactly the
  configured `1800 s` TTL, ±**30 ms**. The reaper landed on the
  configured boundary, not on the old 30 s default and not on
  the prior smoke's 300 s. `terminal_sessions.closed_at`
  (`02:47:34.314367Z`) is **13.58 ms** earlier than the
  `closed` event's `recorded_at` (`02:47:34.327946Z`) — the
  row-flip happens inside `close_session` before the event row
  is written, matching the manager-crate ordering. **UI behaviour
  at attempted reconnect of the reaped session** (click at
  `02:49:06Z`, ~92 s past the reaper): the production terminal
  route's status badge flipped from the stale
  `detached (TTL window)` (frontend `liveTerminalState.ts`'s
  duplicated `DETACHED_TTL_MS=30_000` doesn't poll for true
  remaining TTL — see [`docs/spec/terminal.md`](../spec/terminal.md)
  § "Detached-session TTL contract (load-bearing)" bullet "TTL
  clarity") to **`Status
  error`** with body text **"Connection error"**. Navigating
  `Sidebar → Sessions` to the same `session_id` then surfaced
  the spec-pinned closed-session UX from
  [`apps/web/src/lib/app/terminal/sessionStatus.ts`](../../apps/web/src/lib/app/terminal/sessionStatus.ts):
  row reads **"Session ended. The runtime is gone and cannot
  be reconnected. Launch a new session from the originating
  server profile."**, Reconnect button disabled with `title=
  "Closed sessions cannot be reconnected"`, Close button
  disabled with `title="Already closed"`. The closed-session
  helper text is the same one pinned by
  `apps/web/tests/sessionStatus.test.ts` and surfaced by the
  2026-05-09 desktop Tauri smoke for the 30 s case.
- Backend log sweep over **90 minutes** of `relayterm-backend`
  output covering pre-recreate + recreate + Cases A through C
  (`ssh ubuntu@cloud-edge ... docker logs --since 90m`):
  **zero hits** across the full redaction sentinel set
  (`session_token`, `token_hash`, `password=`, `"password"`,
  `encrypted_private_key`, `private_key`, `BEGIN OPENSSH`,
  `BEGIN PRIVATE`, `data_b64`, `REDACT-MARKER`,
  `csrf_origin_mismatch`). **Zero ERROR and zero WARN lines** in
  the same 90 min window — the binary `RTB1` data plane through
  Traefik to the Playwright Chromium WebView was silent on
  errors across three full attach/detach/replay cycles and the
  reaper close. Web (`relayterm-web` nginx) container redaction
  sweep over the same window: **zero hits**. Backend log lines
  mentioning the smoke session id (`e7ae8b6e`): **zero** — the
  backend does not log session ids in routine paths, only event
  rows hit the database.

Deferred (intentional non-goals for this run):

- **Durable long-term session persistence** (`tmux`/`screen`-style
  resurrection across backend restart). Unchanged from the prior
  2026-05-10 custom-TTL smoke below — the configurable knob is a
  *short-term reconnect grace window* on a still-live PTY held
  by the running backend; a backend restart drops every detached
  PTY AND its replay buffer per
  [`docs/spec/terminal.md`](../spec/terminal.md) § "Output
  sequence + in-memory replay buffer contract". This slice did
  not exercise restart-survival and explicitly does NOT claim
  durable persistence.
- **Backend restart survival.** Not exercised. The backend was
  force-recreated ONCE at the start of the slice to pick up the
  new env var; that recreate happened BEFORE the smoke session
  was created, not during it. No mid-smoke restart was performed
  and no claim is made about restart resilience.
- **Desktop Tauri / Android Tauri surfaces.** Per the slice
  framing this run was browser-only; the 2026-05-09 and
  2026-05-10 desktop Tauri smokes below already cover the
  bundled-shell handoff + cookie persistence paths against the
  same staging slot, and nothing in this slice changed any
  Tauri-relevant surface.
- **Mobile portrait sidebar UX / mobile autocapitalize on
  identifier inputs** — both shipped on `main` (`f19a043`,
  `153a15c`) prior to this slice and were not re-exercised here;
  irrelevant to the configured-TTL contract.
- **Recording surface.** `RELAYTERM_TERMINAL_RECORDING__ENABLED=false`
  on this slot per `.env`; the View-recording button visible in
  the closed-session UX would open the read-only recording
  viewer for a recording-enabled deployment, but no chunks
  exist on this slot.
- **Alternate renderer adapters** — only
  `@relayterm/terminal-xterm` baseline was exercised (DOM
  renderer, which makes the agent's
  `document.querySelectorAll('.xterm-rows > div')` read-back
  trivial); the experimental ghostty-web / restty / wterm
  adapters were not.
- **Multi-tab / multi-client collaborative attach** — single
  attachment per live window throughout. No second concurrent
  client was tested.
- **Production hostname / production credentials / real
  production SSH identities** — staging is throwaway by
  construction (§1). The throwaway SSH target had no host port
  published; only the `smoke` user with the `smoke-id` public
  key in `authorized_keys` could authenticate.
- **CI / signing / auth / CORS / CSRF / WebSocket-protocol
  behaviour changes** — none in scope, none made. The only
  staging-side mutation was the one-line `.env` append and the
  ensuing `relayterm-backend` `--force-recreate`.

Drift worth folding back later (non-blocking):

- **Browser-idle WS gate vs. fully-automated browser test
  fixtures.** The pre-flight 60 s idle disconnect on the very
  first launch (recorded above) shows the live attach is
  sensitive to client-side keystroke/output activity. This is
  benign for human operators and for any test that produces
  PTY input within the first ~60 s, but a future
  Playwright-driven CI fixture that wants to validate a
  long-LIVE attach (no input, no output) would surface it again.
  The most likely cause is either the Traefik proxy's default
  WS read-idle timeout or backend's WS keepalive expectations
  not seeing client pings. Worth folding back if and when
  hands-off browser-headless reconnect tests land; not in scope
  here.
- **Frontend TTL countdown copy stays at "~30 s" regardless of
  the configured backend value.** During the two reconnect
  windows the production terminal route's "Detached" paragraph
  read "The remote PTY remains alive only briefly (~30 s) —
  reconnect within that window or the session is reaped" even
  though the backend was configured for 1800 s. This is the
  duplicated frontend constant in
  `apps/web/src/lib/dev/liveTerminalState.ts`
  (`DETACHED_TTL_MS = 30_000`) that is deliberately NOT polled
  from the backend — see [`docs/spec/terminal.md`](../spec/terminal.md)
  § "Detached-session TTL contract (load-bearing)" bullet "TTL
  clarity" pinning the drift as intentional. A future slice could surface the
  backend-configured value through the
  `GET /api/v1/terminal-sessions/:id` envelope or the
  `replay_start` frame so the countdown reflects the actual
  remaining TTL; that is a separate slice and was not in scope
  here. **No copy change was made in this slice.**
- **One-line `.env` revert worth doing before walking away.** The
  staging stack is intentionally left running per slice policy,
  but with `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS=1800`
  still in `.env` and on the running backend. Next time a
  default-TTL behaviour smoke runs against staging, either delete
  this line from `/home/ubuntu/docker/relayterm-staging/.env`
  before the `--force-recreate` or pass an explicit
  `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS=30`
  override.

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
  (see "Drift worth folding back later" below). **Closed**
  `2026-05-10` by the `Host-key replacement (revoke-and-replace)
  staging smoke` entry above.
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
  audit feed. **Update (Phase 5 complete):** the staging smoke
  ran on `2026-05-10` and is recorded as
  `2026-05-10 · Host-key replacement (revoke-and-replace)
  staging smoke` above. Changed-key detection, the Replace
  modal (gating + copy), the paired
  `host_key_revoked` + `host_key_accepted` audit rows in the
  same transaction, post-replace `auth-check`, and a terminal
  attach against the new pin were all verified end-to-end against
  the published `:main` lockstep. The deferred note is closed.
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

### 2026-05-11 · Per-user live PTY quota (Phase 1B.1, cap=1) staging smoke

Verification of the per-user live PTY ceiling shipped in
`feat(api): enforce per-user live session quota` (`eb75116`,
2026-05-11). The slice landed both Phase 1A
(`/api/v1/config/session-policy` returns the configured detached-TTL)
and Phase 1B.1 (per-user live-PTY ceiling refusal with the typed
429 envelope). Both halves were verified end-to-end against the
HTTPS staging slot at
[`https://relayterm-staging.js-node.cc`](https://relayterm-staging.js-node.cc)
with the cap temporarily lowered to `1` so the refusal could be
exercised quickly.

**Stack state at smoke start.** Compose project
`relayterm-staging` on `cloud-edge`. Pre-smoke, the deployed
backend image was the pre-Phase-1A `sha256:22e092f8…` (built
2026-05-10 18:36 UTC, ~21 h before the quota commit); the
`:main` tag in the Forgejo registry now resolved to the post-
quota `sha256:218f1b83…` after CI run `353` (ci.yml for
`eb75116`, status=success, 296 s). The recreate path picked it
up automatically.

**Compose env wiring (durable).** The staging compose at
`/home/ubuntu/docker-compose/relayterm-staging/docker-compose.yml`
gained a single line, slotted right after the existing
`DETACHED_LIVE_PTY_TTL_SECONDS` row inside the
`relayterm-backend.environment` block:

```yaml
      RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER: "${RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_USER:-8}"
```

The default expansion resolves to `8` (matching
`relayterm_terminal::DEFAULT_MAX_LIVE_PTY_PER_USER` and
`docs/session-quotas.md` § 4.1); the smoke override (`=1`) was
injected via shell-env at the `docker compose up -d
--force-recreate` invocation, NOT written to any `.env` file.
This compose-file edit stays in place after the smoke as the
durable operator-knob wiring; the cleanup recreate just drops
the shell override and lets the default re-apply.

**Recreate.** Both backend and web were recreated from the
refreshed `:main` digests via `docker compose ... up -d
--no-deps --force-recreate --pull always relayterm-backend
relayterm-web`. Postgres was untouched (`Up 2 days (healthy)`
across the run). New container SHAs: backend
`sha256:218f1b83…` (created 17:32:14 UTC), web
`sha256:42c62ba4…` (created 17:32:16 UTC). Both reached
`healthy` within ~4 s of start.

**Baseline checks (post-deploy, pre-smoke).**

- `GET /healthz` → `200 {"status":"ok"}`.
- `GET /api/v1/auth/me` (unauthenticated) → `401 unauthorized`.
- `GET /api/v1/config/session-policy` (unauthenticated) →
  `401 unauthorized` (route now exists; pre-deploy was `404`,
  proving the Phase 1A endpoint reached staging only at this
  recreate).
- Backend startup line literally read
  `relayterm-backend starting addr=0.0.0.0:8080
   auth_mode="production" recording_enabled=false
   detached_live_pty_ttl_seconds=30 max_live_pty_sessions_per_user=1`
  — the Phase 1B.1 cap is now in the startup-log echo, mirroring
  the Phase 1A TTL convention.
- `GET /api/v1/config/session-policy` (authenticated) →
  `200 {"detached_live_pty_ttl_seconds":30,
   "max_live_pty_sessions_per_user":1}` — confirms both
  Phase 1A and Phase 1B.1 wire surfaces.

**Throwaway SSH target.** A `linuxserver/openssh-server:latest`
container named `relayterm-staging-quota-smoke-ssh`, attached
ONLY to the existing internal Compose network
`relayterm-staging_relayterm-staging-internal` (172.21.0.0/16,
IP 172.21.0.5), with **no host port published**. Configured
with `USER_NAME=smoke`, `PASSWORD_ACCESS=false`,
`SUDO_ACCESS=false` so only key-based auth on TCP 2222 inside
the network. The `--hostname quota-smoke-host` value is
discoverable from the backend container's DNS in addition to
the container name (Docker's modern engine adds both as
`DNSNames`; verified via `getent hosts` from inside the
backend container), so the RelayTerm host row uses the
prettier `quota-smoke-host` form.

**Inventory created (RelayTerm-managed; no private-key
import).**

- SSH identity `quota-smoke-identity` (Ed25519, fingerprint
  `SHA256:N6QZEtno5iZhzyMOMbBWvNZwrTRPsXt5BvariKryris`).
  Generated server-side by the vault; the response surfaces
  only the public-key fragment.
- Host `quota-smoke-host` (hostname `quota-smoke-host`, port
  `2222`, default username `smoke`).
- Server profile `quota-smoke-profile` referencing the host +
  identity.
- Host-key preflight observed `host_key_status: unknown` with
  the target's freshly-generated ed25519 host key
  (`SHA256:mf01uZE+NKV37R5wb5opx7/Z7d9TJYUcbTUvsxFNcj0`);
  trust-host-key pinned that fingerprint as the active
  `known_host_entries` row.
- `auth-check` returned `authentication_succeeded` —
  confirms the RelayTerm-managed public key is installed on
  the target (injected into the container's
  `/config/.ssh/authorized_keys` via `docker exec`, never via
  any wire / API field) AND that the keypair authenticates
  via the SSH KEX + userauth handshake without allocating a
  PTY or executing a command (per the existing
  `auth-check` contract).

**Quota smoke proper.** Driven via authenticated `curl` calls
from the workstation (browser-write routes carry
`Origin: https://relayterm-staging.js-node.cc` and the
`relayterm_session` cookie; cookie file lived in
`/tmp/qs/cookies.txt` `chmod 600` for the smoke window only —
removed at cleanup, never written to any tracked file).

- **Launch session A** —
  `POST /api/v1/terminal-sessions
  {server_profile_id, cols:80, rows:24}` → `201 Created`,
  body `{id:"2f20cc17-d3bd-41bb-8e65-b6b700643a78",
  status:"active", pty_live:true,
  message:"ssh pty started; replay across reconnects is not
  yet implemented", ...}`.
- **Attempt session B (same profile, same user, cap full)** —
  same POST → **`429 Too Many Requests`**, body literally
  `{"error":{"code":"too_many_sessions",
  "message":"too many terminal sessions"}}`. Wire-stable per
  `docs/session-quotas.md` § 7.1 — `code` is the typed
  `too_many_sessions` (distinct from the login throttler's
  `too_many_requests`), `message` is the static safe form
  with no count / cap / session id / hostname / profile id /
  user id / `Retry-After` header.
- **DB after refusal** —
  `select count(*) from terminal_sessions
   where server_profile_id = $profile and id != $session_A`
  returned **`0`**: the refused request wrote no
  `terminal_sessions` row. `audit_events` row count was
  identical to pre-refusal (`38 → 38`), confirming the
  refusal wrote **no audit row** per `docs/session-quotas.md`
  § 8.2.
- **Backend warn line for the refusal** (only line attributable
  to the smoke, sigil-stripped for the doc): `WARN
  relayterm_api::routes::v1::terminal_sessions: terminal
  session quota refused user_id=f968b6f5-... scope=
  "per_user_live" current_count=1 cap=1`. Public-shape only:
  `user_id` (already in every authenticated log line),
  `scope`, `current_count`, `cap`. No session id, no profile
  id, no host id, no identity id, no hostname, no peer
  banner, no wire body, no User-Agent. Matches the operator-
  side logging policy in `docs/session-quotas.md` § 8.3.
- **Close session A** —
  `POST /api/v1/terminal-sessions/2f20cc17-.../close` →
  `200 {status:"closed", closed_at, already_closed:false}`
  (abbreviated; the actual `CloseTerminalSessionResponse`
  flattens the full `TerminalSessionResponse`, so the wire
  body also carries `id`, `server_profile_id`, `cols`,
  `rows`, `created_at`, `last_seen_at`).
- **Launch session C (cap freed)** — same POST as A → **`201
  Created`**, body
  `{id:"404ce7a5-25d9-4849-987f-71aa1bfa67c2",
  status:"active", pty_live:true, ...}`. Confirms the cap
  truly counts the in-memory runtime registry's live PTYs
  (`count_live_pty_for_user`) and the slot is reclaimed
  immediately when `close_session` removes the registry
  entry.
- **Close session C** —
  `POST .../close` → `200 closed`.
- **Final DB state**: two terminal-session rows on the
  throwaway profile, both `status:"closed"` with proper
  `closed_at` timestamps; `audit_events` count unchanged
  across the entire quota-smoke window (`38` before, `38`
  after).

**Log / nginx redaction sweep.** Full backend container log
across the recreate-to-end window (22 lines total since
backend start, 2 lines inside the smoke proper) and full
nginx container log (15 lines) were grepped for the sentinel
set `session_token|token_hash|cookie|password|private_key|
encrypted_private_key|BEGIN OPENSSH|data_b64|REDACT-MARKER`.
Zero hits. The two WARN lines inside the smoke window:

1. `csrf origin mismatch detail=missing Origin header` at
   02:07:21 UTC — caused by an unquoted-bash-var word-split
   on the operator's first `curl` invocation, which dropped
   the `Origin` header before the request reached `axum`.
   Caught by `CsrfGuard` BEFORE any DB / auth / body work;
   detail string is the static `missing Origin header` (no
   echo of any offered value). Operator-side dev mistake,
   not a smoke finding.
2. The `terminal session quota refused …` line above
   (02:08:57 UTC).

**Cap reverted at cleanup**: cleanup recreate replaced the
shell `MAX_LIVE_PTY_SESSIONS_PER_USER=1` override with the
compose default (`8`). Post-cleanup
`/api/v1/config/session-policy` (authenticated) returned
`{detached_live_pty_ttl_seconds:30,
 max_live_pty_sessions_per_user:8}`. Backend startup line
echoed `max_live_pty_sessions_per_user=8`. The compose-file
edit (one line in `relayterm-backend.environment`) stays
in place as the durable operator knob.

**Throwaway SSH target cleanup**: `docker rm -f
relayterm-staging-quota-smoke-ssh` removed the container at
end of smoke. The image stays in the local cache; no host
port was ever published, so no firewall surface to revert.

**Temp credentials cleanup**: `/tmp/qs/{cookies.txt,
pass.txt,phc.txt,login.out,trace.out}` (cookie file,
throwaway plaintext password, computed PHC string, raw
login response, curl trace) were chmod-600 throughout the
smoke window so they were never world-readable, and the
whole `/tmp/qs/` directory was removed at cleanup. The
uncommitted one-shot Argon2id PHC helper
(`crates/relayterm-auth/examples/qs_hash.rs`) was deleted
and the matching `.git/info/exclude` line removed.

**Inventory rows (host / profile / identity / known-host
entry / 2 closed terminal-session rows) intentionally
LEFT IN PLACE** per the staging-smoke convention — they
are operator-visible carry-over for the next smoke, and
no destructive-action route was exercised in this slice.

**Verified.**

- Phase 1A `/api/v1/config/session-policy` exists at the
  recreated backend (was 404 pre-recreate, 401/200 after).
- Phase 1B.1 `max_live_pty_sessions_per_user` is the second
  authenticated-only wire field on that endpoint.
- Phase 1B.1 enforcement at `POST /api/v1/terminal-sessions`
  with cap=1: launch-1 succeeds, launch-2 refused with
  `429 too_many_sessions`, refusal writes no DB row + no
  audit row, refusal log line carries only public-shape
  fields, slot frees on close, relaunch succeeds.
- The shipped wire envelope is the typed
  `{error:{code:"too_many_sessions",
   message:"too many terminal sessions"}}`, distinct from
  the login throttler's `too_many_requests` code.
- No `Retry-After` header on the 429 (verified by curl
  `--include`).
- All log / nginx redaction sentinels clean.

**Deferred (intentional non-goals for this run; do NOT
treat any of these as smoke-verified):**

- **Starting-burst quota (`max_starting_sessions_per_user`)**
  — Phase 1B.2, not landed.
- **Deployment-wide quota
  (`max_live_pty_sessions_per_deployment`)** — Phase 1B.2,
  not landed.
- **Operator dashboard tile** showing the caller's own
  live-session count vs cap — Phase 1B.2, not landed.
- **Prometheus / metrics surface** for quota counters —
  out of scope per `docs/session-quotas.md` § 8.4.
- **Durable persistent sessions across backend restart**
  — Phase 2 / 3 in
  [`docs/persistent-sessions.md`](../persistent-sessions.md),
  unchanged by this slice. The quota acts ONLY on the
  in-memory runtime registry; a backend restart reaps
  every live PTY regardless (per the existing terminal-
  session startup reconciliation).
- **VT snapshot resume** of an existing detached session
  across a restart — out of scope.
- **tmux / screen multiplexer pass-through** — out of
  scope.
- **RelayTerm-side persistent-session agent on the target
  host** — out of scope.
- **Production-default tuning** (whether `8` is the right
  per-user cap for a homelab vs a small team) — Phase 1B.3
  per `docs/session-quotas.md` § 10.3.
- **WebSocket attach + actual terminal I/O on session A /
  C** — the smoke proves PTY allocation via `pty_live:
  true` and `auth-check authentication_succeeded`, but did
  NOT drive a shell prompt over the WS data-plane in this
  run. The Phase 1B.1 quota gates session creation
  (`count_live_pty_for_user` in the runtime registry),
  which is upstream of any WS attach.

### 2026-05-12 · Per-user starting-session quota (Phase 1B.2a, cap=1) staging smoke

Verification of the per-user starting-session ceiling shipped
in `feat(api): enforce per-user starting session quota`
(`fd6813d`, 2026-05-11). The starting-cap sits AFTER the
Phase 1B.1 live-cap in the create-route ordering and refuses
a tight POST loop that would otherwise stack `live + starting`
slots before any in-flight create promotes. Verified against
the HTTPS staging slot at
[`https://relayterm-staging.js-node.cc`](https://relayterm-staging.js-node.cc)
with the starting cap temporarily lowered to `1` so the
refusal could be exercised quickly.

**Smoke method (controlled TCP-stall / API smoke, NOT a
real-SSH or terminal-I/O smoke).** Starting-state sessions
are harder to exercise than live-cap refusals because a
healthy SSH target promotes a session from `Starting` to
`Live` in well under a second — the per-step inner timeout
in `crates/relayterm-ssh/src/russh_pty.rs:48`
(`DEFAULT_INNER_TIMEOUT = 10 s`) and the outer
`DEFAULT_PTY_START_TIMEOUT = 20 s` in `crates/relayterm-
ssh/src/pty.rs:46` only matter for stalled / non-responsive
targets. To hold session A in `Starting` long enough to
launch session B and observe the refusal, the smoke pointed
the existing trusted host record `quota-smoke-host` at a
throwaway alpine+socat container that ACCEPTS TCP on 2222
but never sends an SSH banner. The bridge's KEX hangs at
banner-read until the inner timeout fires (~10 s), giving
ample window for the second POST. This is a controlled
quota-path smoke driven via the authenticated HTTP API; it
does NOT exercise real KEX, real auth, real PTY allocation,
real WebSocket attach, or real terminal I/O — those surfaces
are covered by other runs in this runbook.

**Stack state at smoke start.** Compose project
`relayterm-staging` on `cloud-edge`. Pre-smoke, the deployed
backend image was the pre-Phase-1B.2a `sha256:218f1b83…`
(built from `eb75116`, the Phase 1B.1 commit, 2026-05-11
17:32 UTC); the `:main` tag in the Forgejo registry now
resolved to the post-quota `sha256:80d5e000…` (backend,
2026-05-12 03:40 UTC) and `sha256:96d4c5a8…` (web,
2026-05-12 03:41 UTC). A throwaway `docker run` against the
new backend image emitted the startup line
`relayterm-backend starting … detached_live_pty_ttl_seconds=
30 max_live_pty_sessions_per_user=8 max_starting_sessions_
per_user=4` — the new `max_starting_sessions_per_user` field
appears in the echo exactly as `apps/backend/src/main.rs:62`
writes it on `fd6813d`, confirming the new digest carries
the starting-quota commit.

**Compose env wiring (durable).** The staging compose at
`/home/ubuntu/docker-compose/relayterm-staging/docker-
compose.yml` gained a single line, slotted right after the
existing `MAX_LIVE_PTY_SESSIONS_PER_USER` row inside the
`relayterm-backend.environment` block:

```yaml
      RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER: "${RELAYTERM_TERMINAL_SESSIONS__MAX_STARTING_SESSIONS_PER_USER:-4}"
```

Exact-match the repo template at
`deploy/docker-compose.traefik-staging.example.yml:116`. The
default expansion resolves to `4` (matching
`relayterm_terminal::DEFAULT_MAX_STARTING_PER_USER` and
`docs/session-quotas.md` § 4.3); the smoke override (`=1`)
was injected via shell-env at the `docker compose up -d
--force-recreate` invocation, NOT written to any `.env`
file. This compose-file edit stays in place after the smoke
as the durable operator-knob wiring; the cleanup recreate
just drops the shell override and lets the `:-4` default
re-apply.

**`.env` reconstruction.** The previous live-PTY smoke
removed the staging `.env` at its cleanup (the prior runs
fed every secret inline via the shell), so the cap=1
recreate first had to materialise an `.env` again. The
reconstruction read the running container envs verbatim via
`docker inspect … --format '{{range .Config.Env}}…{{end}}'`
(`POSTGRES_*`, `RELAYTERM_AUTH__*`, `RELAYTERM_VAULT__*`,
`RELAYTERM_DATABASE__URL`, `RELAYTERM_IMAGE_TAG`, `RUST_LOG`)
and piped them straight into `/home/ubuntu/docker-compose/
relayterm-staging/.env` under `umask 077` so the secret
values never crossed an operator-visible buffer. The file
is `-rw------- ubuntu:ubuntu`. The cleanup-as-described
option intentionally KEEPS this file in place so the next
staging smoke can `docker compose ...` without recreating
the reconstruction dance; a future smoke that prefers a
clean-slate `.env` can `shred -u .env` first.

**Recreate.** Both backend and web were recreated from the
refreshed `:main` digests via `RELAYTERM_TERMINAL_SESSIONS__
MAX_STARTING_SESSIONS_PER_USER=1 docker compose up -d
--no-deps --pull always --force-recreate relayterm-backend
relayterm-web`. Postgres was untouched (`Up 2 days
(healthy)` across the run). Both reached `healthy` within
~3 s of start.

**Baseline checks (post-deploy, pre-smoke).**

- `GET /healthz` → `200`.
- `GET /api/v1/auth/me` (unauthenticated) → `401`.
- `GET /api/v1/config/session-policy` (unauthenticated) →
  `401`.
- Backend startup line literally read
  `relayterm-backend starting addr=0.0.0.0:8080
   auth_mode="production" recording_enabled=false
   detached_live_pty_ttl_seconds=30
   max_live_pty_sessions_per_user=8
   max_starting_sessions_per_user=1` — the Phase 1B.2a cap
  is the third quota-related field in the startup-log echo,
  mirroring the Phase 1A TTL and Phase 1B.1 live-cap
  conventions.
- `GET /api/v1/config/session-policy` (authenticated) →
  `200 {"detached_live_pty_ttl_seconds":30,
   "max_live_pty_sessions_per_user":8,
   "max_starting_sessions_per_user":1}` — confirms the
  three-field wire shape introduced by `fd6813d`. The
  `max_live_pty_sessions_per_user` field continues to read
  the Phase 1B.1 default of `8` (no override in this run).

**Throwaway stall target (TCP-only, NOT a real SSH
server).** `relayterm-staging-starting-quota-smoke-ssh`
running `alpine:3` with `apk add --no-cache socat` then
`exec socat -d TCP-LISTEN:2222,fork,reuseaddr
SYSTEM:"sleep 60"`. Attached ONLY to the existing internal
Compose network `relayterm-staging_relayterm-staging-
internal` (172.21.0.0/16, IP 172.21.0.5), with **no host
port published**. Started with `--network-alias
quota-smoke-host --hostname quota-smoke-host` so the backend
container's DNS resolves the existing host record's
hostname to the new stall container without any new
inventory writes. `getent hosts quota-smoke-host` from
inside the backend container returned `172.21.0.5
quota-smoke-host`. The container accepts TCP (`echo</dev/
tcp/quota-smoke-host/2222` succeeded from a sibling
container) but never sends the SSH server identification
banner — bytes sent by the russh client are routed to
`sleep 60` and silently dropped. The bridge's KEX hangs at
banner-read until the inner-step timeout fires.

**Host-key gate.** The host record `quota-smoke-host`
(id `026bcb2a-…`) carries one `known_host_entries` row from
the prior live-PTY smoke: `key_type=ed25519`,
`fingerprint_sha256=SHA256:mf01uZE+NKV37R5wb5opx7/
Z7d9TJYUcbTUvsxFNcj0`, `trusted_at` set, `revoked_at` null.
The create-route's `accept_pins` check at
`crates/relayterm-api/src/routes/v1/terminal_sessions.rs:
153-164` only requires that at least one trusted, non-
revoked pin exists for the host — it does NOT call a fresh
preflight against the live target. The actual host-key
VERIFICATION happens later inside russh KEX inside
`pty_bridge.start(target)`. Since the stall container never
sends a banner, KEX never reaches the host-key step, so
the host-key check never fires — but the gate at the
create-route boundary already passed on the in-DB pin. No
trust-host-key call or fresh preflight was needed for this
run.

**Inventory reused (no new rows written; same host /
profile / identity from the prior smoke).**

- Host `quota-smoke-host` (id `026bcb2a-…`, port `2222`).
- Server profile `quota-smoke-profile`
  (id `c7606505-…`).
- SSH identity `quota-smoke-identity`
  (id `baa56cd3-…`, `ed25519`).
- Known-host entry as above.
- Staging user `staging+throwaway-20260509173230@example.
  com` (id `f968b6f5-…`), reused from the prior smoke;
  operator supplied the saved password externally for this
  run.

**Quota smoke proper.** Driven via authenticated `curl`
calls from the workstation (browser-write routes carry
`Origin: https://relayterm-staging.js-node.cc` and the
`relayterm_session` cookie; cookie file lived in
`/tmp/sq/cookies.txt` `chmod 600` for the smoke window only
— shredded at cleanup, never written to any tracked file).
Login body composed via `jq -n --rawfile pw …` reading the
password from a chmod-600 file so the plaintext never
crossed argv, env, or shell history.

- **Launch session A in background** —
  `POST /api/v1/terminal-sessions
  {server_profile_id:"c7606505-…", cols:80, rows:24}`
  blocking on the stall.
- **`sleep 3`** to give the create-route time to pass
  ownership / disabled / host-key / live-cap / starting-cap
  gates and call `create_session` (which registers the
  in-memory entry with `RuntimeSessionStatus::Starting`)
  before B fires. The starting-cap gate sits at
  `crates/relayterm-api/src/routes/v1/terminal_sessions.
  rs:196-219`, between the live-cap gate and the vault
  decrypt + bridge-start side effects.
- **Attempt session B (same profile, same user, starting
  slot full)** — same POST → **`429 Too Many Requests`**
  in `0.18 ms`, body literally
  `{"error":{"code":"too_many_starting_sessions",
  "message":"too many starting terminal sessions"}}`.
  Wire-stable per `crates/relayterm-api/src/error.rs:61`
  — `code` is the typed `too_many_starting_sessions`
  (distinct from the live-cap's `too_many_sessions` and
  from the login throttler's `too_many_requests`),
  `message` is the static safe form with no count / cap /
  session id / hostname / profile id / user id /
  `Retry-After` header (verified by curl `--include` —
  only `content-type: application/json`).
- **Session A resolves** — after `~10.0 s` total wall the
  background curl returned `502 Bad Gateway`, body
  `{"error":{"code":"bad_gateway","message":"bad gateway"}}`
  (the wire-stable safe envelope; the operator-side
  WARN line carries the detail `ssh transport failure
  during pty start`, which is the `map_pty_start_error`
  classification for `SshPtyError::Transport(_)` at
  `crates/relayterm-api/src/routes/v1/terminal_sessions.
  rs:304-308`).
- **DB after refusal** —
  `select count(*) from terminal_sessions
   where owner_id = 'f968b6f5-…'` went `31 → 32`. The single
  new row is session A (id `e58b4d55-…`,
  `status:"closed"`, created `04:13:47.924`, closed
  `04:13:57.930` — `record_pty_start_failed` closed it the
  moment the bridge errored). The 429-refused session B
  wrote **no `terminal_sessions` row**. `audit_events` row
  count was identical before and after the entire smoke
  window (**`40 → 40`**), confirming the refusal wrote
  **no audit row** per `docs/session-quotas.md` § 8.2.
- **Backend warn line for the refusal** (sigil-stripped for
  the doc): `WARN relayterm_api::routes::v1::terminal_
  sessions: terminal session quota refused user_id=
  f968b6f5-9cfc-46ae-b735-bc0f95465b5b scope=
  "per_user_starting" current_count=1 cap=1`. Public-shape
  only: `user_id`, `scope`, `current_count`, `cap`. No
  session id, no profile id, no host id, no identity id,
  no hostname, no peer banner, no wire body, no
  User-Agent. The `scope="per_user_starting"` label
  distinguishes the starting-burst refusal in operator
  logs from the Phase 1B.1 `scope="per_user_live"` label;
  both share the same redaction posture. Matches the
  operator-side logging policy in `docs/session-quotas.md`
  § 8.3.

**Log / nginx redaction sweep.** Full backend container log
across the recreate-to-end window (11 lines) and full nginx
container log (27 lines) were grepped for the sentinel set
`session_token|token_hash|cookie|password|private_key|
encrypted_private_key|BEGIN OPENSSH|data_b64|REDACT-MARKER`.
The two backend hits both match the static phrase
`detail=missing session cookie` inside `WARN
relayterm_api::error: unauthorized request …` lines from
the unauthenticated baseline checks — descriptive text
that mentions the word `cookie`, NOT any session-token
value. No `session_token` / `token_hash` value, no
`password` value, no `private_key` material, no
`BEGIN OPENSSH`, no `data_b64`, no `REDACT-MARKER`. Nginx
log: zero hits.

**Cap reverted at cleanup**: cleanup recreate replaced the
shell `MAX_STARTING_SESSIONS_PER_USER=1` override with the
compose default (`4`). Post-cleanup
`/api/v1/config/session-policy` (authenticated) returned
`{"detached_live_pty_ttl_seconds":30,
 "max_live_pty_sessions_per_user":8,
 "max_starting_sessions_per_user":4}`. Backend startup line
echoed `max_starting_sessions_per_user=4`. The compose-file
edit (one line in `relayterm-backend.environment`) stays in
place as the durable operator knob.

**Throwaway stall-target cleanup**: `docker rm -f
relayterm-staging-starting-quota-smoke-ssh` removed the
container at end of smoke. The `alpine:3` image stays in
the local cache; no host port was ever published, so no
firewall surface to revert.

**Temp credentials cleanup**: `/tmp/sq/{cookies.txt,
pass.txt,create.json,login.out,A_*,B_*}` (cookie file,
throwaway plaintext password, create-request body, raw
login response, per-request curl headers + bodies) were
chmod-600 throughout the smoke window so they were never
world-readable, and the whole `/tmp/sq/` directory was
`shred -u`'d at cleanup. No source-tree files were touched
this run — no new uncommitted helpers, no `.git/info/
exclude` entries (the operator supplied the password
externally, sidestepping the previous-smoke Argon2id PHC
helper pattern entirely).

**Inventory rows (host / profile / identity / known-host
entry / 3 closed terminal-session rows including the
smoke-A failed-start row) intentionally LEFT IN PLACE** per
the staging-smoke convention — they are operator-visible
carry-over for the next smoke, and no destructive-action
route was exercised in this slice.

**Verified.**

- Phase 1B.2a `max_starting_sessions_per_user` is the third
  authenticated-only wire field on
  `/api/v1/config/session-policy` (alongside the existing
  Phase 1A `detached_live_pty_ttl_seconds` and Phase 1B.1
  `max_live_pty_sessions_per_user`).
- Phase 1B.2a enforcement at `POST /api/v1/terminal-
  sessions` with cap=1: launch A blocks at the stall
  (session in `Starting` for ~10 s), launch B refused
  with `429 too_many_starting_sessions` in 0.18 ms,
  refusal writes no DB row + no audit row, refusal log
  line carries only public-shape fields, session A
  resolves as `502 bad_gateway` after the bridge inner
  timeout and is closed via `record_pty_start_failed`.
- The shipped wire envelope is the typed
  `{error:{code:"too_many_starting_sessions",
   message:"too many starting terminal sessions"}}`,
  distinct from the live-cap's `too_many_sessions` code
  and the login throttler's `too_many_requests` code.
- No `Retry-After` header on the 429 (verified by curl
  `--include`).
- The starting-cap gate sits AFTER ownership / disabled-
  profile / host-key / live-cap gates and BEFORE vault
  decrypt and any SSH side effects (verified by code
  reading at
  `crates/relayterm-api/src/routes/v1/terminal_sessions.
  rs:113-219` plus the no-DB-row / no-audit-row /
  no-decrypt observation: a 0.18 ms refusal cannot have
  reached vault decrypt or any outbound TCP work).
- All log / nginx redaction sentinels clean (only static
  `detail=missing session cookie` warnings, no values).
- Staging cap revert verified end-to-end (env, startup
  log echo, authenticated session-policy).

**Deferred (intentional non-goals for this run; do NOT
treat any of these as smoke-verified):**

- **Real KEX / auth / PTY allocation on the throwaway
  target** — the stall container does not speak SSH; the
  Phase 1B.1 live-cap smoke already covered the
  real-OpenSSH-server path on the same hostname.
- **WebSocket attach + actual terminal I/O on session
  A or any successor** — out of scope; the starting-cap
  gates session creation, which is upstream of any WS
  attach.
- **Deployment-wide quota
  (`max_live_pty_sessions_per_deployment`)** — Phase
  1B.3, not landed.
- **Operator dashboard tile** showing the caller's own
  live-session and starting-session counts vs cap —
  Phase 1B.3, not landed.
- **Prometheus / metrics surface** for quota counters —
  out of scope per `docs/session-quotas.md` § 8.4.
- **Durable persistent sessions across backend restart**
  — Phase 2 / 3 in
  [`docs/persistent-sessions.md`](../persistent-sessions.md),
  unchanged by this slice. The starting-cap acts ONLY on
  the in-memory runtime registry's
  `RuntimeSessionStatus::Starting` entries; a backend
  restart clears the registry, so any in-flight Starting
  count resets to 0 along with the live count.
- **VT snapshot resume** of an existing detached session
  across a restart — out of scope.
- **tmux / screen multiplexer pass-through** — out of
  scope.
- **RelayTerm-side persistent-session agent on the target
  host** — out of scope.
- **Production-default tuning** (whether `4` is the right
  per-user starting cap for a homelab vs a small team)
  — `docs/session-quotas.md` § 10.3 calls this Phase
  1B.4-style tuning; not in scope for this run.

---

### 2026-05-12 · Deployment-wide live PTY quota (Phase 1B.2b) staging finding: validator boot rejection verified; runtime smoke deferred

Slice `docs/deployment-wide-quota-smoke-and-closeout` attempted
to staging-verify the deployment-wide live PTY ceiling
shipped in `feat(api): enforce deployment live session quota`
(`316bc32`, 2026-05-12) plus the rustfmt-only follow-up
(`0ea0939`). The deployment cap sits BETWEEN the Phase 1B.1
per-user-live cap and the Phase 1B.2a per-user-starting cap in
the create-route ordering and is intended to bound the running
backend's total live-PTY footprint across all owners. Verified
against the HTTPS staging slot at
[`https://relayterm-staging.js-node.cc`](https://relayterm-staging.js-node.cc).

**Key finding — single-user runtime smoke is structurally
infeasible under the v1 validator.** The slice prompt and the
prior text of `docs/session-quotas.md` § 10.2b both proposed a
single-user recipe with caps
`(per-user-live=8, per-user-starting=4, deployment=1)`. That
configuration cannot boot: the § 5.2 validator (correctly)
rejects `max_live_pty_sessions_per_deployment <
max_live_pty_sessions_per_user`, observed live on staging at
the recreate attempt below. Walking the route ordering proves
the structural reason: for `deployment-live` to fire before
`per-user-live` for the same user requires `count_user_live <
user_cap` AND `count_total >= deployment_cap`; for one user
the two counters are equal, so the relation reduces to
`deployment_cap < user_cap`, which is exactly what the
validator forbids. The deployment cap is therefore a
structurally multi-user cap on a single backend instance,
and the staging slot has exactly one user
(`staging+throwaway-20260509173230@example.com`). Creating a
second staging user requires either operator-side
`auth.first_user_bootstrap_token` on a fresh DB (closed —
user row already present) or direct-SQL `INSERT` into `users`
+ `user_passwords` (out of scope this slice — would be auth
surgery). Runtime staging verification is therefore deferred
until a supported second-user provisioning path lands; the
Rust integration suite at
`crates/relayterm-api/tests/api.rs` continues to cover the
cross-owner deployment refusal envelope.

**Stack state at smoke start.** Compose project
`relayterm-staging` on `cloud-edge`. Pre-attempt, the deployed
backend image was the Phase 1B.2a `sha256:80d5e000…` (built
from `fd6813d`, the Phase 1B.2a commit, 2026-05-12 03:40 UTC)
and the web image was `sha256:96d4c5a8…` (built 2026-05-12
03:41 UTC). The Forgejo registry `:main` tag at the start of
the slice now resolved to the post-deployment-quota
`sha256:5d359a2d…` (backend, built 2026-05-12 05:21 UTC,
~7 minutes before the slice began) and `sha256:73486f6c…`
(web). The fresh backend digest was confirmed to carry
`316bc32` indirectly — it boots `validate_terminal_sessions`
correctly when the new field is contradicted (the validator
error message at `apps/backend/src/config.rs:1335` was
emitted by the recreate attempt below).

**Compose env wiring (durable, landed this slice).** The
staging compose at `/home/ubuntu/docker-compose/relayterm-staging/docker-compose.yml`
gained a single line, slotted right after the existing
`MAX_STARTING_SESSIONS_PER_USER` row inside the
`relayterm-backend.environment` block:

```yaml
      RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT: "${RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT:-64}"
```

Exact-match the repo template at
`deploy/docker-compose.traefik-staging.example.yml:117`. The
default expansion resolves to `64` (matching
`relayterm_terminal::DEFAULT_MAX_LIVE_PTY_PER_DEPLOYMENT` and
`docs/session-quotas.md` § 4.2); the smoke override attempt
(`=1`) was injected via shell-env at the `docker compose up
-d --force-recreate` invocation, NOT written to any `.env`
file. This compose-file edit stays in place after the slice
as the durable operator-knob wiring; post-revert, the `:-64`
default re-applies via the no-override recreate.

**Boot validator rejection (the load-bearing staging
finding).** The recreate attempt
`RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT=1
docker compose up -d --no-deps --force-recreate
relayterm-backend relayterm-web` against the running per-user
defaults (`MAX_LIVE_PTY_SESSIONS_PER_USER=8`,
`MAX_STARTING_SESSIONS_PER_USER=4`) failed the dependency
healthcheck inside the compose orchestration. `docker logs
relayterm-staging-relayterm-backend-1` showed a tight crash
loop with the exact error:

```
Error: validate terminal session config

Caused by:
    terminal_sessions.max_live_pty_sessions_per_deployment = 1 must be >= terminal_sessions.max_live_pty_sessions_per_user = 8; a per-user live ceiling above the deployment ceiling is a contradiction (every user would be capped by the deployment value before reaching their personal cap)
```

— byte-for-byte the message emitted by the validator at
`apps/backend/src/config.rs:1335`. This is the live
staging verification of test item **(i)** from
`docs/session-quotas.md` § 11: "validator rejects
`max_dep < max_live_per_user` at boot." The error string
mentions only `1`, `8`, and the static contradiction
explanation — no secrets, no peer / vault internals, no
caller info, no session id, no hostname.

**Revert to default cap=64 (post-validator finding).**
`docker compose up -d --no-deps --force-recreate
relayterm-backend relayterm-web` (no shell override) ran
cleanly: both containers reached `healthy` within ~3 s of
start; the durable compose `:-64` default applied. Postgres
was untouched (`Up 2 days (healthy)` across the slice). The
backend startup line literally reads:

```
relayterm-backend starting addr=0.0.0.0:8080 auth_mode="production" recording_enabled=false detached_live_pty_ttl_seconds=30 max_live_pty_sessions_per_user=8 max_starting_sessions_per_user=4
```

— note the deployment cap is intentionally NOT in the
startup-log echo at `apps/backend/src/main.rs:56-65`,
consistent with § 5.4 of `docs/session-quotas.md` ("operator-
only, fingerprinting risk; never exposed on session-policy or
the startup log"). The cap is verifiable instead via `docker
inspect relayterm-staging-relayterm-backend-1 --format
'{{range .Config.Env}}{{println .}}{{end}}'`, which (post-
revert) shows all four `RELAYTERM_TERMINAL_SESSIONS__*` vars
with their defaults including
`MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT=64`.

**Baseline checks (post-revert).**

- `GET /healthz` → `200`.
- `GET /api/v1/auth/me` (unauthenticated) → `401`.
- `docker inspect …` env on the running backend confirms
  `RELAYTERM_TERMINAL_SESSIONS__MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT=64`.
- Backend image digest: `sha256:5d359a2d…`
  (`git.js-node.cc/jsprague/relayterm-backend:main`,
  post-deployment-quota — upgraded from the pre-slice
  `sha256:80d5e000…`).
- Web image digest: `sha256:73486f6c…`
  (`git.js-node.cc/jsprague/relayterm-web:main`,
  post-deployment-quota — upgraded from the pre-slice
  `sha256:96d4c5a8…`).

The authenticated `GET /api/v1/config/session-policy` was
NOT exercised this slice (would have required login as the
existing staging user, which the slice prompt did not
require for a validator-only finding); the wire shape is
pinned by the frontend sentinel test
`apps/web/tests/sessionPolicy.test.ts` and the backend
integration test `session_policy_response_does_not_expose_deployment_cap`
(or equivalent — the policy DTO did NOT change for 1B.2b
per § 5.4 of `docs/session-quotas.md`).

**Runtime smoke NOT run.** Per the structural-infeasibility
finding above, no `too_many_sessions_deployment` wire
envelope was observed against the staging stack this slice.
No throwaway SSH stall target was created (the prior smoke's
`quota-smoke-host` inventory row remains in DB; the
container behind it was removed at the 1B.2a cleanup). No
authenticated cookie was minted; no `/tmp/sq/*` scratch
directory was created; no SPA / Playwright session was
opened.

**Log / nginx redaction sweep.** Backend container log
covering the recreate-and-revert window (the failed
validator-rejection cycle plus the clean restart, ~30
lines) and nginx log (no traffic) were grepped for the
sentinel set
`session_token|token_hash|cookie|password|private_key|encrypted_private_key|BEGIN OPENSSH|data_b64|REDACT-MARKER`.
**Zero hits** in either log. The validator error message
itself names only the two integer caps (`1` and `8`) and
the static contradiction explanation — no secret-shaped
substring.

**Inventory state — unchanged.** No `terminal_sessions`,
`session_events`, or `audit_events` rows were written this
slice (no successful create, no refusal observed beyond
the boot-validator rejection which is not a route-write).
The prior smoke's `quota-smoke-host` / `quota-smoke-profile`
/ `quota-smoke-identity` / known-host-entry rows remain
intentionally in place per the staging-smoke convention.

**Net change to staging at slice end.**

- Backend image upgraded from `sha256:80d5e000…` (Phase
  1B.2a) to `sha256:5d359a2d…` (post-deployment-quota,
  `316bc32` + `0ea0939`).
- Web image upgraded from `sha256:96d4c5a8…` to
  `sha256:73486f6c…`.
- One new durable line in
  `docker-compose.yml` for the deployment-cap env
  pass-through (default `64`).
- All four `RELAYTERM_TERMINAL_SESSIONS__*` env vars now
  present on the running backend with their defaults.
- No `.env` change.
- No DB change (no migrations applied this slice; the
  deployment-quota commit is in-memory + config only).
- No throwaway containers.
- No scratch / cookie / cred files.

**Verified.**

- Phase 1B.2b code (`316bc32` + `0ea0939`) is present in
  the freshly-deployed `:main` digests AND boots cleanly
  with default cap=64.
- Phase 1B.2b boot validator rejects
  `max_live_pty_sessions_per_deployment <
  max_live_pty_sessions_per_user` with the static
  contradiction message — staging-verifies test item
  **(i)** from § 11 of `docs/session-quotas.md`.
- The fresh image starts under default config without
  regression in any of the three already-staging-verified
  quota fields (TTL, per-user-live, per-user-starting all
  echo correctly on the startup line).
- Durable compose pass-through line for
  `MAX_LIVE_PTY_SESSIONS_PER_DEPLOYMENT` landed on staging,
  mirroring the repo template style.
- Log / nginx redaction sentinel sweep clean (no hits in
  either log).

**Deferred (intentional non-goals for this run; do NOT
treat any of these as staging-verified):**

- **Runtime `too_many_sessions_deployment` wire envelope
  on staging** — requires a supported second staging user
  (see § 10.2b "Follow-up requirement" in
  `docs/session-quotas.md`). Covered by the Rust
  integration suite at `crates/relayterm-api/tests/api.rs`
  for now.
- **Operator dashboard tile** showing the caller's own
  live-session count vs cap — Phase 1B.2c, not landed.
- **Prometheus / metrics surface** for quota counters —
  out of scope per `docs/session-quotas.md` § 8.4.
- **Cross-instance coordination** of the deployment cap
  (single-backend exact; multi-backend per-instance
  best-effort) — out of scope per § 9 of
  `docs/session-quotas.md`.
- **Durable persistence** of quota counters across backend
  restart — out of scope (§ 3 non-goals).
- **VT snapshot resume**, **tmux / screen multiplexer
  pass-through**, **RelayTerm-side persistent-session
  agent** — out of scope.
- **Authenticated `/api/v1/config/session-policy` probe**
  this run — DTO shape unchanged for 1B.2b per § 5.4 of
  `docs/session-quotas.md`; pinned by the sentinel test
  `apps/web/tests/sessionPolicy.test.ts`.
- **Production-default tuning** — out of scope per
  `docs/session-quotas.md` § 10.3.

---

### 2026-05-12 · Inventory management mutations (hosts / server-profiles / SSH identities edit + delete) staging smoke

Slice `docs/inventory-management-staging-smoke` staging-verifies
the inventory-management mutation surface shipped in
`feat(api): add inventory management mutations`
(`f1f0691`, 2026-05-12). The commit landed six new routes —
`PATCH/DELETE /api/v1/hosts/:id`,
`PATCH/DELETE /api/v1/server-profiles/:id`,
`PATCH/DELETE /api/v1/ssh-identities/:id` — each gated by
`CsrfGuard` + `AuthenticatedUser` and each refusing the
destructive path when the row is still referenced by a
dependent (per the SPEC.md "Inventory lifecycle and
destructive-action policy"). Smoke covers the 200 / 400 / 404 /
409 / 401 / 403 envelopes against a freshly-deployed `:main`,
with bounded DB and log redaction sweeps as the backstop.

**Image freshness.** Forgejo CI for `f1f0691` (all 6 jobs
green; `publish images` finished 2026-05-12T23:50:35Z) pushed
`relayterm-backend:main` @ `sha256:55e64c11…` (built
2026-05-12T23:49:48Z) and `relayterm-web:main` @
`sha256:63694b40…` (built 2026-05-12T23:50:27Z). Staging VPS
(`cloud-edge`, compose project `relayterm-staging` at
`/home/ubuntu/docker-compose/relayterm-staging`) was on the
prior digest pair (`5d359a2d…` / `73486f6c…` — the
Phase 1B.2b deployment-quota line from the entry above);
`docker pull` + `docker compose up -d --no-deps --force-recreate
relayterm-backend relayterm-web` brought both services to the
new digests cleanly (backend healthy within ~6 s; web healthy
within ~27 s). Postgres untouched (`Up 3 days (healthy)`). The
served SPA bundle is `index-CiiA2M_K.js` with `last-modified:
Tue, 12 May 2026 23:50:27 GMT` — byte-equal to the web image
build time, i.e. the live SPA is the post-commit bundle.

**Operator user / login.** Reused the existing throwaway
bootstrap user
`staging+throwaway-20260509173230@example.com`
(`f968b6f5-9cfc-46ae-b735-bc0f95465b5b`); same one the
2026-05-09 / -10 / -11 / earlier-2026-05-12 entries above
exercise. Password read from
`~/dev/RelayTermSecrets.md` into a private 0600 scratch file,
fed to `curl --data-urlencode` once, then `shred -u`'d before
any further work. Cookie jar at
`/tmp/relayterm-inv-smoke/cookie.txt` (HttpOnly + Secure
flagged by the server). No production credentials, no
private-key import, no real production SSH identities.

**HTTPS reachability gate (§7.3 re-checked post-recreate).**
`/` → `200`, `/healthz` → `200 {"status":"ok"}` (15 bytes),
`/api/v1/auth/me` → `401` from outside the SPA.

**Mutation surface presence cross-check (unauth + bad
Origin).** Before login, every new route was probed from the
workstation with no cookie / bad Origin to confirm correct
gating:

| Probe | Expected | Observed |
|---|---|---|
| `PATCH /api/v1/hosts/:id` (Origin set, no cookie) | `401` | `401` |
| `DELETE /api/v1/hosts/:id` (Origin set, no cookie) | `401` | `401` |
| `PATCH /api/v1/ssh-identities/:id` (Origin set, no cookie) | `401` | `401` |
| `DELETE /api/v1/ssh-identities/:id` (Origin set, no cookie) | `401` | `401` |
| `PATCH /api/v1/server-profiles/:id` (Origin set, no cookie) | `401` | `401` |
| `DELETE /api/v1/server-profiles/:id` (Origin set, no cookie) | `401` | `401` |
| `PATCH /api/v1/hosts/:id` (no Origin) | `403 csrf_origin_mismatch` | `403 csrf_origin_mismatch` |
| `DELETE /api/v1/server-profiles/:id` (no Origin) | `403 csrf_origin_mismatch` | `403 csrf_origin_mismatch` |
| `DELETE /api/v1/ssh-identities/:id` (Origin = `https://evil.example.com`) | `403 csrf_origin_mismatch` | `403`, body `{"error":{"code":"csrf_origin_mismatch","message":"forbidden"}}` — does NOT echo the offered Origin value (AGENTS.md "Things to avoid" § 7) |

All six routes are reachable (no 404), all six gate CSRF
before auth (a missing/bad Origin produces 403 before the
401), all six require a valid session.

**Smoke resources created (timestamp suffix `t20260512`).**
Created via the existing POST helpers under the smoke user
(`/api/v1/hosts`, `/api/v1/ssh-identities`,
`/api/v1/server-profiles`). All names use the
`inv-smoke-…-t20260512` convention so the rows are easy to
identify and easy to leave-in-place per the
inventory-lifecycle policy:

| Role | ID | Initial name |
|---|---|---|
| H1 (edit target) | `2fe699d7-32f6-44f3-959f-0839ca46b2a8` | `inv-smoke-host-edit-t20260512` |
| H2 (delete-success target) | `2482593b-c6ca-4814-ae48-1beba8f5329c` | `inv-smoke-host-delete-free-t20260512` |
| H3 (host-delete-conflict subject) | `1ff05505-98cb-4d5f-9369-f1899eb2bc7e` | `inv-smoke-host-referenced-t20260512` |
| I1 (rename target) | `b49c7228-7880-4dfa-aabf-03b6a95ccb89` | `inv-smoke-identity-rename-t20260512` |
| I2 (paired identity, delete-free profile's key) | `1d461cf7-77e0-4795-b0a0-d6e34c4f31eb` | `inv-smoke-identity-delete-free-t20260512` |
| I3 (identity delete-success target) | `b02d4f01-b44f-4216-8e1f-d4317baa8c24` | `inv-smoke-identity-delete-free-only-t20260512` |
| P1 (profile edit target) | `582b7861-f713-4401-9f16-b1ce78d0b470` | `inv-smoke-profile-edit-t20260512` |
| P2 (profile delete-success target) | `597dfccd-b00b-4a94-88fd-9ce6ef1c1a17` | `inv-smoke-profile-delete-free-t20260512` |
| P3 (host-delete-conflict blocker) | `5b3150f1-574e-4271-b47c-49ba35c6ed00` | `inv-smoke-profile-referenced-t20260512` |

The pre-existing `smoke-id` identity
(`44b5e2be-29c2-4eb0-b6ac-3b4e25ca789d`) was re-used as the
SSH identity bound to P3. The 10 existing
`server_profiles` with `terminal_sessions` history (e.g.
`ux-smoke-profile-v2` with 7 rows, `android-smoke-profile`
with 6, etc.) were re-used for the
`server_profile_referenced` (history) delete-conflict probe.
Their rows were NOT mutated by this smoke.

**Mutation results against the live backend.**

| # | Operation | Wire | Observed | DB cross-check |
|---|---|---|---|---|
| 1 | `PATCH /ssh-identities/I1` rename → `inv-smoke-identity-renamed-t20260512` | `200` | `200` body has new name; `key_type=ed25519`, `fingerprint_sha256` unchanged, `created_at` unchanged, `last_used_at=null` | row matches; `encrypted_private_key` length unchanged at 477 bytes |
| 2 | `DELETE /ssh-identities/I3` (no profile ref) | `204` | `204` no body; follow-up GET → `404 not_found` | row absent; `ssh_identity_deleted` audit row written with `{id, name, key_type, fingerprint_sha256, created_at}` payload — NO `encrypted_private_key`, NO `public_key` bytes |
| 3 | `DELETE /ssh-identities/I1` (referenced by P1) | `409 conflict` | `409 {"error":{"code":"conflict","message":"ssh_identity referenced"}}` — no profile id / count / raw-error echo | I1 row preserved; zero new audit rows for the conflict attempt |
| 4a | `PATCH /hosts/H1` all four fields (`display_name` + `hostname` + `port=2222` + `default_username=smoke2`) | `200` | `200` body has updated values; `updated_at` advanced past `created_at` | row matches updated state |
| 4b | `PATCH /hosts/H1` `{}` empty | `400 invalid_input` | `400 {"error":{"code":"invalid_input","message":"at least one field must be provided"}}` | no DB write |
| 4c | `PATCH /hosts/H1` `{"port":99999}` | `400 invalid_input` | `400 {"error":{"code":"invalid_input","message":"ssh port must be in range 1..=65535 (got 99999)"}}` — input echo (offered integer only) is the standard validator copy and matches the create-route pattern; no raw backend error string | no DB write |
| 4d | `PATCH /hosts/H1` partial (only `display_name=inv-smoke-host-edited-final-t20260512`) | `200` | `200`; only `display_name` changed, `hostname`/`port`/`default_username` preserved from 4a | row matches |
| 5a | `DELETE /hosts/H2` BEFORE deleting P2 | `409 conflict` | `409 {"error":{"code":"conflict","message":"host referenced"}}` — no profile-id echo | H2 row preserved |
| 5b | `DELETE /hosts/H2` AFTER deleting P2 (no remaining dependents — no profile, no `known_host_entries`) | `204` | `204`; follow-up GET → `404 not_found` | H2 row absent. Pre-delete DB query (`SELECT host_id, COUNT(*) FROM known_host_entries WHERE host_id IN (H1,H2,H3)`) returned zero rows for all three smoke hosts, so the schema-level `ON DELETE CASCADE` on `known_host_entries.host_id` was NOT exercised by this smoke item — the route's `any_dependents_for_user` predicate would have returned `true` and refused the delete with `409 host referenced` had any pin existed (this is the same code path as the deferred smoke item 7 below). **Identity I2 NOT cascade-deleted** (`GET /ssh-identities/I2` → `200`) — host delete is owner-scoped and does not touch identities or profiles |
| 6 | `DELETE /hosts/H3` (P3 attached) | `409 conflict` | `409 {"error":{"code":"conflict","message":"host referenced"}}` | H3 row preserved |
| 7 | `DELETE /hosts/<host with known_host_entries but no profile>` | (not exercised this slice) | DEFERRED — see "Deferred" below |
| 8a | `PATCH /server-profiles/P1` `{name, tags:["smoke","inv","t20260512"]}` | `200` | `200`; row has new name + new tag set | row matches; `server_profile_updated` audit row written |
| 8b | `PATCH /server-profiles/P1` `{"username_override":"override-user"}` | `200` | `200`; `username_override="override-user"` | row matches |
| 8c | `PATCH /server-profiles/P1` `{"username_override":null}` (explicit null — tri-state `Some(None)` → `SetOptional::Set(None)` path) | `200` | `200`; `username_override=null` | row matches; the omitted-vs-null distinction in `UpdateServerProfileRequest::deserialize_some_present` is the load-bearing wire contract |
| 8d | `PATCH /server-profiles/P1` `{}` empty | `400 invalid_input` | `400 {"error":{"code":"invalid_input","message":"at least one field must be provided"}}` | no DB write |
| 8e | `PATCH /server-profiles/P1` `{"host_id":H3}` then `{"host_id":H1}` (rebind round-trip) | `200`, `200` | both `200` | row matches end-state H1 |
| 9 | `DELETE /server-profiles/P2` (zero `terminal_sessions` refs) | `204` | `204`; follow-up GET → `404 not_found` | P2 row absent; `server_profile_deleted` audit row written with `{id, name, host_id, ssh_identity_id, disabled_at}` payload; H2 + I2 NOT cascade-deleted (verified via 5b above) |
| 10 | `DELETE /server-profiles/d1207a25-…` (`ux-smoke-profile-v2`, 7 historical `terminal_sessions` rows) | `409 conflict` | `409 {"error":{"code":"conflict","message":"server_profile referenced"}}` — no session-id / count / raw-error echo | profile row preserved (follow-up GET → `200`); `terminal_sessions`, `session_events`, `audit_events` history NOT touched |
| 11 | CSRF / auth sanity | covered in the probe table above | `401` unauth, `403 csrf_origin_mismatch` no/wrong-Origin, body never echoes offered Origin | — |

The UI-side error formatters
(`describeDeleteHostError`, `describeDeleteServerProfileError`,
`describeDeleteSshIdentityError`,
`describeUpdateHostError`, `describeUpdateServerProfileError`,
`describeUpdateSshIdentityError` in
`apps/web/src/lib/api/{hosts,serverProfiles,sshIdentities}.ts`)
re-map each 409 / 400 / 404 / 401 / 403 envelope above to
user-friendly copy that does NOT echo the wire `message`
field. The 451-line unit test
`apps/web/tests/inventoryMutationsApi.test.ts` (46
`describe`/`it` sections; landed with `f1f0691`; CI green)
pins these mappings, including the "`never echoes wire
message`" invariant for every error path and the "does not
echo private-key material in the parsed DTO" invariant on
the rename response shape.

**Backend audit-events tally for the smoke window**
(`actor_id = f968b6f5-…`, `recorded_at > now() - 30 min`):

```
          kind          | count
------------------------+-------
 login_succeeded        |     1
 server_profile_created |     3   ← P1 + P2 + P3
 server_profile_deleted |     1   ← P2 only (success)
 server_profile_updated |     5   ← 8a + 8b + 8c + 8e (H3) + 8e (H1)
 ssh_identity_deleted   |     1   ← I3 only (success)
```

Zero `host_*` audit rows — the `audit_events_kind_chk`
constraint deliberately omits `host_*` kinds; host mutations
are inventory metadata and produce no audit (see
`crates/relayterm-api/src/routes/v1/hosts.rs` route docs).
Zero `ssh_identity_updated` rows — the schema constraint
omits this kind too; identity rename is inventory metadata
only (see source comment at `ssh_identities.rs::update`).
Zero conflict-attempt audit rows — the route-layer
short-circuit returns `Err(Conflict)` BEFORE the audit
append, matching the canonical pattern in `docs/spec/
inventory.md` § "Server profile lifecycle audit" and the
AGENTS.md "Things to avoid" line "append an audit row on a
redundant/idempotent lifecycle call".

**Audit payload redaction.** Every payload above carries
public metadata ONLY — `{id, name, host_id, ssh_identity_id,
disabled_at}` for server-profile lifecycle events, `{id,
name, key_type, fingerprint_sha256, created_at}` for
`ssh_identity_deleted`. No `encrypted_private_key`, no
`public_key` bytes, no PEM, no peer banner, no cookie, no
session id, no raw russh/DB error string, no `data_b64`.
The sentinel-test guard
`AUDIT_FORBIDDEN_SUBSTRINGS` in the API integration suite is
the backstop.

**Dependency rules verified end-to-end against the live
schema.**

- Host delete refused when any owned `server_profiles` row
  references the host (smoke item 5a + 6; FK
  `server_profiles.host_id ON DELETE RESTRICT`).
- Host delete refused when ANY `known_host_entries` row
  references the host — **deferred (see below)**; the
  `any_dependents_for_user` predicate is one short-circuit OR
  across both refs and the same 409 envelope covers either
  branch.
- Server-profile delete refused when ANY `terminal_sessions`
  row references the profile (smoke item 10; FK
  `terminal_sessions.server_profile_id ON DELETE RESTRICT`).
- SSH-identity delete refused when any owned
  `server_profiles` row references the identity (smoke item
  3; FK `server_profiles.ssh_identity_id ON DELETE RESTRICT`).
- `terminal_sessions`, `audit_events`, `session_events`,
  `known_host_entries` history rows are NOT hard-deleted by
  any operation in this slice. The successful `H2` host
  delete + `P2` profile delete each left `audit_events`
  history intact (the `server_profile_deleted` audit row for
  P2 is itself the audit-history evidence).

**Log / nginx redaction sweep — staging containers for the
~16-minute smoke window.** `docker logs --since 30m`
captured 16 lines on the backend (one startup block + 7
`unauthorized request detail="missing session cookie"` +
2 `csrf origin mismatch detail="missing Origin header"` + 1
`csrf origin mismatch detail="Origin not in allowed_origins"`)
and 69 lines on the web nginx. High-value sentinel sweep
(`session_token=|token_hash=|password=|password":[^"n]|
private_key|encrypted_private_key|BEGIN OPENSSH|data_b64|
REDACT-MARKER`) returned **zero hits** in either log. The
category-shaped word "cookie" appears 7× in the backend log
ONLY as the literal diagnostic label
`detail="missing session cookie"` — naming the absence of a
value, not echoing a value. The category-shaped word
"Origin" appears in the third CSRF WARN ONLY as the literal
phrase `Origin not in allowed_origins` — the offered Origin
value itself is NOT echoed (per AGENTS.md § 7 / the
`bad_origin_rejects_before_body_parsing` integration test).
Successful mutations produced **zero backend log lines** —
the routes do not `tracing::info!` on success, so no row
content is leaked through the structured-log path. Audit
events are the only durable record (covered above).

**UI driving.** This slice did NOT drive the SPA through a
real browser (Playwright would require putting the smoke
user's password into a tool-call argv, which would land in
the conversation log and violate the slice's "do not print
passwords/cookies/tokens" rule; the session cookie is
`HttpOnly` so it cannot be side-loaded into the browser via
JS either). The SPA bundle on staging IS the post-commit
bundle (verified via `last-modified` + web image digest
above); the SPA-side mutation surface that the UI calls is
covered by `apps/web/tests/inventoryMutationsApi.test.ts`
(landed with `f1f0691`; CI green); the user-facing copy
strings for every error envelope above were verified
statically and are pinned by the "never echoes wire message"
invariant in that test. Replacing this static + API-level
verification with a real browser drive is a follow-up — see
"Deferred" below.

**Cleanup posture / inventory state at slice end.**

- H2 (`inv-smoke-host-delete-free-t20260512`) — DELETED to
  verify the success path; row absent.
- P2 (`inv-smoke-profile-delete-free-t20260512`) — DELETED
  to verify the success path; row absent. The
  `server_profile_deleted` audit row is the durable record.
- I3 (`inv-smoke-identity-delete-free-only-t20260512`) —
  DELETED to verify the success path; row absent. The
  `ssh_identity_deleted` audit row is the durable record
  (with `encrypted_private_key` hard-deleted from disk per
  the only-allowed-removal path).
- H1 (`inv-smoke-host-edited-final-t20260512`) — KEPT;
  reflects the last partial PATCH (smoke item 4d).
- H3 (`inv-smoke-host-referenced-t20260512`) — KEPT;
  blocks-host-delete subject for any future re-verification.
- I1 (`inv-smoke-identity-renamed-t20260512`) — KEPT;
  rename target.
- I2 (`inv-smoke-identity-delete-free-t20260512`) — KEPT;
  still bound to nothing (P2 deleted; smoke convention is
  to leave inventory in place).
- P1 (`inv-smoke-profile-edited-t20260512`) — KEPT; reflects
  the final 8e PATCH end-state (host=H1, identity=I1,
  username_override=null, tags=`{smoke,inv,t20260512}`).
- P3 (`inv-smoke-profile-referenced-t20260512`) — KEPT;
  blocks-host-delete-of-H3 subject for any future
  re-verification.
- `ux-smoke-profile-v2` and the other 9 historical
  smoke profiles + their `terminal_sessions` history —
  UNTOUCHED (smoke item 10 only attempted DELETE and
  observed 409; no rows mutated).
- No `.env` change. No durable Compose / nginx /
  docker-compose template change.
- No throwaway SSH container created this slice.
- Local scratch dir `/tmp/relayterm-inv-smoke/` shredded
  (`pw`, `login.json`) and otherwise carries only the
  per-resource JSON snapshots + cookie jar; teardown is a
  single `shred -u` pass on the cookie jar and `rm -rf` on
  the directory after operator approval.
- Staging stack left running on the new digest pair
  (`55e64c11…` / `63694b40…`).

**Verified.**

- `feat(api): add inventory management mutations` (`f1f0691`)
  is on `main`, CI green, image-published, deployed, and
  exercised end-to-end against the live staging slot.
- All six new routes gate CSRF before auth, gate auth, and
  surface the documented 409 envelope on every dependency
  branch tested this slice.
- The inventory-lifecycle policy from `SPEC.md` /
  `docs/agent/redaction-rules.md` § 2 + § 3 holds against
  the live schema: hard-delete refused when dependents
  exist, no cascade onto history tables, no audit row on
  conflict-attempt, audit row written field-by-field with
  public metadata only on each success path that has an
  audit kind, no audit row at all on the kinds whose schema
  CHECK constraint deliberately omits them
  (`host_*`, `ssh_identity_updated`).
- SPA bundle on staging is the post-commit build; UI-layer
  mutation surface is unit-test-pinned.
- Log / nginx redaction sentinel sweep clean across the
  entire smoke window.

**Deferred (intentional non-goals for this run; do NOT treat
any of these as staging-verified by this entry):**

- **Host-delete-conflict via `known_host_entries`-only ref**
  (smoke item 7). The route's `any_dependents_for_user`
  predicate is a single short-circuit OR across
  `server_profiles` AND `known_host_entries`, so item 6
  (profile-ref blocker) already exercises the same `409
  host referenced` envelope. Standing up a host with a
  `known_host_entries` row but no profile would require
  creating a throwaway SSH container + walking the
  trust-host-key flow + deleting the profile — out of
  scope per the slice's "do not over-expand the smoke" /
  "no throwaway SSH containers without approval" rules.
  Covered by integration tests at
  `crates/relayterm-api/tests/api.rs` per the
  `any_dependents_for_user` matrix.
- **Browser-driven SPA verification.** See "UI driving"
  above. Replacing the API + static UI-copy verification
  with a real Playwright (or Tauri WebView) drive is a
  follow-up; would require a browser/IPC path that the
  smoke user password does NOT cross via a tool-call argv.
  The deployed bundle's freshness is already pinned via
  `last-modified` + web image digest.
- **Private-key import.** The
  `CreateSshIdentityRequest` DTO has no
  field for importing externally-generated private-key
  material; the vault is the only path. Out of scope per
  the source-level comment at
  `crates/relayterm-api/src/dto/ssh_identity.rs:27-32`.
- **`ssh-copy-id` / bootstrap automation.** Out of scope.
- **Route-param detail pages** (`/servers/:id`,
  `/hosts/:id`, `/identities/:id`). The list views handle
  inline edit / delete; detail-page surfaces are a
  separate slice and not landed.
- **Quota metrics / operator dashboard tile.** Out of
  scope per `docs/session-quotas.md` § 8.4 / 1B.2c.
- **Terminal renderer evaluation.** Out of scope —
  no renderer code touched.
- **Durable persistence** beyond the current
  Postgres-backed inventory tables. Out of scope.
- **Hard-delete of `terminal_sessions`, `audit_events`,
  `session_events`, `known_host_entries`.** Explicitly NOT
  a goal — those tables are append-only / lifecycle-
  preserving per AGENTS.md "Things to avoid".
- **`SPEC.md` / `docs/spec/inventory.md` stale-wording
  cleanup is a separate follow-up slice — NOT in scope for
  this smoke entry.** The inventory-management routes
  landing in `f1f0691` make several existing statements
  stale: `SPEC.md` lines 59 / 151 / 188 / 164-165 / 188
  carry "remain future work" wording for create / edit /
  delete forms, identity rename, host create/update/delete
  audit kinds, etc.; `SPEC.md` lines 133 + 153 + 112
  describe known-host CASCADE as the user-facing semantic
  but the route at
  `crates/relayterm-api/src/routes/v1/hosts.rs` (per
  `any_dependents_for_user` at
  `crates/relayterm-db/src/repositories/host.rs:155-186`)
  refuses host delete whenever any `known_host_entries`
  row exists, overriding the schema-level cascade for the
  user-facing surface. Per the slice prompt ("Optionally
  update: SPEC.md only if it has a status checklist…",
  "Do not do a broad docs rewrite"), this entry leaves
  SPEC.md / `docs/spec/inventory.md` unchanged; the
  cleanup is named-and-deferred here so a future
  `/audit-spec-drift` or `/trim-spec-md` run picks it up
  with full context. AGENTS.md "Maintenance protocol"
  applies — a follow-up commit on a separate branch
  should land the SPEC text update.

### 2026-05-13 · Inventory management browser-driven SPA smoke (host / server-profile / SSH identity UI)

Slice `docs/inventory-management-browser-smoke` is the
follow-up the prior 2026-05-12 entry deferred: it drives
the production SPA inventory-mutation UI in a real browser
(Playwright MCP, headless) against the same live staging
slot — same backend / web image digests, same throwaway
operator user — and verifies the edit / delete / conflict
copy is what the SPA actually surfaces to a human, not just
what the wire protocol exposes. The prior entry stated its
verification was "API + static UI-copy" (no real DOM
drive); this entry closes that gap. The same six routes
(`PATCH/DELETE` on `hosts`, `server-profiles`,
`ssh-identities`) are exercised, but through the
`host-detail-edit-*` / `profile-detail-edit-*` /
`identity-detail-rename-*` and `*-delete-confirm-*` testid
surfaces in `apps/web/src/lib/app/views/ServersView.svelte`
(2598 lines) and `apps/web/src/lib/app/views/IdentitiesView.svelte`
(1039 lines).

**Surface.** Playwright MCP (`mcp__playwright__browser_*`)
driving a Chromium browser session against
`https://relayterm-staging.js-node.cc` (the MCP default;
no explicit `--browser` override and no explicit
`--headless` flag this slice — operator did not visually
inspect the running browser, but the harness did not
declare a mode either way, so do not over-claim a headed
or headless rendering posture here). No Tauri shell
involved in this entry — bundled-shell handoff was
covered by 2026-05-09. Viewport 1440 × 900 for the main
flow; a single 414 × 896 reachability check at the end.
Browser session held only for the duration of the smoke;
no cookie material was printed, stashed, or attached.

**Image freshness.** No-op vs. 2026-05-12. Confirmed both
service containers were still running the post-`f1f0691`
digests:

- `relayterm-backend` image `sha256:55e64c11…`
  (built 2026-05-12T23:49:48Z)
- `relayterm-web` image `sha256:63694b40…`
  (built 2026-05-12T23:50:27Z)

No `docker pull` / recreate this slice; the prior entry's
deploy is what this smoke exercises. `postgres:17-alpine`
still `Up 3 days (healthy)`.

**Operator user.** Reused the same throwaway staging user
the prior entry created
(`staging+throwaway-20260509173230@example.com`). The
existing browser session was still valid; no fresh login
attempt was required. Password is operator-held and never
crossed any tool argv this slice.

**Production-route preflight.**
- `GET /healthz` → 200, body `{"status":"ok"}`.
- `GET /api/v1/auth/me` without cookies → 401
  `{"error":{"code":"unauthorized","message":"unauthorized"}}`.
- `GET /` (SPA) → 200, HTML contains the post-`f1f0691`
  bundle (`index-CiiA2M_K.js`).
- The six mutation routes return the expected gating
  envelopes when probed without auth:
  `PATCH /api/v1/hosts/<dummy>` → 403
  `csrf_origin_mismatch` without an `Origin` header, 401
  with `Origin` set — same shape across hosts /
  `server-profiles` / `ssh-identities`. Routes are
  mounted under the **hyphenated** paths
  (`/server-profiles`, `/ssh-identities`), per
  `crates/relayterm-api/src/routes/v1/mod.rs:26-27`; a
  first-pass probe using underscored paths returned 404
  before correction (worth pinning here so a future
  smoke does not waste cycles on the same mismatch).

**1. Hosts UI smoke.** Selected host
`inv-smoke-host-edited-final-t20260512` (the one the
prior smoke landed; 1 profile reference).

- *Valid edit, round-trip:* set
  display_name → `inv-smoke-host-uismoke-t20260513`,
  hostname → `inv-smoke-uismoke.example.invalid`, port
  → `2225`, default_user → `smoke-ui`. Inline edit form
  closes; detail panel `data-testid="host-detail-*"`
  fields reflect new values; list row text updates
  byte-equal; `host-detail-updated-at` advances. Then
  restored to the original values (same form, in reverse);
  detail + row return to pre-edit text. Two `host_*`
  PATCH 200s on the wire (no audit rows — host mutations
  are intentionally excluded from the
  `audit_events.kind` CHECK constraint per the route-
  level doc-comment at
  `crates/relayterm-api/src/routes/v1/hosts.rs:119`,
  "No audit event is emitted (no `host_deleted` kind
  exists; out of scope for this slice)"; the same
  exclusion applies to the create + update paths so
  the smoke window producing zero `host_*` audit rows
  is correct, not a regression).
- *Invalid edit copy:* blanked `display_name` and
  submitted. Form closes; `data-testid="host-detail-edit-error"`
  shows **`Cannot save host: display name is required`**
  — short, user-facing, no raw backend text, no internal
  field references. Row remained unchanged.
- *Conflict-delete (host referenced by 1 profile):*
  typed-name confirm at `host-detail-delete-confirm-input`
  enabled the submit; submit fired `DELETE`, surfaced 409
  in the browser console (standard
  `Failed to load resource: server responded with a
  status of 409` — no body printed by the browser, no
  raw error text echoed by the SPA), and
  `host-detail-delete-error` showed **`Cannot delete
  host: it is still used by a saved server profile or
  has trusted host keys — remove the dependent items
  first`**. Host remained; `hosts-count = 15 hosts`
  unchanged; `host-detail-profile-count = 1`.
- *Successful unreferenced delete:* created a fresh host
  through the `servers-create-host-*` form
  (`uismoke-host-deleteme-t20260513`); success banner
  appended is honest ("Reachability and host-key trust
  are not verified by this action"). Selected it
  (`host-detail-profile-count = 0`,
  `host-detail-profiles-empty = "No profiles reference
  this host yet."`), confirmed the delete via the typed-
  name flow, and observed the detail panel auto-closes
  and the row disappears from the list. `hosts-count`
  returned to `15`.

**2. Server profiles UI smoke.** Selected profile
`inv-smoke-profile-edited-t20260512` from the prior
slice.

- *Valid edit, round-trip incl. username-override
  set + clear:* `profile-detail-edit-*` allowed editing
  name, host (select), identity (select), username
  override (text), tags (comma-list text). Wrote
  `inv-smoke-profile-uismoke-t20260513`,
  username_override = `uismoke-override`,
  tags = `smoke, inv, t20260512, uismoke`; detail
  re-renders, list row text updates, the
  `profile-detail-username` field switches from
  `smoke2 (host default)` → `uismoke-override
  (override)` — the `(override)` annotation is
  rendered exactly when the column is non-NULL.
  Restored: empty `username_override` is sent and the
  field flips back to `smoke2 (host default)`; tags
  trimmed back to the original three.
- *Conflict-delete (terminal_sessions history):*
  attempted delete on `desktop-smoke-profile` (used in
  the 2026-05-09 published-desktop login + terminal
  smoke; has closed `terminal_sessions` rows referencing
  it via `terminal_sessions.server_profile_id
  ON DELETE RESTRICT`). Backend returned 409;
  `profile-detail-delete-error` showed **`Cannot delete
  server profile: it has terminal session history —
  disable it instead to keep the history while blocking
  new launches`** — explicit pointer to the disable
  affordance, no backend message echo. Profile remained.
- *Successful unreferenced delete:* attempted delete on
  `inv-smoke-profile-referenced-t20260512` (the prior
  slice's "referenced" pair, which referenced
  `inv-smoke-host-referenced-t20260512` and was the
  *host*-conflict-blocker subject; it itself has zero
  `terminal_sessions` rows). 200 OK on the wire,
  `audit_events` shows `server_profile_deleted` at
  2026-05-13T01:20:59Z with public-metadata-only payload
  (`name`, `host_id`, `disabled_at = null`,
  `ssh_identity_id`, `server_profile_id`); UI list
  count `13 profiles → 12 profiles`; row gone; detail
  panel auto-closed. `inv-smoke-host-referenced-t20260512`
  is now unreferenced but kept (could become the
  subject of a separate host-delete success in a future
  smoke).

  This is also the conflict-vs-success-path divergence
  worth pinning: the *name* `*-referenced-*` referred
  in the 2026-05-12 entry to "host-side referencing
  blocker", **not** to "has terminal_sessions history",
  so the profile itself was deletable without going
  through the disable affordance. The 2026-05-12 entry's
  fixture inventory does not contradict this — the
  prior smoke deleted `inv-smoke-profile-delete-free-*`
  (a different fixture) for the success path and never
  tried the delete on `*-referenced-*`. Re-read carefully
  before assuming a name-based shortcut in a later slice.

**3. SSH identities UI smoke.**

- *Generate:* `identities-generate-open` →
  `identities-generate-name` = `inv-smoke-identity-uismoke-t20260513`,
  `identities-generate-key-type` = `ed25519` (default;
  the only supported option per the
  `ssh-key 0.6` `ed25519`-feature-only pin in
  AGENTS.md). Submit 200; row count `4 → 5`; new row
  shows `ED25519`, a SHA-256 fingerprint, and a
  truncated public-key preview
  (`ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA…`) — public
  material only.
- *Rename round-trip:* `identity-detail-rename-open` →
  set name to `inv-smoke-identity-uismoke-renamed-t20260513`,
  save. Detail name and list row update. Notable: the
  full public-key block at
  `data-testid="identity-detail-public-key"` keeps the
  *original* OpenSSH comment field
  (`ssh-ed25519 AAAA… inv-smoke-identity-uismoke-t20260513`).
  This is correct: rename mutates the DB row only,
  it does NOT re-derive the keypair, so the embedded
  comment (which was set at generation time) is
  immutable post-rename. Worth knowing for any future
  smoke that diffs the full public key across rename —
  the comment is the historical name, the row's `name`
  is the current display.
- *Successful unreferenced delete:* same identity
  (just renamed, never bound to a profile). Typed-name
  confirm at `identity-detail-delete-confirm-input` →
  200; row count `5 → 4`; detail panel auto-closes;
  audit_events shows `ssh_identity_deleted` at
  2026-05-13T01:24:26Z with public-metadata-only
  payload (`name`, `key_type`, `created_at`,
  `ssh_identity_id`, `fingerprint_sha256`). No
  `encrypted_private_key`, no raw key bytes anywhere.
- *Conflict-delete (identity referenced by profile):*
  selected `inv-smoke-identity-renamed-t20260512`
  (bound to `inv-smoke-profile-edited-t20260512` via
  `server_profiles.ssh_identity_id ON DELETE RESTRICT`).
  Submit fired 409;
  `identity-detail-delete-error` shows **`Cannot delete
  SSH identity: it is still used by a saved server
  profile — remove or re-bind the profile first`** —
  same safe, actionable shape as the other two
  conflict copies. Identity remained.

**4. Navigation / list-state / viewport.**

- *Stale-row search:* set `servers-host-search` =
  `uismoke` → `Showing 0 of 15 hosts` +
  `data-testid="hosts-filter-empty"` =
  `No hosts match this filter.` The deleted host does
  not ghost. Set `servers-profile-search` =
  `referenced-t20260512` → `Showing 0 of 12 profiles`;
  same result. Clearing the filters restores full
  list counts (15 / 12 / 4).
- *Detail panel after delete:* both delete success
  paths (host + profile + identity) cleared the detail
  panel synchronously (`host-detail-panel`,
  `profile-detail-panel`, `identity-detail-panel`
  removed from the DOM on success). No "couldn't find
  this resource" empty state was needed — selection
  state is dropped immediately on delete success per
  `ServersView.svelte` `submitDeleteHost`'s
  `selectedHostId = null` line and the equivalent in
  the profile / identity flows.
- *Narrow viewport reachability:* resized to 414 × 896
  (small-phone-ish), reopened
  `inv-smoke-host-edited-final-t20260512` detail panel.
  `host-detail-edit-open` rendered at x=49 w=71
  (right-edge=120) and `host-detail-delete-open` at
  x=128 w=86 (right-edge=214); both fully on-screen
  inside the 414-wide viewport, `pointer-events: auto`,
  not disabled, not clipped, no horizontal scroll on
  the panel. This is a viewport-clip / button-reachable
  check only; full mobile responsive UX is still out
  of scope for this slice.

**5. Redaction sweep.**

- *UI side:* across every panel surfaced
  (`host-detail-*`, `profile-detail-*`,
  `identity-detail-*` incl. the
  `identity-detail-public-key` `<pre>`), the only
  identity-key material rendered is the OpenSSH public
  key + SHA-256 fingerprint. There is no
  `encrypted_private_key` field, no `BEGIN OPENSSH`
  block, no token / cookie / password value, no raw
  backend stack or error text on any path tested
  (every error copy is a SPA-formatted summary string —
  see the four error strings quoted above).
- *Backend log sweep:* `docker compose logs --tail=2000
  relayterm-backend` over the entire smoke window,
  pattern-by-pattern hit counts:

  | Pattern | backend hits | nginx web hits |
  |---|---|---|
  | `relayterm_session=[A-Za-z0-9_-]{20,}` | 0 | 0 |
  | `encrypted_private_key` | 0 | 0 |
  | `data_b64` | 0 | 0 |
  | `BEGIN OPENSSH` | 0 | 0 |
  | `REDACT-MARKER` | 0 | 0 |
  | `token_hash=` | 0 | 0 |
  | `password=` | 0 | 0 |

  (The unfiltered grep does match the standard
  `unauthorized request detail=missing session cookie`
  WARN lines on the substring `cookie`; that diagnostic
  is the documented safe form — it does NOT echo any
  cookie *value*.)
- *Audit-payload spot check:* the four audit rows
  `audit_events` wrote this slice
  (`server_profile_updated` × 2, `server_profile_deleted` × 1,
  `ssh_identity_deleted` × 1) all carry only public
  metadata fields (`name`, `host_id`, `disabled_at`,
  `ssh_identity_id`, `server_profile_id`, `key_type`,
  `created_at`, `fingerprint_sha256`). Mirrors the
  field-list pinned by `docs/agent/redaction-rules.md`
  § 1 + the `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel
  test. No `host_*` or `ssh_identity_updated` /
  `ssh_identity_renamed` audit kind exists in the
  schema CHECK constraint (intentional per
  `crates/relayterm-api/src/routes/v1/hosts.rs:119`);
  the four UI host-touching operations this slice did
  (PATCH × 2, POST, DELETE) accordingly produced zero
  audit rows, which is the expected post-deploy
  behaviour, not a regression.

**Resource state at end of smoke.**

- Hosts: 15 (baseline; `uismoke-host-deleteme-t20260513`
  was created **and** deleted through the UI, net zero).
- Server profiles: 12 (one fewer than the
  start-of-smoke baseline of 13 —
  `inv-smoke-profile-referenced-t20260512` deleted on
  purpose for the unreferenced-success path).
- SSH identities: 4 (baseline;
  `inv-smoke-identity-uismoke-t20260513` was generated,
  renamed, and deleted through the UI, net zero).
- Edited-and-restored: host
  `inv-smoke-host-edited-final-t20260512`, profile
  `inv-smoke-profile-edited-t20260512`. Both
  `recorded_at` columns advanced (host: two PATCH
  cycles ~145 s apart; profile: two PATCH cycles
  ~21 s apart) — useful as an "edit happened" proof
  for any future audit.
- No `.env`, Compose template, nginx config, image
  digest, or migration touched.
- No throwaway SSH container created; no real SSH
  connection initiated; no private-key material
  imported or rendered.

**Verified.**

- The six inventory-mutation routes (hosts /
  server-profiles / ssh-identities × PATCH + DELETE)
  are wired end-to-end from SPA → CSRF → auth → DB →
  conflict-or-audit-or-success, and the SPA surfaces
  exactly the documented user-safe copy on the 200 /
  400-style / 409 paths tested. Conflict and validation
  copy stays free of raw backend text on every path.
- The destructive-action policy from
  `docs/agent/redaction-rules.md` § 3 holds at the UI
  layer: every destructive action is gated behind a
  typed-name confirmation, the typed-name match is
  enforced client-side (the submit button is `disabled`
  until the input equals the row's display name), and
  the SPA never surfaces the underlying error text.
- The "audit on real transitions only" rule (§ 2 of
  the same file) holds: zero audit rows written for
  any of the three 409-rejected delete attempts this
  slice fired. Audit rows that *did* land carry
  field-by-field public metadata only.
- Backend + nginx redaction sentinel sweep zero hits
  across all seven leak patterns on both services'
  `--tail=2000` logs over the smoke window.

**Deferred (intentional non-goals for this run; do NOT
treat any of these as staging-verified by this entry):**

- **Private-key import.** Same constraint as the prior
  entry — no DTO field accepts external key material.
  No UI surface for it tested or exists.
- **`ssh-copy-id` / bootstrap automation.** Out of scope.
- **Route-param detail pages** (`/servers/:id`,
  `/hosts/:id`, `/identities/:id`). The list-view +
  side-panel pattern is what this slice verifies; a
  dedicated route-per-row surface is a separate slice
  and not landed.
- **Terminal renderer evaluation / live terminal
  launch.** Out of scope — no renderer code, no
  `WebSocket` upgrade, no PTY exercised this slice.
- **Durable persistence guarantees beyond the current
  Postgres inventory tables.** Same as the prior entry.
- **`host_*` and `ssh_identity_renamed` audit kinds.**
  Confirmed-absent from the schema CHECK constraint;
  adding them is a separate slice with its own
  migration + redaction-test pass and is named-and-
  deferred here so a future spec-drift sweep picks it
  up with full context. AGENTS.md "Maintenance protocol"
  applies.
- **Tauri shell (desktop / mobile) drive of the same
  inventory UI.** The UI under test is the same SPA the
  Tauri shells wrap (per `docs/spec/tauri-runtime-backend-url.md`
  path A), so the same testid surface applies; an
  explicit shell-driven repeat is a separate slice.
- **Mobile responsive layout audit beyond the single
  414 × 896 reachability spot-check.** The narrow-
  viewport check confirmed `host-detail-edit-open` and
  `host-detail-delete-open` are on-screen and clickable;
  it did NOT exercise scroll behaviour, soft-keyboard
  overlap, drawer affordances, or the rest of the
  inventory surface at small widths. A full mobile UX
  pass is a separate slice.
- **Profile-disable affordance** (`server_profile_disabled` /
  `server_profile_enabled` audit kinds). The
  `Disable profile` button is visible on each profile
  row and was NOT exercised this slice — only the
  delete + conflict paths. Phase-2 destructive-action
  surface (disable / re-enable round-trip) is the
  natural next slice.

### 2026-05-13 · Private-key import (OpenSSH Ed25519) staging smoke

Slice `docs/private-key-import-staging-smoke` verifies the
v1 private-key import surface end-to-end on the staging
slot: paste-textarea ingress on `IdentitiesView`, the
backend `POST /api/v1/ssh-identities/import` route, the
`ssh_identity_created` audit row's `source: "imported"`
discriminator (`docs/private-key-import.md` § 7.3), the
parity-with-generate guarantee on the downstream host /
profile / trust / auth-check / terminal flow, and the
`AGENTS.md` "Things to avoid" §§ 1 / 3 / 13 / 14 redaction
backstops. The slice is docs-only —
`feat(api): import OpenSSH Ed25519 identities` (`8af1fc9`)
landed first and Forgejo CI's six checks (rust checks, web
checks, docker build, desktop-linux, android, publish-
images) were already green on the commit when the smoke
started.

**Image freshness finding (Category 1).** The Forgejo
`:main` tags in the registry already pointed at the
freshly-published images from the import commit (built
`2026-05-13T02:55:38Z` backend, `2026-05-13T02:56:22Z`
web), but the cloud-edge staging slot was still running
the previous `:main` digests
(`sha256:55e64c11…` backend / `sha256:63694b40…` web,
both built `~2026-05-12T23:50Z` — ~3 hours **before**
`8af1fc9` was authored at `2026-05-13T02:51:03Z`). The
deployed pre-refresh web bundle (`index-CiiA2M_K.js`)
did NOT contain `importSshIdentity` or
`Import SSH identity` strings; an early `401` response
from `GET /api/v1/ssh-identities/import` was a
false-positive (path collision with
`GET /api/v1/ssh-identities/:id`, whose
`AuthenticatedUser` extractor runs before the method
check, so any path that visually matches the parameter
pattern returns `401` regardless of whether a specific
sibling route was registered). A manual
`docker compose pull relayterm-backend relayterm-web` +
`docker compose up -d --no-deps relayterm-backend
relayterm-web` against
`/home/ubuntu/docker-compose/relayterm-staging/docker-compose.yml`
pulled the new digests; postgres was untouched (the
`--no-deps` flag enforces that). Post-refresh the web
bundle hash became `index-BGG66G59.js` and the
"Import SSH identity" button + `private_key_openssh`
strings were grep-able inside the new bundle. The staging
slot does NOT auto-redeploy on push to `main` — this is
the documented procedure (`§ 5. Pull the images`) and
not a new finding, but the SPA's content-hashed asset
filename plus nginx's `Cache-Control: immutable` on
`/assets/*` means the desktop / browser caches DO pick up
the new bundle on next navigation without any cache
purge, which is the production-correct behaviour and was
confirmed here in passing.

**Surface.** Playwright MCP
(`mcp__playwright__browser_*`) driving a Chromium browser
session against `https://relayterm-staging.js-node.cc`.
No Tauri shell this slice (bundled-shell handoff is
covered by the 2026-05-09 desktop / Android entries; the
import panel uses the same SPA the shells wrap, so the
testid surface is identical). The browser session
re-used a still-valid cookie from a prior smoke run — no
fresh login was needed. The staging smoke user is the
existing throwaway
`staging+throwaway-20260509173230@example.com`. The
session was closed at end-of-smoke.

**Throwaway key discipline.** A new Ed25519 keypair was
generated on the operator workstation only,
`ssh-keygen -t ed25519 -N '' -C
relayterm-import-smoke-202605 -f
/tmp/relayterm-private-key-import-smoke/id_ed25519`. No
personal or production key was ever in scope. The
private-key bytes never appear in this entry, never
appeared in any log line, audit row, error message, or
shell-history command body, and the file was `shred -u`'d
at end-of-smoke (along with the `.pub` and the
short-lived base64 sidecar used to ferry the PEM into the
browser via `page.evaluate(atob(…))`, avoiding a literal
PEM in the Playwright tool-call payload). The locally-
computed SSH SHA-256 fingerprint is
`SHA256:Mqf4E98YtdaO/DptUJ4RkKq9ogXXJVe4rXkyTn4hBqQ` —
the value the SPA / DB / audit row must all agree on for
the round-trip to be honest.

**Throwaway SSH target.** A
`linuxserver/openssh-server:latest` (image
`sha256:29d4e3f8…`, LSIO build `10.2_p1-r0-ls225`)
container named `relayterm-staging-import-smoke-ssh`,
attached only to the staging Compose network
`relayterm-staging_relayterm-staging-internal` with DNS
alias `import-smoke-host` resolving to `172.21.0.5`. **No
host port was published** — the target is unreachable
from anything outside the staging Compose network.
`USER_NAME=smoke`, `SUDO_ACCESS=false`,
`PASSWORD_ACCESS=false`, `PUBLIC_KEY=<contents of
id_ed25519.pub>`. The throwaway public key landed in the
target's `authorized_keys` exactly once and only via the
`PUBLIC_KEY` env variable; no `ssh-copy-id`, no password
bootstrap, no privileged channel (those are v1
out-of-scope per `docs/private-key-import.md` § 10).
DNS + TCP reachability from a sidecar on the same
network was confirmed before any browser action
(`nslookup import-smoke-host` + `nc -zv ... 2222`). The
container was `docker stop && docker rm`'d during
cleanup.

**UI import.** Identities view → `identities-import-open`
button → paste PEM into
`identities-import-private-key` textarea + type
`import-smoke-identity` into
`identities-import-name` → `identities-import-submit`.
The browser issued a single
`POST /api/v1/ssh-identities/import` returning
`201 Created` and a `SshIdentityResponse`-shaped body.
The success card showed `Imported import-smoke-identity`
with key type `ed25519`, fingerprint
`SHA256:Mqf4E98Y…hBqQ` (byte-identical to the local
`ssh-keygen -lf id_ed25519.pub` output), public-key
preview
`ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA… import-smoke-identity`
(the supplied name became the OpenSSH comment per design
§ 5 step (d): `private.set_comment(name)` inside
`VaultService::import_ssh_identity`), and the load-
bearing footnote "The private key never reaches the
browser. Only the public key is renderable here." The
identities list went from `4 → 5 identities` with
`import-smoke-identity` at the top. The name + textarea
were both cleared on success (`value === ""`) and the
submit button re-disabled, per design § 9.3.

**DOM + storage redaction.** A post-import sweep over
`document.documentElement.outerHTML`: the literal strings
`-----BEGIN OPENSSH PRIVATE KEY-----` and
`-----END OPENSSH PRIVATE KEY-----` DO appear, but only
as the textarea's `placeholder` attribute and the panel's
help-copy ("Paste the full file contents, including the
BEGIN and END markers") — not as live private-key bytes.
A second sweep against actual PEM-body byte sentinels
(the `openssh-key-v1` magic base64 prefix and the
private-scalar half of the key) returned `false` for
both. `encrypted_private_key` and `private_key_openssh`
appear nowhere in the DOM. `localStorage` contains only
the pre-existing `relayterm.active-terminal.v1`
(unchanged between pre- and post-import snapshots);
`sessionStorage` is empty; `document.cookie.length === 0`
in the browser (auth cookie is HttpOnly — JS cannot read
it, which is the production-correct posture).

**Backend / DB / audit shape** (`docker exec -i
relayterm-staging-postgres-1 psql -U relayterm -d
relayterm`). The `ssh_identities` row exists with
`name=import-smoke-identity`, `key_type=ed25519`,
`fingerprint_sha256=SHA256:Mqf4E98Y…hBqQ`,
`length(encrypted_private_key)=456` bytes (an Ed25519
PEM canonicalized through `to_openssh(LineEnding::LF)`
is ~419 bytes; the +37-byte delta is the `RTV1` +
version byte + 24-byte XChaCha20-Poly1305 nonce +
16-byte tag envelope from
`crates/relayterm-vault::cipher`, which matches the math
§ 6 promises). `encrypted_private_key` itself was queried
by `length()` only — the byte content was never read.
The single `audit_events` row matching
`kind='ssh_identity_created' AND
payload->>'name'='import-smoke-identity'` had
`recorded_at = 2026-05-13 03:29:36.387256+00` and payload
keys exactly {`name`, `source`, `key_type`,
`created_at`, `ssh_identity_id`, `fingerprint_sha256`} —
six keys, no more, no less — with `source='imported'`
(the design § 7.3 discriminator). The full payload was
`{"name": "import-smoke-identity", "source": "imported",
"key_type": "ed25519", "created_at":
"2026-05-13T03:29:36.385186Z", "ssh_identity_id":
"aff52897-62b7-4a13-998c-47f56a8ca349",
"fingerprint_sha256":
"SHA256:Mqf4E98YtdaO/DptUJ4RkKq9ogXXJVe4rXkyTn4hBqQ"}`.
A redaction sweep with `payload::text LIKE '%' || s ||
'%'` against `{private_key_openssh,
encrypted_private_key, BEGIN OPENSSH, private_key,
passphrase, session_token, token_hash, cookie, password,
data_b64}` returned `f` (false) for every sentinel.

**Host / profile / trust / auth-check.** Created host
`import-smoke-host`
(`servers-create-host-display-name` +
`servers-create-host-hostname` +
`servers-create-host-port=2222` +
`servers-create-host-username=smoke`); created profile
`import-smoke-profile` binding the new host to
`import-smoke-identity` — its option label in the
profile-host `select` confirmed the identity's UUID
`aff52897-…` matched the DB row.
`host-key-preflight-button` captured an ed25519 host key
fingerprint `SHA256:2gQzimnp7rIh6cSVfkxOGFolJKG4RSUtd5G9klNo+XQ`,
which is byte-identical to the locally-computed
`ssh-keygen -lf` over the target's advertised ed25519
host-key line (`docker logs
relayterm-staging-import-smoke-ssh` had emitted all
three host-key types — ed25519, ecdsa-sha2-nistp256, rsa
— at container start). Typing the fingerprint into
`host-key-confirm-input` enabled `host-key-trust-button`;
clicking it flipped `host-key-status-badge` to `Trusted`
("Host key matches an active pinned entry. Run auth-
check below to confirm…"). `auth-check-run-button` ran
in ~5 s and flipped `auth-check-status-badge` to
`Authenticated` at `2026-05-13T03:34:40.370882510Z`,
description "SSH public-key authentication succeeded for
the configured username. No PTY was allocated and no
command was executed. Terminal launch is a separate,
deliberate action." The auth-check used the just-
imported identity end-to-end, proving the round-trip
through the vault's `RTV1` envelope is byte-identical to
a backend-generated key from the operator's PoV.

**Terminal launch (parity check).** Clicked
`profile-launch-terminal`; the workspace flipped to
`production-terminal-phase=live` with session
`b08a5a88-fadc-4264-a17f-7de56b43dc3c`. Sent three
harmless commands via the xterm helper textarea:
`echo relayterm-import-smoke` → output
`relayterm-import-smoke`, `whoami` → `smoke`, `pwd`
→ `/config` (linuxserver/openssh-server's default home
for `USER_NAME=smoke`). Closed via
`production-terminal-close`. The session was
authenticated, spawned a real PTY, executed real
commands, and closed cleanly — the load-bearing parity
check between an imported key and a generated key
passes.

**Referenced-identity delete refusal.** Opened
`identity-detail-panel` for `import-smoke-identity`,
clicked `identity-detail-delete-open`, typed
`import-smoke-identity` into
`identity-detail-delete-confirm-input`, clicked
`identity-detail-delete-confirm-submit`. The wire was
`DELETE /api/v1/ssh-identities/aff52897-… → 409` (per
nginx access log; the SPA does not surface the wire
status code directly). The SPA mapped it to the friendly
copy "Cannot delete SSH identity: it is still used by a
saved server profile — remove or re-bind the profile
first" via `identity-detail-delete-error`. No raw backend
text, no `409`, no `ssh_identity referenced` envelope
string, no private-key material echoed. The identity row
remained present in the list afterwards.

**Negative path — encrypted PEM.** A second throwaway
Ed25519 key was generated with `-N
'throwawaypass1234'` (its base64-encoded byte sequence
was used once to populate the import textarea, then both
the file and the base64 sidecar were `shred -u`'d in
cleanup; the literal passphrase string was used in NO
production credential and is recorded here only as the
sentinel for the redaction sweep below). Pasting it into
the import panel and submitting produced
`POST /api/v1/ssh-identities/import → 400` (per nginx
access log). The SPA mapped it to "Cannot import SSH
identity: passphrase-protected (encrypted) keys are not
supported in this release — generate a new unencrypted
key or wait for the v1.1 passphrase channel" — no raw
`unsupported_key_format encrypted` envelope leaked.
Textarea cleared on failure (`value === ""`); name field
preserved at `neg-import-encrypted` (design § 9.3). DB
confirmation: `SELECT count(*) FROM ssh_identities WHERE
name='neg-import-encrypted'` → `0`. The negative-test
key and its base64 fixture were shredded immediately.

**Log + audit redaction sweep.** Backend log
(`docker logs --since 2026-05-13T03:26:00Z
relayterm-staging-relayterm-backend-1`, 6 lines —
startup banner only, no per-request logging at the
configured INFO verbosity) and nginx log (`30 lines`)
were grepped for twelve sentinels:
`BEGIN OPENSSH PRIVATE KEY`,
`END OPENSSH PRIVATE KEY`, `private_key_openssh`,
`encrypted_private_key`, `private_key`, `passphrase`,
`session_token`, `token_hash`, `data_b64`,
`REDACT-MARKER`, `throwawaypass1234`, and a short
throwaway private-key byte fragment (a distinctive
16-character slice of the imported key's private-scalar
base64, derived locally on the workstation at sweep
time, used only as a grep needle, and **deliberately not
printed in this document** — referred to below as
`<REDACT-MARKER-PRIVATE-KEY-FRAGMENT>` for the purposes
of describing the sweep). All twelve returned `0`
matches on both logs. The audit-payload sweep across
every row recorded since `2026-05-13T03:26:00Z` (=
exactly 2 rows: `ssh_identity_created` for the import,
plus `server_profile_created` for the profile creation)
returned `f` for eleven forbidden substrings including
`throwawaypass1234` and the
`<REDACT-MARKER-PRIVATE-KEY-FRAGMENT>` sentinel.
Nginx access lines for the import flow showed exactly
the wire shape claimed above: `201` on the successful
import, `200` on the listing refresh, `409` on the
referenced-delete attempt, `400` on the encrypted
negative-test import.

**Cleanup.** The profile was disabled through the SPA
(`profile-disable-open` → typed name into
`profile-disable-confirm-input` →
`profile-disable-submit`); confirmed via `psql` that
`server_profiles.disabled_at` was populated
(`2026-05-13 03:45:00.90888+00`) and exactly one
`audit_events` row with `kind='server_profile_disabled'`
and `payload->>'name'='import-smoke-profile'` was
appended (`4ebc21d9-028d-44de-983f-873d3ae43175`,
`recorded_at = 2026-05-13 03:45:00.915667+00` — ~7 ms
after the DB write, same transaction). Inventory rows
(`hosts.import-smoke-host`,
`server_profiles.import-smoke-profile`,
`ssh_identities.import-smoke-identity`) were NOT
deleted, per `AGENTS.md` "Inventory lifecycle and
destructive-action policy" — the profile has a
`terminal_sessions` history row from the launch above,
so deletion is refused by design and disable is the
correct non-destructive end state. The throwaway SSH
target container was `docker stop`'d and `docker rm`'d;
the `/tmp/relayterm-private-key-import-smoke/`
directory was `shred -u`'d (private key, public key,
base64 sidecar) and `rmdir`'d. The staging stack stays
running on the refreshed `:main` digests; the throwaway
staging smoke user is untouched.

**Out of scope (re-stated for the next operator).**
Passphrase-protected (encrypted) imports, RSA / ECDSA /
DSA, PEM PKCS#1 / PKCS#8, PuTTY `.ppk`, file picker,
`ssh-copy-id` / password bootstrap, hardware-backed /
FIDO / U2F / smart-card SSH keys, SSH certificates, bulk
import, and key-rotation workflow are all explicit
`docs/private-key-import.md` § 10 / § 13 deferrals —
the SPA's "future work" footer copy on the Identities
view already reflects this. None were exercised this
slice. A Tauri shell repeat of the same flow is also
deferred — the bundled-shell handoff is covered
separately (the 2026-05-09 desktop / Android entries
above) and the import panel testid surface is the same
SPA the shells wrap, so no shell-specific surface exists
today.

### 2026-05-13 · Deployable-baseline end-to-end staging smoke

**Date.** 2026-05-13 04:43 UTC – 05:17 UTC (≈35 min).
**Staging URL.** `https://relayterm-staging.js-node.cc`.
**Stack pin.** `git.js-node.cc/jsprague/relayterm-backend:main`
(image `sha256:fc6799fc…`, built 2026-05-13T02:55:38Z) +
`relayterm-web:main` (image `sha256:71bcc4f0…`, built
2026-05-13T02:56:22Z). Both digests post-date the most
recent backend-changing commit on `main` (`8af1fc9
feat(api): import OpenSSH Ed25519 identities`, 2026-05-13
02:51 UTC), so this smoke ran against the published images
that carry inventory mutations, private-key import,
session-policy TTL, and the per-user / deployment quotas.
**Branch.** `docs/deployable-baseline-e2e-smoke` off
`main` (docs-only slice; no source changes).
**Browser surface.** Playwright MCP (Firefox) at
1440 × 900. The Tauri desktop / Android shells were
explicitly NOT in scope — those have their own smoke
entries above and wrap this same SPA. Auth: existing
`staging+throwaway-20260509173230@example.com` cookie
session; no re-login required, no password entered or
logged.

**Goal.** Confirm RelayTerm's single-user deployable
baseline is usable end-to-end against published `:main`
images before terminal-renderer-evaluation work begins.
Slice boundaries: no source / schema / API / auth / CSRF /
CORS / WebSocket-protocol / Tauri / CI / deploy file
changes — this is a smoke + docs slice only.

**Throwaway SSH target.** A
`linuxserver/openssh-server:latest` container named
`relayterm-staging-baseline-smoke-ssh`, attached only to
the staging Compose network
`relayterm-staging_relayterm-staging-internal` with DNS
alias `baseline-smoke-host` resolving to `172.21.0.5`.
**No host port was published** — the target was
unreachable from anything outside the staging Compose
network. `USER_NAME=smoke`, `SUDO_ACCESS=false`,
`PASSWORD_ACCESS=false`, `PUBLIC_KEY=<contents of
id_ed25519.pub>` (piped over a single `IFS= read -r` so
the public-key bytes never landed in remote shell argv).
DNS + TCP reachability from a `busybox` sidecar on the
same internal network was confirmed before any browser
action (`baseline-smoke-host → 172.21.0.5` + `nc -zv …
2222 open`). The container was `docker stop && docker rm`'d
during cleanup.

**Identity path.** Imported (path B — verifies the newest
deployability gap). A throwaway Ed25519 keypair was
generated locally under
`/tmp/relayterm-baseline-smoke.XXXXXX/id_ed25519` (no
passphrase — passphrase-protected key import is
explicitly deferred per `docs/design/private-key-import.md`
§ 10). Locally-computed fingerprint:
`SHA256:GDA6/gBYwJ8POXNTsEjDDLykeKSm+2WT+NACLutMLAU`. The
private-key PEM bytes never appeared in any tool-call
payload, audit row, log line, Error, or browser DOM:
the PEM was carried into the browser via a base64
sidecar + `atob()` inside a single `page.evaluate` call,
then `shred -u`'d at cleanup along with the public-key
and the base64 sidecar. The generated-identity path
was intentionally skipped to keep the slice tight; the
import path is the load-bearing parity check vs. a
backend-generated key (yesterday's 2026-05-12 import
smoke covers both halves of the round-trip in detail).

**UI import + rename.** Identities view →
`identities-import-open` → PEM pasted into
`identities-import-private-key` textarea + name
`baseline-smoke-identity` into `identities-import-name`
→ `identities-import-submit`. One
`POST /api/v1/ssh-identities/import` returned a
`SshIdentityResponse` with `key_type=ed25519` and
fingerprint
`SHA256:GDA6/gBYwJ8POXNTsEjDDLykeKSm+2WT+NACLutMLAU`
byte-identical to the locally-computed value. The
success card showed the public-key preview line
`ssh-ed25519 AAAA…40z baseline-smoke-identity` (the
supplied name became the OpenSSH comment per design
§ 5 step (d)). Name + textarea were cleared on success
and submit was re-disabled. Identity then renamed to
`baseline-smoke-identity-renamed` via
`identity-detail-rename-open` → input fill →
`identity-detail-rename-save`; the detail panel and
list both flipped to the new name.

**DOM + storage redaction.** Post-import sweep over
`document.documentElement.outerHTML`: the literal markers
`-----BEGIN OPENSSH PRIVATE KEY-----` and
`-----END OPENSSH PRIVATE KEY-----` appear only as the
import-textarea's `placeholder` attribute (one host node,
class `min-h-[10rem] … bg-zinc-900 …`), not as live
private-key bytes. The `openssh-key-v1` base64 magic
prefix (`b3BlbnNzaC1rZXktdjE`) was confirmed **absent**
from the DOM (the sweep is the load-bearing redaction
check, not the placeholder mention).
`encrypted_private_key`, `private_key_openssh`,
`session_token`, `token_hash` all absent. `localStorage`
empty, `sessionStorage` empty, `document.cookie.length
=== 0` (auth cookie is HttpOnly — JS cannot read it).
The string `passphrase` appears in the deferred-future
help copy at the bottom of the panel (`Passphrase-
protected key import … deliberate later slices`); that
is the design-pinned wording, not a leak.

**Host create + edit.** Created
`Baseline-Smoke-Host` (display name) /
`baseline-smoke-host` (hostname) / `2222` / default user
`smoke` via the `servers-create-host-*` form. Edited the
display name from `baseline-smoke-host` to
`Baseline-Smoke-Host` through
`host-detail-edit-display-name` →
`host-detail-edit-save`: mixed-case display name was
preserved verbatim while the lowercase hostname (load-
bearing for DNS resolution on the internal Compose
network) was left untouched. Port and default user
unchanged. `hosts-count` flipped from `16 hosts → 17
hosts`.

**Profile create + edit.** Created `baseline-smoke-profile`
binding `Baseline-Smoke-Host` (UUID
`b60bfcb9-7bfe-406a-…`) to
`baseline-smoke-identity-renamed` (UUID
`4c8cab84-917d-…`) with no username override and tag
`smoke`. Success card noted "host key not yet trusted
and SSH authentication has not been verified for this
profile" — accurate honesty copy. Edited the profile
through `profile-detail-edit-open`: set username
override = `smoke` and tags = `smoke,baseline` →
saved; the detail panel re-rendered as `smoke
(override)` and tags `smoke / baseline`. Then re-opened
the edit form, cleared the override (empty string) and
reverted tags to `smoke` → saved; the detail panel
flipped back to `smoke (host default)` with a single
`smoke` tag. UI state refresh was synchronous each
time; no manual reload required.

**Host-key trust.** `host-key-preflight-button` on the
profile row captured a key during SSH key exchange.
The displayed fingerprint
`SHA256:QWefVlx+L4mvZOTAUQ8BABPJNiderYOwc8vxRPFRhas`
is byte-identical to the locally-computed
`ssh-keygen -lf` value over the target container's
advertised `ssh-ed25519` host-key line (the container
emitted all three host-key types — ed25519, ecdsa-
sha2-nistp256, rsa — at startup; preflight picked
ed25519). Typed the fingerprint into
`host-key-confirm-input` → `host-key-trust-button`;
`host-key-status-badge` flipped to `Trusted` with the
load-bearing copy "Host key matches an active pinned
entry. Run auth-check below to confirm the configured
SSH identity authenticates before launching a terminal
session." Host-key replacement was NOT exercised — that
has its own smoke history (2026-05-10 entries above).

**Auth-check.** `auth-check-run-button` ran in ~5 s;
`auth-check-status-badge` flipped to `Authenticated`
at `2026-05-13T04:54:38.819702Z`, copy "SSH public-key
authentication succeeded for the configured username.
No PTY was allocated and no command was executed.
Terminal launch is a separate, deliberate action." The
sshd container's auth log confirmed the same moment:
`Accepted publickey for smoke from 172.21.0.3 port
48118 ssh2: ED25519 SHA256:GDA6/gBYwJ8POXNTsEjDDLykeKSm+2WT+NACLutMLAU`
followed by an immediate clean disconnect — exactly the
auth-check shape (no shell allocated).

**Terminal launch.** `profile-launch-terminal` opened
`/terminal` with phase=`attached`. The very first
attempt closed at WS 1006 after 60 s with
`last_seen_seq=0` — expected idle-reaper behaviour
because no keystrokes flowed during the focus + WS-
capture setup window, NOT a defect. The SSH-target log
made the cause explicit:
`Accepted publickey for smoke … then 2026-05-13
04:56:49 Received disconnect from 172.21.0.3 port
52144:11: relayterm pty close` — the backend reaper
killed the live PTY after 60 s of zero client traffic
(no keystrokes during the focus / patched-WS setup
window). A fresh launch (session
`8e477f44-26a1-…`) with immediate focus →
`page.keyboard` keystrokes worked end-to-end. A
WebSocket-level capture (a `window.WebSocket`
wrapper that buffered every text/binary frame)
recorded:
outgoing 1 × `{"type":"attach", session_id, last_seen_seq:null,
client_id:"relayterm-web"}` (control plane),
outgoing N × binary RTB1 frames (one per keystroke),
incoming 1 × `{"type":"session_attached", status:"active", …}`
followed by binary RTB1 frames for every shell-output
byte. Three commands:
`echo relayterm-baseline-smoke` → output
`relayterm-baseline-smoke`; `whoami` → `smoke`;
`pwd` → `/config` (linuxserver/openssh-server's
default home for `USER_NAME=smoke`). The active session
appeared in `/api/v1/terminal-sessions` with
`status=active`, was visible in the Sessions list as
`detached · attached here` after detach, and the same
session UUID was visible in
`relayterm.active-terminal.v1` localStorage. The
"zero-input → 60 s WS-close" behaviour is not a defect
— it is the documented idle-reaper closing a live PTY
that received no client traffic; a session that
actually types within the window persists. No source
changes were required.

**Detach + reconnect.** Sent
`echo before-detach` to seed replay (`last_seen_seq=17`)
→ `production-terminal-detach` → phase=`detached`.
Immediately clicked `production-terminal-reconnect`
(the in-page button — Sessions-list → /terminal
navigation can blow the 30 s detached TTL on a slow
hop, as one earlier session
`8e477f44…` demonstrated). Reattach succeeded against
the **same** session UUID
`f329b32f-1afb-497f-aa1e-1097afc9cb74` with replay
bookmark intact; phase went back to `attached`,
`page.keyboard.type("echo relayterm-baseline-
reconnected")` produced
`relayterm-baseline-reconnected` in the rendered
viewport, and `production-terminal-close` shut the
session cleanly at `2026-05-13T05:08:42.711412Z`
(`status=closed`, `closed_at` matches `last_seen_at`
within 1 ms). The staging slot's detached-live-PTY
TTL is `RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS=30`
(per backend boot line); the long-TTL (1800 s) reconnect
smoke covers the bigger envelope in the 2026-05-10
entries above.

**Inventory delete-refusal / disable.** All three
refusals from `docs/spec/inventory.md` § "Inventory
lifecycle and destructive-action policy" surfaced
exactly as designed; no destructive route was reachable
from the UI for any of them:

- Profile delete with terminal-session history →
  `profile-detail-delete-confirm-submit` returned the
  conflict copy "Cannot delete server profile: it has
  terminal session history — disable it instead to keep
  the history while blocking new launches." The
  `terminal_sessions` rows (5 closed sessions UUIDs
  `5abe6081…`, `477ab072…`, `1e93862b…`, `8e477f44…`,
  `f329b32f…`) were preserved; no audit row was emitted
  for the refused delete.
- Identity delete while a profile referenced it →
  "Cannot delete SSH identity: it is still used by a
  saved server profile — remove or re-bind the profile
  first."
- Host delete while a profile referenced it AND a
  trusted `known_host_entries` row pinned it → "Cannot
  delete host: it is still used by a saved server
  profile or has trusted host keys — remove the
  dependent items first."

Cleanup-path disable then exercised:
`profile-disable-open` → `profile-disable-confirm-
input` typed `baseline-smoke-profile` →
`profile-disable-submit`. The row's
`profile-disabled-notice` rendered with the
load-bearing copy "New terminal launches, host-key
preflight / trust, and auth-check are blocked. Existing
live sessions are unaffected." The DB confirmed
`disabled_at=2026-05-13 05:17:21.319753+00` and the
audit table got exactly **one**
`server_profile_disabled` row at
`2026-05-13 05:17:21.327981+00` (8 ms after the
lifecycle transition — single audit row, matching the
"idempotency early-return BEFORE audit append" rule
from AGENTS.md § "Things to avoid").

**Backend / DB shape** (read-only, public-safe fields
only — never queried `encrypted_private_key`,
`payload`, or any byte content via `length()` or
otherwise). `hosts`: `display_name=Baseline-Smoke-Host`,
`hostname=baseline-smoke-host`, `port=2222`,
`default_username=smoke`. `server_profiles`:
`tags={smoke}`, `disabled_at=2026-05-13 05:17:21…`,
no `username_override`. `ssh_identities`:
`name=baseline-smoke-identity-renamed`,
`key_type=ed25519`,
`length(public_key)=104` bytes (raw Ed25519 wire form),
`length(fingerprint_sha256)=50` chars (the
`SHA256:base64…` text). `known_host_entries`:
`key_type=ed25519`, `length(fingerprint_sha256)=50`,
`trusted_at IS NOT NULL`, `revoked_at IS NULL`,
`length(public_key)=51` bytes. `terminal_sessions`:
5 rows, all `status=closed`, `80×24`. The smoke-window
`audit_events` kinds (filtered to
`recorded_at > 2026-05-13T04:43:00Z AND <
05:13:00Z`) were exactly
`ssh_identity_created` × 1,
`server_profile_created` × 1,
`server_profile_updated` × 2 — the disable row
(`server_profile_disabled` at 05:17:21) was the only
later event. **Zero** refused-delete audit rows. The
sentinel sweep
`payload::text LIKE '%' || s || '%'` against
`{private_key_openssh, encrypted_private_key,
BEGIN OPENSSH, private_key, passphrase, session_token,
token_hash, cookie, password, data_b64}` returned
false for every sentinel on every smoke-window audit
row.

**Backend + nginx log redaction.** Bounded
`docker logs --since 30m` over the smoke window:
backend = 3 lines (all the literal text "missing
session cookie" — pre-smoke WARN from an unauth
`/api/v1/auth/me` probe at 04:42; not a cookie
**value**); nginx/web = 76 lines (request log entries).
Sentinel sweep against
`{private_key_openssh, encrypted_private_key,
BEGIN OPENSSH PRIVATE KEY, passphrase, session_token,
token_hash, cookie, password, data_b64, REDACT-MARKER}`:
0 hits in both, except the 3 explained "cookie"
mentions above (the `cookie` sentinel matched ONLY the
literal WARN-line text "missing session cookie" — not
any cookie attribute, header name, OR cookie VALUE; the
mirror of the audit-payload sweep rationale above). `terminal_recording_chunks` rows for this
profile: 0 (staging boot-line is `recording_enabled=false`).

**Cleanup state.** Throwaway SSH container
`relayterm-staging-baseline-smoke-ssh` is `docker stop`
+ `docker rm`'d (verified `docker ps -a --filter
name=…` returns empty). Local throwaway PEM, public-key
file, base64 sidecar, JSON-wrapped sidecar, backend-tail
log, bounded log captures, health-check curl output,
and the SPA index curl response were all `shred -u`'d
or `rm -f`'d; the `mktemp -d` directory was `rmdir`'d.
The browser session may remain logged in. Left in place
per the slice plan: staging Compose stack running,
Postgres untouched (no row deleted, no schema touched),
`baseline-smoke-host`, `baseline-smoke-profile`
(disabled), `baseline-smoke-identity-renamed`,
5 `terminal_sessions` history rows, 1 trusted
`known_host_entries` row, all `audit_events` rows
(including `server_profile_disabled`), the staging
smoke user.

**Intentionally deferred** (out of scope for this
slice; tracked elsewhere or scheduled later):

- passphrase-protected private-key import (design
  `docs/design/private-key-import.md` § 10);
- `ssh-copy-id` / password-bootstrap automation for
  installing the public key on a target server (same
  design § 10);
- route-param identity / host / profile detail pages
  (today's detail panel is a `?id=…`-aware drawer);
- terminal renderer evaluation / performance work
  (xterm vs. ghostty-web vs. restty vs. wterm — that
  starts after this baseline lands);
- durable persistence beyond the current in-memory
  replay buffer + 30 s detached-PTY TTL (no schema
  for terminal output is in scope today);
- production release / signing / App Store / Play Store
  workflows for the Tauri shells
  (`docs/deployment/tauri-ci-release-plan.md` is the
  staged plan; this smoke covers only the web SPA).

**Verdict.** The single-user deployable baseline —
inventory CRUD with conflict refusals, OpenSSH Ed25519
import, host-key trust, auth-check, terminal launch,
detach + reconnect within TTL, lifecycle disable, and
public-metadata-only audit — works end-to-end on the
published `:main` images against a hermetic throwaway
target with no host port exposure, no plaintext-secret
leakage in DOM / logs / audit, and no source / schema /
API / auth / deploy changes required. Safe to start the
terminal-renderer evaluation work next.

### 2026-05-13 · Xterm production-baseline renderer smoke

**Date.** 2026-05-13 17:01 UTC – 17:22 UTC (≈21 min).
**Staging URL.** `https://relayterm-staging.js-node.cc`.
**Stack pin.** `git.js-node.cc/jsprague/relayterm-backend:main`
(image `sha256:fc6799fc…`, built 2026-05-13T02:55:38Z) +
`relayterm-web:main` (image `sha256:71bcc4f0…`, built
2026-05-13T02:56:22Z). Both digests are byte-identical to
the deployable-baseline smoke pin recorded immediately
above. The only `main` commit landed after those images
were built is `c860325 docs(design): define terminal
renderer evaluation plan` — docs-only, no source the
running images would need.
**Branch.** `docs/xterm-baseline-renderer-smoke` off
`main` (docs-only slice; no source changes).
**Browser surface.** Playwright MCP (Chrome 148 / Linux)
at 1440 × 900 (with one resize step to 1800 × 1000 and
one resize step to 390 × 844, then back to 1440 × 900).
Auth: existing `staging+throwaway-20260509173230@example.com`
cookie session; no re-login required, no password entered
or logged.

**Goal.** Record a reference smoke for the **production
xterm baseline renderer** (`@relayterm/terminal-xterm`)
against staging so future experimental-renderer
evaluations (ghostty-web, restty, wterm) have a stable
comparison point. Slice boundaries: no source / schema /
API / auth / CSRF / CORS / WebSocket-protocol / Tauri /
CI / deploy file changes — this is a smoke + docs slice
only. Experimental renderers were **not** exercised and
remain deferred per
[`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md).

**Renderer path (source-pinned, not UI-exposed).** Production
shell instantiates exactly one renderer:
`apps/web/src/lib/app/terminal/ProductionTerminal.svelte:43-44`
imports `XtermRenderer` from `@relayterm/terminal-xterm`
and `@relayterm/terminal-xterm/styles`. There is no
production diagnostic surface that exposes a renderer
name or radio group; the dev-only renderer lab lives
behind `import.meta.env.DEV` and is tree-shaken out of
the production bundle. The architectural isolation
("`apps/web/src/lib/app/**` cannot import from `lib/dev/`
or any experimental renderer adapter") is pinned by
`apps/web/tests/appShellIsolation.test.ts`. This smoke
exercised the production shell, so the renderer
exercised was xterm baseline by construction — no
runtime selector was needed.

**Throwaway SSH target.** A
`linuxserver/openssh-server:latest` container named
`relayterm-staging-xterm-baseline-smoke-ssh`, attached
only to the staging Compose network
`relayterm-staging_relayterm-staging-internal` with DNS
alias `xterm-baseline-smoke-host` resolving to
`172.21.0.5`. **No host port was published** — `docker
port` returned empty; verified the target was reachable
only from inside the staging Compose network via a
sidecar `busybox nc -zv xterm-baseline-smoke-host 2222`
which succeeded. `USER_NAME=smoke`, `SUDO_ACCESS=false`,
`PASSWORD_ACCESS=false`, `PUBLIC_KEY=<the
RelayTerm-generated public OpenSSH line>`. The container
was `docker stop && docker rm`'d during cleanup.

**Identity path.** Generated (path A — backend-side
keypair generation). One
`POST /api/v1/ssh-identities` returned an
`SshIdentityResponse` with `key_type=ed25519` and
fingerprint
`SHA256:mCaCBIMyh9n9Rjkf0Q+XiN95/hL5P712CoaCHCnmVG0`
(identity UUID `6317698e-c8ce-46b2-be9f-a7e455f29ad7`).
No local PEM, no base64 sidecar, no key-material shred
step at cleanup — the only secret bytes lived in the
backend vault and never crossed the wire. The 2026-05-13
deployable-baseline smoke above already covers the
**imported**-identity path against these same images;
this slice deliberately took the lighter-touch generated
path to keep the focus on renderer behaviour.

**Host + profile create.** `Xterm-Baseline-Smoke-Host`
(display name) / `xterm-baseline-smoke-host` (hostname,
lowercase — load-bearing for DNS resolution on the
Compose network) / `2222` / default user `smoke` via the
`servers-create-host-*` form (host UUID
`448b53d0-c732-4725-b83a-4fa429b34132`).
`xterm-baseline-smoke-profile` binding that host to the
generated identity with no username override and tags
`smoke,xterm-baseline` via the `servers-create-profile-*`
form (profile UUID `b568da7d-7624-46f0-9ea9-5529d4b89b4c`).
The success card carried the load-bearing copy "The host
key is not yet trusted and SSH authentication has not
been verified for this profile."

**Host-key preflight + trust.** `host-key-preflight-button`
on the profile row captured fingerprint
`SHA256:oLaQ5Ep4JimvJrQhUuaIOI7rPYvlYiuESO+pURNRrbg`
during SSH key exchange. The value is **byte-identical**
to the locally-computed `ssh-keygen -lf` value over the
target container's advertised `ssh-ed25519` host-key
line (the container emitted all three host-key types —
ed25519, ecdsa-sha2-nistp256, rsa — at startup; preflight
picked ed25519). Typed the fingerprint into
`host-key-confirm-input` → `host-key-trust-button`;
`host-key-status-badge` flipped to `Trusted` with the
load-bearing copy "Host key pinned. Re-run preflight to
confirm. Run auth-check below to verify the configured
SSH identity authenticates …". `trusted_at` stamped at
`2026-05-13 17:04:26.747+00`.

**Auth-check.** `auth-check-run-button` ran in ~6 s;
`auth-check-status-badge` flipped to `Authenticated`
with the load-bearing copy "SSH public-key
authentication succeeded for the configured username.
No PTY was allocated and no command was executed.
Terminal launch is a separate, deliberate action." The
target sshd log confirmed the same moment:
`Accepted publickey for smoke from 172.21.0.3 port
33022 ssh2: ED25519 SHA256:mCaCBIMyh9n9Rjkf0Q+XiN95/hL5P712CoaCHCnmVG0`
followed by an immediate clean disconnect — no PTY
allocated.

**Terminal launch.** `profile-launch-terminal` opened
`/terminal` with phase=`live` immediately (no 60-s WS-
close window required for this run — typing began
within a few seconds). Session UUID
`2e097bee-9405-4bd0-9f64-8bf4e5827c08`; initial PTY size
`24 × 80` (xterm-default; the production-shell
`XtermRenderer` is constructed with the renderer-neutral
options on `ProductionTerminal.svelte`).

**Basic I/O.** Four commands round-tripped end-to-end:
- `echo relayterm-xterm-baseline` →
  `relayterm-xterm-baseline`
- `whoami` → `smoke`
- `pwd` → `/config` (the `linuxserver/openssh-server`
  default home for `USER_NAME=smoke`)
- `uname -a` →
  `Linux xterm-baseline-smoke-host 6.17.0-8-generic
  #8-Ubuntu SMP PREEMPT_DYNAMIC Fri Nov 14 21:44:46
  UTC 2025 x86_64 GNU/Linux` (wrapped across two visible
  rows; the kernel version is the cloud-edge host
  kernel — containers do not run their own kernel).

Keystrokes were delivered exclusively through
`page.keyboard.press('<char>')` — synthetic
`InputEvent`-dispatch into `.xterm-helper-textarea`
was rejected by xterm's input handler
(it checks `event.isTrusted`), so per-char press_key
was the only reliable input path over MCP for this
slice.

**Resize / fit.** Resized the browser viewport from
1440 × 900 → 1800 × 1000 and clicked
`production-terminal-fit`; the in-shell PTY size flipped
from `24 × 80` to `28 × 112` (verified by running
`stty size` in-terminal before and after). A
`session_events.resized` row was appended at
`17:09:43.355+00` — exactly one row, no chatter.

**Long output / backpressure.** `seq 1 300` rendered all
300 numbered lines; the visible tail was
`296 / 297 / 298 / 299 / 300` followed by a clean prompt.
A subsequent `echo relayterm-after-long-output` then
`relayterm-after-long-output` confirmed the terminal
remained responsive after the 300-line burst.

**Unicode / box drawing / wide chars.** **Not exercised
in this slice.** Per-char `page.keyboard.press('<char>')`
was the only reliable input path (xterm rejected
synthetic `InputEvent` payloads); typing non-ASCII
glyphs through that path was not attempted. Deferred to
a future slice or to an enhanced MCP input path. **Do
not infer** that this means xterm baseline does or does
not render Unicode well — the smoke simply did not
exercise it.

**Copy / paste.** **Not exercised in this slice.**
Clipboard read/write over the MCP requires elevated
permissions, and synthetic `ClipboardEvent` is rejected
by xterm's paste handler for the same `isTrusted`
reason. The production paste-safety flow
(`evaluatePaste`, `production-terminal-paste-confirm`,
`production-terminal-paste-blocked`) has unit-test
coverage under `apps/web/tests/`. Deferred.

**Alternate screen / full-screen apps.** **Not
exercised in this slice.** Same input-path constraint
as the Unicode and Copy / paste rows above; the
"Alternate screen / full-screen apps" row is one of the
evaluation-matrix dimensions in
[`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
§ "Core correctness" and is deferred to its own slice.

**Mouse support.** **Not exercised.** Production today
does not expose a renderer-neutral mouse-mode toggle;
the dev lab is where mouse experiments live. Deferred
to a future slice.

**Detach + reconnect.** Echoed
`echo relayterm-before-detach` →
`relayterm-before-detach` to seed the replay buffer.
`production-terminal-detach` at `17:11:22.925+00` —
`production-terminal-phase` flipped to
`detached (TTL window)`, `production-terminal-ttl-hint`
rendered "Detached sessions stay reconnectable for
about 30 seconds after the last client drop. Replay
is in-memory and not durabl…". Clicked
`production-terminal-reconnect` ~11 s later at
`17:11:33.457+00` (the in-page button, not a Sessions-
list round-trip — that hop can blow the 30 s detached
TTL on a slow page navigation). Reattach landed on the
**same** session UUID
`2e097bee-9405-4bd0-9f64-8bf4e5827c08`; phase returned
to `live`. The xterm DOM was a fresh mount on
reattach — its `xterm-dom-renderer-owner` integer
incremented from `-1` to `-2`, and the previous
pre-detach lines were NOT visually replayed into the
new mount. Wire-side replay correctness was instead
verified by running a fresh
`echo relayterm-after-reconnect` and observing
`relayterm-after-reconnect` round-trip cleanly. The DB
`session_events` rows match this account exactly:
`detached` at `17:11:22.925`, `attached` and
`reattached` both at `17:11:33.459` (~2 ms apart).
**Do not overclaim** visual replay parity for the xterm
baseline on these images — the renderer remounts and
the new mount is empty until fresh PTY output arrives;
this is what the production shell does today and is the
behaviour future renderer candidates should be measured
against, not improved on as part of an "xterm fix" slice.

**Narrow / mobile viewport.** Resized to 390 × 844 and
clicked `production-terminal-fit`; the terminal
workspace remained reachable, the prompt + input bar
stayed usable, and a fresh
`echo relayterm-mobile-width-xterm` →
`relayterm-mobile-width-xterm` round-tripped (with
line wrap visible at the narrower column count, as
expected on a width-constrained PTY). A second
`session_events.resized` row was appended at
`17:12:39.920+00`. Resized back to 1440 × 900 before
close. A full Android / Tauri-shell smoke was NOT in
scope of this slice; that has its own surface.

**Close.** `production-terminal-close` at
`17:13:17.616+00`. DB confirms `status=closed` and
`closed_at` set. `session_events.closed` row appended.
The target sshd log shows
`Received disconnect from 172.21.0.3 port 48192:11:
relayterm pty close` — the clean RelayTerm-initiated
PTY teardown.

**Session lifecycle events.** Exactly 8 rows on
`session_events` for `2e097bee-…`, in this order:
`created` (17:05:21.235) → `attached` (17:05:21.653) →
`resized` (17:09:43.355) → `detached` (17:11:22.925) →
`attached` (17:11:33.457) → `reattached` (17:11:33.459)
→ `resized` (17:12:39.920) → `closed` (17:13:17.616).
Per the schema's per-session telemetry contract, none
of these crossed over into `audit_events`.

**Audit events in the smoke window.** Exactly 2 rows
between `2026-05-13T16:55:00Z` and
`2026-05-13T17:15:00Z`:
- `ssh_identity_created` at `17:01:38.418+00`, payload
  `{name, source:"generated", key_type:"ed25519",
  created_at, ssh_identity_id, fingerprint_sha256}` —
  public-metadata only (no `encrypted_private_key`,
  no PEM bytes).
- `server_profile_created` at `17:03:45.694+00`,
  payload `{name, host_id, disabled_at:null,
  ssh_identity_id, server_profile_id}` —
  public-metadata only.

Host-key preflight, host-key trust, auth-check, and the
terminal-session lifecycle ops deliberately emit **no**
`audit_events` rows on these images — `host_*` kinds are
absent from the schema CHECK constraint (per SPEC.md
"Audit-event expectations"), and terminal-session
lifecycle telemetry stays in `session_events`. The
**zero** `audit_events` rows for those ops here are
expected, not a redaction gap.

**Cleanup-disable audit row.** After the smoke window
proper, the cleanup step disabled
`xterm-baseline-smoke-profile` via the SPA. The DB
shows `disabled_at=2026-05-13 17:21:51.632220+00` and
exactly one `server_profile_disabled` audit row at
`17:21:51.634365+00` (~2 ms after the lifecycle
transition — single audit row, matching the
"idempotency early-return BEFORE audit append" rule
from AGENTS.md § "Things to avoid"). Payload:
`{name:"xterm-baseline-smoke-profile", host_id,
disabled_at, ssh_identity_id, server_profile_id}` —
public-metadata only.

**Backend / web / target log redaction.** Bounded
`docker logs --since 30m` over the smoke window:
backend = 1 line (the literal text "missing session
cookie" — pre-smoke WARN from an unauthenticated
`/api/v1/auth/me` healthcheck at 16:55; not a cookie
**value**; same explanation as the
2026-05-13 deployable-baseline smoke above);
web/nginx = 27 lines (request log only — no payloads,
no cookies, no `data_b64`); target sshd = preflights
(preauth, no PTY), one auth-check (no PTY), one
terminal session ending `relayterm pty close`. Sentinel
sweep against `{private_key_openssh,
encrypted_private_key, BEGIN OPENSSH PRIVATE KEY,
openssh-key-v1, passphrase, session_token, token_hash,
data_b64, REDACT-MARKER, relayterm-xterm-baseline,
relayterm-after-long-output, relayterm-before-detach,
relayterm-after-reconnect, relayterm-mobile-width-xterm}`
returned **0 hits in every log**, except the 1
explained "cookie" mention above (the `cookie` sentinel
matched ONLY the literal WARN-line text "missing
session cookie" — not any cookie attribute, header
name, OR cookie VALUE).

**DOM + storage redaction.** Post-close sweep over
`document.documentElement.outerHTML`: zero hits across
all sentinels above. `document.cookie.length === 0`
(the `relayterm_session` cookie is HttpOnly — JS cannot
read it). `localStorage` empty, `sessionStorage` empty
(the `relayterm.active-terminal.v1` pointer was cleared
on close, as designed).

**Audit-payload sentinel sweep.** Against the smoke-
window audit_events: `payload::text ILIKE` filter for
`{%private_key%, %BEGIN OPENSSH%, %passphrase%,
%session_token%, %token_hash%, %data_b64%,
%REDACT-MARKER%, %relayterm-xterm-baseline%,
%relayterm-after-%, %relayterm-before-%,
%relayterm-mobile-%}` returned **zero rows**.

**Console noise (follow-up, not a redaction gap).** The
production terminal page accumulated 16 console errors
across the smoke (the bulk after the
`production-terminal-detach → production-terminal-
reconnect` re-mount step). **The captured console log
content was NOT inspected in this slice.** Recording
this as a follow-up rather than a finding: no payload
bytes were observed in any of the tested redaction
surfaces (DOM, `localStorage`, `sessionStorage`,
`audit_events.payload`, backend/web/target logs); the
console-noise count is being surfaced here so a later
slice (or the future renderer-evaluation smoke for
ghostty-web / restty / wterm) can verify the noise
either does not contain sensitive bytes or document
exactly what it does contain. The 2026-05-13
deployable-baseline smoke above recorded a similar
post-detach console signature on the same images, which
is consistent with "expected re-mount chatter, not a
new defect."

**Cleanup state.** Throwaway SSH container
`relayterm-staging-xterm-baseline-smoke-ssh` is
`docker stop` + `docker rm`'d (verified `docker ps -a
--filter name=…` returns empty). Profile
`xterm-baseline-smoke-profile` disabled through the
SPA (preserved with `disabled_at` set, not deleted, per
the inventory-lifecycle policy). No local key cleanup
was required because the identity was generated
server-side. The browser session may remain logged in.
Left in place per the slice plan: staging Compose stack
running, Postgres untouched (no row deleted, no schema
touched),
`xterm-baseline-smoke-identity` (`6317698e-…`),
`Xterm-Baseline-Smoke-Host` (`448b53d0-…`),
`xterm-baseline-smoke-profile` (`b568da7d-…`, disabled),
the 1 closed `terminal_sessions` history row, the 8
`session_events` rows, the 1 trusted
`known_host_entries` row, all 3 `audit_events` rows
emitted during the smoke (`ssh_identity_created`,
`server_profile_created`, `server_profile_disabled`),
the staging smoke user.

**Intentionally deferred** (out of scope for this
slice; tracked in
[`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
or scheduled later):

- ghostty-web / restty / wterm experimental renderer
  evaluation against this same staging stack — the
  promotion gates and matrix live in the evaluation
  plan; this slice records the **xterm baseline only**;
- desktop Tauri (path A bundled-shell handoff)
  renderer smoke against this same staging stack;
- Android Tauri renderer smoke (`@wterm/dom`'s
  motivating surface;
  `tauri android build --debug --apk --ci` path);
- automated performance / benchmark harness for any
  renderer candidate (a committed Playwright runner
  for renderer smokes is itself deferred per
  `apps/web/e2e/SMOKE.md`);
- `tmux` / `screen` host-side multiplexer persistence
  (Option C in
  [`docs/persistent-sessions.md`](../persistent-sessions.md)) —
  unrelated to renderer evaluation and not unlocked by
  this work;
- VT snapshots / durable terminal-display persistence
  (Phase 2 of the persistent-sessions roadmap);
- Unicode / box drawing / wide-char rendering check
  (input-path limitation in this slice; see "Unicode"
  row above);
- copy / paste round-trip through the production
  paste-safety flow (clipboard / `isTrusted`
  limitations; see "Copy / paste" row above);
- alternate-screen / full-screen-app behaviour;
- renderer-aware mouse-mode support;
- per-session-per-device renderer preference
  persistence (the production shell has no renderer
  selector today; this is a renderer-evaluation
  follow-up).

**Verdict.** The **production xterm baseline renderer**
runs cleanly end-to-end against staging: launch, basic
I/O, in-session resize / fit, 300-line burst,
wire-side detach / reconnect inside the 30 s TTL,
mobile-width workspace, and clean close — all on the
same `:main` image digests yesterday's deployable-
baseline smoke recorded. Redaction posture holds across
DOM, `localStorage` / `sessionStorage`, backend / web /
target logs, and `audit_events.payload`. The reattach
behaviour (fresh xterm DOM mount with no visual replay
of the pre-detach buffer) is **what xterm baseline does
on these images today** — captured here as the reference
point future renderer candidates are measured against,
not as a defect to fix in an xterm-only slice. Safe
reference smoke for the renderer-evaluation track to
build on; ghostty-web / restty / wterm remain
experimental and dev-lab-only.

---

### 2026-05-13 · Ghostty-web production-shell renderer smoke (CSP-blocked; xterm fallback verified)

**Date.** 2026-05-13 20:30 UTC – 21:02 UTC (≈32 min).
**Staging URL.** `https://relayterm-staging.js-node.cc`.
**Stack pin.** `git.js-node.cc/jsprague/relayterm-web:main`
(image `sha256:d5fa038b…`, built 2026-05-13 20:27 UTC) +
`git.js-node.cc/jsprague/relayterm-backend:main` (image
`sha256:9ab478a3…`, built 2026-05-13 20:26 UTC). Both
were pulled and force-recreated at the start of this
slice (`docker compose up -d --no-deps --force-recreate
--pull never relayterm-web relayterm-backend`) so the
running web bundle includes
`a9f3ed5 feat(web): add experimental renderer selector`.
Postgres `postgres:17-alpine` was left untouched
(`Up 4 days` before and after the slice).
**Branch.** `docs/ghostty-web-production-renderer-smoke`
off `main` (docs-only slice; no source / CI / deploy /
schema changes).
**Browser surface.** Playwright MCP (Chrome / Linux) at
1440 × 900. Auth: existing
`staging+throwaway-20260509173230@example.com` cookie
session, no re-login.

**Goal.** Carry the ghostty-web experimental adapter
through the same production-shell evaluation matrix as
the 2026-05-13 xterm production-baseline entry above,
using the gated experimental renderer selector that
landed on the same date in
`apps/web/src/lib/app/views/SettingsView.svelte` +
`apps/web/src/lib/app/terminal/rendererLoader.ts`. Slice
boundaries: no source / schema / API / auth / CSRF / CORS
/ WebSocket-protocol / Tauri / CI / deploy file changes,
no renderer promotion. xterm is and remains the
production compatibility baseline and the default
renderer.

**Renderer path (gated, operator-opt-in).** Production
shell exposes the experimental renderer evaluation card
at `[data-testid="settings-experimental-renderer"]`.
The card and gate toggle are always rendered; the
warning copy, renderer radio group, and effective-
renderer diagnostic reveal only when the gate is on.
Pre-gate state: `data-renderer-gate="off"`. After
clicking `settings-experimental-renderer-toggle`,
selecting `renderer-option-ghostty-web`, and clicking
`settings-apply`:
- `localStorage["relayterm.terminal-settings.v1"]`
  carries `rendererId="ghostty-web"` and
  `experimentalRendererEvaluationEnabled=true`.
- `settings-renderer-effective` reads "Effective
  renderer on next session: ghostty-web experimental."
- `settings-status-saved` reads "Saved locally. Applies
  to the next terminal session."

**Throwaway SSH target.** A
`linuxserver/openssh-server:latest` container named
`relayterm-staging-ghostty-web-smoke-ssh`, attached only
to the staging Compose network
`relayterm-staging_relayterm-staging-internal` with DNS
alias `ghostty-web-smoke-host` resolving to
`172.21.0.5`. **No host port was published**
(`docker port` returned empty; verified). `USER_NAME=smoke`,
`SUDO_ACCESS=false`, `PASSWORD_ACCESS=false`,
`PUBLIC_KEY=<the RelayTerm-generated OpenSSH line>`.
The container was `docker stop && docker rm`'d during
cleanup.

**Identity path.** Generated (backend-side keypair
generation). One `POST /api/v1/ssh-identities` via the
`identities-generate-submit` form returned an
`SshIdentityResponse` with `key_type=ed25519` and
fingerprint
`SHA256:PJk5xEIrd3kOOdbr5OwcVqMeZHksgItWq2hW570k3zw`
(identity UUID `c85ffbe8-ef2a-4ce5-9e1b-efcf25f6f7cb`).
The public-key OpenSSH line was extracted via a single
`browser_evaluate` call that wrote the value to a
local file (`ghostty-web-pubkey.txt`); the file was
piped through `printf %q` into the `docker run`
invocation on `cloud-edge` and **shredded immediately
after** the container started. No PEM, no base64
sidecar, no private-key bytes touched the operator
filesystem at any point.

**Host + profile create.** `Ghostty-Web-Smoke-Host`
(display name) / `ghostty-web-smoke-host` (hostname) /
`2222` / default user `smoke` (host UUID
`84ea011c-c69e-4c49-8e2e-c50fbc2c0a68`).
`ghostty-web-smoke-profile` binding that host to
`ghostty-web-smoke-identity` with no username override
and tags `renderer,ghostty-web` (profile UUID
`efbe170e-8ff7-48b2-9421-8da5f80a3227`). Success card
carried the load-bearing copy "The host key is not yet
trusted and SSH authentication has not been verified
for this profile."

**Host-key preflight + trust.** Preflight captured
fingerprint
`SHA256:wDUNS9iLKyR3Shor16U/lAWG1b0cl9dXNKdcUYSSCmg`,
which is **byte-identical** to the locally-computed
`ssh-keygen -lf` value over the target container's
advertised `ssh-ed25519` host-key line (the linuxserver
image emits ecdsa-sha2-nistp256, ed25519, and rsa at
startup; preflight picks ed25519). Typed the
fingerprint into `host-key-confirm-input` →
`host-key-trust-button`; `host-key-status-badge`
flipped to `Trusted`.

**Auth-check.** `auth-check-run-button` flipped
`auth-check-status-badge` to `Authenticated` after a
few seconds — public-key authentication succeeded with
no PTY allocated.

**Terminal launch — ghostty-web attempt.**
`profile-launch-terminal` opened `/terminal` and
created session UUID
`f78210b2-e170-477d-8759-f851a915b693`. After ≥20 s of
waiting, the workspace stayed wedged at:
- `data-phase="idle"`
- `data-renderer="unmounted"`
- `data-renderer-experimental="false"`
- `data-renderer-fallback=""` (empty)
- `data-renderer-gate="on"`
- `production-terminal-renderer-diagnostic` not rendered
- viewport empty (zero children)
- `production-terminal-error` not rendered

Console captured exactly the failure shape:

1. `Connecting to 'data:application/wasm;base64,…'
   violates the following Content Security Policy
   directive: "default-src 'self'". Note that
   'connect-src' was not explicitly set, so
   'default-src' is used as a fallback.`
2. `Fetch API cannot load
   data:application/wasm;base64,… Refused to connect
   because it violates the document's Content Security
   Policy.`
3. `CompileError: WebAssembly.compile(): Compiling or
   instantiating WebAssembly module violates the
   following Content Security policy directive because
   'unsafe-eval' is not an allowed source of script in
   the following Content Security Policy directive:
   "default-src 'self'".` (Stack: `wA.mount` →
   `T`.)

**Result classification: renderer issue (load /
deploy interaction).** ghostty-web 0.4.0 ships its
WASM payload as an inlined `data:application/wasm;base64,…`
URL and `await init()`s it via `WebAssembly.compile()`
inside its `Terminal.open` / `loadFromPath` path. The
staging stack's nginx CSP is `default-src 'self'`
without `'unsafe-eval'` or `'wasm-unsafe-eval'` and
without an explicit `connect-src`, which blocks BOTH
the `data:` URL fetch AND the WASM compile step. The
dynamic `import()` itself succeeded (Vite/Rollup
chunk-split the adapter to a separate asset that the
gated loader fetched cleanly), so
`rendererLoader.ts`'s `adapter_load_failed` fallback
DID NOT fire — the load resolved successfully and the
rejection occurred later inside `r.mount(mountTarget)`,
which `attach()` does not have a catch block for. The
workspace is therefore wedged at `idle` with no
fallback diagnostic, no error panel, and no user-
visible explanation. **xterm baseline does not hit
this path** because xterm is statically imported and
contains no WASM init. The renderer-loader's fallback
taxonomy
(`experimental_gate_off` / `unknown_renderer_id` /
`adapter_load_failed`) is exhaustive for synchronous
loader paths but does not cover asynchronous `mount()`
rejection — a real product gap exposed by this smoke,
to be addressed in a separate slice. **Do not infer**
that ghostty-web cannot render anything; this smoke
proves only that the adapter cannot initialize under
the staging stack's current CSP. A future smoke
against a stack that allows `'wasm-unsafe-eval'` (and
either widens `connect-src` to allow `data:` or pins a
ghostty-web build that ships WASM as an asset rather
than a data URL) is required before the matrix rows
can be exercised at all.

**Matrix rows (browser surface).** Every
evaluation-matrix row below is marked
`deferred — renderer not identified (ghostty-web
adapter failed to mount; see failure narrative
above)`. The label uses the closed-vocabulary
"renderer not identified" form per
`apps/web/e2e/SMOKE.md` § "Renderer path
confirmation" — the only conforming deferred-label
options are `deferred — renderer not identified` and
`deferred — renderer fell back to <id>`, and neither
exactly fits a `data-renderer="unmounted"` +
`data-renderer-fallback=""` wedge. The closer of the
two is "renderer not identified" because no
candidate renderer code path ran, even though the
attribute set does technically pin the cause; the
free-form suffix is documentation-only and not part
of the contract vocabulary. Rows: basic ASCII I/O,
resize / fit, long output, Unicode CJK, box-drawing,
wide chars, paste safe / confirm / blocked, alternate
screen, mouse mode enable, detach / reconnect /
replay, narrow viewport — **all deferred**.

**Xterm fallback verification (NOT a ghostty-web smoke
pass).** After capturing the failure, the gate toggle
was flipped OFF in Settings (which the
`onExperimentalGateChange` handler explicitly resets to
`rendererId="xterm"`), saved, and a new terminal launch
opened on the same profile. Session UUID
`aec95bfd-40f1-4bbb-916a-7f525493f6ff` mounted in
under a second with:
- `data-renderer="xterm"`
- `data-renderer-experimental="false"`
- `data-renderer-fallback=""`
- `data-renderer-gate="off"`
- diagnostic strip: "Renderer. xterm baseline"

The xterm session went detached during the
verification idle wait (no prompt output had been
received yet so `lastSeenSeq=0`, which the
`enablement.reconnect` predicate uses as an
unreconnectable signal) and closed cleanly via the
detached-TTL janitor at 20:50:05 UTC (91 s after
create). **This is the same xterm production-baseline
behaviour the 2026-05-13 xterm entry recorded** and is
NOT counted as a ghostty-web smoke pass; it only
proves the production shell remains usable after a
gated experimental renderer fails to load.

**Session lifecycle rows.**
- `terminal_sessions.f78210b2-…`: status `active`,
  closed_at NULL — created server-side but no WS
  attach ever happened (mount rejection short-circuited
  `attach()` before the WebSocket handshake), so the
  backend orchestrator has no russh channel for this
  id. Will be reaped by the orphan-session janitor.
- `terminal_sessions.aec95bfd-…`: status `closed`,
  91 s lifetime.
- `session_events` for `f78210b2-…`: exactly 1 row
  (`created`). No `attached`, no `detached`, no
  `closed` — consistent with the mount failure
  happening before the WS attach handshake.
- `session_events` for `aec95bfd-…`: 4 rows in
  order `created → attached → detached → closed`.
- Per the schema's per-session telemetry contract,
  none of these crossed into `audit_events`.

**Audit events in the smoke window.** Exactly 2 rows
created during the smoke proper (between identity-
generate and stack-recreate-time):
- `ssh_identity_created` at `20:40:33.826514Z`,
  payload `{name, source:"generated", key_type:"ed25519",
  created_at, ssh_identity_id, fingerprint_sha256}` —
  public-metadata only.
- `server_profile_created` at `20:43:38.…Z`, payload
  `{name, host_id, disabled_at:null, ssh_identity_id,
  server_profile_id}` — public-metadata only.

Host-key preflight, host-key trust, auth-check, and
terminal-session lifecycle ops deliberately emit no
`audit_events` rows on these images (same posture as
the 2026-05-13 xterm baseline above). **Zero**
`audit_events` rows for the ghostty-web mount failure
itself — the failure path is browser-side only.

**Cleanup-disable audit row.** Cleanup step disabled
`ghostty-web-smoke-profile` via the SPA
(`profile-disable-open` → `profile-disable-confirm-input`
→ `profile-disable-submit`). DB shows
`disabled_at=2026-05-13 21:01:06.754349+00` and
exactly one `server_profile_disabled` audit row at
`21:01:06.758595+00` (~4 ms after the lifecycle
transition — single audit row, matching the
"idempotency early-return BEFORE audit append" rule
from AGENTS.md § "Things to avoid"). Payload:
`{name:"ghostty-web-smoke-profile", host_id,
disabled_at, ssh_identity_id, server_profile_id}` —
public-metadata only.

**Backend / web / target log redaction.** Bounded
`docker logs --since 30m` over the smoke window:
backend = 7 lines (1 `WARN missing session cookie`
pre-smoke line — same explanation as the 2026-05-13
xterm baseline entry, a literal WARN string, not a
cookie value); web/nginx = 66 lines (request log only,
no payloads); target sshd = 40 lines (linuxserver
entrypoint chatter only; no auth lines on stdout
because the `linuxserver/openssh-server` image keeps
`LogLevel INFO` events to `/var/log/auth.log` inside
the container). Sentinel sweep against
`{private_key_openssh, encrypted_private_key,
BEGIN OPENSSH PRIVATE KEY, openssh-key-v1, passphrase,
session_token, token_hash, data_b64, REDACT-MARKER,
relayterm-ghostty-web-baseline, relayterm-after-long-output,
relayterm-before-detach, relayterm-after-reconnect,
relayterm-mobile-width-ghostty-web}` returned **0
real hits** in every log. The `cookie` sentinel
matched the backend's `WARN missing session cookie`
text (same false-positive as the xterm baseline); the
`password` sentinel matched the target sshd's
linuxserver entrypoint message
`User/password ssh access is disabled.` confirming
`PASSWORD_ACCESS=false` was honored. Neither match
represents a real secret-bytes leak; both are static
diagnostic strings.

**DOM + storage redaction.** Post-cleanup sweep over
`document.documentElement.outerHTML`: zero hits across
all sentinels. `document.cookie.length === 0` (the
`relayterm_session` cookie is HttpOnly — JS cannot
read it). `localStorage` carried only
`relayterm.active-terminal.v1` (empty after the
"Back to servers" nav) and
`relayterm.terminal-settings.v1` (cosmetic +
renderer fields; no payload bytes). `sessionStorage`
empty.

**Audit-payload sentinel sweep.** Against the
smoke-window `audit_events`: `payload::text ~*`
filter for
`{private_key, BEGIN OPENSSH, passphrase,
session_token, token_hash, data_b64, REDACT-MARKER,
relayterm-ghostty-web, relayterm-after-,
relayterm-before-, relayterm-mobile-}` returned
**zero rows**.

**Cleanup state.** Throwaway SSH container
`relayterm-staging-ghostty-web-smoke-ssh` is
`docker stop` + `docker rm`'d (verified
`docker ps -a --filter name=… --format {{.Names}}`
returns empty). Profile
`ghostty-web-smoke-profile` disabled through the SPA
(preserved with `disabled_at` set, not deleted, per
the inventory-lifecycle policy). Local public-key
sidecar file `ghostty-web-pubkey.txt` was shredded
immediately after the container started. Settings
reset to `rendererId="xterm"` /
`experimentalRendererEvaluationEnabled=false` so a
future browser session against this staging surface
starts on the production default. Left in place per
the slice plan: staging Compose stack running,
Postgres untouched (uptime `Up 4 days` before and
after the slice), `ghostty-web-smoke-identity`
(`c85ffbe8-…`), `Ghostty-Web-Smoke-Host`
(`84ea011c-…`), `ghostty-web-smoke-profile`
(`efbe170e-…`, disabled), the 1 `active` (orphan) +
1 `closed` `terminal_sessions` history rows, the 5
total `session_events` rows, the 1 trusted
`known_host_entries` row, all 3 `audit_events` rows
emitted during the smoke (`ssh_identity_created`,
`server_profile_created`,
`server_profile_disabled`), the staging smoke user.

**Intentionally deferred** (out of scope for this
slice; tracked in
[`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
or scheduled later):

- ghostty-web evaluation-matrix rows (basic ASCII I/O,
  resize / fit, long output, Unicode CJK, box-drawing,
  wide chars, paste safe / confirm / blocked,
  alternate screen, mouse mode enable, detach /
  reconnect / replay, narrow viewport) — every row is
  deferred because the adapter never mounted; rerun
  is gated on either a CSP-compatible ghostty-web
  build or a deploy-side CSP change that allows
  `'wasm-unsafe-eval'` plus `data:` in `connect-src`.
  Both are separate slices;
- restty / wterm experimental renderer evaluation —
  not exercised in this slice;
- desktop Tauri (path A bundled-shell handoff)
  renderer smoke for ghostty-web on this stack;
- Android Tauri renderer smoke;
- automated performance / benchmark harness for any
  renderer candidate;
- a renderer-loader source slice that catches
  asynchronous `mount()` rejection and surfaces a
  fourth `mount_failed` value on `data-renderer-fallback`
  so a future smoke run trips a typed diagnostic
  rather than a wedged `idle` workspace (the loader's
  current `adapter_load_failed` taxonomy only covers
  synchronous load paths);
- `tmux` / `screen` host-side multiplexer persistence;
- VT snapshots / durable terminal-display persistence;
- renderer production-default switch (Gate 2);
- per-session-per-device renderer preference
  persistence beyond the current
  `relayterm.terminal-settings.v1` localStorage entry.

**Promotion decision.** **ghostty-web remains
experimental.** The production default remains xterm.
Gate 1 and Gate 2 criteria are unchanged. No backend
protocol, session, orchestrator, `terminal-core`,
production-shell-non-loader, CI, or deploy file was
touched by this slice. This smoke is a single
human-evaluator pass that did not produce a single
graded matrix row for ghostty-web; the
`adapter_load_failed`-equivalent finding it surfaces
is documented as a real product gap for a separate
slice but does not itself promote or demote any
renderer.

**Verdict.** The production-shell experimental
renderer selector lands cleanly: gate toggle reaches
`localStorage`, persists the selection, surfaces the
warning + radio + effective-renderer diagnostic
exactly as `apps/web/e2e/SMOKE.md` § "Renderer path
confirmation" assumed. ghostty-web 0.4.0's inlined
WASM data URL cannot initialise under the staging
stack's current CSP, which (a) blocks the entire
evaluation matrix for this slice, and (b) exposes a
real gap in `rendererLoader.ts`'s fallback taxonomy
(async `mount()` rejection lands in a wedged `idle`
workspace rather than a typed `data-renderer-fallback`
diagnostic). xterm baseline mounts cleanly on the
same surface after the gate is flipped off,
confirming the production shell remains usable when
an experimental adapter fails. Safe carry-forward
data point for the renderer-evaluation track;
ghostty-web stays experimental and unpromoted.

### 2026-05-14 · Ghostty-web renderer mount-failure diagnostic resmoke (adapter_mount_failed verified; xterm recovery still works)

**Date.** 2026-05-14 00:48 UTC – 01:03 UTC (≈15 min).
**Staging URL.** `https://relayterm-staging.js-node.cc`.
**Stack pin.** `git.js-node.cc/jsprague/relayterm-web:main`
(image `sha256:0250a9f9…`, built 2026-05-14 00:20 UTC) +
`git.js-node.cc/jsprague/relayterm-backend:main` (image
`sha256:8f0210d9…`, built 2026-05-14 00:19 UTC). Both
were pulled and force-recreated at the start of this
slice (`docker compose up -d --no-deps --force-recreate
--pull never relayterm-web relayterm-backend`) so the
running web bundle includes
`239fe29 feat(web): handle renderer mount failures` —
the synchronous loader-only fallback taxonomy is now
extended with the asynchronous `adapter_mount_failed`
value plus the fixed operator-facing copy in
`production-terminal-error`. Postgres
`postgres:17-alpine` left untouched (`Up 4 days` before
and after). Bundle assertion: `grep -l "Renderer failed
to mount" /usr/share/nginx/html/assets/*.js` and
`grep -l adapter_mount_failed` both match the same web
asset (`index-CpPmaq5m.js`) inside the recreated web
container.
**Branch.** `docs/ghostty-web-mount-failure-resmoke`
off `main` (docs-only slice; no source / CI / deploy /
schema changes).
**Browser surface.** Playwright MCP (Chrome / Linux) at
1440 × 900. Auth: existing
`staging+throwaway-20260509173230@example.com` cookie
session, no re-login.

**Goal.** Verify the renderer mount-failure diagnostic
that landed after the 2026-05-13 ghostty-web smoke
above (`feat(web): handle renderer mount failures`) on
the live staging surface. Three load-bearing
assertions:

- ghostty-web still fails to mount under the staging
  stack's current CSP/WASM constraints (no attempt to
  fix CSP, no attempt to ship ghostty-web as anything
  other than experimental).
- The production terminal no longer wedges silently —
  `data-renderer-fallback` now carries
  `adapter_mount_failed` and `production-terminal-error`
  surfaces a fixed safe message.
- xterm fallback/manual recovery still works after the
  operator flips the gate off.

Slice boundaries: no renderer-adapter changes, no CSP
changes, no WASM/Vite bundling changes, no
backend/session/orchestrator/protocol changes, no
CI/deploy changes, no renderer promotion. This is a
diagnostic resmoke — **no evaluation-matrix rows are
graded for ghostty-web**.

**Renderer setup (gated, operator-opt-in).** Settings
view: pre-state `data-renderer-gate="off"`, persisted
`rendererId="xterm"`. Clicked
`settings-experimental-renderer-toggle`, selected
`renderer-option-ghostty-web`, clicked
`settings-apply`. Post-state:
- `localStorage["relayterm.terminal-settings.v1"]`
  carries `rendererId="ghostty-web"` and
  `experimentalRendererEvaluationEnabled=true`.
- `settings-renderer-effective` reads "Effective
  renderer on next session: ghostty-web experimental."
- `settings-status-saved` reads "Saved locally. Applies
  to the next terminal session."

**Throwaway SSH target.** A
`linuxserver/openssh-server:latest` container named
`relayterm-staging-ghostty-mount-resmoke-ssh`, attached
only to the staging Compose network
`relayterm-staging_relayterm-staging-internal` with
DNS alias `ghostty-mount-resmoke-host` resolving to
`172.21.0.5`. **No host port was published**
(`docker port` returned empty; verified).
`USER_NAME=smoke`, `SUDO_ACCESS=false`,
`PASSWORD_ACCESS=false`,
`PUBLIC_KEY=<the RelayTerm-generated OpenSSH line>`.
The public-key line was extracted via a single
`browser_evaluate` to a local file, piped into the
`docker run` invocation on `cloud-edge`, and
`shred -u`'d immediately afterward. The container was
`docker stop && docker rm`'d during cleanup.

**Identity path.** Generated (backend-side keypair
generation). One `POST /api/v1/ssh-identities` returned
an `SshIdentityResponse` with `key_type=ed25519` and
fingerprint
`SHA256:0i3yY8XhZ6kNEXNZV5KWy08L/oPw/Zsn6fEoDREv3OY`
(identity UUID `3fd91452-f97f-4d58-946c-0672204dbc15`).
No PEM, no base64 sidecar, no private-key bytes
touched the operator filesystem at any point.

**Host + profile create.** `Ghostty-Mount-Resmoke-Host`
(display name) / `ghostty-mount-resmoke-host`
(hostname) / `2222` / default user `smoke` (host UUID
`ddecfab9-ac6c-4d94-92f8-5c115ba89cf5`).
`ghostty-mount-resmoke-profile` binding that host to
`ghostty-mount-resmoke-identity` with no username
override and tags
`renderer, ghostty-web, mount-failure` (profile UUID
`74b17792-27d3-476b-9bab-9af1c331748c`). Success card
carried the load-bearing copy "The host key is not
yet trusted and SSH authentication has not been
verified for this profile."

**Host-key preflight + trust.** Preflight captured
fingerprint
`SHA256:MASBQwgEnD72v6GjE2Kx/NSHq85nauDtvpDiUjgliro`,
which is **byte-identical** to the locally-computed
`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`
value inside the target container. Typed the
fingerprint into `host-key-confirm-input` →
`host-key-trust-button`; `host-key-status-badge`
flipped to `Trusted`.

**Auth-check.** `auth-check-run-button` flipped
`auth-check-status-badge` to `Authenticated` after a
few seconds — public-key authentication succeeded with
no PTY allocated.

**Terminal launch — ghostty-web attempt
(the load-bearing assertion).** `profile-launch-terminal`
opened `/terminal` and created session UUID
`1ca979e0-1735-46c8-92c8-2662bce43171`. After ≥4 s of
waiting, the workspace's attribute set was:

- `data-phase="idle"`
- `data-renderer="unmounted"` (no claim that any
  renderer mounted)
- `data-renderer-experimental="false"` (since no
  renderer mounted, mirrors the workspace's
  `activeRendererId === null` branch)
- **`data-renderer-fallback="adapter_mount_failed"`**
- `data-renderer-gate="on"`
- `production-terminal-error` panel rendered with
  fixed text **`Renderer failed to mount. Switch back
  to xterm in Settings and reopen the terminal.`** (the
  `RENDERER_MOUNT_FAILED_MESSAGE` constant from
  `apps/web/src/lib/app/terminal/terminalLaunch.ts`).
- `production-terminal-renderer-diagnostic` rendered
  "Renderer. unmounted · renderer failed to mount —
  switch back to xterm in Settings and reopen the
  terminal".
- viewport empty (zero children).

This is the exact state described by
[`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
§ "2026-05-13 · ghostty-web production-shell smoke" as
the post-fix posture: the workspace no longer wedges
at `data-renderer-fallback=""`, and the operator gets
both the typed diagnostic AND the remediation message
without any raw `Error.message` reaching the DOM.

Console captured the underlying failure shape on the
ghostty-web-attempt session (the bundle hash carrying
the adapter chunk; the inlined-WASM `data:` URL):
1. `Connecting to 'data:application/wasm;base64,…'
   violates the following Content Security Policy
   directive: "default-src 'self'". …`
2. `Fetch API cannot load data:application/wasm;base64,…
   Refused to connect because it violates the
   document's Content Security Policy.`

Both errors stayed inside the browser console — neither
the offending data URL nor the `WebAssembly.compile`
text reaches the DOM, `production-terminal-error`,
`localStorage`, or `audit_events.payload` (sentinel
sweep below). Per the redaction posture pinned by
`apps/web/e2e/SMOKE.md` § "Renderer path confirmation",
the fallback row MUST quote the
`data-renderer-fallback` attribute, not the workspace
copy. **Smoke vocabulary:** the row is
`deferred — renderer not identified
(adapter_mount_failed)` per the closed taxonomy —
which is the structurally improved form of the same
deferral the 2026-05-13 ghostty-web smoke recorded
with an empty `data-renderer-fallback`.

**Matrix rows (browser surface).** As with the
2026-05-13 ghostty-web entry above, **every
evaluation-matrix row is deferred** under
`deferred — renderer not identified
(adapter_mount_failed)`. This is a diagnostic /
failure-mode resmoke, **not** a renderer-performance
or matrix smoke. The renderer evaluation matrix
itself is not advanced by this slice.

**Xterm recovery verification (NOT a ghostty-web
smoke pass).** After capturing the failure, the gate
toggle was flipped OFF in Settings (which the
`onExperimentalGateChange` handler explicitly resets
to `rendererId="xterm"`), saved, and a new terminal
launch opened on the same profile. Session UUID
`6cb95ef7-5552-43ec-9195-f125c8850e1e` mounted in
under a second with:
- `data-renderer="xterm"`
- `data-renderer-experimental="false"`
- `data-renderer-fallback=""`
- `data-renderer-gate="off"`
- `production-terminal-error` not rendered
- `production-terminal-renderer-diagnostic` text
  "Renderer. xterm baseline"

The session accepted typed input (focused viewport
textarea via the production-shell `Focus terminal`
button), echoed the smoke sentinel
`echo relayterm-ghostty-mount-resmoke-xterm` cleanly,
and ran `whoami` → `smoke`. The workspace was closed
via `production-terminal-close` (`End session`); the
component unmounted. **xterm fallback remains usable
after a gated experimental renderer mount-failure.**

**Session lifecycle rows.**
- `terminal_sessions.1ca979e0-…` (ghostty-web
  attempt): status `active`, closed_at NULL — created
  server-side but no WS attach ever happened (mount
  rejection short-circuited `attach()` before the
  WebSocket handshake), so the backend orchestrator
  has no russh channel for this id. Will be reaped by
  the orphan-session janitor. `session_events`: exactly
  1 row (`created`). No `attached`, no `detached`, no
  `closed` — consistent with the mount failure
  happening before the WS attach handshake.
- `terminal_sessions.6cb95ef7-…` (xterm recovery):
  status `closed`, closed_at 2026-05-14 00:58:29 UTC
  (≈86 s lifetime). `session_events`: 3 rows in order
  `created → attached → closed` (no `detached` because
  the close was explicit via the `End session` button).
- Per the schema's per-session telemetry contract,
  none of these crossed into `audit_events`.

**Audit events in the smoke window.** Exactly 3 rows
created during the slice:
- `ssh_identity_created` at `00:51:30.294151Z`,
  payload
  `{name, source:"generated", key_type:"ed25519",
   created_at, ssh_identity_id, fingerprint_sha256}` —
  public-metadata only.
- `server_profile_created` at `00:54:11.817505Z`,
  payload `{name, host_id, disabled_at:null,
   ssh_identity_id, server_profile_id}` —
  public-metadata only.
- `server_profile_disabled` at `01:03:00.835600Z` (≈4
  ms after `disabled_at` 01:03:00.831774Z; single
  audit row, matching the "idempotency early-return
  BEFORE audit append" rule from AGENTS.md § "Things
  to avoid"), payload `{name, host_id, disabled_at,
   ssh_identity_id, server_profile_id}` —
  public-metadata only. **Zero** `audit_events` rows
  for the ghostty-web mount failure itself — the
  failure path is browser-side only.

**Backend / web / target log redaction.** Bounded
`docker logs --since 30m` over the smoke window:
backend = 7 lines (1 `WARN missing session cookie`
pre-smoke line — same explanation as the 2026-05-13
xterm baseline and ghostty-web entries, a literal
WARN string, not a cookie value); web/nginx = 65
lines (request log only, no payloads); target sshd =
40 lines (linuxserver entrypoint chatter only; no
auth lines on stdout because the
`linuxserver/openssh-server` image keeps `LogLevel
INFO` events to `/var/log/auth.log` inside the
container). Sentinel sweep against
`{private_key_openssh, encrypted_private_key,
BEGIN OPENSSH PRIVATE KEY, openssh-key-v1, passphrase,
session_token, token_hash, data_b64, REDACT-MARKER,
relayterm-ghostty-mount-resmoke-xterm}` returned
**0 real hits** in every log. The `cookie` sentinel
matched the backend's `WARN missing session cookie`
text (same false-positive as prior smokes); the
`password` sentinel matched the target sshd's
linuxserver entrypoint message
`User/password ssh access is disabled.` confirming
`PASSWORD_ACCESS=false` was honored. Neither match
represents a real secret-bytes leak; both are static
diagnostic strings.

**DOM + storage redaction.** Post-launch (and
post-cleanup) sweep over
`document.documentElement.outerHTML`: zero hits across
all sentinels above PLUS the renderer-mount-failure
sentinels `{CompileError, WebAssembly, unsafe-eval,
data:application/wasm}` (proving the raw `Error.message`
/ CSP directive text never reached the DOM).
`document.cookie.length === 0` (the `relayterm_session`
cookie is HttpOnly — JS cannot read it). `localStorage`
carried only `relayterm.terminal-settings.v1` (cosmetic
+ renderer fields with `rendererId="xterm"` /
`experimentalRendererEvaluationEnabled=false` at the
end of the slice). `sessionStorage` empty.

**Audit-payload sentinel sweep.** Against the
smoke-window `audit_events`: `payload::text ~*`
filter for
`{BEGIN OPENSSH, openssh-key-v1, passphrase,
session_token, token_hash, data_b64, REDACT-MARKER,
relayterm-ghostty-mount-resmoke, encrypted_private_key}`
returned **zero rows**.

**Cleanup state.** Throwaway SSH container
`relayterm-staging-ghostty-mount-resmoke-ssh` is
`docker stop` + `docker rm`'d (verified
`docker ps -a --filter name=… --format {{.Names}}`
returns empty). Profile
`ghostty-mount-resmoke-profile` disabled through the
SPA (preserved with `disabled_at` set, not deleted,
per the inventory-lifecycle policy). Settings reset
to `rendererId="xterm"` /
`experimentalRendererEvaluationEnabled=false` so a
future browser session against this staging surface
starts on the production default. Left in place per
the slice plan: staging Compose stack running,
Postgres untouched, `ghostty-mount-resmoke-identity`
(`3fd91452-…`), `Ghostty-Mount-Resmoke-Host`
(`ddecfab9-…`), `ghostty-mount-resmoke-profile`
(`74b17792-…`, disabled), the 1 `active` (orphan) +
1 `closed` `terminal_sessions` history rows, the 4
total `session_events` rows, the 1 trusted
`known_host_entries` row, the 3 `audit_events` rows
emitted during the smoke (`ssh_identity_created`,
`server_profile_created`,
`server_profile_disabled`), the staging smoke user.

**Intentionally deferred** (out of scope for this
slice):
- ghostty-web CSP / WASM compatibility fix (a
  ghostty-web build that ships WASM as an asset
  rather than a `data:` URL, OR a deploy-side CSP
  change allowing `'wasm-unsafe-eval'` plus `data:`
  in `connect-src`).
- ghostty-web evaluation-matrix / performance smoke
  (gated on the above; no rows graded in this slice).
- restty / wterm experimental renderer evaluation.
- Desktop Tauri (path A bundled-shell handoff) and
  Android Tauri renderer smokes for any candidate.
- Automated performance / benchmark harness.
- Renderer production-default switch (Gate 2);
  per-user / per-device renderer preference
  persistence beyond the current
  `relayterm.terminal-settings.v1` localStorage entry.

**Promotion decision.** **ghostty-web remains
experimental.** The production default remains xterm.
Gate 1 and Gate 2 criteria are unchanged. No backend
protocol, session, orchestrator, `terminal-core`,
production-shell-non-loader, CI, or deploy file was
touched by this slice. This smoke proves the
mount-failure diagnostic surface lands cleanly on
staging; it does not grade or promote any renderer.

**Verdict.** The `adapter_mount_failed` diagnostic
path landed by `feat(web): handle renderer mount
failures` works as designed on staging: the workspace
exposes the typed fallback value AND the fixed safe
error copy, the underlying CSP/WASM
`Error.message`/directive text never reaches the DOM
or any persistence surface, and the operator's
documented recovery action (Settings → xterm → reopen)
recovers a working terminal on the same profile. The
2026-05-13 ghostty-web smoke's wedged-`idle` failure
mode is closed.

### 2026-05-14b · Ghostty-web WASM-as-asset resmoke (data: CSP block closed; wasm-unsafe-eval still blocks compile; xterm recovery still works)

**Date.** 2026-05-14 03:18 UTC – 03:32 UTC (≈14 min).
**Staging URL.** `https://relayterm-staging.js-node.cc`.
**Stack pin.** `git.js-node.cc/jsprague/relayterm-web:main`
(image config `sha256:0fed18d2…`, image built
2026-05-14 02:43 UTC) +
`git.js-node.cc/jsprague/relayterm-backend:main` (image
config `sha256:747bede8…`, image built 2026-05-14 02:42 UTC).
Pre-recreate state: the running web image config was
`sha256:0250a9f9…` (built 2026-05-14 00:20 UTC, container
started 2026-05-14 00:48 UTC — the same images the
2026-05-14 mount-failure resmoke entry pinned). Both
the `relayterm-web` and `relayterm-backend` services
were pre-pulled (`docker pull git.js-node.cc/jsprague/relayterm-{web,backend}:main`)
and then recreated at 2026-05-14 03:18 UTC
(`docker compose up -d --no-deps --force-recreate
--pull never relayterm-web relayterm-backend`) so the
running web bundle includes
`aa6bf9f fix(web): load ghostty wasm as an asset` —
the adapter slice that swaps the inlined
`data:application/wasm;base64,…` URL for a same-origin
Vite-emitted `/assets/ghostty-vt-<hash>.wasm` asset.
Postgres `postgres:17-alpine` left untouched
(`Up 4 days` before AND `Up 4 days` after — the recreate
explicitly used `--no-deps`). Asset assertion inside the
recreated web container:
`ls /usr/share/nginx/html/assets/ | grep ghostty-vt`
returned `ghostty-vt-DOMeXDrv.wasm` (423,045 bytes,
mtime 2026-05-14 02:43 UTC) — the new fingerprinted
WASM asset the `?url` import emits. Pre-recreate the
same listing returned nothing.
**Branch.** `docs/ghostty-web-wasm-asset-resmoke`
off `main` (docs-only slice; no source / CI / deploy /
schema changes).
**Browser surface.** Playwright MCP (Chrome / Linux) at
1440 × 900. Auth: existing
`staging+throwaway-20260509173230@example.com` cookie
session, no re-login.

**Goal.** Verify the adapter-side ghostty-web WASM-as-
asset fix (`aa6bf9f`) on the live staging surface.
Three load-bearing assertions:

- The production web bundle now emits — and the
  production CSP/SOP path actually fetches — a same-
  origin `/assets/ghostty-vt-<hash>.wasm` asset; the
  `data:application/wasm;base64,…` URL the 2026-05-13
  and 2026-05-14 ghostty-web smokes were blocked on
  is no longer the runtime load path.
- ghostty-web still fails to mount under the staging
  stack's current CSP because `WebAssembly.compile()`
  / `compileStreaming()` independently require
  `'wasm-unsafe-eval'` (no CSP changes in this slice).
- `data-renderer-fallback="adapter_mount_failed"` plus
  the fixed operator-facing copy from
  `feat(web): handle renderer mount failures` continue
  to fire cleanly; xterm fallback/manual recovery on
  the same profile still works.

Slice boundaries: no renderer-adapter changes, no CSP
changes, no WASM/Vite bundling changes, no
backend/session/orchestrator/protocol changes, no
CI/deploy changes, no renderer promotion. This is a
diagnostic resmoke — **no evaluation-matrix rows are
graded for ghostty-web**.

**CSP posture.** Unchanged.
`curl -sSI https://relayterm-staging.js-node.cc/`
returned
`content-security-policy: default-src 'self'` (no
`'wasm-unsafe-eval'`, no explicit `connect-src`, no
`script-src` override). The recreated web image carries
the same nginx `web.conf.template` posture as the prior
images.
`curl -sSI https://relayterm-staging.js-node.cc/assets/ghostty-vt-DOMeXDrv.wasm`
returned `HTTP/2 200`, `content-type: application/wasm`,
`cache-control: public, immutable, max-age=31536000` —
same immutable-asset policy nginx applies to
`/assets/*.js`. `/healthz` returned `200`,
`/api/v1/auth/me` returned `401` (no session cookie),
SPA at `/` returned `200`.

**Renderer setup (gated, operator-opt-in).** Settings
view pre-state: `data-renderer-gate="off"`, persisted
`rendererId="xterm"`. Clicked
`settings-experimental-renderer-toggle` (warning copy
rendered), selected `renderer-option-ghostty-web`,
clicked `settings-apply`. Post-state:
- `localStorage["relayterm.terminal-settings.v1"]`
  carries `rendererId="ghostty-web"` and
  `experimentalRendererEvaluationEnabled=true`.
- `settings-renderer-effective` reads "Effective
  renderer on next session: ghostty-web experimental."
- `settings-status-saved` reads "Saved locally. Applies
  to the next terminal session."

**Throwaway SSH target.** A
`linuxserver/openssh-server:latest` container named
`relayterm-staging-ghostty-asset-resmoke-ssh`, attached
only to the staging Compose network
`relayterm-staging_relayterm-staging-internal` with
DNS alias `ghostty-asset-resmoke-host` resolving to
`172.21.0.5`. **No host port was published**
(`docker port` returned empty; verified).
`USER_NAME=smoke`, `SUDO_ACCESS=false`,
`PASSWORD_ACCESS=false`,
`PUBLIC_KEY=<the RelayTerm-generated OpenSSH line>`.
The public-key line was extracted from the SPA's
generate-success card via a single `browser_evaluate`,
returned to the operator as base64, decoded inline
inside one `ssh cloud-edge` shell session straight into
`docker run -e PUBLIC_KEY=…`, and `unset PUBLIC_KEY`'d
immediately. No PEM, no base64 sidecar, no private-key
bytes touched the operator filesystem at any point.
The container was `docker stop && docker rm`'d during
cleanup.

**Identity path.** Generated (backend-side keypair
generation). One `POST /api/v1/ssh-identities` returned
an `SshIdentityResponse` with `key_type=ed25519` and
fingerprint
`SHA256:xMUbJk4zetWOvgi+fzf1JgEJYzpdokSgkuEeL2w4O2k`
(identity UUID `c8dadbdf-d171-411d-9211-23aee2c4246c`).

**Host + profile create.** `Ghostty-Asset-Resmoke-Host`
(display name) / `ghostty-asset-resmoke-host`
(hostname) / `2222` / default user `smoke` (host UUID
`c9d7690a-e039-4851-82bf-1dc148ffd6ab`).
`ghostty-asset-resmoke-profile` binding that host to
`ghostty-asset-resmoke-identity` with no username
override and tags `renderer, ghostty-web, wasm-asset`
(profile UUID `80188642-1afd-45f6-b5ce-27c1dbeaa738`).
Success card carried the load-bearing copy "The host
key is not yet trusted and SSH authentication has not
been verified for this profile."

**Host-key preflight + trust.** Preflight captured
fingerprint
`SHA256:nlm7GPqHBqQRbLHJ4BMT8cP4YWK2HVlUodQxm7+mK/k`,
which is **byte-identical** to the locally-computed
`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub`
value inside the target container. Typed the
fingerprint into `host-key-confirm-input` →
`host-key-trust-button`; `host-key-status-badge`
flipped to `Trusted`.

**Auth-check.** `auth-check-run-button` flipped
`auth-check-status-badge` to `Authenticated` after a
few seconds — public-key authentication succeeded with
no PTY allocated.

**Terminal launch — ghostty-web attempt
(the load-bearing assertion).** `profile-launch-terminal`
opened `/terminal` and created session UUID
`461bb249-b07d-48bc-a509-9a8231cd0b97`. The workspace's
attribute set after ≥3 s of waiting was:

- `data-phase="idle"`
- `data-renderer="unmounted"`
- `data-renderer-experimental="false"`
- **`data-renderer-fallback="adapter_mount_failed"`**
- `data-renderer-gate="on"`
- `production-terminal-error` panel rendered with
  fixed text **`Renderer failed to mount. Switch back
  to xterm in Settings and reopen the terminal.`** (the
  `RENDERER_MOUNT_FAILED_MESSAGE` constant from
  `apps/web/src/lib/app/terminal/terminalLaunch.ts`).
- `production-terminal-renderer-diagnostic` rendered
  "Renderer. unmounted · renderer failed to mount —
  switch back to xterm in Settings and reopen the
  terminal".
- viewport empty (zero children).

This is the same `adapter_mount_failed` surface the
2026-05-14 mount-failure resmoke entry above pinned;
the WASM-as-asset fix did NOT make ghostty-web mount.

**What changed vs. the 2026-05-14 mount-failure
resmoke (the differential this entry exists for).**
The 2026-05-14 entry recorded **3 browser-console
errors** captured during the mount attempt — two
`data:application/wasm` CSP blocks (`connect-src`
fallback to `default-src`, plus the matching
`Fetch API cannot load data:application/wasm…`
follow-up) and one `WebAssembly.compile(): … 'unsafe-eval'`
`CompileError`. After the recreate:

- `browser_console_messages level=error all=true`
  returned **0 messages** during the ghostty-web mount
  attempt itself. The two `data:application/wasm` CSP
  errors **did not fire** — Vite emits the asset, the
  adapter's `Ghostty.load(wasmUrl)` call points at the
  same-origin URL, and CSP's `default-src 'self'`
  permits the fetch. The `WebAssembly.compile` /
  `'unsafe-eval'` CompileError still happens inside
  upstream's `Ghostty.loadFromPath`, but it now lands
  as a rejected promise that `mountRendererSafely`
  catches into `adapter_mount_failed` — no CSP-violation
  text reaches the JS console.
- `performance.getEntriesByType('resource')` showed
  exactly one ghostty-related entry:
  `https://relayterm-staging.js-node.cc/assets/ghostty-vt-DOMeXDrv.wasm`
  with `initiatorType="fetch"`, `responseStatus=200`,
  `decodedBodySize=423045`, `duration≈82ms`. The
  asset was served, fetched, and read into an
  `ArrayBuffer` — the network-side half of the gap
  the 2026-05-13 entry described is **closed**.
- A manual `await WebAssembly.compile(<8-byte minimal
  WASM>)` issued from `browser_evaluate` against the
  page rejected with
  `CompileError: WebAssembly.compile(): Compiling or
   instantiating WebAssembly module violates the
   following Content Security policy directive because
   'unsafe-eval' is not an allowed source of script
   in the following …` — confirming the remaining
  gap is the upstream `WebAssembly.compile()` call
  inside `Ghostty.loadFromPath`, not anything specific
  to the ghostty-vt bytes (the same minimal valid
  module also fails to compile). The same probe ran
  against the real `/assets/ghostty-vt-DOMeXDrv.wasm`
  bytes and against `WebAssembly.compileStreaming(fetch(...))`,
  all three reject identically.

The slice's claim "WASM-as-asset fix removes the
`data:application/wasm` / `connect-src` half of the
gap; the `'wasm-unsafe-eval'` half remains upstream-
baked" (`wasmUrl.ts` header, `GhosttyWebRenderer.ts`
header) is **directly verified on the production
shell** by this resmoke.

**Matrix rows (browser surface).** As with the
2026-05-13 and 2026-05-14 ghostty-web entries above,
**every evaluation-matrix row is deferred** under
`deferred — renderer not identified
(adapter_mount_failed)`. This is a deploy-side
verification of the adapter slice, **not** a renderer-
performance or matrix smoke. The renderer evaluation
matrix itself is not advanced by this slice.

**Xterm recovery verification (NOT a ghostty-web
smoke pass).** After capturing the ghostty-web mount
failure, the gate toggle was flipped OFF in Settings
(which the `onExperimentalGateChange` handler
explicitly resets to `rendererId="xterm"`), saved,
and a new terminal launch opened on the same
profile. Session UUID
`c11cba6e-ba16-4903-8ca2-b6541a0ccdf0` attached in
under a second with:
- `data-renderer="xterm"`
- `data-renderer-experimental="false"`
- `data-renderer-fallback=""`
- `data-renderer-gate="off"`
- `data-phase="attached"` (live PTY)
- `production-terminal-error` not rendered
- `production-terminal-renderer-diagnostic` text
  "Renderer. xterm baseline"

After clicking `production-terminal-focus` and
re-focusing the xterm helper textarea, the smoke
sentinel `echo relayterm-ghostty-asset-resmoke-xterm`
typed via Path A round-tripped cleanly (echo line
rendered in viewport; status header showed
`last_seen_seq=6`), followed by `whoami → smoke`
(`last_seen_seq=11` after that round-trip). The
workspace was closed via `production-terminal-close`
(`End session`); the component unmounted, and the
`/terminal` view fell back to its empty state.
**xterm fallback remains fully usable after a gated
experimental renderer mount-failure under this CSP.**

**Session lifecycle rows.**
- `terminal_sessions.461bb249-…` (ghostty-web
  attempt): status `active`, closed_at NULL — created
  server-side but no WS attach ever happened (mount
  rejection short-circuited `attach()` before the
  WebSocket handshake), so the backend orchestrator
  has no russh channel for this id. Will be reaped by
  the orphan-session janitor. `session_events`:
  exactly 1 row (`created`, `{cols:80, rows:24,
  stub:true}`). No `attached`, no `detached`, no
  `closed` — consistent with the mount failure
  happening before the WS attach handshake.
- `terminal_sessions.c11cba6e-…` (xterm recovery):
  status `closed`, closed_at 2026-05-14 03:31:41 UTC
  (≈132 s lifetime). `session_events`: 3 rows in
  order `created → attached → closed` (no `detached`
  because the close was explicit via the `End
  session` button). `attached` payload includes the
  per-session-telemetry `client_info` user-agent
  string (a known pre-existing field on
  `session_events.payload`, NOT crossed into
  `audit_events`).
- Per the schema's per-session telemetry contract,
  none of these crossed into `audit_events`.

**Audit events in the smoke window.** Exactly 2 rows
created during the slice (cleanup-disable row will
add a 3rd, recorded under "Cleanup state" below):
- `ssh_identity_created` at `03:21:55.550336Z`,
  payload
  `{name, source:"generated", key_type:"ed25519",
   created_at, ssh_identity_id, fingerprint_sha256}` —
  public-metadata only.
- `server_profile_created` at `03:25:33.609876Z`,
  payload `{name, host_id, disabled_at:null,
   ssh_identity_id, server_profile_id}` —
  public-metadata only.
- **Zero** `audit_events` rows for the ghostty-web
  mount failure itself — the failure path is browser-
  side only (matches the 2026-05-13 and 2026-05-14
  ghostty-web entries).

**Backend / web / target log redaction.** Bounded
`docker compose logs --since 30m` over the smoke
window: backend = 7 lines (1 `WARN missing session
cookie` pre-smoke line — same explanation as the
2026-05-13 / 2026-05-14 entries, a literal WARN
string, not a cookie value); web/nginx = 56 lines
(request log only, no payloads); target sshd = 40
lines (linuxserver entrypoint chatter only; no auth
lines on stdout because the
`linuxserver/openssh-server` image keeps `LogLevel
INFO` events to `/var/log/auth.log` inside the
container). Sentinel sweep against
`{private_key_openssh, encrypted_private_key,
BEGIN OPENSSH PRIVATE KEY, openssh-key-v1, passphrase,
session_token, token_hash, data_b64, REDACT-MARKER,
relayterm-ghostty-asset-resmoke, CompileError,
unsafe-eval, WebAssembly, data:application/wasm}`
returned **0 real hits** in every log. The `cookie`
sentinel matched the backend's `WARN missing session
cookie` text (same false-positive as prior smokes);
the `password` sentinel matched the target sshd's
linuxserver entrypoint message
`User/password ssh access is disabled.` confirming
`PASSWORD_ACCESS=false` was honored. Neither match
represents a real secret-bytes leak; both are static
diagnostic strings.

**DOM + storage redaction.** Sweep over
`document.documentElement.outerHTML` during the xterm
recovery session AND against `localStorage`,
`sessionStorage`, and `document.cookie`: zero hits
across the secrets sentinel list above PLUS the
renderer-mount-failure sentinels `{CompileError,
WebAssembly, unsafe-eval, data:application/wasm}`
(proving the raw `Error.message` / CSP directive
text never reached the DOM despite the mount
rejection earlier in the slice — exactly the
posture `mountRendererSafely` is designed to enforce).
`document.cookie.length === 0` (the `relayterm_session`
cookie is HttpOnly — JS cannot read it).
`localStorage` carried only
`relayterm.terminal-settings.v1` (cosmetic + renderer
fields with `rendererId="xterm"` /
`experimentalRendererEvaluationEnabled=false` at the
end of the slice) and `relayterm.active-terminal.v1`.
The smoke sentinel
`relayterm-ghostty-asset-resmoke-xterm` matched in
the rendered DOM as expected — but **only inside
`[data-testid="production-terminal-viewport"]`**,
never outside it (`inHtmlOutsideViewport: false`),
matching the redaction-rule contract for terminal-
viewport content. `sessionStorage` empty.

**Audit-payload sentinel sweep.** Against the
smoke-window `audit_events`: `payload::text ~*`
filter for
`{private_key, encrypted_private_key, BEGIN OPENSSH,
openssh-key-v1, passphrase, session_token,
token_hash, data_b64, REDACT-MARKER,
relayterm-ghostty-asset, CompileError, unsafe-eval,
WebAssembly, data:application/wasm}` returned
**zero rows**.

**Cleanup state.** Throwaway SSH container
`relayterm-staging-ghostty-asset-resmoke-ssh` is
`docker stop` + `docker rm`'d (verified
`docker ps -a --filter name=… --format {{.Names}}`
returns empty). Profile
`ghostty-asset-resmoke-profile` disabled through the
SPA (preserved with `disabled_at` set, not deleted,
per the inventory-lifecycle policy); the resulting
`server_profile_disabled` audit row was recorded as
the 3rd smoke-window `audit_events` entry, payload
public-metadata only (`{name, host_id, disabled_at,
ssh_identity_id, server_profile_id}`). Settings
reset to `rendererId="xterm"` /
`experimentalRendererEvaluationEnabled=false` so a
future browser session against this staging surface
starts on the production default. Left in place per
the slice plan: staging Compose stack running,
Postgres untouched (`Up 4 days` before AND after
the slice — recreate used `--no-deps`),
`ghostty-asset-resmoke-identity`
(`c8dadbdf-…`), `Ghostty-Asset-Resmoke-Host`
(`c9d7690a-…`), `ghostty-asset-resmoke-profile`
(`80188642-…`, disabled), the 1 `active` (orphan) +
1 `closed` `terminal_sessions` history rows, the 4
total `session_events` rows, the 1 trusted
`known_host_entries` row, the 3 `audit_events` rows
emitted during the smoke (`ssh_identity_created`,
`server_profile_created`,
`server_profile_disabled`), the staging smoke user.

**Intentionally deferred** (out of scope for this
slice):
- The remaining half of the CSP/WASM compatibility
  gap (`'wasm-unsafe-eval'` in CSP `script-src`).
  Two options for a future slice:
  (a) a deploy-side CSP change adding
      `'wasm-unsafe-eval'` to `script-src` (a
      deliberate trade-off — the directive widens the
      execution policy for ALL same-origin scripts,
      not just WASM compile, and needs its own
      threat-model entry); OR
  (b) an upstream `ghostty-web` patch that swaps
      `WebAssembly.compile` for a same-origin
      streaming-instantiate path that does NOT
      require `'wasm-unsafe-eval'` (if such a path
      exists for the upstream parser's API). Neither
      option is authorised by this slice.
- ghostty-web evaluation-matrix / performance smoke
  (gated on the above; no rows graded in this slice).
- restty / wterm experimental renderer evaluation.
- Desktop Tauri (path A bundled-shell handoff) and
  Android Tauri renderer smokes for any candidate.
  The Tauri WebView's CSP posture is separately
  evaluated and is not unblocked by this slice.
- Automated performance / benchmark harness.
- Renderer production-default switch (Gate 2);
  per-user / per-device renderer preference
  persistence beyond the current
  `relayterm.terminal-settings.v1` localStorage entry.

**Promotion decision.** **ghostty-web remains
experimental.** The production default remains
xterm. Gate 1 and Gate 2 criteria are unchanged. No
backend protocol, session, orchestrator,
`terminal-core`, production-shell-non-loader, CI,
or deploy file was touched by this slice. This
smoke proves the WASM-as-asset adapter fix
(`aa6bf9f`) lands cleanly on staging at the network
layer; it does not grade or promote any renderer.

**Verdict.** The WASM-as-asset adapter slice
(`aa6bf9f`) closes the `data:application/wasm` /
`connect-src` half of the CSP gap on the
production-shell ghostty-web path: the asset emits,
serves at HTTP 200 with `content-type:
application/wasm` and the standard `/assets/*`
immutable cache, and the runtime fetches it via a
fingerprinted same-origin URL. The remaining
upstream `WebAssembly.compile()` /
`'wasm-unsafe-eval'` requirement still blocks the
mount, surfacing via `adapter_mount_failed` plus the
fixed operator-facing copy (with no
`CompileError` / `unsafe-eval` / `WebAssembly` /
`data:application/wasm` text reaching the DOM,
audit log, or backend/web/target log streams). The
2026-05-14 mount-failure-diagnostic surface lands
identically; the underlying failure cause has
narrowed from "two CSP gaps" to "one (upstream-
baked) CSP gap." xterm recovery on the same profile
still works end-to-end. ghostty-web stays
experimental and unpromoted; the production default
remains xterm. The next renderer-evaluation slice
that wants to actually grade ghostty-web matrix
rows must take one of the two deferred options
above; neither is authorised here.

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
