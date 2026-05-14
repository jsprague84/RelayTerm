# ghostty-web WebAssembly CSP — decision doc

> Design / threat-model doc for how (and whether) RelayTerm should
> permit WebAssembly execution under the production CSP so the
> ghostty-web experimental renderer can mount. **Docs-only.** This
> entry recommends a path; it does NOT authorise any source / CI /
> deploy / CSP change. Every implementation slice below is a separate,
> deliberate slice that must be approved on its own merits.
>
> Status: **proposed** (2026-05-13). **Option D landed on staging
> 2026-05-14**, host-side only; production deploy templates remain
> strict and a production-side CSP decision is still deferred. See
> [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
> § 2026-05-14c — Staging-only CSP wasm-unsafe-eval +
> ghostty-web production-shell mount. Author: see git log on this
> file.

## 1 · Status / decision summary

**Decision recommended:** **Option D — gated, staging-only.** Land
`'wasm-unsafe-eval'` (and **only** that) in the staging deploy
surface's CSP `script-src` first, run a fresh staging resmoke, then
decide whether to extend the same relaxation to production deploy
examples as its own later slice.

**Headline constraints that fall out of the threat model below.**

- Do **NOT** add the broad `'unsafe-eval'` source expression
  anywhere. It permits the `eval` builtin (string evaluation), the
  `Function` constructor with a string argument, the `setTimeout` /
  `setInterval` string forms, **and** the WASM compile path. The
  narrower `'wasm-unsafe-eval'` permits only the WASM compile path.
  The CSP WG-tracked WebAssembly proposal frames
  `'wasm-unsafe-eval'` as the dedicated narrow control for
  permitting `WebAssembly.compile` / `WebAssembly.instantiate` /
  `compileStreaming` / `instantiateStreaming` while keeping JS
  string-evaluation paths blocked.
- Do **NOT** re-add `data:` to any directive. The
  `aa6bf9f fix(web): load ghostty wasm as an asset` slice closed
  the `data:application/wasm` rejection by emitting the WASM as a
  same-origin Vite asset. Reintroducing `data:` would silently
  widen the page's fetch surface for no benefit (the runtime no
  longer touches a data URL).
- Do **NOT** change `connect-src`. The staging resmoke
  (`docs/deployment/vps-staging-smoke.md` § "2026-05-14b ·
  Ghostty-web WASM-as-asset resmoke") confirmed the same-origin
  asset fetch already works under the current `default-src 'self'`
  posture; there is no evidence `connect-src` is the blocker.
- **xterm stays the production default.** ghostty-web (and restty,
  wterm) stay experimental and operator-gated. The renderer
  evaluation track's Gate 1 / Gate 2 promotion criteria (in
  `docs/terminal-renderer-evaluation.md`) are NOT moved by this
  decision; this doc only addresses the CSP precondition that
  blocks the Gate 1 ghostty-web evidence.

**What this doc does NOT decide.** The exact mechanism for landing
the CSP change in this repo (Traefik host-side middleware vs. an
explicit CSP header in `deploy/nginx/web.conf.template` vs. an
opt-in env knob) is left to the implementation slice. The
threat-model analysis below applies regardless of mechanism.

## 2 · Findings from the 2026-05-14b asset resmoke

The full smoke entry is `docs/deployment/vps-staging-smoke.md` §
"2026-05-14b · Ghostty-web WASM-as-asset resmoke (data: CSP block
closed; wasm-unsafe-eval still blocks compile; xterm recovery still
works)". Recapped here so this doc reads cold:

1. The new `dist/assets/ghostty-vt-<hash>.wasm` is emitted by the
   web bundle, served same-origin, returns
   `content-type: application/wasm` and the standard
   `cache-control: public, immutable, max-age=31536000`.
2. The runtime fetches the asset cleanly:
   `performance.getEntriesByType('resource')` showed exactly one
   ghostty entry — the same-origin asset URL — with
   `initiatorType="fetch"`, `responseStatus=200`,
   `decodedBodySize=423045`, `duration ≈ 82 ms`.
