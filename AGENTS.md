# AGENTS.md

> Instructions for AI coding agents working in this repo. Read fully at session start. Re-read when stuck.

## Project: RelayTerm

A web/mobile SSH terminal where sessions live on the backend, clients can detach and reconnect, and the terminal renderer is replaceable. Built as a multi-language monorepo: a Rust/Axum backend that owns the SSH sessions, a Svelte 5 + Vite + Tailwind v4 web frontend, a Tauri v2 desktop shell, a Tauri v2 mobile (Android-first) shell, and a small set of swappable terminal-renderer packages.

**Owner:** <<TODO: OWNER_NAME>>
**Production URL:** <<TODO: PRODUCTION_URL>>
**Repo:** <<TODO: REPO_PATH>>

For the product spec, see `SPEC.md` (index) and `docs/spec/*` (per-surface detail). For situational rules per file type, see `.claude/skills/`. Long-form redaction / auth / session rules live in `docs/agent/redaction-rules.md`. Long-form task procedures live in `docs/agent/task-patterns.md`. The full archive of past one-off lessons lives in `docs/agent/encountered-lessons.md`.

## Architectural rule (load-bearing)

This rule is what makes RelayTerm different from a normal web terminal. Every change must respect this separation. If a piece of code blurs it, **stop and ask**.

- **The SSH session belongs to the backend.** russh holds the live connection.
- **The terminal renderer belongs to the frontend.** xterm.js / wterm / ghostty-web / restty are interchangeable adapters; none owns state.
- **The terminal state belongs to the session orchestrator.** It owns the output sequence numbers, the replay ring buffer, and (eventually) the libghostty-vt state engine.
- **The client is allowed to disappear and come back.** Reconnect by sequence-number replay. Never assume a single live socket per session.

## Session start ritual

The plugin's `SessionStart` hook runs the baseline checks below automatically.

1. Read this file, `SPEC.md`, and [`docs/agent/redaction-rules.md`](docs/agent/redaction-rules.md). The redaction-rules file is the long form for the high-risk auth / session / paste / recording / CSRF / login-throttle / retention-purge rules summarised in "Things to avoid" below; reading it at session start guarantees an agent doing a multi-surface change sees every property, not just the table summary.
2. Run baseline:
   ```
   cargo check --workspace --all-targets
   pnpm -r check
   ```
   If anything fails on a clean tree, **stop and report** — the baseline is broken.
3. Run `git status` and `git log -5 --oneline`.
4. Acknowledge in your first reply: stack version pins, branch, task starting.

## Stack — pinned versions and rationale

Do not change without asking.

