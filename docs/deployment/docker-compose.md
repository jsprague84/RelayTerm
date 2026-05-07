# RelayTerm — Docker Compose deployment

This guide covers the minimal production-like Compose stack shipped under
`deploy/`. It is the recommended path for **self-hosted, single-host**
deployments. It is intentionally NOT a full release platform: there is
no Kubernetes/Helm, no Nomad, no multi-node HA, no zero-downtime, no
image signing, no SBOM generation, no automated backups, and no
managed-secrets integration. Operators who want any of those layer
them on top.

> **Looking for the operator runbook?** The step-by-step "what do I
> actually run, in order" checklists for first deploy, upgrade,
> rollback, migration, backup, reverse-proxy, secret rotation, and
> post-deploy smoke live in
> [`docs/deployment/production-runbook.md`](./production-runbook.md).
> This file is the **setup and reference** — services, env contract,
> CI workflow, registry publish. The runbook is what you walk on
> deploy day.

The canonical specs live elsewhere — when something here drifts from
the code, the code wins:

- Auth: [`docs/production-auth.md`](../production-auth.md), `SPEC.md` →
  "Production authentication architecture".
- Recording / cleanup: [`docs/terminal-recording.md`](../terminal-recording.md).
- Config schema: `apps/backend/src/config.rs`,
  `docs/config-examples/relayterm.production.example.toml`.
- Operator runbook: [`docs/deployment/production-runbook.md`](./production-runbook.md).

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
  — build-mode (operator builds images locally from `Dockerfile.*`).
- [`deploy/docker-compose.images.example.yml`](../../deploy/docker-compose.images.example.yml)
  — image-mode (operator pulls published images from
  `git.js-node.cc`; see §6.4).
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

The step-by-step upgrade / rollback / backup / smoke checklists for
operators live in
[`docs/deployment/production-runbook.md`](./production-runbook.md).
This section captures the *reference* shape of each operation — the
runbook is what you walk in order on deploy day.

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

### 5.1 Selected base images

The Dockerfiles pin every build/runtime base image with a versioned
`ARG`. The toolchain values (`RUST_VERSION`, `NODE_VERSION`) are also
pinned by `.forgejo/workflows/ci.yml`, but in *separate workflow steps*
rather than a job-level `container.image:` — the workflow's job
container is `catthehacker/ubuntu:act-latest` (a generic Forgejo-runner
base, NOT a toolchain pin and NOT included in the table below) and the
Rust/Node versions are installed inside the job. Bumping a toolchain is
one commit that touches the Dockerfile `ARG`, the matching workflow
step, and this section. See §6.3 for the bump procedure.

The table below lists ONLY Dockerfile base images. The CI runner base
(`catthehacker/ubuntu:act-latest`) is fixed and not version-bumped
alongside Rust/Node — replacing it would be its own slice (e.g.
switching to a Forgejo-official runner image when one stabilises).

| Image | Pin | Why |
|---|---|---|
| `rust:${RUST_VERSION}-bookworm` | `RUST_VERSION=1.95` | The Cargo.toml-declared MSRV is `rust-version = "1.85"` and `edition = "2024"` is the only post-1.85 language feature currently in use (1.85 stabilised it), so the workspace compiles cleanly at MSRV today. The 1.95 pin here is NOT driven by a specific feature requirement — it tracks the working local dev toolchain so the container compiler matches host diagnostics (including new lints and clippy nudges) and so a CI build does not surface a warning the dev never saw. The repo has no `rust-toolchain.toml` yet; the Dockerfile is the de facto source of truth for "what Rust does CI build with", and CI's `rustup --default-toolchain 1.95` mirrors that. A future slice should add a `rust-toolchain.toml` at the repo root and have this `ARG` plus the workflow step read from it, collapsing the three pin sites to one. |
| `node:${NODE_VERSION}-bookworm-slim` | `NODE_VERSION=22` | Node 22 is the current LTS line. `package.json#packageManager` (`pnpm@10.33.0`) is the source of truth for pnpm — corepack handles the activation in both the Dockerfile and CI, so there's nothing to pin twice. CI's `setup-node` step pins to the same `22` value. |
| `nginx:${NGINX_VERSION}` | `NGINX_VERSION=1.27-alpine` | Stable line. The `envsubst`-on-`/etc/nginx/templates/*.template` behaviour the runtime stage relies on is alpine-specific. |
| `debian:${DEBIAN_VERSION}` | `DEBIAN_VERSION=bookworm-slim` | Matches the `bookworm` base of the Rust builder so `glibc` versions line up between build and runtime. |

