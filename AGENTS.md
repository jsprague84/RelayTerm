# AGENTS.md

> Instructions for AI coding agents working in this repo. Read fully at session start. Re-read when stuck.

## Project: RelayTerm

A web/mobile SSH terminal where sessions live on the backend, clients can detach and reconnect, and the terminal renderer is replaceable. Built as a multi-language monorepo: a Rust/Axum backend that owns the SSH sessions, a Svelte 5 + Vite + Tailwind v4 web frontend, a Tauri v2 desktop shell, a Tauri v2 mobile (Android-first) shell, and a small set of swappable terminal-renderer packages.

**Owner:** <<TODO: OWNER_NAME>>
**Production URL:** <<TODO: PRODUCTION_URL>>
**Repo:** <<TODO: REPO_PATH>>

For the product spec, see `SPEC.md`. For situational rules per file type, see `.claude/skills/`. For one-off observations, see "Encountered Lessons" at the bottom.

## Architectural rule (load-bearing)

This rule is what makes RelayTerm different from a normal web terminal. Every change must respect this separation. If a piece of code blurs it, **stop and ask**.

- **The SSH session belongs to the backend.** russh holds the live connection.
- **The terminal renderer belongs to the frontend.** xterm.js / wterm / ghostty-web / restty are interchangeable adapters; none owns state.
- **The terminal state belongs to the session orchestrator.** It owns the output sequence numbers, the replay ring buffer, and (eventually) the libghostty-vt state engine.
- **The client is allowed to disappear and come back.** Reconnect by sequence-number replay. Never assume a single live socket per session.

## Session start ritual

The plugin's `SessionStart` hook runs the baseline checks below automatically.