| Component | Pin | Why |
|---|---|---|
| axum | `0.8.x` | Stable line; `0.9` is in-progress on `main`. |
| tokio | `^1` | Stable runtime; pin to current `1.x`. |
| russh | `0.5x` | Current channel API (`channel.into_stream`, `request_pty`, `window_change`). |
| sqlx | `0.8.x` | `.sqlx/` offline metadata; `runtime-tokio-rustls` is the canonical feature set. |
| svelte | `^5` | Runes API. Svelte 4 patterns DO NOT compile cleanly. |
| vite | `^7` | Stable Environment API; `oxc` minifier default. |
| tailwindcss | `^4` | CSS-first config (`@theme`); auto content detection. |
| xterm.js | `^5` (`@xterm/xterm`) | Scoped package; legacy `xterm` is unmaintained. |
| ghostty-web | `0.4.0` | libghostty-vt parser via WASM; xterm.js-API-compatible `Terminal`. WASM is inlined as a base64 data URL — no Vite asset wiring needed. Used only by `@relayterm/terminal-ghostty-web`. Requires `await init()` once before constructing a `Terminal`. Detail in `docs/spec/terminal-adapters.md` § "ghostty-web experimental renderer adapter". |
| restty | `0.1.x` | libghostty-vt + WebGPU/WebGL2 + text-shaper experimental renderer. Used only by `@relayterm/terminal-restty`; binds to the focused xterm-compat shim at `restty/xterm`. `Terminal.write` takes `string` only — adapter UTF-8-decodes `Uint8Array` writes. Ships ~3 MB JS plus inlined WASM; `sideEffects: false` keeps it tree-shaken. Detail in `docs/spec/terminal-adapters.md` § "restty experimental renderer adapter". |
| @wterm/dom | `0.2.x` | DOM-rendered emulator (Zig+WASM core + CSS-themed grid). Used only by `@relayterm/terminal-wterm`. `WTerm` constructor mutates the host element synchronously — defer construction AND `await init()` to `mount(element)`. `WTerm.write` accepts `string \| Uint8Array` directly. Theming goes through CSS variables on `.wterm`, not options. Detail in `docs/spec/terminal-adapters.md` § "wterm experimental renderer adapter". |
| tauri | `^2` | Adds Android/iOS; v1 conf schema is incompatible. |
| ssh-key | `^0.6` | OpenSSH keypair gen + `authorized_keys` text + SHA-256 fingerprint. RustCrypto; `ed25519` feature only — no RSA/ECDSA generators yet. |
| chacha20poly1305 | `^0.10` | XChaCha20-Poly1305 AEAD for the vault envelope. 24-byte nonce → safe random nonces. `alloc`, no `std`. |
| zeroize | `^1` | Wipes vault secrets on drop. `derive` feature for `ZeroizeOnDrop`. |
| rand | `^0.8` | `OsRng` for nonce + keypair. `0.8` is what `ssh-key 0.6` and `chacha20poly1305 0.10` interop with via `rand_core 0.6`. |
| tokio-tungstenite | `^0.29` (dev-dep) | WebSocket client used only by `relayterm-api` integration tests. Pinned to match the `tungstenite` axum 0.8 pulls in transitively so the lockfile keeps a single copy. |
| base64 | `^0.22` | Standard-alphabet RFC 4648 codec for `ServerMsg::Output { data }` (centralised in `relayterm-protocol::output_data_encode/decode`; TS mirror uses `atob`/`btoa`). The `relayterm-auth::session_token` module uses `URL_SAFE_NO_PAD` for the cookie value (43 ASCII chars in `A-Za-z0-9-_`). Do not switch the session token to the standard alphabet — `+` and `/` need percent-encoding inside `Set-Cookie`. |
| argon2 | `^0.5` | Argon2id via the RustCrypto `password-hash` integration. Default parameters are `PasswordHasherConfig::OWASP_2023` (`m=19456 KiB`, `t=2`, `p=1`) — `m` is **already in kibibytes**, do NOT multiply by 1024. PHC strings carry parameters + per-password salt inline so a future parameter upgrade can re-hash on next login without a schema column. |

## Critical gotchas

Training data may suggest older patterns. Per-component depth lives in `.claude/skills/<component>-tasks/` and auto-loads when relevant files are edited.

- **axum 0.8** — `axum::serve(listener, app)`; enable `features = ["ws"]` for WebSockets; pair `with_graceful_shutdown` with a tokio signal future.
- **tokio** — `tokio::sync::Mutex` only when holding the lock across `.await`; `select!` branches must be cancel-safe; use `JoinSet` for dynamic concurrency, never `tokio::spawn`-and-forget.
- **russh** — `check_server_key` MUST verify against the known_hosts vault (do not return `Ok(true)`); `ChannelMsg::ExtendedData { ext: 1 }` is stderr; call `window_change(cols, rows, 0, 0)` on resize.
- **sqlx 0.8** — `.sqlx/` (folder) is the offline cache, not the legacy `sqlx-data.json`. Commit it. Run `cargo sqlx prepare --workspace` after any schema or query change. Use `fetch_optional` when zero rows is valid.
- **svelte 5** — `let count = $state(0)` (top-level `let` is no longer reactive); `$derived` replaces `$:`; `$effect` replaces `onMount` for derivations; `$props()` replaces `export let`; event attributes are DOM-style (`onclick`, not `on:click`).
- **vite ^7** — minifier `oxc` is default; use `defineConfig(({ command, mode, isSsrBuild }) => ...)` for env-conditional config.
- **tailwind v4** — `@import "tailwindcss";` (not `@tailwind base/components/utilities`); theme in a CSS `@theme {}` block; install `@tailwindcss/vite`; CLI is `npx @tailwindcss/cli`.
- **xterm.js v5** — install `@xterm/xterm` (scoped); use `@xterm/addon-fit`, `@xterm/addon-webgl`, `@xterm/addon-serialize`; `term.write(data, cb)` callback signals parse-completion — use it for backpressure when streaming PTY.
- **tauri v2** — `tauri.conf.json` is split into `app`/`build`/`bundle`/`plugins`; capabilities replace v1 allowlist; `tauri android init` scaffolds under `src-tauri/gen/` (do not edit by hand). Local Android smoke uses `pnpm --filter @relayterm/mobile exec tauri android build --debug --apk --ci` (unsigned debug APK, no keystore needed); `--aab` is the Phase 4+ signed-release / Play Store path and is deliberately NOT the local-smoke command. Android packaging rejects `version: "0.0.0"` in `tauri.conf.json` — must be ≥ `0.0.1` (mobile config is `0.0.1`; desktop stays at `0.0.0` because Linux `.deb`/`.rpm` accept it).