## 6. CI image build and registry publishing (Forgejo Actions)

`.forgejo/workflows/ci.yml` runs four jobs. The first three run on every
push to `main`, every `v*` tag push, and every pull request; the fourth
runs ONLY on push-to-main, `v*` tag pushes, and operator-driven
`workflow_dispatch`:

1. **`rust-checks`** — `cargo fmt --all -- --check`, `cargo clippy
   --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
   The workspace integration tests on `relayterm-db` and
   `relayterm-api` are gated behind a `postgres-tests` cargo feature
   and do NOT run in CI — they require a live Postgres reachable via
   `DATABASE_URL` and are exercised manually per
   [`crates/relayterm-db/tests/repositories.rs`](../../crates/relayterm-db/tests/repositories.rs).
2. **`web-checks`** — `pnpm install --frozen-lockfile`, `pnpm -r
   check`, `pnpm -r build`, `pnpm -r test`. pnpm is activated via
   `corepack` against the version pinned in
   `package.json#packageManager`.
3. **`docker-build`** — `docker build` for `Dockerfile.backend`
   (`runtime` and `migrate` targets) and `Dockerfile.web`. Build-only
   verification — runs on PRs and is the gate that must pass before
   `publish-images` runs.
4. **`publish-images`** — pushes the three OCI images to the Forgejo
   container registry at `git.js-node.cc`. Gated by `if:` to push-to-
   main / `v*` tags / `workflow_dispatch` only; PRs never publish. See
   §6.4.

### 6.1 Runner Docker access

The `docker-build` and `publish-images` jobs run `docker build` from
inside the job container. Forgejo runners do NOT expose the host Docker
daemon by default — you must configure ONE of the following in the
runner's `config.yml`:

- **Socket mount** (preferred for self-hosted runners on a trusted
  host):
  ```yaml
  container:
    docker_host: 'automount'
  ```
  Forgejo Runner mounts the host's `/var/run/docker.sock` into the job
  container as `/var/run/docker.sock`. The job container
  (`catthehacker/ubuntu:act-latest`, which ships docker CLI + buildx)
  then talks to the host daemon directly. Note: any job on this runner
  can reach the host daemon, which is approximately equivalent to root
  on the host.
