# RelayTerm production auth smoke guide

This is the operator-side smoke procedure for a RelayTerm deployment that
runs `auth.mode = production` behind an HTTPS reverse proxy. It exists to
verify that a fresh deploy or a fresh release matches the contracts pinned
by `SPEC.md` → "Production authentication architecture".

`SPEC.md` is the normative spec. `docs/production-auth.md` is the
deployment / configuration guide. **This file is the deployment-time
checklist** — what to run, what to expect, and how to read the results.
Drift between this guide and the code is a doc bug; the integration tests
in `crates/relayterm-api/tests/api.rs` are the executable contract.

If anything below disagrees with the code, the code wins. File a doc fix.

---

## Prerequisites

- A real deployment behind a reverse proxy that terminates TLS (Traefik,
  Caddy, nginx, Cloudflare, …). The smoke commands assume `https://` —
  the cookie carries the `Secure` flag and browsers refuse to send it
  over plain HTTP.
- Postgres reachable and migrations applied. The backend does NOT
  auto-run sqlx migrations on startup — apply them explicitly with
  `cargo sqlx migrate run` from `apps/backend/` (or from a CI step)
  before the first boot. A backend started against a database with no
  schema will appear healthy on `GET /healthz` and then return
  `500 internal_error` on the first `POST /api/v1/auth/bootstrap`
  because the `user_passwords` table does not exist yet.
- The configuration envelope from `docs/production-auth.md` § 2 is
  satisfied. The backend MUST have started cleanly — check the log for
  `auth_mode = production` and no `bail!` lines.
- An HTTP client that can send cookies. The examples below use `curl`
  with `--cookie` / `--cookie-jar`; any client works as long as it
  preserves cookies across calls AND can set the `Origin` header.
- Optional: a separate admin shell with `psql` access to the deployment
  database. Several of the audit verification steps require querying
  `audit_events` directly — the backend has no admin / cross-user audit
  surface in v1 (`SPEC.md` → "Out of scope (v1)").

> **Warning — secrets and shell history.** `curl --data` puts the
> bootstrap token / password into `~/.bash_history` (or `fish_history`)
> by default. Suppress history before running these commands (bash:
> `HISTCONTROL=ignorespace` plus a leading space on every command;
> fish: `fish_private_mode=1` exported into the smoke shell, OR `set -g
> fish_history ""` for the duration of the session, OR delete the
> recorded entries afterwards with `builtin history delete --
> "<command>"`). The portable alternative is to pipe the request body
> in from a file you delete after the smoke (works in both shells; the
> body never lands in history). Never commit the values, never paste
> them into a chat client, never put them in a tracing log line.

---

## Required env / config recap

These are the only fields this smoke directly depends on — see
`docs/production-auth.md` § 2 for the full table.

| Field | Smoke role |
|---|---|
| `auth.mode = production` | Activates the production validation envelope (signing key required, non-empty allow-list, Secure cookies). |
| `auth.allowed_origins` | The smoke `curl` commands send `Origin: <one entry>` on every POST. The value MUST match byte-for-byte. |
| `auth.cookie_secure = true` | The `Set-Cookie` carries `Secure`. Browsers refuse to send it over `http://`; the smoke `curl` examples use `https://`. |
| `auth.first_user_bootstrap_token` | Required at startup ONLY when `user_passwords` is empty. The smoke verifies this is unset / disabled after the first user lands (see § Negative cases). |
| `RELAYTERM_DATABASE__URL` (or `DATABASE_URL`) | The audit-event `psql` step queries this DB. |

Throughout this guide:

- Replace `https://relay.example.com` with the real public origin.
- Replace `<bootstrap_token>` with the value of
  `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN`.
- Replace `<password>` / `<new_password>` with high-entropy values
  ≥ 12 chars (the `PASSWORD_MIN_LEN` floor enforced by
  `crates/relayterm-api/src/dto/auth.rs`).
- Replace `operator@example.com` with the real operator email.

`/tmp/relayterm.cookies` is the cookie jar used by every example; pick
any path you have write access to. Delete the jar after the smoke is done
— the cookie value is a live session token until it expires or you call
`/auth/logout`.