1. Read this file and `SPEC.md`.
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
| ghostty-web | `0.4.0` | libghostty-vt parser via WASM; xterm.js-API-compatible `Terminal`. WASM payload is inlined as a base64 data URL inside the shipped JS — no separate asset wiring needed under Vite. Used only by `@relayterm/terminal-ghostty-web`; consumed via the renderer-neutral `TerminalRenderer` interface. Requires `await init()` once before constructing a `Terminal`. |
| restty | `0.1.x` | libghostty-vt + WebGPU/WebGL2 + text-shaper experimental renderer. Used only by `@relayterm/terminal-restty`; the adapter binds to the focused xterm-compatibility shim at `restty/xterm` (the native `Restty` pane/plugin/shader surface is intentionally NOT promoted). `restty/xterm`'s `Terminal.write(data)` takes `string` only — the adapter UTF-8-decodes `Uint8Array` writes before forwarding. restty's own `package.json` declares `engines.bun >= 1.2.0` for its dev workflow; this is a runtime hint for restty contributors and is irrelevant to RelayTerm — pnpm/Node installs the shipped `dist/*` fine. Ships ~3 MB JS plus an inlined WASM binary; `sideEffects: false` on the adapter keeps it tree-shaken from any non-dev bundle. |
| @wterm/dom | `0.2.x` | DOM-rendered terminal emulator (Zig+WASM core wrapped by a CSS-themed grid renderer); the DOM/mobile/accessibility-oriented experimental adapter. Used only by `@relayterm/terminal-wterm`. `WTerm`'s constructor takes the host element AND synchronously mutates it (appends a child grid div, adds the `.wterm` class, attaches a click listener) — the adapter therefore defers BOTH construction AND `await wterm.init()` to its own `mount(element)`. `WTerm.write(data)` accepts `string \| Uint8Array` directly via the `WasmBridge`, so no UTF-8 decode step is needed (unlike `restty/xterm`). `autoResize` defaults to `true` upstream (attaches an internal `ResizeObserver`); the adapter flips the default to `false` for parity with the other renderers and exposes opt-in via `wtermOnly.autoResize: true`. Theming/typography goes through CSS variables on the `.wterm` host (see `@wterm/dom/src/terminal.css`), not `WTermOptions` — the neutral cosmetic knobs accepted on the adapter surface are silently dropped during option mapping. The optional `@wterm/dom/css` import lives in the dev-lab module and disappears with it under tree-shaking. `@wterm/core` inlines its WASM as a base64 module (~17 KB), so no separate asset wiring is needed under Vite. |
| tauri | `^2` | Adds Android/iOS; v1 conf schema is incompatible. |
| ssh-key | `^0.6` | OpenSSH keypair gen + `authorized_keys` text + SHA-256 fingerprint. RustCrypto; pulls `ed25519` feature only — no RSA/ECDSA generators yet. |
| chacha20poly1305 | `^0.10` | XChaCha20-Poly1305 AEAD for the vault envelope. 24-byte nonce → safe random nonces. `alloc` feature; no `std`. |
| zeroize | `^1` | Wipes vault secrets (master key, plaintext PEM, b64 source string) on drop. `derive` feature for `ZeroizeOnDrop`. |
| rand | `^0.8` | `OsRng` for nonce + keypair generation. `0.8` line is what `ssh-key 0.6` and `chacha20poly1305 0.10` interop with via `rand_core 0.6`. |
| tokio-tungstenite | `^0.29` (dev-dep) | WebSocket client used only by `relayterm-api` integration tests to drive the `/api/v1/terminal-sessions/:id/ws` route against an in-process `axum::serve`. Pinned to match the `tungstenite` axum 0.8 pulls in transitively so the lockfile keeps a single copy. Not a runtime dep. |
| base64 | `^0.22` | Standard-alphabet RFC 4648 codec. The protocol encodes raw PTY output bytes as base64 inside `ServerMsg::Output { data }` (JSON strings can't carry arbitrary binary). Centralised in `relayterm-protocol::output_data_encode/decode`; the TS mirror uses `atob`/`btoa` against the same alphabet. A binary frame format is future work. The `relayterm-auth::session_token` module also uses the URL-safe-no-pad alphabet (`URL_SAFE_NO_PAD`) for the cookie value — 32 random bytes encode to 43 ASCII chars in the `A-Za-z0-9-_` set. Do not switch the session token to the standard alphabet; `+` and `/` need percent-encoding inside a `Set-Cookie` header. |
| argon2 | `^0.5` | Argon2id password hashing via the RustCrypto `password-hash` integration (re-exported as `argon2::password_hash::*`). `std` feature enabled for PHC `Display`/`FromStr`; defaults already include `password-hash` + `rand` (per-password random salt via `OsRng`). Used only by `relayterm-auth::password::PasswordHasher`. Default parameters are `PasswordHasherConfig::OWASP_2023` (`m=19456 KiB`, `t=2`, `p=1`); the `m` parameter is **already in kibibytes** — do NOT multiply by 1024. PHC strings (`$argon2id$...`) carry parameters and per-password salt inline so a future parameter upgrade can re-hash on next successful login without a schema column. There is no separate `password-hash` workspace entry; the API is consumed via the re-export. |

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
- **tauri v2** — `tauri.conf.json` is split into `app`/`build`/`bundle`/`plugins`; capabilities replace v1 allowlist; `tauri android init` scaffolds under `src-tauri/gen/` (do not edit by hand); mobile builds via `pnpm tauri android build --aab`.

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
├─ Cargo.toml                  # workspace root
├─ pnpm-workspace.yaml
└─ AGENTS.md / SPEC.md / CLAUDE.md
```

**Tauri shells.** The desktop and mobile shells live in `apps/desktop/` and `apps/mobile/` respectively, NOT inside `apps/web/`. Each shell has its own `src-tauri/` and consumes the built web frontend from `apps/web/`. Do not collapse them back under `apps/web/src-tauri/` — that layout is obsolete.

If you're tempted to invent a new directory, propose it here first.

## Task patterns

> TODO: fill in numbered procedures for RelayTerm's recurring changes.

- **Adding a new terminal renderer adapter.** Mirrors the shape established by `@relayterm/terminal-xterm` (baseline), `@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`, and `@relayterm/terminal-wterm`. The architectural rule: `terminal-core` stays renderer-agnostic, and the backend protocol stays renderer-neutral — never reshape either to accommodate a renderer. Steps:
  1. Scaffold `packages/terminal-<name>/` (package name `@relayterm/terminal-<name>`); implement `TerminalRenderer` from `@relayterm/terminal-core`.
  2. Keep exports minimal and renderer-neutral. Extend `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (which carries the shared `fontFamily`/`fontSize`/`lineHeight`/`cursorStyle`/`cursorBlink`/`scrollbackLines`/`theme` shape, `RendererTheme`, `RendererThemeAnsi`, and `RendererCursorStyle`) — DO NOT redefine these neutral types in the adapter. Renderer-specific knobs go behind a local `<renderer>Only` escape hatch on the options object — never on the `TerminalRenderer` surface, never on `BaseTerminalRendererOptions`.
  3. Do NOT add the renderer's runtime as a dep of `terminal-core`. Only the adapter package depends on the underlying lib.
  4. Add adapter unit tests (vitest). Mock the underlying terminal when WASM/WebGPU/DOM/jsdom is awkward — see `terminal-ghostty-web`, `terminal-restty`, and `terminal-wterm` tests for the mock pattern.
  5. Add redaction tests covering input, output, log, and error paths. Raw terminal bytes/strings must never appear in console, logs, or thrown error messages.
  6. Wire the package into `apps/web` ONLY for the dev lab: register an id/label in `apps/web/src/lib/dev/rendererDiagnostics.ts` and add creation/switching to `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte`. Do not promote experimental renderers into the main app surface.
  7. Update the Stack table in this file with the package, version pin, and any API caveats (UTF-8 decode requirements, async init, asset/WASM wiring, bundle size, tree-shaking flags). Update `SPEC.md` with adapter limitations and tree-shaking notes.
  8. Verify the production bundle: confirm the new package is tree-shaken out of any non-dev build (`sideEffects: false` on the adapter, no top-level imports from app code).
  9. Add a `data-testid="renderer-option-<id>"` attribute to the new radio in `XtermLiveTerminalLab.svelte` and extend the smoke selectors in `apps/web/e2e/SMOKE.md`. Re-run the manual Playwright MCP smoke (dev + production halves) so the new option is in the verified set. The smoke is intentionally manual; if it ever needs to be a committed runner, that is its own slice.

  Recurring rules for renderer work:
  - `xterm` is the compatibility baseline and the default. Don't change the default without an explicit ask.
  - Experimental renderers must be labeled experimental in UI, diagnostics, and docs.
  - Renderer diagnostics in `rendererDiagnostics.ts` are metadata only — not formal benchmarks. Don't present them as perf claims.
  - Renderer-specific APIs must not leak into `TerminalRenderer` or `terminal-core`.
  - The backend protocol does not change to accommodate a renderer. If a renderer needs new data, it transforms what's already on the wire.
  - Raw terminal input/output (keystrokes, PTY bytes, decoded strings) must never be logged or surfaced outside the terminal viewport.
- **Adding a new backend WebSocket message type** — define in `apps/backend/src/http/` protocol module; mirror schema with the web `lib/ws/` client. ...
- **Adding a production app-shell view.** The production shell lives under `apps/web/src/lib/app/`. Production components MUST NOT import from `lib/dev/` or any experimental renderer adapter package (`@relayterm/terminal-{ghostty-web,restty,wterm}`); the production terminal workspace uses `@relayterm/terminal-core` + `@relayterm/terminal-xterm` (the baseline) only, and the experimental adapters stay dev-lab-only. To add a view: (1) extend `AppViewId` and `NAV_ITEMS` in `lib/app/navigation.ts` (id, label, description); (2) add a `*View.svelte` under `lib/app/views/` — placeholders should compose `PlaceholderView.svelte` with honest copy ("not implemented yet", a short bullet list of what currently exists, and a `futureWork` note); (3) wire the new id into the `{#if}` chain in `AppShell.svelte`; (4) extend the navigation tests in `tests/navigation.test.ts`. Do NOT show fake data, mock secret values, or any `private_key`/`encrypted_private_key` field. Update `apps/web/e2e/SMOKE.md` if a new stable selector should be in the verified set, and update SPEC.md "Production web app shell" if the contract changes.
- **Fetching backend data from a production view.** Use the typed helpers in `apps/web/src/lib/api/` and the shared error envelope from `apiErrors.ts`. Steps: (1) add a `parseX(raw: unknown): X | null` runtime guard in the resource module — construct the DTO field-by-field so unknown extra fields are dropped silently and a stray `private_key` / `encrypted_private_key` cannot smuggle onto the parsed object; (2) call `fetchJsonList(endpoint, parseX)` so transport, HTTP, and parse failures collapse to a single typed `LoadError`; (3) format UI strings via `describeLoadError(label, err)` — NEVER echo the wire `message` of an HTTP error or the thrown `Error.message` of a transport failure in any string that reaches the DOM; (4) render explicit loading / empty / error / ready states (no auto-retry storms, no polling unless explicitly scoped); (5) for SSH-identity-shaped data, do NOT declare `encrypted_private_key` / `private_key` on the TypeScript interface AND add sentinel-string redaction tests asserting absence in the parsed object, in `JSON.stringify` of the parsed object, and in any formatted preview / copy string.

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
| Redefine `RendererTheme`, `RendererThemeAnsi`, `RendererCursorStyle`, or `BaseTerminalRendererOptions` inside an adapter package | Import them from `@relayterm/terminal-core`; extend `BaseTerminalRendererOptions` for the adapter's option interface |
| Import from `lib/dev/` inside `apps/web/src/lib/app/`, OR import an experimental renderer adapter (`@relayterm/terminal-{ghostty-web,restty,wterm}`) inside `apps/web/src/lib/app/` | Production shell stays dev-free; the production terminal workspace uses `@relayterm/terminal-core` + `@relayterm/terminal-xterm` (the baseline) only. Reach the dev lab via the `devTools` snippet in `App.svelte`, gated by `import.meta.env.DEV` — see `tests/appShellIsolation.test.ts` |
| Show fake data, mock secret values, or a `private_key`/`encrypted_private_key` field on a placeholder view | Use `PlaceholderView` with honest copy: a one-line summary, a "what currently exists on the backend" bullet list, and a `futureWork` note |
| Add a delete / disable / archive / hard-revoke route or UI without consulting the lifecycle policy | Read `SPEC.md` "Inventory lifecycle and destructive-action policy" first. Default user-facing destructive action for `server_profiles` is disable (not delete). `hosts`/`ssh_identities` delete is blocked while a `server_profile` references them. `terminal_sessions` are never deleted from the user UI. Every destructive action writes one audit event with public metadata only |
| Append an audit row on a redundant/idempotent lifecycle call (re-disable, re-enable, no-op trust, etc.) | Audit only on the actual state transition. The route's idempotency early-return MUST sit *before* the audit append, so a no-op call returns the unchanged row and writes zero rows. SPEC.md "Server profile lifecycle audit" is the canonical pattern |
| Put `encrypted_private_key`, plaintext PEM bytes, public-key bytes, raw russh / DB error text, peer banners, terminal I/O, vault internals, or `client_info` blobs in an `audit_events.payload` | Public metadata only — ids, names, fingerprints (public), `key_type`, timestamps, reference counts, reason codes. Build the JSON object field-by-field from a small helper; mirror `write_lifecycle_audit` in `routes/v1/server_profiles.rs`. Sentinel-string tests against `AUDIT_FORBIDDEN_SUBSTRINGS` are the redaction backstop |
| Stash, log, or pass-around the plaintext value of a `SessionToken` after the cookie is set, OR build any storage/lookup index on it | The plaintext crosses the service boundary EXACTLY ONCE — as the `token` field of `CreatedSession` returned from `AuthService::create_session`. The HTTP layer puts the bytes in `Set-Cookie` and drops the wrapper. Storage and lookup are by `SessionTokenHash` (SHA-256 of the encoded token). The wrapper redacts in `Debug`, has no `Display`, has no `serde`, and zeroizes on drop — keep it that way. A logged token + a DB dump = full session takeover, so the plaintext is treated like a vault private-key plaintext: visible on exactly one wire surface, never persisted, never logged |
| Add `Display`, `serde`, or any `as_bytes() -> &[u8]` accessor to `SessionToken`, OR widen `SessionToken::expose()` to public callers other than the `Set-Cookie` writer | `expose()` exists for the cookie-writing route ONLY. Repository inserts go through `SessionTokenHash::into_bytes()`; lookups go through `SessionTokenHash::as_bytes()`. Any new caller of `expose()` is a redaction regression — push the requirement up to `SessionTokenHash` instead, or talk to the auth-service surface |
| Tune `argon2` parameters below `PasswordHasherConfig::OWASP_2023` in production (`m=19456`, `t=2`, `p=1`) | Test-only fast paths construct `PasswordHasherConfig { m_cost: 19_456, t_cost: 1, p_cost: 1 }` explicitly. Production callers MUST use `PasswordHasher::default()`. `password::tests::default_uses_owasp_2023` pins the default constants — a PR that weakens them MUST update the test in the same commit and explain why (an ADR is appropriate) |
| Add a state-changing browser-write route that touches DB, auth, OR a body extractor without running the shared CSRF / `Origin` guard FIRST | Place `_csrf: CsrfGuard` (`relayterm_api::CsrfGuard`) ahead of `Json<...>` / `Form<...>` / any other body extractor in the handler signature, OR call `auth::csrf::check_origin(&headers, &state.auth_routes.allowed_origins)?` before the first DB / auth / body access. Wire policy is `403 csrf_origin_mismatch`; `GET`s are exempt. Never echo the offered `Origin` value in either the wire body OR the operator-side `warn!` line. **Note on ordering:** in axum 0.8 every `FromRequestParts` extractor runs before the single `FromRequest` body extractor regardless of source order, so the "ahead of `Json<...>`" placement is **conventional** (documents intent, keeps the call site self-explanatory) rather than load-bearing — rearranging the signature does not break the rejection-before-body-parse guarantee. Still pin the guarantee with an integration test that POSTs malformed JSON + a disallowed Origin and expects 403, not 400 — see `bad_origin_rejects_before_body_parsing` in `crates/relayterm-api/tests/api.rs` |
| Add a protected `/api/v1/*` route that pulls the caller's `UserId` from anywhere other than `AuthenticatedUser` (e.g. an `Option<UserId>` extracted from state, a header, a query string, or a re-introduced dev shim) | Take `user: AuthenticatedUser` and bind the id via `user.user_id()`. Browser-write handlers additionally take `_csrf: CsrfGuard` as the first parameter; WS / GET routes take `AuthenticatedUser` only (no `CsrfGuard`). Owner-scope every repository read by `owner_id == user.user_id()` and collapse foreign-vs-missing to a byte-identical 404. The handler must NEVER reach the session token, the token hash, or the session row — only the resolved `UserId` / `User`. The legacy `DevUser` extractor and the `dev@relayterm.local` fixture user are gone; production runs through `AuthenticatedUser` only and dev mode runs through the same code path with relaxed boot-time validation. Pin the auth gate with an integration test that hits the route with no cookie and expects 401 (use `json_post_no_auth` / `get_no_auth` from the test fixture) — see `protected_hosts_routes_return_401_without_session_cookie` for the canonical shape |
| Touch the login throttler with the raw password, the offered email pre-normalization, OR a key built from anything other than `relayterm_auth::normalize_login_identifier(&email)`. Don't `tracing::*` the throttle key. Don't add a `Retry-After` header to the 429 (would leak countdown telemetry to a probe). Don't gate the throttle behind "user exists" — that re-introduces the probe channel. Don't `record_failure` on the success path. Don't `record_success` on the failure path. Don't bypass the throttler for any login branch (unknown-email AND wrong-password BOTH must record). Don't reach the throttler from a CSRF-rejected handler — the `CsrfGuard` extractor must short-circuit FIRST so a third-party origin cannot lock out a legitimate user | Call `state.login_throttler.check(&throttle_key, now)` AFTER `CsrfGuard` + `req.validated()` and BEFORE the user lookup. Build `throttle_key = normalize_login_identifier(&req.email)` and never log it. On the throttled branch return `ApiError::TooManyRequests(...)` and write `login_failed { reason: "throttled" }` best-effort. On the failure branch call `record_failure(&throttle_key, now)` BEFORE the audit append so the bookkeeping is symmetric with the wire return. On the success branch call `record_success(&throttle_key)` BEFORE `create_session` — a transient session-mint failure that leaves the bucket cleared but the user not-yet-logged-in is the kinder failure mode (next retry is not pre-throttled), and once the password verify has succeeded the bucket-clear is the right side of the trade-off. The integration tests in `crates/relayterm-api/tests/api.rs::login_throttle_*` pin every property; if you add a new login branch, add a matching test in the same module |
| Stash paste content in Svelte `$state`, persist it to local/sessionStorage, route it through the audit-log surface, render it inside the paste-confirm or paste-blocked panel body, OR include it in any thrown `Error.message` / `console.*` / `data-*` attribute. Don't bypass `evaluatePaste` for "trusted" / "small" / "internal" paths in the production terminal workspace. Don't widen `PasteDecision` to carry the source text. Don't add a backend command-inspection / shell-aware paste analysis surface | Hold the original paste text in a script-scoped `pendingPasteText` variable on `ProductionTerminal.svelte` between `evaluatePaste` returning `confirm` and the operator's confirm/cancel click. Snapshot-and-clear the variable in the Send-paste handler before calling `client.sendInput` exactly once. Render the confirm/blocked panels from `PasteDecision` METADATA only (line count, byte length, reason code, the static `safeUserMessage`). Pin the redaction with sentinel tests in `tests/pastePolicy.test.ts` that assert the decision object — across `safe` / `confirm` / `blocked` outcomes — never carries a sentinel string from its input through any field, JSON form, or String() form. SPEC.md "Production terminal paste safety" is the canonical contract; the policy is shape-based (newlines, size, control chars), not semantics-based |
| Put `terminal_recording_chunks.payload` bytes (or the bytes from any future encrypted/compressed chunk envelope) into a `tracing::*` line, an `audit_events.payload`, a thrown `Error`, an HTTP error body, a session-list / dashboard / activity cell, a `data-*` attribute, frontend `localStorage` / `sessionStorage`, or any `Debug` impl that formats the bytes themselves. Don't add a `Display` / `Serialize` impl on `TerminalRecordingChunk` or `CreateTerminalRecordingChunk` that exposes `payload`. Don't widen the recording repository surface to a cross-session lister; the trait is session-scoped on purpose. Don't add a recording read route that pulls the caller's `UserId` from anywhere other than `AuthenticatedUser`, that skips the `terminal_sessions.owner_id == user.user_id()` filter on the addressed session, OR that surfaces chunk bytes in any field other than the explicit `data_b64` base64 surface. Don't echo a chunk's `data_b64` (or any other base64 form of payload) into a `tracing::*` line, an error body, or an audit row — base64 is wire-shape only, NOT a redaction layer. Don't write an `audit_events` row from a recording read endpoint | Treat chunk `payload` like a vault private-key plaintext: the bytes cross the service boundary ONLY through the parsed domain field. The `TerminalRecordingChunk` and `CreateTerminalRecordingChunk` `Debug` impls redact `payload` to `<redacted: N bytes>`. The `TerminalRecordingChunkRow` SQLx row deliberately does NOT derive `Debug` — convert through `try_into_domain()` first. Repository errors go through `map_sqlx_error` which strips driver text down to the entity name plus the constraint. Marker `payload` is metadata-only by contract: build the JSON object field-by-field (counts, dims, reason codes, enum tags), never `serde_json::to_value` a bag of arbitrary types. Owner-scope happens at the API layer (the `terminal_sessions.owner_id == user.user_id()` join belongs in the route, not the repository); the repository takes `terminal_session_id` and trusts the caller. The read API surface (`/api/v1/terminal-sessions/:id/recording/{metadata,chunks,markers}`) ONLY returns chunk bytes through `TerminalRecordingChunkResponse::data_b64`, base64-encodes inside the handler, never logs the encoded form, and writes zero audit rows. Foreign / unknown sessions collapse to a byte-identical 404 via `resolve_owned_session`. SPEC.md "Durable terminal recording and replay architecture" + `docs/terminal-recording.md` Section 7 + Section 10 are the canonical contracts; the redaction backstops live in `crates/relayterm-core/src/terminal_recording.rs` tests, `crates/relayterm-db/tests/repositories.rs::recording_chunk_payload_not_in_error_or_debug`, and `crates/relayterm-api/tests/api.rs::recording_*` |
| `SELECT payload` (or any column derived from `payload`) inside the retention purge primitive — including `SELECT length(payload)`, `SELECT octet_length(payload)`, or a `RETURNING payload, ...` on the chunk DELETE — and don't aggregate the byte total any way other than `COALESCE(SUM(byte_len), 0)` on `terminal_recording_chunks`. Don't add a marker payload read inside the purge path (counts only via `COUNT(*)`). Don't echo the marker `payload` JSON into the `recording_purged` audit row, an error body, or a `tracing::*` line. Don't widen the purge primitive to take caller-supplied chunk-id / marker-id lists; the surface is `(terminal_session_id, retention_days, now)` only. Don't relax audit-failure rollback into a two-phase pattern (commit deletes first, then audit) — the recording purge is irreversibly destructive and the fail-closed transactional shape is load-bearing. Don't drop or re-order the `FOR UPDATE` lock on `terminal_sessions` inside the purge transaction; a concurrent sweep against the same session id (multi-instance deployment, racing tests) MUST serialise. Don't write a `recording_purged` audit row with `actor_id != NULL`; the cleanup worker is the system, not a user, and the user-facing `recent_for_actor` feed deliberately excludes NULL-actor rows | Use `TerminalRecordingRepository::purge_for_retention(input)`. The Postgres impl in `crates/relayterm-db/src/repositories/terminal_recording.rs` is the canonical pattern: `BEGIN`, `SELECT closed_at FROM terminal_sessions WHERE id = $1 AND closed_at IS NOT NULL AND closed_at + make_interval(days => $2) <= $3 FOR UPDATE`, then `SELECT COUNT(*), COALESCE(SUM(byte_len), 0) FROM terminal_recording_chunks WHERE terminal_session_id = $1`, then `SELECT COUNT(*) FROM terminal_recording_markers ...`, then `DELETE FROM terminal_recording_markers ...`, then `DELETE FROM terminal_recording_chunks ...`, then `INSERT INTO audit_events (...) VALUES (..., NULL, 'recording_purged', $payload, NULL, $now)`, then `COMMIT`. Predicate (3) — at least one chunk OR marker row exists — is the schema-side idempotency keystone: a session whose chunks AND markers were already deleted falls out as `Ok(None)` with no audit row written. The `recording_purged` payload is built field-by-field from primitives (`target_kind: "terminal_session"`, `target_id`, `chunk_count`, `marker_count`, `bytes_purged`, `retention_days`, `closed_at`, `purged_at`, `reason: "retention_expired"`) — never `serde_json::to_value` of a domain struct, never per-chunk seq ranges, never per-chunk byte counts. The redaction backstop is `crates/relayterm-db/tests/repositories.rs::purge_for_retention_audit_payload_redacted` (chunk byte sentinel + marker payload sentinel + `AUDIT_FORBIDDEN_SUBSTRINGS`). `docs/terminal-recording.md` Section 12.4 / 12.5 / 12.10 are the canonical contracts |

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
| New convention discovered | "Folder conventions" or "Task patterns" |
| Same mistake hit twice | "Things to avoid" with paired correct pattern |
| New top-level dependency | "Stack" table; run `/agentic-sync` to refresh component skills |
| New gotcha in a pinned component | "Critical gotchas" or the relevant per-stack skill |
| New ambiguous decision | "Decision tables" |

**Three tiers:** always-relevant → this file; file-type-specific → `.claude/skills/<component>-tasks/SKILL.md` (auto-loads via `paths:` glob); one-off → "Encountered Lessons" below (append-only).

## When unsure

Prefer fewer abstractions, explicit over clever. Ask before adding a top-level dependency or a new terminal-renderer package — renderers are an architectural surface, and the stack is deliberately small. For product-decision ambiguity, ask in chat rather than guess in code.

<!-- agentic-init: curated above this line; do not auto-overwrite content above when running /agentic-sync -->

---

## Encountered Lessons

> Append-only by agents. Owner graduates recurring items to curated sections above. Cap ~20 entries.

**Format:** `YYYY-MM-DD · situation · what was learned · what to do next time`

**Add when:** >15 min debugging with non-obvious cause; documented pattern didn't apply; runtime gotcha not captured anywhere. **Don't add:** routine bugs in your own newly-written code; things already covered above.

---

- 2026-04-28 · API `get_by_id` ownership · The `.filter(|x| x.owner_id == caller)` guard belongs on **every** `get_by_id` handler, not only `list` and `create`. The hosts route initially shipped without it while server-profiles and ssh-identities had it, which would leak cross-user existence by id. · Any time `repository.get(id)` returns a row that is not already scoped to the caller, filter by `owner_id` before mapping `None` → 404. Cross-user reads must be byte-identical to a genuine 404.
- 2026-04-28 · dev-auth stopgap transitions · Stopgap auth shims (e.g. the `DevUser` extractor) must be configured for two-phase coexistence with real auth, not a hard bail when the shim is disabled. A `bail!`-on-disable forces a single coordinated cutover with no dark-launch window. · `if cfg.dev_auth.enabled { Some(id) } else { None }` plus a 401 from the extractor leaves room to land real auth alongside the shim and migrate handlers one at a time.
- 2026-04-29 · Host-key preflight disconnects before auth · The host-key preflight route captures the host key during KEX and disconnects WITHOUT attempting authentication. Authenticating against an untrusted host would transmit our public-key signature to a potential attacker. Identity material is parsed (round-trip validated) but never put on the wire during the probe. · If a future "verify auth works" slice lands, gate it behind a separate route that requires the host key to already be `trusted` — never auto-promote on first sight. Wire-side response wording must name the KEX-only scope explicitly; never imply auth or session readiness was checked.
- 2026-04-29 · Trust-host-key needs captured-vs-expected AND revoked-aware checks · The trust route must (1) re-probe to capture the CURRENT host key, (2) require the caller's `expected_fingerprint` to match the captured fingerprint, (3) refuse if the classifier returns `Changed`, AND (4) refuse if any revoked row exists for the captured `(key_type, fingerprint)`. Skipping (4) lets a revoked-and-reappearing key be silently re-trusted because the classifier filters revoked rows out of `Trusted`/`Changed` and the captured-vs-expected check passes. · Two-layer defense: route guard produces a clean 409 before any write; `record_trusted` SQL also enforces `WHERE revoked_at IS NULL` on the conflict branch and surfaces a `Conflict` repo error. Never touch `revoked_at` from `record_trusted` — recovery is a separate, deliberate operator workflow.
- 2026-04-29 · TerminalSessionManager partial-success on event write · `create_session` writes the `terminal_sessions` row first, then appends the `created` `session_event`. If the event insert fails, the row exists in `starting` with no `created` event — an audit gap. Surfacing the error to the caller (current behavior) is correct: the orphan row is a sweep-able stale placeholder, not a security risk, and the `close` route is the single hand-back surface. · Do NOT add a synchronous `sessions.delete(id)` rollback to the error path of `create_session`. Two writes across an unbounded sequence of repositories is the wrong shape for atomicity — Postgres transactions belong inside one repository call, not across the manager. If a future slice needs strict atomicity, push both writes into a single repository method that owns a `BEGIN/COMMIT`.
- 2026-04-29 · xterm `onResize` fires synchronously inside `Terminal.resize` · Calling `renderer.resize(cols, rows)` on `XtermRenderer` invokes every `onResize` subscriber synchronously before the call returns. A UI control that calls `renderer.resize(...)` AND `client.sendResize(...)` directly will double-emit the wire frame. The xterm-live-terminal lab hit this when both the post-attach init path and the manual "apply resize" button were wired to send the resize themselves. · The `onResize` subscriber is the single place that calls `client.sendResize`. Manual resize controls and post-attach init must call `renderer.resize(...)` only — never `client.sendResize(...)` directly — and let the subscriber drive the wire frame. The same rule applies to any future renderer adapter whose own resize entry point fans out to `onResize` listeners synchronously.
- 2026-04-30 · `WTerm` constructor mutates the host element synchronously · Unlike `xterm.js`, `ghostty-web`, and `restty/xterm` (which all expose a `Terminal()` no-arg constructor + a separate `open(element)` step), `@wterm/dom`'s `WTerm(element, options)` takes the host element on construction and immediately appends a `.term-grid` child div, adds the `.wterm` class to the host, and attaches a click listener — before the async `init()` runs. Constructing the `WTerm` at adapter-construction time would silently mutate the page before `mount(element)` is called and the host element is even known. · Defer BOTH `new WTerm(element, opts)` AND `await wterm.init()` to the adapter's own `mount(element)`. Re-check the disposed flag after the awaited init and call `wterm.destroy()` immediately if a synchronous `dispose()` raced ahead — same shape as the ghostty-web adapter's post-init disposed check. The same rule generalises: any renderer whose constructor takes the host element belongs inside `mount`, not the adapter constructor.
- 2026-05-01 · Lifecycle audit emission is fail-closed across two writes · The server-profile lifecycle routes (create / disable / enable) write the lifecycle row first and then append one `audit_events` row from a single `write_lifecycle_audit` helper. If the audit insert fails, the route returns `500 internal_error` and the lifecycle row state is already committed — the same partial-success shape `create_session` keeps. · Do NOT add a synchronous rollback (`server_profiles.delete` / `set_disabled_at(...prior...)`) to the audit-failure path. Two writes across separate repositories is the wrong shape for atomicity; if strict atomicity ever becomes load-bearing, push both writes into a single repository method that owns the `BEGIN/COMMIT`. The audit-only failure mode is operator-actionable (orphan row + 500); a silent audit gap is not.
- 2026-05-01 · `recent_for_actor` NULL-actor exclusion · The current-user audit feed MUST filter with `WHERE actor_id = $1` (equality), not `IS NOT DISTINCT FROM` or `COALESCE`. Pre-auth events (`actor_id IS NULL`: failed-login attempts, unauthenticated probes) are intentionally invisible to a normal user route — surfacing them would expose every login-throttle / probe pattern to whichever user happened to query. An admin surface that wants those rows uses the unscoped `recent` query directly. · Any future user-scoped feed (sessions-by-actor, audit-by-actor, anything-by-actor) must use `actor_id = $caller`. If a NULL-actor row needs to be visible somewhere, route it through a separate admin-only endpoint, never relax the filter on a per-user feed.
- 2026-05-01 · Cookie parser collapses empty value and missing cookie · The shared `auth::cookie::extract_session_cookie` helper returns `None` for both an absent `Cookie:` header AND a present-but-empty `relayterm_session=` pair. The previous inline parser in `routes/v1/auth.rs` returned `Some("")` on the second case, which then forced the downstream `validate_session_token` path to do the rejection. The new behaviour is intentional — collapsing the two paths means the wire response and operator-side `warn!` are byte-identical for both cases, so a probe cannot tell "you sent no cookie" from "you sent an empty one". · Any future cookie-parser refactor MUST preserve this collapse. A test that constructs `relayterm_session=` and expects a 401 with the `missing session cookie` operator detail (not `session invalid`) pins the contract; do not "fix" it by returning the empty string.
- 2026-05-01 · `auth.mode = dev` is no longer a route-handler signal · After the DevUser runtime path was retired, `auth.mode` no longer changes which extractor a handler uses — every protected route runs through `AuthenticatedUser` regardless of mode. The mode now selects only the *boot-time validation envelope* (`production` requires a session signing key, non-empty `allowed_origins`, and `cookie_secure = true`; `dev` relaxes all three). · Do not branch handler logic on `auth.mode` and do not reintroduce a `dev_user_id`-shaped state field. If a route legitimately needs different behavior in development, plumb a single typed flag (matching `auth.cookie_secure` / `vault.enabled`) — never a "skip the auth check" toggle. The "production must have a real user" requirement is enforced in `apps/backend/src/main.rs` after the DB connect (no first user + no `first_user_bootstrap_token` → fail-fast), not in the per-handler path.
- 2026-05-02 · `last_seen_at` touch is best-effort inline · The `AuthenticatedUser` extractor stamps `user_sessions.last_seen_at` after a successful validate + user-load by awaiting `touch_last_seen` inline. A repository failure logs `warn!` with the session id only (never the cookie value, the token hash, or repository internals) and the request still succeeds. Failed / expired / revoked / unknown extractions never reach the touch — the early returns above guarantee `last_seen_at` is updated only on the happy-path. · Any future "stamp X on every authenticated request" surface follows the same shape: inline await, swallow-and-log on failure, no `tokio::spawn`-and-forget, no `JoinSet`-and-forget. Spawning would let one failing repository call accumulate orphaned futures AND would drop the touch out of the request span observability sees. If a stamp ever NEEDS to be off-request (high-cost work, batched flush), promote it to a managed background task owned by `AppState` with explicit shutdown — never a fire-and-forget `tokio::spawn`.
- 2026-05-03 · Recording retention startup sweep is fail-soft, reconciliation is fail-fast · Stage A retention sweep (`run_recording_retention_startup_sweep` in `relayterm-terminal::retention`) runs at boot AFTER `terminal_sessions::reconcile_orphaned_on_startup` and BEFORE the listener bind, but unlike reconciliation the sweep is fail-soft — a candidate-selection error or first per-session purge error logs the static category tag `"retention_sweep_failed"` and the boot continues. Reconciliation correctness affects the user-facing session list and the live PTY recovery path; retention correctness affects only durable corpus size, so a missed sweep cycle is operationally undesirable but not security-relevant. · Any future retention surface (Stage B periodic worker, an admin manual-purge route) must keep the same posture: never block the listener bind on a retention DB error, never echo repository error text into the wire body, never log session ids in the operator-side aggregate line — only primitive counts (purged_sessions, chunks_purged, markers_purged, bytes_purged). The purge primitive's transactional ROLLBACK preserves chunks/markers on audit failure, so dropped sweep cycles are recoverable; a fail-fast boot would be a bigger blast radius for the same property. The candidate-selection method `list_eligible_for_retention` reads `terminal_sessions.id` ONLY (no `payload`, no `byte_len`); aggregating bytes is the job of `purge_for_retention` inside its own transaction.
- 2026-05-04 · Periodic retention worker is shutdown-aware and advisory-locked · The Stage B periodic worker (`relayterm-terminal::retention_worker::spawn_recording_retention_periodic_worker`) is wired from `apps/backend/src/main.rs` AFTER `AppState` construction and BEFORE the listener accepts connections. It returns a `RetentionPeriodicWorkerHandle` that `main` awaits AFTER `axum::serve` returns; the same `tokio::sync::watch::Receiver<bool>` channel that signals graceful shutdown to `axum::serve` fans the signal out to the worker (the closure passed to `with_graceful_shutdown` calls `shutdown_tx.send(true)` after `shutdown_signal()` resolves). The first periodic tick fires AFTER `cleanup.sweep_interval_seconds` has elapsed — Stage A already drained the eligibility set on boot, so an immediate periodic tick would be a redundant no-op; `tokio::time::interval_at(now + interval, interval)` with `MissedTickBehavior::Delay` is the canonical shape. Each tick is wrapped in `pg_try_advisory_lock(RECORDING_RETENTION_ADVISORY_LOCK_KEY)` via `relayterm-db::PgRetentionAdvisoryLock`; a contended tick is a silent `debug!` skip, NOT an error. · Any future managed background task in this codebase follows the same shape: never `tokio::spawn`-and-forget; return a `JoinHandle` (or wrap it in a typed handle); shut down via a watch / cancellation token wired into the same graceful-shutdown closure that drives the listener; await the handle AFTER the listener returns. Cross-instance correctness for periodic DB-touching work goes through a Postgres advisory lock with a centralised `i64` key constant — never re-derive the key per-call-site, never silently drop the lock release on the success path. Lock acquisition / release errors are surfaced through `RepositoryError::Database` with a SHORT generic string (the `map_lock_error` helper does NOT echo `sqlx::Error` messages — driver text is operator-side only).