- **Docker-in-Docker** (preferred for shared/multi-tenant runners):
  run a `docker:dind` sidecar and point `DOCKER_HOST` at it. The full
  recipe is in
  [Forgejo's docker-access docs](https://forgejo.org/docs/latest/admin/actions/docker-access).
  This is the pattern in use on `git.js-node.cc` — the runner's
  `container.options` config plumbs `DOCKER_HOST` + TLS certs into
  every job, mirroring `git.js-node.cc/jsprague/qshift`'s working
  setup.

Without one of these, the `docker info` step in the workflow fails
fast with a recognisable "Cannot connect to the Docker daemon" error
— that's the operator's signal to fix runner config, not the
workflow.

### 6.2 What the workflow does NOT do

Intentionally deferred — these will land in their own slices once
conventions are set:

- **Auto-deploy.** The workflow publishes images but never SSHes /
  pulls / restarts on the deploy host. Operators run
  `docker compose pull` + `up -d` themselves (see §6.4). Watchtower,
  GitOps, and SSH push are all separate slices.
- **Multi-arch / `linux/arm64`.** Single-arch builds only. A future
  slice can wire `docker buildx` against a QEMU-backed builder.
- **Image signing (cosign / notary), SBOM generation, vulnerability
  scanning, registry retention/cleanup policies.** Deferred.
- **Remote / registry build cache.** The Dockerfiles use BuildKit
  cache mounts, but no remote / registry cache is wired up. First-run
  CI will be slow (fully recompiles the Rust workspace); cached
  reruns on the same runner reuse the local BuildKit cache. The
  publish job runs the build a second time (it does not share state
  with `docker-build`); a future slice can collapse the two via a
  shared buildx builder.
- **Playwright / browser SSH smoke.** The browser smoke runbook in
  [`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md) is
  intentionally manual.
- **Production secrets in CI beyond registry login.** The
  `publish-images` job consumes exactly one repo secret —
  `FORGEJO_REGISTRY_TOKEN`. No deploy SSH keys, no cloud credentials,
  no app secrets (session signing key, vault master key, bootstrap
  token) live in the workflow. App secrets are operator-side env on
  the deploy host.

### 6.3 Updating the toolchain

The workflow's Rust and Node major versions live alongside the
matching `ARG` defaults in the Dockerfiles. Any bump is a single
commit touching all of:

- `Dockerfile.backend` `ARG RUST_VERSION=...`
- `Dockerfile.web` `ARG NODE_VERSION=...`
- `.forgejo/workflows/ci.yml` — Rust pin lives in the
  `rust-checks → install rust …` step (`--default-toolchain X.Y.Z`);
  Node pin lives in the `web-checks → setup node …` step
  (`with: node-version: 'NN'`). The job-level `container.image` is
  `catthehacker/ubuntu:act-latest` for every job and is NOT the
  toolchain pin — it's only the Forgejo-runner-compatible base for
  the JS action runtime.
- This section's "Selected base images" table.
- `Cargo.toml` `rust-version = "..."` IF (and only if) the bump also
  raises the MSRV — most local-toolchain bumps do NOT raise the
  MSRV, since 1.85 is the floor that `edition = "2024"` requires.

If the local `rustc --version` and the Dockerfile pin diverge, the
Dockerfile is the source of truth — bump local rustup to match, then
update all four (or five) files in the same commit. Cite the actual
trigger in the commit message: a new lint, a new feature requirement,
or just routine alignment with the local dev toolchain.

### 6.4 Registry publishing

After `rust-checks`, `web-checks`, and `docker-build` pass, the
`publish-images` job builds and pushes three OCI images to the Forgejo
container registry at `git.js-node.cc`. Pull requests never publish —
only build verification.

**When publish runs.** The `if:` guard in `publish-images` allows
exactly three event shapes:

| Event | Image tag derived from |
|---|---|
| `push` to `refs/heads/main` | `:main` + `:sha-<short>` |
| `push` to `refs/tags/v*` | `:vX.Y.Z` + `:sha-<short>` |
| `workflow_dispatch` (any branch) | `:<ref_name>` + `:sha-<short>` |

> **Operator-only `workflow_dispatch` from a feature branch.**
> Dispatching the workflow from a non-`main`, non-`v*` ref produces
> images tagged with the branch slug (e.g. `:chore/foo`). That tag is
> intended for ad-hoc operator-side debugging / staging only — it is
> NOT a release path. Pin production deployments to `:vX.Y.Z` (or
> `:sha-<short>` for a deliberate rollback target). The compose
> example's `RELAYTERM_IMAGE_TAG` only documents the three normal
> shapes.

**No `:latest`.** Operators pin explicitly: `:vX.Y.Z` for releases,
`:sha-...` for rollback to a specific build, `:main` for branch-
tracking dev / staging installs. A floating `:latest` is a footgun
when combined with `docker compose pull` — silently picking up the
next push instead of the intended tag — so we don't publish it.

**Image names.** Three flat package names under the `jsprague` owner:

| Image | Built from |
|---|---|
| `git.js-node.cc/jsprague/relayterm-backend:<tag>` | `Dockerfile.backend` (`runtime` target) |
| `git.js-node.cc/jsprague/relayterm-backend-migrate:<tag>` | `Dockerfile.backend` (`migrate` target) |
| `git.js-node.cc/jsprague/relayterm-web:<tag>` | `Dockerfile.web` |

#### 6.4.1 Required Forgejo setup

The publish job consumes exactly one secret, and it MUST be added to
the RelayTerm repo's secrets before the workflow can push images. The
workflow runs a preflight check (`verify registry token is configured`)
ahead of `docker/login-action` — when the secret is missing, that step
fails with the static, redacted message:

> `FORGEJO_REGISTRY_TOKEN repository secret is not configured or is unavailable to this workflow.`

If you see this message, the steps below have NOT been completed (or
the secret is scoped wrong, e.g. environment-scoped where the workflow
expects a repository secret).

- **`FORGEJO_REGISTRY_TOKEN`** — Forgejo personal access token with
  `write:package` scope.
  1. Forgejo → Settings → Applications → Generate new token.
  2. Name it `relayterm-ci-publish` (or similar — the name is
     bookkeeping only).
  3. Under "Permissions", grant only **`write:package`**. No `repo`,
     no `admin`, no `read:user`.
  4. Copy the token immediately (Forgejo shows it once).
  5. RelayTerm repo (`jsprague/RelayTerm`) → Settings → Secrets →
     Add Secret. Name: `FORGEJO_REGISTRY_TOKEN`. Value: the token.
     Add it as a **repository secret** (not an environment secret) so
     it's available to every `publish-images` run on `main` / `v*`
     tags / `workflow_dispatch`.

The username `${{ github.actor }}` is the Forgejo user that triggered
the run; Forgejo's container-registry login policy accepts a
`(user, write:package token)` pair as long as the user can write to
packages under the configured owner namespace (`jsprague`).

**Deploy host (pull only).** The `FORGEJO_REGISTRY_TOKEN` used in CI
has `write:package` scope — it can push. The deploy host only needs
to **pull**, so issue a separate Forgejo PAT with `read:package` only
for the host's `docker login`. Keep the write-scope token to CI; do
not copy it to the deploy host. Rotation is per-token, so a leaked
read-only host token cannot be used to publish over your release
tags.

#### 6.4.2 Pulling on the deploy host

```sh
# One-time login — interactive, the prompt accepts the same token.
docker login git.js-node.cc
# Username: jsprague
# Password: <paste FORGEJO_REGISTRY_TOKEN-equivalent>
#
# Or, non-interactively, with the token in a file the operator owns:
cat ~/.config/relayterm/registry-token | \
  docker login git.js-node.cc -u jsprague --password-stdin
```

Pin the image tag in your `.env` (image-mode example file lives at
[`deploy/docker-compose.images.example.yml`](../../deploy/docker-compose.images.example.yml)):

```env
RELAYTERM_IMAGE_TAG=v0.1.0
```

Then pull + migrate + start:

```sh
docker compose -f docker-compose.images.example.yml pull
docker compose -f docker-compose.images.example.yml \
    --profile migrate run --rm relayterm-migrate
docker compose -f docker-compose.images.example.yml up -d \
    postgres relayterm-backend relayterm-web
```

#### 6.4.3 Upgrade

```sh
sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=v0.2.0/' .env
docker compose -f docker-compose.images.example.yml pull
docker compose -f docker-compose.images.example.yml \
    --profile migrate run --rm relayterm-migrate
docker compose -f docker-compose.images.example.yml up -d \
    --no-deps relayterm-backend relayterm-web
```

The `relayterm-backend-migrate` image is published in lockstep with
the backend (same source tree, same tag). Always run `--profile
migrate run --rm relayterm-migrate` against the NEW tag before
restarting the backend on that tag — `relayterm-backend` does NOT
auto-migrate on boot.

#### 6.4.4 Rollback by `:sha-<short>` tag

Each push to main carries a `:sha-abc1234` tag (the seven-char SHA
prefix of the commit). To roll back to a previously running build:

```sh
sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=sha-abc1234/' .env
docker compose -f docker-compose.images.example.yml pull
docker compose -f docker-compose.images.example.yml up -d \
    --no-deps relayterm-backend relayterm-web
```

If the rolled-back tag predates a forward-only schema migration, you
need `sqlx migrate revert` (or a backup-restore) — pre-rehearse the
revert plan in staging. The migrate image's `revert` is documented in
§4.1.

#### 6.4.5 Auto-deploy is deferred

The publish step ends at "image is in the registry." Operators
trigger the deploy by hand (`pull` + `up -d` on the host). Watchtower
/ SSH push / GitOps are separate slices — see §8.

#### 6.4.6 Verified smoke (image-mode)

A first end-to-end image-mode deployment against
`git.js-node.cc/jsprague/relayterm-{backend,backend-migrate,web}`
has been exercised by hand with `RELAYTERM_IMAGE_TAG=sha-51a772e`
(an example tag — your deploys should pin a `:sha-<short>` from a
specific successful CI publish, or a `:vX.Y.Z` for releases). The
following observations held; this section captures the scope of what
a green run looks like, NOT a runner that re-executes on every push.

The smoke covered:

- `docker compose -f deploy/docker-compose.images.example.yml config`
  rendered cleanly with the env file contract above.
- `docker compose pull` fetched all three images from the registry.
  The pull was observed to succeed without `docker login` against
  `git.js-node.cc` because the relevant package was anonymously
  pullable at the time. **Recommendation stands**: issue a
  `read:package`-only Forgejo PAT for the deploy host and
  `docker login` with it (see §6.4.1) so a future tightening of
  registry visibility does not break pulls.
- `docker compose --profile migrate run --rm relayterm-migrate`
  applied migrations on first run. Re-running the same command was
  idempotent — `sqlx` skipped already-applied migrations.
- `docker compose up -d postgres relayterm-backend relayterm-web`
  brought the stack to `running` + `healthy` for all three services.
- `curl -i http://127.0.0.1:8081/api/v1/auth/me` returned `401`
  through the nginx proxy without a session cookie (the expected
  auth-gate behavior).
- The SPA was served at `/` with a normal `200`, `index.html` body.
- `curl -sf http://127.0.0.1:8081/_web_health` returned `ok`.
- The bootstrap `Origin` guard worked end-to-end: a state-change
  request without a matching `Origin` was rejected with `403`
  before reaching the body extractor.
- An unauthenticated WebSocket connect to
  `/api/v1/terminal-sessions/:id/ws` reached the backend and was
  rejected with `401` (the auth gate runs before the upgrade).
- A targeted log-leakage sweep against the backend stdout for the
  redaction sentinels (session token, vault internals, terminal
  I/O, recording bytes) was clean.
- No source changes were needed to land the smoke.

Two follow-ups surfaced and are tracked separately:

- The duplicate `Content-Type` on `/_web_health` is fixed in this
  commit. The endpoint now sets `default_type text/plain` and
  returns a single header.
- Auto-deploy (CI → host pull/restart) is intentionally deferred —
  see §6.4.5 and §8.

Future deployments should prefer pinning the immutable
`:sha-<short>` tag from a known-green CI publish, or the
`:vX.Y.Z` tag of a tagged release. `:main` is fine for branch-
tracking dev / staging installs but is mutable on the next push to
`main`.

---

## 7. Smoke / verification checklist

The full operator-side smoke (including bad-Origin, log-leakage sweep,
WebSocket auth gate, and the deeper variants) lives in
[`docs/deployment/production-runbook.md`](./production-runbook.md) §10.
The minimum viable checklist for a quick post-deploy sanity:

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

## 8. Deferred / out of scope

Tracking ledger for things this slice intentionally does NOT cover.
File these as separate slices when they're needed:

- Kubernetes / Helm / Nomad manifests.
- Multi-node HA (active-active backend with shared session state).
- Image signing (cosign / notary v2).
- SBOM generation.
- Vulnerability scanning of published images.
- Multi-arch (`linux/arm64`) image variants.
- Automated production secrets management (Vault auto-unwrap, etc).
- Backup automation (snapshots, off-site replication).
- Zero-downtime deploys (rolling restart, blue/green).
- Auto-deploy from CI to a host. The publish job ends at "image is
  in the registry"; Watchtower, SSH push, and GitOps are all
  separate slices.
- Registry retention / cleanup policies (pruning old `:sha-*` tags).
- Production renderer selector (production stays on the
  `@relayterm/terminal-xterm` baseline; experimental renderers are
  dev-lab-only).
- Tauri v2 desktop / mobile (Android-first) packaging and CI —
  separate deployment track. The Docker image publish documented
  here only covers the server (`relayterm-backend`,
  `relayterm-backend-migrate`) and web (`relayterm-web`)
  deployment. Desktop and mobile shells live under
  `apps/desktop/` and `apps/mobile/` respectively and have no CI
  release workflow yet.
