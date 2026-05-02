# RelayTerm production auth deployment guide

This guide explains how to run the RelayTerm backend with `auth.mode = production` behind a real HTTPS reverse proxy. It covers required configuration, secret generation, the first-user bootstrap flow, smoke commands, startup failure modes, and recovery paths.

The canonical specification for the auth model is `SPEC.md` → "Production authentication architecture". The corresponding code surface lives in `apps/backend/src/config.rs` (`Config::validate_auth`), `apps/backend/src/main.rs` (the post-DB first-user gate), and `crates/relayterm-api/src/auth/` + `crates/relayterm-api/src/routes/v1/auth.rs` (cookie, CSRF, and the four `/api/v1/auth/*` routes).

If anything in this guide drifts from the code, the code wins. File a doc fix.

---

## 1. Overview

- Authentication is **cookie-backed**. The backend issues a server-side opaque session token, persists its SHA-256 hash in `user_sessions`, and binds the plaintext to an `HttpOnly; SameSite=Strict; Secure` cookie named `relayterm_session`. There is no JWT.
- The legacy `DevUser` runtime bypass is gone. Every protected `/api/v1/*` route — HTTP and the terminal WebSocket — runs through the same `AuthenticatedUser` extractor in **both** `auth.mode = dev` and `auth.mode = production`. A request without a valid `relayterm_session` cookie returns `401 unauthorized`.
- `auth.mode = production` is **opt-in** and **fail-fast**. The backend refuses to start unless every required field is set; misconfiguration never opens a socket.
- Browser-write routes additionally enforce a CSRF / `Origin`-header allow-list. A POST/PATCH/DELETE without a matching `Origin` returns `403 csrf_origin_mismatch` *before* the request body is parsed.
- RelayTerm assumes it sits behind a TLS-terminating reverse proxy (Traefik, nginx, Caddy, …) on production deployments. The backend speaks plain HTTP on `127.0.0.1:8080` by default; the proxy is responsible for HTTPS.

What is **not** covered by v1 production auth and is documented as deferred work in §8 below: login throttling, password reset, passkeys/WebAuthn, a session-management UI, the `last_seen_at` touch on the auth extractor, and admin/RBAC tooling.

---

## 2. Required production configuration

All keys are nested under `auth`. Environment variables use the `RELAYTERM_AUTH__*` convention with double-underscore as the nesting separator (this is the same convention every other RelayTerm config field uses; see `apps/backend/src/config.rs`).

Each row below shows the env var first and the equivalent TOML key second. The TOML form lives in `config/relayterm.toml` (or whatever path you point `RELAYTERM_CONFIG` at). Env vars override TOML values; later wins.

| Env var | TOML key | Required? | Notes |
|---|---|---|---|
| `RELAYTERM_AUTH__MODE` | `auth.mode` | yes | Must be `production`. |
| `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64` | `auth.session_signing_key_b64` | one of the two | 32 random bytes, standard base64. Mutually exclusive with `…_FILE`. |
| `RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE` | `auth.session_signing_key_file` | one of the two | Path to a file holding the signing key. The validator only checks that the configuration *field* is set (`is_some()`); the file itself is never opened or stat-checked at boot, because the v1 hashed-opaque-token session model does not yet consume the key. A bogus path will boot fine today — the validation will land alongside the signed-CSRF / signed-cookie code that first reads the file. Mutually exclusive with `…_B64`. |
| `RELAYTERM_AUTH__ALLOWED_ORIGINS` | `auth.allowed_origins` | yes | Comma-separated list (env) or array (TOML). Each entry is an exact `scheme://host[:port]` string. Empty rejects every browser-write. |
| `RELAYTERM_AUTH__COOKIE_SECURE` | `auth.cookie_secure` | yes | Must be `true` in production. The `Secure` flag is non-negotiable — there is no escape hatch. |
| `RELAYTERM_AUTH__COOKIE_DOMAIN` | `auth.cookie_domain` | optional | Omit for a host-only cookie (the recommended default). Set only if you need to share the session across subdomains and you understand the implications. |
| `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` | `auth.first_user_bootstrap_token` | conditional | Required at startup **iff** no row exists in `user_passwords` (i.e. no user has ever finished the bootstrap flow). After the first user is created, unset this and restart. |