## Web app defaults (overlay)

- **Sessions over JWTs.** Server-side opaque session IDs in Postgres; cookie holds the ID. JWT only for edge/serverless, OAuth-federated mobile, or stateless-by-mandate.
- **Validate at boundaries.** Inputs crossing into the backend get schema-validated (`serde` + `validator` on Rust; `zod`/`valibot` mirroring the same shape on the web side). Re-check session in any mutation handler — layouts can be bypassed via direct POST.
- **Build-time envs.** `import.meta.env.VITE_*` is inlined at build time; rebuild when it changes. File uploads never go in Postgres BLOBs — use object storage and persist only the URL.

## Folder conventions

```
RelayTerm/
├─ apps/
│  ├─ backend/                 # Rust crate: axum + tokio + russh + sqlx
│  │  ├─ src/{http,session,ssh,db,auth}/
│  │  └─ migrations/           # sqlx (timestamped)
│  ├─ web/                     # Svelte 5 + Vite + Tailwind v4 (browser app)
│  │  └─ src/{lib/ws,lib/stores,terminals}/
│  ├─ desktop/                 # Tauri v2 desktop shell (wraps apps/web)
│  │  ├─ src-tauri/
│  │  │  ├─ capabilities/      # v2 permission manifests
│  │  │  └─ gen/               # platform scaffolds — DO NOT edit by hand
│  │  └─ ...
│  └─ mobile/                  # Tauri v2 mobile shell — Android first (wraps apps/web)
│     ├─ src-tauri/
│     │  ├─ capabilities/      # v2 permission manifests
│     │  └─ gen/android/       # tauri android init scaffold — DO NOT edit by hand
│     └─ ...
├─ crates/                     # Rust workspace crates (relayterm-core, -api, -db, ...)
├─ packages/                   # swappable renderers
│  ├─ terminal-xterm/          # xterm.js baseline
│  ├─ terminal-ghostty-web/    # libghostty-vt parser experiment
│  ├─ terminal-restty/         # perf experiment
│  └─ terminal-wterm/          # DOM/mobile/a11y experiment
├─ deploy/                     # Docker Compose, Traefik, optional WireGuard
├─ docs/
│  ├─ agent/                   # long-form rules for coding agents
│  └─ spec/                    # per-surface SPEC detail
├─ Cargo.toml                  # workspace root
├─ pnpm-workspace.yaml
└─ AGENTS.md / SPEC.md / CLAUDE.md
```

**Tauri shells.** The desktop and mobile shells live in `apps/desktop/` and `apps/mobile/` respectively, NOT inside `apps/web/`. Each shell has its own `src-tauri/` and consumes the built web frontend from `apps/web/`. Do not collapse them back under `apps/web/src-tauri/` — that layout is obsolete.

If you're tempted to invent a new directory, propose it here first.

## Task patterns

Long-form step-by-step procedures live in [`docs/agent/task-patterns.md`](docs/agent/task-patterns.md). Index:

