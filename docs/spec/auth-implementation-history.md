# SPEC — Authentication implementation history

> Landed-slice implementation narrative split out of
> [`docs/spec/auth.md`](auth.md) on 2026-05-07 to keep that file
> contract-focused. Nothing here is normative on its own — the contract
> auth code MUST satisfy lives in `auth.md`, the operator deployment
> guide lives in [`../production-auth.md`](../production-auth.md), and
> the deploy-time smoke procedure lives in
> [`../auth-smoke.md`](../auth-smoke.md). This file documents what
> shipped per slice, in the order the slices landed, so a reviewer asking
> "when did `password_changed` land? what migration paired with it?" has
> a single place to look without trawling git log.
>
> Drift policy: when a new auth slice lands, append the slice's status
> paragraph to the matching § below (or add a new §). Do NOT use this
> file as the source of truth for any wire shape, table column, audit
> payload field, or boundary rule — those live in `auth.md` and are
> enforced by the integration tests in `crates/relayterm-api/tests/api.rs`.

## Status snapshot (current)

Real cookie-backed authentication is wired up across every protected
`/api/v1/*` route AND `auth.mode = production` boots cleanly when the
configuration envelope is satisfied (signing key, non-empty
`allowed_origins`, `cookie_secure = true`). The legacy `DevUser`
extractor, the `AppState::dev_user_id` field, the `DevAuthConfig`
struct, and the `dev@relayterm.local` startup bootstrap are gone — both
modes now run the same real-auth code path; only the boot-time
validation envelope differs. The current-user session-management
foundation (`GET /api/v1/auth/sessions`, single-row revoke,
revoke-all-except-current) is live with a Settings-panel UI, and
current-user password change (`POST /api/v1/auth/change-password`) is
live with its own Settings panel. Login throttling is in place
(email-keyed in-memory `LoginThrottler`); IP-aware / distributed
throttling is still deferred. Passkeys / WebAuthn, password reset, and
admin / RBAC are deferred — see auth.md "Out of scope (v1)".

## Implementation order — per-step status

The contract for each step lives in `auth.md` under "Implementation
order"; this section is the long landed-slice report.

### Step 1 — Auth-mode + config plumbing (✅ landed)

`apps/backend/src/config.rs` carries `AuthConfig { mode,
session_signing_key_b64, session_signing_key_file,
first_user_bootstrap_token, cookie_secure, cookie_domain,
allowed_origins }` with the same `Debug`-redaction posture as
`VaultConfig` / `FileVaultConfig` (the secret-shaped fields render as
`_set: bool` markers; `FileAuthConfig` mirrors the redaction so the
deserialized intermediate cannot re-introduce the leak).
`Config::validate_auth` runs in `apps/backend/src/main.rs` BEFORE any
irreversible work (db connect, ssh services, listener bind). Policy:
`auth.mode = dev` → permissive (insecure cookies, missing signing key,
empty allow-list all accepted); `auth.mode = production` → enforce
exactly-one-signing-key-source, non-empty `allowed_origins`,
`cookie_secure = true`. After the DB connect, `main.rs` adds a runtime
gate: production with no first user AND no `first_user_bootstrap_token`
→ `bail!`. The default mode is `dev`. Reserved keys are read from
`RELAYTERM_AUTH__MODE`, `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64`,
`RELAYTERM_AUTH__SESSION_SIGNING_KEY_FILE`,
`RELAYTERM_AUTH__FIRST_USER_BOOTSTRAP_TOKEN`,
`RELAYTERM_AUTH__COOKIE_SECURE`, `RELAYTERM_AUTH__COOKIE_DOMAIN`, and
`RELAYTERM_AUTH__ALLOWED_ORIGINS` (comma-separated). The `[auth]` TOML
section mirrors the same names. Unknown `auth.mode` values are rejected
at parse time (TOML via serde rename, env via `AuthMode::from_str`) —
never silently coerced. The legacy `RELAYTERM_DEV_AUTH__ENABLED` env var
and `[dev_auth]` TOML section are silently ignored (legacy operator
config does not block a load). Property-1 tests landed in the same
module as the existing vault redaction tests and pin every
production-mode failure mode (missing key, both keys, empty allow-list,
cookie_secure=false) plus the redaction posture across all four error
paths.

### Step 2 — Schema migrations + repositories foundation (✅ landed, partial)

Two migrations are in place: `20260501000013_user_passwords.sql` and
`20260501000014_user_sessions.sql`. The audit-kind extension
(`first_user_created`, `password_changed`, `session_revoked`) is paired
with step 4 (the route slice that emits them) so the migration and the
emitter ship together; until then the kinds were documented but not in
the CHECK constraint. `relayterm-core` carries `PasswordCredential`,
`UserSession`, the `UserSessionId` newtype, the
`CreatePasswordCredential` / `CreateUserSession` inputs, and the
`PasswordCredentialRepository` / `UserSessionRepository` traits.
`relayterm-db` provides `PgPasswordCredentialRepository` and
`PgUserSessionRepository`, both reachable via `Db::password_credentials()`
/ `Db::user_sessions()`. No routes, no extractor, no auth-service
wrapper yet — the schema is reachable only through the repositories.

- **`user_passwords` columns.** `user_id UUID PK REFERENCES users(id) ON
  DELETE CASCADE` (the original sketch's choice; an orphan password row
  would never be reachable, so cascade is the only sensible behavior);
  `password_hash TEXT NOT NULL` (Argon2id PHC string — the algorithm and
  parameters are encoded in the string itself, which is why no separate
  `algo_version` column was added); `password_changed_at TIMESTAMPTZ NOT
  NULL DEFAULT NOW()`; `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`;
  `updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`. Upsert (the only
  mutation) overwrites `password_hash` and bumps `updated_at` and
  `password_changed_at`; `created_at` is preserved across upserts. A
  future re-hash-on-parameter-upgrade flow uses the same
  `upsert_for_user` call site — bumping `password_changed_at` on every
  re-hash is acceptable because the audit `password_changed` event is
  what carries semantic intent; the timestamp is metadata.