Companion (non-auth) keys you almost always need to set on a real deploy:

| Env var | TOML key | Notes |
|---|---|---|
| `RELAYTERM_DATABASE__URL` *or* `DATABASE_URL` | `database.url` | Postgres connection string. The password segment is masked in `Debug` logs but is still on disk in your config — protect it. The env loader applies `RELAYTERM_DATABASE__URL` first and `DATABASE_URL` second; if both are set, **`DATABASE_URL` wins** (it is honoured for `sqlx-cli` parity). |
| `RELAYTERM_SERVER__BIND` | `server.bind` | Default `127.0.0.1:8080`. Behind a reverse proxy, keep this on loopback. |
| `RELAYTERM_VAULT__MASTER_KEY_B64` *or* `RELAYTERM_VAULT__MASTER_KEY_FILE` | `vault.master_key_b64` / `vault.master_key_file` | Vault master key (separate from the session signing key). Required unless you set `vault.enabled = false`, in which case `POST /api/v1/ssh-identities` returns 503 until you wire one up.

Behavioural rules baked into the boot-time validator:

- **Exactly one** session signing key source. Setting both is rejected at startup as ambiguous; setting neither is rejected as missing.
- **`allowed_origins` must match the browser's `Origin` byte-for-byte.** That means `scheme + host + port` only, with no trailing slash and no path. The CSRF guard does not normalise — comparison is case-sensitive byte equality. Browsers serialise the scheme and host of the `Origin` header in lower-case, so configure your allow-list entries in the lower-case form the browser actually sends. `https://relay.example.com` and `https://relay.example.com/` are not the same value to the guard.
- **Empty `allowed_origins` is a hard boot failure in production.** It would also reject every browser-write at the CSRF guard, but failing fast at startup gives a clearer operator signal than every POST returning 403.
- The session-signing key is currently *reserved* — it is required at boot but not consumed by the v1 hashed-opaque-token session model. Pinning the requirement now reserves the operational discipline (rotation, redaction, file-vs-env sourcing) for the signed-CSRF / signed-cookie variants that come later.

A worked configuration template ships at `docs/config-examples/relayterm.production.example.toml`; copy it, fill in the values, and put real paths or env-supplied values in for every secret.

---

## 3. Generating secrets

Generate every secret on a trusted machine. Never commit these values, never put them in shell history, never log them.

**Session signing key (32 bytes, base64):**

```sh
openssl rand -base64 32
```

Place the result either in `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64` or write it to a file readable only by the backend's UNIX user (`chmod 0400`) and point `RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE` at that path.

**Vault master key (32 bytes, base64):**

```sh
openssl rand -base64 32
```

Same handling as the session signing key. Use a *different* value — they are unrelated secrets.

**First-user bootstrap token:**

Any high-entropy random string ≤ 4096 bytes works; the comparison is constant-time. A 32-byte URL-safe random value is plenty:

```sh
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

Rules:

- Never commit any of these values to source control.
- Never put them in `tracing` log lines, panic messages, or audit payloads. The relevant `Debug` impls already redact them to `_set: bool` markers; do not work around the redaction.
- After the first user is created, **rotate (or simply unset) the bootstrap token and restart**. The route is one-shot — once any password row exists in `user_passwords`, every `/auth/bootstrap` call returns `409 already_bootstrapped` regardless of whether the token is still configured. The leftover token is only useful to an attacker who wants to log a confusing audit row, but rotating it is the operational hygiene baseline.

---

## 4. First-user bootstrap flow

There is no "default user". On a fresh database the only path to mint the first account is `POST /api/v1/auth/bootstrap` with the configured bootstrap token.

The order of operations:

1. Configure the production env (signing key, allowed origins, `cookie_secure = true`, bootstrap token, vault master key, database URL, …).
2. Start the backend. It connects to Postgres, runs migrations, and listens.
3. Call `POST /api/v1/auth/bootstrap` exactly once with the bootstrap token, your operator email, display name, and password. The response is `201 Created` with the user record. **Bootstrap does not log you in.**
4. Call `POST /api/v1/auth/login` with your email + password. The response is `200 OK` with the user record and a `Set-Cookie: relayterm_session=…; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000; Secure` header.
5. Call `GET /api/v1/auth/me` with the cookie to confirm.
6. Call `POST /api/v1/auth/logout` to revoke the session and clear the cookie.
7. **Unset `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN` and restart the backend.** Subsequent bootstrap calls return `409 already_bootstrapped` whether the token is set or not, but unsetting it is good hygiene.

`curl` smoke commands. Replace `https://relay.example.com` with your own origin, and replace the placeholders with real values. Every POST sends an `Origin` header because the CSRF guard requires one — this is what your browser does automatically; `curl` does not, so you must add it.