- **Adding a new terminal renderer adapter.** Mirrors `@relayterm/terminal-{xterm,ghostty-web,restty,wterm}`. `terminal-core` stays renderer-agnostic; the backend protocol stays renderer-neutral; renderer-specific knobs go behind a local `<renderer>Only` escape hatch. Adapter unit tests + redaction tests are required. Wire only into the dev lab. See `docs/agent/task-patterns.md` § 1.
- **Adding a production app-shell view.** Production shell (`apps/web/src/lib/app/`) MUST NOT import from `lib/dev/` or any experimental renderer adapter. Extend `AppViewId`/`NAV_ITEMS`, add a `*View.svelte` (use `PlaceholderView` with honest copy), wire into `AppShell.svelte`, extend `tests/navigation.test.ts`. Never show fake data or any `private_key`/`encrypted_private_key` field. See `docs/agent/task-patterns.md` § 2.
- **Fetching backend data from a production view.** Use `apps/web/src/lib/api/` typed helpers + `apiErrors.ts` shared envelope. Add a `parseX` runtime guard, call `fetchJsonList(endpoint, parseX)`, format UI strings via `describeLoadError` (NEVER echo wire `message` or `Error.message`), render explicit loading/empty/error/ready states. SSH-identity-shaped data MUST drop `encrypted_private_key`/`private_key` and add sentinel-string redaction tests. See `docs/agent/task-patterns.md` § 3.
- **Adding a new backend WebSocket message type.** Define in `relayterm-protocol`; mirror in `lib/ws/`. Wire-stable variants append, never renumber. JSON for control plane; binary `RTB1` for the hot terminal data path. See `docs/spec/terminal.md` § "Terminal WebSocket attach/detach contract" and "Terminal data plane: binary envelope".

## Decision tables

### Where does this code go?

| What you're adding | Where it lives |
|---|---|
| SSH protocol behavior (auth, channel, PTY) | `apps/backend/src/ssh/` |
| Session lifecycle, replay, sequence numbers | `apps/backend/src/session/` |
| HTTP / WebSocket route, axum extractor (general) | `apps/backend/src/http/` |
| HTTP-layer auth glue: cookie parsing, `AuthenticatedUser` extractor, shared CSRF / `Origin` guard (`CsrfGuard` extractor + `check_origin` helper) | `crates/relayterm-api/src/auth/` (extractors live with the rest of the `relayterm-api` HTTP surface; the crypto and persistence primitives live in `crates/relayterm-auth/`) |
| DB query or schema change | `apps/backend/src/db/` + new migration |
| Vault primitives (keypair gen, AEAD envelope, master key) | `crates/relayterm-vault/` |
| Auth wiring (session/passkey services, password hashing, session token primitives) | `crates/relayterm-auth/` |
| Known-hosts policy, audit-log surface | `crates/relayterm-auth/` (vault is for credentials only) |
| Renderer behavior (drawing, fit, perf) | `packages/terminal-<name>/` |
| Reconnect, sequence-replay, transport | `apps/web/src/lib/ws/` |
| UI state (Svelte runes) | `apps/web/src/lib/stores/` |
| Desktop shell, IPC, capabilities | `apps/desktop/src-tauri/` |
| Mobile (Android) shell, IPC, capabilities | `apps/mobile/src-tauri/` |
| Tauri runtime detection, bootstrap-picker UI, backend-URL primitives + handoff (path A) | `apps/web/src/lib/runtime/` (frontend-side; the Tauri shells re-use `apps/web` and only see the picker via `isTauriBootstrapEnabled()`). See `docs/spec/tauri-runtime-backend-url.md` |
| Terminal recording chunk / marker domain types, repository trait | `crates/relayterm-core/` (`terminal_recording.rs`, `repository.rs`); Postgres impl in `crates/relayterm-db/src/repositories/terminal_recording.rs`; migrations under `apps/backend/migrations/`. Owner-scope happens at the API layer, NOT inside the repository |

### State: who owns it?

| State kind | Owner |
|---|---|
| Live SSH connection (`russh::Channel`, host keys) | Backend `SessionManager` |
| Terminal output sequence + replay ring | Backend session orchestrator (eventually `libghostty-vt`) |
| Client view (cursor blink, font, theme, scrollback view) | Frontend renderer package |
| Session metadata (host, user, tags, last-connected) | Postgres via sqlx |
| In-flight UI state (open menu, focused tab) | Svelte runes (`$state`) |

## Things to avoid

Long-form rules live in [`docs/agent/redaction-rules.md`](docs/agent/redaction-rules.md). Each row below is the load-bearing summary; follow the section pointer when about to touch the surface in question.