- **`user_sessions` columns.** `id UUID PK` (NOT the cookie value — the
  stable session identifier referenced by `logout_succeeded.session_id`
  and `session_revoked.revoked_session_id` audit payloads); `user_id
  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE` (orphan sessions
  after a user delete would be a logout-bypass); `token_hash BYTEA NOT
  NULL UNIQUE` (SHA-256 digest of the random cookie token); `created_at
  TIMESTAMPTZ NOT NULL DEFAULT NOW()`; `last_seen_at TIMESTAMPTZ NOT
  NULL DEFAULT NOW()`; `expires_at TIMESTAMPTZ NOT NULL`; `revoked_at
  TIMESTAMPTZ NULL`; `revoked_reason TEXT NULL` (a short free-form code
  such as `"logout"` / `"admin_revoke"`; display metadata only, never an
  auth input). Indexes: the unique on `token_hash` (constraint name
  `user_sessions_token_hash_key`), one on `user_id` (for revoke-all-for-
  user and the future active-sessions list), one on `expires_at` (for
  the future sweeper). `remote_addr` and `user_agent` are intentionally
  deferred — the active-sessions list is the only consumer and that
  surface lands later. Adding the columns now without a writer would
  normalize empty / NULL display metadata into the rows from day one and
  lock that shape in.
- **Repository contract: passwords.** `upsert_for_user(input)` is the
  only mutation; `get_for_user(user_id)` returns the row or `None`.
  There is no `delete_for_user` — a password row's lifecycle is tied to
  its user via `ON DELETE CASCADE`, and a "remove password without
  deleting the user" surface does not exist in v1. A foreign-key failure
  (no matching `users.id`) is mapped to `RepositoryError::Database` via
  the underlying constraint — the auth service is the only caller and is
  expected to ensure the user row exists first.
- **Repository contract: sessions.** `create(input)` inserts a fresh row
  (duplicate `token_hash` → `RepositoryError::Conflict { entity:
  "user_session", constraint: "user_sessions_token_hash_key" }`; the
  constraint name never echoes the digest bytes). `get_by_token_hash(&
  [u8])` is the only auth-extractor lookup; `get(id)` is for management
  code that already knows the row. Neither method filters on
  `revoked_at` / `expires_at` — the auth service / extractor is the
  single place that enforces the policy, so the SQL stays trivial and
  there is no second source of truth to drift. `touch_last_seen(id, at)`
  updates the timestamp; `revoke(id, at, reason)` is idempotent (a
  second call against an already-revoked row preserves the original
  `revoked_at` and `revoked_reason` so the audit trail remains honest);
  `revoke_all_for_user(user_id, at, reason)` returns the number of rows
  transitioned from non-revoked to revoked so the caller can decide
  whether to write any audit events. An unknown id on `touch_last_seen`
  / `revoke` returns `RepositoryError::NotFound { entity: "user_session"
  }`; an unknown user on `revoke_all_for_user` returns `0`.
- **Redaction contract (load-bearing — sentinel-tested).**
  `PasswordCredential::Debug` redacts `password_hash` to `<redacted: N
  chars>`. `UserSession::Debug` redacts `token_hash` to `<redacted: N
  bytes>`. `CreatePasswordCredential::Debug` redacts `password_hash`.
  `CreateUserSession::Debug` redacts `token_hash`. The private SQLx row
  structs (`PasswordCredentialRow`, `UserSessionRow`) deliberately do
  NOT derive `Debug` — the redacting domain types are the only thing
  reachable to a formatter outside the row module.
  `RepositoryError::Conflict` strings carry the schema constraint name
  only (no digest, no hash, no user input, no SQL fragment); the
  existing sentinel-string tests at the route layer
  (`AUDIT_FORBIDDEN_SUBSTRINGS` in `crates/relayterm-api/tests/api.rs`)
  extend to cover `password_hash`, `argon2id`, `session_token`, and
  `bootstrap_token` once step 4 lands and these substrings are reachable
  through a route.
- **What plaintext NEVER reaches this layer.** Plaintext passwords are
  not modeled at the domain or repository level — the auth service
  hashes them before constructing `CreatePasswordCredential`. Plaintext
  cookie tokens are not modeled here either — the auth service generates
  the token, SHA-256-hashes it, and passes only the digest as
  `CreateUserSession::token_hash`. Any future caller that inverts this —
  e.g. a "store the password and hash it on the way out" helper — is a
  spec bug, not a refactor. There are no API surfaces that read or
  return password material or session-token material from this layer.

`cargo sqlx prepare --workspace` is intentionally NOT required by this
slice: the project uses the runtime SQLx API (`sqlx::query` /
`sqlx::query_as::<_, RowType>`) rather than the compile-time-checked
macros, as documented in `crates/relayterm-db/src/lib.rs`. Step 4 (the
route slice) decides whether to migrate hot queries to the macros at the
same time it adds them.

### Step 3 — Auth service primitives (✅ landed, no routes)

The repository traits and sqlx impls
(`relayterm-core::repository::{PasswordCredentialRepository,
UserSessionRepository}` + `relayterm-db::{PgPasswordCredentialRepository,
PgUserSessionRepository}`) already landed in step 2. This step adds
`relayterm-auth::AuthService` plus the password and session-token
primitives it composes; no HTTP routes, cookie wiring, CSRF middleware,
or extractor changes shipped in this slice (those landed in steps 4–6).

- **`relayterm-auth::password`.** `PasswordHasher` wraps `argon2 =
  "0.5"` (Argon2id). Default parameters are
  `PasswordHasherConfig::OWASP_2023` (`m=19456 KiB`, `t=2`, `p=1`) — the
  OWASP 2023 baseline; tests pin the constant so a future PR cannot
  silently weaken it. `hash_password(&str) -> Result<String,
  PasswordHashingError>` produces a fresh-salt `$argon2id$...` PHC
  string; `verify_password(&str, &str) -> Result<bool,
  PasswordHashingError>` returns `Ok(true)` on match, `Ok(false)` on a
  structurally-valid wrong-password verify, and `Err(InvalidStoredHash)`
  only when the stored value is not a PHC string at all (the service
  collapses this to `InvalidCredentials` so a probe cannot distinguish
  "your password is wrong" from "the row is corrupt"). `PasswordHasher`,
  `PasswordHasherConfig`, and `PasswordHashingError` all redact in
  `Debug` (parameter numerics never appear in formatter output, and a
  malformed-hash error never echoes either input). Tests injected a
  tuned-down hasher (`t=1`) so the suite runs in well under a second;
  production callers use `PasswordHasher::default()`.
