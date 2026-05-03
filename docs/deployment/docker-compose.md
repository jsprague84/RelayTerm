# RelayTerm — Docker Compose deployment

This guide covers the minimal production-like Compose stack shipped under
`deploy/`. It is the recommended path for **self-hosted, single-host**
deployments. It is intentionally NOT a full release platform: there is
no Kubernetes/Helm, no Nomad, no multi-node HA, no zero-downtime, no
image signing, no SBOM generation, no automated backups, and no
managed-secrets integration. Operators who want any of those layer
them on top.

The canonical specs live elsewhere — when something here drifts from
the code, the code wins:

- Auth: [`docs/production-auth.md`](../production-auth.md), `SPEC.md` →
  "Production authentication architecture".
- Recording / cleanup: [`docs/terminal-recording.md`](../terminal-recording.md).
- Config schema: `apps/backend/src/config.rs`,
  `docs/config-examples/relayterm.production.example.toml`.

---

## 1. What's in the stack

| Service | Image | Purpose | Exposed |
|---|---|---|---|
| `postgres` | `postgres:17-alpine` | Authoritative state for users, sessions, hosts, ssh-identities, audit, recording chunks/markers. | Internal only. |
| `relayterm-migrate` | built from `Dockerfile.backend --target migrate` | One-shot `sqlx migrate run` against the database. Profile-gated (`migrate`); does NOT run as part of `docker compose up`. | Internal only. |
| `relayterm-backend` | built from `Dockerfile.backend` (default `runtime` target) | The Rust/Axum backend — owns SSH sessions, auth, recording. | Internal only. |
| `relayterm-web` | built from `Dockerfile.web` | nginx serving the production SPA bundle and reverse-proxying `/api` (with WebSocket upgrade) and `/healthz` to the backend. | `127.0.0.1:8081` by default. |

Files:

