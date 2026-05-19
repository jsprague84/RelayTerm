# RelayTerm — Manual production deployment runbook

> Operator-facing checklists for the server/web Docker deployment path.
> Read this end-to-end before your first production deploy. Re-read the
> relevant section before every upgrade or rollback.

The companion document
[`docs/deployment/docker-compose.md`](./docker-compose.md) is the
**setup and reference** for the Compose stack: what each service is,
how the env contract is wired, what the CI workflow does. This file is
the **runbook** — what an operator actually does, in order, with
commands they can copy to a terminal.

When the two disagree, the deployed code wins; file the drift as a
bug.

---

## 1. Purpose and scope

This runbook covers:

- The **server/web Docker deployment** track only — the
  `relayterm-backend`, `relayterm-backend-migrate`, and `relayterm-web`
  images plus a Postgres container (or an external managed Postgres).
- First production deploy, normal upgrade, rollback by immutable tag,
  the migration-before-upgrade flow, backup reminders, reverse-proxy
  prerequisites, secret-rotation notes, and post-deploy smoke checks.

This runbook does **not** cover:

- Auto-deploy (see §13). Every deploy here is operator-triggered.
- The Tauri v2 desktop and mobile (Android-first) shells. Those live
  under `apps/desktop/` and `apps/mobile/` and have their own release
  tracks; no CI workflow exists for them yet. The staged plan for that
  future work lives in
  [`docs/deployment/tauri-ci-release-plan.md`](./tauri-ci-release-plan.md).
- Kubernetes / Helm / Nomad, multi-node HA, zero-downtime rolling
  deploys, image signing, SBOM / vulnerability scanning, registry
  retention automation, multi-arch images, managed-secrets integrations,
  or backup automation. Each is a separate slice — see §13 for the
  ledger.

---

## 2. Required artifacts

Before you start, gather:

**Published OCI images** (Forgejo container registry,
`git.js-node.cc/jsprague/...`):

| Image | Purpose |
|---|---|
| `git.js-node.cc/jsprague/relayterm-backend:<tag>` | Rust/Axum backend (the runtime container). |
| `git.js-node.cc/jsprague/relayterm-backend-migrate:<tag>` | One-shot `sqlx migrate` runner; same source tree as the backend, published in lockstep. |
| `git.js-node.cc/jsprague/relayterm-web:<tag>` | nginx serving the SPA bundle and reverse-proxying `/api` (with WS upgrade) and `/healthz` to the backend. |

The `<tag>` for all three must match — they are built from the same
commit. See §3 for tag-policy guidance.

**Compose / env templates** (in this repo):

- [`deploy/docker-compose.production.example.yml`](../../deploy/docker-compose.production.example.yml)
  — the **production-oriented Compose template** (image-mode, no
  reverse-proxy hardcoding, ships with a commented Traefik labels
  block and a Caddy / outer-nginx alternative documented inline).
  This is the file the v1 release-checklist § 4 / § 5 walk against
  on the operator's production host; copy it to the deploy host as
  `docker-compose.yml`.
- [`deploy/docker-compose.images.example.yml`](../../deploy/docker-compose.images.example.yml)
  — the original image-mode reference. Equivalent service shape;
  kept as the canonical minimal-comment file. Either file works on
  the production host; new deploys should prefer the production
  template above.
