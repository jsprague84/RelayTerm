# Redaction, auth, and session rules — long form

> The **summary table** of these rules lives in `AGENTS.md` →
> "Things to avoid". When a row in that table says **(see this file)**,
> the full prose contract is here.
>
> These are load-bearing security rules. The summary in AGENTS.md is
> intentionally terse so it stays in every session-start context; this
> file is what you read when you're about to touch the surface in
> question.

---

## 1. Audit-event payload contents

**Don't** put `encrypted_private_key`, plaintext PEM bytes, public-key
bytes, raw russh / DB error text, peer banners, terminal I/O, vault
internals, or `client_info` blobs in an `audit_events.payload`.

**Do** include only public metadata — ids, names, fingerprints
(public), `key_type`, timestamps, reference counts, reason codes.

Build the JSON object field-by-field from a small helper; mirror
`write_lifecycle_audit` in `routes/v1/server_profiles.rs`. Sentinel-string
tests against `AUDIT_FORBIDDEN_SUBSTRINGS` are the redaction backstop.

## 2. Lifecycle audit emission idempotency

**Don't** append an audit row on a redundant/idempotent lifecycle call
(re-disable, re-enable, no-op trust, etc.).

**Do** audit only on the actual state transition. The route's
idempotency early-return MUST sit *before* the audit append, so a no-op
call returns the unchanged row and writes zero rows. SPEC.md
(`docs/spec/inventory.md`) → "Server profile lifecycle audit" is the
canonical pattern.

## 3. Inventory destructive-action policy

**Don't** add a delete / disable / archive / hard-revoke route or UI
without consulting the lifecycle policy.

**Do** read `SPEC.md` → "Inventory lifecycle and destructive-action
policy" first. Default user-facing destructive action for
`server_profiles` is **disable** (not delete). `hosts` / `ssh_identities`
delete is blocked while a `server_profile` references them.
`terminal_sessions` are NEVER deleted from the user UI. Every
destructive action writes one audit event with public metadata only.

## 4. SessionToken plaintext lifetime

**Don't** stash, log, or pass-around the plaintext value of a
`SessionToken` after the cookie is set, OR build any storage/lookup
index on it.

**Do** treat the plaintext as a one-shot wire value. The plaintext
crosses the service boundary EXACTLY ONCE — as the `token` field of
`CreatedSession` returned from `AuthService::create_session`. The HTTP
layer puts the bytes in `Set-Cookie` and drops the wrapper. Storage and
lookup are by `SessionTokenHash` (SHA-256 of the encoded token). The
wrapper redacts in `Debug`, has no `Display`, has no `serde`, and
zeroizes on drop — keep it that way.

A logged token + a DB dump = full session takeover, so the plaintext is
treated like a vault private-key plaintext: visible on exactly one wire
surface, never persisted, never logged.

## 5. SessionToken accessor surface

**Don't** add `Display`, `serde`, or any `as_bytes() -> &[u8]` accessor
to `SessionToken`, OR widen `SessionToken::expose()` to public callers
other than the `Set-Cookie` writer.

**Do** keep `expose()` for the cookie-writing route ONLY. Repository
inserts go through `SessionTokenHash::into_bytes()`; lookups go through
`SessionTokenHash::as_bytes()`. Any new caller of `expose()` is a
redaction regression — push the requirement up to `SessionTokenHash`
instead, or talk to the auth-service surface.

## 6. Argon2 default parameters

**Don't** tune `argon2` parameters below `PasswordHasherConfig::OWASP_2023`
in production (`m=19456`, `t=2`, `p=1`).

**Do** let test-only fast paths construct
`PasswordHasherConfig { m_cost: 19_456, t_cost: 1, p_cost: 1 }`
explicitly. Production callers MUST use `PasswordHasher::default()`.
`password::tests::default_uses_owasp_2023` pins the default constants —
a PR that weakens them MUST update the test in the same commit and
explain why (an ADR is appropriate).

## 7. CSRF / Origin guard ordering

**Don't** add a state-changing browser-write route that touches DB,
auth, OR a body extractor without running the shared CSRF / `Origin`
guard FIRST.

**Do** place `_csrf: CsrfGuard` (`relayterm_api::CsrfGuard`) ahead of
`Json<...>` / `Form<...>` / any other body extractor in the handler
signature, OR call `auth::csrf::check_origin(&headers, &state.auth_routes.allowed_origins)?`
before the first DB / auth / body access. Wire policy is
`403 csrf_origin_mismatch`; `GET`s are exempt. Never echo the offered
`Origin` value in either the wire body OR the operator-side `warn!`
line.