| Don't | Do instead |
|---|---|
| Buffer terminal state on the client and trust it on reconnect | Replay from the backend's sequence-numbered ring buffer; client is a view |
| Hold a `russh::Channel` across a reconnect | Reopen on a fresh `client::Handle`; channels are session-bound |
| `check_server_key` returning `Ok(true)` | Verify against the known_hosts vault; reject and log on mismatch |
| Use Svelte 4 syntax (`export let`, `on:click`, `$:`) | Runes (`$props`, `onclick`, `$state`/`$derived`/`$effect`) |
| `@tailwind base; @tailwind components; @tailwind utilities;` | `@import "tailwindcss";` and a `@theme {}` block |
| Import from the unscoped `xterm` package | `@xterm/xterm` and `@xterm/addon-*` |
| Put a Tauri shell under `apps/web/src-tauri/` | Desktop shell lives in `apps/desktop/`, mobile shell in `apps/mobile/` |
| Edit `apps/{desktop,mobile}/src-tauri/gen/**` by hand | Re-generate via `tauri android init` (mobile) or platform init (desktop); configure via capabilities |
| Use a JWT for browser auth, or trust client-validated input | Server-side session; re-validate inputs at the axum boundary |
| Redefine `RendererTheme`, `RendererThemeAnsi`, `RendererCursorStyle`, or `BaseTerminalRendererOptions` inside an adapter package | Import them from `@relayterm/terminal-core`; extend `BaseTerminalRendererOptions`. (`docs/agent/redaction-rules.md` § 15) |
| Import from `lib/dev/` or any experimental renderer adapter inside `apps/web/src/lib/app/` | Production shell stays dev-free; production terminal workspace uses `terminal-core` + `terminal-xterm` only. (`docs/agent/redaction-rules.md` § 13) |
| Show fake data, mock secret values, or a `private_key`/`encrypted_private_key` field on a placeholder view | Use `PlaceholderView` with honest copy. (`docs/agent/redaction-rules.md` § 14) |
| Add a delete / disable / archive / hard-revoke route or UI without consulting the lifecycle policy | Read `SPEC.md` "Inventory lifecycle and destructive-action policy" first. Default destructive action for `server_profiles` is **disable** (not delete); `hosts`/`ssh_identities` delete is blocked while a `server_profile` references them; `terminal_sessions` are NEVER deleted from the user UI; every destructive action writes one audit event with public metadata only. (`docs/agent/redaction-rules.md` § 3) |
| Append an audit row on a redundant/idempotent lifecycle call | Audit only on actual state transitions. The route's idempotency early-return MUST sit before the audit append. (`docs/agent/redaction-rules.md` § 2; canonical pattern in `docs/spec/inventory.md` § "Server profile lifecycle audit") |
| Put `encrypted_private_key`, plaintext PEM bytes, public-key bytes, raw russh / DB error text, peer banners, terminal I/O, vault internals, or `client_info` blobs in an `audit_events.payload` | Public metadata only. Build the JSON object field-by-field; mirror `write_lifecycle_audit`. Sentinel tests against `AUDIT_FORBIDDEN_SUBSTRINGS` are the redaction backstop. (`docs/agent/redaction-rules.md` § 1) |
| Stash, log, or pass-around the plaintext value of a `SessionToken` (`session_token`) after the cookie is set, OR build any storage/lookup index on it | Plaintext `session_token` crosses the service boundary EXACTLY ONCE — as `CreatedSession.token`. HTTP layer puts bytes in `Set-Cookie` and drops the wrapper. Storage and lookup are by SHA-256 `token_hash` (the `SessionTokenHash` wrapper). The plaintext wrapper redacts in `Debug`, has no `Display`, has no `serde`, zeroizes on drop. (`docs/agent/redaction-rules.md` § 4) |
| Add `Display`, `serde`, or any `as_bytes() -> &[u8]` accessor to `SessionToken`, OR widen `SessionToken::expose()` to public callers other than the `Set-Cookie` writer | `expose()` is for the cookie-writing route ONLY. Repository inserts go through `SessionTokenHash::into_bytes()`; lookups go through `SessionTokenHash::as_bytes()`. The `token_hash` column is the only durable form of the token. (`docs/agent/redaction-rules.md` § 5) |
| Tune `argon2` parameters below `PasswordHasherConfig::OWASP_2023` in production | Production callers MUST use `PasswordHasher::default()` (`m=19456`, `t=2`, `p=1`). Test-only fast paths construct the explicit struct. `password::tests::default_uses_owasp_2023` pins it. (`docs/agent/redaction-rules.md` § 6) |
| Add a state-changing browser-write route that touches DB, auth, OR a body extractor without running the shared CSRF / `Origin` guard FIRST | Place `_csrf: CsrfGuard` ahead of any body extractor, OR call `auth::csrf::check_origin(...)?` before the first DB / auth / body access. Wire policy is `403 csrf_origin_mismatch`; `GET`s are exempt. Never echo the offered `Origin` value in body or `warn!` line. (`docs/agent/redaction-rules.md` § 7; integration test `bad_origin_rejects_before_body_parsing`) |
| Add a protected `/api/v1/*` route that pulls the caller's `UserId` from anywhere other than `AuthenticatedUser` | Take `user: AuthenticatedUser`; bind via `user.user_id()`. Owner-scope every read; collapse foreign-vs-missing to byte-identical 404. Browser-write handlers additionally take `_csrf: CsrfGuard` first. (`docs/agent/redaction-rules.md` § 8) |
| Touch the login throttler with the raw password, the offered email pre-normalization, OR a key built from anything other than `relayterm_auth::normalize_login_identifier(&email)`. Don't gate the throttle behind "user exists" — that re-introduces the probe channel. Don't bypass the throttler for any login branch | `state.login_throttler.check(&throttle_key, now)` AFTER `CsrfGuard` + `req.validated()` and BEFORE the user lookup. Build `throttle_key = normalize_login_identifier(&req.email)`; never log it. No `Retry-After` header on 429. Both unknown-email AND wrong-password branches must `record_failure`. (`docs/agent/redaction-rules.md` § 9; integration tests in `login_throttle_*`) |
| Stash paste content in `$state` / storage / audit / panel body / Error / `console.*` / `data-*`; bypass `evaluatePaste` for "trusted" paths | Hold paste text in a script-scoped `pendingPasteText`; snapshot-and-clear before `client.sendInput`; render panels from `PasteDecision` METADATA only. (`docs/agent/redaction-rules.md` § 10) |
| Put `terminal_recording_chunks.payload` bytes (or any future envelope) in any log / audit / Error / HTTP body / UI cell / `data-*` / browser storage / `Debug` | Chunk bytes cross the wire ONLY through `TerminalRecordingChunkResponse::data_b64`; never logged; zero audit rows on read. Foreign sessions collapse to byte-identical 404. (`docs/agent/redaction-rules.md` § 11) |
| `SELECT payload` (or `length`/`octet_length`/`RETURNING payload`) inside the retention purge primitive; aggregate bytes outside `COALESCE(SUM(byte_len), 0)`; commit deletes before audit write; relax the `FOR UPDATE` lock; audit `recording_purged` with `actor_id != NULL` | Use `TerminalRecordingRepository::purge_for_retention(input)`: single transaction, lock `FOR UPDATE`, count-only aggregates, delete markers→chunks, insert audit field-by-field, `COMMIT`. Audit failure ROLLBACK reverts deletes. (`docs/agent/redaction-rules.md` § 12) |