3. The two `data:application/wasm` CSP-violation console errors the
   prior smoke captured **did not fire**. The
   `connect-src`-falling-through-to-`default-src` rejection of the
   inlined data URL is gone.
4. `WebAssembly.compile` inside upstream's `Ghostty.loadFromPath`
   STILL rejects with `'unsafe-eval' is not an allowed source of
   script`. A direct
   `await WebAssembly.compile(<8-byte minimal WASM>)` issued from
   `browser_evaluate` rejects identically — so the remaining gap
   is the `WebAssembly.compile` call itself, not anything specific
   to the ghostty-vt bytes.
5. The wedge has a clean operator-visible diagnostic now:
   `data-renderer-fallback="adapter_mount_failed"` plus the fixed
   copy `Renderer failed to mount. Switch back to xterm in
   Settings and reopen the terminal.` xterm recovery on the same
   profile still works end-to-end.

**Net.** The adapter-side half of the historical CSP gap is closed.
The remaining blocker is exactly one source expression in CSP
`script-src`: `'wasm-unsafe-eval'`.

## 3 · Current CSP posture

### 3.1 · What the browser sees today

`curl -sSI https://relayterm-staging.js-node.cc/` returns one CSP
header on the production-shell HTML response:

```
content-security-policy: default-src 'self'
```

There is **no** `script-src`, `connect-src`, `style-src`,
`img-src`, `font-src`, `frame-ancestors`, or any other directive.
Every fetch / script / style / WebSocket / connect falls back to
`default-src 'self'`. Same-origin scripts run; same-origin assets
load; cross-origin and `data:` / `blob:` / inline are blocked.

### 3.2 · Where the CSP comes from

**Not from this repo.** `deploy/nginx/web.conf.template` adds no
CSP header — the only `add_header` line in that file is the
`/assets/*` `Cache-Control: public, immutable`. The CSP arrives
from the host-side Traefik **`secure-chain@file`** middleware that
the staging deployment chains onto the public router (see
`deploy/docker-compose.traefik-staging.example.yml:188`). The
`secure-chain@file` definition lives on the deploy host, outside
this repo, and applies HSTS / CSP / sane defaults to every router
that opts in.

**Implication for any implementation slice.** A CSP change for
the current staging surface is a **host-side** Traefik middleware
change, NOT a RelayTerm-repo change. RelayTerm's own deploy
templates would only need to grow an explicit CSP if the project
decides to stop relying on the host-side middleware OR ship a
per-deployment override. The implementation slice below proposes
both as candidate mechanisms.

### 3.3 · Which directive actually blocks ghostty-web now

Per the resmoke (§ 2 finding 4), the remaining browser refusal
fires with the diagnostic:

> `WebAssembly.compile(): Compiling or instantiating WebAssembly
> module violates the following Content Security policy directive
> because 'unsafe-eval' is not an allowed source of script in the
> following …`

The browser checks the `script-src` directive (falling through to
`default-src 'self'`) when `WebAssembly.compile` /
`compileStreaming` / `instantiate` / `instantiateStreaming` is
called. `'self'` permits same-origin scripts but does NOT permit
WASM compile. The historical fix is the explicit
`'wasm-unsafe-eval'` source expression — a CSP3-era addition
specifically intended to allow WASM execution without re-opening
JS string-evaluation paths.

### 3.4 · What is already same-origin and working

- The bundle JS (`/assets/index-<hash>.js`) under `'self'`.
- The bundle CSS (`/assets/index-<hash>.css`) under `'self'`.
- The same-origin WASM asset fetch
  (`/assets/ghostty-vt-<hash>.wasm`) returning `200` with
  `content-type: application/wasm`, under `'self'` (`connect-src`
  / `default-src` permits it).
- The terminal WebSocket upgrade
  (`/api/v1/terminal-sessions/:id/ws`) under `'self'`.

`connect-src`, `style-src`, `img-src`, and `font-src` are NOT
implicated by this decision. The doc deliberately scopes its
recommendation to one directive (`script-src`) and one source
expression (`'wasm-unsafe-eval'`).

## 4 · Options