- [`deploy/docker-compose.example.yml`](../../deploy/docker-compose.example.yml)
- [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
- [`deploy/nginx/web.conf.template`](../../deploy/nginx/web.conf.template)
- [`Dockerfile.backend`](../../Dockerfile.backend) — multi-target
  (`runtime`, `migrate`).
- [`Dockerfile.web`](../../Dockerfile.web) — Node builder + nginx
  runtime.

---

## 2. First deployment — step by step

The example deploy lives in `deploy/`. Copy the two example files into
the same directory you'll run Compose from, then edit them:

```sh
cp deploy/docker-compose.example.yml docker-compose.yml
cp deploy/relayterm.env.example .env
$EDITOR .env
```

### 2.1 Generate secrets

Every `CHANGE_ME_*` value in `.env` MUST be replaced. Generate the
random ones on a trusted machine:

```sh
# Session signing key.
openssl rand -base64 32

# Vault master key (different value!).
openssl rand -base64 32

# First-user bootstrap token (URL-safe).
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

Three independent secrets. The session signing key and the vault
master key MUST be different — the validator does not currently
enforce inequality on these two, but treating one secret as both is a
blast-radius mistake (a vault disclosure becomes an auth disclosure
and vice versa).

### 2.2 Set the public origin

`RELAYTERM_AUTH__ALLOWED_ORIGINS` must match the URL the browser sees,
**byte for byte**:

- Scheme + host + optional port. Lower case (browsers serialise the
  scheme/host of the `Origin` header in lower case).
- No trailing slash.
- No path.
- Comma-separated for multiple origins.

For a deployment at `https://relayterm.example.com`:

```env
RELAYTERM_AUTH__ALLOWED_ORIGINS=https://relayterm.example.com
```

### 2.3 Apply database migrations

The backend does NOT auto-migrate. Run the one-shot migration container:

```sh
docker compose --profile migrate run --rm relayterm-migrate
```

This invokes `sqlx migrate run --source /app/migrations` against
`$DATABASE_URL` (assembled from `POSTGRES_*` in `.env`). Re-running it
is idempotent — `sqlx` skips already-applied migrations.

For a dry run / status check:

```sh
docker compose --profile migrate run --rm relayterm-migrate \
    migrate info --source /app/migrations
```

### 2.4 Start the stack

```sh
docker compose up -d postgres relayterm-backend relayterm-web
```

Compose waits for `postgres` to be healthy, then starts the backend,
then starts the web container.

Smoke check from the host:

```sh
# Web container is up and serving.
curl -sf http://127.0.0.1:8081/_web_health
# → ok

# Web → backend proxy + backend health.
curl -sf http://127.0.0.1:8081/healthz
# → {"status":"ok"}

# Auth gate is enforced — every protected route is 401 without the cookie.
curl -i -sf http://127.0.0.1:8081/api/v1/auth/me || true
# → HTTP/1.1 401 Unauthorized
```

A 401 from `/api/v1/auth/me` without a cookie is the correct, expected
response.

### 2.5 Bootstrap the first user

With `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` set in `.env` (and
the stack restarted to pick it up), POST to `/api/v1/auth/bootstrap`.
The full flow lives in [`docs/production-auth.md`](../production-auth.md)
§5; the short version:

```sh
curl -i -sf -X POST \
  -H 'Content-Type: application/json' \
  -H 'Origin: https://relayterm.example.com' \
  -d '{"bootstrap_token":"<paste token>","email":"you@example.com","password":"<your password>"}' \
  https://relayterm.example.com/api/v1/auth/bootstrap
```

Once the response is 200 with a `Set-Cookie: relayterm_session=...`,
unset the bootstrap token in `.env` and restart the backend:

```sh
docker compose up -d --no-deps relayterm-backend
```

A second `bootstrap` call now returns 409.

---

## 3. Production behind a reverse proxy

The example stack binds `relayterm-web` to `127.0.0.1:8081` so a bare
`docker compose up` does NOT publish plain HTTP to the public internet.
Production deployments terminate TLS at an outer reverse proxy
(Traefik, Caddy, or another nginx) that forwards traffic to the web
container.

### 3.1 Traefik

The example Compose file has a commented `labels:` block on
`relayterm-web`. Uncomment, replace `relayterm.example.com` with your
host, and replace `letsencrypt` with whatever cert resolver name your
Traefik config uses. Then attach Traefik's container to the
`relayterm-internal` network — either by joining it with
`docker network connect relayterm_relayterm-internal traefik`, or by
promoting the network to `external: true` in Compose and creating it
ahead of time.

Traefik forwards the standard `X-Forwarded-*` headers that nginx
inside `relayterm-web` already passes through to the backend, so the
backend's CSRF guard sees the public `Origin` byte-for-byte.

### 3.2 Caddy / outer nginx

Configure the outer proxy to forward to `http://127.0.0.1:8081` (or
remove the host port mapping in Compose and have the outer proxy talk
to the container over a shared docker network — that is preferred,
because it keeps plain HTTP off the host's loopback).

Required upstream behaviour:

- **Pass the original `Origin` header through.** nginx does not
  forward `Origin` automatically — without an explicit
  `proxy_set_header Origin $http_origin;`, the inner backend's CSRF /
  `auth.allowed_origins` check sees nothing and rejects every browser
  state-change request with `403 csrf_origin_mismatch`. Caddy
  preserves request headers by default; raw nginx does not. Minimum
  outer-nginx snippet:

  ```nginx
  location / {
      proxy_pass http://127.0.0.1:8081;
      proxy_http_version 1.1;
      proxy_set_header Host             $host;
      proxy_set_header Origin           $http_origin;
      proxy_set_header Upgrade          $http_upgrade;
      proxy_set_header Connection       $connection_upgrade;
      proxy_set_header X-Forwarded-For  $proxy_add_x_forwarded_for;
      proxy_set_header X-Forwarded-Proto $scheme;
      proxy_read_timeout 3600s;
      proxy_send_timeout 3600s;
      proxy_buffering off;
  }
  ```

  The `$connection_upgrade` variable is the same WebSocket-aware map
  used in `deploy/nginx/web.conf.template`; copy that `map` block
  verbatim into your outer nginx config.
- Honour WebSocket upgrade headers — the terminal attach endpoint
  `/api/v1/terminal-sessions/:id/ws` is a WS upgrade. nginx alpine's
  `proxy_pass` map in `deploy/nginx/web.conf.template` already does
  this for the inner hop; the outer hop must too.
- Generous read/write timeouts on the `/api/` location. PTY sessions
  are long-lived; a 60-second proxy timeout will close idle terminals.

### 3.3 Same-origin contract

RelayTerm assumes the SPA and the API are served from the **same
origin**:

```
https://relayterm.example.com/        → SPA (index.html + /assets/...)
https://relayterm.example.com/api/    → backend (REST + WS)
https://relayterm.example.com/healthz → backend
```

This is what makes the cookie posture (`HttpOnly; SameSite=Strict;
Secure; relayterm_session=...`) and the CSRF allow-list trivially
correct: the browser only ever sends the cookie with the API request,
and the API only ever sees the public origin in the `Origin` header.
Splitting the SPA and the API across origins breaks the cookie and
forces a CORS posture RelayTerm does not currently support.

---

## 4. Operations

### 4.1 Migrations on upgrade

After pulling a new image / rebuilding:

```sh
docker compose build relayterm-backend relayterm-web relayterm-migrate
docker compose --profile migrate run --rm relayterm-migrate
docker compose up -d --no-deps relayterm-backend relayterm-web
```

Migrations are forward-only and idempotent. `sqlx migrate revert` is
available via the migrate image but is NOT a routine deploy step —
revert plans should be pre-rehearsed in a staging copy of the
database.

### 4.2 Persistence and backups

The Postgres data lives in the named volume `relayterm-pgdata`. This
volume is the entire blast radius:

- Users + password hashes (Argon2id PHC strings)
- Hosts + ssh-identities (encrypted private keys; vault master key
  unwraps them)
- Terminal recording chunks (plaintext-at-rest by default; encryption
  is a future slice)
- Audit log

Back up the volume on whatever cadence your environment requires.
`pg_dump` against the running container is the simplest path:

```sh
docker compose exec -T postgres \
  pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc \
  > "relayterm-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

Backup automation is intentionally NOT shipped here — every host has
its own off-site / encryption / retention story. Wire what you have.

### 4.3 Logs

The backend speaks structured tracing to stdout. `docker compose logs
-f relayterm-backend` is the entry point. The default `RUST_LOG` keeps
the application at info, sqlx at warn, dependencies at info; tune it
in `.env` and restart the container.

The redaction discipline documented in `AGENTS.md` means terminal I/O,
recording bytes, password hashes, session tokens, vault internals, and
peer banners must NEVER reach a log line. If you find one, it is a bug
— file it, do not normalise it.

### 4.4 External managed Postgres

The stack ships its own Postgres for self-host convenience. Production
environments often prefer a managed Postgres (RDS, Cloud SQL,
Supabase, etc). Two changes:

1. Remove the `postgres:` service and the `relayterm-pgdata:` volume
   from your Compose file.
2. Point `RELAYTERM_DATABASE__URL` (and the migrate service's
   `DATABASE_URL`) at the managed instance's connection string.
   Confirm SSL mode matches what the provider requires —
   `?sslmode=require` is common.

The migrate container still applies the same migrations against the
managed database. Run it once on deploy.

### 4.5 Secrets management

`.env` is the simplest mechanism and is fine for a single host where
the operator already controls disk encryption. For anything beyond
that, swap to:

- `docker compose` secrets: `secrets:` blocks + file-mount the value
  and point `RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE` /
  `RELAYTERM_VAULT__MASTER_KEY_FILE` /
  `RELAYTERM_TERMINAL_RECORDING__ENCRYPTION__MASTER_KEY_FILE` at the
  mounted path.
- A systemd `EnvironmentFile=` outside the repo.
- An external secrets manager (Vault, sops, age-encrypted file, your
  cloud's secret store) wired into your deploy pipeline.

The backend's config schema accepts both `*_B64` and `*_FILE` for the
session signing key and the vault master key — pick whichever fits
your secret-store contract.

---

## 5. Building images locally

```sh
# Backend (default `runtime` target).
docker build -f Dockerfile.backend -t relayterm-backend:local .

# Backend migrate image.
docker build -f Dockerfile.backend --target migrate -t relayterm-migrate:local .

# Web.
docker build -f Dockerfile.web -t relayterm-web:local .
```

`docker compose build` from the `deploy/` directory uses these
Dockerfiles automatically (paths are relative to the repo root via
`context: ..`).

Image registry push, tag policy, and image signing are intentionally
out of scope for this slice — when CI lands, that's the layer that
publishes images.

---

## 6. Smoke / verification checklist

After a fresh deploy or upgrade:

- [ ] `docker compose ps` — every service is `running` and (where
      configured) `healthy`.
- [ ] `curl -sf http://127.0.0.1:8081/_web_health` — `ok`.
- [ ] `curl -sf http://127.0.0.1:8081/healthz` — `{"status":"ok"}`.
- [ ] `curl -i http://127.0.0.1:8081/api/v1/auth/me` — `401`.
- [ ] Browser hits the public URL, the SPA loads, the login flow
      works, a terminal session attaches and echoes input. The
      browser-side smoke runbook lives in
      [`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md).

---

## 7. Deferred / out of scope

Tracking ledger for things this slice intentionally does NOT cover.
File these as separate slices when they're needed:

- Kubernetes / Helm / Nomad manifests.
- Multi-node HA (active-active backend with shared session state).
- Image signing (cosign / notary v2).
- SBOM generation.
- Automated production secrets management (Vault auto-unwrap, etc).
- Backup automation (snapshots, off-site replication).
- Zero-downtime deploys (rolling restart, blue/green).
- Forgejo Actions / CI workflows. The repo currently has no CI
  workflows, and adding one was deferred until conventions are set —
  see the next slice.
- Image registry push and tag policy.
- Production renderer selector (production stays on the
  `@relayterm/terminal-xterm` baseline; experimental renderers are
  dev-lab-only).
- Mobile / Tauri packaging — covered by `apps/desktop/` and
  `apps/mobile/`, not this stack.