```sh
# 1. Bootstrap the first user. No cookie yet.
curl -fsS -X POST https://relay.example.com/api/v1/auth/bootstrap \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --data '{
    "bootstrap_token": "<paste the configured token here>",
    "email":           "operator@example.com",
    "display_name":    "Operator",
    "password":        "<at-least-12-character-password>"
  }'

# 2. Log in. Cookie jar captures the Set-Cookie response.
curl -fsS -X POST https://relay.example.com/api/v1/auth/login \
  -H 'Origin: https://relay.example.com' \
  -H 'Content-Type: application/json' \
  --cookie-jar  /tmp/relayterm.cookies \
  --data '{
    "email":    "operator@example.com",
    "password": "<the-password-you-just-set>"
  }'

# 3. Verify the session cookie.
curl -fsS https://relay.example.com/api/v1/auth/me \
  --cookie /tmp/relayterm.cookies

# 4. Log out (revokes the session, clears the cookie).
curl -fsS -X POST https://relay.example.com/api/v1/auth/logout \
  -H 'Origin: https://relay.example.com' \
  --cookie     /tmp/relayterm.cookies \
  --cookie-jar /tmp/relayterm.cookies
```

Notes:

- `bootstrap` does **not** mint a session. You must follow up with `login` to receive a cookie. This is deliberate — bootstrap is the "first-user creation" route, login is the session-mint route, and folding them together would split the audit trail (`first_user_created` + `login_succeeded` are two distinct rows).
- `me`, `login`, and `logout` all use the same cookie. `--cookie-jar` writes the response cookies; `--cookie` sends them. Logout writes a `Max-Age=0` clearing cookie back into the jar so the next call has no live session.
- Failed bootstrap (bad token, already-bootstrapped) returns `401` or `409`, never echoes the offered token, and writes a `login_failed` audit row with `actor_id = NULL`. The "bootstrap disabled" path (`503`, no `first_user_bootstrap_token` configured) is the deliberate exception — it writes no audit row, because there is no token to compare against and a `login_failed` row would just be operator noise. Failed login collapses unknown-email and bad-password to the same `401 invalid credentials` shape.
- `403 csrf_origin_mismatch` on a POST means your `Origin` header is missing, malformed, or not in `auth.allowed_origins`. The wire body never echoes the offered value; check the request and the configured allow-list yourself.

---

## 5. Reverse proxy / HTTPS notes

The backend is HTTPS-agnostic. It binds plain HTTP on `127.0.0.1:8080` by default and trusts the reverse proxy to terminate TLS. The session cookie still needs `Secure` set in production because **the browser sees HTTPS even when the proxy talks HTTP to the backend** — the `Secure` flag is about the browser-side scheme, not the upstream-side scheme.

Concretely:

