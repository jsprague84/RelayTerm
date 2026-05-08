# Tauri runtime backend URL — design

> **Status: design only.** No implementation has shipped against this
> document. The bundled Tauri shells (desktop + mobile) currently render
> the SPA but cannot reach a backend; the launch smoke
> ([`docs/deployment/tauri-local-build.md`](../deployment/tauri-local-build.md)
> § "Mobile / Android — local device install + launch smoke") proves the
> WebView mounts and surfaces a `Cannot Reach RelayTerm /
> Cannot reach the backend: malformed response` modal — that is the
> deferred-runtime-backend-URL failure path, not a launch failure.
>
> This document fixes the design space *before* code is written. It
> evaluates two materially different shapes (remote web shell vs.
> bundled SPA + cross-origin API) against the existing auth/CSRF
> contract in [`docs/spec/auth.md`](auth.md), and recommends the v1
> path. It is normative for the runtime-backend slice; drift goes
> through this doc first.
>
> **Out of scope for this doc:** any frontend implementation, any Tauri
> plugin selection, any backend CORS / cookie / CSRF change, any native
> secure-storage choice for SSH credentials, signing / release / Play
> Store, multi-server account-synced URL settings, deep-link / managed-
> config provisioning. Those land as follow-up slices once a path is
> chosen here.

## 1. Problem statement

`apps/web` is the same SPA the browser-deployment uses; it is a thin
client over the backend at `/api/v1/*` and `/healthz`. Today:

- **Web dev (`pnpm --filter @relayterm/web dev`)** — SPA loads from
  `http://localhost:5173`. `apps/web/vite.config.ts` proxies `/api`
  and `/healthz` (with `ws: true` on `/api` for the terminal-attach
  upgrade) to `http://127.0.0.1:8080`. All frontend helpers in
  `apps/web/src/lib/api/*.ts` and `terminalLaunch.buildAttachWsUrl`
  use **relative** paths and let the page origin pick the backend.
- **Web prod (Docker / Traefik in `docs/deployment/`)** — same SPA
  served by nginx in front of the backend on the same origin. Same-
  site cookies, same-origin fetches, no CORS, the existing
  `CsrfGuard` / `Origin`-allowlist guard is sufficient.
- **Tauri dev (`tauri dev`, `tauri android dev`)** — `tauri.conf.json`
  loads `devUrl: http://localhost:5173`, so dev-mode reuses the Vite
  proxy with no new wiring.
- **Tauri build (`tauri build`, `tauri android build`)** — Tauri
  serves `frontendDist: ../../web/dist` from a custom protocol on a
  per-platform origin (see § 3 below). **There is no proxy.** Every
  relative `/api/v1/*` fetch lands on the WebView's protocol origin,
  not on a RelayTerm backend, so every API call fails. The launch
  smoke proves this — the `getCurrentUser()` call from
  `apps/web/src/lib/app/auth/AuthGate.svelte` returns malformed-response.

The user-visible problem is "the bundled shell cannot reach a backend."
The design problem is **how a bundled shell is allowed to reach a
remote backend without breaking the auth / CSRF contract.** That is
not a one-line config; it is an architectural choice.

## 2. Architectural risk (read this before anything else)