- **`relayterm-auth::session_token`.** `SessionToken::generate()` reads
  32 bytes from `OsRng` and URL-safe-base64-encodes them with no
  padding — the resulting cookie value is exactly 43 ASCII characters
  from the URL-safe alphabet (`A-Za-z0-9-_`). `SessionToken` exposes the
  encoded bytes only via `expose() -> &str` (the single legitimate
  caller is the `Set-Cookie` writer in step 4); there is no `Display`,
  no `serde`, and `Debug` redacts to `<redacted: N chars>`. The wrapper
  zeroizes on drop. `hash_session_token(&str) -> SessionTokenHash` is a
  free function so the auth extractor can hash the cookie value without
  instantiating a service. `SessionTokenHash` is a `[u8; 32]` newtype
  with `as_bytes` / `into_bytes` constructors for the repository's
  `get_by_token_hash(&[u8])` and `CreateUserSession::token_hash` fields
  respectively; it also redacts in `Debug`. The plaintext token crosses
  the service boundary exactly once — as the `token` field of
  `CreatedSession` returned from `AuthService::create_session`.
- **`relayterm-auth::AuthService`.** Composes `Arc<dyn
  PasswordCredentialRepository>` + `Arc<dyn UserSessionRepository>` + a
  `PasswordHasher`. Methods (all async): `set_password(user_id,
  plaintext)`, `verify_password(user_id, plaintext)`,
  `create_session(user_id, ttl, now) -> CreatedSession`,
  `validate_session_token(plaintext_token, now) -> UserSession`,
  `revoke_session(id, now, reason)`, `revoke_all_for_user(user_id, now,
  reason) -> u64`. Time is passed in as `DateTime<Utc>` rather than read
  from a clock trait — the surface stays small and tests stay literal.
  `verify_password` collapses every failure shape (no row, wrong
  password, corrupt stored hash) into a single `InvalidCredentials` so a
  probe cannot distinguish them. `validate_session_token` keeps
  `SessionInvalid` / `SessionExpired` / `SessionRevoked` distinct
  internally but the route layer (step 4) MUST collapse them to one 401
  body on the wire. `validate_session_token` does NOT touch
  `last_seen_at` — that is the extractor's responsibility (best-effort,
  error-tolerant). `revoke_session` returns `SessionInvalid` for an
  unknown id (not `Repository`) so a probe cannot distinguish "your id
  is unknown" from "your session was already revoked"; idempotent
  re-revoke is a no-op that preserves the original `revoked_at` and
  `revoked_reason` so the audit trail stays honest.
- **Error posture (sentinel-tested).** `AuthServiceError` variants are
  structural: `InvalidCredentials`, `SessionInvalid`, `SessionExpired`,
  `SessionRevoked`, `Repository(String)`, `Crypto`. `Display` and
  `Debug` for any error never echo the offered password, the stored
  hash, the offered token, or the stored digest. The `Repository`
  variant wraps the upstream `RepositoryError`'s `Display` — that string
  is already redaction-safe per the repository contract. `Crypto`
  deliberately drops the wrapped `PasswordHashingError` detail so the
  public string is fixed (the audit-substring tests in step 4 pin
  this).
- **Dependencies added (workspace).** `argon2 = "0.5"` (with the `std`
  feature; defaults already include `password-hash` + `rand`).
  `password-hash` is consumed transitively via
  `argon2::password_hash::*` and is NOT a separate workspace entry.
  `rand`, `sha2`, `zeroize`, and `base64` were already in the workspace
  for the vault.
- **What this slice intentionally did NOT do.** No `bootstrap` / `login`
  / `logout` / `me` routes; no cookie reading or writing; no CSRF
  middleware; no `AuthenticatedUser` extractor; no frontend auth UI; no
  passkeys; no production-auth enablement; no audit-event emission. The
  audit-kind extension migration was paired with step 4.

### Step 4 — Login / logout / bootstrap / me API + inline CSRF (✅ landed)

`POST /api/v1/auth/bootstrap`, `POST /api/v1/auth/login`, `POST
/api/v1/auth/logout`, `GET /api/v1/auth/me`. Cookie set / clear in
`axum`. Audit-event emission. Because step 6 had not landed yet at the
time of this slice, every state-changing auth route carried an INLINE
Origin-header check (the small per-route helper
`AuthRoutesConfig::check_origin`, not shared middleware) so
login/logout/bootstrap were not CSRF-vulnerable in the gap. When step 6
landed, the inline check was removed in the same commit that wired the
shared middleware so there was no gap and no double-check.

- **Routes.** All four routes are mounted under `/api/v1/auth/*` in
  `crates/relayterm-api/src/routes/v1/auth.rs`. Bootstrap creates the
  first user only (no session minted; the SPA calls `/auth/login`
  next). Login mints a 30-day session via `AuthService::create_session`
  and emits the cookie. Logout reads the cookie, revokes the matching
  session row through `AuthService::revoke_session` (idempotent at the
  repository), and writes a clear-cookie header. `GET /auth/me`
  validates the cookie via `AuthService::validate_session_token` and
  returns the safe `UserResponse` DTO.
