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
- **Adding a production app-shell view.** Production shell (`apps/web/src/lib/app/`) MUST NOT import from `lib/dev/` and MUST NOT statically import any experimental renderer adapter (`@relayterm/terminal-{ghostty-web,restty,wterm}`). Experimental adapters reach the production shell ONLY via the gated lazy loader at `apps/web/src/lib/app/terminal/rendererLoader.ts` (dynamic `import()` + explicit operator gate); the static-import rule, the single-file rule, and the dynamic-only rule are pinned by `apps/web/tests/appShellIsolation.test.ts`. Extend `AppViewId`/`NAV_ITEMS`, add a `*View.svelte` (use `PlaceholderView` with honest copy), wire into `AppShell.svelte`, extend `tests/navigation.test.ts`. Never show fake data or any `private_key`/`encrypted_private_key` field. See `docs/agent/task-patterns.md` § 2.
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
| Import from `lib/dev/` inside `apps/web/src/lib/app/`, OR statically import any experimental renderer adapter (`@relayterm/terminal-{ghostty-web,restty,wterm}`), OR reference an experimental adapter package name outside `apps/web/src/lib/app/terminal/rendererLoader.ts` | Production shell stays dev-free. xterm is the production compatibility baseline and the default renderer; the experimental adapters reach the production shell ONLY through the gated lazy loader at `apps/web/src/lib/app/terminal/rendererLoader.ts`, via dynamic `import()`, AND ONLY when the operator has flipped the `experimentalRendererEvaluationEnabled` gate in Settings. The static-import rule, the single-file rule, and the dynamic-only rule are all pinned by `apps/web/tests/appShellIsolation.test.ts`. (`docs/agent/redaction-rules.md` § 13) |
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
- 2026-05-09 · Path A bundled-shell handoff against a local Compose stack ended in a "Connecting…" splash that flashed indefinitely after pressing Connect — the post-handoff page kept reloading itself · The Tauri v2 WebView injects `__TAURI_INTERNALS__` on remote-origin pages too, so after `window.location.assign(${origin}/)` the served SPA at `http://localhost:8081/` ALSO ran `ConfiguredBackendGate`, found its own (just-saved) `relayterm.backend-config.v1` at the new origin's `localStorage`, and the original `decideHandoff` scheduled another `assign(${origin}/)` against the same origin — looping the page. Empirical proof was `~/.local/share/cc.js-node.relayterm.desktop/localstorage/http_localhost_8081.localstorage` getting written when the picker was answered at the remote origin (that file only exists if `isTauriRuntime()` returned true there). The Tauri v2 docs do not call this out — the `__TAURI__`/`__TAURI_INTERNALS__` injection is a per-WebView boot script, not a per-`frontendDist`-only injection. · Any path-A or Phase C-style handoff guard MUST short-circuit on `currentOrigin === backendOrigin` BEFORE scheduling navigation — not only on `isTauriBootstrapEnabled()`. The fix lives in `apps/web/src/lib/runtime/backendHandoff.ts` (`decideHandoff` returns `{ kind: "passthrough", reason: "already_at_backend" }` when `cfg.backendOrigin` byte-equals `window.location.origin`); pinned by the `decideHandoff — same-origin short-circuit (already_at_backend)` block in `apps/web/tests/backendHandoff.test.ts`. Origin equality is byte-strict, mirroring `crates/relayterm-api/src/auth/csrf.rs::check_origin` and the entry above (no normalisation collapses `localhost ↔ 127.0.0.1`). Do not assume the Tauri global is bundled-origin-only; an `isTauriBootstrapEnabled()`-style runtime predicate runs at every origin the WebView visits, and any decision that triggers a navigation MUST be idempotent against running again at the navigation's destination.
- 2026-05-09 · While iterating on the path-A handoff fix, the SAME observable failure (post-handoff splash flashing) reproduced AFTER a real fix had landed and the new bundle was demonstrably reaching the host (`curl http://127.0.0.1:8081/assets/index-<hash>.js` from the host returned the new content). Spent ~20 min wondering why the fix's unit tests passed locally but the WebView still looped · `deploy/nginx/web.conf.template` sets `Cache-Control: public, immutable` + `max-age=31536000` on `/assets/*` (correct for production, where Vite's content-hashed filenames make every release land at a NEW URL — the cache key changes). On a local-stack iteration that swaps the served bundle in place via `docker cp apps/web/dist/. <web-container>:/usr/share/nginx/html/` + `nginx -s reload`, the asset URL is identical, so the desktop WebView's WebKitGTK HTTP cache (`~/.local/share/cc.js-node.relayterm.desktop/WebKitCache/`) keeps serving the OLD bundle for a year and never re-fetches. Production deployments NEVER hit this. · For local-stack iteration where the served bundle is being replaced in-place (any docker-cp + nginx reload pattern on the `:main` image), wipe `~/.local/share/cc.js-node.relayterm.desktop/{WebKitCache,CacheStorage}` between iterations, OR rebuild the web image so the asset hash actually changes. Never assume a desktop relaunch picks up a hot-swapped bundle. Detail in `docs/deployment/tauri-local-build.md` Troubleshooting "WebKit HTTP cache + nginx immutable assets".
- 2026-05-09 · Adding the configurable detached-PTY TTL knob (`RELAYTERM_TERMINAL_SESSIONS__DETACHED_LIVE_PTY_TTL_SECONDS`) wired the env var into `deploy/relayterm.env.example` and `docs/config-examples/*.toml` but missed two of the three Compose templates (`docker-compose.example.yml` and `docker-compose.traefik-staging.example.yml` got it; `docker-compose.images.example.yml` did NOT). The drift was discovered only during the staging smoke; CI passed because no test exercised the image-mode template. · When introducing a new operator env knob, update **all** of: `deploy/relayterm.env.example`, every `deploy/docker-compose*.example.yml` that ships explicit `environment:` mappings, both `docs/config-examples/*.toml` examples, AND the matrix in `scripts/check-doc-contracts.sh` §9 ("Deploy config plumbing — env var × file matrix"). The contract is enforced by `bash scripts/check-doc-contracts.sh` / `pnpm run check:docs-contracts` and gated by Forgejo CI's `web-checks` job. Per-file intentional omissions (e.g. dev TOML omitting `RELAYTERM_AUTH__SESSION_SIGNING_KEY_B64`) MUST be encoded explicitly in the matrix loop with a justifying comment, not as a silent skip.
- 2026-05-11 · Routing Forgejo Actions jobs to the LXC runners with DinD, every job that declared its own `container: image: ...` block found that the runner-config.yml's `container.options` (which tries to inject `--add-host=docker:host-gateway -e DOCKER_HOST=tcp://docker:2375`) did NOT reach the spawned job container — confirmed empirically in this environment via `docker inspect` on a live job container showing `HostConfig.ExtraHosts: null` and no `DOCKER_HOST` in `Config.Env`. Adding the same `options` field on the workflow's own `container:` block had the same null result. Symptom: `docker` CLI inside the job falls back to `/var/run/docker.sock` (not mounted in this DinD setup) and the build fails before any source steps run. No upstream issue tracker was searched in this slice; treat as observed-in-our-Forgejo-runner-setup rather than a confirmed upstream regression. · Use `./.forgejo/actions/dind-docker-host` (parses `/proc/net/route` for the default-route gateway, then writes `DOCKER_HOST=tcp://<gw>:2375` to `$GITHUB_ENV`) instead of relying on `container.options` propagation. Every workflow job that talks to the DinD daemon — `ci.yml::docker-build`, `ci.yml::publish-images`, and any future docker-touching job — MUST reference this composite right after `checkout`; do not re-inline the gateway-discovery shell.
- 2026-05-11 · `docker/setup-buildx-action@v3` (which under the hood runs `docker buildx create --driver docker-container`) silently spawned a BuildKit builder container with `HostConfig.DeviceRequests: [{Driver:"", Capabilities:[["gpu"]]}]` against our DinD daemon, which has no GPU runtime, so the container start request was rejected with `could not select device driver "" with capabilities: [[gpu]]`. Per `docs.docker.com/build/building/cdi`, buildx v0.22 introduced "Automatic GPU Detection" that adds the equivalent of `--gpus=all` to the builder container whenever the host running buildx has NVIDIA kernel drivers loaded — in this environment that is the outer LXC docker daemon (epyc-ai also hosts AI workloads). No `--no-gpus` / `BUILDX_NO_GPU` flag exists in buildx as of 2026-05; pinning the CLI is the documented workaround per `docs.docker.com/build/ci/github-actions/configure-builder` ("Pin Buildx Version"). Pinning the BuildKit image (`driver-opts: image=moby/buildkit:v0.12.5`) did NOT help because the `--gpus=all` flag is added by the buildx CLI before the BuildKit image runs. · `setup-buildx-action@v3` in `.forgejo/workflows/ci.yml::publish-images` is pinned to `version: v0.21.3` (the last release before v0.22 added autodetect). Keep the pin until upstream ships an opt-out flag, then bump deliberately. If a future workflow also needs buildx, mirror the same pin — do not call `setup-buildx-action` without `version:` on this runner host.
- 2026-05-11 · The same host on epyc-ai runs both the Forgejo runner DinD stack AND AI workloads that need NVIDIA passthrough, so the outer LXC docker daemon registers `nvidia-container-runtime` in `/etc/docker/daemon.json`'s `runtimes:` map (not as `default-runtime`). Even though the runner DinD daemon spawned inside that outer docker has no GPU runtime itself, buildx v0.22's autodetect (previous lesson) probes the buildx-CALLER's kernel, not the target daemon — so the GPU device-request leaked into our backend image build path despite RelayTerm's CI not needing any GPU surface. Observed in this Forgejo runner setup; not a generic Docker behaviour claim. · Before adding registry cache exporters (`cache-to=type=registry`), reconfiguring DinD to share the outer daemon's socket, GPU runtime tuning on the outer LXC, or any other change that lets the outer-daemon environment further bleed into runner-side builds, re-check whether the runner DinD daemon remains the isolation boundary it is today. Concretely: keep `forgejo-dind` as the only docker daemon RelayTerm CI talks to (the `dind-docker-host` composite enforces this via `DOCKER_HOST=tcp://<dind-gateway>:2375`), and treat any deviation from that as a separate operator decision with its own audit of GPU/runtime leakage paths.
