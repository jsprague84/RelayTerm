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

A short, append-only record of when this runbook was actually walked
end-to-end against the live VPS slot. Each entry pins what was
verified, what was deferred, and any drift between the runbook and
observed behaviour worth folding into the next iteration.

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