- Set `RELAYTERM_AUTH__COOKIE_SECURE=true`. The cookie writer in `routes::v1::auth` appends `; Secure` to the `Set-Cookie` value. Browsers will then refuse to send the cookie over plain HTTP on subsequent requests, which is what you want.
- Set `RELAYTERM_AUTH__ALLOWED_ORIGINS` to your **public HTTPS origin**, exactly as the browser will send it. For a deployment served at `https://relay.example.com`, the value is `https://relay.example.com` — no trailing slash, no path, no port unless you serve on a non-default port. Multiple values are comma-separated (env) or an array (TOML).
- The `Origin` allow-list is **not CORS**. CORS controls cross-origin reads via `Access-Control-*` headers; the RelayTerm CSRF guard rejects browser-write requests whose `Origin` is not on the allow-list, regardless of CORS. RelayTerm v1 does not configure CORS — the SPA is served from the same origin as the API, and cross-origin browser writes are intentionally unsupported.
- Path prefixes (e.g. mounting RelayTerm at `https://example.com/relay/`) are **not currently documented or supported**. The backend mounts every route at the bare path (`/api/v1/auth/login`, not `/relay/api/v1/auth/login`) and the cookie's `Path=/` reflects that. If you need a path prefix, strip it at the proxy.
- Behind Traefik specifically, the only TLS setup that matters for auth is "browsers see HTTPS". A minimal label sketch (your real config will have more — TLS resolver, entrypoints, …):

  ```yaml
  # deploy/docker-compose.yml fragment — sketch only
  services:
    backend:
      labels:
        - "traefik.enable=true"
        - "traefik.http.routers.relayterm.rule=Host(`relay.example.com`)"
        - "traefik.http.routers.relayterm.entrypoints=websecure"
        - "traefik.http.routers.relayterm.tls=true"
        - "traefik.http.services.relayterm.loadbalancer.server.port=8080"
  ```

  The sample `deploy/docker-compose.yml` in this repo only ships Postgres today; the backend service block is left for the operator to wire up to their existing Traefik setup.

`X-Forwarded-*` headers are not currently consumed by RelayTerm — `audit_events.remote_addr` is `None` on every row in v1. If you wire up a custom forwarded-IP scheme later, do it in a single boundary middleware; do not let individual handlers re-parse headers.

---

## 6. Local development mode

`auth.mode = dev` is the default. It is the **same code path** as production — the same `AuthenticatedUser` extractor, the same `/api/v1/auth/*` routes, the same cookie semantics. Only the boot-time validation envelope is relaxed:

- `auth.session_signing_key_b64` / `…_file` may be unset.
- `auth.allowed_origins` may be empty (but every browser-write will then return 403 — populate it explicitly to actually serve writes).
- `auth.cookie_secure` may be `false` (so the cookie works over plain `http://localhost`).

Recommended dev config (env or TOML):

```sh
export RELAYTERM_AUTH__MODE=dev
export RELAYTERM_AUTH__COOKIE_SECURE=false
export RELAYTERM_AUTH__ALLOWED_ORIGINS='http://localhost:5173,http://127.0.0.1:5173'
export RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN='<dev-token>'
```

Background:

- The Vite dev server runs on port 5173 (`apps/web/vite.config.ts`) and proxies `/api` and `/ws` to the backend on `127.0.0.1:8080`. Because the proxy preserves the browser's origin, the backend sees `Origin: http://localhost:5173` (or `http://127.0.0.1:5173`, depending on which URL you typed). Both forms must be on the allow-list if you want to use either.
- The legacy `dev@relayterm.local` fixture user is gone. Dev mode no longer auto-creates a user. The first time you boot dev mode against an empty database, you bootstrap exactly the same way as production: configure `auth.first_user_bootstrap_token`, call `POST /api/v1/auth/bootstrap`, then log in.
- The legacy `RELAYTERM_DEV_AUTH__ENABLED` env var and `[dev_auth]` TOML section are silently ignored. An operator with stale config does not see a hard load failure — but the values are no-ops.
- `auth.mode` does NOT change handler behaviour. There is no "skip the auth check" toggle, anywhere. If a future PR ever introduces one, it is a regression. (The same rule is pinned in `AGENTS.md` "Things to avoid".)

A worked dev configuration template ships at `docs/config-examples/relayterm.dev.example.toml`.

---

## 7. Startup failure modes

The boot-time validator (`Config::validate_auth`) is fail-fast. The backend never opens its listener with a half-valid auth posture. Each failure below is a `bail!` from `apps/backend/src/main.rs` or `apps/backend/src/config.rs`; the wire-side symptom is "process exits during startup with an `Err(...)`" and a single descriptive log line.