## Git workflow

Mixed strategy — solo dev, optimized for speed.

- **Push to `main`** for: typo/doc fixes, single-line bug fixes with clear cause, patch bumps.
- **Feature branch + `--no-ff` merge** for: schema changes, new routes, significant refactors, minor/major dep upgrades, anything touching auth/deploy/CI, anything crossing the backend↔frontend↔orchestrator boundary.
- Branches: `feat|fix|chore|docs/<short-name>`. Commits: Conventional Commits (`feat(scope): subject`), 72-char body wrap. Before merging: rebase onto `main`, squash fixups, confirm `cargo check --workspace` + `pnpm -r check` pass.

## Definition of done

A change is not done until ALL of these are true:

1. `cargo check --workspace --all-targets` and `cargo clippy --workspace --all-targets -- -D warnings` pass.
2. `pnpm -r check` (svelte-check + tsc) and `pnpm -r lint` pass.
3. Affected unit tests pass (`cargo test`, `pnpm -r test`).
4. If schema changed: a sqlx migration was generated AND committed; `cargo sqlx prepare --workspace` was run.
5. If a new route or WebSocket message: it's reachable, auth-checked, validated at the boundary.
6. If a feature per the Git workflow: branch + Conventional Commits message. Trivial fixes land on `main`.
7. For changes touching auth or input handling: every input was schema-validated at the boundary; every protected handler re-checked the session.
8. **AGENTS.md / SPEC.md updated** per the Maintenance protocol if any trigger applied.
9. **Non-obvious gotcha?** An entry was appended to "Encountered Lessons" below.
10. **Pushed to origin.** A commit not on the remote is not "done."