- [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
  — env template with every required variable annotated. Operators
  copy this to `.env` and fill it in.
- [`deploy/nginx/web.conf.template`](../../deploy/nginx/web.conf.template)
  — referenced for outer-proxy setup; baked into the `relayterm-web`
  image.

**Operator-side artifacts** (NOT in this repo):

- A production `.env` file stored **outside** git, on the deploy host
  only, with strict file permissions (`chmod 600`).
- A Postgres data directory or volume, under regular off-site backup
  (§8). The default Compose stack uses a named docker volume
  `relayterm-pgdata`; an external managed Postgres is the supported
  alternative (see `docker-compose.md` §4.4).
- A reverse proxy (Traefik / Caddy / outer nginx) terminating TLS for
  the public origin — see §9.
- A Forgejo personal access token with `read:package` scope on the
  deploy host, for `docker login git.js-node.cc`. The token used in CI
  is `write:package` and **MUST NOT be reused** on the deploy host.

---

## 3. Tag policy

The CI workflow publishes three tag shapes; pick the right one for the
right job.

| Tag shape | When to use | Mutable? |
|---|---|---|
| `:vX.Y.Z` | Tagged releases. The recommended pin for production. | Immutable per release. |
| `:sha-<short>` | An exact commit's build. The recommended pin for production *between* releases, and the rollback target. | Immutable. |
| `:main` | Branch-tracking dev / staging only. Not for production pinning. | **Mutable** — re-points on every push to `main`. |
| `:latest` | Does not exist. The CI deliberately does not publish it; pinning to a floating tag is a footgun when combined with `docker compose pull`. | n/a |

**Rule of thumb.** Production deploys should pin
`RELAYTERM_IMAGE_TAG=vX.Y.Z` (when a release tag is available) or
`RELAYTERM_IMAGE_TAG=sha-<short>` (when deploying an in-between
commit). Rolling back means setting `RELAYTERM_IMAGE_TAG` to a
previously known-good value of either shape and running
`docker compose pull` + `up -d`.

`:main` is fine for a staging environment that *wants* to track the
branch tip. Don't pin it on a production host.

---

## 4. First production deploy checklist

Do this **once**, on a fresh host, with no existing RelayTerm state.

> **First personal deploy?** Read
> [`docs/deployment/first-production-deploy-plan.md`](first-production-deploy-plan.md)
> first — it is the short, opinionated planning page that says
> what to decide (§2), what the recommended posture is (§3), and
> what counts as "ready for personal use" (§7). It composes this
> runbook + the backup-restore runbook + the v1 release checklist
> into a single page; this §4 stays the load-bearing procedure.

### 4.1 Choose a host directory

Pick a directory the operator account owns; this is where the Compose
file and `.env` will live. Example: `/srv/relayterm/`.

```sh
sudo mkdir -p /srv/relayterm
sudo chown "$USER":"$USER" /srv/relayterm
cd /srv/relayterm
```

### 4.2 Copy the templates

From a clone of this repo (or via a release artifact):

```sh
cp /path/to/RelayTerm/deploy/docker-compose.production.example.yml \
   /srv/relayterm/docker-compose.yml
cp /path/to/RelayTerm/deploy/relayterm.env.example \
   /srv/relayterm/.env
chmod 600 /srv/relayterm/.env
```

The production template (`docker-compose.production.example.yml`) is
the recommended starting point — it ships with the upgrade /
rollback / backup comments inline and a commented Traefik labels
block you can adapt to your reverse proxy. The original
`docker-compose.images.example.yml` is equivalent in service shape
and is still valid; pick whichever the operator prefers.

The `chmod 600` is load-bearing: this file ends up holding the session
signing key, the vault master key, and the database password.

### 4.3 Generate secrets

On a trusted machine, generate three independent random secrets. **Do
not reuse the session signing key as the vault master key** — a
disclosure of one MUST NOT compromise the other.

```sh
# Session signing key. 32 random bytes, base64. The boot-time
# validator requires this to be set; it is reserved for the future
# signed-CSRF / signed-cookie scheme and is NOT yet consumed by the
# v1 hashed-opaque-token session model. Rotation has no live-session
# impact today (see §11.1).
openssl rand -base64 32

# Vault master key (AEAD-wraps stored SSH private keys). 32 random
# bytes, base64. MUST be a different value from the session signing
# key.
openssl rand -base64 32

# First-user bootstrap token (URL-safe, no padding).
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='

# Postgres password. A long random string is fine.
openssl rand -base64 24
```

Paste each into the matching field in `.env`. The example file flags
every `CHANGE_ME_*` placeholder.

**Suppress shell history while you do this.** The four secrets above
are plaintext at the point of generation; if your shell records history,
the `openssl` invocations and any subsequent `echo` / `cat` of `.env`
end up on disk. The auth-smoke runbook
([`docs/auth-smoke.md`](../auth-smoke.md) "Prerequisites") documents
the per-shell suppression pattern (`set +o history` for bash/zsh, the
private-mode equivalent for fish). At minimum, generate the values
into a file the operator owns (`umask 077; openssl rand -base64 32 >
/tmp/sk; ...`), copy them into `.env`, then `shred -u` the temporary
files.

### 4.4 Verify the production envelope

In `.env`, confirm:

> **Boot-time placeholder refusal.** Production mode refuses to
> start if any of the secret-shaped fields below still contains
> the literal `CHANGE_ME` substring from
> `deploy/relayterm.env.example` — covering session signing key,
> bootstrap token, vault master key, recording master key, and
> the Postgres password embedded in `RELAYTERM_DATABASE__URL`.
> The boot error names the failing field but never echoes the
> placeholder value. If you hit this, generate a real value per
> the matching bullet below — do NOT work around the refusal
> (every refused value would have shipped a known weak secret
> into the deploy).

- `RELAYTERM_AUTH__MODE=production`. Never deploy `dev` to a public
  host — the boot validator's relaxed rules in `dev` mode are a
  development-only ergonomic.
- `RELAYTERM_AUTH__ALLOWED_ORIGINS` matches the public origin
  **byte-for-byte** the browser will use: `scheme://host[:port]`,
  lowercase, no trailing slash, no path. The CSRF guard does
  byte-equality. Comma-separate if you publish from more than one
  origin.
- `RELAYTERM_AUTH__COOKIE_SECURE=true`. The session cookie carries the
  `Secure` flag — required behind HTTPS, mandatory in production.
- `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64` is set and **not equal to**
  `RELAYTERM_VAULT__MASTER_KEY_B64`.
- `RELAYTERM_VAULT__ENABLED=true` for any deploy that lets users add
  SSH identities. Disabled mode is supported but `POST
  /api/v1/ssh-identities` returns 503 until the vault is wired up.
- `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` is set to the random
  token from §4.3. This is the ONLY way to create the first user. After
  bootstrap, you will unset and restart (§4.9).
- `RELAYTERM_TERMINAL_RECORDING__ENABLED` matches your privacy posture
  for this deploy. Read [`docs/terminal-recording.md`](../terminal-recording.md)
  Section 7 before flipping it on. Cleanup knobs are honoured even
  when recording itself is off.

### 4.5 Pin the image tag

Pick the tag from §3 and add it to `.env`:

```sh
echo 'RELAYTERM_IMAGE_TAG=v0.1.0' >> .env       # release pin
# or
echo 'RELAYTERM_IMAGE_TAG=sha-abc1234' >> .env  # immutable commit pin
```

### 4.6 Log in to the registry

Use a Forgejo PAT with **`read:package` scope only** on the deploy
host. The CI's `write:package` token MUST NOT be copied here.

```sh
docker login git.js-node.cc
# Username: <your forgejo username>
# Password: <paste the read:package PAT>
```

If your registry visibility allows anonymous pulls, this step can be
skipped — but issue the read-only PAT anyway, so a future tightening
of registry visibility does not break pulls without warning.

### 4.7 Render and pull

Confirm the rendered Compose config is what you expect, then pull:

```sh
docker compose config           # prints the merged config; sanity-check env interpolation
docker compose pull             # fetch all three images at the pinned tag
```

`docker compose config` will fail loudly if a required env var is
missing — every `${...:?}` placeholder in the example Compose file
errors with the operator-facing hint.

### 4.8 Apply migrations before starting the backend

The backend does **not** auto-migrate on boot. The migrate container
is a one-shot, profile-gated service:

```sh
docker compose --profile migrate run --rm relayterm-migrate
```

This invokes `sqlx migrate run --source /app/migrations` against the
Postgres instance. Re-running is idempotent — already-applied
migrations are skipped. For status only:

```sh
docker compose --profile migrate run --rm relayterm-migrate \
    migrate info --source /app/migrations
```

Never start the backend without running the migrate container first.

### 4.9 Start the stack

```sh
docker compose up -d postgres relayterm-backend relayterm-web
docker compose ps
```

Compose waits for `postgres` to become `healthy`, then starts the
backend, then starts the web container. All three should reach
`running` (and where configured, `healthy`).

### 4.10 Bootstrap the first user

With the bootstrap token configured in §4.4 and the stack running,
POST to `/api/v1/auth/bootstrap` through the public origin. The full
flow with audit-row verification is in
[`docs/production-auth.md`](../production-auth.md) §4 and the
end-to-end smoke procedure is in
[`docs/auth-smoke.md`](../auth-smoke.md).

The short version (run from a workstation with HTTPS reachability):

```sh
# Replace the public origin and read the bootstrap token from a file
# the operator owns. Never paste a real token into a shell history.
curl -fsS -X POST \
  -H 'Content-Type: application/json' \
  -H "Origin: https://relayterm.example.com" \
  --data-binary @- \
  https://relayterm.example.com/api/v1/auth/bootstrap <<'JSON'
{
  "bootstrap_token": "<read from a file>",
  "email": "you@example.com",
  "display_name": "you",
  "password": "<your password>"
}
JSON
```

A `201 Created` with the new user record means bootstrap succeeded.
Bootstrap does **not** mint a session — follow up with `POST
/api/v1/auth/login` to get the cookie, or just sign in through the
browser.

### 4.11 Close the bootstrap window

Once a user exists, **unset the bootstrap token and restart the
backend**:

```sh
sed -i '/^RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN=/d' .env
docker compose up -d --no-deps relayterm-backend
```

A subsequent `POST /api/v1/auth/bootstrap` now returns `503` (token
unset) or `409 already_bootstrapped` (token still set but a user
exists). Either is safe; the unset-and-restart path is the cleaner
end state.

### 4.12 Run the post-deploy smoke

Walk §10 end-to-end. A green smoke is the gate that signals the
deploy is operational.

---

## 5. Upgrade checklist

Use this every time you change `RELAYTERM_IMAGE_TAG` to a newer build.

1. **Back up Postgres first** (§8). The forward-only migration in the
   new image cannot be cleanly reverted on a running database without
   either a pre-rehearsed `sqlx migrate revert` plan or a backup-and-
   restore.

2. **Pick the new tag** from §3 — `:vX.Y.Z` for a release,
   `:sha-<short>` for an in-between commit.

3. **Read the release notes / commit log** for the range you are
   upgrading across. Look for: schema changes (the `migrate` step
   below covers them, but operator-side env additions don't), env
   contract changes (new required `RELAYTERM_*` keys), and any items
   flagged "operator action required."

4. **Update the tag** and pull:

   ```sh
   sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=v0.2.0/' .env
   docker compose pull
   ```

5. **Apply migrations BEFORE swapping the backend.** The migrate image
   is published in lockstep with the backend at the same tag:

   ```sh
   docker compose --profile migrate run --rm relayterm-migrate
   ```

   Idempotent on a no-op upgrade; otherwise applies the new schema
   changes.

6. **Restart the app containers only.** Postgres keeps running with
   the existing volume; only `relayterm-backend` and `relayterm-web`
   swap to the new image:

   ```sh
   docker compose up -d --no-deps relayterm-backend relayterm-web
   ```

7. **Run the post-deploy smoke** (§10).

8. **Record the previous tag** somewhere durable (deploy log, ops
   wiki, ticket). You need it to roll back (§6) without spelunking
   through `docker images` afterwards.

   ```sh
   echo "$(date -u +%Y%m%dT%H%M%SZ)  upgrade  ${PREVIOUS_TAG}  →  ${NEW_TAG}" \
       >> /srv/relayterm/deploy.log
   ```

---

## 6. Rollback checklist

Rollback is "set `RELAYTERM_IMAGE_TAG` to a previous immutable tag and
re-deploy." Two cases.

### 6.1 Schema is backward-compatible — straight rollback is safe

If the upgrade between the two tags did NOT change the database schema
(or only added schema that the older code is happy to ignore — new
nullable columns, new tables it doesn't reference), rolling back the
image is sufficient:

```sh
sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=sha-abc1234/' .env
docker compose pull
docker compose up -d --no-deps relayterm-backend relayterm-web
```

Then run the post-deploy smoke (§10).

### 6.2 Schema is backward-incompatible — restore from backup

RelayTerm has **no formal automated migration-rollback system**.
`sqlx migrate revert` exists in the migrate image and reverses the
*last* migration if the migration file ships a `down` step, but a
multi-step upgrade with mixed `down` coverage is not safe to revert
casually on production data.

If the new image carried a backward-incompatible migration (column
removed, type narrowed, NOT-NULL added on a column the old code
doesn't populate, etc.), rolling back the image alone leaves the
database ahead of the code. The correct path is:

1. Stop the app containers (`docker compose stop relayterm-backend
   relayterm-web`).
2. Restore Postgres from the backup taken **before** the upgrade
   (§8).
3. Pin `RELAYTERM_IMAGE_TAG` to the previous known-good tag.
4. `docker compose up -d --no-deps relayterm-backend relayterm-web`.
5. Run the post-deploy smoke (§10).

Pre-rehearse this in staging if you can. A "we'll figure it out
during the incident" plan is not a plan.

### 6.3 When in doubt

If you cannot quickly determine whether the rollback is in case
§6.1 or §6.2, **assume §6.2** and restore from backup. A 20-minute
restore beats a corrupted production database.

---

## 7. Migration procedure

The migration story is small and explicit; treat every bullet here as
load-bearing.

- **The backend does not auto-migrate.** Boot does not run `sqlx
  migrate run`. A missing schema surfaces as `500 internal_error` on
  the first request that touches the schema gap.
- **The operator runs the `relayterm-backend-migrate` image.** It is a
  one-shot container, profile-gated under `--profile migrate`, and is
  not started by `docker compose up`.
- **First run applies the migrations.** Subsequent runs at the same
  tag are idempotent — `sqlx` skips already-applied migrations.
- **Always migrate BEFORE starting the upgraded backend.** The
  upgraded image expects the upgraded schema; running the new backend
  against the old schema will surface the gap as 500s on whichever
  routes touch the new columns.
- **Never skip migrations on an upgrade.** Even if the release notes
  say "no schema changes," running the migrate image is the cheapest
  way to confirm that. It is idempotent on no-ops.
- **`sqlx migrate revert` exists but is not a routine deploy step.**
  Pre-rehearse any revert plan in staging, against a copy of
  production data, before touching the production DB.

The canonical command:

```sh
docker compose --profile migrate run --rm relayterm-migrate
```

For status only (does not modify the database):

```sh
docker compose --profile migrate run --rm relayterm-migrate \
    migrate info --source /app/migrations
```

---

## 8. Backup and restore reminders

This section is reminders. **Backup automation is intentionally not
shipped** in this slice — every host has its own off-site / encryption
/ retention story. Wire what you have.

### 8.1 What's in the blast radius

The `relayterm-pgdata` named volume (or your external managed Postgres)
contains:

- Users + Argon2id password hashes.
- Hosts + SSH identities (private keys are AEAD-wrapped under the
  vault master key — but a backup that includes Postgres AND a leaked
  vault key is full disclosure).
- Terminal recording chunks, when recording is enabled. Chunks are
  plaintext-at-rest by default; per-chunk encryption is a future
  slice.
- The audit log.

Everything RelayTerm calls "state" lives there.

### 8.2 Take a backup before every upgrade

```sh
docker compose exec -T postgres \
  pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc \
  > "/srv/relayterm/backups/relayterm-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

`-Fc` produces the custom binary format `pg_restore` consumes
directly; it's smaller than plain SQL and supports parallel restore.

### 8.3 Test restore periodically

A backup you have never restored is a hope, not a backup. On a
schedule that matches your environment, restore the most recent dump
into a throwaway Postgres instance and walk a smoke check against it.
The frequency is yours; once a quarter is a reasonable floor for a
self-hosted single-operator deployment.

### 8.4 Don't keep backups in a single failure domain

Backups stored on the same host or the same volume as the running
database are not backups. Move them — to off-site object storage, to
another host, to encrypted offline media. The exact mechanism is yours;
the rule is that the backup survives the loss of the running database.

### 8.5 What's NOT in this slice

- No automated backup schedule (`cron`, systemd timers, etc.).
- No off-site replication.
- No encrypted at-rest backup tooling.
- No retention / pruning of old backups.

These are operator-side concerns. A future slice may ship a sample
backup script; today, you wire it yourself.

---

## 9. Reverse proxy / Traefik checklist

The example stack publishes `relayterm-web` on `127.0.0.1:8081` so a
bare `docker compose up` does not expose plain HTTP to the public
internet. Production deployments terminate TLS at an outer reverse
proxy (Traefik, Caddy, outer nginx).

The `docs/deployment/docker-compose.md` §3 sections cover the per-proxy
configuration (Traefik labels block in the Compose example, Caddy /
outer nginx config snippet). This runbook is the *checklist* you walk
before sending traffic.

### 9.1 Public origin and TLS

- The deployment serves a single HTTPS public origin. The SPA at `/`
  and the API at `/api` MUST be on the same origin (see
  `docker-compose.md` §3.3 — splitting them is not currently
  supported).
- Your `RELAYTERM_AUTH__ALLOWED_ORIGINS` value MUST equal that public
  origin byte-for-byte: `scheme://host[:port]`, lowercase, no trailing
  slash, no path.
- `RELAYTERM_AUTH__COOKIE_SECURE=true`. The session cookie is
  `HttpOnly; SameSite=Strict; Secure` — required behind HTTPS.

### 9.2 WebSocket upgrade

The terminal-attach endpoint `/api/v1/terminal-sessions/:id/ws` is a
WebSocket upgrade. The outer proxy MUST honour `Upgrade` /
`Connection` headers for `/api/`. Without this, the SPA loads but
terminals never attach.

The inner `relayterm-web` nginx already handles the upgrade for the
backend hop; the outer proxy must do the same for the
`relayterm-web → outer-proxy` hop. `docker-compose.md` §3.2 has the
copy-pastable nginx snippet (`map $http_upgrade $connection_upgrade`
and the `proxy_set_header Upgrade / Connection` pair).

### 9.3 Header preservation

- **`Origin` MUST pass through unmodified.** The backend's CSRF guard
  does byte-equality against `RELAYTERM_AUTH__ALLOWED_ORIGINS`. nginx
  does NOT forward `Origin` automatically — you need an explicit
  `proxy_set_header Origin $http_origin;`. Caddy preserves request
  headers by default.
- `Host` should pass through (`proxy_set_header Host $host;`).
- `X-Forwarded-Proto $scheme;` and `X-Forwarded-For
  $proxy_add_x_forwarded_for;` are good hygiene if any downstream
  logging cares about the public scheme/IP. The backend itself does
  not enforce a particular `X-Forwarded-*` posture in v1.

### 9.4 Long-lived connections

PTY sessions are long-lived. Configure generous timeouts on the `/api/`
path:

- `proxy_read_timeout` and `proxy_send_timeout` at 1 hour (`3600s`) or
  more.
- `proxy_buffering off;` for the WS path.

A 60-second proxy timeout will close idle terminals without notice.

### 9.5 No plain HTTP exposure

- Confirm the `127.0.0.1:8081` host port is bound to loopback and is
  NOT reachable from the public internet (firewall it if you have
  any doubt — `ss -lntp` from the host should show
  `127.0.0.1:8081`, never `0.0.0.0:8081`).
- The outer proxy is the ONLY public ingress. Plain HTTP on the outer
  proxy should redirect to HTTPS, not pass through.

---

## 10. Post-deploy smoke checklist

Walk this after every fresh deploy (§4), every upgrade (§5), and
every rollback (§6).

> **What each health probe actually means.** Read this once;
> every bullet below assumes it.
>
> - `/_web_health` is a static `200 ok\n` served by the inner
>   `relayterm-web` nginx with `access_log off`. It confirms the
>   web container is up and serving. It does NOT reach the backend.
> - `/healthz` is a static `200 {"status":"ok"}` served by the
>   backend process and reverse-proxied by the inner nginx at the
>   same path. It is a **process-alive probe only** — it does NOT
>   confirm DB connectivity, migration state, vault unwrap, or
>   russh capability. A `(healthy)` `relayterm-backend` in
>   `docker compose ps` means the process accepts a request on
>   `:8080`; the `(healthy)` `postgres` row (driven by
>   `pg_isready`) is the corresponding DB-side liveness signal.
> - `/api/v1/auth/me` without a cookie is a **routing + auth-gate
>   sanity check**, NOT a health endpoint. `401` is the expected
>   answer; any `2xx` here means the auth gate has been bypassed
>   and is a security regression.

- [ ] **Compose state.** `docker compose ps` shows every service as
      `running`, and `postgres` / `relayterm-backend` /
      `relayterm-web` as `healthy`. Note `(healthy)` on
      `relayterm-backend` is process-alive only — see the box above.
- [ ] **Backend logs.** `docker compose logs --tail=200
      relayterm-backend` — startup is clean; no `ERROR` lines; the
      retention worker starts (when enabled) and reports empty sweeps
      idly.
- [ ] **Web logs.** `docker compose logs --tail=200 relayterm-web` —
      nginx came up; no `emerg` / `alert` lines.
- [ ] **Web health.** `curl -sf http://127.0.0.1:8081/_web_health`
      returns `ok` (single `Content-Type: text/plain` header — see
      §6.4.6 of `docker-compose.md` for the historical fix).
- [ ] **Backend health through the proxy.** `curl -sf
      http://127.0.0.1:8081/healthz` returns `{"status":"ok"}`.
      (Also reachable publicly at `https://<origin>/healthz` because
      the inner nginx proxies the same path — same static body,
      same process-alive semantics.)
- [ ] **Auth gate from the loopback.** `curl -i
      http://127.0.0.1:8081/api/v1/auth/me` returns `401` without a
      cookie. **`401` is the expected result** — this confirms
      `/api/*` is routed to the backend AND the auth gate is in
      front of protected routes. A `2xx` here is the failure case.
- [ ] **Public-origin reachability.** From a workstation: the public
      URL serves the SPA over HTTPS; `GET https://<origin>/_web_health`
      returns `ok`.
- [ ] **Login + protected route.** Sign in through the browser. After
      login, `GET /api/v1/auth/me` returns the user record. A protected
      route (e.g. `/api/v1/hosts`) returns `200` (possibly an empty
      list) instead of `401`.
- [ ] **Bad-Origin write is 403 (optional but easy).** With the session
      cookie in hand. The cookie value is a live session token —
      **do not paste it into a shell history**. Either save the
      `Set-Cookie` from your login into a `curl --cookie-jar` file
      (`-b cookies.txt -c cookies.txt`, then re-use the jar here) or
      run the snippet through `set +o history` (bash/zsh) /
      `function fish_command_not_found; end; set fish_history ""`
      (fish). The literal `<cookie>` placeholder below is a hint to
      replace, not a value to commit:
      ```sh
      curl -i -X POST \
        -H 'Content-Type: application/json' \
        -H 'Origin: https://attacker.example.com' \
        -b cookies.txt \
        -d '{}' \
        https://<origin>/api/v1/hosts
      # → 403 csrf_origin_mismatch
      ```
      The wire body MUST NOT echo the offered Origin.
- [ ] **Terminal WebSocket.** Either: (a) attach a terminal in the
      browser and confirm input echoes; or (b) confirm an
      unauthenticated `wss://<origin>/api/v1/terminal-sessions/<id>/ws`
      connect is rejected with `401` before the upgrade completes.
- [ ] **No secret leakage.** Grep the backend logs for the redaction
      sentinels — session token plaintext, vault internals, terminal
      I/O, recording bytes, base64 chunk payloads, the bootstrap
      token. A clean grep is required:
      ```sh
      docker compose logs --tail=2000 relayterm-backend | \
        grep -E 'relayterm_session=[A-Za-z0-9_-]{20,}|encrypted_private_key|data_b64' || \
        echo "ok: no leakage sentinels found"
      ```
      Treat any hit as a security regression.

The browser-side smoke runbook in
[`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md) covers a
deeper UI walkthrough — run that on releases, not on every routine
upgrade.

---

## 11. Secret rotation notes

There is no automated rotation tooling in this slice. Each rotation
below is operator-driven.

### 11.1 Session signing key

- Stored in `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64` (or
  `_FILE`, mutually exclusive). The boot-time validator requires
  exactly one source.
- **In v1 the key is reserved, not consumed.** The session model is
  hashed-opaque-token (SHA-256 of the random cookie value); the
  signing key is required at boot but not yet read by any code path.
  See [`docs/production-auth.md`](../production-auth.md) §2 for the
  binding statement. Rotating the key today is therefore a **no-op
  for live sessions** — sessions remain valid, users are not signed
  out, and there is nothing to verify.
- **Why rotate at all in v1?** Operational hygiene only — the
  redaction discipline (`*_set` Debug shape, file-vs-env sourcing,
  separation from the vault key) is being exercised now so that the
  signed-CSRF / signed-cookie scheme that lands later inherits the
  habit, not because rotation has live impact today.
- **Once the signed-cookie scheme ships**, rotation will invalidate
  every active session on restart. Plan for that future cutover; do
  not assume it is the case today.
- Procedure (v1, no-op): generate a new 32-byte base64 value
  (`openssl rand -base64 32`), update `.env`, restart the backend
  (`docker compose up -d --no-deps relayterm-backend`).

### 11.2 Vault master key

- Stored in `RELAYTERM_VAULT__MASTER_KEY_B64`.
- The vault master key wraps every stored SSH private key. Rotating
  it is **NOT a casual operation**: the existing wrapped blobs were
  AEAD-sealed under the old key and cannot be opened by the new key.
  RelayTerm has no automated re-encryption pass for stored identities
  in v1.
- **Do not rotate the vault master key without a re-encryption plan.**
  The honest workflow today is: export every owner's identities (via
  the API, owner-by-owner), revoke them in the database, swap the
  key, restart, and have each owner re-import. For a single-operator
  deployment this is feasible; at scale it is not.
- A future slice may ship an explicit re-encryption migration. Until
  then, treat the vault master key as set-and-protect, not
  rotate-on-schedule.

### 11.3 First-user bootstrap token

- After the first user is bootstrapped (§4.10, §4.11), the token
  should be **unset from `.env`** and the backend restarted (§4.11).
- Subsequent `POST /api/v1/auth/bootstrap` calls return `409
  already_bootstrapped` even if the token is still configured, so a
  leftover token is operationally harmless — but unset it for hygiene.
- If you need to bootstrap a *replacement* first user (the only user
  forgot their password and there is no other account), the recovery
  path runs through Postgres directly — see
  [`docs/production-auth.md`](../production-auth.md) §8, "Lost the
  password and there's no other user," for the SQL.

### 11.4 Forgejo registry tokens

- The CI publish token (`FORGEJO_REGISTRY_TOKEN`, `write:package`
  scope) and the deploy-host pull token (`read:package` scope) are
  **independent**. Rotate either one without affecting the other.
- Rotate the CI token: regenerate in Forgejo, update the repo secret
  (`Settings → Secrets → FORGEJO_REGISTRY_TOKEN`), re-run the publish
  workflow to confirm. Old token can be revoked.
- Rotate the deploy-host token: regenerate in Forgejo,
  `docker logout git.js-node.cc` and `docker login` with the new
  value, run a `docker compose pull` to confirm.

### 11.5 Database password

- Stored in `POSTGRES_PASSWORD`; consumed by both the Postgres
  service (initial superuser password on first volume init) and the
  backend's `RELAYTERM_DATABASE__URL`.
- Rotation requires updating the password inside Postgres
  (`ALTER USER ... WITH PASSWORD ...`), updating `.env`, and
  restarting the backend AND the migrate container's next run. The
  Postgres container itself does not need to restart.
- For a managed external Postgres, the rotation flow is whatever your
  provider documents; the backend side is just `.env` update + backend
  restart.

### 11.6 What's NOT rotated by this list

- Per-user passwords. Self-service password change is live — see
  [`docs/production-auth.md`](../production-auth.md). There is no
  email-based reset flow yet (deferred).
- Stored SSH identity keys. Rotating an SSH key is "delete the
  identity, re-add the new one" through the API; the vault key above
  is the wrapping key, not the identity keys themselves.

---

## 12. Operational notes

A small grab-bag of properties operators should know without having to
re-derive them from the spec.

- **Recording retention worker.** When
  `RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED=true`
  (the default), a periodic worker runs inside the backend container
  on the configured `SWEEP_INTERVAL_SECONDS` (default 1 hour) and
  purges recording chunks/markers for sessions whose `closed_at +
  retention_days` has elapsed. The first periodic tick fires *after*
  one full interval (the boot Stage A sweep already drained the
  eligibility set). See [`docs/terminal-recording.md`](../terminal-recording.md)
  Section 12 for the binding contract.
- **Cleanup is independent of `recording.enabled`.** Turning recording
  off later does NOT make existing chunks immortal — the cleanup
  knobs are honoured even when recording itself is disabled. If you
  want to retire a deploy's recording corpus, set retention low,
  leave cleanup enabled, and let the worker drain the table on its
  schedule.
- **No frontend UI for purging recordings.** v1 has no
  user-triggered purge — the worker is the only path. An admin /
  operator purge UI is deferred.
- **No admin / RBAC model.** v1 is single-user / self-hosted; the
  first user owns everything. There is no admin role, no per-user
  permissions, no operator-only routes.
- **Production renderer.** The production terminal workspace uses the
  `@relayterm/terminal-xterm` baseline only. Experimental renderers
  (`ghostty-web`, `restty`, `wterm`) are dev-lab-only and are
  tree-shaken out of the production bundle.
- **Tauri shells.** The desktop and mobile shells live in
  `apps/desktop/` and `apps/mobile/` and are a separate deployment
  track. They are NOT covered by this runbook and have no CI release
  workflow yet — see §13.
- **Origin-locked CSRF guard.** Every state-changing browser write
  goes through the shared `CsrfGuard` extractor before any body
  parsing. A request with a missing or non-allow-listed `Origin`
  returns `403 csrf_origin_mismatch`; the wire body never echoes the
  offered Origin.
- **Auth gate.** Every protected `/api/v1/*` route requires a valid
  session cookie. The legacy dev-auth shim is gone — production and
  development both run through the same `AuthenticatedUser` extractor;
  dev mode only relaxes the boot-time validator, never the per-handler
  check.

---

## 13. Deferred automation

Tracked here so operators know what is **not** in the box. Each line
is a separate slice.

- **Auto-deploy from CI to a host.** The publish job ends at "image
  is in the registry"; no SSH, no `docker compose pull`, no restart.
  Operators run those steps by hand (this runbook's §4 / §5 / §6).
- **Watchtower.** Not configured. A live container watching the
  registry for tag changes is a deliberate omission — combined with
  `:main`-style mutable tags it would be a footgun, and on
  `:vX.Y.Z` / `:sha-<short>` it adds little over a deliberate
  operator pull.
- **GitOps (Argo CD, Flux, etc.).** Not configured. RelayTerm is
  deliberately small enough that a single `docker-compose.yml` plus
  this runbook is the deployment surface.
- **SSH-based deploy automation.** No Ansible / Fabric / paramiko /
  custom shell scripts ship in this repo. Operators wire what they
  have.
- **Image signing.** No cosign / notary v2. Published images are
  unsigned in v1.
- **SBOM generation.** No SBOM is attached to published images.
- **Vulnerability scanning.** No `trivy` / `grype` / Forgejo-side
  scanner runs against published images.
- **Registry retention / cleanup automation.** Old `:sha-*` tags
  accumulate forever. Manual prune via the Forgejo UI or API is the
  only path today.
- **Multi-arch images.** Single-arch (`linux/amd64`) only. ARM64 and
  the QEMU-backed buildx flow are deferred.
- **Production secrets automation.** No Vault / sops / cloud secret
  manager wiring ships in this repo. Operators use `.env`, docker
  secrets, systemd `EnvironmentFile=`, or whatever their host's
  secret store is.
- **Backup automation.** No scripts, no scheduled timers, no off-site
  replication. Reminders only — see §8.
- **Tauri v2 desktop / mobile CI.** The desktop and Android shells
  have no release pipeline yet. Their builds today are local-dev
  workflows.

---

## See also

- [`docs/deployment/first-production-deploy-plan.md`](./first-production-deploy-plan.md)
  — short, opinionated planning page for the first personal
  production deploy; composes this runbook + the backup-restore
  runbook + the v1 release checklist into a single page.
- [`docs/deployment/docker-compose.md`](./docker-compose.md) — the
  Compose-stack reference: services, env contract, CI workflow,
  registry publish.
- [`docs/production-auth.md`](../production-auth.md) — production auth
  configuration, bootstrap flow, recovery paths.
- [`docs/auth-smoke.md`](../auth-smoke.md) — operator-side end-to-end
  auth smoke procedure.
- [`docs/terminal-recording.md`](../terminal-recording.md) — recording
  / replay architecture, retention contract.
- [`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md) — manual
  browser-side smoke runbook.