**Note on ordering:** in axum 0.8 every `FromRequestParts` extractor
runs before the single `FromRequest` body extractor regardless of
source order, so the "ahead of `Json<...>`" placement is **conventional**
(documents intent, keeps the call site self-explanatory) rather than
load-bearing — rearranging the signature does not break the
rejection-before-body-parse guarantee. Still pin the guarantee with an
integration test that POSTs malformed JSON + a disallowed Origin and
expects 403, not 400 — see `bad_origin_rejects_before_body_parsing` in
`crates/relayterm-api/tests/api.rs`.

## 8. Protected route UserId source

**Don't** add a protected `/api/v1/*` route that pulls the caller's
`UserId` from anywhere other than `AuthenticatedUser` (e.g. an
`Option<UserId>` extracted from state, a header, a query string, or a
re-introduced dev shim).

**Do** take `user: AuthenticatedUser` and bind the id via
`user.user_id()`. Browser-write handlers additionally take
`_csrf: CsrfGuard` as the first parameter; WS / GET routes take
`AuthenticatedUser` only (no `CsrfGuard`). Owner-scope every repository
read by `owner_id == user.user_id()` and collapse foreign-vs-missing to
a byte-identical 404.

The handler must NEVER reach the session token, the token hash, or the
session row — only the resolved `UserId` / `User`. The legacy `DevUser`
extractor and the `dev@relayterm.local` fixture user are gone;
production runs through `AuthenticatedUser` only and dev mode runs
through the same code path with relaxed boot-time validation. Pin the
auth gate with an integration test that hits the route with no cookie
and expects 401 (use `json_post_no_auth` / `get_no_auth` from the test
fixture) — see `protected_hosts_routes_return_401_without_session_cookie`
for the canonical shape.

## 9. Login throttler invariants

**Don't** touch the login throttler with the raw password, the offered
email pre-normalization, OR a key built from anything other than
`relayterm_auth::normalize_login_identifier(&email)`. Don't `tracing::*`
the throttle key. Don't add a `Retry-After` header to the 429 (would
leak countdown telemetry to a probe). Don't gate the throttle behind
"user exists" — that re-introduces the probe channel. Don't
`record_failure` on the success path. Don't `record_success` on the
failure path. Don't bypass the throttler for any login branch
(unknown-email AND wrong-password BOTH must record). Don't reach the
throttler from a CSRF-rejected handler — the `CsrfGuard` extractor
must short-circuit FIRST so a third-party origin cannot lock out a
legitimate user.

**Do** call `state.login_throttler.check(&throttle_key, now)` AFTER
`CsrfGuard` + `req.validated()` and BEFORE the user lookup. Build
`throttle_key = normalize_login_identifier(&req.email)` and never log
it.

- On the throttled branch return `ApiError::TooManyRequests(...)` and
  write `login_failed { reason: "throttled" }` best-effort.
- On the failure branch call `record_failure(&throttle_key, now)`
  BEFORE the audit append so the bookkeeping is symmetric with the
  wire return.
- On the success branch call `record_success(&throttle_key)` BEFORE
  `create_session` — a transient session-mint failure that leaves the
  bucket cleared but the user not-yet-logged-in is the kinder failure
  mode (next retry is not pre-throttled), and once the password verify
  has succeeded the bucket-clear is the right side of the trade-off.

The integration tests in `crates/relayterm-api/tests/api.rs::login_throttle_*`
pin every property; if you add a new login branch, add a matching test
in the same module.

## 10. Production paste safety

**Don't** stash paste content in Svelte `$state`, persist it to local /
sessionStorage, route it through the audit-log surface, render it
inside the paste-confirm or paste-blocked panel body, OR include it in
any thrown `Error.message` / `console.*` / `data-*` attribute. Don't
bypass `evaluatePaste` for "trusted" / "small" / "internal" paths in
the production terminal workspace. Don't widen `PasteDecision` to carry
the source text. Don't add a backend command-inspection / shell-aware
paste analysis surface.

**Do** hold the original paste text in a script-scoped
`pendingPasteText` variable on `ProductionTerminal.svelte` between
`evaluatePaste` returning `confirm` and the operator's confirm/cancel
click. Snapshot-and-clear the variable in the Send-paste handler before
calling `client.sendInput` exactly once. Render the confirm/blocked
panels from `PasteDecision` METADATA only (line count, byte length,
reason code, the static `safeUserMessage`).