- **Cookie wire shape.** `relayterm_session=<43-char URL-safe-base64
  token>; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`. `Secure`
  is appended when `auth.cookie_secure = true`. `Domain=<...>` is
  appended when `auth.cookie_domain` is set. The plaintext token is the
  only byte sequence that crosses the boundary unhashed;
  `crates/relayterm-auth::session_token::SessionToken::expose` is the
  single legitimate caller of the cookie writer per AGENTS.md ("Don't
  ... stash, log, or pass-around the plaintext value of a
  `SessionToken`").
- **Origin guard policy.** Both a missing `Origin` header AND an
  `Origin` value not present in `auth.allowed_origins` produce `403
  forbidden { code: "csrf_origin_mismatch" }`. An empty
  `allowed_origins` list rejects every write — that is the secure
  default; tests / dev populate it explicitly. `GET /auth/me` is exempt
  from the inline guard (idempotent read; same exemption step 6's
  middleware preserves).
- **Audit emission.** `first_user_created` (paired migration
  `20260501000015_audit_events_first_user_created_kind.sql` extends the
  CHECK; the matching `AuditEventKind::FirstUserCreated` lands in
  lockstep), `login_succeeded`, `login_failed`, and `logout_succeeded`
  (all three were already in the CHECK from a prior slice). Failure-path
  audits on bootstrap (`bad_token`, `already_bootstrapped`) and login
  (`bad_credentials`) reuse `login_failed` with `actor_id = NULL` and a
  `payload.method` discriminator — `"bootstrap"` vs `"password"`. Audit
  failures on probe / failure paths are best-effort (a transient DB
  failure on the audit append does not turn a 401 into a 500); audit
  failures on the success paths (bootstrap → `first_user_created`, login
  → `login_succeeded`, logout → `logout_succeeded`) are fail-closed and
  surface as 500 to the caller, mirroring the partial-success contract
  documented for `create_session` and the server-profile lifecycle
  audit. Payloads contain public metadata only — sentinel-string
  redaction tests pin that no `password` / `password_hash` /
  `session_token` / `token_hash` / `bootstrap_token` / `argon2id` value
  reaches a persisted row.
- **Production-auth enablement (status at the time of this slice).**
  Was still fail-fast at boot — `Config::validate_auth` rejected
  `auth.mode = production` until the route migration and shared CSRF
  middleware landed. Step 10 retired the gate; production now boots when
  the configuration envelope is satisfied.

Tests:

- DTO redaction unit tests pin that `BootstrapRequest` / `LoginRequest`
  `Debug` redacts the bootstrap token and password to length-only
  markers; that `UserResponse` serialization carries no secret-shaped
  names; that the validation error paths never echo the offered token,
  password, or email value.
- Auth-route module unit tests pin the cookie format (`HttpOnly;
  SameSite=Strict; Path=/; Max-Age=2592000`; `Secure` only when
  configured), the Origin-guard's allow / deny / missing /
  empty-allowlist / non-UTF-8 cases, the `AuthRoutesConfig::Debug`
  bootstrap-token redaction, the constant-time bootstrap-token compare,
  and the `Cookie:` header parser.
- Postgres-backed integration tests (in
  `crates/relayterm-api/tests/api.rs`, `postgres-tests` feature) cover:
  bootstrap creates the first user (and does NOT mint a session
  cookie); bootstrap rejects a wrong token without echoing the
  attempted value; bootstrap is one-shot (the second call returns `409
  conflict { reason: "already_bootstrapped" }`); bootstrap returns 503
  when `auth.first_user_bootstrap_token` is unset; login succeeds and
  sets a `HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000` cookie;
  login wrong-password returns 401 and writes a `login_failed` audit
  row that does NOT echo the password; login unknown-email is
  byte-identical to login wrong-password (probe resistance); `GET
  /auth/me` returns the user for a valid cookie and 401s a missing /
  unknown / revoked cookie; logout revokes and clears the cookie;
  logout is idempotent for missing / unknown cookies (no
  `logout_succeeded` row is written for the no-op paths); the inline
  Origin guard rejects missing and disallowed origins on the write
  routes; `GET /auth/me` is exempt from the Origin guard (no Origin →
  401, not 403). Property 8 (logout invalidates at the wire layer) and
  properties 4, 6 (partial — for the four new auth routes), 9, and 12
  are exercised at this layer; properties 5 and 10 still belong to
  later slices.

### Step 5 — `AuthenticatedUser` extractor (✅ landed)

`crates/relayterm-api/src/auth/` ships the cookie-backed
`AuthenticatedUser` extractor (`auth/user.rs`) plus a shared cookie
helper (`auth/cookie.rs`) consumed both by the extractor and by the
`/api/v1/auth/*` routes. The extractor parses the `Cookie:` header
(exact-match on `relayterm_session`; missing / non-UTF-8 / empty-value /
prefix-named / suffix-named cookies all collapse to a single 401
indistinguishable from "no cookie"), hashes the token via
`relayterm_auth::hash_session_token`, validates it through
`AuthService::validate_session_token` (revoked → 401, expired → 401,
unknown → 401), then loads the `User` row by id (missing → 401).
Failures collapse on the wire to the static `unauthorized` envelope;
operator-side detail (`missing session cookie` / `session invalid` /
`session expired` / `session revoked` / `session references missing
user`) survives in the `warn!` line in `error.rs::IntoResponse`. The
handler surface exposes `user_id() -> UserId`, `user() -> &User`, and
`into_user() -> User`. The session token, the token hash, and the
session row are NEVER reached by the handler — only the resolved
`UserId` and `User` are.

`last_seen_at` is stamped on every successful extraction via
`db.user_sessions().touch_last_seen(session.id, now)`, awaited inline
(no `tokio::spawn`-and-forget per the AGENTS.md concurrency rule). The
touch is best-effort: a repository failure logs at `warn!` with the
session id only (never the cookie, token hash, or repository internals)
and the request still succeeds. Failed / expired / revoked / unknown
extractions never reach the touch — the early returns above guarantee
`last_seen_at` is updated only on the happy-path. The session-management
UI consumes this column.

Tests: `auth::cookie::tests` pins the parser's exact-match policy across
single / multiple / prefix-named / suffix-named / empty-value /
non-UTF-8 / no-equals / duplicate / whitespace-padded fixtures (11 unit
cases). Postgres-backed integration tests in
`crates/relayterm-api/tests/api.rs` cover: `me` returns the user for a
valid cookie via the extractor, `me` rejects a missing cookie (401),
`me` rejects an unknown cookie (401), `me` rejects an expired session
(row inserted with `expires_at` in the past; 401), `me` rejects a
revoked session (revoked at the repository; 401), `me` rejects a
prefix-confusion cookie (`relayterm_session_other=<real-token>`; 401),
the `/me` 200 response carries no `password` / `password_hash` /
`session_token` / `token_hash` / `bootstrap_token` / `argon2id`
substring AND no sentinel-shaped string from the test password / token
/ bootstrap secret, AND the `last_seen_at` touch contract: a successful
`/auth/me` advances `last_seen_at`, a successful protected
`/api/v1/hosts` GET advances it (proves the touch rides on the shared
extractor), an expired session does NOT advance it, a revoked session
does NOT advance it, and an unknown-token request creates no row AND
leaves any pre-existing legitimate session's `last_seen_at` untouched.
The auth crate graduated from `NotImplemented` in step 3.

### Step 6 — Shared CSRF / `Origin` guard foundation (✅ landed)

`crates/relayterm-api/src/auth/csrf.rs` ships the shared helper
`check_origin(&HeaderMap, &[String]) -> Result<(), ApiError>` and the
`CsrfGuard` axum extractor (`FromRequestParts`) that wraps it. Every
browser-write route takes `_csrf: CsrfGuard` as its first extractor —
placed ahead of `Json<...>` so the rejection happens before request
bytes are parsed and before any DB or auth work runs. (Ordering note:
axum 0.8 runs every `FromRequestParts` extractor before the single
`FromRequest` body extractor regardless of source order, so the
rejection-before-body-parse guarantee is enforced by axum's invariant;
the "ahead of `Json<...>`" placement is convention that keeps the call
site self-explanatory and is pinned by the integration tests — not a
load-bearing source-order requirement.)

Wire policy: missing / non-UTF-8 / non-allowlisted `Origin` → `403
forbidden { code: "csrf_origin_mismatch" }`; empty `allowed_origins`
rejects every write; `GET /auth/me` and the WebSocket attach route are
exempt. The wrapped operator-side detail strings (`missing Origin
header` / `Origin header is not valid UTF-8` / `Origin not in
allowed_origins`) are deliberately classified — they never echo the
offered `Origin` value. Comparison is **case-sensitive byte equality**;
a case-insensitive variant is deferred (handling internationalised
hostnames safely is its own slice).

**Out of scope (deliberate).** No double-submit token (deferred until a
non-same-origin client lands per "CSRF posture" in auth.md); no
route-wide `tower` middleware (the extractor approach gives per-route
scope without a global allow-list of GET routes).

**Tests.** `auth::csrf::tests` pins ten unit cases (allow / deny /
missing / empty-allowlist / non-UTF-8 / case-sensitivity /
trailing-slash / multi-origin allow / two distinct sentinel-Origin
redaction cases). Postgres-backed integration tests in
`crates/relayterm-api/tests/api.rs` cover: bad Origin rejects BEFORE
body parsing (a malformed JSON body paired with a disallowed Origin
returns 403 not 400); a CSRF-rejected login does NOT write a
`login_failed` audit row (no auth work runs); a CSRF-rejected bootstrap
creates no user row AND emits zero auth audit rows; the same shape
applies to every other browser-write route
(`create_host_bad_origin_returns_403_before_body_parse`,
`disable_with_bad_origin_returns_403_and_writes_no_audit`, etc.).

### Step 7 — Route migration (Phase B) (✅ landed)

Every protected `/api/v1/*` app route takes `AuthenticatedUser`. The
migrated surfaces are: `hosts` (create/list/get), `ssh-identities`
(create/list/get), `server-profiles` (create/list/get + disable/enable +
host-key-preflight + trust-host-key + auth-check), `terminal-sessions`
(create/list/get/close + the WebSocket attach route), and
`audit-events/recent`. Browser-write routes (`POST` / state-changing
handlers) additionally take the shared `_csrf: CsrfGuard` extractor as
their first parameter so a missing or non-allowlisted `Origin` header
rejects with `403 csrf_origin_mismatch` BEFORE the body is parsed AND
BEFORE any DB / auth / lifecycle work runs. The WebSocket attach route
is `GET` and therefore exempt from `CsrfGuard`; its auth gate is the
cookie-backed `AuthenticatedUser` extractor which short-circuits BEFORE
the upgrade handshake completes (clients see a clean HTTP 401, not an
opened-then-closed socket).

Ownership filtering shape: handlers extract `UserId` via
`user.user_id()`, repository queries stay scoped to `owner_id =
caller`, foreign-vs-missing collapses to a byte-identical 404. The
`into_create(owner_id: UserId)` DTO methods on `CreateHostRequest` /
`CreateServerProfileRequest` take a bare `UserId`; audit lifecycle
helpers (`write_lifecycle_audit`, `resolve_owned_profile`) likewise.

Tests: every fixture (`setup`, `setup_with_probe`, `setup_full`,
`setup_with_auth_check_service`, `setup_with_full_state`,
`setup_with_full_state_short_ttl`, `setup_with_fake_probe`,
`setup_with_fake_auth_checker`, `setup_with_pty_bridge`,
`setup_with_first_user`, `setup_production_first_user`) bootstraps a
real `AuthService` session via `bootstrap_test_session(&auth, user_id)`
and returns the cookie-token plaintext as the last tuple element. The
`json_post(uri, body, cookie)` and `get(uri, cookie)` request builders
attach the cookie + Origin (POST only) automatically; `json_post_no_auth`
/ `get_no_auth` cover the missing-cookie 401 paths;
`json_post_with_origin(uri, body, cookie, origin)` covers the
bad-Origin 403 paths. WebSocket helpers (`open_ws`, `open_ws_attached`,
`ws_handshake_status`) take the cookie token and attach it to the
upgrade handshake. Integration tests cover:
`protected_hosts_routes_return_401_without_session_cookie` (GET + POST
`/hosts` reject when no cookie),
`post_ssh_identity_returns_401_without_session_cookie`,
`auth_check_returns_401_without_session_cookie`,
`terminal_session_routes_return_401_without_session_cookie` (covers
create / list / get / close on the v1 surface),
`ws_attach_returns_401_without_session_cookie` (the WebSocket handshake
fails BEFORE upgrade), `audit_events_recent_unauthorized_without_session_cookie`,
`create_host_bad_origin_returns_403_before_body_parse` (malformed JSON
body + disallowed Origin → 403, not 400; no row written),
`create_host_missing_origin_returns_403`, and
`disable_with_bad_origin_returns_403_and_writes_no_audit` (lifecycle
audit row count stays 0 on the bad-Origin path). Property 5 (CSRF
rejects bad Origin) is covered for at least one representative route per
surface; property 7 (cross-user 404 indistinguishable) and property 10
(no fabricated identity) are exercised against the extractor.

### Step 8 — Login throttling (✅ landed, foundation)

`crates/relayterm-auth::throttle` ships `LoginThrottler` (in-memory map
behind a `std::sync::Mutex`; no I/O under the lock so safe to share
across an async runtime), `LoginThrottleConfig` (v1 default: 5 failures
/ 15-min window → 15-min block), `ThrottleDecision { Allowed, Throttled
{ retry_after_seconds } }`, and `normalize_login_identifier` (lower-case
+ trim). Wired into `AppState::login_throttler` and consumed by `POST
/api/v1/auth/login` ahead of the user lookup; the route emits
`ApiError::TooManyRequests` on a hit (new variant, wire `code:
"too_many_requests"`, status 429, static `"too many requests"` body).
Audit emits `login_failed` with `reason = "throttled"` (best-effort,
mirroring the bad-credentials path). `record_failure` runs on BOTH the
wrong-password and unknown-email branches so the throttle channel
preserves the same probe resistance the wire response does.
`record_success` clears the bucket on a correct login. Bounded at
10,000 keys via opportunistic cleanup; full-map insert silently no-ops
(fail-open under saturation).

**What this slice did NOT include.** IP-aware keying (deferred until
`ConnectInfo` is plumbed through the listener); distributed /
Redis-backed limiter (multi-instance deploy still relies on
reverse-proxy rate-limiting per `docs/production-auth.md`); `Retry-After`
header (would leak throttle-key telemetry — re-evaluate if/when a SPA
needs the countdown UI); a configurable policy on `AuthConfig`
(constants in code for v1 — bumping the policy is a code change).

Property 11 is exercised by
`login_throttle_blocks_after_threshold_with_safe_response`,
`login_throttle_unknown_user_shares_bucket_with_known_user`,
`login_failed_audit_reasons_split_bad_credentials_and_throttled`,
`successful_login_clears_throttle_bucket`,
`bad_origin_login_does_not_engage_throttler`, and
`login_throttle_is_keyed_on_normalized_email` in
`crates/relayterm-api/tests/api.rs`, plus 13 deterministic unit tests in
`crates/relayterm-auth/src/throttle.rs::tests`.

### Step 9 — Frontend auth (Phases 1–4 landed; Phases 5–6 deferred)

`apps/web/src/lib/api/auth.ts` ships typed helpers for `getCurrentUser`,
`login`, `logout`, and `bootstrap` plus the field-by-field
`parseCurrentUser` parser, the `describeAuthError` formatter (function
of `kind` + `status` + `code` only), and frontend-side `validateLoginForm`
/ `validateBootstrapForm` mirrors of the backend bounds. Every helper
sets `credentials: "include"` so the browser ships the
`relayterm_session` cookie; nothing in the SPA reads, writes, or echoes
the cookie value. The `Origin` header is never set from JS — the
browser controls it on POSTs and the backend's CSRF guard is appeased
by the browser-attached value.

`apps/web/src/lib/app/auth/AuthGate.svelte` mounts at the top of
`App.svelte`, calls `getCurrentUser()` on mount, and short-circuits the
rest of the SPA: a small `auth-loading` splash while in flight,
`auth-error-screen` (with explicit retry; no auto-retry storm) on
transport / 5xx / malformed, `LoginView` on HTTP 401, and the existing
`AppShell` view tree on a parsed user. `LoginView` (`auth-login-*`
selectors) submits to `POST /api/v1/auth/login` and collapses the wire
401 to a generic "invalid credentials" line — the copy never reveals
whether the offered email belongs to a known account. A "First-time
setup" affordance switches the unauthenticated screen to `BootstrapView`
(`auth-bootstrap-*` selectors); bootstrap creates the user, shows
"Account created. Please sign in.", and routes back to `LoginView` (no
auto-login — keeping session minting on the login route only). The
`TopBar`'s `auth-sign-out` button calls `POST /api/v1/auth/logout` and
ALWAYS runs local cleanup afterwards (clears the active-terminal
pointer and `activeLaunch`, drops the gate to the login screen)
regardless of the wire outcome — a flaky network can never trap an
operator in a logged-in UI state. The bootstrap form's `bootstrap_token`
is a `<input type="password">`; the SPA does not persist the token, the
password, the session token, or any decoded session payload to local
storage. The redaction posture is sentinel-tested in
`apps/web/tests/authApi.test.ts`: `parseCurrentUser` drops smuggled
`password_hash` / `session_token` / `token_hash` / `bootstrap_token` /
`private_key` / `encrypted_private_key` / `access_token` /
`session_output` field-by-field; `describeAuthError` never echoes the
wire `message` or transport detail; `login` / `bootstrap` request
inputs (offered password / bootstrap token) never reach an error string
or `console.*`.

**Phases still deferred.** Phase 5 (a `lib/api/authState.ts` store + a
`fetchJson` 401 interceptor that drops the SPA to `LoginView` from any
protected `/api/v1/*` 401) is NOT in this slice — protected views still
surface their own 401 via the per-view error formatter, which is
acceptable until we have a richer story for "session expired
mid-flow." Phase 6 (URL-based route guarding that preserves the
originally-requested path across login) is also deferred — `AuthGate`
short-circuits the entire view tree when unauthenticated, so the gate
IS a guard; the "preserve and restore the requested path" affordance is
its own slice.

