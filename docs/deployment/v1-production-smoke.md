# RelayTerm v1 production smoke log

> Operator-recorded production-walked end-to-end smoke entries
> against real production hostnames. **NOT the staging slot.**
> The throwaway staging slot at `relayterm-staging.js-node.cc`
> has its own log at
> [`vps-staging-smoke.md`](vps-staging-smoke.md) — do not
> co-mingle entries; do not cite a staging row as production
> evidence.
>
> **Status as of 2026-05-17:** the file is the template skeleton
> for the v1 production smoke that the release cutline
> ([`docs/v1-production-readiness.md`](../v1-production-readiness.md)
> blocker B2) gates the v1 ship decision on. **No production
> walk has been recorded yet.** The first concrete entry below
> is the template the operator copies and fills in on the day
> they actually walk the smoke against a real production
> hostname. Until then B2 remains **PENDING** in the cutline and
> the release-checklist §12 decision table.

## 1. Purpose

This log is the operator-recorded evidence track for v1
production smokes. Each entry is one walk of the v1 release
gate against a real production hostname, recorded the same day,
committed to the repo.

It composes:

- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  §§3–11 (the operator-facing release-day gate).
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  §9 (the v1 cutline deployment punch list).
- [`docs/deployment/production-runbook.md`](production-runbook.md)
  §4 (first deploy), §10 (post-deploy smoke), §11 (secret
  rotation).