Pin the redaction with sentinel tests in `tests/pastePolicy.test.ts`
that assert the decision object — across `safe` / `confirm` / `blocked`
outcomes — never carries a sentinel string from its input through any
field, JSON form, or String() form. The canonical contract is in
`docs/spec/terminal.md` → "Production terminal paste safety"; the
policy is shape-based (newlines, size, control chars), NOT
semantics-based.

## 11. Recording chunk payload redaction

**Don't** put `terminal_recording_chunks.payload` bytes (or the bytes
from any future encrypted/compressed chunk envelope) into a `tracing::*`
line, an `audit_events.payload`, a thrown `Error`, an HTTP error body,
a session-list / dashboard / activity cell, a `data-*` attribute,
frontend `localStorage` / `sessionStorage`, or any `Debug` impl that
formats the bytes themselves. Don't add a `Display` / `Serialize` impl
on `TerminalRecordingChunk` or `CreateTerminalRecordingChunk` that
exposes `payload`. Don't widen the recording repository surface to a
cross-session lister; the trait is session-scoped on purpose. Don't add
a recording read route that pulls the caller's `UserId` from anywhere
other than `AuthenticatedUser`, that skips the
`terminal_sessions.owner_id == user.user_id()` filter on the addressed
session, OR that surfaces chunk bytes in any field other than the
explicit `data_b64` base64 surface. Don't echo a chunk's `data_b64` (or
any other base64 form of payload) into a `tracing::*` line, an error
body, or an audit row — base64 is wire-shape only, NOT a redaction
layer. Don't write an `audit_events` row from a recording read endpoint.

**Do** treat chunk `payload` like a vault private-key plaintext: the
bytes cross the service boundary ONLY through the parsed domain field.
The `TerminalRecordingChunk` and `CreateTerminalRecordingChunk` `Debug`
impls redact `payload` to `<redacted: N bytes>`. The
`TerminalRecordingChunkRow` SQLx row deliberately does NOT derive
`Debug` — convert through `try_into_domain()` first. Repository errors
go through `map_sqlx_error` which strips driver text down to the entity
name plus the constraint.

Marker `payload` is metadata-only by contract: build the JSON object
field-by-field (counts, dims, reason codes, enum tags), never
`serde_json::to_value` a bag of arbitrary types.

Owner-scope happens at the API layer (the
`terminal_sessions.owner_id == user.user_id()` join belongs in the
route, not the repository); the repository takes `terminal_session_id`
and trusts the caller. The read API surface
(`/api/v1/terminal-sessions/:id/recording/{metadata,chunks,markers}`)
ONLY returns chunk bytes through `TerminalRecordingChunkResponse::data_b64`,
base64-encodes inside the handler, never logs the encoded form, and
writes zero audit rows. Foreign / unknown sessions collapse to a
byte-identical 404 via `resolve_owned_session`.

`SPEC.md` → "Durable terminal recording and replay architecture"
(`docs/spec/recording.md`) + `docs/terminal-recording.md` Section 7 +
Section 10 are the canonical contracts; the redaction backstops live
in `crates/relayterm-core/src/terminal_recording.rs` tests,
`crates/relayterm-db/tests/repositories.rs::recording_chunk_payload_not_in_error_or_debug`,
and `crates/relayterm-api/tests/api.rs::recording_*`.

## 12. Recording retention purge primitive

**Don't** `SELECT payload` (or any column derived from `payload`)
inside the retention purge primitive — including `SELECT length(payload)`,
`SELECT octet_length(payload)`, or a `RETURNING payload, ...` on the
chunk DELETE — and don't aggregate the byte total any way other than
`COALESCE(SUM(byte_len), 0)` on `terminal_recording_chunks`. Don't add
a marker payload read inside the purge path (counts only via
`COUNT(*)`). Don't echo the marker `payload` JSON into the
`recording_purged` audit row, an error body, or a `tracing::*` line.
Don't widen the purge primitive to take caller-supplied chunk-id /
marker-id lists; the surface is `(terminal_session_id, retention_days, now)`
only. Don't relax audit-failure rollback into a two-phase pattern
(commit deletes first, then audit) — the recording purge is
irreversibly destructive and the fail-closed transactional shape is
load-bearing. Don't drop or re-order the `FOR UPDATE` lock on
`terminal_sessions` inside the purge transaction; a concurrent sweep
against the same session id (multi-instance deployment, racing tests)
MUST serialise. Don't write a `recording_purged` audit row with
`actor_id != NULL`; the cleanup worker is the system, not a user, and
the user-facing `recent_for_actor` feed deliberately excludes
NULL-actor rows.