### Step 10 — DevUser retirement + production-auth enablement (✅ landed)

Deleted `crates/relayterm-api/src/dev_user.rs`, dropped
`AppState::dev_user_id` and the `FromRef<AppState> for Option<UserId>`
impl, dropped `DevAuthConfig` and the `dev_auth` config field, dropped
the `bootstrap_dev_user_for_unimplemented_auth` startup call and the
`DEV_USER_EMAIL` / `DEV_USER_DISPLAY_NAME` constants.
`Config::validate_auth` no longer hard-rejects `auth.mode = production`
— it now enforces the production envelope (signing key, allow-list,
Secure cookies) and accepts on success. `apps/backend/src/main.rs` adds
a runtime gate after the DB connect: production with no first user AND
no `first_user_bootstrap_token` configured → `bail!`. The legacy
`dev@relayterm.local` user row is no longer auto-bootstrapped at
startup; existing rows in deployed databases are not touched (no
migration drops them) and behave as a normal user without a password
row.

Tests: every fixture sets up real cookie-backed auth (no `dev_user_id`
field on `AppState` to set);
`production_login_sets_secure_cookie_and_authenticates_protected_route`
is the wire-level proof that a production-shaped `AuthRoutesConfig`
(cookie_secure=true, populated allow-list) mints a `Secure` cookie,
that cookie authenticates a former `DevUser`-only route, AND a
no-cookie request to the same router still returns 401 (production
does not silently bypass auth). `production_no_first_user_no_token_runtime_gate`
mirrors the exact predicate the main.rs gate runs and pins all three
operator states (no users + no token blocks; no users + token-set
proceeds; first-user-exists proceeds regardless of token).
`auth_mode_production_with_valid_config_validates`,
`auth_mode_production_missing_signing_key_fails_fast`,
`auth_mode_production_both_signing_key_sources_set_is_ambiguous`,
`auth_mode_production_empty_allowed_origins_fails_fast`,
`auth_mode_production_cookie_secure_false_fails_fast`,
`auth_mode_production_with_signing_key_file_only_validates`,
`auth_mode_production_with_optional_bootstrap_token_validates`,
`dev_auth_env_var_is_silently_ignored`, and
`legacy_dev_auth_toml_section_is_silently_ignored` pin the new
validate_auth policy. The
`auth_validation_errors_do_not_echo_secret_env_values` test exercises
every reachable production-mode failure path against a sentinel-shaped
bootstrap token and signing key.

