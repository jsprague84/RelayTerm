# RelayTerm v1 backup, restore, and rollback rehearsal record

> Operator-recorded rehearsal log for the procedures in
> [`backup-restore-runbook.md`](backup-restore-runbook.md). Each
> entry is one walk of one rehearsal type against a disposable
> non-production target. **Never against production.** Future
> production reliance is honest only after at least one rehearsal
> entry below records PASS or PASS-WITH-CAVEATS.
>
> **Status as of 2026-05-18: TEMPLATE / NOT EXECUTED.** The file
> is the rehearsal record skeleton. No rehearsal has been run.
> The §10 "first record entry" below is the placeholder seed that
> the next slice
> (`docs/backup-restore-rehearsal-run` — see §12) replaces with a
> dated rehearsal walk. Until then, the v1 release-checklist §10
> / §12 row "Restore-from-backup rehearsal" stays **PENDING** and
> the v1-production-readiness §4.4 row "Restore-test rehearsal"
> reads "DONE / runbook + rehearsal template exist; actual
> rehearsal pending."
>
> This file composes existing primitives — it does NOT redefine
> any contract in `AGENTS.md`, `SPEC.md`, `docs/spec/*`,
> [`production-runbook.md`](production-runbook.md), or
> [`backup-restore-runbook.md`](backup-restore-runbook.md). Where
> this doc disagrees with any upstream contract, the upstream
> wins and this doc is the bug.

## 1. Status