| Option | Summary | Production-side blast radius | Effort |
|---|---|---|---|
| A | Add `'wasm-unsafe-eval'` to `script-src` everywhere (staging + production examples). | Wide — every browser-served page gets WASM compile enabled. | Low (one-line Traefik / nginx change). |
| B | Avoid `WebAssembly.compile` entirely via upstream change. | None for RelayTerm CSP. | High and not RelayTerm-controlled. |
| C | Keep ghostty-web CSP-blocked; evaluate restty / wterm first. | None for RelayTerm CSP. | None (defer). |
| D *(recommended)* | Add `'wasm-unsafe-eval'` to `script-src` in a **staging / evaluation** profile only; production examples stay strict. | Narrow (staging only). | Low — staging-only middleware tweak. |

### 4.A · Add `'wasm-unsafe-eval'` to `script-src` everywhere

Narrowest CSP source expression that unblocks ghostty-web. Cleanly
distinguishable from `'unsafe-eval'`: the narrower form permits
`WebAssembly.compile` / `WebAssembly.instantiate` /
`compileStreaming` / `instantiateStreaming` only; the `eval`
builtin, the `Function` constructor with a string argument, and
the `setTimeout` / `setInterval` string forms all stay blocked.
Browser support is universal in the targets RelayTerm cares about
(Chrome 102+, Firefox 100+, Safari 15.4+ — see §6.1 for the Tauri
WebView caveat).

**Downside.** A staging-and-production rollout widens the WASM
compile surface on **every** same-origin page of the production
deployment, including operators that never flip the experimental
renderer gate on. The widening is small (no JS string-evaluation
exposure) but non-zero — `wasm-unsafe-eval` is a real source
expression the threat model has to account for (§5).

### 4.B · Avoid `WebAssembly.compile`/`instantiate` APIs entirely

ghostty-web 0.4.0's `Ghostty.loadFromPath` calls
`WebAssembly.compile` directly to instantiate the libghostty-vt
module. A path that avoids the compile primitive would require
upstream cooperation OR a different browser API.

**Realistic upstream paths.** None known today. The browsers'
streaming-instantiate (`WebAssembly.instantiateStreaming`) is
itself gated by `script-src 'wasm-unsafe-eval'` exactly the same
way — switching to streaming would not bypass the check. There is
no current browser API for executing WASM bytes without going
through `compile` / `instantiate` (the Module-from-cache path
still triggers the `script-src` check).

**Recommendation.** This option is effectively "wait for the
ecosystem"; pursue it as a long-tail upstream conversation but
do NOT block RelayTerm's evaluation track on it.

### 4.C · Keep ghostty-web blocked; evaluate restty / wterm first

restty (`@relayterm/terminal-restty`) is libghostty-vt-based and
also a WASM consumer — it carries the same
`WebAssembly.compile`-during-`mount` shape as ghostty-web, so
this option does NOT actually defer the CSP decision; it just
relabels the renderer that triggers it.

wterm (`@relayterm/terminal-wterm`) does NOT touch
`WebAssembly.compile` on the load path (its core is Zig+WASM but
inlined inside the JS bundle, and `@wterm/core@0.2.x` may or may
not need `'wasm-unsafe-eval'`; the answer is not in this repo's
evidence). **This option requires a real CSP probe of wterm
first** before it can be recommended over D, and that probe is a
separate slice. Not the path picked here.

### 4.D · Staging / evaluation profile only — RECOMMENDED

Add `'wasm-unsafe-eval'` to the staging surface's CSP
`script-src` only. Production deploy templates
(`deploy/docker-compose.example.yml`,
`deploy/docker-compose.images.example.yml`) and any future
strict-by-default production posture stay strict. Staging gets
the relaxation, which lets the renderer-evaluation runbook
collect real ghostty-web matrix evidence on the production shell.

After at least one staging resmoke passes against the relaxed
CSP without a redaction / correctness regression, a separate
follow-up slice decides whether to extend the relaxation to
production deploy examples (still gated behind the existing
`experimentalRendererEvaluationEnabled` operator opt-in at the
**workspace** layer — but note that **CSP cannot be operator-
gated at the workspace layer**, see §6.4).

## 5 · Threat model

### 5.1 · What `'wasm-unsafe-eval'` actually permits