### Step 12 — Active sessions list + UI (✅ landed)

Three current-user routes are now live under `/api/v1/auth/`: `GET
/sessions`, `POST /sessions/:id/revoke`, `POST
/sessions/revoke-all-except-current`. Ownership is enforced in SQL via
`WHERE user_id = $caller` on every read AND on the single-row revoke
(no `.filter()` at the route layer; a route that forgot to re-check the
owner cannot leak cross-user rows). Foreign-or-missing session ids
collapse to a byte-identical `404 not_found`. Single-row revoke is
idempotent at the repository (the original `revoked_at` /
`revoked_reason` are preserved on a redundant call) and writes an audit
row only on a real non-revoked → revoked transition.

Revoking the current session clears the cookie (`Set-Cookie ...;
Max-Age=0`) and emits `session_revoked` with `current_session: true`;
revoking another session emits the same kind with `current_session:
false` and does NOT touch the cookie.
`revoke-all-except-current` emits `sessions_revoked` (NEW kind, paired
migration `20260502000016_audit_events_session_revoked_kinds.sql`) with
`revoked_count` only — no per-row session ids in the payload — and
writes nothing when the count is zero. CsrfGuard sits ahead of every
state-changing handler so a bad `Origin` rejects with 403 before any DB
mutation. The `AuthenticatedUser` extractor now also exposes the
current `session_id` so the routes can mark `current` in the listing
and decide whether to clear the cookie on revoke; the token plaintext
and the SHA-256 hash remain unreachable to handlers (`AGENTS.md` "Don't
... stash, log, or pass-around the plaintext value of a
`SessionToken`").

The DTO returned by `GET /sessions` (`SessionListItem`) is hand-rolled
so a future column on `UserSession` cannot widen the response by
accident, and `serde(skip)` on the domain row keeps `token_hash`
unreachable through `serde`. Fields on the wire: `id`, `created_at`,
`last_seen_at`, `expires_at`, `revoked_at`, `current: bool`, and a
presentation `status` (`active` / `expired` / `revoked`; `revoked` wins
when both are true, mirroring the `validate_session_token` priority).
`remote_addr` / `user_agent` and device naming are NOT on the wire —
the columns aren't populated yet; adding them with the listing surface
in place is a strictly additive follow-up.

**Frontend Settings session-management UI**
(`apps/web/src/lib/app/views/AuthSessionsPanel.svelte`) is wired into
the production Settings view: it renders the caller's sessions with a
`Current` badge, a status badge (`Active` / `Expired` / `Revoked`),
`Created` / `Last seen` / `Expires` / `Revoked` timestamps, a per-row
Revoke button (active sessions only), and a top-level "Revoke all other
sessions" button. Revoking the current session is allowed but
confirmed; on success the panel hands off to `AppShell` via an
`onCurrentSessionRevoked` callback that runs the same local cleanup as
the explicit Sign-out button (active-launch drop, gate flip) without
re-POSTing `/auth/logout` (the revoke route already cleared the
cookie). The frontend API helpers live alongside the existing auth
helpers in `apps/web/src/lib/api/auth.ts` (`listAuthSessions`,
`revokeAuthSession`, `revokeAllAuthSessionsExceptCurrent`, plus a
`parseAuthSession` field-by-field parser); every helper sets
`credentials: "include"`, `revokeAuthSession` path-encodes the session
id, and the parser's strict allow-list drops smuggled `token_hash` /
`session_token` / `password_hash` / `bootstrap_token` / `private_key` /
`encrypted_private_key` / `access_token` / `session_output` /
`remote_addr` / `user_agent` keys field-by-field.

**Out of scope for this slice (deliberate).** Admin / cross-user session
view; `remote_addr` / `user_agent` capture and device naming;
password reset; passkeys; distributed session tracking; `auth.mode =
production` route-level migration (the same code path runs in dev and
production — only the boot-time validation envelope differs).

Tests: 6 repository postgres-tests pin `list_for_user` ownership
scoping and ordering, `revoke_for_user` transition + idempotency +
foreign-vs-missing collapse, `revoke_all_except` only-other-active
rows, the unknown-user no-op, and that an already-revoked `except_id`
is not re-touched. 11 API postgres-tests pin the wire surface
(current-marker, idempotent revoke audit, foreign 404 collapse, cookie
clear on current revoke, CsrfGuard precedence, all three routes
require auth). 36 frontend tests in
`apps/web/tests/authSessionsApi.test.ts` pin: `credentials: "include"`
on every helper; path-encoding of the session id (with
`../../etc/passwd?x=1` as the pathological fixture); the parser's
accept / reject envelope (status discriminator validated against the
closed enum, malformed responses collapse to `malformed_response`,
unknown extra fields ignored); the redaction sentinels (the eight
forbidden secret-shaped names AND `remote_addr` / `user_agent` cannot
survive the parse step OR `JSON.stringify` the parsed list);
error-formatter posture (function of `kind` + `status` only, no wire
`message` or `code` echoed); and no `console.*` output on success or
transport failure.

### Step 13 — Current-user password change (✅ landed)

`POST /api/v1/auth/change-password` is live: requires
`AuthenticatedUser`; runs `CsrfGuard` ahead of body extraction;
validates the new password against the same length policy as
`BootstrapRequest` / `LoginRequest` (≥12, ≤1024 chars); verifies
`current_password` against the stored Argon2id hash via
`AuthService::verify_password` (which collapses "no row" and "wrong
password" to a single `InvalidCredentials` shape); persists the new
hash via `AuthService::set_password` (per-call random salt, fresh PHC
string even if old == new); revokes every OTHER session for the caller
via `AuthService::revoke_all_sessions_except` with reason
`"password_changed"` (the current cookie stays valid — a successful
rotation is NOT a sign-out from this tab); writes one
`password_changed` audit row on the success path with payload `{
revoked_other_sessions: u64, changed_at }`. A wrong-current-password
attempt returns a static `401 unauthorized`, leaves the password row
untouched, revokes nothing, and writes NO `password_changed` audit row.
A CSRF rejection short-circuits before the verify and writes nothing.
The `password_changed` audit kind was added by
`20260502000017_audit_events_password_changed_kind.sql` (paired
migration).

**Frontend Settings password panel**
(`apps/web/src/lib/app/views/PasswordPanel.svelte`) sits in the
Settings view above the session-management panel: three
`type="password"` fields (current / new / confirmation),
`autocomplete="current-password"` / `"new-password"` hints, a
client-side mirror of the backend length policy, a confirmation-match
check, and a same-as-current rejection. On any answered request —
success OR failure — every password field is wiped so a partially-
entered or just-rotated value does not linger across navigations. The
two formatters in `apps/web/src/lib/api/auth.ts`
(`describeChangePasswordError`, `describeChangePasswordSuccess`) are
sentinel-tested and the only strings the UI renders for this surface;
the 401 collapses to "Current password is incorrect, or your session
has ended." so a probe via this endpoint cannot distinguish the two
cases beyond the status code itself.

**Out of scope for this slice (deliberate).** Email-based password
reset; admin password reset; passkey rotation; password history;
account deletion / disable.

Tests: 11 unit (DTO redaction + validation), 6 API postgres integration
(happy path with revoke + audit, no-other-sessions zero-count,
wrong-current 401 with no mutation, short-new 400, missing cookie 401,
bad Origin 403 before verify), and 29 frontend (parser allow-list +
sentinels, helper request shape, error-formatter posture across all
HTTP statuses, console-silent transport failure, success formatter
pluralization, full validation matrix). The frontend audit-feed
`describeAuditEventKind` now maps `password_changed` to a stable label.

## Notes preserved from earlier iterations

### Legacy `dev@relayterm.local` fixture (historical)

The hard-coded dev-fixture user that the old `DevUser` shim
bootstrapped at startup is gone — `apps/backend/src/main.rs` no longer
creates it on boot, and the `bootstrap_dev_user_for_unimplemented_auth`
helper has been deleted. Existing rows in deployed databases stay (no
migration drops the row); they are now treated as a normal user, but a
password row was never written so the row cannot log in. An operator
who wants to keep that account uses the standard `set_password` path
(no UI yet — direct DB or future admin tooling), and an operator who
wants it gone deletes the row through the same path inventory cleanup
follows.

### Audit-event read-feed visibility (current behaviour)

Listing what is and is not shown on the per-user audit feed lives in
auth.md "Audit events". This sub-section captures the underlying
rationale for posterity:

- `login_succeeded`, `logout_succeeded`, `password_changed`, and
  `first_user_created` carry the user's own id as `actor_id` and ARE
  visible on the per-user feed. A user seeing their own sign-ins is the
  load-bearing UX of an audit feed.
- `session_revoked` carries the revoking user's id as `actor_id`. When a
  user revokes one of their own sessions from the active-sessions list
  (`POST /api/v1/auth/sessions/:id/revoke`), the row IS visible on
  their feed (they are both actor and target). When an admin (future)
  revokes another user's session, the `actor_id` is the admin's id and
  the target user does NOT see the row on their per-user feed — that is
  the intended NULL-actor-style isolation. A future "events about me"
  admin surface (`target_user_id` audit query) is its own slice and is
  NOT mixed into `recent_for_actor`.
- `sessions_revoked` carries the user's own id as `actor_id` (the route
  is current-user-only) and IS visible on their feed.
- `login_failed` is `actor_id = NULL` and never appears on any per-user
  feed.

The frontend `parseAuditEvent` already collapses unknown summary kinds
to `generic`, so additional auth kinds are forward-compatible — the
per-kind sanitizer arms can land in a follow-up without breaking the
frontend.