| Error message contains | Cause | Fix |
|---|---|---|
| `auth.mode = production requires a session signing key` | Neither `…SESSION_SIGNING_KEY_B64` nor `…_FILE` is set in production. | Generate a 32-byte base64 key (§3) and set exactly one of the two. |
| `session_signing_key_b64 and auth.session_signing_key_file are both set` | Both signing-key sources resolved at boot. | Pick one and unset the other. The validator refuses to guess which is canonical. |
| `auth.mode = production requires auth.allowed_origins to list at least one origin` | `auth.allowed_origins` is empty in production. | Set to your public HTTPS origin (e.g. `RELAYTERM_AUTH__ALLOWED_ORIGINS=https://relay.example.com`). |
| `auth.mode = production requires auth.cookie_secure = true` | `cookie_secure` is false in production. | Set `RELAYTERM_AUTH__COOKIE_SECURE=true`. There is no escape hatch — production cookies must carry the `Secure` flag. |
| `auth.mode = production with no existing user requires auth.first_user_bootstrap_token` | Production mode + an empty `user_passwords` table + no bootstrap token. The operator has no path to create a first user. | Generate a bootstrap token (§3), set `RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN`, restart, complete the bootstrap flow (§4), then unset the token. |
| `unrecognized auth.mode value` (config load) | A typo'd `auth.mode` (e.g. `prod`, `Production` (TOML), `live`). Accepted values are `dev` and `production`, case-sensitive. | Fix the value. |
| `vault.master_key_b64 invalid` / `vault.master_key_file` errors | Vault master key source set but unreadable / not 32 bytes after decoding. | Re-generate with `openssl rand -base64 32`, or set `vault.enabled = false` to opt out (POST `/api/v1/ssh-identities` then returns 503 until you wire one up). |

Per-request failure modes that show up *after* boot:

| Wire response | Cause | Operator action |
|---|---|---|
| `403 csrf_origin_mismatch` on a POST/PATCH/DELETE | Missing, malformed, or non-allowlisted `Origin` header. The body wire envelope never echoes the offered value. | Verify the request actually carried the `Origin` header (browser does this automatically; `curl` needs `-H 'Origin: …'`) and that the value is in `auth.allowed_origins` byte-for-byte. |
| `401 unauthorized` on a protected route | No cookie, or the cookie's session is expired / revoked / unknown. The same body shape covers all four cases — the operator-side detail (`missing cookie` vs `session invalid` vs `session expired`) lives in the `warn!` line in `crates/relayterm-api/src/error.rs::IntoResponse`. | Log in again. Sessions hard-expire 30 days after `created_at` (`SESSION_TTL` in `routes/v1/auth.rs`); there is no sliding window in v1. |
| `503 service_unavailable` on `POST /api/v1/auth/bootstrap` | `auth.first_user_bootstrap_token` is unset. The route is disabled. | Either configure the token (if you actually need to bootstrap) or accept that bootstrap is closed. |
| `409 conflict` with `reason: "already_bootstrapped"` | A user with a password row already exists. Bootstrap is one-shot. | Use `POST /api/v1/auth/login` to sign in instead. |

Recovery for the "I locked myself out" cases:

- **Lost the password and there's no other user.** v1 has no email-based password reset (deferred — see SPEC.md "Out of scope (v1)"). Direct DB recourse: connect to Postgres as the DB superuser and either delete the `user_passwords` row for the affected user (so they can re-bootstrap if they're the only user) or run a one-off Argon2id hash and `UPDATE user_passwords SET hash = '$argon2id$…' WHERE user_id = …;`. The schema-level invariants (no admin role, single user can re-bootstrap) keep this path simple.
- **Forgot to unset the bootstrap token.** It is harmless after the first user exists — every call returns `409`. Unset it on the next deploy.
- **Set the wrong allow-list.** Update `RELAYTERM_AUTH__ALLOWED_ORIGINS` and restart. The list is read once at boot.
- **Cookie domain accidentally too wide.** Update `RELAYTERM_AUTH__COOKIE_DOMAIN`, restart. Existing sessions remain valid in the database; the next login mints a cookie with the corrected domain. If you need to mass-invalidate, `DELETE FROM user_sessions;` is safe — every browser sees `401` on the next request and is forced to log in again.

---

## 8. Security caveats and remaining work

Production auth is the floor, not the ceiling. The deliberate v1 cuts:

- **No login throttling.** The plan in SPEC.md "Password authentication (v1) → Throttling" is per-`(remote_addr, email)` plus per-`email` leaky-buckets. Until that ships, do **not** expose the bare backend to the open internet. Sit behind a TLS-terminating reverse proxy that can rate-limit by IP (Traefik middleware, nginx `limit_req`, Cloudflare, …) or restrict access to a VPN / WireGuard mesh.
- **No password reset / "forgot password" flow.** Recovery is DB-level today (§7).
- **No passkeys / WebAuthn.** The session shape is forward-compatible with passkey login, but the registration and authentication routes are deferred.
- **No session-management UI.** `user_sessions` is queryable, but there is no per-session revoke surface. `POST /api/v1/auth/logout` revokes the *current* session only.
- **No `last_seen_at` touch on the auth extractor.** `users.last_login_at` is updated on each login, but a session that has been idle for 29 days still looks "fresh" in the UI until login or expiry.
- **No admin / RBAC model.** RelayTerm is single-user / self-hosted in v1; there is no concept of an admin user, an operator role, or per-user permissions. The first user owns everything.

What this means operationally:

- Treat the deployment as you would a single-user SSH bastion: behind a trusted reverse proxy, behind a VPN if exposed off-LAN, with off-band rate limiting until throttling lands. The ops surface is small on purpose; it does not yet have the in-app defences a public-internet auth surface needs.
- The redaction discipline is load-bearing. Plaintext passwords, password hashes, session tokens, token hashes, bootstrap tokens, raw audit blobs, peer banners, raw DB errors, and terminal I/O **must not** appear in frontend responses, public errors, log lines, `Debug` output, serde output, or audit payloads. The `AuthConfig` / `AuthRoutesConfig` `Debug` impls render secret-shaped fields as boolean presence markers (`session_signing_key_b64_set: true`, `first_user_bootstrap_token_set: true`, …) so a `tracing` log at debug level cannot leak the value. The `AUDIT_FORBIDDEN_SUBSTRINGS` sentinel test in `crates/relayterm-api/tests/api.rs` plus the `Debug` redaction tests are the backstop. If you find a leak, treat it as a security regression.

---

## 9. Verifying a deploy

After standing up production auth, verify:

1. **The backend started cleanly.** No `bail!` lines in the log; the listener bound; the auth-mode line shows `auth_mode = production`.
2. **`POST /api/v1/auth/bootstrap`** with the configured token returns `201` and creates the first user.
3. **`POST /api/v1/auth/login`** returns `200` with a `Set-Cookie: relayterm_session=…` header that includes `HttpOnly`, `SameSite=Strict`, `Secure`, and the expected `Domain` (omitted by default).
4. **`GET /api/v1/auth/me`** with the cookie returns the user record.
5. **`POST /api/v1/auth/logout`** with the cookie returns `204` and a `Max-Age=0` clearing cookie. A subsequent `me` call returns `401`.
6. **`POST /api/v1/auth/login` without `Origin`** returns `403 csrf_origin_mismatch`.
7. **Any protected route without a cookie** returns `401 unauthorized`.
8. **The bootstrap token has been unset and the backend restarted.** A subsequent `POST /api/v1/auth/bootstrap` then returns `503 service_unavailable` ("bootstrap is disabled" — the route key is gone). If you skipped the unset-and-restart step, the response is `409 already_bootstrapped` instead, which is also fine; the 503 is just the cleaner end state.

If any step fails, cross-reference §7 (startup failure modes) and the relevant integration test in `crates/relayterm-api/tests/api.rs`. The integration tests are the executable spec for these contracts; if behaviour drifts from this guide, the tests pin the truth.

---

## See also

- `SPEC.md` → "Production authentication architecture" — normative spec.
- `AGENTS.md` → "Decision tables" rows on `AuthenticatedUser`, `CsrfGuard`, and `SessionToken` — load-bearing rules for any future auth slice.
- `apps/backend/src/config.rs` — the validator and the redaction posture.
- `crates/relayterm-api/src/auth/` — extractors, cookie helper, CSRF guard.
- `crates/relayterm-api/src/routes/v1/auth.rs` — the four `/api/v1/auth/*` routes.
- `docs/config-examples/relayterm.production.example.toml` — production TOML template.
- `docs/config-examples/relayterm.dev.example.toml` — dev TOML template.