- `WebAssembly.compile(bytes)`
- `WebAssembly.instantiate(bytes | module, importObject)`
- `WebAssembly.compileStreaming(response)`
- `WebAssembly.instantiateStreaming(response, importObject)`

It does NOT permit:

- the `eval` builtin called on a string
- the `Function` constructor with a string argument
- the `setTimeout` / `setInterval` string-argument forms
- `<script>` element insertion (still subject to `script-src`)
- inline event handlers (still subject to `script-src` +
  `'unsafe-inline'`)

The CSP3 source expression is deliberately a strict subset of
`'unsafe-eval'`. Adopting it does NOT open the door to broader
JS string-evaluation paths.

### 5.2 · Attacker model

The only scenario where `'wasm-unsafe-eval'` increases attacker
capability is: **an attacker has already achieved arbitrary
script execution inside the RelayTerm web origin** (e.g. via an
XSS, a malicious dependency that runs at load time, a
compromised CDN — RelayTerm self-hosts its assets, so this
narrows to RelayTerm-controlled supply chain). In that scenario:

- Without `'wasm-unsafe-eval'`: the attacker can run any JS the
  page can already run. Today that includes reading session
  cookies (mitigated by `HttpOnly`; see
  `docs/agent/redaction-rules.md` §4 and §5), reading
  `localStorage` (the
  `relayterm.terminal-settings.v1` blob is non-sensitive: a
  rendererId, a boolean gate, and cosmetic preferences), and
  exfiltrating any data the user can already see in the SPA
  (host metadata, public keys, audit-log surface, etc.).
- With `'wasm-unsafe-eval'`: the attacker can additionally
  compile and run arbitrary WebAssembly. This adds:
  - faster cryptographic primitives (e.g. compute-bound
    side-channels, fast hashing, fast key derivation),
  - the ability to embed prebuilt exploit payloads as WASM
    rather than JS,
  - access to the linear-memory model (a faster substrate for
    crypto, parsing, and code obfuscation).
- It does NOT add: filesystem access, network access beyond
  what the page already has (exfiltration is still bounded by
  `connect-src` / `default-src 'self'` — same-origin only —
  and by the page-level `frame-ancestors` / `form-action`
  defaults), DOM access (WASM still goes through JS imports),
  or the ability to bypass HttpOnly cookies.

**The narrow read.** `'wasm-unsafe-eval'` upgrades a JS-script-
execution capability to a JS-plus-WASM-execution capability. It
does NOT manufacture a script-execution capability where there
was none. RelayTerm's first-line defence remains the long-
standing XSS-prevention posture (every input boundary
validated, no raw bytes echoed to DOM,
`document.documentElement.outerHTML` sentinel sweeps in every
smoke entry).

### 5.3 · What RelayTerm relies on regardless of CSP

The load-bearing security controls RelayTerm holds today are
**not** weakened by `'wasm-unsafe-eval'`:

- HttpOnly session cookie (plaintext `session_token` never
  reachable from any script — `docs/agent/redaction-rules.md`
  §4 / §5).