Two production-side facts make the naive answer ("just let the user
type a URL and call it") unsafe.

### 2.1 The session cookie is `SameSite=Strict`

[`docs/spec/auth.md`](auth.md) → "Cookie / CSRF posture" pins
`relayterm_session` as `HttpOnly; SameSite=Strict; Secure; Path=/`.
The cookie writer is in
[`crates/relayterm-api/src/routes/v1/auth.rs`](../../crates/relayterm-api/src/routes/v1/auth.rs)
(`build_session_cookie` /  `build_clear_session_cookie`). The browser
attaches a `SameSite=Strict` cookie **only on same-site requests** —
where the request initiator's site (registrable domain) matches the
cookie host's site.

A bundled Tauri WebView does NOT share a site with a remote backend:

| Platform | Default WebView origin | `useHttpsScheme = true` variant |
|---|---|---|
| Linux (WebKitGTK) | `tauri://localhost` | n/a — Linux always uses the `tauri://` scheme |
| macOS | `tauri://localhost` | n/a |
| iOS | `tauri://localhost` | n/a |
| Windows | `http://tauri.localhost` | `https://tauri.localhost` |
| Android | `http://tauri.localhost` | `https://tauri.localhost` |

(Source: Tauri v2 docs — [`Webview` config reference](https://v2.tauri.app/reference/config),
[v1→v2 migration "New origin URL on Windows"](https://v2.tauri.app/start/migrate/from-tauri-1).
The `useHttpsScheme` option is documented as Tauri 2.1.0+.)

A backend at `https://relayterm.example.com` is on `example.com`;
`tauri://localhost` and `tauri.localhost` are on `.localhost`. These
are different sites. **A `SameSite=Strict` cookie issued by the
backend will not be attached to a fetch initiated by the bundled
WebView.** `SameSite=Lax` would not help either — Lax allows the
cookie only on top-level cross-site navigations, not on `fetch` /
`XMLHttpRequest` / WebSocket-upgrade calls.

The only cookie-mode that survives is `SameSite=None; Secure`, which
materially weakens CSRF defenses and forces a parallel double-submit
or origin-bound token strategy. We do not want to silently downgrade
the production cookie model to enable a Tauri shell.

### 2.2 The backend has no CORS layer

`crates/relayterm-api` does not configure
`tower_http::cors::CorsLayer`. The browser-write `CsrfGuard` does an
**`Origin`-header allow-list check** (byte-equality match against
`auth.allowed_origins`) — that is layer 2 of the CSRF defense. Layer
1 is `SameSite=Strict` on the cookie itself; layer 3 (double-submit
token) is deferred. See
[`crates/relayterm-api/src/auth/csrf.rs`](../../crates/relayterm-api/src/auth/csrf.rs).

For the bundled shell calling a remote backend cross-origin, two
things would need to land that do not exist today:

1. A CORS layer with `Access-Control-Allow-Credentials: true` and the
   Tauri WebView origin in `Access-Control-Allow-Origin`. Browsers
   require a non-wildcard origin when credentials are included.
2. The Tauri WebView origin in `auth.allowed_origins`. The
   `CsrfGuard` test suite already has an example —
   `tauri://localhost` in
   [`crates/relayterm-api/src/auth/csrf.rs`](../../crates/relayterm-api/src/auth/csrf.rs)
   `allows_when_origin_is_one_of_many_allowed` — but no production
   route or operator config yet enrols a Tauri origin.

These changes are **deferred** by this doc; § 4 explains why we
choose a path that does not require them in v1.

### 2.3 Why "just open it in a remote browser" doesn't help

The same-site rule is enforced by the WebView, not by the backend.
Loading `https://relayterm.example.com` from a regular Chrome window
*is* same-site relative to the cookie; loading the same URL from a
Tauri-managed WebView at a `tauri://localhost` origin is not. The
choice is between (a) making the WebView origin the same as the
backend (load the remote SPA in the WebView; § 4 path A) or (b)
weakening the cookie / CSRF model to allow cross-origin authenticated
requests (§ 4 path B).

## 3. Source-of-truth options for the backend URL

Independent of the architecture choice in § 4, the URL itself has to
come from somewhere. Options considered:

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **Build-time `VITE_*` env baked into the bundle** | Zero runtime decisions; identical to the existing `VITE_*` story in `apps/web`. | Per-environment builds (dev/staging/prod each get their own APK). My homelab domain leaks into a public artifact. No portable build. | Reject for v1. Useful only for self-rolled per-environment artifacts later. |
| **User-entered URL persisted in `localStorage` (Tauri WebView)** | One artifact per platform; no secrets in the build; easy first-launch UX. URL is public config, not a secret. Survives app updates inside the WebView's storage area. | Wiped on app uninstall (acceptable; URL is replayable). Not synced across devices (acceptable for v1 — operator types it once per device). | **Default v1 choice for the URL.** |
| **`@tauri-apps/plugin-store` / native config file** | Survives a WebView storage clear; explicit "this is app config, not page state." | Adds a Tauri plugin + capability + IPC surface; not justified for one URL. Re-evaluate when native commands actually exist. | Defer. |
| **Platform-native secure storage (Keychain / Keystore / Secret Service)** | Strong storage guarantees. | Backend URL is not a secret. Using a secret store for non-secret data lulls future contributors into stashing actual secrets there without a separate review. | Reject for the URL specifically. Reserve secure storage for an *eventual* native-token / SSH-credential decision. |
| **Deep link / MDM / managed config** | Operator-friendly fleet provisioning. | No deployment surface needs this in v1. | Defer. |

**Recommendation.** v1 stores the configured backend URL in
WebView `localStorage` under a versioned key (`relayterm.backend-config.v1`).
The URL is treated as **public configuration**, not a secret. No
secret-shaped material (session token, password hash, encrypted
private key) ever moves to the frontend by way of this slice — the
existing redaction posture in
[`docs/agent/redaction-rules.md`](../agent/redaction-rules.md) §§ 1, 4,
5, 7, 8 is unchanged.

If a later phase needs the URL to survive a `localStorage` wipe (e.g.
a "clear site data" Android setting, or a WebView migration), it can
move to `@tauri-apps/plugin-store` without changing the rest of this
design.

## 4. The architectural pivot: bundled SPA vs. remote web shell

This is the load-bearing decision. Both shapes deliver "the bundled
Tauri app reaches a RelayTerm backend." They have very different
auth blast radii.

### Path A — Remote web shell mode (`webviewUrl` = configured backend)

The Tauri shell's WebView loads the *remote* SPA URL directly, the
same way a browser would. The bundled `frontendDist` becomes a
small **bootstrap** SPA whose only job is the first-launch URL
picker; once the URL is configured and validated, the shell
navigates to it (or the operator restarts the shell against it via a
`webviewUrl` reload).

| Property | Outcome |
|---|---|
| WebView origin | The configured backend (`https://relay.example.com`). |
| Cookie attachment | Same-site against the backend. `SameSite=Strict` works. |
| Backend `Origin` allowlist | Already covers the production browser. No new entry. |
| CORS | Not needed. |
| Frontend code change | Tiny: a bootstrap shell that gates on the URL picker; the existing SPA reaches the backend on relative paths exactly as it does in the browser. |
| Backend behavior change | None. |
| Offline / disconnected use | The remote SPA is unreachable when the backend is. The bootstrap shell renders an offline screen + URL change. |
| App-store appraisal | Remote-content apps are subject to extra review (especially Apple). Acceptable risk for self-hosted Android first; iOS is deferred regardless. |
| User experience differ from browser | Effectively none beyond app chrome. |

**Architectural cost for path A: low.** The auth contract is
unchanged. The CSRF guard is unchanged. The cookie is unchanged. No
backend change. No native secure storage. The SPA does not learn
about Tauri at all (apart from the bootstrap shell).

### Path B — Bundled SPA + cross-origin API

The bundled SPA stays the served frontend; every fetch goes to the
configured backend cross-origin. To make auth survive:

1. **Backend cookie**: switch `SameSite=Strict` to `SameSite=None;
   Secure` (or maintain a parallel cookie). This is a real
   degradation of CSRF defenses today and demands layer 3 (double-
   submit / origin-bound CSRF token) to compensate. That layer 3 is
   already noted as deferred in [`docs/spec/auth.md`](auth.md) — this
   path forces it forward.
2. **Backend CORS**: add a CORS layer with credentials, allow-list
   methods, allow-list headers, **non-wildcard** allow-origin. Pair
   with the `CsrfGuard` allow-list so they cannot drift.
3. **Frontend**: every helper in `apps/web/src/lib/api/*.ts` learns
   an absolute base URL. WebSocket URL builder
   (`terminalLaunch.buildAttachWsUrl`) takes a base origin instead of
   `window.location.host`. The auth `credentials: "include"` pattern
   still applies but now requires CORS preflights on every non-GET.
4. **Tauri WebView origin** must be enrolled in
   `auth.allowed_origins` and in the CORS allow-list. Operators
   self-host the backend, so they update both at once; misconfig is
   one of the most likely v1 support-volume sources.
5. **WebSocket attach** runs the same cross-origin gate. The
   `Origin` header on the WS upgrade is enforced by axum's WS
   handler today via the same `CsrfGuard` for browser-write paths;
   the terminal `GET /api/v1/terminal-sessions/:id/ws` is exempt
   from `CsrfGuard` (it relies on `AuthenticatedUser` only — see
   [`docs/spec/auth.md`](auth.md) → "CSRF posture" + the WS handler
   in
   [`crates/relayterm-api/src/routes/v1/terminal_sessions.rs`](../../crates/relayterm-api/src/routes/v1/terminal_sessions.rs)).
   A cross-origin WS attach does not have a CORS preflight; the
   policy lives entirely on the cookie + `Origin` allow-list, which
   means SameSite=None is the gating change here too.

**Architectural cost for path B: high.** It demands a coordinated
auth / CSRF / CORS slice across `crates/relayterm-api`,
`apps/backend/src/config.rs`, and `apps/web/src/lib/`. It also
trades probe-resistance properties that today are pinned by the
sentinel-string redaction tests in `crates/relayterm-api/tests/api.rs`.

### Recommendation

**v1 is path A (remote web shell mode).** Reasons:

1. The auth / CSRF contract is the most security-sensitive surface
   in the codebase (see redaction rules §§ 1, 4, 5, 7, 8, 9). v1
   should not weaken it for a packaging convenience.
2. The Tauri shells are **deferred** for production according to
   `tauri-ci-release-plan.md`; the right v1 outcome is "the shell
   is a thin wrapper that loads the SPA from the backend the user
   already trusts," not "the shell forces a redesign of the auth
   contract."
3. Path A is a strict subset of the browser deployment in security
   posture. If the browser deployment is safe, the path-A Tauri
   shell is safe.
4. Path A is the smaller code change and the smaller doc change.
5. We can *defer* path B without committing to it. If a future
   offline-first / store-distribution requirement demands a bundled
   SPA, we land path B as an explicit, audited slice with the
   double-submit CSRF token and an opt-in cookie-mode change.

**Path B remains an option** but is intentionally not v1 work. Any
future move toward path B MUST start with a SPEC update to
[`docs/spec/auth.md`](auth.md), an audit of every `CsrfGuard`-
protected route, and a sentinel-string redaction sweep on the new
CORS error surface. It is not a frontend-only change.

## 5. UX for path A (the recommended path)

The bundled SPA shipped in `frontendDist` becomes a **bootstrap
view** whose only responsibility is collecting the backend URL,
validating it, and handing off. Bootstrap UX:

1. On first launch (no `relayterm.backend-config.v1` in
   `localStorage`), render a "Connect to RelayTerm Server" screen.
   Single input + "Connect" button. Honest copy: "Enter the URL of
   your RelayTerm server."
2. Validate the URL syntactically (§ 9 below).
3. Probe `GET <url>/healthz` (no credentials, no body parsing
   beyond `response.ok`). Path A means the WebView origin will move
   to `<url>` after handoff, so this probe is same-origin from the
   bootstrap shell's perspective only after handoff — for the *first*
   probe it is cross-origin and CORS-less. Use it as a tri-state
   (`reachable` / `unreachable` / `not RelayTerm`) on a best-effort
   basis: a 2xx is treated as reachable; a CORS-blocked transport
   error is reported as "could not verify; you can save anyway." The
   probe is a hint, not a gate.
4. On user confirmation, persist the URL and **navigate the WebView
   to the configured URL**. The native shell either reloads the
   webview window with the new `webviewUrl` (Tauri 2's
   `WebviewWindowBuilder::url(...)` / `WebviewWindow::navigate(...)`)
   or restarts itself; the exact mechanism is a follow-up slice's
   job (§ 11 phase C).
5. A "Change server" item appears in the bootstrap shell's chrome
   so an operator can return to the picker without uninstalling the
   app.

The browser deployment never sees the bootstrap view: the bootstrap
SPA is a separate entry served only by the Tauri shell (or, if a
single SPA serves both, a Tauri-mode runtime gate routes between
"render the picker" and "the operator already chose, just defer to
the existing AppGate"). § 7 covers detection.

For path B (rejected for v1) the UX changes: the bundled SPA stays in
control after URL save, and login proceeds against the configured
backend. Same picker; different downstream. Recorded for completeness.

## 6. API client behavior

Today every helper in `apps/web/src/lib/api/*.ts` uses a relative
endpoint default with a test override. WebSocket URL is built from
`window.location.protocol` and `window.location.host`
(`apps/web/src/lib/app/terminal/terminalLaunch.ts::buildAttachWsUrl`).

### Path A behavior

After the bootstrap shell hands off to the configured backend, the
WebView's `window.location` IS the backend origin. **No frontend API
helper changes.** Relative paths and the existing WS builder both
resolve to the backend automatically. This is the design's win.

The bootstrap shell itself does NOT call any `/api/v1/*` endpoint; it
calls `GET <url>/healthz` once with `credentials: "omit"` (the
healthz route is unauthenticated and there is no cookie to attach
yet). Result: no helper in `apps/web/src/lib/api/auth.ts` etc.
changes. The only new code is the picker view + a
`backendConfigStore.ts` (rune-based store) for the URL value.

### Path B behavior (rejected for v1; recorded for completeness)

A single `apiBaseUrl()` helper (default empty string for the browser
deployment, configured origin for Tauri), threaded through every
helper's `endpoint` argument. WebSocket URL builder takes the same
base. WS protocol derivation: `https:` → `wss:`, `http:` → `ws:`,
and explicitly **disallow** `ws:` against any non-localhost host even
in dev mode (a configuration-based degradation must be a deliberate
opt-in, never the default).

## 7. Tauri detection

The frontend needs to know whether it is in a Tauri WebView so the
bootstrap picker only renders there. Options:

- **`window.__TAURI_INTERNALS__` / `window.__TAURI__`** — present in
  Tauri v2 WebViews; a stable enough indicator. The exact name is a
  Tauri-internal, so isolate access in one helper (e.g.
  `apps/web/src/lib/runtime/runtime.ts`'s `isTauri()`) so a future
  rename is one diff.
- **Build-time flag** — fragile across the dual desktop/mobile
  shells, since both consume the same `apps/web` build.
- **User-Agent sniff** — Tauri sets a `Tauri-Version` header on its
  IPC and a recognizable UA suffix on some platforms; brittle and
  not officially recommended for runtime branching.

**Recommendation:** one helper, runtime-only, single source of truth.
Tests stub it. Production code that reads it is bounded to the
bootstrap shell + a `ConfiguredBackendUrlGate.svelte` that wraps
`AuthGate`. No other production component sees Tauri-specific logic.

## 8. Persistence and security posture

The redaction rules in [`docs/agent/redaction-rules.md`](../agent/redaction-rules.md)
remain authoritative. This slice MUST NOT widen any of them. Specifically:

- **Backend URL** is public config. `localStorage` is acceptable.
- **`session_token`, `token_hash`, password (clear or hashed),
  `encrypted_private_key`, plaintext PEM, `bootstrap_token`** — none
  of these move to the frontend by way of this slice. The auth
  cookie remains `HttpOnly`; nothing in the bootstrap shell reads it.
- **No new `audit_events` payload field, no new logging path, no new
  `data-*` attribute** carries the URL alongside any auth detail.
  The URL alone is harmless; coupling it to a session id in a log
  line is a regression.
- **No native secure storage is required for the URL.** A future
  decision to move SSH credentials, refresh tokens, or biometric
  unlock state into Keychain / Keystore / Secret Service is its own
  slice with its own SPEC update.

`localStorage` write key:

```
relayterm.backend-config.v1 -> { "url": "https://relay.example.com" }
```

The version suffix (`.v1`) lets a future shape change land without
mis-reading old data; the migration is "drop the old key on shape
mismatch and re-prompt," not "auto-migrate."

## 9. Cookies / CORS / Origin / CSRF — explicit findings

Spelled out so a future "let's just add CORS quickly" PR has a
concrete checklist.

### What works under path A

- **Cookies.** `SameSite=Strict` + `HttpOnly` + `Secure`. Same-site
  against the backend after handoff. No change.
- **Origin guard.** Existing `CsrfGuard` allow-list already includes
  the production backend origin. No change.
- **CORS layer.** Not needed.
- **WebSocket upgrade.** Same-origin against the backend. The
  existing `AuthenticatedUser` extractor + `Origin`-allowlist guard
  applies as in the browser deployment.
- **`relayterm_session` cookie** never traverses a Tauri-controlled
  origin. If the operator configures the backend URL incorrectly to
  a different host, the worst case is "no cookie attaches, login
  fails," not "cookie leaks to a different origin."

### What does NOT work under path A

- **Probe call before handoff.** The bootstrap shell's `GET
  <url>/healthz` is cross-origin (Tauri origin → backend) and is
  blocked by CORS unless the backend allows it. The code is a one-
  shot probe in the bootstrap UX; a CORS failure surfaces as
  "couldn't verify, save anyway." This is acceptable; the probe is
  advisory.
- **Operator using a backend that requires a non-default port or
  path.** The picker MUST validate that the URL is a bare origin
  (no path), or — if a path prefix is allowed — that it resolves
  consistently in both the picker probe and the post-handoff `<base
  href>` interpretation. § 10 lists the validation rules.

### What changes under path B (NOT chosen)

Each of these is a real backend change and does NOT happen in v1:

1. `auth.cookie.same_site` becomes operator-configurable and
   defaults to `None` for Tauri-deployed builds.
2. CORS layer added with `Access-Control-Allow-Credentials: true`,
   `Access-Control-Allow-Origin` populated from
   `auth.allowed_origins`, `Access-Control-Allow-Methods`,
   `Access-Control-Allow-Headers` allow-listed. Disallow `*`.
3. Layer 3 CSRF defense (double-submit token) lands. SPEC update to
   [`docs/spec/auth.md`](auth.md) is a prerequisite, not an
   afterthought.
4. Sentinel-string redaction tests extend to cover CORS error
   responses and any new `Vary` / `Access-Control-Expose-Headers`
   surface.

If the project ever needs path B, none of these are skippable.

## 10. URL validation rules

Path A and path B share these. Implemented in a pure helper with a
unit-test-only surface (no DOM) so a vitest can pin every rejection
path before any UI exists.

Accept:

- `https://<host>` for any host. No exceptions; HTTPS is always
  accepted regardless of host.
- `http://<host>` ONLY when `<host>` is one of `localhost`,
  `127.0.0.1`, `::1`, `10.0.2.2` (the Android emulator loopback to
  the host machine), or `0.0.0.0` (for completeness). Any other
  cleartext URL is rejected — see `url_http_non_localhost` below.
- Bare origin only: `scheme://host[:port]`. No path other than `/`.
  No query string. No fragment.
- Host normalised to lower-case (RFC 3986). Trailing slash on path
  stripped before persisting.

Reject:

- `javascript:`, `file:`, `data:`, `blob:`, `about:`, any non-http(s)
  scheme.
- URLs with an embedded userinfo (`https://alice:hunter2@host/`) — a
  password in the URL is a credential we MUST NOT take. Reject
  before any logging.
- URLs that fail `URL` parser construction.
- URLs whose host contains spaces, `\`, `..`, or other shapes that
  would round-trip differently in `<a href>` vs `fetch`.
- URLs with a non-`/` path. (A future deployment that needs a path
  prefix can land that as an explicit, validated knob — for v1, bare
  origin only.)

Emit no log line that includes the rejected URL value; collapse to a
typed reason enum for the UI (`url_unparseable`, `url_userinfo`,
`url_disallowed_scheme`, `url_path_present`, `url_http_non_localhost`,
`url_too_long`). The UI maps the enum to a static string. This
mirrors the existing `LoadError` / `AuthError` redaction posture in
`apps/web/src/lib/api/`.

`URL_MAX_LEN` is `2048` (a defensible upper bound for an origin; the
common allow-list URL is well under 100 chars).

## 11. Implementation phases

Each phase is its own PR.

### Phase A — design (this slice)

This document. No code. No CI change.

### Phase B — frontend URL primitive + validation

- `apps/web/src/lib/runtime/backendConfig.ts` — pure helpers:
  `parseBackendUrl(raw): ParsedBackendUrl | { ok: false, reason }`,
  `loadBackendConfig(): BackendConfig | null` (reads
  `localStorage`), `saveBackendConfig(cfg)`, `clearBackendConfig()`.
- `apps/web/src/lib/runtime/runtime.ts` — single `isTauri()` helper.
- Unit tests (`apps/web/tests/backendConfig.test.ts`) covering every
  rejection path + every accept case, including sentinel inputs that
  embed `private_key` / `session_token` shapes — those MUST surface
  through the rejection envelope without hitting any log path.
- No UI yet; no `localStorage` write anywhere production.

### Phase C — Tauri-only bootstrap picker + handoff

- `apps/web/src/lib/runtime/BootstrapShell.svelte` — the picker.
  Used only when `isTauri() && !loadBackendConfig()`.
- `apps/web/src/lib/runtime/ConfiguredBackendGate.svelte` — wraps
  `AuthGate`; on Tauri-mode it ensures `loadBackendConfig()` is set,
  otherwise renders the picker. On the browser deployment it is a
  no-op pass-through.
- A "Change server" surface that calls `clearBackendConfig()` and
  reloads the WebView. The Tauri shell may need a tiny native side
  to navigate the WebView to the configured URL on save (Tauri 2's
  `WebviewWindow::navigate(...)` invoked from a frontend helper via
  a single, narrow capability — out of scope for this design's
  recommendation; an alternative is "user restarts the app once
  after save," which is acceptable for a debug build).
- Path A handoff: on save, set `localStorage` and trigger
  `window.location.assign(<configured-url>)`. The WebView is allowed
  to navigate to a configured remote origin; this requires a Tauri
  capability row. **`core:default` does NOT include cross-origin
  WebView navigation** — phase C MUST add a scoped
  `webview:allow-navigate` (or the equivalent platform-specific
  capability per Tauri v2 docs at slice-execution time) bounded to
  the configured origin. The "operator restarts the app once after
  save" fallback is acceptable for a debug build but should not
  ship as the production UX.
- No backend change.

### Phase D — backend-side acceptance work (path A specific)

- `auth.allowed_origins` documentation update: when path A is in
  use, the configured `https://relay.example.com` is already in the
  allow-list (it is the production browser origin). No new entry.
  No code change.
- (Optional) `/healthz` response gains a tiny `RelayTerm` discriminator
  in a header so the picker probe can distinguish "this is a
  RelayTerm backend" from "this is some random 200 OK." This is a
  single-line backend add, low risk; defer until the picker UX
  demands it.
- Tauri origin allow-listing for path B is **not** done in v1.

### Phase E — manual smoke against a reachable backend

- Desktop bundled build: configure a LAN / VPN backend URL; sign
  in; launch a terminal session. No automated test.
- Android bundled build: same, with explicit notes per § 12 below
  on `10.0.2.2` (emulator) vs LAN IP (physical device) vs VPN.
- The smoke is host-side, manual. No CI smoke.

### Phase F — later (deferred and not designed here)

- Native `@tauri-apps/plugin-store` migration if `localStorage` is
  not durable enough.
- Native secure storage for any future client-side token (none in
  v1).
- Multiple concurrent backend profiles (out of scope; v1 is one
  configured backend per device).
- Account-synced URL config (out of scope).
- Path B (cross-origin bundled SPA) — only if a concrete
  requirement makes path A infeasible, and only with a SPEC update
  in lockstep.

## 12. Desktop vs mobile differences

| Concern | Desktop (Linux / Win / macOS) | Android | iOS |
|---|---|---|---|
| `localStorage` durability | Survives app updates; cleared by OS "clear app data." Acceptable. | Survives app updates; cleared by app uninstall and by user "clear storage" in app info. Acceptable. Persists across reboots. | Same as Android in spirit. iOS Tauri shell is itself deferred; no design here. |
| Network reachability | Whatever the OS sees (LAN, VPN, tunneled). | **`localhost` on the device is the device, not the dev machine.** Connecting to a developer's laptop requires a LAN IP, a VPN (e.g. WireGuard mesh), `10.0.2.2` on the emulator only, or `adb reverse tcp:8080 tcp:8080` for USB-attached devices. Document explicitly in the picker hint. | Deferred. |
| HTTPS-only OS rules | None. | API 28+ defaults to "cleartext traffic blocked"; an explicit `usesCleartextTraffic="true"` in `AndroidManifest.xml` is required to permit `http://` to a non-default host. The Tauri Android scaffold does NOT set this today. v1 picker policy: **accept `http://` only for `localhost`, `127.0.0.1`, `10.0.2.2`** (which the OS already permits without manifest changes); reject any other `http://` and prompt the operator to use HTTPS. | Deferred. |
| WebView origin scheme | Linux/macOS/iOS: `tauri://localhost`. Windows: `http://tauri.localhost` (or `https://tauri.localhost` with `useHttpsScheme=true`). | `http://tauri.localhost` (or `https://...` with `useHttpsScheme=true`). | `tauri://localhost`. |
| `useHttpsScheme` recommendation | Leave default (`false` on Win/Android) for v1. **Changing this between releases moves the WebView's `localStorage` location** and the saved backend URL is lost. Any later change MUST be a deliberate, version-aware migration. | Same. | n/a. |

## 13. Dev-mode behavior — must remain unchanged

`tauri:dev` and `tauri:android:dev` keep using `devUrl:
http://localhost:5173` and the existing Vite proxy. The bootstrap
shell renders ONLY when:

```
isTauri() && !import.meta.env.DEV && !loadBackendConfig()
```

Two discriminators, each with a distinct job:

- **`isTauri()`** keeps the picker out of the browser deployment
  entirely. In a browser session `isTauri() === false`, so the
  expression short-circuits and `AuthGate` renders unchanged. The
  browser SPA never sees the picker, no `localStorage` write
  happens, and no behavior changes. This is the load-bearing
  guarantee that path A does not touch the production browser.
- **`!import.meta.env.DEV`** keeps the picker out of `tauri:dev` /
  `tauri:android:dev`. In dev, the WebView is pointed at
  `http://localhost:5173` and the Vite proxy already forwards
  `/api` and `/healthz` to `127.0.0.1:8080`. If the picker fired
  in dev, navigating to a configured backend would leave the Vite
  dev server (losing HMR, the dev proxy, and the renderer-lab
  surface). Excluding dev keeps the existing dev workflow unchanged.

A dev-mode override env var (e.g. `VITE_RELAYTERM_FORCE_BOOTSTRAP=1`)
is acceptable for *manually* exercising the picker in dev without
clearing `localStorage`. It is NOT a runtime backend URL — the URL
still comes from the picker. The override is opt-in and never the
default; under `tauri:dev` the picker stays off unless the env var
is set.

## 14. Testing plan

Unit tests (no DOM, no Tauri):

- Every URL validation accept / reject case from § 9.
- Sentinel-string smuggling: a URL whose host or query contains
  `private_key`, `session_token`, `bootstrap_token`, or
  `encrypted_private_key` substrings MUST surface only through the
  rejection enum and MUST NOT appear in any thrown `Error.message`.
- `localStorage` round-trip: save → load → equal. Shape-mismatch
  on read returns `null` (does NOT throw, does NOT log).
- Version suffix: a `relayterm.backend-config.v0` legacy key (if any
  ever exists) is dropped, not migrated.

Frontend tests (Svelte + jsdom):

- Picker initial render. Submit-disabled until parsable.
- Probe success / unreachable / CORS-blocked: the resulting message
  is a function of the typed reason only, never echoes the URL or
  any wire body.
- "Change server" path: clear → re-render picker.
- Browser deployment (no Tauri): picker NEVER renders; existing
  `AuthGate` flow is unchanged.

Integration / smoke (manual, host-side):

- Desktop bundled build → configure → sign in → launch terminal.
- Android emulator (`10.0.2.2`).
- Android physical device on LAN IP.
- Android physical device through VPN.

No CI changes. No backend tests change.

## 15. Open questions (for the owner)

These are NOT decided by this doc. Each requires an explicit answer
before path A's phase B can land.

1. **Bootstrap shell SPA: same artifact or separate?** Is the
   bootstrap shell a new Vite entry / build target, or a runtime gate
   in the existing `apps/web` SPA? Recommendation: runtime gate
   (one artifact, one build, one set of `pnpm -r` checks). Cost:
   the bootstrap code ships in the browser bundle too, behind
   `isTauri()`. Estimated extra bundle: < 5 KB gzipped.
2. **WebView navigate-to-remote on save: native or full reload?**
   Tauri 2's `WebviewWindow::navigate` from the frontend requires a
   capability row. The "user closes and re-opens the app" fallback
   is acceptable for v1 debug builds; a single-click "Connect" UX
   wants the navigate path. Defer to phase C.
3. **/healthz discriminator?** Add a `relayterm-build` response
   header so the picker can refuse to save a URL whose `/healthz`
   200s but is not a RelayTerm backend? Adds one line of backend
   code; defer until needed.
4. **Per-device or account-synced?** v1 is per-device. An eventual
   account-synced setting is a separate slice.
5. **Multi-server later.** A future "I have a homelab and a work
   relay" model. Not v1. Saving the design in `localStorage` under
   a versioned key keeps the door open without committing to it.
6. **Cookie mode if path B ever happens.** `SameSite=None` plus
   double-submit token, vs. token-bearing `Authorization: Bearer
   <token>` for Tauri. The latter changes the auth contract more
   than the former. Recorded for completeness; no v1 work.
7. **Mobile native session-bearing.** If a later phase moves to
   token-bearing auth on mobile (refresh tokens in Keystore /
   Keychain), it does so as a separate, audited slice — not as a
   side-effect of the URL config slice.

## 16. What this slice intentionally does NOT do

- Implement any of phases B–F. This is design.
- Add any Tauri plugin (`@tauri-apps/plugin-store`,
  `@tauri-apps/plugin-keychain`, etc.).
- Change any `tauri.conf.json` field.
- Change any backend route, cookie, CSRF, CORS, or session behavior.
- Change `auth.allowed_origins` or any `Origin`-allow-list test.
- Add or change any audit-event kind / payload field.
- Change any `apps/web/src/lib/api/*.ts` helper.
- Add any CI workflow or signing config.
- Add any secret to the repo.
- Move any secret to the frontend.
- Decide native secure-storage choice for SSH credentials or any
  client-side token — that is a separate, deliberate slice.

## 17. Cross-references

- [`docs/spec/auth.md`](auth.md) — authoritative cookie / CSRF /
  session contract. Read before proposing any path-B work.
- [`docs/agent/redaction-rules.md`](../agent/redaction-rules.md) §§ 1, 4,
  5, 7, 8 — the redaction posture this slice does NOT widen.
- [`docs/deployment/tauri-local-build.md`](../deployment/tauri-local-build.md)
  — local build prerequisites + the launch-smoke modal that
  motivated this work. § "Mobile / Android — runtime caveats" links
  here.
- [`docs/deployment/tauri-ci-release-plan.md`](../deployment/tauri-ci-release-plan.md)
  § 9 question 5 (Backend URL configuration) and question 7 (Mobile
  session storage) — open questions this doc closes for path A and
  defers explicitly for path B.
- [`crates/relayterm-api/src/auth/csrf.rs`](../../crates/relayterm-api/src/auth/csrf.rs)
  — the `Origin`-allowlist guard whose policy this slice does not
  change.
- [`crates/relayterm-api/src/routes/v1/auth.rs`](../../crates/relayterm-api/src/routes/v1/auth.rs)
  — `build_session_cookie` is the cookie writer this slice does not
  modify.