**Do** use `TerminalRecordingRepository::purge_for_retention(input)`.
The Postgres impl in
`crates/relayterm-db/src/repositories/terminal_recording.rs` is the
canonical pattern: `BEGIN`, `SELECT closed_at FROM terminal_sessions WHERE id = $1 AND closed_at IS NOT NULL AND closed_at + make_interval(days => $2) <= $3 FOR UPDATE`,
then `SELECT COUNT(*), COALESCE(SUM(byte_len), 0) FROM terminal_recording_chunks WHERE terminal_session_id = $1`,
then `SELECT COUNT(*) FROM terminal_recording_markers ...`, then
`DELETE FROM terminal_recording_markers ...`, then `DELETE FROM terminal_recording_chunks ...`,
then `INSERT INTO audit_events (...) VALUES (..., NULL, 'recording_purged', $payload, NULL, $now)`,
then `COMMIT`.

Predicate (3) — at least one chunk OR marker row exists — is the
schema-side idempotency keystone: a session whose chunks AND markers
were already deleted falls out as `Ok(None)` with no audit row written.

The `recording_purged` payload is built field-by-field from primitives
(`target_kind: "terminal_session"`, `target_id`, `chunk_count`,
`marker_count`, `bytes_purged`, `retention_days`, `closed_at`,
`purged_at`, `reason: "retention_expired"`) — never
`serde_json::to_value` of a domain struct, never per-chunk seq ranges,
never per-chunk byte counts.

The redaction backstop is
`crates/relayterm-db/tests/repositories.rs::purge_for_retention_audit_payload_redacted`
(chunk byte sentinel + marker payload sentinel +
`AUDIT_FORBIDDEN_SUBSTRINGS`). `docs/terminal-recording.md` Section
12.4 / 12.5 / 12.10 are the canonical contracts.

## 13. Production app shell isolation

**Don't** import from `lib/dev/` inside `apps/web/src/lib/app/`, AND
**don't** STATICALLY import an experimental renderer adapter
(`@relayterm/terminal-{ghostty-web,restty,wterm}`) anywhere inside
`apps/web/src/lib/app/`, AND **don't** reference an experimental
adapter package name outside the single file
`apps/web/src/lib/app/terminal/rendererLoader.ts`.

**Do** keep the production shell dev-free. The production terminal
workspace uses `@relayterm/terminal-core` + `@relayterm/terminal-xterm`
(the baseline) on the default path. The experimental adapters
(`ghostty-web`, `restty`, `wterm`) reach the production shell ONLY
through `apps/web/src/lib/app/terminal/rendererLoader.ts`, AND only
via dynamic `import()` (never `from "..."`), AND only when the
operator has flipped the `experimentalRendererEvaluationEnabled` gate
in Settings. Every fallback path (gate off, unknown id, dynamic-
import or constructor failure) collapses to xterm with a typed
fallback reason on `data-renderer-fallback`. The static-import rule,
the single-file rule, and the dynamic-only rule are all pinned by
`apps/web/tests/appShellIsolation.test.ts`. Reach the dev lab via the
`devTools` snippet in `App.svelte`, gated by `import.meta.env.DEV`.

The lazy-loader exception is NOT a renderer promotion. xterm remains
the production compatibility baseline and the production default
renderer; the experimental adapters remain experimental. See
[`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
§ "Promotion criteria" for the Gate 1 / Gate 2 path.

## 14. Placeholder views

**Don't** show fake data, mock secret values, or a `private_key` /
`encrypted_private_key` field on a placeholder view.

**Do** use `PlaceholderView` with honest copy: a one-line summary, a
"what currently exists on the backend" bullet list, and a `futureWork`
note.

## 15. Renderer adapter neutral type re-definition

**Don't** redefine `RendererTheme`, `RendererThemeAnsi`,
`RendererCursorStyle`, or `BaseTerminalRendererOptions` inside an
adapter package.

**Do** import them from `@relayterm/terminal-core`; extend
`BaseTerminalRendererOptions` for the adapter's option interface.
Renderer-specific knobs go behind a local `<renderer>Only` escape
hatch on the options object — never on the `TerminalRenderer` surface,
never on `BaseTerminalRendererOptions`.
