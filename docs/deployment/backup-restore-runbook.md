# RelayTerm v1 backup, restore, and rollback runbook

> Manual operator runbook for backing up, restoring, and rolling
> back a single self-hosted RelayTerm v1 Docker Compose deploy.
> Walk this before every upgrade. Re-read the relevant section
> before any restore or rollback. **No automation ships here** —
> every step is operator-driven.
>
> **Status as of 2026-05-17:** drafted on
> `docs/backup-restore-runbook` against the snapshot of `main` at
> commit `3cadbc7` ("feat(web): improve mobile shell usability").
> This file composes existing primitives — it does NOT redefine
> any contract in `AGENTS.md`, `SPEC.md`, `docs/spec/*`, or
> [`docs/deployment/production-runbook.md`](production-runbook.md).
> Where this doc disagrees with any upstream contract, the upstream
> wins and this doc is the bug.

## 1. Purpose and scope

This runbook is the operator-facing manual procedure for:

- Taking a backup of a running RelayTerm v1 deploy before an
  upgrade.
- Restoring from one of those backups on the same host (or a
  rebuilt host).
- Rolling back the deployed image to a previously known-good tag.
- Knowing which of "rollback by image tag" or "rollback by
  restoring the database" applies for a given upgrade.

**In scope.** A single self-hosted RelayTerm v1 deploy running
the published Compose stack from
[`deploy/docker-compose.production.example.yml`](../../deploy/docker-compose.production.example.yml)
(or the equivalent
[`deploy/docker-compose.images.example.yml`](../../deploy/docker-compose.images.example.yml))
behind an outer reverse proxy, operated by a single self-hosted
operator who is also the only RelayTerm user.

**Out of scope** (see §12 for the full list). Anything that
implies automation, scheduling, multi-host or cloud-managed
storage, or per-chunk recording encryption is deferred.

This runbook composes:

- [`docs/deployment/production-runbook.md`](production-runbook.md)
  §4 (first deploy), §6 (rollback), §7 (migration), §8 (backup +
  restore reminders), §10 (post-deploy smoke), §11 (secret
  rotation).
- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  §5 (deployment checks), §10 (backup / restore / rollback
  checks), §12 (decision table).
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  §4.4 (deployment status table — backup / restore rows) and §9
  (deployment cutline).
- [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
  §3 (prerequisites) and §5 row P (DB backup evidence).
- [`docs/terminal-recording.md`](../terminal-recording.md)
  Section 7 (privacy posture) and Section 12 (retention) —
  consulted only when recording is enabled.

## 2. What must be protected

The protect-list below is the v1 blast radius for a "the deploy
host died" or "the DB got corrupted" event. Each row is one piece
of state whose loss is irrecoverable from the rest of the system.

| Item | Where it lives by default | Lost without backup? |
|---|---|---|
| Postgres database (users, password hashes, hosts, identities, server profiles, known-host entries, terminal session metadata + history, session events, audit events, optional recording chunks/markers) | Docker named volume `relayterm-pgdata` (or external managed Postgres) | **Yes** — all RelayTerm state |
| RelayTerm `.env` (session signing key, vault master key, DB password, bootstrap token if still set, knob overrides) | `<compose-dir>/.env` on the deploy host (`chmod 600`) | **Yes** — the vault master key in this file is the ONLY way to decrypt stored SSH identities |
| Docker Compose file | `<compose-dir>/docker-compose.yml` on the deploy host (copy of `deploy/docker-compose.production.example.yml` or `deploy/docker-compose.images.example.yml`) | No — can be re-fetched from the repo, but back it up to record any local overrides |
| Outer reverse-proxy config (Traefik dynamic config, Caddyfile, outer-nginx site config) | Operator-chosen — typically `/etc/traefik/`, `/etc/caddy/`, `/etc/nginx/conf.d/` | **Yes** — needed verbatim to restore the same public origin / TLS / WS-upgrade / `Origin` preservation posture |
| TLS / ACME state if managed locally by the outer proxy | Operator-chosen — typically `/etc/letsencrypt/`, Caddy's `data/` dir, Traefik's `acme.json` | **Partial** — ACME can re-issue, but you eat a rate-limit hit and a brief TLS outage; back it up if practical |
| Image tag / digest currently deployed AND the previous known-good tag / digest | `RELAYTERM_IMAGE_TAG` in `.env` PLUS your deploy log entry | **Yes** — required for §6 rollback. Record both `vX.Y.Z` AND `sha-<short>` |
| Vault master key material (`RELAYTERM_VAULT__MASTER_KEY_B64`) | Inside `.env` (above) | **Yes** — already inside `.env`; called out separately because losing this alone makes every stored SSH identity unrecoverable even if the DB survives |
| Recording master key material (if recording is enabled; SEPARATE from the vault master key per `SPEC.md`) | Inside `.env` (post-v1 surface; not wired today) | **Yes (when present)** — chunks are renderer-neutral bytes today but the SPEC reserves a separate recording master key for the future encrypted-chunk envelope |
| Persistent volumes other than `relayterm-pgdata` | None today | n/a |

**Special-case rows that are part of Postgres history, not a
separate file.** `terminal_sessions`, `terminal_session_attachments`,
`session_events`, `audit_events`, and (when recording is on)
`terminal_recording_chunks` / `terminal_recording_markers` all live
inside the Postgres dump. They are NOT separate backup targets —
the `pg_dump -Fc` step in §4 captures them.

**What is NOT in the protect-list.** Source code (it is in git),
published images (they are in the registry), the Postgres `data/`
directory bit-for-bit (we back up the logical dump instead), and
the inner `relayterm-web` nginx config (it is baked into the
`relayterm-web` image).

## 3. Sensitive material warning

Treat every artifact in this runbook the way you treat the vault
master key. The combination of "a Postgres dump" + "the `.env`
file" is full disclosure of every stored SSH identity and every
recorded terminal byte; the two MUST share the same protected
storage if they share the same storage at all.

- **Do NOT paste secrets into docs, chat, screenshots, or
  bug reports.** This includes the bootstrap token, the session
  signing key, the vault master key, the DB password, any image
  digest taken from a host with sensitive labels in its tags, and
  any `Set-Cookie` value.
- **Do NOT commit `.env` files** to git. The repo's `.gitignore`
  already covers `.env`; do not override it. The example file is
  `deploy/relayterm.env.example` — it is the only `.env`-shaped
  file in the repo, and every value in it is a placeholder.
- **Do NOT commit vault master key material** anywhere. The vault
  master key in `RELAYTERM_VAULT__MASTER_KEY_B64` AEAD-wraps
  every stored SSH private key; the encrypted bytes in
  `ssh_identities.encrypted_private_key` are useless without it
  and full-disclosure with it. Keep it out of git, out of CI
  secrets, out of screenshots, and (where practical) on storage
  physically separate from the database backup.
- **Do NOT commit Postgres dumps.** A dump contains password
  hashes, `encrypted_private_key` bytes, audit history, and
  (when recording is on) terminal output bytes.
- **Store backups encrypted at rest where possible.** "Encrypted
  at rest" can be at the filesystem layer (LUKS, ZFS-native
  encryption), at the object-storage layer (S3-compatible
  server-side encryption with operator-held keys), or at the
  dump layer (`gpg --symmetric` or equivalent before upload).
  The exact tool is yours; the rule is that the at-rest copy is
  not plaintext on shared storage.