---

## 1. Start the backend and the SPA

The backend listens on `127.0.0.1:8080` by default. The reverse proxy is
the public entry point; nothing in the smoke calls the backend directly.

```sh
# On the deployment host. Watch the log for `auth_mode = production`.
systemctl start relayterm-backend         # or your supervisor of choice
journalctl -u relayterm-backend -f        # tail until the listener binds
```

For the SPA half of the smoke, point a real browser at
`https://relay.example.com/`. The smoke expects the SPA's static assets
to be served from the same origin as the API (the AGENTS.md "Web app
defaults" overlay). If you serve them separately, every browser-write
will return `403 csrf_origin_mismatch`.

If `auth.first_user_bootstrap_token` is set AND `user_passwords` is
empty, the backend boots normally — the bootstrap route is enabled and
returns `401 bad bootstrap token` for any non-matching token. If the
token is unset AND `user_passwords` is empty, startup `bail!`s with
`auth.mode = production with no existing user requires
auth.first_user_bootstrap_token` (see `docs/production-auth.md` § 7);
fix the config and restart before continuing.

---

## 2. First-user bootstrap

Run exactly once on a fresh database. After this step succeeds, every
subsequent call to `/auth/bootstrap` returns `409 already_bootstrapped`
(or `503 service_unavailable` if you've unset the token and restarted).

```sh
curl -fsS -X POST https://relay.example.com/api/v1/auth/bootstrap \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --data '{
    "bootstrap_token": "<bootstrap_token>",
    "email":           "operator@example.com",
    "display_name":    "Operator",
    "password":        "<password>"
  }'
```

Expected:

- HTTP `201 Created`.
- Response body: `{ "id": "<uuid>", "email": "operator@example.com",
  "display_name": "Operator", "created_at": "<rfc3339>",
  "last_login_at": null }`.
- **No `Set-Cookie` header.** Bootstrap deliberately does NOT mint a
  session — the audit trail is `first_user_created` + `login_succeeded`
  as two distinct rows.

Common failures to triage from the wire response alone:

| Status | Body shape | Meaning |
|---|---|---|
| `400` | `{ "error": { "code": "validation_error", … } }` | Email malformed, display name empty, password fails the 12–1024 char floor / ceiling. The body NEVER echoes the offered token or password. |
| `401` | `{ "error": { "code": "unauthorized", "message": "bad bootstrap token" } }` | The offered token did not match `auth.first_user_bootstrap_token`. |
| `403` | `{ "error": { "code": "csrf_origin_mismatch", … } }` | Missing or non-allowlisted `Origin` header. The body NEVER echoes the offered Origin value. |
| `409` | `{ "error": { "code": "conflict", "entity": "user", "reason": "already_bootstrapped" } }` | Bootstrap already ran — every subsequent call collapses here regardless of the token. |
| `503` | `{ "error": { "code": "service_unavailable", "message": "bootstrap is disabled (no first_user_bootstrap_token configured)" } }` | The token is unset. The bootstrap route is closed. |

After a `201`, **unset `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` and
restart the backend.** Subsequent calls then return `503` (the cleaner
end state) instead of `409`.

---

## 3. Login

Mints a fresh `user_sessions` row and writes a `Set-Cookie:
relayterm_session=…; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000;
Secure` header. Capture the cookie into a jar — every subsequent call in
this smoke depends on it.

```sh
curl -fsS -X POST https://relay.example.com/api/v1/auth/login \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --cookie-jar /tmp/relayterm.cookies \
  --data '{
    "email":    "operator@example.com",
    "password": "<password>"
  }' \
  -i
```

Expected:

- HTTP `200 OK`.
- Response body: a `UserResponse` shape (same as bootstrap's body) with
  `last_login_at` populated.
- `Set-Cookie: relayterm_session=…` with `HttpOnly`, `SameSite=Strict`,
  `Secure`, `Path=/`, `Max-Age=2592000` (30 days). `Domain` is omitted
  unless you configured `auth.cookie_domain`.
- A row appears in `audit_events` with `kind = login_succeeded` and
  `actor_id = <user_id>` (verified in § 11).

Failure responses:

| Status | Meaning | Notes |
|---|---|---|
| `401 invalid credentials` | Wrong password OR unknown email. Probe-resistant: the two collapse to the same body. | Audited as `login_failed` with `reason = "bad_credentials"` and `actor_id = NULL`. |
| `403 csrf_origin_mismatch` | Missing / non-allowlisted `Origin`. Does NOT touch the throttle bucket. | Body never echoes the offered Origin. |
| `429 too_many_requests` | The login throttler tripped. 5 failures inside a 15-minute sliding window per normalized email. | Static body; no `Retry-After` header (intentional — leaks countdown telemetry). |

---

## 4. /auth/me

Idempotent read. Confirms the cookie is honoured and returns the
caller's `users` row. No CSRF guard (GET).

```sh
curl -fsS https://relay.example.com/api/v1/auth/me \
  --cookie /tmp/relayterm.cookies
```

Expected: `200 OK` with the same `UserResponse` shape as login. A
missing / expired / revoked / unknown cookie returns `401 unauthorized`
with a static body.

---

## 5. Access a protected route

Pick any inventory route — they ALL run through `AuthenticatedUser`.
This smoke uses `GET /api/v1/hosts` because it is read-only and never
touches infrastructure.

```sh
# With cookie — expect 200 OK + JSON body (possibly empty list).
curl -fsS https://relay.example.com/api/v1/hosts \
  --cookie /tmp/relayterm.cookies

# Without cookie — expect 401 unauthorized.
curl -i -X GET https://relay.example.com/api/v1/hosts
```

The 401 path's body shape is `{ "error": { "code": "unauthorized", … } }`
and is byte-identical for "no cookie", "expired cookie", "revoked cookie",
and "unknown cookie" — operator-side detail lives in the backend
`warn!` log line in `crates/relayterm-api/src/error.rs::IntoResponse`.

---

## 6. SPA AuthGate (browser smoke)

Open `https://relay.example.com/` in a fresh browser profile (no prior
cookies for the origin). The SPA boots through the AuthGate flow defined
in `apps/web/e2e/SMOKE.md`:

1. **Loading state.** `[data-testid="auth-loading"]` flashes briefly
   while the SPA issues `GET /api/v1/auth/me`. With no cookie it
   resolves to 401 and the gate flips to the login screen.
2. **Login screen renders.** `[data-testid="auth-login-screen"]` is
   present; the heading is the static "Sign in to RelayTerm" string and
   does NOT reveal whether the offered email belongs to a known account.
3. **Bootstrap link is visible** when no first user exists yet
   (`[data-testid="auth-login-bootstrap-link"]`); after the first user
   lands, the link is still present (the SPA cannot tell a priori) but
   submitting through it returns `409 already_bootstrapped`.
4. **Sign in** with the credentials from § 3 above. On success, the
   gate flips to the production app shell
   (`[data-testid="app-shell-main"]`) and the top-bar shows
   `[data-testid="auth-current-user"]` with the operator's display name.
5. **Reload the page.** The cookie persists, the AuthGate's loading
   splash flashes, and the production shell renders without prompting
   for a password again.

If the AuthGate sticks on `[data-testid="auth-error-screen"]`, the SPA
got a transport / 5xx / malformed response from `/auth/me`. The error
panel carries an explicit `[data-testid="auth-error-retry"]` button —
the SPA does NOT auto-retry. Check the backend log first; the wire
message NEVER reaches the operator-facing copy.

---

## 7. Change password

Authenticated user rotates their own password. The current cookie stays
valid; every OTHER session for the user is revoked.

```sh
curl -fsS -X POST https://relay.example.com/api/v1/auth/change-password \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --cookie /tmp/relayterm.cookies \
  --data '{
    "current_password": "<password>",
    "new_password":     "<new_password>"
  }'
```

Expected:

- HTTP `200 OK`.
- Response body: `{ "revoked_other_sessions": <u64> }`. **Never** the
  passwords, **never** any hash, **never** per-session ids.
- The current cookie still works — confirm by re-running step 4 (`me`)
  with the same jar; expect `200`.

Failure responses:

| Status | Meaning |
|---|---|
| `400` | `current_password` empty / over `PASSWORD_MAX_LEN`, or `new_password` outside `[12, 1024]`. |
| `401` | Wrong current password. **No audit row written.** No password row touched, no other sessions revoked. The body collapses to a generic `unauthorized` — the wire response cannot distinguish "wrong password" from "current cookie just expired between request and verify". |
| `403` | `csrf_origin_mismatch`. |

### 7a. Verify the old password no longer works

```sh
# Use a NEW cookie jar so the failed-login attempt does not touch the
# jar that holds your valid session. (The throttler keys on the
# normalized email — not the cookie — so a separate jar has no effect
# on the throttle bucket; the separation here is purely about not
# disturbing the session jar.)
curl -fsS -X POST https://relay.example.com/api/v1/auth/login \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --cookie-jar /tmp/relayterm.old.cookies \
  --data '{
    "email":    "operator@example.com",
    "password": "<password>"
  }' \
  -i
```

Expected: `401 invalid credentials`. Audit gets one
`login_failed { reason: "bad_credentials" }` row with `actor_id = NULL`.

### 7b. Verify the new password succeeds

```sh
curl -fsS -X POST https://relay.example.com/api/v1/auth/login \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --cookie-jar /tmp/relayterm.new.cookies \
  --data '{
    "email":    "operator@example.com",
    "password": "<new_password>"
  }' \
  -i
```

Expected: `200 OK` + a fresh `Set-Cookie`. Audit gets a
`login_succeeded` row.

---

## 8. Session management

The current-user session API lets a signed-in user see and revoke their
own browser sessions. There is no admin / cross-user surface in v1.

### 8a. List sessions

```sh
curl -fsS https://relay.example.com/api/v1/auth/sessions \
  --cookie /tmp/relayterm.cookies
```

Expected: `200 OK` with body `{ "sessions": [ {…}, {…}, … ] }`. Each
item carries `id`, `created_at`, `last_seen_at`, `expires_at`,
`revoked_at`, `current` (`true` for the row that authenticated this
request), and `status` ∈ `{"active", "expired", "revoked"}`. The wire
DTO **never** carries `token_hash` or any plaintext token. Newest-first
by `created_at`.

If you bootstrapped, logged in once, and changed the password, the list
should show:

- the cookie you're holding (`current: true`, `status: "active"`),
- and possibly a few `revoked` rows (from the password-change
  revoke-other-sessions step, depending on how many concurrent logins
  you'd accumulated).

### 8b. Revoke another session (if available)

If 8a returned more than one row with `status: "active"`, pick any
non-current id and revoke it:

```sh
SESSION_ID=<uuid-from-8a-where-current=false-and-status=active>
curl -fsS -X POST \
  "https://relay.example.com/api/v1/auth/sessions/${SESSION_ID}/revoke" \
  -H 'Origin: https://relay.example.com' \
  --cookie /tmp/relayterm.cookies \
  -i
```

Expected:

- HTTP `204 No Content`.
- **No `Set-Cookie` clear** on the response — you're not revoking your
  own session.
- A subsequent `/auth/sessions` GET shows the row with
  `status: "revoked"` and a non-null `revoked_at`.
- One `audit_events` row appears with `kind = "session_revoked"`,
  `actor_id = <your user id>`, payload `{ "session_id": "<uuid>",
  "current_session": false, "revoked_at": "<rfc3339>" }`.

A revoke against an already-revoked owned row is idempotent: returns
`204` and writes NO audit row. A revoke against a row owned by a
different user OR a row that doesn't exist BOTH return a byte-identical
`404 not_found` (probe resistance).

### 8c. Revoke all OTHER sessions

```sh
curl -fsS -X POST \
  https://relay.example.com/api/v1/auth/sessions/revoke-all-except-current \
  -H 'Origin: https://relay.example.com' \
  --cookie /tmp/relayterm.cookies
```

Expected: `200 OK` with body `{ "revoked_count": <u64> }`. No
`Set-Cookie` clear (the request itself proves the caller wants to keep
the current session). One `audit_events` row appears with `kind =
"sessions_revoked"` and payload
`{ "revoked_count": <count>, "revoked_at": "<rfc3339>" }` — **never**
per-row session ids on the wire OR in the audit payload. A re-call
when there are zero non-revoked others returns `revoked_count: 0` and
writes NO audit row.

### 8d. Revoke the CURRENT session = self-logout

Allowed by design. Pick the row with `current: true` from § 8a:

```sh
CURRENT_SESSION_ID=<uuid-from-8a-where-current=true>
curl -fsS -X POST \
  "https://relay.example.com/api/v1/auth/sessions/${CURRENT_SESSION_ID}/revoke" \
  -H 'Origin: https://relay.example.com' \
  --cookie /tmp/relayterm.cookies \
  --cookie-jar /tmp/relayterm.cookies \
  -i
```

Expected:

- HTTP `204 No Content`.
- `Set-Cookie: relayterm_session=; Max-Age=0` clearing cookie.
- One `audit_events` row with `kind = "session_revoked"`, payload
  `{ "session_id": "<uuid>", "current_session": true, "revoked_at":
  "<rfc3339>" }` — `current_session: true` is what distinguishes
  self-revoke from "revoked another browser".
- A subsequent `/auth/me` against the same jar returns `401`.

### 8e. SPA Settings session panel (browser smoke)

In the browser tab from § 6:

1. Click `[data-testid="nav-settings"]` to navigate to the Settings view.
2. Confirm `[data-testid="settings-auth-sessions"]` renders.
3. The current row carries `[data-testid="settings-auth-sessions-current-badge"]`
   and the per-row Revoke button is the
   `[data-testid="settings-auth-sessions-revoke-current"]` variant.
4. Confirm `[data-testid="settings-auth-sessions-future-note"]` is the
   honest disclaimer naming `remote_addr` / `user_agent` / device-name
   / password-reset / passkeys / admin views as deferred.

Clicking Revoke on the current row runs the local sign-out cleanup and
flips the gate back to `[data-testid="auth-login-screen"]`. Clicking
"Revoke all other sessions" requires confirmation, POSTs to
`/auth/sessions/revoke-all-except-current`, and shows
`[data-testid="settings-auth-sessions-success"]` with the count.

---

## 9. Logout

Plain logout via the dedicated route (the equivalent of § 8d using
`/auth/logout` instead of self-revoke):

```sh
curl -fsS -X POST https://relay.example.com/api/v1/auth/logout \
  -H 'Origin: https://relay.example.com' \
  --cookie /tmp/relayterm.cookies \
  --cookie-jar /tmp/relayterm.cookies \
  -i
```

Expected:

- HTTP `204 No Content`.
- `Set-Cookie: relayterm_session=; Max-Age=0` clearing cookie.
- One `audit_events` row with `kind = "logout_succeeded"` and
  `actor_id = <user_id>`.
- A subsequent `/auth/me` against the same jar returns `401`.

In the SPA, `[data-testid="auth-sign-out"]` runs the same flow.

---

## 10. Negative cases

These probes verify the security posture is intact. The expected outcome
is a specific failure shape, NOT success.

### 10a. POST with no `Origin` header → 403

```sh
curl -i -X POST https://relay.example.com/api/v1/auth/login \
  -H 'Content-Type: application/json' \
  --data '{"email":"operator@example.com","password":"<password>"}'
```

Expected: `403 csrf_origin_mismatch`. The login route is short-circuited
by `CsrfGuard` BEFORE `Json<…>` parses the body. No `login_failed` audit
row is written. The throttle bucket is NOT touched.

### 10b. POST with bad `Origin` → 403

```sh
curl -i -X POST https://relay.example.com/api/v1/auth/login \
  -H 'Origin: https://attacker.example' \
  -H 'Content-Type: application/json' \
  --data '{"email":"operator@example.com","password":"<password>"}'
```

Expected: `403 csrf_origin_mismatch`. Body NEVER echoes the offered
Origin value. The operator-side `warn!` log line also redacts the
offered value.

### 10c. Protected route without cookie → 401

Already covered in § 5 — a `GET /api/v1/hosts` with no cookie returns
`401 unauthorized`. Any other protected route is equivalent
(`/auth/me`, `/auth/sessions`, `/server-profiles`, …).

### 10d. Login throttle: 6× wrong password → 429

Pick a real email AND a deliberately-wrong password. The bucket is
keyed on the **normalized** email, so case folds — `Operator@…` and
`operator@…` share the bucket.

> If you mistyped the password earlier in the smoke against this same
> email (e.g. during § 3 or § 7b), the bucket already holds entries and
> the `429` will arrive before call 6 — possibly on call 1 if you have
> already accumulated five failures within the 15-minute window. To
> guarantee the documented `401, 401, 401, 401, 401, 429` sequence,
> either restart the backend (clears in-memory throttle state) or pick
> an email that has not been used in the smoke yet.

```sh
for i in $(seq 1 6); do
  curl -s -o /dev/null -w "%{http_code}\n" \
    -X POST https://relay.example.com/api/v1/auth/login \
    -H 'Origin: https://relay.example.com' \
    -H 'Content-Type: application/json' \
    --data '{"email":"operator@example.com","password":"definitely-not-the-password-XYZ"}'
done
```

Expected: `401, 401, 401, 401, 401, 429`. The 6th call returns
`429 too_many_requests` with body
`{"error":{"code":"too_many_requests","message":"too many requests"}}`
and **no `Set-Cookie`** and **no `Retry-After` header**. A correct
password during the block continues to return `429`. The bucket clears
on success; restarting the backend also wipes in-memory state (the v1
throttler is local-process only).

After the throttle hits, recover by either waiting out the 15-minute
window OR restarting the backend. **A successful login after recovery
clears the bucket via `record_success`.**

### 10e. Bootstrap after first user → 409

```sh
curl -i -X POST https://relay.example.com/api/v1/auth/bootstrap \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --data '{
    "bootstrap_token": "<bootstrap_token>",
    "email":           "second@example.com",
    "display_name":    "Second",
    "password":        "<another-12+-char-password>"
  }'
```

Expected: `409 conflict { "entity": "user", "reason":
"already_bootstrapped" }`. Audited as `login_failed { method:
"bootstrap", reason: "already_bootstrapped" }` with `actor_id = NULL`.
This holds regardless of whether the bootstrap token is still
configured — the route refuses on the basis of "any password row exists".

### 10f. Bootstrap with no token configured → 503

After unsetting `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` and
restarting:

```sh
curl -i -X POST https://relay.example.com/api/v1/auth/bootstrap \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --data '{
    "bootstrap_token": "anything",
    "email":           "x@example.com",
    "display_name":    "X",
    "password":        "doesnt-matter-12+chars"
  }'
```

Expected: `503 service_unavailable` with message `bootstrap is disabled
(no first_user_bootstrap_token configured)`. **No audit row is written**
on this path — there is no token to compare against, so a `login_failed`
row would just be operator noise.

If the smoke runs against a fresh database AND the bootstrap token is
unset, the backend fails to start (see `docs/production-auth.md` § 7).
The 503 path is reachable only after the first user has been created
and the token has been removed; that is the documented end state.

---

## 11. Audit verification

The smoke writes a small, deterministic set of audit rows. Verify them
either through the SPA's "Recent activity" surfaces (see § 11a) OR
directly against the database (§ 11b).

### 11a. Recent-activity panels (SPA)

The SPA exposes the current-user audit feed in two places — neither is
an admin view. Both consume `GET /api/v1/audit-events/recent` (default
`?limit=20`, capped at 100) which returns ONLY the caller's rows
(`WHERE actor_id = $caller`). Pre-auth events
(`actor_id IS NULL`: failed logins, throttled logins, bootstrap probes)
are NEVER visible here by design.

1. **Dashboard.** Click `[data-testid="nav-dashboard"]`. The recent-activity
   card lives under `[data-testid="dashboard-recent-activity"]` and shows
   the most recent 5 rows. Each row carries `data-kind` matching the
   wire enum (`first_user_created`, `login_succeeded`, `logout_succeeded`,
   `password_changed`, `session_revoked`, `sessions_revoked`).
2. **Settings.** Click `[data-testid="nav-settings"]` then scroll to
   `[data-testid="settings-recent-activity"]`. Same data, fuller listing,
   per-section refresh button (`[data-testid="settings-recent-activity-refresh"]`).
   No auto-refresh, no polling.

After running this smoke as the operator user you should see (in
reverse chronological order):

- `logout_succeeded` (from § 9)
- `session_revoked` × N (from § 8b, 8d if exercised)
- `sessions_revoked` (from § 8c)
- `password_changed` (from § 7)
- `login_succeeded` × M (one per § 3, § 7b login)
- `first_user_created` (from § 2)

`login_failed` rows from § 7a, § 10d will NOT appear in either SPA panel
(they have `actor_id IS NULL`). Verify them via § 11b.

### 11b. Direct DB inspection (operator only)

Run this from a host that has `psql` access to the deployment Postgres.
The query is read-only.

```sh
psql "$DATABASE_URL" -c "
  SELECT
    occurred_at,
    kind,
    actor_id,
    payload
  FROM audit_events
  ORDER BY occurred_at DESC
  LIMIT 20;
"
```

Expected rows after a clean smoke run, newest-first:

| `kind` | `actor_id` | Payload (selected fields) |
|---|---|---|
| `logout_succeeded` | `<user_id>` | `{ "user_id": "<uuid>", "session_id": "<uuid>", "logout_at": "<rfc3339>" }` |
| `session_revoked` | `<user_id>` | `{ "session_id": "<uuid>", "current_session": true \| false, "revoked_at": "<rfc3339>" }` |
| `sessions_revoked` | `<user_id>` | `{ "revoked_count": <u64>, "revoked_at": "<rfc3339>" }` |
| `password_changed` | `<user_id>` | `{ "revoked_other_sessions": <u64>, "changed_at": "<rfc3339>" }` |
| `login_failed` | `NULL` | `{ "method": "password", "reason": "bad_credentials" }` (from § 7a) |
| `login_failed` | `NULL` | `{ "method": "password", "reason": "throttled" }` (from § 10d's 6th call) |
| `login_succeeded` | `<user_id>` | `{ "user_id": "<uuid>", "login_at": "<rfc3339>", "method": "password" }` |
| `first_user_created` | `<user_id>` | `{ "user_id": "<uuid>", "created_at": "<rfc3339>" }` |

Hard rules — every row MUST satisfy these. If any fails, the deployment
has a redaction regression and you should treat it as a security
incident:

- No row's `payload` contains the offered email (rejected emails) or
  the bootstrap-token bytes.
- No row's `payload` contains plaintext passwords, password hashes,
  session token bytes, session token hashes, the offered Origin value,
  or raw DB error text.
- No `login_failed` row carries an `actor_id` (it is always `NULL`).
- No `login_succeeded` / `first_user_created` / `password_changed` /
  `session_revoked` / `sessions_revoked` row has `actor_id IS NULL`.

The `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel test in
`crates/relayterm-api/tests/api.rs` is the executable backstop for the
redaction contract; this manual check is the deployment-time mirror.

---

## 12. Tear-down

After the smoke is complete:

- `rm /tmp/relayterm.cookies /tmp/relayterm.old.cookies
  /tmp/relayterm.new.cookies` — the jars hold live (or recently-revoked)
  session tokens.
- Wipe the shell history that contains the bootstrap token / passwords
  if they leaked into history (`history -c` for bash; `history clear`
  for fish).
- Confirm `auth.first_user_bootstrap_token` is unset on the running
  backend (`docs/production-auth.md` § 4 step 8).

The deployment is now in its steady-state production posture.

---

## See also

- `SPEC.md` → "Production authentication architecture" — normative spec.
- `docs/production-auth.md` — deployment / configuration guide.
- `apps/web/e2e/SMOKE.md` — the full SPA smoke procedure (this guide
  references its auth-relevant subset; the SMOKE.md procedure also
  covers the dev renderer lab and the production app shell).
- `crates/relayterm-api/tests/api.rs` — executable contract for every
  expectation in this guide.