## Maintenance protocol

| Trigger | Where it goes |
|---|---|
| New convention discovered | "Folder conventions" or "Task patterns" (index here; long form in `docs/agent/task-patterns.md`) |
| Same mistake hit twice | "Things to avoid" (summary here; long form in `docs/agent/redaction-rules.md`) |
| New top-level dependency | "Stack" table; run `/agentic-sync` to refresh component skills |
| New gotcha in a pinned component | "Critical gotchas" or the relevant per-stack skill |
| New ambiguous decision | "Decision tables" |
| New product behaviour contract | `docs/spec/<area>.md`; refresh the matching summary in `SPEC.md` |

**Three tiers:** always-relevant → this file; file-type-specific → `.claude/skills/<component>-tasks/SKILL.md` (auto-loads via `paths:` glob); long-form → `docs/agent/*.md` and `docs/spec/*.md`; one-off → "Encountered Lessons" below (append-only, archive cap ~10 entries; older lessons graduate to `docs/agent/encountered-lessons.md`).

## When unsure

Prefer fewer abstractions, explicit over clever. Ask before adding a top-level dependency or a new terminal-renderer package — renderers are an architectural surface, and the stack is deliberately small. For product-decision ambiguity, ask in chat rather than guess in code.

<!-- agentic-init: curated above this line; do not auto-overwrite content above when running /agentic-sync -->

---

## Encountered Lessons

> Append-only by agents. Owner graduates older entries to `docs/agent/encountered-lessons.md`. Cap inline at ~10 entries.

**Format:** `YYYY-MM-DD · situation · what was learned · what to do next time`

**Add when:** >15 min debugging with non-obvious cause; documented pattern didn't apply; runtime gotcha not captured anywhere. **Don't add:** routine bugs in your own newly-written code; things already covered above.

Older entries (2026-04-28 through 2026-05-04) live in [`docs/agent/encountered-lessons.md`](docs/agent/encountered-lessons.md).

---