- **Document status:** TEMPLATE / NOT EXECUTED.
- **Date created:** 2026-05-18 on
  `docs/backup-restore-rehearsal-record` against the snapshot of
  `main` at commit `c27e328` ("feat(config): harden production
  deploy validation").
- **Intended use.** The operator copies §5 into a new dated
  entry under §10 each time a rehearsal is walked. Entries are
  append-only by date (newest at the top of §10's verification
  log). Do NOT edit a committed entry to make a row PASS
  retroactively — write a follow-up entry instead.
- **Production is not touched by this file.** No entry in this
  file may exercise the production stack as the "restore
  target" or the "rollback target." The rehearsal target is
  always a disposable, operator-owned, non-production Compose
  project. See §4 (safety rules) for the exact contract.
- **Relationship to the v1 cutline.** v1 may ship with this
  template in place AND with the runbook in place AND with at
  least the §10 "first record entry" below seeded NOT RUN. v1
  is not blocked on a recorded rehearsal entry, but the v1
  release notes / sign-off should explicitly acknowledge that
  restore reliance is "documented + templated; not yet
  rehearsed" — see §11 for the exact wording.

## 2. Scope — rehearsal types

This log accepts entries for four rehearsal types, ranked by
incremental coverage. An entry may walk a subset (e.g. backup
verification only) and explicitly skip the rest; mark skipped
rows `SKIPPED` and cite the reason in the entry's caveats
block.

| Type | What it walks | Disposable target required? |
|---|---|---|
| **T1 — Backup-only verification** | Pre-upgrade backup procedure ([`backup-restore-runbook.md`](backup-restore-runbook.md) §4) executed against any RelayTerm deploy the operator owns — staging slot, dev VM, throwaway Compose project. Confirms dump exists, non-zero size, optional SHA-256 recorded, config archive captured, image tag/digest recorded. No restore command runs. | No. May run against any operator-owned deploy. |
| **T2 — Restore into disposable non-production stack** | Full restore-from-backup walk ([`backup-restore-runbook.md`](backup-restore-runbook.md) §5 Case R-B) against a fresh Compose project the operator stands up just for the rehearsal. Confirms `pg_restore` succeeds, migrate container converges (if needed per §5.4 of the runbook), backend reaches healthy, login + inventory + xterm launch all work, redaction sweep returns zero hits. | **Yes.** Target must be a separate Compose project (different `name:`), separate `relayterm-pgdata`-shaped volume, separate `.env`. |
| **T3 — Image rollback rehearsal** | Image-tag rollback ([`backup-restore-runbook.md`](backup-restore-runbook.md) §6.1) against a disposable stack. Confirms `sed` of `RELAYTERM_IMAGE_TAG`, `docker compose pull`, `docker compose up -d --no-deps`, post-rollback healthcheck. Walks the `sha-<short>` fallback path if the operator wants to exercise the registry-retention case. | **Yes.** Same disposability rule as T2. |
| **T4 — Full backup + restore + rollback rehearsal** | T1 + T2 + T3 in sequence against the same disposable stack. The most thorough rehearsal; the closest the operator can get to "I have proven I can recover from a real incident" without touching production. | **Yes.** |

The "production-adjacent" rehearsal type that the slice spec
mentioned — i.e. a rehearsal walked against the production
stack itself — is **deliberately not a row here**. The v1
backup-restore posture is "production is never the restore
target." If a future operator needs to test recovery against
the production blast radius, the right path is to clone the
production volume snapshot into a disposable stack and walk T2
/ T3 / T4 there.

## 3. Preconditions

Before walking any rehearsal entry, the operator must know /
have produced each of the following. Track each row by
checkbox; copy this list into the entry's "Prerequisites
confirmed" block when starting a real walk.

### 3.1 Source deployment (where the backup was taken from)

- [ ] **Source deployment name** (e.g. `relayterm-staging`,
  `relayterm-dev-vm-2026-05`). The Compose project name and the
  Compose-dir path.
- [ ] **Source `compose-dir` path** (matches runbook §4.1;
  example: `/srv/relayterm-staging`).
- [ ] **Source image tag** (the `RELAYTERM_IMAGE_TAG` value the
  source was running, e.g. `vX.Y.Z` or `sha-<short>`) AND the
  resolved digest from `docker compose images`.
- [ ] **Source DB container/service name** (default:
  `postgres`; the `docker compose exec postgres` form in the
  backup runbook §4.4 assumes this).
- [ ] **Source migration version** (final applied migration ID
  per [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §4.3 / runbook §7).

### 3.2 Backup artifacts

- [ ] **Backup output path** (the `pg_dump -Fc` artifact path
  per [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §4.4). NOT inside `<source-compose-dir>` and NOT inside the
  `relayterm-pgdata` volume.
- [ ] **Backup SHA-256** if recorded
  ([`backup-restore-runbook.md`](backup-restore-runbook.md)
  §4.6).
- [ ] **Config archive path** (`.env` + `docker-compose.yml`
  ±  reverse-proxy config) per
  [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §4.7. `chmod 600` preserved on `.env`.

### 3.3 Restore target (disposable, non-production)

- [ ] **Restore target host** chosen — laptop VM, throwaway
  cloud instance, separate homelab box. **MUST be disposable.**
  Never the production deploy host.
- [ ] **Restore target `compose-dir` path** chosen and prepared
  (empty, or holds a freshly-cloned `docker-compose.images.example.yml`
  + a freshly-cloned `relayterm.env.example`).
- [ ] **Restore target Compose project name** chosen — distinct
  from any production / staging project the operator runs (e.g.
  `relayterm-rehearsal-2026-05-18`). Avoid collisions with
  existing project names so `docker compose ps` cannot ambiguate
  the wrong stack.
- [ ] **Restore target isolation confirmed.** Target's
  `relayterm-pgdata` volume is a separate Docker named volume.
  Target's `.env` is a separate file. Target's outer reverse
  proxy (if any) does NOT terminate the production hostname.
- [ ] **Restore target hostname / origin posture decided.** The
  rehearsal stack is typically loopback-only
  (`http://127.0.0.1:<port>`); a public origin is only required
  if the rehearsal exercises a `cookie_secure=true` posture, in
  which case use a throwaway hostname the operator owns.

### 3.4 Vault / config / env material

- [ ] **Vault master key for the restored stack decided.** The
  cleanest rehearsal posture is "restore is `Case R-B` against
  the SAME `.env` so the vault master key matches the dump's
  encrypted private keys." If the operator wants to rehearse a
  key-rotation path, that is a separate slice — out of scope
  here. For T2/T3/T4 here, the rehearsal stack's `.env` is the
  byte-equality clone of the source `.env` per
  [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §4.7 + §5.5.
- [ ] **Production secrets NOT cross-pollinated.** If the
  source is a production deploy and the rehearsal target is a
  lab box, the rehearsal `.env` MUST stay on storage the
  operator already treats as production-secret-grade. The
  rehearsal does NOT relax §3 of
  [`backup-restore-runbook.md`](backup-restore-runbook.md) —
  the same "vault key + DB dump = full disclosure of every SSH
  identity" rule applies to the rehearsal target.

### 3.5 Rollback target (only required for T3 / T4)

- [ ] **Previous known-good image tag** for the rehearsal
  target identified (the `RELAYTERM_IMAGE_TAG` the rehearsal
  starts at, e.g. `vX.Y.(Z-1)` or a `sha-<short>`).
- [ ] **Rollback digest recorded** in case the previous tag has
  been cleaned up by the registry (use `docker pull
  <repo>@sha256:<digest>` per
  [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §6.1 last paragraph).

### 3.6 Operator confirmation

- [ ] **Operator has explicitly confirmed the destructive target
  is the rehearsal stack and not production.** The runbook §5
  banner ("Destructive warning") and §6.2 banner apply with full
  force to the rehearsal too.
- [ ] **Rehearsal window** chosen — a contiguous time window
  during which no other operator work happens on the rehearsal
  target, so the log / audit sweeps in §9 are bounded.

## 4. Safety rules — destructive command discipline

Read this section before running any rehearsal. Each rule maps
to a load-bearing AGENTS.md "Things to avoid" row or to the
backup-restore-runbook §3 / §5 banners.

- **Never restore into production without explicit operator
  approval AND a recorded reason.** The default rehearsal target
  is disposable. Restoring into production is the recovery path
  in [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §5 Case R-A / R-C — that procedure is invoked from a real
  incident, not from a rehearsal. A rehearsal that ends up
  pointing at the production stack is no longer a rehearsal; it
  is the §5 procedure and must be classified that way.
- **Never commit DB dumps, `.env`, vault keys, recording keys,
  bootstrap tokens, signing keys, or screenshots containing any
  of the above to this repo.** The repo's `.gitignore` already
  covers `.env`. Dumps and config archives belong on off-host
  storage per
  [`backup-restore-runbook.md`](backup-restore-runbook.md) §3.
  This rehearsal log records **paths / object keys / sentinels /
  outcomes only** — never the secret material itself.
- **Use a disposable restore target first; only ever the
  disposable one.** §2 T2 / T3 / T4 require this. A rehearsal
  entry recorded against the production host is a contract
  violation, not a rehearsal.
- **Confirm target hostname / Compose project / `compose-dir`
  path BEFORE running any destructive command.** Echo the
  target's project / dir / hostname into the entry's
  "Prerequisites confirmed" block before the first `docker
  compose stop`, `psql DROP DATABASE`, or `pg_restore` line. The
  cost is one line; the upside is preventing the "I was in the
  wrong terminal" mode of failure.
- **Preserve pre-rehearsal state on the rehearsal target when
  useful.** If the rehearsal target carries operator-meaningful
  state (say, a partly-finished dev experiment), take a fresh
  `pg_dump -Fc` of THAT target before invoking
  [`backup-restore-runbook.md`](backup-restore-runbook.md) §5.1.0
  on it. The rehearsal's destructive step is no less destructive
  for being a rehearsal — `DROP DATABASE` against the wrong
  rehearsal target still loses data.
- **Do NOT manually edit migration rows in the rehearsal
  target's `_sqlx_migrations` table.** The supported reconcile
  path is [`backup-restore-runbook.md`](backup-restore-runbook.md)
  §5.4 (run the migrate container if the restored DB is older
  than the deployed image; pin `RELAYTERM_IMAGE_TAG` to the
  schema-matching build if the restored DB is newer). Manual
  row surgery is not a supported rehearsal path.
- **Do NOT delete `audit_events`, `session_events`,
  `terminal_sessions`, `terminal_session_attachments`,
  `terminal_recording_chunks`, `terminal_recording_markers`, or
  `known_host_entries` rows manually as part of a rehearsal.**
  The supported lifecycle paths
  ([`docs/spec/inventory.md`](../spec/inventory.md) "Inventory
  lifecycle and destructive-action policy",
  [`docs/spec/recording.md`](../spec/recording.md) retention)
  govern these tables. The rehearsal observes; it does not
  hand-edit history.
- **Suppress shell history when secrets are on the command
  line.** `set +o history` (bash/zsh) / `set fish_history ""`
  (fish) before any command that reads or writes a secret. The
  auth-smoke runbook ([`docs/auth-smoke.md`](../auth-smoke.md)
  "Prerequisites") has the per-shell pattern.
- **Stop and ask before any destructive command if the
  rehearsal entry was written by an agent, not directly by the
  operator.** Agents drafting an entry into this log MUST NOT
  invoke `pg_restore`, `DROP DATABASE`, `docker compose stop`
  against the production stack, or `sed -i` on a production
  `.env` without an explicit, in-conversation operator approval
  that names the exact target. The default agent posture is
  "produce the entry template; let the operator run the
  destructive lines themselves."

## 5. Rehearsal record template

Copy the entire block below into a new dated section under §10
when walking a rehearsal. Do not delete the inline `<!-- … -->`
comments until the row is filled in or explicitly skipped. The
template intentionally mirrors the
[`backup-restore-runbook.md`](backup-restore-runbook.md) §10
short-form template so an operator who has only read the
runbook recognises the shape.

```markdown
### YYYY-MM-DD · Backup-restore rehearsal — <T1 | T2 | T3 | T4>

> **Status: NOT EXECUTED.** Replace with `Status: PASS` /
> `Status: PASS-WITH-CAVEATS` / `Status: FAIL` once every
> applicable row is filled.

**Rehearsal type.** <T1 backup-only | T2 restore into
disposable | T3 rollback rehearsal | T4 full backup + restore +
rollback>

**Source deployment.**
- Name / Compose project : `<name>`
- Compose-dir path       : `<path>`
- Image tag deployed     : `<vX.Y.Z>`   digest `sha256:<…>`
- Migration version      : `<id>`
- Source config backup   : `<off-host path / object key>`

**Backup artifacts.**
- DB dump path           : `<off-host path / object key>`
- Dump size (bytes)      : `<N>`
- Dump SHA-256           : `<hex or "not recorded">`
- Backup taken at        : `YYYY-MM-DDThh:mm:ssZ`
- Backup procedure       : `backup-restore-runbook.md` §4

**Restore target (T2 / T3 / T4 only).**
- Target host            : `<hostname; MUST be disposable / non-production>`
- Target Compose project : `<name>`
- Target compose-dir     : `<path>`
- Target volume name     : `<docker named volume; distinct from source>`
- Target `.env` source   : `<byte-equality clone of source .env | freshly generated for rotation rehearsal>`
- Isolation statement    : "Target host / project / volume / .env are
                            distinct from any production or staging
                            deploy the operator runs. Operator has
                            confirmed the destructive commands point at
                            this target and not at production." <!-- The
                            operator records this verbatim or with a
                            slightly tightened wording; do NOT omit. -->

**Restore commands recorded (T2 / T3 / T4 only).**
- App-stop command       : `docker compose stop relayterm-backend relayterm-web`
- DB drop / recreate     : per runbook §5.3 (DROP / CREATE / pg_restore)
- Migrate reconcile      : `<ran | not needed | pinned RELAYTERM_IMAGE_TAG>`
                           (record which §5.4 branch applied)
- App-start command      : `docker compose up -d postgres relayterm-backend relayterm-web`

**Rollback commands recorded (T3 / T4 only).**
- Previous tag selected  : `<vX.Y.(Z-1)>`   digest `sha256:<…>`
- Current tag recorded   : `<vX.Y.Z>`   digest `sha256:<…>`
- Rollback command       : `sed -i 's/^RELAYTERM_IMAGE_TAG=.*/RELAYTERM_IMAGE_TAG=<previous-tag>/' .env`
                           then `docker compose pull` then
                           `docker compose up -d --no-deps relayterm-backend relayterm-web`
- Migration caveat       : `<backward-compatible — image rollback only |
                            backward-incompatible — restore-from-backup path took
                            place per runbook §6.2>`

**Health checks (T2 / T3 / T4 only).**
- `docker compose ps`    : postgres / backend / web all `(healthy)` — `<PASS | FAIL>`
- `curl -sf http://127.0.0.1:<port>/_web_health` → `ok` — `<PASS | FAIL>`
- `curl -sf http://127.0.0.1:<port>/healthz` → `{"status":"ok"}` — `<PASS | FAIL>`
- `curl -i http://127.0.0.1:<port>/api/v1/auth/me` unauthenticated → `401` — `<PASS | FAIL>`

**Sanity walk against restored data (T2 / T3 / T4 only).**
- Login check            : SPA login with operator credentials → `<PASS | FAIL>`
- Sessions list          : `SessionsView.svelte` loads — `<PASS | FAIL>`
- Inventory check        : `IdentitiesView` + `ServersView` load with the rows
                           the dump should carry — `<PASS | FAIL>`
- Identity-detail redaction : No `private_key`, no `encrypted_private_key`, no
                              raw PEM, no `BEGIN OPENSSH PRIVATE KEY` substring
                              anywhere on the page — `<PASS | FAIL>`
- xterm launch check     : Launch against a trusted, auth-checked profile;
                           `whoami` returns the expected user — `<PASS | FAIL>`
                           (exercises the vault master key end-to-end)

**Redaction / log sweep (every type that produces logs).**
- Backend log sweep      : §9 grep over the rehearsal window
                           → `ok: no leakage sentinels found` — `<PASS | FAIL>`
- Web (nginx) log sweep  : §9 grep over the rehearsal window
                           → `ok: no leakage sentinels found` — `<PASS | FAIL>`
- Audit payload sweep    : §9 SQL over the rehearsal window
                           → `count = 0` — `<PASS | FAIL>`

**Result.** PASS / PASS-WITH-CAVEATS / FAIL / NOT RUN.

**Caveats / notes.** <Free text — what was unexpected, what
needed manual intervention, any sentinel hits, any timing
surprises, any row marked SKIPPED with the reason.>

**Operator.** `<name / handle>`

**Follow-up actions.** <Each follow-up that the rehearsal
surfaced; ranked. Examples: "tighten runbook §5.3 wording on
the pg_restore --clean flag," "add a §4 sentinel for the new
'XYZ' substring," "schedule next rehearsal at YYYY-MM-DD."
None is a valid value.>

**Next rehearsal due (optional).** YYYY-MM-DD.
```

## 6. Backup verification checklist (T1, T2, T3, T4)

Walk this before declaring a backup acceptable evidence for a
later restore. Maps to [`backup-restore-runbook.md`](backup-restore-runbook.md)
§4 and to [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
§10.

- [ ] Dump file exists at the recorded path (`ls -lah`
  confirms).
- [ ] Dump file size is non-zero and matches the rough operator
  expectation (zero / suspiciously small = STOP, investigate
  per runbook §4.5).
- [ ] Dump SHA-256 recorded (optional per runbook §4.6;
  recommended for any backup that will travel).
- [ ] Config archive (`.env` + Compose file + reverse-proxy
  config if separate) exists at the recorded path with
  `chmod 600` preserved on `.env` (runbook §4.7).
- [ ] Image tag AND digest recorded for `relayterm-backend`,
  `relayterm-web`, and `relayterm-backend-migrate` (runbook
  §4.2).
- [ ] Migration version recorded (runbook §4.3).
- [ ] Vault master key + (when wired) recording master key
  storage strategy reviewed against runbook §3. Keys are NOT
  committed to this repo and NOT in the same single failure
  domain as the DB dump if the operator's threat model demands
  separation.
- [ ] Backup location recorded **outside** the repo (off-host
  path, object-storage URI, or operator-controlled vault). The
  rehearsal entry records the path, NOT the artifact.

## 7. Restore rehearsal checklist (T2, T4)

Walk this against the disposable restore target. Maps to
[`backup-restore-runbook.md`](backup-restore-runbook.md) §5
(specifically Case R-B for rehearsal use) and to §9.2 of the
runbook.

- [ ] Restore target is disposable (§3.3 confirmed; isolation
  statement recorded in the entry's §5 template block).
- [ ] App containers (`relayterm-backend`, `relayterm-web`)
  stopped before restore (runbook §5.1).
- [ ] Pre-restore dump of the target's current DB taken (runbook
  §5.1.0) — skip ONLY if the target is empty / never used.
- [ ] DB restore command recorded verbatim in the entry
  (runbook §5.3 DROP / CREATE / `pg_restore --no-owner --clean
  --if-exists` form is the v1 default).
- [ ] Migration reconcile path recorded (runbook §5.4 — "same
  version", "restored is older — ran migrate container", or
  "restored is newer — pinned `RELAYTERM_IMAGE_TAG`").
- [ ] App stack started; `docker compose ps` reaches healthy on
  all three services (runbook §5.5).
- [ ] `/healthz` and `/_web_health` return OK from loopback
  (runbook §5.6).
- [ ] `/api/v1/auth/me` returns `401` unauthenticated (runbook
  §5.6).
- [ ] SPA login with the dump's operator credentials succeeds
  (runbook §5.6 — note the "pre-dump password is what works
  post-restore" caveat).
- [ ] Sessions list loads.
- [ ] Inventory loads (`IdentitiesView`, `ServersView`).
- [ ] Identity detail panel shows public-key metadata +
  SHA-256 fingerprint only — NO `private_key`, NO
  `encrypted_private_key`, NO raw PEM, NO `BEGIN OPENSSH
  PRIVATE KEY` substring anywhere.
- [ ] xterm launch against a trusted, auth-checked profile
  works; `whoami` returns the expected user (this exercises
  the vault master key end-to-end — if it is wrong, the launch
  fails with a clean error even though every other route
  works).
- [ ] Redaction sweep per §9 returns `ok: no leakage sentinels
  found` over the rehearsal window's backend AND web logs.

## 8. Rollback rehearsal checklist (T3, T4)

Walk this against the disposable rollback target. Maps to
[`backup-restore-runbook.md`](backup-restore-runbook.md) §6.

- [ ] Previous image tag selected and recorded (`vX.Y.(Z-1)` or
  a `sha-<short>`; digest also recorded — runbook §6.1).
- [ ] Current image tag recorded (the tag the rehearsal target
  is rolling BACK from).
- [ ] Rollback command recorded verbatim — `sed -i` of
  `RELAYTERM_IMAGE_TAG` in `.env`, `docker compose pull`,
  `docker compose up -d --no-deps relayterm-backend
  relayterm-web` (runbook §6.1).
- [ ] Migration-compatibility caveat stated explicitly in the
  entry: either "backward-compatible schema — image rollback
  alone suffices" OR "backward-incompatible — exercised the
  restore-from-backup path per runbook §6.2 BEFORE the image
  rollback." A rehearsal that does NOT name this caveat is
  incomplete.
- [ ] App starts after the rollback; `(healthy)` on all three
  services within the operator's expected window. If the
  rollback exercised the §6.2 path, the §7 checklist above also
  ran.
- [ ] Post-rollback healthcheck + redaction sweep recorded (§9
  grep set; mirrors the §M / §M' / §O rows in
  [`v1-production-smoke.md`](v1-production-smoke.md) §5).
- [ ] Rollback result recorded as `PASS` / `PASS-WITH-CAVEATS`
  / `FAIL` in the entry's overall `**Result.**` line at the
  bottom of the §5 template block. (T3-only walks may skip
  the §5 "Restore commands recorded" and "Sanity walk against
  restored data" subsections, but the bottom-line `**Result.**`
  applies to T1/T2/T3/T4 alike.)

## 9. Redaction sweep block

Every rehearsal entry that produces logs (T2, T3, T4) runs the
sentinel sweep below over the rehearsal window. T1 (backup-only)
need not run the sweep unless the operator wants to confirm the
backup procedure itself wrote nothing to logs.

### 9.1 Sentinel set

The sentinel set is intentionally byte-aligned with
[`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
§5.1 (the canonical v1 superset). If a new sentinel is added to
`AUDIT_FORBIDDEN_SUBSTRINGS` in `crates/relayterm-api/tests/api.rs`
or to the v1-production-smoke §5.1 block, mirror it here too.

```
# Mirrored from AUDIT_FORBIDDEN_SUBSTRINGS (must stay in sync;
# if a new sentinel is added to that constant, add it here too):
private_key
encrypted_private_key
BEGIN OPENSSH PRIVATE KEY
password_hash
session_token
token_hash
bootstrap_token
argon2id
client_info
remote_addr
user_agent

# Additional sentinels covered by staging / release-checklist /
# runbook sweeps (private-key body / passphrase / recording
# chunk / cookie value / explicit redaction marker):
openssh-key-v1
passphrase
data_b64
REDACT-MARKER
relayterm_session=[A-Za-z0-9_-]{20,}
```

`private_key_openssh` is covered by the broader `private_key`
prefix above — the sentinel set is a superset, not a
hyper-specific token list.

**Do NOT include bare `password` in the grep set.** The sshd
line `"User/password ssh access is disabled"` produces a known
false positive on a throwaway target (this is the same
false-positive call-out as v1-production-smoke §5.2); the
actual forbidden token is `password_hash`, which is already in
the set above. Including bare `password` would block every
rehearsal that touched any sshd-capable target.

### 9.2 Sweep query templates

Backend log (substitute `<window>` for the rehearsal window;
the form mirrors v1-production-smoke §5.3 so an operator copying
both has byte-identical greps):

```sh
docker compose logs --since <window> relayterm-backend | \
  grep -E 'private_key|encrypted_private_key|BEGIN OPENSSH PRIVATE KEY|password_hash|session_token|token_hash|bootstrap_token|argon2id|client_info|remote_addr|user_agent|openssh-key-v1|passphrase|data_b64|REDACT-MARKER|relayterm_session=[A-Za-z0-9_-]{20,}' || \
  echo "ok: no leakage sentinels found"
```

Nginx web log:

```sh
docker compose logs --since <window> relayterm-web | \
  grep -E 'private_key|encrypted_private_key|BEGIN OPENSSH PRIVATE KEY|password_hash|session_token|token_hash|bootstrap_token|argon2id|client_info|remote_addr|user_agent|openssh-key-v1|passphrase|data_b64|REDACT-MARKER|relayterm_session=[A-Za-z0-9_-]{20,}' || \
  echo "ok: no leakage sentinels found"
```

Audit payload sweep:

```sh
docker compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c \
  "SELECT count(*) FROM audit_events WHERE created_at > now() - interval '<window>' AND (payload::text ~ 'private_key|encrypted_private_key|BEGIN OPENSSH|password_hash|session_token|token_hash|bootstrap_token|argon2id|client_info|remote_addr|user_agent|openssh-key-v1|passphrase|data_b64|REDACT-MARKER');"
```

Expected: `count = 0`. Any row is a leak — the rehearsal CANNOT
record PASS until the leak is investigated and the followup is
captured as either an entry-level FAIL or as a recorded
follow-up action in the entry's §5 template block.

### 9.3 Known false positives (do NOT flag)

- `missing session cookie` — diagnostic label naming the
  ABSENCE of a value (same call-out as v1-production-smoke
  §5.2 and runbook §10).
- `User/password ssh access is disabled` — sshd policy line on
  a throwaway SSH target (same call-out as v1-production-smoke
  §5.2).

## 10. Verification log

> Append entries in reverse chronological order (newest first).
> Each entry uses the §5 template verbatim. **Do not edit a
> committed entry to make a row PASS retroactively** — write a
> follow-up entry instead.

### 2026-05-18 · Backup-restore rehearsal — TEMPLATE SEED (not executed)

> **Status: NOT RUN.** This entry is the placeholder seed that
> ships with the template. No rehearsal was walked. No
> production stack was touched. No disposable rehearsal stack
> was stood up. The §5 template above is the deliverable; this
> entry exists so the §10 verification log is non-empty from
> day one and so the v1 release-checklist §10 row "Restore-
> from-backup rehearsal" has a concrete file to point at.

**Rehearsal type.** None (template seed; no walk).

**Why no walk happened.** Per the slice contract: this slice
ships docs only. The operator has not yet chosen a disposable
rehearsal target (T2/T3/T4 require one per §3.3), and the
slice deliberately does NOT spin one up on the operator's
behalf. The first real entry above this one is the next
slice's deliverable (`docs/backup-restore-rehearsal-run` — see
§12).

**What this slice DID change** (docs-only):
- `docs/deployment/backup-restore-rehearsal-record.md` —
  created; contains §1 status, §2 scope (T1–T4), §3
  preconditions, §4 safety rules, §5 rehearsal record
  template, §6 backup verification checklist, §7 restore
  rehearsal checklist, §8 rollback rehearsal checklist, §9
  redaction sweep block, §10 verification log (this entry),
  §11 relationship to v1, §12 next slices.
- [`docs/deployment/backup-restore-runbook.md`](backup-restore-runbook.md)
  — §10 cross-reference updated to point at this file as the
  canonical rehearsal log; §11 v1-readiness wording updated to
  match.
- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  — §10 / §12 / §14 rehearsal-record pointers updated to point
  at this file. The "Restore-from-backup rehearsal" row stays
  PENDING (a template is not evidence).
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  — §4.4 "Restore-test rehearsal" row updated to read "DONE /
  runbook + rehearsal template exist; actual rehearsal
  pending" with a pointer to this file. §7 honourable mention
  for `docs/backup-restore-rehearsal-record` resolved as
  "DONE — template" with the still-pending operator-walked
  successor named.
- [`docs/deployment/v1-production-smoke.md`](v1-production-smoke.md)
  — §3 prerequisites and §5 row P pointer updated to cite this
  file as the rehearsal record the "rehearsal status" line
  refers to.

**What this slice did NOT change** (load-bearing):
- No source / schema / migration / route / CSP / deploy / CI /
  orchestrator / protocol / renderer code.
- No production stack: zero `docker pull`, zero `docker
  compose` calls against any production or staging host, zero
  HTTP / WS / SSH connection to any production origin.
- No disposable rehearsal stack was stood up; no `pg_dump`,
  `pg_restore`, `psql DROP DATABASE`, or `sed -i` of
  `RELAYTERM_IMAGE_TAG` ran as part of this slice.
- No staging-smoke entry was added or edited; the staging log
  at [`vps-staging-smoke.md`](vps-staging-smoke.md) is
  untouched.
- No experimental renderer was promoted; the
  `experimentalRendererEvaluationEnabled` gate default is
  unchanged; xterm remains the production default.
- No B2 / B3 status was recorded — B2 / B3 in the cutline +
  release-checklist remain **PENDING**, untouched by this
  slice.

**Result.** NOT RUN.

**Caveats / notes.** Production target / disposable restore
target not yet selected; rehearsal deferred to the next slice
named in §12.

**Operator.** (template seed; no walker).

**Follow-up actions.**
- Choose a disposable rehearsal target (laptop VM, throwaway
  cloud instance, separate homelab box) and walk a T2
  rehearsal against the most recent staging backup. T2 alone
  closes the v1-readiness §4.4 "Restore-test rehearsal" row;
  T3 / T4 are useful but not v1-cutting.
- After the first T2 PASS records here, update the
  v1-release-checklist §12 row "Restore-from-backup
  rehearsal" from PENDING to PASS and cite the dated entry.

**Next rehearsal due.** Recommended cadence: one rehearsal
before the first production deploy
([`backup-restore-runbook.md`](backup-restore-runbook.md) §10
restates this); one per quarter thereafter. The first dated
entry above this one is the slice that picks the target.

## 11. Relationship to v1

- **Runbook exists.** Landed 2026-05-17 on
  `docs/backup-restore-runbook` —
  [`backup-restore-runbook.md`](backup-restore-runbook.md).
- **Rehearsal record template exists.** Landed 2026-05-18 on
  `docs/backup-restore-rehearsal-record` — this file.
- **Actual restore rehearsal is RECOMMENDED before relying on
  production**, not REQUIRED for the v1 ship gate. v1 ships
  with "documented + templated; not yet rehearsed" being an
  honest description of the restore posture. The
  v1-release-checklist §10 / §12 row "Restore-from-backup
  rehearsal" stays PENDING until the first dated entry under
  §10 above records PASS or PASS-WITH-CAVEATS; the
  v1-production-readiness §4.4 row "Restore-test rehearsal"
  reads "DONE / runbook + rehearsal template exist; actual
  rehearsal pending."
- **Not a blocker to continued implementation.** The cutline
  blockers are B2 (production smoke) and B3 (mobile portrait
  sanity); the rehearsal is an honourable mention per
  v1-production-readiness §7. The operator may decide to elevate
  the rehearsal to "required before first production deploy" at
  any time; doing so is a release-day judgement, not a code or
  doc change.
- **The first production deploy SHOULD invoke a rehearsal as a
  pre-flight.** That is the cheapest moment to discover a
  bad-restore-procedure bug — before relying on it for real
  data. Wording for the release notes: "Restore-from-backup
  rehearsal: template at
  [`docs/deployment/backup-restore-rehearsal-record.md`](deployment/backup-restore-rehearsal-record.md);
  first dated entry pending; recommended before relying on
  production." This wording avoids overclaiming.

## 12. Next slices

Ranked by what most moves the needle, given v1 readiness and
this slice's docs-only deliverable.

1. **`docs/backup-restore-rehearsal-run`.** Operator-walked T2
   rehearsal against a disposable Compose project. Copies §5
   into a new dated entry above the §10 template seed and
   walks the §7 restore rehearsal checklist + §9 redaction
   sweep end-to-end. Closes the v1-release-checklist §10 /
   §12 row "Restore-from-backup rehearsal" with a dated PASS.
   This is the slice the v1-readiness §7 honourable mention
   "`docs/backup-restore-rehearsal-record`" was originally
   blocked on; with this template landed, the run slice has
   the structure it needs.
2. **`feat/v1-operational-status-page` enhancements** (only if
   future rehearsals surface a gap the panel could cover).
   The Operational Status panel already surfaces
   backend reachability, session counts, quotas, and renderer
   posture; it does NOT surface "last backup at" or "last
   restore rehearsal at." A future enhancement could add a
   read-only line linking to this file's §10 most-recent
   entry — only worth doing if the operator finds themselves
   asking "when did I last rehearse?" frequently. Not v1-
   blocking, not part of this slice.
3. **`docs/v1-production-smoke-record`** (resolves cutline B2 —
   v1-readiness §7 row 2). Independent of this rehearsal slice
   but cross-references row P (DB backup evidence) which now
   cites this file as the rehearsal log. Becomes available
   once the operator has chosen a production hostname; the
   template skeleton at
   [`v1-production-smoke.md`](v1-production-smoke.md) §5 is
   what that slice's successor copies and walks.

Honourable mentions:

- **Quarterly rehearsal cadence reminder.** Operator-side; not
  a code or doc slice. The §10 verification log here is the
  canonical place to discover "we are overdue" by reading the
  newest entry's "Next rehearsal due" line.
- **Cross-stack rehearsal — restore from a recording-ENABLED
  backup.** Only when the operator has opted recording IN per
  [`docs/terminal-recording.md`](../terminal-recording.md) and
  has a recording-bearing dump to test against. The chunk
  bytes never leave the dump per
  [`docs/agent/redaction-rules.md`](../agent/redaction-rules.md)
  § 11; this rehearsal would confirm that the §9 redaction
  sweep stays clean even on a recording-bearing restore. Not a
  separate file — fits as a T2 entry in §10 with an explicit
  "recording was enabled in the source dump" caveat.

Deliberately NOT recommended as next slices:

- Any renderer-promotion or experimental-renderer matrix
  rehearsal. Renderer evaluation is post-v1; rehearsal lives
  in the deployment lane.
- Automated rehearsal CI. Runbook §12 explicitly defers backup
  verification automation; the same applies to rehearsal
  automation. The operator runs §6 / §7 / §8 by hand on the
  cadence they choose.
- Cross-host / multi-instance rehearsal. v1 is single-instance
  Docker Compose; multi-instance rehearsal belongs to the
  post-v1 HA story (runbook §12 non-goals).

---

## See also

- [`backup-restore-runbook.md`](backup-restore-runbook.md) —
  the operator-facing backup / restore / rollback procedure
  this rehearsal log exercises. §4 (pre-upgrade backup), §5
  (restore — specifically Case R-B for rehearsal), §6
  (rollback), §10 (rehearsal record short-form template the
  §5 block here extends).
- [`production-runbook.md`](production-runbook.md) — load-
  bearing operator runbook §4 (first deploy), §6 (rollback),
  §7 (migration), §8 (backup + restore reminders), §10 (post-
  deploy smoke), §11 (secret rotation).
- [`v1-production-smoke.md`](v1-production-smoke.md) — v1
  production smoke log; §3 prerequisites cite this file as the
  rehearsal record the "rehearsal status" line refers to;
  §5.1–§5.3 sentinel set + sweep query templates are the
  canonical v1 sentinel superset this file's §9 mirrors.
- [`vps-staging-smoke.md`](vps-staging-smoke.md) — staging
  smoke log; the most recent staging-backup-bearing entry is
  the natural source backup for a first T2 rehearsal.
- [`../v1-release-checklist.md`](../v1-release-checklist.md) —
  §10 backup / restore / rollback checks; §12 decision-table
  "Restore-from-backup rehearsal" row pointed at this file;
  §14 next-slices.
- [`../v1-production-readiness.md`](../v1-production-readiness.md)
  — §4.4 "Restore-test rehearsal" row; §7 honourable mention
  for this slice.
- [`../v1-release-notes.md`](../v1-release-notes.md) — v1
  release-notes draft; the restore posture wording in §11
  above is what the release notes restate for the user.
- [`../terminal-recording.md`](../terminal-recording.md) —
  off-by-default recording posture; cited by §12 honourable-
  mention "recording-ENABLED backup" rehearsal type.
- [`../agent/redaction-rules.md`](../agent/redaction-rules.md)
  — §§ 1, 4, 5, 11, 12 are the load-bearing redaction rules
  the §9 sentinel set enforces.
- [`../spec/inventory.md`](../spec/inventory.md) — inventory
  lifecycle and destructive-action policy referenced by §4
  (manual deletes forbidden in rehearsal).
- [`../../deploy/relayterm.env.example`](../../deploy/relayterm.env.example)
  — env contract the §3.4 vault / config / env material
  prerequisites rest on.
- [`../../deploy/docker-compose.images.example.yml`](../../deploy/docker-compose.images.example.yml)
  — image-mode Compose stack the §3 / §7 restore-target
  procedure assumes.