- Backend-owned SSH session state (russh `Channel` stays
  server-side; client is a view — AGENTS.md "Architectural
  rule").
- Argon2id `OWASP_2023` password hashing
  (`docs/agent/redaction-rules.md` §6).
- CSRF / `Origin` allow-list at every state-changing route
  (§7), login throttle keyed on
  `normalize_login_identifier` (§9).
- Paste-safety pipeline (`evaluatePaste`, §10) — does NOT live
  in the renderer; the renderer never sees the paste content
  outside its viewport.
- Recording chunk redaction (§11 / §12) — chunk bytes cross
  the wire ONLY through `data_b64`; never logged.
- Encrypted-private-key redaction
  (`docs/agent/redaction-rules.md` overall + the
  per-adapter sentinel tests, e.g.
  `packages/terminal-*/tests/*Renderer.test.ts`).

None of those depend on CSP `script-src` excluding
`'wasm-unsafe-eval'`. The CSP layer is depth-in-defence, not
the primary control.

### 5.4 · Why this is an acceptable widening for an experimental renderer

The risk premium of `'wasm-unsafe-eval'` is non-zero but small,
the alternative paths (B / C) are blocked on upstream cooperation
or require a separate CSP-probe slice, and Option D contains the
widening to **staging-only** until the evaluation work has actual
matrix evidence. Production deploy posture stays strict by
default; the production-side decision is a deliberate later
slice on its own threat-model entry.

## 6 · Browser / surface implications

### 6.1 · Browser staging

Universally supported in the targets the renderer-evaluation
plan names (Firefox staging, Chrome via Playwright MCP, Safari
on macOS):

- Chrome / Edge / Chromium: `'wasm-unsafe-eval'` honoured since
  Chrome 102 (May 2022).
- Firefox: honoured since Firefox 100 (May 2022).
- Safari: honoured since Safari 15.4 (March 2022).

No browser-target carve-out is needed.

### 6.2 · Desktop Tauri

`apps/desktop/src-tauri/tauri.conf.json` sets `app.security.csp = null`
— **Tauri does not inject a CSP** into the WebView. The
production WebView on desktop sees ONLY whatever CSP the served
HTML carries. With the current staging Compose template, the
WebView gets the same `content-security-policy: default-src 'self'`
header the browser sees (because Tauri loads the same bundled
web frontend served by the same nginx).

A staging-side `'wasm-unsafe-eval'` relaxation therefore propagates
to a Tauri desktop session that hits the staging URL. A future
**production** Tauri-desktop slice that ships a different CSP
posture (e.g. an offline bundled-shell mode that does NOT round-
trip through nginx) is a separate decision out of scope here.

WebView CSP support on the desktop targets is **a function of
the WebView's underlying engine version, not of Tauri's
configuration.** Concretely:

- **WebView2 (Windows)** is evergreen — auto-updated from the
  Edge Stable channel — so any non-frozen deployment runs a
  Chromium build that honours `'wasm-unsafe-eval'` (Chrome 102+,
  May 2022). A WebView2 Fixed-Version distribution OR an
  enterprise LTSC channel can lag; an operator pinning to an
  older WebView2 runtime is responsible for verifying support.
- **WebKitGTK (Linux)** ships with the host distribution and
  is not auto-updated. CSP `'wasm-unsafe-eval'` is honoured
  from WebKit / WebKitGTK 2.36 onward (the same release that
  introduced Safari 15.4's support, March 2022); any current
  long-term-support Linux distro carries a newer WebKitGTK
  than that. A frozen WebKitGTK older than 2.36 would not.

No platform carve-out is needed for current desktop targets,
but the doc deliberately does not claim universal support
follows from "Tauri" — it follows from the WebView engine
version that happens to be installed.

### 6.3 · Android Tauri WebView

Same posture as the desktop shell: `app.security.csp = null` in
`apps/mobile/src-tauri/tauri.conf.json`; the Android System
WebView sees only the CSP the served HTML carries.

**Important nuance: API level ≠ WebView Chromium version.**
`minSdkVersion = 28` sets the **floor** for the Android API
RelayTerm targets (Android 9, factory WebView Chromium 68 in
2018) — but the Android System WebView is delivered separately
via Google Play and is auto-updated on every Play-equipped
device. On any Android 9+ device receiving Play updates the
shipped WebView is currently Chromium 102+ and honours
`'wasm-unsafe-eval'`. The cases where the assertion can fail:

- A device pinned to its factory WebView (no Play services, or
  WebView updates disabled at the system level).
- A non-Google AOSP build with an older bundled WebView.
- A WebView-replacement (e.g. some MIUI / EMUI variants on
  older firmware).

Operators evaluating a frozen-fleet deployment are responsible
for verifying support before treating ghostty-web as available.
For the renderer-evaluation track's staging smoke, the target
is the Tauri Android shell on a current developer device — that
configuration is in scope; frozen-fleet survey is out of scope.

### 6.4 · CSP is page-level, not per-renderer-id

A directly load-bearing observation: **CSP cannot be runtime-
gated on the per-operator
`experimentalRendererEvaluationEnabled` localStorage flag**.
CSP is a response header attached to the HTML document; the
browser locks it in at parse time. Even though the renderer
selector is hidden by default and the experimental adapters are
behind dynamic `import()`s, flipping `'wasm-unsafe-eval'` on in
the CSP affects **every** page-load of that deployment for
**every** operator, regardless of whether the experimental
gate is on.

This is why the recommendation is staging-only (a deployment-
level boundary, not a per-operator boundary): the deployment's
CSP applies to everyone who loads that origin. A "production
deploy with CSP relaxed for everyone" decision is a separate
policy choice from "production deploy with the experimental
renderer surface available to operators who flip a gate."
Option D contains the page-level widening to the staging
deployment.

## 7 · Recommended next path

Land Option D — `'wasm-unsafe-eval'` in `script-src`, **staging
only**, as a host-side Traefik `secure-chain@file` tweak (since
that is where the current staging CSP comes from). After a
clean staging resmoke and a soak window, decide separately
whether to extend the relaxation to production deploy examples
(or to ship an explicit CSP in `deploy/nginx/web.conf.template`
that production operators opt into).

In one paragraph: **xterm stays the production default;
ghostty-web stays experimental and operator-gated at the
workspace layer; the CSP relaxation is contained to staging,
where the renderer-evaluation matrix can finally collect
ghostty-web evidence; production deployments inherit no CSP
change from this slice.**

## 8 · Proposed implementation slice boundary

(Suggested branch name: `deploy/staging-csp-wasm-unsafe-eval`.
Illustrative — the slice can pick its own.)

### In scope

- Add `'wasm-unsafe-eval'` to `script-src` on the staging
  surface's CSP. Concrete mechanism is for the slice to pick:
  - host-side Traefik `secure-chain@file` middleware edit
    (lives outside this repo; the slice records the diff in a
    deploy-host runbook), **or**
  - an explicit `add_header Content-Security-Policy` line in
    `deploy/nginx/web.conf.template` gated by a new env var
    (default off — production deploy examples stay strict),
    **or**
  - a new `deploy/docker-compose.staging-evaluation.example.yml`
    overlay that sets the env var on.
- One sentence in the new doc explaining why the slice picked
  the mechanism it picked.
- Update `apps/web/e2e/SMOKE.md` § "D. Renderer evaluation
  smoke" with the new CSP precondition (Operator MUST confirm
  the deployment serves `script-src … 'wasm-unsafe-eval' …`
  before the matrix rows can run).

### Out of scope (explicit)

- Any `'unsafe-eval'` widening. NEVER. The narrow form is the
  only direction.
- Any `data:` source on any directive. The asset-loading fix
  closed that surface; do not regress.
- Any `connect-src` change. No evidence it is needed.
- Any change to `default-src`, `style-src`, `img-src`,
  `font-src`, `frame-ancestors`, `base-uri`, `form-action`,
  `worker-src`, `manifest-src`.
- Any change to the production deploy examples — the
  production-side CSP decision is a separate slice that runs
  after the staging soak.
- Any renderer promotion. ghostty-web stays experimental;
  xterm stays default; Gate 1 / Gate 2 criteria in
  `docs/terminal-renderer-evaluation.md` are unchanged.
- Any backend / protocol / session / orchestrator /
  `terminal-core` / production-shell-non-loader / schema /
  migration / auth / CSRF / paste / recording / audit /
  redaction change.
- Any persistent per-user / per-device renderer preference
  beyond the current `relayterm.terminal-settings.v1`
  localStorage entry.

## 9 · Proposed staging smoke (post-CSP-change)

Mirrors the existing
`docs/deployment/vps-staging-smoke.md` template. Posture:

1. Recreate the staging stack against the unchanged
   `:main` images (no new web / backend image needed — this
   is a CSP-only change).
2. `curl -sSI https://relayterm-staging.js-node.cc/` and
   confirm:
   `content-security-policy:` carries
   `script-src 'self' 'wasm-unsafe-eval'` (and **nothing
   else** different from the prior strict posture — no
   `'unsafe-eval'`, no `data:`, no widened `connect-src`).
3. Flip the operator gate ON in Settings, select
   `ghostty-web`, launch the same hermetic SSH target
   pattern (`linuxserver/openssh-server:latest`,
   internal-only Compose network, no host port, key pasted
   via the same redaction-safe pattern). Confirm:
   - `data-renderer="ghostty-web"` (NOT `unmounted`).
   - `data-renderer-fallback=""` (empty — the fallback
     taxonomy does NOT fire).
   - `data-renderer-experimental="true"`,
     `data-renderer-gate="on"`.
   - Sentinel sweep over `document.documentElement.outerHTML`
     for `BEGIN OPENSSH PRIVATE KEY`, `openssh-key-v1`
     magic, `encrypted_private_key`, `session_token`,
     `token_hash`, `data_b64`, `REDACT-MARKER` returns
     zero hits.
4. Walk the renderer evaluation matrix
   (`docs/renderer-smoke-harness.md`) row-by-row for
   ghostty-web on the production shell. Record results in
   `docs/deployment/vps-staging-smoke.md` peer-to-peer with
   the 2026-05-13 xterm production-baseline entry.
5. Flip the gate OFF → relaunch → confirm xterm fallback
   still works (parity with the 2026-05-14b resmoke).
6. Tear down the throwaway SSH container; nothing else
   touched.

The smoke is the **first ghostty-web Gate 1 evidence on the
production shell** under the renderer-evaluation track. It is
NOT a Gate 2 default-flip. It is NOT a promotion. xterm stays
the production default after the smoke regardless of how
ghostty-web grades.

## 10 · Explicit non-goals

This decision doc deliberately does **not** address:

- **Renderer promotion.** Gate 1 / Gate 2 are unchanged. A
  ghostty-web matrix pass under the relaxed staging CSP is one
  data point, not a default flip.
- **Production deploy CSP change.** A separate slice, after the
  staging soak. Production deploy examples
  (`deploy/docker-compose.example.yml`,
  `deploy/docker-compose.images.example.yml`,
  `deploy/docker-compose.traefik-staging.example.yml`) stay at
  their current CSP posture until that later slice lands.
- **restty / wterm CSP probes.** Each is its own slice. wterm
  in particular needs a real CSP probe before the doc can
  claim it is or isn't affected by the same gap.
- **Upstream ghostty-web changes.** Out of scope. Option B
  remains a long-tail conversation, not a blocker for D.
- **Tauri production-shell CSP.** The desktop and mobile
  shells set `csp: null` and inherit the served HTML's CSP;
  any future bundled-offline-shell posture is a separate
  decision.
- **The Tauri `csp: null` posture itself.** That setting is
  the existing project posture (see `apps/desktop/src-tauri/`
  and `apps/mobile/src-tauri/`); revisiting it is a separate
  slice with its own threat model.
- **Operator-gated CSP.** Not possible at the workspace
  layer (§6.4). Don't try.
- **A new `experimentalRendererEvaluationEnabled` analogue at
  the deploy layer.** The deployment is the gate. If a
  production deployment opts into the relaxed CSP later, it
  applies to every page-load on that origin.

## See also

- `docs/terminal-renderer-evaluation.md` § "2026-05-14 ·
  ghostty-web WASM-as-asset staging resmoke" — the resmoke
  this doc threads from.
- `docs/spec/terminal-adapters.md` § "ghostty-web experimental
  renderer adapter" — the adapter contract; describes the
  asset-loading approach but defers the CSP `script-src`
  decision to this doc.
- `docs/deployment/vps-staging-smoke.md` § "2026-05-14b ·
  Ghostty-web WASM-as-asset resmoke" — the staging smoke
  that pinned the remaining `'wasm-unsafe-eval'` gap.
- `docs/agent/redaction-rules.md` — the security controls
  the renderer evaluation does NOT widen.
- `apps/web/e2e/SMOKE.md` § "D. Renderer evaluation smoke" —
  the runbook step that the implementation slice must
  update to record the new CSP precondition.
- `packages/terminal-ghostty-web/src/wasmUrl.ts` — the
  adapter-side asset-loading helper that closed half the gap.
- `packages/terminal-ghostty-web/src/GhosttyWebRenderer.ts`
  file header — the same CSP context recorded
  next to the adapter implementation.