- 2026-05-06 · nginx `return` + `add_header Content-Type` emits a duplicate Content-Type · `deploy/nginx/web.conf.template`'s `/_web_health` originally used `return 200 "ok\n"; add_header Content-Type text/plain;`. nginx serves the inline `return` body with a default `Content-Type: text/html` AND then `add_header` appends a second `Content-Type: text/plain` — the response carries both headers. Observed during the production image deployment smoke. · For static inline responses produced by `return`, set the body type with `default_type <mime>;` (which replaces the default) instead of `add_header Content-Type ...;` (which adds alongside it). Same rule applies to any future static inline endpoint in this template — `default_type` for type, `add_header` only for orthogonal headers (cache, security).
- 2026-05-06 · AGENTS.md / SPEC.md context split landed · `AGENTS.md` was 56 KB / 275 lines and `SPEC.md` was 407 KB / 2083 lines — the session-start hook surfaced the >40 KB performance warning. Long-prose redaction rules, multi-step task patterns, older Encountered Lessons, and most Surfaces detail moved out of the two top-level files into `docs/agent/*.md` (redaction rules, task patterns, lessons archive, preservation map) and `docs/spec/*.md` (terminal, auth, inventory, recording, web-shell). · When a load-bearing rule moves out of `AGENTS.md` or `SPEC.md`, the matching short summary in the source file MUST link to the destination explicitly (e.g. "(see `docs/agent/redaction-rules.md` § N)"). The preservation map at `docs/agent/context-split-map.md` is the audit trail; cite it in any review that asks "where did rule X go?".
- 2026-05-07 · `pnpm --filter @relayterm/desktop tauri:build` fails at the AppImage stage on CachyOS / Arch with the opaque message `failed to bundle project ´failed to run linuxdeploy´`; `.deb` and `.rpm` build cleanly · Direct invocation of `~/.cache/tauri/linuxdeploy-x86_64.AppImage --appdir RelayTerm.AppDir --output appimage` shows the actual error: repeated `ERROR: Strip call failed: ... unknown type [0x13] section ´.relr.dyn´` lines for libs in `RelayTerm.AppDir/usr/lib/`. Cause: `linuxdeploy` ships a bundled `binutils` whose `strip` predates DT_RELR support, but modern glibc on Arch / CachyOS emits `.relr.dyn` sections in the libs `linuxdeploy` copies in. Cargo workspaces also surface a related path gotcha — release artifacts live at workspace root `target/release/bundle/...`, NOT `apps/desktop/src-tauri/target/release/bundle/...`. · Workaround for the AppImage failure: re-run with `NO_STRIP=true pnpm --filter @relayterm/desktop tauri:build`. Do NOT bake `NO_STRIP=true` into `apps/desktop/package.json` — keep the canonical `tauri build` command and document the host-environment workaround in `docs/deployment/tauri-local-build.md` (under "AppImage strip incompatibility"). Test for the same DT_RELR issue on Phase 1 CI (likely Ubuntu/Debian runners that also emit `.relr.dyn` from modern glibc) before assuming the AppImage stage is green there.
- 2026-05-08 · Tauri v2 capability manifest does NOT gate browser-level WebView navigation · Phase C of the runtime-backend-URL slice (`docs/spec/tauri-runtime-backend-url.md`) initially called for adding a scoped `webview:allow-navigate` capability to `apps/{desktop,mobile}/src-tauri/capabilities/default.json` so the bootstrap picker could call `window.location.assign(<configured-origin>)`. Reading the Tauri v2 docs (`v2.tauri.app/security/capabilities`, `v2.tauri.app/reference/config` → Capability Object) shows capabilities are an **IPC** allow-list for Tauri commands and plugin permissions — they do not restrict browser-level page navigation. WebView navigation is governed by `tauri.conf.json`'s `security.csp` (currently `null` for both shells, so unrestricted) and an optional Rust-side `Builder::on_navigation` hook (no plugin uses it here). With CSP null and no `on_navigation` filter, `window.location.assign(remote)` works under `core:default` alone. · For path A handoffs in this codebase, do NOT add capability rows for navigation. Capability changes are for new IPC commands or plugin permissions only. If we ever want to *constrain* which remote origins the WebView is allowed to navigate to, the right tool is a Rust-side `Builder::on_navigation` allow-list in the desktop / mobile `lib.rs`, NOT a `webview:allow-*` permission (none of those exist for navigation in Tauri 2.x).
- 2026-05-09 · Tauri desktop bundled-shell login smoke through the Phase C remote-web handoff against a throwaway local Compose stack — bootstrap kept failing with the SPA's "request blocked by browser security policy" message (= 403 `csrf_origin_mismatch`) even though both the page origin and the env's `RELAYTERM_AUTH__ALLOWED_ORIGINS` pointed at the loopback · The WebView's saved origin in `localStorage.relayterm.backend-config.v1` was `http://localhost:8081`; the env's initial allow-list was `http://127.0.0.1:8081`. The byte-equality check in `crates/relayterm-api/src/auth/csrf.rs::check_origin` does NOT resolve hostnames or treat loopback aliases as equivalent, and `validateBackendOrigin` in `apps/web/src/lib/runtime/backendConfig.ts` correctly lower-cases the host but does not collapse `localhost ↔ 127.0.0.1` (Origin equality is per RFC 6454 a tuple of scheme/host/port strings). The SPA mapping `403 csrf_origin_mismatch` → "request blocked by browser security policy" lives in `apps/web/src/lib/api/auth.ts::describeAuthError`. · When configuring `RELAYTERM_AUTH__ALLOWED_ORIGINS` for local stacks OR any deployment with multi-alias public URLs (apex + www, IP + DNS, dual-stack v4/v6), enumerate every alias the client may send as an `Origin` — `127.0.0.1`, `localhost`, `[::1]`, externally-resolved DNS names are all distinct strings to the guard. When a "browser security policy" message appears in the SPA on a localhost stack, check `localStorage.relayterm.backend-config.v1` and the env allow-list before suspecting any backend, CORS, cookie, or code-layer issue.