- **Keep vault master key and recording master key on separate
  storage from the database backup when feasible.** The point is
  to make "a single bucket leak" not be game-over. A practical
  posture for a single-operator deploy: DB dumps + `.env` in one
  off-host location; an additional copy of `.env` (which carries
  the keys) on a second offline medium the operator personally
  controls (encrypted USB stick, hardware password manager
  attachment). When recording is enabled, the future recording
  master key SHOULD be stored alongside the vault master key
  with the same separation discipline.
- **Suppress shell history when secrets are on the command
  line.** `set +o history` (bash/zsh) / `set fish_history ""`
  (fish) before any command that reads or writes a secret. The
  auth-smoke runbook ([`docs/auth-smoke.md`](../auth-smoke.md)
  "Prerequisites") has the per-shell pattern.
- **Redaction sentinel discipline still applies to backup
  evidence.** Do not paste the contents of a dump file, an
  `.env`, or a log excerpt into the release log / deploy log /
  bug tracker without running the
  [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  §11 grep first. The sentinel set lives in
  [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
  §5.1.

## 4. Pre-upgrade backup procedure

Run this before every upgrade (§5 of the production runbook) and
before any other risky operation (schema migration, secret
rotation, image-tag rollback, restore rehearsal). The whole
procedure is operator-driven; nothing in the stack runs it on a
schedule.

Throughout, replace placeholders verbatim:

- `<compose-dir>` — the directory holding `docker-compose.yml`
  and `.env` (matches runbook §4.1; example: `/srv/relayterm`).
- `<backup-dir>` — an off-host or otherwise protected location
  the operator has chosen. NOT the same volume as the running
  database. Example: a mounted object-storage path, a separate
  encrypted volume, a remote rsync target. Do NOT use a directory
  inside `<compose-dir>` or inside the `relayterm-pgdata` volume.
- `<tag>` — the `RELAYTERM_IMAGE_TAG` value currently deployed
  (e.g. `vX.Y.Z` or `sha-<short>`).

### 4.1 Identify what is running

```sh
cd <compose-dir>
docker compose ps
docker compose images
```

`docker compose ps` should show `postgres`, `relayterm-backend`,
and `relayterm-web` as `running` (and `(healthy)` where
configured). `docker compose images` records the resolved digest
per service — copy these values into the deploy log so the
rollback step in §6 has an unambiguous target.

Record the running container names too — under the default
Compose project name they will be `relayterm-postgres-1`,
`relayterm-relayterm-backend-1`, and `relayterm-relayterm-web-1`.
The commands below assume `docker compose exec <service>` works
from `<compose-dir>`; if you have customised the project name,
substitute accordingly.

### 4.2 Record the deployed image tag and digest

For each of `relayterm-backend`, `relayterm-backend-migrate`,
and `relayterm-web`, record:

- `RELAYTERM_IMAGE_TAG` value from `.env`.
- Resolved image digest from `docker compose images`.
- Source commit (`git log -1 --format='%H'` against the local
  clone you used to deploy, OR the CI publish URL for the tag).

Stash this in the deploy log. **You will need it to roll back in
§6.**

### 4.3 Record the current migration version

```sh
docker compose --profile migrate run --rm relayterm-migrate \
    migrate info --source /app/migrations
```

The final applied migration ID is the "schema version" of the
backup you are about to take. Record it next to the deploy log
entry. The full list of v1 migrations lives in
`apps/backend/migrations/` (timestamped names; the v1 set is the
`20260428000001_users.sql` through `20260510000022_known_host_entries_revoke_metadata.sql`
range as of `3cadbc7`).

### 4.4 Take the Postgres dump

```sh
mkdir -p "<backup-dir>"
cd <compose-dir>
docker compose exec -T postgres \
    pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc \
    > "<backup-dir>/relayterm-pre-<tag>-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

`-Fc` is the custom binary format `pg_restore` consumes
directly. It is smaller than plain SQL and supports parallel
restore. Do NOT use `-Fp` unless you have a specific reason —
plain-SQL dumps round-trip identically but are large and harder
to validate.

If the Compose stack is running with `POSTGRES_USER` /
`POSTGRES_DB` shell-expansion turned off in your environment,
substitute the literal values from `.env` instead of `"$..."`.

### 4.5 Verify the dump exists and is non-zero

```sh
ls -lah "<backup-dir>"/relayterm-pre-<tag>-*.dump
```

The file MUST exist and MUST have a non-zero size. A `pg_dump`
that returned exit code 0 but wrote zero bytes is a known
operator footgun — the redirect step above silently writes an
empty file if `pg_dump` itself errored. If the size is zero or
suspiciously small (under ~10 KiB for a fresh deploy, under
typical operator data for an upgrade), STOP and investigate
before proceeding to the upgrade.

### 4.6 Optional: record a checksum

```sh
sha256sum "<backup-dir>"/relayterm-pre-<tag>-*.dump \
    > "<backup-dir>"/relayterm-pre-<tag>-*.dump.sha256
```

Recommended for backups that will travel (object storage, remote
sync). Lets a future restore verify the dump matches what was
written. Not required for backups that stay on the same protected
host the dump was written on.

### 4.7 Back up `.env`, the Compose file, and any reverse-proxy config

```sh
config_ts=$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "<backup-dir>/config-<tag>-${config_ts}"
cp -p <compose-dir>/.env \
       <compose-dir>/docker-compose.yml \
       "<backup-dir>/config-<tag>-${config_ts}/"
```

`-p` preserves `chmod 600` on `.env`. Capturing the timestamp
in `config_ts` first avoids a race where the `mkdir` and `cp`
forks tick across a second boundary and end up using different
directory names. Confirm afterwards with
`ls -l "<backup-dir>/config-<tag>-${config_ts}/"`.

If your outer reverse proxy has its own config file(s) (Traefik
dynamic config, a Caddyfile, an outer-nginx site config, the
ACME state directory if you keep it), back those up to the same
config directory with the same `-p` flag preservation. Tag the
copy so the restore step knows which proxy version it came from
— e.g. `traefik.yml`, `caddyfile-2026-05-17.txt`.

A compact alternative if you want a single archive instead of a
directory tree:

```sh
config_ts=$(date -u +%Y%m%dT%H%M%SZ)
tar -C <compose-dir> -czf \
    "<backup-dir>/config-<tag>-${config_ts}.tgz" \
    .env docker-compose.yml
chmod 600 "<backup-dir>/config-<tag>-${config_ts}.tgz"
```

The `chmod 600` is load-bearing — the tarball carries the vault
master key.

### 4.8 Record the current git commit / tag of the deployed image

If you deployed from a clone of this repo, record `git rev-parse
HEAD` (and any tag annotation) for traceability. If you deployed
purely from a published image, the source commit is already
recorded in §4.2 (it is encoded in the `sha-<short>` tag).

### 4.9 Optional: write an encrypted copy

If the off-host storage is shared, untrusted, or you simply want
defence-in-depth, encrypt the dump + config bundle with a tool
the operator already uses. A neutral example with `gpg`:

```sh
gpg --symmetric --cipher-algo AES256 \
    "<backup-dir>"/relayterm-pre-<tag>-*.dump
shred -u "<backup-dir>"/relayterm-pre-<tag>-*.dump  # only after .gpg exists
```

Substitute your preferred tool (`age`, `openssl enc`, the
operator's password manager) — the rule is that the at-rest
artifact on shared storage is not plaintext.

### 4.10 Record the backup in the deploy log

A minimal entry for the deploy log (e.g. `/srv/relayterm/deploy.log`):

```
YYYY-MM-DDThh:mm:ssZ  backup
  image_tag           : <tag>
  backend_digest      : sha256:<…>
  web_digest          : sha256:<…>
  migrate_digest      : sha256:<…>
  migration_version   : <id>
  dump_path           : <backup-dir>/relayterm-pre-<tag>-<ts>.dump
  dump_size_bytes     : <N>
  dump_sha256         : <hex>     # if §4.6 ran
  config_path         : <backup-dir>/config-<tag>-<ts>/  (or .tgz)
  git_commit          : <sha>     # if §4.8 ran
  notes               : <free text — operator>
```

This entry is what §5 (restore) and §6 (rollback) look up.

## 5. Restore procedure

Restore is the manual recovery path: "the database is broken,
the host died, an upgrade went wrong in a way image rollback
alone cannot fix." Walk this top to bottom; do not skip steps.

> **Destructive warning.** This procedure replaces the contents
> of the live Postgres database. Read §5.0 in full before
> running any of the commands below. A successful restore of the
> wrong dump silently destroys the current operator state with
> no automated recovery path; the only undo is "restore from the
> dump you took right before the restore," which requires you to
> have taken one (§5.1.0).

### 5.0 Confirm the scenario

Pick the matching case BEFORE touching the running stack:

- **Case R-A** — restore onto the same Compose stack, replacing
  the current DB contents. Used for: "an upgrade migration broke
  data," "a destructive operation went wrong," "I need to test
  the restore procedure end-to-end against a real backup." The
  procedure below covers this case.
- **Case R-B** — restore onto a *separate* throwaway Postgres
  for verification / rehearsal, without touching the live stack.
  Recommended for §9 below and for the
  `docs/backup-restore-rehearsal-record` follow-up slice
  (`docs/v1-production-readiness.md` §7 honourable mentions).
  Use a fresh Compose project (different `name:`), a fresh
  volume, and a fresh `.env` cloned for the rehearsal.
- **Case R-C** — restore onto a rebuilt host after total host
  loss. Same procedure as R-A, but you also need to restore
  `.env`, the Compose file, and the outer reverse-proxy config
  from §4.7 first.

Cases R-A and R-C share §5.1–§5.6 below. Case R-B follows the
same steps against a separate Compose project — flag it
explicitly in your deploy log so the rehearsal does not get
mistaken for a production restore.

### 5.1 Stop the app containers

Stop only `relayterm-backend` and `relayterm-web`. Leave
Postgres running — you need it to accept the restore.

```sh
cd <compose-dir>
docker compose stop relayterm-backend relayterm-web
docker compose ps
```

Confirm both app services show `exited` and Postgres shows
`running (healthy)`.

#### 5.1.0 Take a fresh dump of the *current* (about-to-be-replaced) DB

This is your only undo if the restore turns out to be wrong.
Skip it ONLY if the current DB is already proven corrupt to the
point where `pg_dump` itself fails (rare).

```sh
docker compose exec -T postgres \
    pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc \
    > "<backup-dir>/relayterm-pre-restore-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

Verify size per §4.5 before proceeding.

### 5.2 Start Postgres (if Case R-C)

If you are restoring onto a rebuilt host, restore `.env` +
`docker-compose.yml` from §4.7 first, then:

```sh
cd <compose-dir>
docker compose up -d postgres
docker compose ps
```

Wait for `(healthy)`. Do NOT start the migrate, backend, or web
containers yet.

### 5.3 Restore the dump

> **Destructive: the next command replaces the live DB
> contents.** Read §5.4 about migration-version compatibility
> before running it.

The recommended path for v1 is "restore into a clean database":

```sh
cd <compose-dir>
# Verify the dump file before piping it into psql.
ls -lah "<backup-dir>/relayterm-<…>.dump"

# Drop and recreate the application DB. This destroys current
# contents of $POSTGRES_DB.
docker compose exec -T postgres \
    psql -U "$POSTGRES_USER" -d postgres -c \
    "DROP DATABASE IF EXISTS \"$POSTGRES_DB\";"
docker compose exec -T postgres \
    psql -U "$POSTGRES_USER" -d postgres -c \
    "CREATE DATABASE \"$POSTGRES_DB\";"

# Restore the dump.
docker compose exec -T postgres \
    pg_restore -U "$POSTGRES_USER" -d "$POSTGRES_DB" --no-owner --clean --if-exists \
    < "<backup-dir>/relayterm-<…>.dump"
```

If `pg_restore` reports warnings about objects that already
exist, double-check that the DROP / CREATE pair above actually
ran against the right DB.

Alternative ("restore into existing DB"): if you have a reason
NOT to drop and recreate (e.g. you want to keep extensions
installed under that DB), `pg_restore --clean --if-exists`
against the existing DB works, but the failure modes are
strictly worse than DROP / CREATE — partial restores leave the
DB in an indeterminate state. Prefer DROP / CREATE for v1.

### 5.4 Reconcile migration version

The restored DB is at whatever migration version the dump was
taken at (§4.3). Compare against the deployed image tag's
expected migration version:

- **Same version.** No action required. Skip to §5.5.
- **Restored version is older than the image expects.** The
  image will return `500 internal_error` on routes that touch
  the gap until you run the migrate container:

  ```sh
  docker compose --profile migrate run --rm relayterm-migrate
  ```

  This is the same migrate step as the upgrade flow (runbook
  §4.8 / §7). After it returns exit code 0 with the new applied
  migration ID, proceed.
- **Restored version is *newer* than the image expects.** This
  is the case the production runbook §6.2 calls out: the
  database is ahead of the code. Pin
  `RELAYTERM_IMAGE_TAG` to a build that matches the restored
  schema BEFORE starting the backend (use the tag recorded in
  §4.2 of the backup, or any tag from that schema range), then
  continue.

### 5.5 Re-apply config and start the stack

If Case R-C, confirm `.env` is in place with `chmod 600`
preserved. For R-A, `.env` was never removed.

```sh
cd <compose-dir>
docker compose up -d postgres relayterm-backend relayterm-web
docker compose ps
```

Wait until all three reach `(healthy)`. Do NOT start the
migrate container again unless §5.4 told you to.

### 5.6 Health checks and sanity walk

Each of these is a hard gate. Do not declare the restore done
until all of them pass. Probe semantics (mirrors
[`production-runbook.md`](production-runbook.md) §10):
`/healthz` is a backend process-alive probe (static
`{"status":"ok"}`; not DB readiness); `/_web_health` is an
nginx-static probe on the web container (does not reach the
backend); the unauthenticated `/api/v1/auth/me` returns `401` —
**`401` is the expected pass condition**, not a failure.

- `docker compose ps` — `(healthy)` on `postgres`,
  `relayterm-backend`, `relayterm-web`. `(healthy)` on
  `relayterm-backend` is process-alive only; the `postgres` row
  (driven by `pg_isready`) is the corresponding DB-side
  liveness — both are required after a restore.
- `curl -sf http://127.0.0.1:8081/_web_health` → `ok`.
- `curl -sf http://127.0.0.1:8081/healthz` → `{"status":"ok"}`.
- `curl -i http://127.0.0.1:8081/api/v1/auth/me` without cookie
  → `401` (expected; a `2xx` here means the auth gate is
  missing from the protected route — STOP).
- **Login check.** Sign in through the SPA at the production
  origin with the operator's existing credentials. `GET
  /api/v1/auth/me` returns the user. (If the operator's
  password changed after the dump was taken, the pre-dump
  password is what works post-restore — `user_passwords` was
  restored to the dump's state.)
- **Inventory sanity check.** `IdentitiesView.svelte`,
  `ServersView.svelte` (Hosts + Server profiles) all load and
  list the rows you expect from the dump. The identity detail
  panel shows NO `private_key` / `encrypted_private_key` / raw
  PEM bytes / `BEGIN OPENSSH PRIVATE KEY` substring anywhere
  (per the v1 release checklist §7 row on identity detail
  redaction).
- **Sessions list check.** `SessionsView.svelte` loads. Old
  `closed` sessions appear as historical metadata.
- **Terminal xterm launch sanity check.** Launch a fresh xterm
  session against a trusted, auth-checked profile. Prompt
  appears; `whoami` returns the expected user. Close. (This
  exercises the vault master key — if it is wrong, host-key
  preflight / auth-check / launch will fail with a clean error
  even though every other route works.)
- **Redaction sweep on the restore window.** Per the v1
  release-checklist §11 / v1 production smoke §M:

  ```sh
  docker compose logs --since 1h relayterm-backend | \
    grep -E 'relayterm_session=[A-Za-z0-9_-]{20,}|encrypted_private_key|data_b64|BEGIN OPENSSH PRIVATE KEY|token_hash' \
    || echo "ok: no leakage sentinels found"
  ```

  MUST print `ok: no leakage sentinels found`. Any hit is a
  security regression; treat it as one — do NOT declare the
  restore done.

If any gate above fails, the restore is NOT done. Do not
re-import data, do not let the operator log back in for "real"
work, and consider rolling back to the §5.1.0 dump.

## 6. Rollback procedure

Rollback is for "the upgrade was wrong" — image-tag rollback is
the cheap path and works when the schema change in the upgrade
was backward-compatible. When it is not, the path is restore
from backup (§5) AND rollback the image tag.

### 6.1 Rollback by image tag (backward-compatible schema only)

This is the production-runbook §6.1 path; the runbook is the
authoritative source. The summary here is the safe-default
operator workflow.

Prerequisites:

- The previous known-good `RELAYTERM_IMAGE_TAG` value
  (`vX.Y.Z` or `sha-<short>`) is recorded — from §4.2 of the
  pre-upgrade backup, or from the deploy log.
- The schema change between the two tags is backward-compatible
  (the older code can tolerate the newer schema). If you cannot
  tell, **assume it is NOT** and go to §6.2.

Procedure:

```sh
cd <compose-dir>
sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=<previous-tag>/' .env
docker compose pull
docker compose up -d --no-deps relayterm-backend relayterm-web
docker compose ps
```

Confirm `(healthy)`. Walk runbook §10 (post-deploy smoke) — the
short version is "healthz + web_health + 401 on unauthenticated
/auth/me + login + protected route + redaction sweep." Walk
release-checklist §11 (redaction sweep) on the rollback window.

If `docker compose pull` says the previous tag is no longer
available (registry retention cleaned it up), use the digest
recorded in §4.2 — `docker pull <repo>@sha256:<digest>` and
then `docker tag` it to the `<previous-tag>` value so Compose
finds it.

### 6.2 Rollback by restoring the database (backward-incompatible schema)

When the upgrade carried a backward-incompatible migration
(column removed, type narrowed, NOT-NULL added on a column the
old code does not populate), the image rollback alone leaves
the database ahead of the code. The operator path is:

1. Stop the app containers (`docker compose stop
   relayterm-backend relayterm-web`).
2. Restore the pre-upgrade dump per §5 — but at §5.4 you DO
   NOT re-run the migrate container (the older tag expects the
   older schema, which is what the dump carries).
3. Pin `RELAYTERM_IMAGE_TAG` to the previous known-good tag
   per §6.1.
4. `docker compose up -d --no-deps relayterm-backend
   relayterm-web`.
5. Walk §5.6 (health + sanity + redaction sweep) as the gate.

If you cannot quickly decide between §6.1 and §6.2, **assume
§6.2** — runbook §6.3 carries the same advice. A 20-minute
restore beats a corrupted database.

### 6.3 Migrations caveat

Application rollback (changing `RELAYTERM_IMAGE_TAG`) does NOT
reverse database migrations. `sqlx migrate revert` exists in
the migrate image and steps back one migration if that
migration ships a `down` step, but:

- v1 migrations do not ship `down` steps as a deliberate
  default; treat the schema as forward-only.
- A multi-step upgrade with mixed `down` coverage is not safe
  to revert casually on production data.
- For v1, the documented and supported "schema rollback" path
  is restore-from-backup (§5 + §6.2). There is no v1-supported
  automated `migrate down` for a backward-incompatible upgrade.

This is consistent with runbook §6.2 and §7.

## 7. Database migration caveat

A normative summary of the migration story as it relates to
backup and restore:

- **Migrations are forward-only by default.** The v1 migration
  set in `apps/backend/migrations/` does not ship `down` steps
  as a routine matter. Treat any forward migration as a one-way
  schema change for backup-and-restore planning.
- **Application rollback without database restore is safe ONLY
  when the older app can tolerate the migrated schema** — i.e.
  when the upgrade added nullable columns, added new tables the
  older code does not reference, or made other strictly
  additive changes. Operators who cannot determine this from
  the release notes / commit log MUST assume the migration is
  not backward-compatible and use §6.2.
- **For v1, treat a fresh DB backup as REQUIRED before every
  upgrade.** The v1 release checklist §5 names this as a
  required row (`pg_dump -Fc` before the upgrade). The cost is
  small; the upside is that §6.2 is always available.
- **The migrate container is published in lockstep with the
  backend.** `relayterm-backend-migrate:<tag>` and
  `relayterm-backend:<tag>` are built from the same commit and
  carry the same migration set. Pin them to the same value of
  `RELAYTERM_IMAGE_TAG`.
- **Migrations run only when the operator runs them.** The
  backend does not auto-migrate on boot (runbook §7). A
  restored DB that is older than the deployed image will surface
  the gap as `500 internal_error` on routes that touch the gap
  until the migrate container is run.

## 8. Recording-specific backup caveat

Terminal recording is **OFF by default** at v1
(`RELAYTERM_TERMINAL_RECORDING__ENABLED=false`, per
[`docs/v1-release-checklist.md`](../v1-release-checklist.md) §4
and [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
§4.4). The default v1 deploy has no recording state at rest
beyond the (empty) `terminal_recording_chunks` /
`terminal_recording_markers` tables that the `pg_dump` in §4
already captures.

### 8.1 If recording is OFF (default v1)

- No additional backup target. The dump file from §4 is
  complete.
- The retention worker may still be enabled
  (`RELAYTERM_TERMINAL_RECORDING__CLEANUP__PERIODIC_SWEEP_ENABLED=true`
  is the example default); it is a no-op when there are no
  chunks to purge.
- No additional key material to protect — the SPEC.md-reserved
  "recording master key, separate from the vault master key"
  has not been wired in v1.

### 8.2 If the operator opted recording IN

Read [`docs/terminal-recording.md`](../terminal-recording.md)
Section 7 (privacy posture) and Section 12 (retention) end-to-end
before relying on a recording-enabled backup workflow. Notable
properties:

- **Recording chunks live inside Postgres** in
  `terminal_recording_chunks.payload`. The `pg_dump -Fc` in §4
  captures them as part of the standard DB dump — no separate
  storage target.
- **Markers live in `terminal_recording_markers`.** Same — part
  of the DB dump.
- **Chunk bytes contain everything the terminal printed.** Treat
  the DB dump as carrying secret material. The §3 sensitive
  material warning applies with extra force: a dump from a
  recording-enabled deploy must NEVER land on plaintext shared
  storage.
- **Do not extract or paste chunk bytes anywhere outside the
  dump.** The SPEC and `docs/agent/redaction-rules.md` § 11
  forbid `terminal_recording_chunks.payload` (and the future
  envelope) from any log / audit / Error / HTTP body / UI cell /
  `data-*` / browser storage / `Debug`. That rule applies to
  backup tooling too — do NOT write a "diagnostic dump" that
  extracts chunk bytes into a separate file.
- **Recording master key management.** When the SPEC's
  reserved-but-not-yet-wired recording master key surface lands,
  it MUST be stored separately from the SSH-identity vault
  master key (per [`SPEC.md`](../../SPEC.md) → "Durable
  terminal recording and replay"). Until that surface lands,
  this row is informational only — there is no recording master
  key in `.env` to back up at v1.
- **Retention cleanup interaction.** The retention worker may
  purge old chunks between the moment you took the backup and
  the moment you would have read them. A restore from an older
  dump may surface chunks the current retention policy would
  delete on its next sweep tick (Stage A at boot, Stage B
  periodic). This is benign — the worker will sweep them on its
  next run — but worth knowing so a "I restored an old dump and
  some chunks vanished an hour later" observation is not
  misread as a leak.

## 9. Verification checklist

Walk these gates as part of the standard backup / restore
discipline. Each block is independently meaningful.

### 9.1 After a backup (§4)

- [ ] Dump file exists at the recorded path (`ls -lah` confirms).
- [ ] Dump file size is non-zero and matches the rough operator
  expectation (zero / suspiciously small = STOP, investigate).
- [ ] Dump SHA-256 recorded if §4.6 ran.
- [ ] Config archive (`.env` + Compose file + reverse-proxy
  config if separate) exists at the recorded path with
  `chmod 600` preserved on `.env`.
- [ ] Vault master key + (if applicable) recording master key
  storage strategy reviewed against §3. Keys are NOT in the
  same single failure domain as the database dump if your
  threat model demands separation.
- [ ] Image tag AND digest recorded for `relayterm-backend`,
  `relayterm-web`, and `relayterm-backend-migrate`.
- [ ] Migration version recorded.
- [ ] Restore command dry-run reviewed against §5 — read the
  exact `pg_restore` / DROP / CREATE block and walk it in your
  head before relying on it for a real incident. The cheap way
  to actually-rehearse is the §5 Case R-B path against a
  separate Compose project.

### 9.2 After a restore (§5)

`/healthz` is process-alive (not DB readiness); `/_web_health`
is nginx-static (does not reach the backend); the
unauthenticated `/api/v1/auth/me → 401` is a routing + auth-gate
sanity check, and `401` is the expected pass condition. See §5.6
for the full context.

- [ ] `docker compose ps` shows `(healthy)` on `postgres`,
  `relayterm-backend`, `relayterm-web`.
- [ ] `curl -sf http://127.0.0.1:8081/_web_health` → `ok`.
- [ ] `curl -sf http://127.0.0.1:8081/healthz` →
  `{"status":"ok"}`.
- [ ] `curl -i http://127.0.0.1:8081/api/v1/auth/me`
  unauthenticated → `401` (expected; `2xx` here is the
  failure case).
- [ ] SPA login with operator credentials succeeds.
- [ ] Sessions page loads.
- [ ] Inventory loads (`IdentitiesView`, `ServersView`).
- [ ] Identity detail panel shows public-key metadata + SHA-256
  fingerprint only — NO `private_key`, NO
  `encrypted_private_key`, NO raw PEM, NO
  `BEGIN OPENSSH PRIVATE KEY` substring anywhere.
- [ ] xterm launch against a trusted, auth-checked profile
  works; `whoami` returns the expected user.
- [ ] Redaction sweep over the restore window's backend log
  returns `ok: no leakage sentinels found` (release-checklist
  §11 grep; v1 production-smoke §5.3 sweep query).
- [ ] No `terminal_recording_chunks` payload bytes leaked into
  any log line, response body, or `data-*` attribute (only
  meaningful when §8.2 applies).

## 10. Rehearsal record template

Use this short-form template for any operator-recorded rehearsal
entry the operator chooses to keep alongside the deploy log. The
canonical, fuller rehearsal log is
[`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
— that file's §5 template is a superset of this block (preconditions
checklist, restore-target isolation statement, per-row PASS/FAIL
gates, redaction sweep). Use this short form for ad-hoc deploy-log
entries; use the canonical log for entries that close the
v1-release-checklist §10 / §12 "Restore-from-backup rehearsal"
row.

```
## YYYY-MM-DD · Backup-restore rehearsal

- Date / time            : YYYY-MM-DDThh:mm:ssZ
- Source deployment      : <hostname or compose project>
- Backup file            : <backup-dir>/relayterm-<…>.dump
- Backup SHA-256         : <hex>
- Dump size (bytes)      : <N>
- Backup taken at        : YYYY-MM-DDThh:mm:ssZ
- Restore target         : <Case R-A | R-B | R-C>; target stack /
                           project name
- Image tag deployed     : <vX.Y.Z>   (digest sha256:<…>)
- Migration version      : <id>
- Result                 : PASS  /  PASS-WITH-CAVEATS  /  FAIL
- Caveats / notes        : <free text — what was unexpected,
                           what needed manual intervention, any
                           sentinel hits, any timing surprises>
- Operator               : <name / handle>
- Next rehearsal due     : YYYY-MM-DD (optional)
```

Recommended cadence: one rehearsal before the first production
deploy (B2-adjacent — closes the §11 "rehearsal pending" row),
and one rehearsal per quarter thereafter. Treat any backup that
has never been restored as a hope, not a backup (runbook §8.3).

## 11. Integration with the v1 release checklist

How this runbook plugs into the v1 cutline:

- **v1 can ship with the runbook in place AND the rehearsal
  template in place before a full restore rehearsal has been
  recorded.** This runbook closes the documentation gap; the
  rehearsal template at
  [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
  closes the template gap; the first dated entry in that
  file's §10 verification log closes the verification gap.
  The release-checklist §10 row "Restore-from-backup
  rehearsal" remains PENDING until that first dated entry
  records PASS. The
  [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  §4.4 row "Restore-test rehearsal" reads "DONE / runbook +
  rehearsal template exist; actual rehearsal pending" with
  the same meaning.
- **Before public / personal production reliance, at least one
  backup MUST have been taken.** Spinning up a fresh deploy and
  using it for real SSH work without ever exercising §4 is the
  failure mode this runbook is meant to prevent.
- **Before any risky upgrade, a backup is MANDATORY.** "Risky"
  = any upgrade across a schema change you have not personally
  reviewed for backward-compatibility, any upgrade that touches
  the auth surface, any upgrade that touches recording, any
  upgrade marked operator-action-required in release notes.
  Runbook §5 (upgrade checklist) names the backup step as
  required; this runbook is the procedure that step refers to.
- **A full restore rehearsal is STRONGLY RECOMMENDED before
  treating any production deploy as durable.** It may become a
  later gate (e.g. on a v1.x point release) once the
  `docs/backup-restore-rehearsal-record` slice lands and the
  operator has actually walked it. For v1 itself the rehearsal
  is recommended, not blocking.

## 12. Non-goals

Tracked here so the runbook stays honest and the v1 cutline
stays single-page. Each line is a separate post-v1 slice.

- **Automated backup service.** No daemon, no sidecar, no
  built-in scheduler. Operators wire whatever scheduling they
  already use (`cron`, systemd timers, the operator's NAS
  scheduler).
- **S3 / restic / borg implementation.** No first-party tooling
  for object-storage upload, deduplicated incremental backup,
  or remote rsync target. Pick what your host already runs.
- **Scheduled backups.** No `cron` snippet, no systemd unit, no
  Compose-level scheduling shipped here. (A separate slice MAY
  ship a sample script later; today, you wire it yourself.)
- **HA / multi-instance backup strategy.** v1 is single-instance
  Docker Compose. Multi-instance backup is part of the broader
  multi-instance / HA story called out as POST-V1 in
  [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  §2.
- **Point-in-time recovery (PITR).** No WAL archiving, no
  continuous replication, no PITR tooling. The recovery
  granularity is "the moment of the last `pg_dump`."
- **Managed Postgres provider specifics.** The runbook assumes
  the Compose-managed `postgres:17-alpine` service. Managed
  Postgres providers (RDS, Cloud SQL, Crunchy, etc.) have their
  own backup tooling that supersedes §4; this runbook does NOT
  document those.
- **Encryption tooling selection.** §4.9 references `gpg` as a
  neutral example. The runbook deliberately does NOT pick a
  one-true encryption tool — `age`, `openssl enc`, the
  operator's password manager, filesystem-level LUKS, and
  object-storage server-side encryption are all acceptable.
- **Backup retention / pruning automation.** No first-party
  retention policy. The operator picks how many dumps to keep
  and prunes them by hand or with their existing tooling.
- **Per-chunk recording encryption migration.** Recording chunks
  are renderer-neutral bytes at v1; the SPEC reserves a
  separate recording master key for the future encrypted-chunk
  envelope, but the envelope itself is post-v1. No migration is
  shipped here.
- **Vault master key rotation with automated re-encryption.**
  Per runbook §11.2 the vault master key is set-and-protect at
  v1. Re-encrypting every wrapped SSH private key under a new
  master key is a separate slice.
- **Backup verification automation.** No CI workflow, no
  scheduled rehearsal job. The operator runs §9.2 by hand on
  the cadence they choose.

## 13. Next slices

Ranked by what most moves the needle, given v1 readiness.

1. ~~**`docs/backup-restore-rehearsal-record`**~~ **DONE
   (template) — 2026-05-18.** Landed on
   `docs/backup-restore-rehearsal-record` as
   [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md).
   Rehearsal record template + §10 verification log; the §10
   "first record entry" is the placeholder seed (status NOT
   RUN). The still-pending successor slice is
   **`docs/backup-restore-rehearsal-run`** — operator-walked
   Case R-B restore against a throwaway Postgres, recorded as
   the first dated entry under §10 of the rehearsal record.
   Closes the v1-release-checklist §10 row "Restore-from-backup
   rehearsal" and the v1-production-readiness §4.4 row
   "Restore-test rehearsal" with a dated PASS.
2. **`feat/operational-status-page`** (v1-readiness §7
   honourable mention). A small operations page in Settings
   surfacing healthcheck status, effective quotas, and
   recording on / off. Not v1-blocking; would help future
   restore rehearsals because the post-restore health gate in
   §5.6 becomes a single page instead of three `curl` lines.
3. **`docs/v1-production-smoke-record`** (resolves cutline B2 —
   v1-readiness §7 row 2). Operator-walked production smoke
   entry that copies §5 of
   [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
   into a new dated entry. Becomes available once the operator
   has chosen a production hostname; the backup runbook here is
   a prerequisite, not a successor (the §5 production smoke
   template already cites this runbook from row P).
4. **Optional automation slice (later).** A sample backup
   script (shell or systemd timer) that runs §4 and uploads to
   the operator's storage of choice. Deliberately deferred —
   §12 names this as out-of-scope for v1.

---

## See also

- [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
  — operator-recorded rehearsal log; §5 template + §10
  verification log; the canonical destination for entries that
  close the v1-release-checklist §10 / §12 "Restore-from-backup
  rehearsal" row.
- [`docs/deployment/production-runbook.md`](production-runbook.md)
  — §4 (first deploy), §6 (rollback), §7 (migration), §8
  (backup + restore reminders), §10 (post-deploy smoke), §11
  (secret rotation).
- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  — §5 deployment checks, §10 backup / restore / rollback
  checks, §11 redaction sweep, §12 decision table.
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  — §4.4 (deployment status), §9 (deployment cutline), §7
  (next-slices ranking).
- [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
  — §3 prerequisites, §5 row P (DB backup evidence),
  §5.1–§5.3 redaction sweep query templates.
- [`docs/terminal-recording.md`](../terminal-recording.md) —
  Section 7 (privacy posture), Section 12 (retention).
- [`docs/agent/redaction-rules.md`](../agent/redaction-rules.md)
  — § 11 (terminal recording chunk redaction).
- [`docs/production-auth.md`](../production-auth.md) — §8 (lost
  the only password recovery path).
- [`deploy/docker-compose.production.example.yml`](../../deploy/docker-compose.production.example.yml)
  — production Compose template the procedures here target;
  recommended starting point for new deploys.
- [`deploy/docker-compose.images.example.yml`](../../deploy/docker-compose.images.example.yml)
  — equivalent image-mode Compose reference (minimal comments).
- [`deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
  — env contract the §3 sensitive-material warning rests on.