- [`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md) §B
  (production smoke), §C (auth flow smoke).

This file does NOT redefine any contract in
`AGENTS.md`, `SPEC.md`, `docs/spec/*`, or the runbook. Where it
disagrees with any upstream contract, the upstream wins and the
entry here is the bug.

## 2. Posture (load-bearing — repeat in every entry)

State these out loud in every entry; they pin operator
expectations and prevent scope creep.

- **Production hostname ≠ staging hostname.** Entries here
  exercise a real production deploy the operator owns. The
  throwaway staging slot is for the staging log only.
- **xterm.js is the v1 production default on every surface.**
  No entry here flips the production renderer default. The
  experimental renderers (`ghostty-web`, `wterm`, `restty`)
  reach the production shell ONLY through the gated lazy loader
  at `apps/web/src/lib/app/terminal/rendererLoader.ts`, AND
  ONLY when the operator has flipped the
  `experimentalRendererEvaluationEnabled` gate in Settings.
- **The experimental renderer gate stays OFF for v1 production
  smokes.** Any entry that flips the gate on a production deploy
  for evaluation must say so explicitly and must restore the
  gate to off before the entry is recorded as PASS.
- **No production CSP widening.** The staging-only
  `'wasm-unsafe-eval'` CSP relaxation does NOT migrate to
  production. The production CSP keeps the strict
  `default-src 'self'` posture xterm relies on.
- **No source / schema / migration / route / CSP / deploy / CI /
  orchestrator / protocol / renderer change** lands as part of a
  smoke entry here. Smoke entries are docs-only. Any
  implementation gap surfaced during a smoke is recorded as a
  follow-up slice, not patched in-place.
- **Sentinel-string redaction is non-negotiable.** Every entry
  ends with a redaction sweep over backend log, web/nginx log,
  and `audit_events.payload` for the canonical sentinel list
  (§ 5.3). One hit blocks the smoke entry from PASS.
- **Operator credentials never cross any tool argv.** Passwords
  and session-cookie values stay operator-held; the
  `relayterm_session` cookie is `HttpOnly` and must never be
  read by the smoke. The bootstrap token (if used) must be
  unset and the backend restarted before the entry is recorded.

## 3. Required prerequisites before walking the first production smoke

The operator must have decided / produced each item below before
running the v1 production smoke. Some are answered by
[`docs/v1-release-checklist.md`](../v1-release-checklist.md)
§§3–4; this list is the consolidated "without these, the walk
cannot start" prerequisites view.

Track each row by checkbox; copy into the entry's "Prerequisites
confirmed" block when starting a real walk.

- [ ] **Production hostname** chosen (e.g. `https://relay.example.com`).
- [ ] **Production deploy host** chosen (the box where Compose
  runs) and reachable from the operator's workstation (SSH or
  console).
- [ ] **Compose / project path on the deploy host** known
  (matches runbook §4.1).
- [ ] **Image tag/digest** decided (`vX.Y.Z` AND `sha-<short>`
  for both `relayterm-backend` and `relayterm-web`, plus
  `relayterm-backend-migrate`). `:main` and unpinned `:latest`
  are NEVER the v1 release tag.
- [ ] **Migration status** verified: migration container/run
  completed; final applied migration ID recorded.
- [ ] **Recording posture** decided for v1. Default and
  recommended is OFF; if the operator flips it ON, they must
  re-read [`docs/terminal-recording.md`](../terminal-recording.md)
  end-to-end first (release-checklist §4 last bullet).
- [ ] **Deployment posture** decided: public-internet OR
  VPN-only / WireGuard-only. Either is supported; the choice
  drives which entry rows are applicable (e.g. public-internet
  CSRF/Origin negative checks).
- [ ] **Identity posture** decided: generate-only is acceptable
  for v1; if private-key import is in scope, restrict to
  Ed25519 unencrypted OpenSSH per
  [`docs/private-key-import.md`](../private-key-import.md).
  **Do not import a personal / production-critical private key
  for the purpose of a smoke** — use a throwaway identity.
- [ ] **Safe SSH target strategy** decided. Options, ranked by
  preference:
  1. A new throwaway internal-only SSH target reachable from the
     production backend (preferred — no blast radius).
  2. A production-safe SSH target the operator already owns,
     used read-only (`whoami` / `pwd` only — no destructive
     commands).
  3. A VPN / internal-network-only target if the deployment is
     VPN-only.
  **Never** point the smoke at a third-party / shared host or
  any target the operator does not control.
- [ ] **Backup posture** decided. The pre-deploy `pg_dump -Fc`
  exists and is off-host; the path/object key is recorded.
  Restore-from-backup procedure read end-to-end (runbook §8 +
  §6.2; operator-facing manual procedure at
  [`backup-restore-runbook.md`](backup-restore-runbook.md) §4
  for backup and §5 + §6 for restore / rollback). Restore
  rehearsal status recorded (PASS / PENDING; canonical log +
  §5 template at
  [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)).
- [ ] **Rollback posture** decided. The previous `vX.Y.Z` AND
  `sha-<short>` for both backend and web images are recorded
  next to the deploy log entry (runbook §6.1). Mark rollback
  "untested" unless actually rehearsed.
- [ ] **Operator-recorded smoke window** chosen — a contiguous
  time window during which no other operator work happens on
  the production stack, so the log / audit sweeps are bounded.
- [ ] **Cleanup approval gate acknowledged.** Per the slice
  contract: BEFORE cleanup the entry stops and reports
  resources; cleanup runs only after operator approval, and
  uses the supported UI/API (no manual DB deletes).

If any prerequisite is not satisfied, **do not start the walk**.
Either resolve it first or record the gap as a PENDING row in
the cutline § 5 / checklist §12, then defer the entry.

## 4. Out of scope for this log

To keep the v1 production smoke log honest:

- **Staging smokes.** Those live in
  [`vps-staging-smoke.md`](vps-staging-smoke.md). Do not append
  staging entries here.
- **Renderer evaluation matrix walks.** Those live in their own
  lane per
  [`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
  and
  [`docs/renderer-comparison-scorecard.md`](../renderer-comparison-scorecard.md);
  v1 production smokes exercise xterm only.
- **Mobile portrait sanity smoke (cutline blocker B3).** That
  is a separate `docs/v1-mobile-portrait-sanity-smoke` slice
  per v1-readiness §7 row 3 and lives in its own entry (target
  doc TBD by that slice; default would be a dedicated sibling
  here or a row in this same file labelled "Mobile portrait
  sanity"). **B3 is NOT resolved by any entry in this log**
  unless an entry explicitly says so AND walks the §9 mobile
  rows of the release-checklist.
- **Tauri desktop / Android shell exercise.** Out of scope for
  v1; the shells are "available, build-it-yourself" per
  [`docs/deployment/tauri-local-build.md`](tauri-local-build.md)
  and the release plan defers signed CI / store bundling
  ([`docs/deployment/tauri-ci-release-plan.md`](tauri-ci-release-plan.md)).
- **Backup-restore rehearsal.** Tracked separately as
  `docs/backup-restore-rehearsal-record` per v1-readiness §7
  honourable mentions; this log only records WHETHER a
  rehearsal has happened, not the rehearsal itself.
- **Production CSP changes.** Out of scope. The strict
  `default-src 'self'` CSP stays.

---

## 5. Entry template

Copy the entire block below into a new dated section under §6
when walking the smoke. Do not delete the inline `<!-- … -->`
comments until the row is filled in or explicitly skipped.

```markdown
### YYYY-MM-DD · v1 production smoke — first walk

> **Status: NOT YET EXECUTED.** This is the placeholder for
> the operator-recorded production-walked end-to-end smoke
> that resolves v1 cutline blocker B2. Until this header is
> replaced with `Status: PASS` and every row below is filled
> in, B2 remains PENDING in
> [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
> §5 and in the release-checklist §12 decision table.

**Slice.** `docs/v1-production-smoke-record` (or the
operator's chosen successor slice that walks against the real
production hostname).

**Surface.** Operator's workstation browser against
`https://<prod-origin>`. Browser/version recorded. The
smoke is operator-walked; no automation drives the writes
in this entry.

**Image freshness.**
- `relayterm-backend` image tag `<vX.Y.Z>` digest `<sha-…>`.
- `relayterm-web` image tag `<vX.Y.Z>` digest `<sha-…>`.
- `relayterm-backend-migrate` image tag `<vX.Y.Z>` digest `<sha-…>`.
- `postgres:17-alpine` — recorded uptime/health.
- Source commit on `main` at smoke time: `<sha-short>`.

**Prerequisites confirmed.** Copy the §3 list above and tick
each box. Any unchecked row blocks the entry from PASS.

**Preflight (§ 5 of the release-checklist re-checked).** Probe
semantics (do NOT re-derive per row): `/healthz` is a
process-alive probe on the backend (static `{"status":"ok"}`; no
DB / config / version exposure); `/_web_health` is an
nginx-static probe on the web container (does not reach the
backend); `/api/v1/auth/me` without a cookie is a routing +
auth-gate sanity check — **`401` is the expected result, not a
failure**.

- `GET https://<prod-origin>/` → `200 (HTML)`.
- `GET https://<prod-origin>/healthz` → `200 {"status":"ok"}`
  (process-alive; not DB readiness).
- `GET https://<prod-origin>/api/v1/auth/me` without cookie →
  `401 {"error":{"code":"unauthorized","message":"unauthorized"}}`
  (`401` is the PASS condition; a `2xx` here is a security
  regression).
- TLS cert valid from the operator workstation (browser AND
  `curl -sf` both succeed; no `--insecure`).
- Reverse proxy verified to preserve `Origin`, pass WS upgrade
  headers, hold long-lived `proxy_read_timeout` on the API
  location (runbook §9.2 / §9.3 / §9.4).
- Production-shell CSP header is the strict
  `default-src 'self'` posture — NO `'wasm-unsafe-eval'`,
  NO experimental-renderer relaxations.
- Backend health from the deploy-host loopback:
  `curl -sf http://127.0.0.1:8081/healthz` → `{"status":"ok"}`.
- Web health from the deploy-host loopback:
  `curl -sf http://127.0.0.1:8081/_web_health` → `ok`.
- `docker compose ps` shows `postgres` / `relayterm-backend` /
  `relayterm-web` as `(healthy)`. Note `(healthy)` on
  `relayterm-backend` is process-alive only; the `postgres` row
  (driven by `pg_isready`) is the corresponding DB-side liveness.
- Migration status: final applied migration ID `<id>` (no
  pending / failed migration on the migrate container).

**Throwaway record naming (timestamp suffix `tYYYYMMDD`,
prefix `v1-prod-smoke-`).** Throwaway records used by the
smoke:

| Role | Initial name |
|---|---|
| Identity | `v1-prod-smoke-identity-tYYYYMMDD` |
| Host | `v1-prod-smoke-host-tYYYYMMDD` |
| Server profile | `v1-prod-smoke-profile-tYYYYMMDD` |

(Add a second throwaway row for any delete-success step that
needs a "delete me" record so the smoke does not delete an
in-use production record.)

#### Authentication rows

| Row | Goal | Result | Wire / observed |
|---|---|---|---|
| A | Bootstrap / first-user state | PENDING | <!-- existing user → bootstrap closed/refused; OR fresh install → bootstrap once with configured token; record which case applies. Do not paste the token. --> |
| B | Login → /me → logout → /me 401 → login again | PENDING | <!-- SPA login; /api/v1/auth/me returns user; logout clears cookie; /me without cookie returns 401; re-login for the rest of the smoke. --> |
| C | Session list visible; revoke a non-current session if safe | PENDING | <!-- AuthSessionsPanel.svelte; if no second session is in flight, mark "not exercised — single session only" and cite staging evidence for revoke. --> |
| C' | Password change (operator approval required) | PENDING | <!-- Mark DEFERRED for production smoke and cite staging if operator has not approved a production password change. --> |
| C'' | CSRF / Origin negative check | PENDING | <!-- curl -i -X POST -H 'Origin: https://attacker.example.com' -b cookies.txt -d '{}' https://<prod-origin>/api/v1/hosts → 403 csrf_origin_mismatch; body MUST NOT echo the offered Origin. --> |

#### Inventory rows

Use the throwaway naming above. Every row maps to an entry in
the release-checklist §7.

| Row | Goal | Result | Wire / observed |
|---|---|---|---|
| D | SSH identity create (generate Ed25519) | PENDING | <!-- IdentitiesView.svelte; identity detail panel shows NO private_key / encrypted_private_key / raw PEM / BEGIN OPENSSH PRIVATE KEY substring anywhere. Only public-key metadata + fingerprint. --> |
| D' | SSH identity import (only if v1 import is in scope AND operator approves a throwaway key) | PENDING / DEFERRED | <!-- If deferred: cite docs/private-key-import.md + staging evidence and record "generate-only accepted for v1". --> |
| E | Host create | PENDING | <!-- servers-create-host-* selectors. --> |
| E' | Host edit (display-name / hostname / port / username) | PENDING | <!-- host-detail-edit-open + host-detail-edit-{display-name,hostname,port,username}; verify change persists in list and detail. --> |
| E'' | Host delete success (use a SECOND throwaway host so prod data stays intact) | PENDING | <!-- host-detail-delete-open → typed confirm → submit → 204; follow-up GET → 404; row absent from list. --> |
| E''' | Host referenced-delete refused (only if practical without creating production noise) | PENDING / SKIPPED | <!-- If skipped, cite the 2026-05-17 staging entry rows A + C as the byte-exact safe-formatter mapping evidence; the production walk does NOT need to re-prove the wire-message redaction since it is pinned by unit tests in inventoryMutationsApi.test.ts. --> |
| F | Host-key preflight + trust | PENDING | <!-- HostKeyPanel.svelte; trust step writes one audit row with public-key fingerprint metadata only — no key bytes, no peer banners, no russh error text. --> |
| G | Server profile create | PENDING | <!-- servers-create-profile-*. --> |
| G' | Server profile edit (name / username-override / tags) | PENDING | <!-- profile-detail-edit-open + profile-detail-edit-{name,host,identity,username-override,tags}. --> |
| G'' | Server profile delete success (SECOND throwaway profile with no history) | PENDING | <!-- profile-detail-delete-open → typed confirm → submit → 204; follow-up GET → 404. --> |
| G''' | Server profile disable + re-enable (ONLY against a throwaway profile that has history; do NOT delete profiles with history) | PENDING / SKIPPED | <!-- If no production profile has history yet (likely for the first production smoke), mark SKIPPED and cite the 2026-05-17 staging row G as the byte-exact evidence. --> |
| H | Auth-check against the configured target | PENDING | <!-- AuthCheckPanel.svelte succeeds. --> |

#### Terminal rows (xterm only)

xterm.js is the v1 production default. No experimental
renderer is loaded by any row below; do NOT flip the
`experimentalRendererEvaluationEnabled` gate. → SMOKE.md § D
for the renderer-fair input methodology.

| Row | Goal | Result | Wire / observed |
|---|---|---|---|
| I | Launch xterm session from production shell | PENDING | <!-- production-terminal-viewport; data-renderer="xterm" if visible; data-renderer-fallback="" (empty); no error panel; prompt appears. --> |
| I' | Type `whoami`, `pwd`, `echo relayterm-v1-prod-smoke` | PENDING | <!-- Output matches the configured username + home dir; the echo line lands as exactly the byte string above so the redaction sweep below has a deterministic positive sentinel to look for. --> |
| J | Window resize (Fit) | PENDING | <!-- Terminal remains usable; output reflows; no replay_window_lost frame. Do not require experimental renderer behavior. --> |
| K | Detach / reconnect inside DETACHED_LIVE_PTY_TTL | PENDING | <!-- Run `echo relayterm-v1-before-detach`; detach through UI; reconnect from Sessions list within TTL; verify same session if visible; run `echo relayterm-v1-after-reconnect`. The two echo strings serve as sentinels for the redaction sweep AND as proof of replay. --> |
| L | Close / end session | PENDING | <!-- Sessions list shows the closed state; second close call is idempotent. No stuck active session. --> |
| L' | Launch timing diagnostics sanity | PENDING | <!-- With the SPA debug panel open during launch I, record the counter ms values: `create_session_post_resolved`, `ws_open`, `first_server_message`, `first_output`, `ws_close` (if closed). No timing field carries terminal payload bytes. --> |

#### Security / redaction / log rows

| Row | Goal | Result | Wire / observed |
|---|---|---|---|
| M | Bounded backend log sweep | PENDING | <!-- docker compose logs --since <window> relayterm-backend \| grep -E '<sentinel-set>'  → "ok: no leakage sentinels found". One hit blocks PASS. Sentinel set is §5.3 below. --> |
| M' | Bounded web (nginx) log sweep | PENDING | <!-- Same sentinel set; same decision rule. --> |
| M'' | SSH target sshd log sweep (only if throwaway) | PENDING / SKIPPED | <!-- Only against a throwaway target you own; never sweep production sshd. Known false positives to ignore: "missing session cookie" (it is the absence-of-value label, not a leak); "User/password ssh access is disabled" (sshd policy line). --> |
| N | Browser storage inspection (if safe) | PENDING | <!-- document.cookie should NOT reveal the HttpOnly auth cookie (it returns ""); localStorage must NOT contain private-key material or terminal payload; sessionStorage must NOT contain terminal payload. --> |
| O | Audit row inspection (recent audit_events.payload) | PENDING | <!-- SELECT count(*) FROM audit_events WHERE created_at > now() - interval '<window>' AND (payload::text ~ '<sentinel-regex>'); expected 0. --> |

#### Backup / rollback rows

| Row | Goal | Result | Wire / observed |
|---|---|---|---|
| P | DB backup evidence pre-smoke | PENDING | <!-- pre-deploy pg_dump path/object key recorded (off-host); chmod 600 preserved on .env copy. Walk the operator-facing procedure at backup-restore-runbook.md §4 (backup) and have §5 (restore) + §6 (rollback) read end-to-end before recording PASS. If rehearsal status is PENDING, record it as such (the rehearsal log is backup-restore-rehearsal-record.md; the operator-walked entry lands there via the future docs/backup-restore-rehearsal-run slice). --> |
| Q | Rollback evidence | PENDING | <!-- Current image tag/digest recorded; previous known-good `vX.Y.Z` + sha-<short> recorded; rollback procedure (runbook §6.1) read end-to-end. Mark "untested" unless actually rehearsed. --> |

#### Cleanup posture

Per the AGENTS.md "Inventory lifecycle and destructive-action
policy" and the slice contract:

- Report all throwaway records / hosts / profiles / identities
  / SSH targets / cookies before cleanup; **stop and await
  operator approval**.
- After approval: delete throwaway records via the supported
  UI/API in dependency-safe order (profiles → hosts →
  identities). Disable (do not delete) any profile that has
  `terminal_sessions` history.
- Leave `terminal_sessions`, `session_events`, `audit_events`,
  and `known_host_entries` untouched. No manual SQL DELETE on
  any of those tables.
- Leave the production stack running. Do NOT delete the real
  operator account.

#### Decision

- **B2 status:** PENDING (template; no production walk yet) /
  PASS (operator filled in every row above and the redaction
  sweep returned zero hits) / BLOCKED (one or more rows
  blocked PASS; record the blocker inline).
- **B3 status:** unchanged by this entry — B3 is the mobile
  portrait sanity smoke (separate slice
  `docs/v1-mobile-portrait-sanity-smoke`).
- **Renderer posture:** xterm remains the production default;
  no renderer promotion; the experimental gate stayed `off`
  for the entire entry.
- **Scope diff against the repo:** docs-only. Zero source /
  schema / migration / route / CSP / deploy / CI /
  orchestrator / protocol / renderer change.
```

## 5.1 Sentinel set (§ M / § O)

The canonical sentinel set the redaction sweep grep / regex
must include. This is the **superset** of:

1. `AUDIT_FORBIDDEN_SUBSTRINGS` in
   `crates/relayterm-api/tests/api.rs` (the in-CI audit-payload
   backstop the backend tests every audit-emitting kind against),
   AND
2. the staging-smoke 2026-05-17 entry's sweep set, AND
3. the release-checklist §8 / §11 grep set, AND
4. the cookie-value pattern from the runbook §10 smoke.

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

**Do NOT include bare `password` in the grep set** — the sshd
line `"User/password ssh access is disabled"` produces a known
false positive (§ 5.2) and the actual forbidden token is
`password_hash`, which is already in the set above.

Smoke-introduced positive sentinels (these MUST appear in the
terminal viewport / replay path but MUST NOT appear in any
log / audit / response body / DOM `data-*` / browser storage):

```
relayterm-v1-prod-smoke
relayterm-v1-before-detach
relayterm-v1-after-reconnect
```

If any of the smoke-introduced strings shows up in a backend
log, nginx access log, `audit_events.payload`, or a `data-*`
attribute, that is a terminal-payload leak — the entry CANNOT
record PASS.

## 5.2 Known false positives (do NOT flag)

- `missing session cookie` — diagnostic label naming the
  ABSENCE of a value, not a cookie leak (per the runbook §10
  smoke and the 2026-05-17 staging entry).
- `User/password ssh access is disabled` — sshd policy line on
  a throwaway SSH target; not a credential leak.

## 5.3 Sweep query templates

Backend log (substitute `<window>` for the smoke window):

```sh
docker compose logs --since <window> relayterm-backend | \
  grep -E 'private_key|encrypted_private_key|BEGIN OPENSSH PRIVATE KEY|password_hash|session_token|token_hash|bootstrap_token|argon2id|client_info|remote_addr|user_agent|openssh-key-v1|passphrase|data_b64|REDACT-MARKER|relayterm_session=[A-Za-z0-9_-]{20,}|relayterm-v1-prod-smoke|relayterm-v1-before-detach|relayterm-v1-after-reconnect' || \
  echo "ok: no leakage sentinels found"
```

Nginx web log (same sentinel set as the backend grep above —
inlined here so an operator copy-pasting the snippet does not
have to reconstruct the regex):

```sh
docker compose logs --since <window> relayterm-web | \
  grep -E 'private_key|encrypted_private_key|BEGIN OPENSSH PRIVATE KEY|password_hash|session_token|token_hash|bootstrap_token|argon2id|client_info|remote_addr|user_agent|openssh-key-v1|passphrase|data_b64|REDACT-MARKER|relayterm_session=[A-Za-z0-9_-]{20,}|relayterm-v1-prod-smoke|relayterm-v1-before-detach|relayterm-v1-after-reconnect' || \
  echo "ok: no leakage sentinels found"
```

Audit payload sweep (the SQL regex string is intentionally on
a single line — `psql` does accept embedded newlines in `-c`
input, but a single-line literal is friendlier to copy-paste
into a non-interactive shell):

```sh
docker compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c \
  "SELECT count(*) FROM audit_events WHERE created_at > now() - interval '<window>' AND (payload::text ~ 'private_key|encrypted_private_key|BEGIN OPENSSH|password_hash|session_token|token_hash|bootstrap_token|argon2id|client_info|remote_addr|user_agent|openssh-key-v1|passphrase|data_b64|REDACT-MARKER|relayterm-v1-prod-smoke|relayterm-v1-before-detach|relayterm-v1-after-reconnect');"
```

Expected: `count = 0`. Any row is a leak.

---

## 6. Verification log

> Append entries in reverse chronological order (newest first).
> Each entry uses the §5 template verbatim. **Do not edit a
> committed entry to make a row PASS retroactively** — write a
> follow-up entry instead.

### 2026-05-17 · v1 production smoke — slice docs/v1-production-smoke-record SKELETON ONLY (not executed)

> **Status: NOT EXECUTED.** This slice was scoped as a
> documentation skeleton because the operator has not yet
> chosen a production hostname / deploy host. The §5 template
> above is the deliverable. No production stack was touched;
> no smoke rows were walked; no PASS decision was recorded.

**Slice.** `docs/v1-production-smoke-record`.

**Why no walk happened.** Per the slice's operator
clarification (recorded the same day): production target is
not chosen yet. The slice was re-scoped to deliver the
template only, so a subsequent slice can copy §5 into a new
dated entry and walk the rows once the operator commits to a
production hostname / deploy host / image tag.

**What this slice DID change** (docs-only):
- `docs/deployment/v1-production-smoke.md` — created; contains
  the §1 purpose, §2 posture, §3 prerequisites checklist, §4
  out-of-scope, §5 entry template, §5.1–§5.3 sentinel sets +
  sweep query templates, §6 verification log (this entry).
- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  — §12 decision-table "Production-walked end-to-end smoke
  (B2)" row updated to point at this file; §14 "Next slices"
  row 1 updated to point at this file as the template ready
  for an operator-walked successor slice. B2 row stays
  PENDING (template is not evidence).
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  — §5 B2 row updated to point at this file as the
  to-be-filled-in template; B2 stays PENDING. **B3 unchanged
  — this slice does NOT touch the mobile portrait sanity
  smoke.**

**What this slice did NOT change** (load-bearing):
- No source / schema / migration / route / CSP / deploy / CI /
  orchestrator / protocol / renderer code.
- No production stack: zero `docker pull`, zero `docker
  compose` calls against any production host, zero HTTP /
  WS / SSH connection to any production origin.
- No staging-smoke entry was added or edited; the staging log
  at [`vps-staging-smoke.md`](vps-staging-smoke.md) is
  untouched.
- No experimental renderer was promoted; the
  `experimentalRendererEvaluationEnabled` gate default is
  unchanged; xterm remains the production default.
- No B2 PASS / B3 PASS / B2 BLOCKED was recorded — B2 status
  in the cutline + release-checklist remains **PENDING**.

**Cleanup posture.** No throwaway records created (no
production stack touched). No cleanup needed.

#### Posture (load-bearing)

- **No renderer promotion.** xterm remains the production
  default; the experimental gate default is unchanged.
- **Docs-only.** Source / schema / deploy / CI untouched.
- **No production hostname touched.** This slice did not
  contact any production origin.
- **B2 status:** still PENDING. The template now exists;
  walking it is the next slice's deliverable.
- **B3 status:** unchanged.

#### Next slice (proposed; not executed by this slice)

- **Operator-walked successor of `docs/v1-production-smoke-
  record`** (resolves B2). Once the operator has chosen a
  production hostname / deploy host / image tag, copy §5
  into a new dated entry above this one and walk every row
  against the real production deploy. Treat each PENDING
  cell in the §5 template as an explicit gate; do not record
  PASS until every row is filled and the §M / §M' / §O
  redaction sweeps return zero hits across the canonical
  sentinel set (§5.1).
- **`docs/v1-mobile-portrait-sanity-smoke`** (resolves B3;
  v1-readiness §7 row 3). Independent of this log; walks
  release-checklist §9 against a real Android phone on the
  default xterm production path.
- **Honourable mention:**
  ~~`docs/backup-restore-rehearsal-record`~~ **DONE
  (template) — 2026-05-18** at
  [`backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md).
  Operator-recorded rehearsal log skeleton (§5 template, §10
  verification log seeded NOT RUN). Successor slice
  `docs/backup-restore-rehearsal-run` is the operator-walked
  Case R-B restore against a throwaway Postgres that closes
  v1-readiness §4.4 "Restore-test rehearsal." Not v1-blocking
  but tightens P / Q rows of any future production smoke
  entry here.

---

## See also

- [`docs/v1-release-checklist.md`](../v1-release-checklist.md)
  — release-day operator checklist; the §12 decision-table B2
  row points here.
- [`docs/v1-production-readiness.md`](../v1-production-readiness.md)
  — v1 cutline; §5 blocker B2 ("No production-walked end-to-end
  smoke recorded") is what entries in this log resolve.
- [`docs/deployment/production-runbook.md`](production-runbook.md)
  — load-bearing operator runbook (§4 first deploy, §6
  rollback, §7 migration, §8 backup/restore, §9 reverse proxy,
  §10 post-deploy smoke, §11 secret rotation).
- [`docs/deployment/backup-restore-runbook.md`](backup-restore-runbook.md)
  — operator-facing manual backup / restore / rollback procedure
  the §3 backup-posture prerequisite and §5 row P rest on.
- [`docs/deployment/backup-restore-rehearsal-record.md`](backup-restore-rehearsal-record.md)
  — operator-recorded rehearsal log; §3 backup-posture
  prerequisite and §5 row P cite this file for the rehearsal
  status.
- [`docs/deployment/vps-staging-smoke.md`](vps-staging-smoke.md)
  — staging-only smoke log; do not co-mingle entries.
- [`apps/web/e2e/SMOKE.md`](../../apps/web/e2e/SMOKE.md) — SPA
  smoke runbook; §B is the input methodology for §5 inventory
  + terminal rows.
- [`docs/spec/inventory.md`](../spec/inventory.md) —
  destructive-action policy that §5 inventory rows D–H rest on.
- [`docs/private-key-import.md`](../private-key-import.md) — v1
  Ed25519 unencrypted OpenSSH constraint for §5 row D'.
- [`docs/terminal-recording.md`](../terminal-recording.md) —
  off-by-default recording posture cited in §3 + §5 row L'.
- [`docs/spec/terminal-adapters.md`](../spec/terminal-adapters.md)
  — xterm-as-default rule + experimental-renderer gate
  contract referenced in §2 + §5 terminal rows.
- [`AGENTS.md`](../../AGENTS.md) — "Things to avoid",
  inventory-lifecycle policy, and the load-bearing redaction
  rules every smoke entry rests on.
