# Terminal renderer evaluation plan

> Product / technical plan for RelayTerm's next phase after the
> single-user deployable baseline. This doc is a **plan**, not a
> contract: nothing here ships before the matching SPEC entries and
> per-package contracts land. The renderer-adapter contracts proper
> live in [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md);
> the architectural invariants they sit under live in
> [`SPEC.md`](../SPEC.md) § "Architectural invariants" and AGENTS.md.

## Status

**Draft, not started.** The deployable baseline is on `main` and was
exercised end-to-end against staging on 2026-05-13 (see
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-13 · Deployable-baseline end-to-end staging smoke"). The
private-key import v1 smoke landed the same day. Renderer evaluation
is the next product track. Quota / metrics / dashboard work is
paused unless an explicit ask reopens it.

A **reference smoke for the production xterm baseline renderer**
landed on 2026-05-13 (see
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-13 · Xterm production-baseline renderer smoke"). It
exercises launch, basic I/O, in-session resize / fit, 300-line
burst, wire-side detach / reconnect inside the 30 s TTL,
mobile-width workspace, and clean close against a hermetic
throwaway SSH target with no host-port exposure and zero
sentinel leakage in DOM / logs / audit. It is the comparison
point future ghostty-web / restty / wterm candidate smokes are
measured against. The baseline smoke deliberately did NOT
exercise Unicode, copy / paste, alternate screen, or mouse
support (input-path limitations recorded inline in the smoke
entry); each is its own deferred slice. The harness plan that
carries those deferred rows forward — input-path taxonomy,
recommended renderer-fairness strategy, command matrix, and
next-slice options — lives in
[`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md);
the experimental renderer comparisons (ghostty-web → restty →
wterm) stay deferred until that harness plan's recommended
implementation slice lands. **xterm is and remains the
production compatibility baseline and the default renderer**
until a candidate clears the Gate 2 promotion criteria below.

A **renderer comparison scorecard** —
[`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
— summarises the current production-shell evidence for all four
adapters in one place (status table, per-category qualitative
labels, recommended next development lane). It is a snapshot of
evidence, not a promotion, and is the recommended starting point
for choosing the next renderer slice.

### 2026-05-13 · ghostty-web renderer-evaluation status (docs-only)

A docs-only evaluation slice on 2026-05-13 attempted to carry the
ghostty-web experimental renderer through the renderer evaluation
runbook (`apps/web/e2e/SMOKE.md` § "D. Renderer evaluation smoke")
and the input-path taxonomy in
[`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md), then
compare it against the 2026-05-13 xterm production-baseline smoke.
The slice stopped at the gate check below before running any
matrix row, on the rule already stated in `apps/web/e2e/SMOKE.md`
§ "Renderer path confirmation": "Until [a stable production-shell
selector] exists, experimental renderers are exercised through the
dev lab only … A dev-lab pass is **not** a production renderer
pass."

Findings on this date, **without source changes**:

- **Where the adapter is wired today.** `@relayterm/terminal-ghostty-web`
  is reachable only through the dev-only live terminal lab —
  `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` constructs the
  adapter at `new GhosttyWebRenderer(themed)` and exposes a
  `data-testid="renderer-option-ghostty-web"` radio in its renderer
  picker. `DevTerminalWorkbench` (the lab's host) is mounted only
  inside the `import.meta.env.DEV` branch of `App.svelte`. The
  production-shell isolation rule
  ("`apps/web/src/lib/app/**` cannot import from `lib/dev/` or any
  experimental renderer adapter package") is pinned by
  `apps/web/tests/appShellIsolation.test.ts`.
- **Production status.** **Production-excluded by both gates** — the
  `import.meta.env.DEV` constant inlines as `false` in production
  builds, Rollup eliminates the dev branch, and the
  `appShellIsolation` test forbids any production-shell import of
  the experimental adapter packages. ghostty-web's inlined WASM
  data URL is tree-shaken out of the production `apps/web` bundle.
- **Selection surface today, without code changes.** Only the
  **local Vite dev server** (`pnpm --filter @relayterm/web dev`,
  or equivalent), which serves the dev-mode build with the dev
  lab mounted. The staging stack, the production stack, and the
  packaged Tauri shells all consume the production build and
  cannot select ghostty-web. A `tauri dev` against a local Vite
  dev server could in principle reach the dev lab inside the
  Tauri WebView, but that is a different surface (WebKitGTK on
  Linux / WebView2 on Windows / Android WebView) and is out of
  scope for the first ghostty-web pass per the "Surfaces" list
  below.
- **Fairness vs. the 2026-05-13 xterm production-baseline smoke.**
  The xterm baseline ran against the **production shell**
  (`apps/web/src/lib/app/terminal/ProductionTerminal.svelte`), which
  drives the production paste-safety pipeline, the production
  detach / reconnect buttons, the production audit-event surface,
  and the production-shell selector hooks. A local-dev-lab
  ghostty-web pass would run through the lab UI — different
  attach/detach controls, a different event log, no production
  paste-policy panels, no production telemetry surface — so any
  side-by-side reading would compare two **surfaces** as much as
  two renderers. Recording dev-lab ghostty-web results next to
  the xterm production-baseline entry would overstate parity, so
  this slice deliberately does **not** run that smoke.
- **Promotion posture.** ghostty-web remains **experimental** and
  is **not promoted**. The production default remains xterm.
  Gate 1 and Gate 2 criteria are unchanged. No backend protocol,
  session, orchestrator, `terminal-core`, production-shell, CI,
  or deploy file was touched by this slice.

The evaluation cannot move further without the Gate 1 production-
shell experimental-renderer selector that the runbook's "Renderer
path confirmation" step assumes. The recommended next
implementation slice is captured under [§ "Smoke plan" →
"Recommended next implementation slice"](#recommended-next-implementation-slice)
below.

### 2026-05-13 · production-shell experimental-renderer selector landed (Gate 1 prerequisite, not promotion)

The recommended next slice from the entry above landed on the same
date. The production shell now carries an operator-opt-in
**experimental renderer evaluation** gate:

- Hidden by default. Off by default. xterm is and remains the
  production compatibility baseline and the default renderer.
- Gate UI lives in the Settings view at
  `[data-testid="settings-experimental-renderer"]` (toggle, warning
  copy, renderer radio group, effective-renderer diagnostic). When
  the operator flips the toggle off, the persisted renderer id is
  reset back to `xterm` so a stale experimental selection cannot
  survive a future flip.
- Renderer selection persists only in `localStorage`
  (`relayterm.terminal-settings.v1` — same store as the existing
  cosmetic preferences). No backend / per-user / per-device
  persistence work was introduced. Validation is strict: any unknown
  renderer id collapses to `xterm`, and the gate flag only accepts
  the literal boolean `true`.
- Experimental adapters reach the production shell ONLY through
  `apps/web/src/lib/app/terminal/rendererLoader.ts`, and ONLY via
  `dynamic import()`. The default-renderer attach path still pulls
  no experimental WASM into the main bundle — Vite/Rollup
  chunk-splits each experimental adapter into its own asset.
- The production terminal workspace surfaces which renderer was
  actually mounted via:
  - `data-renderer` (= `xterm` | `ghostty-web` | `restty` | `wterm`)
  - `data-renderer-experimental` (`"true"` / `"false"`)
  - `data-renderer-fallback` (closed vocabulary:
    `""` | `experimental_gate_off` | `unknown_renderer_id` |
    `adapter_load_failed` | `adapter_mount_failed`)
  - `data-renderer-gate` (`"on"` / `"off"`)
  - the visible diagnostic strip
    `[data-testid="production-terminal-renderer-diagnostic"]`
- Synchronous failure paths (gate off + experimental id selected,
  unknown persisted id, dynamic import / constructor failure) fall
  back silently to xterm — `data-renderer="xterm"` AND the reason on
  `data-renderer-fallback` ∈ `{experimental_gate_off,
  unknown_renderer_id, adapter_load_failed}`. The asynchronous
  mount-failure path landed 2026-05-13 (see the ghostty-web
  CSP-blocked entry below): if `renderer.mount(target)` rejects, the
  workspace stays `data-renderer="unmounted"` AND surfaces
  `data-renderer-fallback="adapter_mount_failed"` plus the
  operator-facing error copy `Renderer failed to mount. Switch back
  to xterm in Settings and reopen the terminal.` The underlying
  `Error.message` is never echoed to the workspace, the audit log,
  or the console — the fallback taxonomy is a closed vocabulary by
  design.

This slice is the prerequisite the "Renderer path confirmation" step
in `apps/web/e2e/SMOKE.md` § "D. Renderer evaluation smoke" assumed.
It is explicitly **not** a Gate 1 / Gate 2 promotion: ghostty-web,
restty, and wterm remain experimental. xterm remains the default and
the only supported production baseline. Promotion still requires the
full Gate 1 / Gate 2 evidence under
[§ "Promotion criteria"](#promotion-criteria), and is its own
deliberate later slice.

Architectural posture unchanged by this slice: no backend protocol
change, no session / orchestrator change, no `terminal-core` change,
no schema or migration change. The static-import isolation rule that
`apps/web/tests/appShellIsolation.test.ts` enforces was sharpened —
references to experimental adapter package names are now allowed
ONLY inside the renderer loader file AND only inside dynamic
`import()` expressions.

### 2026-05-13 · ghostty-web production-shell smoke (CSP-blocked, remains experimental)

The Gate-1 production-shell selector that landed earlier the same
day unblocked the first production-side renderer-evaluation pass for
ghostty-web. The full smoke entry is in
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-13 · Ghostty-web production-shell renderer smoke
(CSP-blocked; xterm fallback verified)".

**Operator pass posture.** Gate flipped on through Settings, renderer
`ghostty-web` selected, persisted to
`relayterm.terminal-settings.v1`, terminal launched against a
hermetic throwaway target on the staging Compose network. The
selector + diagnostic surface (`data-renderer`,
`data-renderer-experimental`, `data-renderer-fallback`,
`data-renderer-gate`, `[data-testid="production-terminal-renderer-diagnostic"]`)
worked exactly as `apps/web/e2e/SMOKE.md` § "Renderer path
confirmation" specifies.

**ghostty-web result.** **All evaluation-matrix rows deferred under
`deferred — renderer not identified`** (per
`apps/web/e2e/SMOKE.md` § "Renderer path confirmation" closed
vocabulary; the staging smoke entry records the same label with a
"ghostty-web adapter failed to mount" free-form suffix). The
proof that no renderer code ran is the post-launch attribute
dump: `data-renderer="unmounted"` (never transitioned to a
renderer id), `data-renderer-experimental="false"`,
`data-renderer-fallback=""` (empty), `data-renderer-gate="on"`,
`data-phase="idle"`. ghostty-web 0.4.0 inlines its WASM payload as
a `data:application/wasm;base64,…` URL and calls
`WebAssembly.compile()` from inside its
`Terminal.open`/`loadFromPath` path during `r.mount(mountTarget)`.
The staging stack's nginx CSP (`default-src 'self'` with no
`'unsafe-eval'`, no `'wasm-unsafe-eval'`, no explicit `connect-src`)
blocked the data-URL fetch AND the WASM compile step. The dynamic
`import()` itself resolved, so the loader's
`adapter_load_failed` fallback (synchronous-load-failure path) did
NOT fire — the rejection happened later inside `r.mount(...)`, and
the production workspace's `attach()` does not catch errors thrown
by `mount()`. The result is a wedged workspace with no
operator-visible error panel.

This gap was a real one in the loader's fallback taxonomy at the
time of the smoke: the loader's three synchronous values
(`experimental_gate_off`, `unknown_renderer_id`,
`adapter_load_failed`) covered synchronous loader paths but not
asynchronous `mount()` rejection. The gap was closed in
`feat/renderer-mount-failure-diagnostics`: the production workspace
now wraps `r.mount(mountTarget)` in `mountRendererSafely` (defined
in `apps/web/src/lib/app/terminal/terminalLaunch.ts`), translates
any rejection into a fourth taxonomy value `adapter_mount_failed`,
disposes the half-built renderer, and surfaces a fixed operator-
facing copy (`Renderer failed to mount. Switch back to xterm in
Settings and reopen the terminal.`) in `production-terminal-error`.
The closed vocabulary on `data-renderer-fallback` is now
`{experimental_gate_off, unknown_renderer_id, adapter_load_failed,
adapter_mount_failed}`; `apps/web/e2e/SMOKE.md`'s selector
vocabulary row mirrors the same set. A future operator hitting the
same CSP / WASM / data-URL gotcha sees the typed diagnostic + error
panel instead of a stuck `idle` phase. The fix did NOT attempt to
make ghostty-web CSP-compatible; that is its own slice. xterm
remains the production default; the operator must still flip the
persisted renderer back to xterm in Settings to recover (the
workspace deliberately does not auto-mutate persisted settings).

**xterm fallback verification.** After flipping the gate OFF (which
the `onExperimentalGateChange` handler resets to `rendererId="xterm"`
explicitly), a fresh launch on the same profile mounted with
`data-renderer="xterm"`, `data-renderer-experimental="false"`,
`data-renderer-gate="off"`, diagnostic strip "Renderer. xterm
baseline". This proves the production shell stays usable when an
experimental adapter fails — it does **not** count as a
ghostty-web matrix pass and is not graded as one in the staging
smoke entry. xterm's path is unchanged from the
2026-05-13 xterm production-baseline entry.

**Promotion posture.** **ghostty-web remains experimental.** xterm
remains the production compatibility baseline and the default
renderer. No backend protocol, session, orchestrator,
`terminal-core`, production-shell-non-loader, CI, or deploy file
was touched by this slice. A future smoke able to grade ghostty-web
matrix rows requires either (a) a ghostty-web build that ships WASM
as an asset rather than a data URL, or (b) a deploy-side CSP change
that allows `'wasm-unsafe-eval'` plus `data:` in `connect-src`.
Both are separate slices and out of scope here.

**Deferred from this slice (per the staging smoke entry).** restty
/ wterm experimental evaluation, desktop / Android Tauri renderer
smokes, automated performance / benchmark harness, the
loader-fallback taxonomy extension above, `tmux` / `screen` host-side
multiplexer persistence, VT snapshot persistence, Gate-2 default
flip, persistent per-user / per-device renderer preference.

### 2026-05-14 · ghostty-web mount-failure diagnostic resmoke (adapter_mount_failed verified on staging)

A docs-only resmoke on 2026-05-14 against the staging stack
recreated from images carrying
`239fe29 feat(web): handle renderer mount failures` exercised the
same ghostty-web → production-shell launch path as the
2026-05-13 ghostty-web entry above. The CSP/WASM
`data:application/wasm` block still fires, but the workspace now
exposes:

- `data-renderer="unmounted"`,
  `data-renderer-fallback="adapter_mount_failed"`,
  `data-renderer-gate="on"`,
- the operator-facing fixed copy `Renderer failed to mount. Switch
  back to xterm in Settings and reopen the terminal.` in
  `production-terminal-error`,
- the matching diagnostic in
  `production-terminal-renderer-diagnostic`,

with the underlying `Error.message`, the CSP directive text, and
the inlined-WASM `data:` URL still confined to the browser console
(zero hits in DOM, `localStorage`, `audit_events.payload`, or any
docker log). xterm recovery on the same profile (gate OFF →
relaunch) attached cleanly and executed the smoke sentinels.

The resmoke is **not** a ghostty-web matrix pass — every
evaluation-matrix row stays `deferred — renderer not identified
(adapter_mount_failed)` under the closed
`apps/web/e2e/SMOKE.md` § "Renderer path confirmation" vocabulary.
The 2026-05-13 wedged-`idle` failure mode is closed; the
ghostty-web CSP/WASM compatibility fix, the renderer-evaluation
matrix itself, restty/wterm smokes, desktop/Android renderer
smokes, automated benchmark harness, and any renderer promotion
remain deferred per the prior entry's deferral list. Full smoke
entry: [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14 · Ghostty-web renderer mount-failure diagnostic
resmoke (adapter_mount_failed verified; xterm recovery still
works)".

### 2026-05-13b · ghostty-web WASM-as-asset adapter fix (data URL removed; staging resmoke pending)

The adapter-side half of the CSP/WASM compatibility gap called
out in the 2026-05-13 ghostty-web entry above ("Promotion
posture") landed in `feat/ghostty-web-wasm-asset-loading`. The
`@relayterm/terminal-ghostty-web` adapter now loads its WASM via
a same-origin Vite-emitted asset URL instead of upstream's
inlined `data:application/wasm;base64,…` URL:

- `packages/terminal-ghostty-web/src/wasmUrl.ts` imports
  `ghostty-web/ghostty-vt.wasm?url`. Vite copies the upstream
  package's sibling `.wasm` (exposed via ghostty-web's
  `exports` map at `./ghostty-vt.wasm`) into the production
  build's `dist/assets/` directory with a fingerprinted
  filename (e.g. `dist/assets/ghostty-vt-DOMeXDrv.wasm`).
- `GhosttyWebRenderer.mount` calls `Ghostty.load(wasmUrl)`
  directly and passes the resulting instance into
  `new Terminal({ ghostty })`, so upstream's no-arg `init()`
  sugar — the only call site that consumes the inlined data URL
  — is never reached.
- A static-source pin
  (`packages/terminal-ghostty-web/tests/wasmAssetSource.test.ts`)
  asserts the adapter neither imports `init` nor embeds an
  executable `data:application/wasm` literal, and that
  `wasmUrl.ts` imports the upstream subpath with Vite's `?url`
  suffix.

What this fix removes from the production CSP gap: the
`connect-src` rejection of the inlined `data:application/wasm`
URL. What it does NOT remove: `WebAssembly.compile()` /
`WebAssembly.instantiate()` inside upstream's
`Ghostty.loadFromPath` still require `'wasm-unsafe-eval'` in the
deployment's CSP `script-src`. That is upstream-baked and
explicitly out of scope for this slice; closing it is a separate
deploy-side or upstream-patch decision.

The upstream `ghostty-web@0.4.0` bundle continues to embed the
inlined data URL as text inside `dist/ghostty-web.js` because
Rollup cannot prove the no-arg branch of `Ghostty.load(A)` is
unreachable; the literal therefore survives into the lazy
ghostty-web chunk as dead code. The runtime never `fetch`es it.
The main entry chunk (default xterm path) does not reference
either the data URL or the emitted `.wasm` asset.

This is **adapter-side only**: no backend protocol change, no
session / orchestrator change, no `terminal-core` change, no
schema or migration change, no deployment CSP change, no
ghostty-web promotion, no xterm-default flip. xterm remains the
production compatibility baseline and the default renderer; the
production-shell experimental-renderer gate continues to apply.

**Staging verification posture.** The adapter slice was
validated locally first — `pnpm -r build` confirmed
`dist/assets/ghostty-vt-<hash>.wasm` emission and the
renderer / adapter test suites passed. The staging resmoke
landed 2026-05-14 (see the next section below) and
**confirmed option (i) — `'wasm-unsafe-eval'` is
independently required to actually mount the renderer
under the staging stack's current CSP**. The
ghostty-web evaluation-matrix rows therefore stay deferred
under the closed `apps/web/e2e/SMOKE.md` vocabulary
until either (a) a deploy-side CSP slice adds
`'wasm-unsafe-eval'` to `script-src`, or (b) an upstream
`ghostty-web` patch removes the `WebAssembly.compile()`
requirement from `Ghostty.loadFromPath`. Both are
separate, deliberate slices that this docs entry does
NOT authorise.

### 2026-05-14 · ghostty-web WASM-as-asset staging resmoke (data: CSP block closed; `'wasm-unsafe-eval'` still blocks compile)

A docs-only resmoke on 2026-05-14 against the staging
stack — recreated from the `:main` images that include
`aa6bf9f fix(web): load ghostty wasm as an asset` —
verified the adapter-side fix on the live production
shell. Full smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14b · Ghostty-web WASM-as-asset resmoke
(data: CSP block closed; wasm-unsafe-eval still blocks
compile; xterm recovery still works)".

What the resmoke confirmed, on the production shell,
without any source / CI / deploy / CSP changes:

- The new same-origin asset emits and serves cleanly:
  `https://relayterm-staging.js-node.cc/assets/ghostty-vt-<hash>.wasm`
  returns HTTP 200 with `content-type: application/wasm`
  and the standard `/assets/*` immutable cache. The
  recreated web container's
  `/usr/share/nginx/html/assets/` lists exactly one
  fingerprinted `ghostty-vt-<hash>.wasm` file
  (423,045 bytes); the pre-recreate listing had none.
- The runtime fetches the asset via `Ghostty.load(wasmUrl)`:
  `performance.getEntriesByType('resource')` showed
  the asset URL with `initiatorType="fetch"`,
  `responseStatus=200`, `decodedBodySize=423045`,
  `duration≈82 ms`. The inlined
  `data:application/wasm;base64,…` URL is no longer
  the load path. The two `data:application/wasm` CSP
  errors the 2026-05-14 mount-failure resmoke recorded
  in the browser console **did not fire**.
- `WebAssembly.compile()` inside upstream's
  `Ghostty.loadFromPath` still rejects with the
  `'unsafe-eval' is not an allowed source of script`
  CompileError. A direct `await WebAssembly.compile(<8-byte
  minimal WASM>)` issued from `browser_evaluate` rejected
  identically — confirming the remaining gap is the
  `WebAssembly.compile` call itself, not anything
  specific to the ghostty-vt bytes.
- ghostty-web therefore still fails to mount under the
  staging CSP. `data-renderer-fallback="adapter_mount_failed"`
  fires cleanly with the fixed operator-facing copy from
  `feat(web): handle renderer mount failures`. xterm
  recovery on the same profile still works end-to-end
  (the smoke ran `echo relayterm-ghostty-asset-resmoke-xterm`
  and `whoami → smoke` round-trips and closed the
  session via `End session`).

What this slice did **not** do (deferred):

- Evaluation-matrix rows for ghostty-web — every row
  stays `deferred — renderer not identified
  (adapter_mount_failed)`.
- A deploy-side CSP slice adding `'wasm-unsafe-eval'`
  to `script-src` (the directive widens the execution
  policy for ALL same-origin scripts, not just WASM
  compile, and needs its own threat-model entry).
- Any upstream ghostty-web patch.
- restty / wterm / desktop-Tauri / Android-Tauri
  smokes; benchmark harness; renderer promotion;
  per-user / per-device renderer preference
  persistence beyond the current
  `relayterm.terminal-settings.v1` localStorage entry.

Architectural posture unchanged: no backend protocol,
session, orchestrator, `terminal-core`, production-shell-
non-loader, CI, or deploy file was touched. xterm remains
the production compatibility baseline and the default
renderer.

### 2026-05-13 · ghostty-web WebAssembly CSP decision doc (proposed; no implementation)

A docs-only design / threat-model slice on 2026-05-13 wrote up
the next decision the renderer-evaluation track needs in order
to collect ghostty-web Gate 1 evidence on the production shell:
how (and whether) RelayTerm should permit
`WebAssembly.compile` / `WebAssembly.instantiate` under the
current strict CSP. Full doc:
[`docs/ghostty-web-wasm-csp.md`](ghostty-web-wasm-csp.md).

**Posture (load-bearing summary, NOT an authorisation to
implement).**

- Recommended next path is **Option D — staging only**: add
  `'wasm-unsafe-eval'` (and only that) to `script-src` on the
  staging surface's CSP; production deploy examples
  (`deploy/docker-compose.example.yml`,
  `deploy/docker-compose.images.example.yml`,
  `deploy/docker-compose.traefik-staging.example.yml`) stay
  strict.
- The broader `'unsafe-eval'` source expression is **NOT** the
  fix; the doc distinguishes the two and rules `'unsafe-eval'`
  out.
- `data:` is **NOT** re-added to any directive — the
  `aa6bf9f fix(web): load ghostty wasm as an asset` slice
  closed the data-URL surface; reintroducing it would regress.
- `connect-src` is **NOT** changed; the same-origin asset fetch
  already works under the current `default-src 'self'`.
- CSP is page-level, not per-renderer-id — the operator gate
  in Settings cannot scope a CSP relaxation per-user; the
  staging-only scope is what contains the widening.
- xterm remains the production compatibility baseline and the
  default renderer. ghostty-web (and restty, wterm) stay
  experimental and operator-gated at the workspace layer.
  Gate 1 / Gate 2 promotion criteria are unchanged.

The decision doc proposes the implementation slice boundary
and the staging resmoke shape; the slice itself is a separate,
later, deliberate slice and is **not** authorised by this
entry. The CSP / WASM compatibility row in the per-candidate
deferral list above remains deferred until that slice lands
and a fresh staging resmoke records ghostty-web matrix
evidence under the relaxed CSP.

### 2026-05-14c · Staging-only CSP `'wasm-unsafe-eval'` landed; first ghostty-web production-shell mount (matrix rows still deferred)

The Option D recommendation from the 2026-05-13 CSP
decision doc landed on the staging surface only. Full
smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14c · Staging-only CSP `'wasm-unsafe-eval'` +
ghostty-web production-shell mount".

What landed:

- Host-side Traefik file-provider middleware
  `relayterm-staging-secure-chain@file` (new, scoped to
  the staging router only — `secure-chain@file` and
  `default-security-headers` are unchanged, so other
  consumers like `weathrs`/`rstify`/`tinyauth` keep the
  original strict CSP).
- Staging router CSP header is now
  `default-src 'self'; script-src 'self' 'wasm-unsafe-eval'`.
  `'unsafe-eval'` is **NOT** added; `data:` is **NOT**
  added; `blob:` is **NOT** added; `connect-src` is
  unchanged.
- ghostty-web mounted on the production shell —
  `data-renderer="ghostty-web"`, `data-phase="attached"`,
  `data-renderer-fallback=""` — for the first time. The
  same-origin `ghostty-vt-<hash>.wasm` asset fetches at
  HTTP 200; `WebAssembly.compile` no longer rejects.
  Zero console errors during the ghostty-web mount.
- xterm recovery on the same profile still works
  end-to-end (`data-renderer="xterm"`, sentinel
  `relayterm-ghostty-csp-xterm-recovery` round-tripped
  cleanly).

What did **not** change:

- Repo production deploy templates
  (`deploy/docker-compose.example.yml`,
  `deploy/docker-compose.images.example.yml`,
  `deploy/docker-compose.traefik-staging.example.yml`)
  remain strict and **were not edited**.
- Repo nginx `web.conf.template` was not edited (still
  emits no CSP — the staging CSP comes from host-side
  Traefik).
- ghostty-web evaluation-matrix rows
  (Unicode / box drawing / paste / alternate-screen /
  mouse / 300-line burst / detach-reconnect-replay)
  remain `deferred` under the closed
  `apps/web/e2e/SMOKE.md` § "Renderer path
  confirmation" vocabulary — this slice's MCP input
  path could not consistently drive ghostty-web's
  xterm-compat shim, which is a renderer-fairness gap
  that belongs to the renderer-evaluation harness
  slice, not the CSP slice. The slice goal (CSP
  precondition unblocked, mount verified) is met
  without grading the matrix.
- Gate 1 / Gate 2 promotion criteria are unchanged.
  ghostty-web stays **experimental and unpromoted**;
  xterm stays the **production compatibility baseline
  and default renderer**. No backend protocol,
  session, orchestrator, `terminal-core`, or
  production-shell-non-loader file was touched.
- A separate later slice is required to actually grade
  ghostty-web matrix rows under the relaxed CSP
  (renderer-fairness input strategy is its
  precondition). The production-side CSP decision —
  whether to extend the relaxation to the production
  deploy examples — is its own deliberate later slice,
  not authorised by this entry.

### 2026-05-14d · renderer-fair input affordance landed (precondition for grading the matrix; still not a matrix run)

The renderer-fairness input strategy the 2026-05-14c entry
named as a precondition landed in
`feat/renderer-evaluation-input-fairness`. The
`TerminalRenderer` interface gained an optional
`focusTarget(): HTMLElement | null` (implemented for the
xterm and ghostty-web adapters; restty / wterm deferred),
and the production workspace stamps a renderer-neutral
marker `data-relayterm-terminal-input` on the element a
real keystroke hits — xterm's hidden helper textarea or
ghostty-web's contenteditable host. `apps/web/e2e/SMOKE.md`
§ "D. Renderer evaluation smoke" → "Renderer-fair input"
now carries a concrete focus + verify procedure keyed on
that selector. Detail:
[`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md)
§ "2026-05-14 · renderer-fair input affordance landed".

Posture unchanged: no backend protocol / session /
orchestrator change, no new wire surface, no
WebSocket-injection input path (the harness plan's
rejected Path I stays rejected). xterm remains the
production compatibility baseline and the default
renderer; ghostty-web / restty / wterm stay experimental
and operator-gated. This slice is the **input
precondition** — it does NOT itself run the ghostty-web
evaluation matrix; that stays deferred to its own staging
smoke slice.

### 2026-05-14e · ghostty-web production-shell renderer matrix smoke (first graded matrix; not a promotion)

The staging smoke slice the 2026-05-14d entry named as
deferred landed. The full smoke entry is in
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14e · Ghostty-web production-shell renderer
matrix smoke (first graded matrix; xterm recovery
verified)".

This is the **first graded** ghostty-web run of the
renderer-evaluation matrix on the production shell — the
2026-05-13 and 2026-05-14/14b/14c ghostty-web entries
either fell back to xterm (CSP/WASM blocked) or mounted
but deferred every matrix row for lack of a
renderer-fair input path. With the staging CSP relaxation
(2026-05-14c) and the renderer-fair input affordance
(2026-05-14d) both in place, the matrix could finally be
driven.

**What the matrix found, on the production shell, with
no source / CI / deploy / CSP changes:**

- ghostty-web mounted cleanly — `data-renderer="ghostty-web"`,
  `data-renderer-experimental="true"`,
  `data-renderer-fallback=""`, `data-renderer-gate="on"`,
  `data-renderer-input="marked"`, zero console errors
  during the session.
- Input was driven renderer-fairly through the
  `[data-relayterm-terminal-input]` marker + the
  `production-terminal-focus` button, with
  `document.activeElement` verified before every Path A /
  Path C row. The same selector resolved to xterm's
  helper textarea on the recovery row — one selector,
  correct element per renderer.
- **Core correctness** rows: basic I/O, long output
  (300-line burst), copy-paste (trusted Ctrl+V →
  production paste-safety pipeline →
  `bracketed_paste_markers` confirm panel → send), and
  detach / reconnect / replay (same session UUID,
  renderer + marker re-stamped, prior output replayed)
  all `pass`. Alternate-screen `works` (raw
  `\033[?1049h`/`l` — the target image lacks `tput`).
- **Text / typography** row: unicode / emoji / box
  drawing / wide CJK all render legibly (`works`;
  typography precision beyond "renders legibly" not
  measured).
- Resize / fit and narrow-viewport are `works with
  caveats` — ghostty-web does not expose an xterm-style
  `fit()` and does not reflow its grid on container
  resize (the workspace's `safeFit` probes for the
  capability and no-ops cleanly when it is absent). This
  is documented adapter behaviour, **not** a `regression
  vs. baseline`.
- Mouse is `deferred — fixture absent` (no
  click-coordinate fixture; harness plan defers the
  mouse-input half).
- xterm recovery verified end-to-end after the
  ghostty-web session (gate OFF → fresh launch →
  `data-renderer="xterm"` → commands round-trip). The
  six xterm `style-src` inline-style console errors are
  pre-existing (2026-05-14c), not a regression, and did
  NOT fire during the ghostty-web session.
- Redaction posture intact: 0 sentinel hits across DOM /
  `localStorage` / `sessionStorage` / `document.cookie`,
  backend / web / target logs, and `audit_events.payload`
  (3 public-metadata-only audit rows in the window).

**Promotion posture.** A single matrix run is one
human-evaluator data point, **not** a Gate-2 promotion.
**ghostty-web remains experimental and unpromoted; xterm
remains the production compatibility baseline and the
default renderer.** Gate 1 / Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged — Gate 1 still requires the Core-correctness
rows to hold on a target surface *with caveats
documented in `docs/spec/terminal-adapters.md`*, plus
the bundle-size sign-off and the SPEC updates; Gate 2
still requires per-surface coverage and a soak window.
The resize/fit and narrow-viewport caveats this
smoke surfaced (ghostty-web exposes no xterm-style
`fit()` and did not reflow its grid on container
resize) are now documented in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Production-shell evaluation status and resize/fit
caveat" — the Gate 1 requirement that a candidate's
Core-correctness caveats be recorded in the adapter
spec is met for this row. The resize-behavior
*decision* itself (implement renderer-neutral fit
support vs. accept the limitation for experimental
opt-in) remains future Gate 1 work, not this
docs-smoke slice.
Architectural posture unchanged: no backend protocol /
session / orchestrator / `terminal-core` /
production-shell-non-loader / CI / deploy-template / CSP
file was touched.

**Deferred from this slice:** restty / wterm matrix
smokes; desktop-Tauri / Android-Tauri renderer smokes;
automated performance / benchmark harness; the
production-side CSP decision; renderer production-default
flip (Gate 2); persistent per-user / per-device renderer
preference; `tmux` / `screen` and VT-snapshot
persistence; a purpose-built mouse click-coordinate
fixture and a larger-tooling target image for the
full-screen-app alternate-screen row.

### 2026-05-14f · restty production-shell renderer gate (mounts but non-functional under staging CSP; not promoted)

A docs-only smoke slice on 2026-05-14 carried the
**restty** experimental renderer through the
production-shell gate on the staging surface, to
decide whether restty could be matrix-evaluated like
ghostty-web (2026-05-14e). Full smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14f · restty production-shell renderer gate
smoke".

**What the gate found, on the production shell, with
no source / CI / deploy / CSP changes:**

- restty's **loader path is healthy** — the gated
  dynamic `import()` of `@relayterm/terminal-restty`
  resolved, the `ResttyRenderer` constructor ran,
  `mount()` resolved, the WASM compiled under the
  staging `'wasm-unsafe-eval'` CSP, and the backend
  session attached (`session_events`:
  created → attached → resized → closed). Diagnostics:
  `data-renderer="restty"`,
  `data-renderer-experimental="true"`,
  `data-renderer-fallback=""` (no `adapter_mount_failed`),
  `data-renderer-gate="on"`, `data-phase="attached"`.
- restty is nonetheless **visually / functionally
  non-functional** on the staging surface. The restty
  `<canvas>` stayed at **1 × 1 px** and `last_seen_seq`
  stayed `0` — nothing rendered. Three compounding
  causes: (1) restty applies **inline styles** for
  layout, blocked by `default-src 'self'` (the
  `style-src` fallback, no `'unsafe-inline'`) → canvas
  never sized; (2) restty's runtime text-shaper
  `fetch()`es a **font stack from `cdn.jsdelivr.net`**,
  blocked by the same directive (the `connect-src`
  fallback); (3) **WebGPU `No available adapters`** in
  the headless browser environment.
- This is a **distinct failure stage from
  ghostty-web's**: ghostty-web's `mount()` *rejected*
  (`adapter_mount_failed`). restty's `mount()`
  *resolves cleanly* — so the loader's closed fallback
  taxonomy cannot describe "mounted-but-non-functional"
  and the workspace shows **no operator-visible error
  panel**. Recorded as a taxonomy gap; no fix this
  slice.
- restty's adapter does **not** implement the optional
  `focusTarget()` method, so `data-renderer-input="none"`
  and the renderer-fair Path A / Path C input seam was
  unavailable. Combined with the 1 × 1 canvas, **no
  evaluation-matrix row was run or graded** — the
  slice stopped at the gate per "if it fails, document
  the blocker and stop."
- **xterm recovery passed** end-to-end (gate OFF →
  relaunch → `data-renderer="xterm"` →
  `relayterm-restty-gate-xterm-recovery` and `whoami`
  round-tripped). Redaction sweep clean across DOM /
  storage / cookies / backend-web-target logs /
  `audit_events` payloads — 0 sentinel/secret hits.

**Promotion posture.** **restty remains experimental
and unpromoted.** xterm remains the production
compatibility baseline and the default renderer.
Gate 1 / Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged — restty cannot clear Gate 1's Core-correctness
rows because it is not a usable renderer surface on
the evaluated staging CSP. The CSP blockers that
prevent Gate 1, and the `focusTarget()` precondition a
future restty production-shell smoke needs, are
documented in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Production-shell evaluation status and CSP caveat".
No backend protocol / session / orchestrator /
`terminal-core` / production-shell / renderer-adapter /
CI / deploy-template / CSP file was touched.

**Deferred from this slice:** the staging-CSP decision
that would let restty render at all (`style-src
'unsafe-inline'` for the inline-style block, plus a
`connect-src` allowance or a self-hosted font bundle
for the jsdelivr block — its own deliberate later
decision, not authorised here); a restty matrix smoke
once/if restty can render; wterm matrix smoke;
desktop-Tauri / Android-Tauri renderer smokes;
automated performance / benchmark harness; renderer
production-default flip (Gate 2); persistent per-user /
per-device renderer preference; `tmux` / `screen` and
VT-snapshot persistence.

### 2026-05-14g · wterm production-shell renderer gate (mounts cleanly AND renders functionally; matrix deferred on the `focusTarget()` gap; not promoted)

A docs-only smoke slice on 2026-05-14 carried the
**wterm** experimental renderer through the
production-shell gate on the staging surface — wterm
was the last experimental renderer not yet
production-shell gate-tested. Full smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14g · wterm production-shell renderer gate
smoke".

**What the gate found, on the production shell, with
no source / CI / deploy / CSP changes:**

- wterm's **loader and mount path is healthy** — the
  gated dynamic `import()` of `@relayterm/terminal-wterm`
  resolved (lazy chunk `assets/index-BKAYX4nB.js`,
  40,830 bytes, fetched HTTP 200 on attach), the
  `WtermRenderer` constructor ran, `mount()` resolved,
  `@wterm/core`'s inlined WASM compiled under the
  staging `'wasm-unsafe-eval'` CSP, and the backend
  session attached (`session_events`:
  created → attached → resized → closed). Diagnostics:
  `data-renderer="wterm"`,
  `data-renderer-experimental="true"`,
  `data-renderer-fallback=""` (no `adapter_mount_failed`),
  `data-renderer-gate="on"`, `data-phase="attached"`,
  **0 console errors** during the wterm mount.
- Unlike restty (2026-05-14f), wterm is **visually /
  functionally healthy** on the staging surface. wterm
  is DOM-rendered (no canvas / WebGPU, no runtime
  font-CDN `fetch`), so none of restty's three
  compounding CSP failures applied. The `.wterm` DOM
  host sized correctly to `642 × 434 px` (not restty's
  1 × 1 wedge), with 24 `.term-row` divs and the
  `.term-grid` present. A **diagnostic input probe**
  (not a graded renderer-fair row) confirmed the
  input → wire → output → render path works: clicking
  `production-terminal-focus` moved focus onto wterm's
  textarea-backed `InputHandler`, and trusted
  `browser_press_key` keystrokes (`who` + Enter)
  raised `last_seen_seq` `0 → 7` and rendered the
  command echo + shell prompt into wterm's DOM grid.
- wterm's adapter does **not** implement the optional
  `focusTarget()` method, so `data-renderer-input="none"`
  and the renderer-fair Path A / Path C input seam
  (`apps/web/e2e/SMOKE.md` § "Renderer-fair input") was
  unavailable. **No evaluation-matrix row was formally
  graded** — the slice stopped at the gate per "if it
  is blocked, document the blocker and stop." The gate
  question (does wterm load + mount cleanly, and is it
  functional) is answered **yes**; the formal matrix
  stays deferred on the `focusTarget()` gap, the same
  precondition restty (2026-05-14f) also lacks.
- **xterm recovery passed** end-to-end (gate OFF →
  relaunch → `data-renderer="xterm"`,
  `data-renderer-input="marked"`; renderer-fair focus
  verified via `[data-relayterm-terminal-input]`;
  `relayterm-wterm-gate-xterm-recovery` and `whoami`
  round-tripped). The 6 xterm `style-src` inline-style
  console errors are pre-existing (2026-05-14c/e/f),
  not a regression, and did **not** fire during the
  wterm session. Redaction sweep clean across DOM /
  storage / cookies / backend-web-target logs /
  `audit_events` payloads — 0 sentinel/secret hits.

**Promotion posture.** **wterm remains experimental
and unpromoted.** xterm remains the production
compatibility baseline and the default renderer.
Gate 1 / Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged — wterm's **Core-correctness** matrix rows
cannot be graded until a renderer-fair input path
exists for it, which needs `focusTarget()` implemented
in `WtermRenderer`. wterm clearing the gate (clean
mount + functional render) is one data point, **not**
a Gate 1 pass. The `focusTarget()` precondition is
documented in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Production-shell evaluation status and
`focusTarget()` caveat". No backend protocol /
session / orchestrator / `terminal-core` /
production-shell / renderer-adapter / CI /
deploy-template / CSP file was touched.

**Deferred from this slice:** a renderer-fair wterm
matrix smoke once `WtermRenderer` implements
`focusTarget()` (a code slice; the xterm and
ghostty-web adapters already meet it); desktop-Tauri /
Android-Tauri renderer smokes; automated performance /
benchmark harness; renderer production-default flip
(Gate 2); persistent per-user / per-device renderer
preference; the production-side CSP decision; `tmux` /
`screen` and VT-snapshot persistence.

### 2026-05-14h · `WtermRenderer.focusTarget()` implemented (unblocks the deferred wterm matrix smoke; still not a matrix run)

The `focusTarget()` code slice the 2026-05-14g gate
entry named as the precondition for grading wterm's
**Core-correctness** matrix landed in
`feat/wterm-renderer-focus-target`. `WtermRenderer` now
implements the optional `focusTarget(): HTMLElement |
null` — it returns wterm's hidden keyboard `<textarea>`
(the `InputHandler.textarea` element wterm appends to
the host and `WTerm.focus()` ultimately focuses), and
`null` before mount / after dispose / after a
dispose-vs-pending-`init()` race. With this, the
production workspace can stamp the renderer-neutral
`[data-relayterm-terminal-input]` marker on a wterm
mount (`data-renderer-input="marked"`) and the
renderer-fair Path A / Path C input seam is available
for wterm. This **unblocks** — but does **not perform**
— the deferred renderer-fair wterm matrix smoke; that
remains a separate, deliberate slice. wterm remains
experimental and unpromoted; xterm remains the
production compatibility baseline and the default
renderer. `ResttyRenderer` still lacks `focusTarget()`.
Scope: adapter package + its tests + docs only — no
backend protocol / session / orchestrator /
`terminal-core` / production-shell / CI / deploy /
CSP file was touched. Detail in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Production-shell evaluation status and
`focusTarget()` caveat".

**Deferred from this slice:** the renderer-fair wterm
production-shell matrix smoke; `ResttyRenderer.focusTarget()`;
desktop-Tauri / Android-Tauri renderer smokes;
automated performance / benchmark harness; renderer
production-default flip (Gate 2); persistent per-user /
per-device renderer preference; the production-side
CSP decision; `tmux` / `screen` and VT-snapshot
persistence.

### 2026-05-14i · wterm production-shell renderer matrix smoke (first graded wterm matrix; not a promotion)

The staging smoke slice the 2026-05-14g gate entry and
the 2026-05-14h `focusTarget()` entry both named as
deferred landed. The full smoke entry is in
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-14i · wterm production-shell renderer matrix
smoke".

This is the **first graded** wterm run of the
renderer-evaluation matrix on the production shell.
The 2026-05-14g wterm gate smoke proved wterm loads,
mounts, and renders functionally on the staging surface
but **deferred every matrix row** because
`WtermRenderer` had no `focusTarget()` — so
`data-renderer-input` was `"none"` and the
renderer-fair input seam was unavailable. The
2026-05-14h slice landed `WtermRenderer.focusTarget()`
in `bde039e feat(web): expose wterm focus target`. With
that, the production workspace stamps the
renderer-neutral `[data-relayterm-terminal-input]`
marker on a wterm mount, and this slice drove the
matrix.

**What the matrix found, on the production shell, with
no source / CI / deploy / CSP changes** (the staging
stack's web + backend were recreated from fresh
`:main` images that include `bde039e`; Postgres
untouched via `--no-deps`):

- wterm mounted cleanly — `data-renderer="wterm"`,
  `data-renderer-experimental="true"`,
  `data-renderer-fallback=""` (no `adapter_mount_failed`),
  `data-renderer-gate="on"`,
  `data-renderer-input="marked"` (the new state — the
  gate smoke had `"none"`), exactly one
  `[data-relayterm-terminal-input]` element (a
  `TEXTAREA`, wterm's `InputHandler.textarea`), the
  `.wterm` DOM grid sized correctly, **0 console
  errors** during the wterm session.
- Input was driven renderer-fairly through the
  `[data-relayterm-terminal-input]` marker + the
  `production-terminal-focus` button, with
  `document.activeElement` verified before every Path A
  / Path C row. The same selector resolved to xterm's
  `xterm-helper-textarea` on the recovery row — one
  selector, correct element per renderer.
- **Core correctness** rows: basic I/O, long output
  (300-line burst), and copy-paste (trusted Ctrl+V →
  wterm's DOM textarea `paste` handler → production
  paste-safety pipeline → `bracketed_paste_markers`
  confirm panel → send) all `pass`. Detach / reconnect /
  replay is `pass` **wire-side** (same session UUID,
  renderer + marker re-stamped, fresh input round-trips)
  — renderer-side visual scrollback parity is NOT
  claimed; see the Detach/reconnect bullet below.
  Alternate-screen `works` (raw `\033[?1049h`/`l` — the
  target image lacks `tput`): wterm switched to the alt
  buffer and restored the normal buffer correctly.
- **Text / typography** row: unicode / emoji / box
  drawing / wide CJK all render with correct codepoints
  in wterm's DOM grid (`works`). wterm renders each
  `.term-row` as a single text node, not per-cell
  spans, so codepoint correctness was confirmed but
  precise per-glyph cell-width was not measured;
  typography precision beyond "renders legibly" not
  measured.
- Resize / fit and narrow-viewport are `works with
  caveats` — wterm does not expose an xterm-style
  `fit()` and does not reflow its cell grid on
  container resize (the adapter defaults `autoResize`
  to `false`; the `.wterm` DOM host pixel-width tracks
  the container but the grid / PTY geometry does not
  reflow). This is documented adapter behaviour, **not**
  a `regression vs. baseline` — the same posture
  ghostty-web's matrix smoke recorded.
- Detach / reconnect: wterm remounted **fresh** on
  reattach — the DOM grid was empty until new output,
  matching the documented xterm-baseline behaviour
  ("renderer remounted; viewport empty until new
  output"). Wire-side replay is correct (same session
  row, still active); renderer-side scrollback parity
  is a separate property not claimed.
- Mouse is `deferred — fixture absent` (no
  click-coordinate fixture; harness plan defers the
  mouse-input half).
- xterm recovery verified end-to-end after the wterm
  session (gate OFF → fresh launch →
  `data-renderer="xterm"` → renderer-fair focus →
  commands round-trip). The 6 xterm `style-src`
  inline-style console errors are pre-existing
  (2026-05-14c/e/f/g), not a regression, and did NOT
  fire during the wterm session.
- Redaction posture intact: 0 sentinel hits across DOM
  / `localStorage` / `sessionStorage` /
  `document.cookie`, backend / web / target logs, and
  `audit_events.payload` (2 public-metadata-only audit
  rows in the window).

**Promotion posture.** A single matrix run is one
human-evaluator data point — **not** a Gate-1 pass and
**not** a Gate-2 promotion. **wterm remains experimental
and unpromoted; xterm remains the production
compatibility baseline and the default renderer.** Gate 1
/ Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged; wterm clearing the gate (2026-05-14g) plus
this one graded matrix run are evaluation data points,
not the deliberate Gate 1 promotion review. wterm is the
**second experimental renderer (after ghostty-web) to
complete a graded production-shell matrix smoke**, and
the only DOM-rendered one; its adapter caveats (no
xterm-style `fit()`, no cell-grid reflow on resize —
resize/fit is a separate evaluation-matrix row, not a
Core-correctness row) are recorded in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Production-shell evaluation status and
`focusTarget()` caveat" — the Gate 1 requirement that a
candidate's Core-correctness caveats be recorded in the
adapter spec is met for this row. Architectural posture
unchanged: no backend protocol / session / orchestrator
/ `terminal-core` / production-shell / renderer-adapter
/ CI / deploy-template / CSP file was touched.

**Deferred from this slice:** `ResttyRenderer.focusTarget()`
and a restty matrix smoke once/if restty can render;
desktop-Tauri / Android-Tauri renderer smokes;
automated performance / benchmark harness; the
production-side CSP decision; renderer
production-default flip (Gate 2); persistent per-user /
per-device renderer preference; `tmux` / `screen` and
VT-snapshot persistence; a purpose-built mouse
click-coordinate fixture and a larger-tooling target
image for the full-screen-app alternate-screen row.

### 2026-05-14j · wterm fit/reflow investigation (docs-only; root cause identified; renderer-neutral autofit deferred to its own slice)

The `feat/wterm-fit-reflow-investigation` slice the
2026-05-14i matrix entry and the renderer comparison
scorecard both named as the wterm product/mobile UX
lane's first step. It is an **investigation slice** —
a code-reading study of `@wterm/dom@0.2.1` (`WTerm`),
`WtermRenderer`, `rendererLoader.ts`, and
`settingsToRendererOptions`, not a smoke and not a
renderer change.

**Conclusion: the wterm non-reflow is a RelayTerm-side
abstraction gap, not a wterm/upstream limitation.**
wterm *can* reflow:

- `WTerm.resize(cols, rows)` (public; wired by the
  adapter) calls `bridge.resize()` **and**
  `renderer.setup()` — a genuine cell-grid + PTY-geometry
  reflow. Anything that recomputes `(cols, rows)` and
  calls `renderer.resize()` already reflows wterm.
- wterm ships a native fit-to-container path: with
  `autoResize: true` (wterm's own default), `init()`
  attaches a `ResizeObserver` that measures char size
  and calls `resize()` on every container change. With
  `autoResize: false` it runs `_lockHeight()` instead
  and never observes the container.

The production-shell non-reflow is the sum of three
RelayTerm-side facts: (1) `WtermRenderer` defaults
`autoResize` to `false` for cross-adapter parity; (2)
the `wtermOnly.autoResize` opt-in is **structurally
unreachable** from the production shell —
`settingsToRendererOptions()` returns
`Required<BaseTerminalRendererOptions>`, and `wtermOnly`
is on `WtermRendererOptions`, not the base type, so the
loader never forwards it; (3) the production "Fit"
affordance and `safeFit()` are xterm-`FitAddon`-shaped —
they duck-type for a synchronous `fit(): { cols, rows }`
that wterm has no public method to satisfy
(`_measureCharSize` / `_container` are private).

`safeFit()` *is* too xterm-shaped: a synchronous
one-shot `fit(): { cols, rows }` is the `FitAddon`
contract; wterm's honest fit model is observer-driven,
not a synchronous one-shot (the `ResizeObserver`
callback fires asynchronously — the measurement inside
it is synchronous, but there is no inline method that
returns post-fit dims). A single neutral `fit()` cannot
unify the two without leaking one renderer's model onto
the other.

**Outcome: docs-only (Outcome A).** No safe in-boundary
code change was found. Implementing a synchronous
`fit()` on the wterm adapter would require
reimplementing wterm's private char-measurement against
its private DOM — a fragile internals leak. Adding an
optional `fit?()` to `terminal-core` would formalize
xterm's shape without giving wterm a way to honor it.
Flipping the adapter `autoResize` default, or routing
`wtermOnly` through the production shell, are
behavior / production-surface changes outside an
investigation slice's boundary. The precise root cause,
the rejected options, and the recommendation are now
recorded in
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Resize / fit / reflow — investigation findings".

**Recommendation for the follow-on slice.** Design the
renderer-neutral resize/fit capability **observer-shaped**
("observe my container and self-fit, emitting
`onResize`") rather than as xterm's synchronous one-shot
`fit()`: xterm's `FitAddon` can be wrapped to that
shape, wterm's `autoResize` `ResizeObserver` already *is*
that shape, ghostty-web can no-op until it grows one.
That design — plus the product decision of whether the
production shell / Settings expose an "auto-fit" toggle —
is its own deliberate slice, with a follow-on staging
resmoke (`docs/wterm-fit-reflow-resmoke`) once behavior
actually changes.

**Design slice landed (2026-05-14, `docs/renderer-neutral-autofit-design`).**
That observer-shaped design is now recorded, docs-only, in
[`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md):
a mount-time renderer-neutral `autofit` option on
`BaseTerminalRendererOptions` plus an optional
`autofitActive()` query on `TerminalRenderer`, with a
local-only Settings toggle and a `data-renderer-autofit`
diagnostic. Implementation is the named follow-on slice
`feat/renderer-neutral-autofit`.

**Implementation slice landed (2026-05-15,
`feat/renderer-neutral-autofit`).** The capability now
exists in code. `BaseTerminalRendererOptions.autofit?:
boolean` and `TerminalRenderer.autofitActive?(): boolean`
ship on `@relayterm/terminal-core`; `XtermRenderer`
wires its own `ResizeObserver` + `FitAddon` behind
`autofit`; `WtermRenderer` maps `autofit` →
`WTerm.autoResize` (with `wtermOnly.autoResize`
precedence preserved); `GhosttyWebRenderer` /
`ResttyRenderer` accept-and-no-op the option, returning
`autofitActive()` as `false` honestly. `TerminalSettings`
gains `autofitEnabled` (default `false`, v1→v2 storage
migration), `SettingsView` exposes a "Fit terminal to its
container" checkbox with honest copy, and
`ProductionTerminal.svelte` mirrors the resolved state
onto `data-renderer-autofit ∈ {off, active, unsupported}`.
The production "Fit" button stays a best-effort one-shot
refit, but reads `autofitActive()` for its enablement /
copy via `computeFitButtonState`. The implementation
ships **off by default** so fresh users see zero
behaviour change; xterm's fixed-grid + manual-Fit
posture is untouched on the default path. **No** renderer
promotion, **no** xterm-default flip, **no** backend /
protocol / session / orchestrator change, **no** CSP /
CI / deploy change. The staging resmoke
(`docs/wterm-fit-reflow-resmoke`) — re-running the wterm
production-shell matrix with autofit enabled — is a
separate deliberate slice and has not yet run.

**Promotion posture unchanged.** wterm remains
experimental and unpromoted; xterm remains the
production compatibility baseline and the default
renderer. Gate 1 / Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged. This slice touched no backend protocol /
session / orchestrator / `terminal-core` /
production-shell / renderer-adapter / CI / deploy /
CSP file — it is docs-only.

**Deferred from this slice:** the renderer-neutral
autofit design + implementation; any production-shell /
Settings auto-fit toggle; the `docs/wterm-fit-reflow-resmoke`
staging resmoke (only meaningful once behavior changes);
renderer promotion; the xterm-default flip; restty
`focusTarget()` / restty matrix; desktop-Tauri /
Android-Tauri renderer smokes; the performance /
benchmark harness.

### 2026-05-15 · `docs/wterm-fit-reflow-resmoke` staging resmoke (real PTY reflow verified for wterm AND xterm under operator-opt-in autofit; `data-renderer-autofit` workspace diagnostic bug discovered)

**Status:** docs-only resmoke recording the runtime
behaviour of the now-landed
`feat(web): add renderer-neutral autofit` slice on
the production shell. **No** renderer / `terminal-core`
/ production-shell-non-doc / protocol / backend /
session / orchestrator / CSP / CI / deploy file was
edited.

**Surface.** `https://relayterm-staging.js-node.cc`,
Playwright MCP browser. Operator-approved web-only
recreate from fresh `:main` registry image
`sha256:7fc53fc7aba0…` (image config) /
`sha256:7197d33160d2…` (multi-arch index manifest).
Backend + Postgres untouched.

**What changed at runtime.** With the operator opting
in via Settings → "Fit terminal to its container"
(`autofitEnabled: true`), wterm's PTY now actually
reflows on a container resize:

- Initial `stty size` at 896-px container: `24 80`
  (constructor cols=80 hint).
- After narrowing the browser to 390 × 844 (the
  AppShell collapses to mobile and `.wterm` shrinks
  to 327 × 448 px): `stty size` reflowed to **`24
  35`**. The 2026-05-14i baseline (autofit not yet
  shipped) stayed at `24 80` for the same step.
- After restoring to 1440 × 900 (`.wterm` back to
  896 × 448): `stty size` settled at `24 103`
  (wterm's own `floor(width / charWidth)` measurement
  via the upstream `ResizeObserver` — wterm re-measures
  character width on each observer fire rather than
  restoring the constructor's `cols=80` seed, so the
  post-cycle column count is whatever wterm's
  current measurement reports, not the initial value).
- xterm with autofit on, exercised as the H control:
  `24 80` → `26 40` after the same narrow resize.
  xterm's adapter-owned `ResizeObserver` + `FitAddon`
  reflowed end-to-end.

The **resize / fit / reflow** Gate-1 caveat the 14j
investigation opened is therefore **substantively
closed for wterm** under the operator-opt-in autofit
path — the underlying capability does what the
design said it would.

**Workspace diagnostic bug (newly discovered).** The
`data-renderer-autofit` attribute stayed at
`"unsupported"` for the entire matrix run on BOTH
wterm and xterm, even with autofit enabled and the
underlying renderer reflow working. The Fit-button
autofit-active tooltip ("Autofit is keeping the
terminal sized to its container.") never appeared.
Cause traced to
`apps/web/src/lib/app/terminal/ProductionTerminal.svelte`:
`let renderer: TerminalRenderer | null = null;` is a
plain `let`, not `$state`, so the
`autofitStatus = $derived(computeRendererAutofitStatus({
autofitEnabled, renderer }))` derivation does not
re-run when `renderer = r` is later assigned. The
derivation runs once during attach with `renderer =
null` (because `autofitEnabled = true` is set
synchronously *before* `renderer = r`) and then
stays at `"unsupported"`. This affects ONLY the
workspace diagnostic surface — the actual autofit
capability works fine. The follow-on bug-fix slice
is named **`fix(web): make renderer reactive for
data-renderer-autofit`**: make `renderer` a `$state`
(or mirror it to a `$state` shadow), extend
`apps/web/tests/` to pin the
`data-renderer-autofit="active"` post-mount
transition for both wterm and xterm under
`autofitEnabled = true`. SMOKE.md's autofit
precondition (`data-renderer-autofit="active"`)
remains structurally unverifiable from the
production shell until that ships — runbook step
ordering is correct, the assertion just cannot be
made truthfully today.

**Promotion posture unchanged.** wterm remains
experimental and unpromoted; xterm remains the
production compatibility baseline and the production
default renderer. Gate 1 / Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged. The renderer-neutral autofit
implementation does what the design said it would
and removes the resize/fit reason a Gate 1 reviewer
might cite for wterm, but neither this resmoke nor
the autofit slice itself is a Gate 1 review or a
promotion mechanism.

**Cleanup posture.** Throwaway SSH target, host
record, server profile, and SSH identity created by
this slice are still in place — cleanup is deferred
pending operator approval (see the smoke entry's
Cleanup section for the full resource list and the
exact cleanup commands).

**Cross-links.** Smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-15 · wterm production-shell renderer-neutral
autofit resmoke". Design:
[`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md).
Implementation: `a2c806b feat(web): add renderer-neutral
autofit`. Adapter contracts:
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Implementation status (since 2026-05-15…)". Scorecard:
[`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
§ "Resize / fit status".

### 2026-05-15b · `docs/wterm-autofit-diagnostic-resmoke` staging resmoke (`data-renderer-autofit="active"` workspace fix verified for wterm AND xterm; SMOKE.md autofit precondition is now truthful from the production shell)

**Status:** docs-only resmoke verifying the
production-shell behaviour of
`79c216b fix(web): update autofit diagnostic after
renderer mount`. **No** renderer / `terminal-core` /
production-shell-non-doc / protocol / backend /
session / orchestrator / CSP / CI / deploy file was
edited.

**Surface.** `https://relayterm-staging.js-node.cc`,
Playwright MCP browser. Operator-approved web-only
recreate from fresh `:main` registry image
`sha256:cb9620986ddf…` (image config) created
`2026-05-15T23:00:42Z` — built **after** the
2026-05-15 (`a`) entry's `sha256:7fc53fc7aba0…`
image, carrying the fix commit. Backend + Postgres
untouched (the diagnostic fix is web-only).

**What changed at runtime.** With the operator opted
in via Settings (`autofitEnabled: true`,
`experimentalRendererEvaluationEnabled: true`,
`rendererId: wterm`), the production-terminal
workspace now reports `data-renderer-autofit="active"`
on first paint after the renderer mounts (the
2026-05-15a state was `"unsupported"`); the
Fit-button autofit-active tooltip ("Autofit is
keeping the terminal sized to its container.")
finally renders, and the attribute stays `"active"`
through a narrow-viewport resize cycle. The xterm
control row (autofit on, gate off,
`rendererId: xterm`) reports the same `"active"`
and the same tooltip. The
[2026-05-15a entry](#2026-05-15--docswterm-fit-reflow-resmoke-staging-resmoke-real-pty-reflow-verified-for-wterm-and-xterm-under-operator-opt-in-autofit-data-renderer-autofit-workspace-diagnostic-bug-discovered)'s
"the `autofit="active"` precondition is structurally
unverifiable from the production shell until that
ships" caveat is **closed** for both renderers
under the production-shell autofit path.

**Scope is intentionally narrow** — this is the
workspace-diagnostic resmoke; it is NOT a renderer
promotion, NOT a renderer-default change, NOT a
renderer-evaluation matrix re-run, and NOT a re-do
of the 2026-05-15a reflow verification (which
already pinned the underlying wterm + xterm autofit
reflow behaviour end-to-end).

**Promotion posture unchanged.** wterm remains
experimental and unpromoted; xterm remains the
production compatibility baseline and the production
default renderer. Gate 1 / Gate 2 criteria under
[§ "Promotion criteria"](#promotion-criteria) are
unchanged.

**Cleanup posture.** Throwaway SSH target
`relayterm-staging-wterm-autofit-diagnostic-smoke-ssh`
and the new server profile
`wterm-autofit-diagnostic-smoke-profile` are still
in place — cleanup is deferred pending operator
approval (see the smoke entry's Cleanup section for
the full resource list and the exact cleanup
commands). Settings were reset to fresh-user
defaults in the smoke browser at slice end.

**Cross-links.** Smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-15b · wterm autofit diagnostic resmoke".
Bug origin: the 2026-05-15a entry above. Fix
commit: `79c216b fix(web): update autofit diagnostic
after renderer mount`. Design unchanged:
[`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md).
Adapter contracts unchanged:
[`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Implementation status (since 2026-05-15…)".

### 2026-05-15c · `docs/wterm-android-browser-smoke` (surface 2) — first Android Chrome execution; mount + rotation pass, WS-attach detach-at-seq-0 is the headline open question

**Status:** docs-only smoke execution against the deployed
staging stack. **No** renderer / `terminal-core` /
production-shell-non-doc / protocol / backend / session /
orchestrator / CSP / CI / deploy file was edited. The slice's
sole new artefacts are the dated entry in
`docs/deployment/vps-staging-smoke.md` (§ "2026-05-15c"), the
per-row status block in `docs/wterm-mobile-smoke-plan.md` § 5,
the "Known concerns" update in
`docs/renderer-comparison-scorecard.md` § "wterm", this entry,
and a single AGENTS.md Encountered-Lessons line.

**Surface.** `https://relayterm-staging.js-node.cc` against a
physical Samsung Android phone (adb-visible `R38N500TY3E`),
Chrome `148.0.0.0` on Android 10 (Chrome's reduced UA strings
"K"), driven via the workstation's adb. Web container =
`sha256:cb9620986ddf…` (the 2026-05-15b digest);
backend = `sha256:90573e96bcbc…` (unchanged from 2026-05-14g
onward). Bundle `index-9Ss46Hol.js`. CSP unchanged:
`default-src 'self'; script-src 'self' 'wasm-unsafe-eval'`.

**Renderer setting.** Settings → Renderer evaluation gate ON +
renderer = `wterm`, carried via auto-login cookie from a
prior workstation session.

**What landed.**

- Row 1 (renderer mount): **PASS** — visible block cursor in
  the `production-terminal` viewport on both Launch attempts.
- Rows 3 / 4 (tap-to-focus + soft keyboard): partial pass —
  `production-terminal-focus` button worked, samsung IME rose
  on focus, wterm cursor stayed visible above the IME.
- Row 11 (rotation): partial pass — nav rail + control row
  reflowed cleanly in landscape. wterm grid did not re-fit on
  rotation (autofit is mount-time only for the current
  `@wterm/dom` 0.2.x adapter; consistent with the
  2026-05-14j / 2026-05-15 scorecard row, not a regression).
- Row 13 (workspace nav usability): partial pass — every
  control reachable on a 1080-wide screen, though button
  spacing is tight for thumb input.
- Row 16 (redaction): **PASS** — sentinel-clean grep across
  backend + nginx + SSH-target logs for the smoke window.

**What did NOT land — the headline open question.** Both
fresh sessions (`45e2f261-c96c-45d2-8301-06b63d105b65`,
`033c48ac-3838-4214-8fe6-5e5ee5cbf768`) immediately reached
`Status: detached (TTL window)` with `last_seen_seq 0`.
Backend nginx access log confirms POST `/api/v1/terminal-sessions`
→ 201, then GET `/ws` → 101 for both — but with a **consistent
~60-second gap between POST and WS dial** for both sessions.
SSH-target container log shows **zero inbound connections** for
the smoke window, so russh on the backend never even dialed
the throwaway SSH target for either session, even though the
preflight + trust-host-key + auth-check routes (which DO dial
russh) had all returned 200 for the same profile +
identity less than 90s before the first Launch. Reconnect
within the 30-second TTL window did not flip the workspace
state to live. Without a live PTY, the renderer-fair input
rows (6 ASCII input, 7 modifier-key affordances, 8 paste, 9
copy/select, 10 long output) could not be exercised and are
all carried into the next slice as `deferred — blocked by
Row 12`. Per-row status table is in
`docs/wterm-mobile-smoke-plan.md` § 5.

**Why this is NOT a renderer judgement.** The renderer
mounts; the renderer's grid is visible; the renderer's
rotation behaviour is acceptable. The detach-at-seq-0 pattern
is a *workspace* observation that this slice cannot
distinguish from a *renderer* observation without an xterm
control on the same phone / network / staging stack. The
next slice (`docs/wterm-android-browser-resmoke`) runs Row
17 (xterm control comparison) **first** specifically to make
that distinction; if xterm reproduces the same detach
pattern, the bug is workspace-side and wterm is exonerated;
if xterm attaches cleanly, the bug is wterm-specific and
the next fix slice should look at wterm's mount-completion
→ WS-dial ordering on touch devices.

**Posture.** Do NOT promote wterm. Do NOT flip the xterm
production baseline. wterm's experimental status is
unchanged. The 2026-05-14g production-shell mount gate and
the 2026-05-14i matrix smoke remain the last graded data
points for wterm on the desktop surface; this entry is the
first surface-2 data point and is honestly mixed.

**Cross-links.** Smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-15c · wterm Android Chrome (surface 2) browser
smoke". Plan: [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
§ 5 (per-row status table) and § 11 (next slice proposal).
Scorecard updates:
[`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
§ "wterm" Known concerns, § "Mobile / browser-native UX
potential" footnote.

### 2026-05-16 · `docs/wterm-android-browser-resmoke` (surface 2, xterm control) — Row 12 reclassified as workspace-bound + transient; wterm exonerated as the cause of the 2026-05-15c finding

**Status:** docs-only diagnostic resmoke against the deployed
staging stack with the renderer flipped to the **xterm**
production baseline. **No** renderer / `terminal-core` /
production-shell-non-doc / protocol / backend / session /
orchestrator / CSP / CI / deploy file was edited. Per the
slice's Phase 1 → Phase 2 decision tree, **wterm was not
re-tested** — the xterm result was structurally sufficient
to resolve the 2026-05-15c open question.

**Surface.** Same Samsung phone (`R38N500TY3E`), Android
Chrome `148.0.0.0`, same home wifi, same staging stack as
2026-05-15c. Web + backend container digests unchanged
(`sha256:cb9620986ddf…` / `sha256:90573e96bcbc…`). CSP
unchanged (`default-src 'self'; script-src 'self'
'wasm-unsafe-eval'`).

**Renderer setting.** Settings → renderer evaluation gate
**OFF**, renderer **xterm**. Gate never flipped on during
this slice.

**What landed.** Three xterm Launch attempts against the same
hermetic throwaway target on the same network:

- **Launch 1** (`a469711b`) — POST `/terminal-sessions` 201
  at 14:30:42, `GET …/ws` 101 at 14:31:50 — **68-second
  POST→WS gap**. The `session_events.attached` row fired at
  14:30:43 (orchestrator pre-mark), then `session_events.detached`
  fired at 14:31:50 with `last_seen_seq: null` (the WS upgrade
  arrived past the server-side attach-timeout window;
  immediate detach on arrival). The session auto-closed at
  14:32:20 (`reason: client_requested`). **Reproduces the
  2026-05-15c pattern with the xterm production baseline
  renderer.**
- **Launch 2** (`494fd0f5`) — POST → 201 at 14:33:39, attach
  event at 14:33:40, **`netstat -tn` inside the throwaway
  showed an ESTABLISHED `172.21.0.3:60646 → 172.21.0.5:2222`
  connection** with a live `sshd-session.pam: smoke@pts/0`
  process. Operator typed `echo` and `whoami`; both
  round-tripped (`whoami → smoke` confirms the throwaway
  user is the live PTY's identity).
- **Launch 3** (`7cbbb2d8`) — POST → 201 at 14:37:57. Inside
  the throwaway, the per-3-second netstat poll showed
  `established_to_2222=0` at 14:37:46 – 14:37:56, then
  **`=1` from 14:37:59 (≈2 s after POST) sustained through
  the full 90 s capture window**. Operator typed the slice
  sentinel `echo relayterm-android-xterm-resmoke`.

**Methodology correction for 2026-05-15c.** The
linuxserver/openssh-server throwaway image writes only its
init / boot lines to docker stdout — runtime sshd
connection activity goes to syslog inside the container,
not visible via `docker logs`. The accurate "is the SSH
PTY actually live" probe is `netstat -tn | grep :2222`
inside the throwaway, or `ps -ef | grep sshd-session`. The
2026-05-15c read of "russh never dialed" was based on the
incorrect probe; with the corrected probe, the 2026-05-15c
detach pattern most plausibly maps to "WS upgrade arrived
past the server-side attach-timeout window, immediate
detach on arrival" — *not* "russh never dialed". This
slice does not edit the 2026-05-15c entry in place; the
2026-05-16 dated entry in the staging-smoke log carries
the interpretation correction.

**Why this is a renderer-NEUTRAL finding.** The xterm
production baseline reproduced the 2026-05-15c detach
pattern on its first launch. wterm is therefore **not
implicated** as the cause — the bug is on the workspace /
mobile-Chrome / orchestrator attach-timeout side, and
solving it for xterm solves it for every renderer.

**Posture.** Do NOT promote wterm. Do NOT flip the xterm
production baseline. xterm's 2026-05-13 baseline smoke
remains the last graded data point on the desktop surface;
this 2026-05-16 resmoke is xterm's first surface-2 (Android
Chrome) data point and is "**works with intermittent
first-launch detach pattern shared with every renderer**".

**Cross-links.** Smoke entry:
[`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
§ "2026-05-16 · `docs/wterm-android-browser-resmoke`
(surface 2, xterm control)". Plan:
[`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
§ 5 ("Status after the 2026-05-16 xterm-control resmoke")
and § 11 ("Update after the 2026-05-16 xterm-control
resmoke"). Scorecard update:
[`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
§ "wterm" Known concerns (the 2026-05-15c mobile detach
finding is now reclassified as workspace-bound).

**2026-05-16 · `docs/mobile-smoke-methodology-update`
(methodology follow-up, docs-only).** A separate docs slice
has refactored the mobile smoke execution model in
[`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
§ 5 ("2026-05-16 methodology update — Playwright-first
execution model, real-phone narrow scope") and added the
operator-runbook surface in
[`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § D →
"Mobile smoke methodology (Playwright-first; real-phone
narrow)". Every future surface-2 / surface-3 row sweep — under
the workspace-side investigation slice above or after it lands
— defaults to Playwright mobile emulation + server-side log /
DB inspection, with real-phone operator work reserved for the
closed list of rows whose evidence depends on hardware (soft
keyboard, selection handles, OS paste menu, Android back
gesture, touch ergonomics, real orientation event chain,
tab / session lifecycle). Operator prompts are short and
row-based; the SSH-inbound probe is `netstat -tn` inside the
throwaway target (not `docker logs`); every evidence row is
tagged with an evidence-class label. The methodology change
does NOT promote any renderer, does NOT flip the xterm
default, and does NOT alter the Phase 1 / Phase 2 promotion
gates.

## Purpose

Decide which terminal renderer RelayTerm should ship in production —
and on which device class — by exercising the four adapters that
already exist behind the renderer-neutral seam (`xterm`, `ghostty-web`,
`restty`, `wterm`) and a future native/Tauri candidate against the
same evaluation matrix. The output of this work is a recommendation
(which renderer becomes the default on desktop browser, on Tauri
desktop, and on Android), not a finished renderer-swap UX.

## Product positioning

- RelayTerm is a **secure SSH session and control-plane app** with
  renderer experimentation. The renderer is one swappable layer; the
  load-bearing differentiators (backend-owned SSH sessions,
  sequence-numbered replay, paste-safety policy, audit/redaction
  posture, host-key + private-key trust model) are independent of the
  renderer choice.
- RelayTerm **is not trying to clone tmux.** The product is "a web /
  Tauri client for SSH sessions whose state lives on the backend and
  survives client disconnects," not "tmux in a browser." Multi-pane
  workspaces, window layouts, copy-mode, and the rest of the tmux
  feature surface are explicitly out of scope for this track.
- **tmux / screen are a future optional integration**, not a current
  primitive. The current backend primitive is the in-memory replay
  ring (default `max_frames = 1024`, `max_bytes = 1 MiB`) plus the
  bounded `DETACHED_LIVE_PTY_TTL` reconnect window — see
  [`docs/spec/terminal.md`](spec/terminal.md) § "Output sequence +
  in-memory replay buffer contract" and § "Detached-session TTL
  contract." A host-side multiplexer (Option C) is one of several
  staged options in [`docs/persistent-sessions.md`](persistent-sessions.md)
  and is **deferred**. Nothing in the renderer track depends on it
  and nothing in the renderer track unlocks it.

## Current baseline

- `@relayterm/terminal-xterm` (xterm.js v5) is the **production
  compatibility baseline and the default renderer**. The production
  terminal workspace uses `@relayterm/terminal-core` +
  `@relayterm/terminal-xterm` only. The isolation rule is pinned by
  `apps/web/tests/appShellIsolation.test.ts`.
- `@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`,
  and `@relayterm/terminal-wterm` are **experimental and dev-only**.
  Each adapter is wired into the dev-only live terminal lab
  (`apps/web/src/lib/dev/XtermLiveTerminalLab.svelte`); production
  bundles tree-shake all three out. None of them is promoted into
  any production view as part of this track without passing the
  promotion gates below.
- The renderer-neutral seam is the contract these candidates sit
  behind: `TerminalRenderer` from `@relayterm/terminal-core`,
  consumed by `TerminalSessionClient`. The wire protocol (control
  plane JSON + `RTB1` binary `Output`/`Input` envelope) does not
  change for any renderer experiment.

## Evaluation candidates

| Candidate | Package | Today's status |
|---|---|---|
| xterm.js baseline | `@relayterm/terminal-xterm` | Production default. Compatibility baseline. |
| ghostty-web | `@relayterm/terminal-ghostty-web` | Experimental (dev lab only). libghostty-vt via WASM; xterm-API-compatible. |
| restty | `@relayterm/terminal-restty` | Experimental (dev lab only). libghostty-vt + WebGPU/WebGL2 + text shaping; ~3 MB JS + inlined WASM. |
| wterm | `@relayterm/terminal-wterm` | Experimental (dev lab only). DOM-rendered cell grid + Zig/WASM core; mobile / accessibility / native-text-input oriented. |
| Future native / Tauri | not started | Hypothetical native renderer embedded inside the Tauri shells. Not in this slice; listed so the matrix has a place for it when the time comes. |

## Non-negotiable architecture rules

Every evaluation step MUST respect these. If a renderer requires
breaking one of them to look good, that renderer fails the
evaluation, not the architecture.

1. **Backend protocol stays RelayTerm-shaped.** Wire-stable JSON
   control plane and `RTB1` binary `Output`/`Input` envelope. A
   renderer that needs new data transforms what is already on the
   wire — the protocol does not reshape itself for any renderer.
2. **`terminal-core` stays renderer-neutral.** No imports of
   `@xterm/*`, `ghostty-web`, `restty`, or `@wterm/*` are allowed
   inside `terminal-core`. The neutral shapes (`TerminalRenderer`,
   `BaseTerminalRendererOptions`, `RendererTheme`,
   `RendererThemeAnsi`, `RendererCursorStyle`) are the only thing
   adapters extend.
3. **Renderer-specific APIs stay behind adapter escape hatches.**
   Each adapter exposes a local `<renderer>Only` options slot
   (`xtermOnly`, `ghosttyOnly`, `resttyOnly`, `wtermOnly`) that is
   explicitly **non-portable** across adapters and never leaks into
   `TerminalRenderer` or `BaseTerminalRendererOptions`.
4. **No raw terminal input / output logging.** Keystrokes, PTY
   output bytes, and decoded terminal strings never appear in
   `console.*`, log lines, thrown `Error` messages, `audit_events`,
   `data-*` attributes, `localStorage`, `sessionStorage`, the
   recording chunk wire shape (chunk bytes cross only via
   `data_b64`), or any DOM string outside the terminal viewport
   itself. The sentinel-string redaction tests pinned for each
   adapter (`packages/terminal-*/tests/*Renderer.test.ts`)
   are the executable backstop and remain green for any new
   candidate.
5. **No renderer owns reconnect state.** Sequence numbers, the
   in-memory replay ring, the detach window, and resume-from-`seq`
   handshake are all orchestrator-owned. A renderer is free to
   maintain a transient local scroll position or selection, but
   nothing about correctness across `client_dropped → reconnect →
   resume_at_sequence_n` is allowed to live in the renderer.
6. **Production shell isolation.** `apps/web/src/lib/app/**` cannot
   import from `lib/dev/` or any experimental renderer adapter
   package. The dev lab is the only consumer of experimental
   renderers until promotion (see "Promotion criteria" below).

## Evaluation matrix

The matrix below is the **set of dimensions** a candidate is graded
on. It is intentionally not a benchmark harness — the harness is
deferred (see "Explicitly deferred" below). Each row is something a
human evaluator can answer with a short verdict (`works` / `works
with caveats` / `regression vs. baseline` / `blocker`) plus
free-form notes.

### Core correctness

| Dimension | What to verify |
|---|---|
| Basic input/output | Round-trip a typed line through `Input` → PTY → `Output` → renderer. No garbled bytes, no lost keystrokes, no echoed plaintext outside the viewport. |
| Resize / fit / reflow | Container resize fires `onResize(cols, rows)`, the wire `resize` frame flows, the PTY honours it, and the renderer reflows existing scrollback without corruption. |
| Detach / reconnect / replay | Tear down the client, reconnect with `(session_id, last_seen_seq)`, observe the `replay_start → output … → replay_end` handshake, confirm the renderer state is consistent with what the orchestrator replayed. `replay_window_lost` surfaces as a clean error, not a crash. |
| Long output / backpressure | Stream a `yes`-style firehose for a fixed window. Renderer must accept all bytes the orchestrator delivers; no internal queue stalls, no torn frames, no missing tail. Watch for `Terminal.write` callbacks if the adapter exposes one (xterm does). |
| Alternate screen / full-screen apps | `htop`, `vim`, `less`, `ssh -t … bash` — the alternate-screen switch and restore round-trips cleanly; cursor returns to the right cell on exit. |
| Mouse support | Where the renderer claims SGR mouse mode, click + drag + wheel translate into wire-correct input. Where it does not, the failure is documented, not silently wrong. |

### Text / typography

| Dimension | What to verify |
|---|---|
| Unicode, emoji, box drawing, wide chars | CJK + emoji + `┌─┐` table characters render at the right cell width. Wide chars do not desync the column count. ZWJ sequences render as one glyph or are documented as not supported. |
| Theme / settings parity | The neutral knobs (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`) apply on the candidate, OR are documented as silently dropped (with the alternative — e.g. CSS custom properties on the `.wterm` host — captured in the adapter docs). |
| Scrollback behavior | `scrollbackLines` honoured (or the renderer's analogue documented). Scrollback survives renderer-internal redraws; it is wiped on `dispose` (correct — replay restores from the orchestrator). |

### Platform fit

| Dimension | What to verify |
|---|---|
| Copy / paste (clipboard, paste-safety) | Browser clipboard read/write integrates with the renderer's selection model. `evaluatePaste` policy still gates large / multiline / control-char-heavy pastes — paste handling lives above the renderer; this row confirms the renderer does not bypass it. |
| Mobile keyboard / soft IME | On Android (Tauri + browser), virtual keyboard typing, autocorrect, swipe-to-type, CJK IME, and emoji picker reach the wire as the expected bytes. The `wterm` DOM-rendered grid is specifically motivated by this row; the canvas renderers (`xterm`, `ghostty-web`, `restty`) may have rougher mobile behavior — document the gap honestly. |
| Accessibility | Screen-reader-friendly output (the DOM-rendered `wterm` adapter is the candidate here); focus ring, keyboard-only navigation, contrast under the curated themes. Document gaps; do not block on this row in the first pass. |
| Bundle size / tree-shaking | Confirm the candidate stays tree-shaken out of the production `apps/web` bundle while `import.meta.env.DEV` gates the dev lab. Caveats from `docs/spec/terminal-adapters.md` (none of the upstreams currently declare `sideEffects: false` in their own `package.json`) still hold. |
| Memory / CPU rough observations | Human-readable notes only ("scrolling 50 MiB feels smooth on this laptop"). Not a microbenchmark. |

### Safety / redaction

| Dimension | What to verify |
|---|---|
| Redaction / log safety | Re-run each adapter's redaction test (`packages/terminal-*/tests/*Renderer.test.ts`). Add a manual sweep: trigger an Error inside `onInput`, write a sentinel string, dump the rendered HTML — the sentinel appears only inside the terminal viewport, nowhere in `console`, `localStorage`, `sessionStorage`, the lab event log, or any `data-*` attribute. |
| Renderer swap during attach | Switching renderers tears down the previous adapter and `TerminalSessionClient`, reconnects via the same `(session_id, last_seen_seq)` path, and replays. No payload bytes appear in the lab event log on switch (only the renderer name). |

## Promotion criteria

A candidate moves through three states: **experimental (dev-lab
only)** → **production opt-in** → **production default**. Each
state has a hard gate.

### Gate 1 — experimental → production opt-in

The candidate must pass before it is allowed to render in any
production-shell view (even behind a user-facing "experimental"
toggle). All of:

1. Every row of the **Core correctness** group is `works` or
   `works with caveats` on at least one of the three target
   surfaces (desktop browser, desktop Tauri, Android Tauri /
   Android browser), and the caveats are documented in
   `docs/spec/terminal-adapters.md`.
2. The four non-negotiable architecture rules above hold. The
   adapter's redaction tests are green; the `terminal-core`
   neutrality rule is unchanged; `appShellIsolation.test.ts`
   passes after the production-shell wiring.
3. A staged dev / production smoke against the throwaway staging
   stack exercises the candidate end-to-end at least once
   (login → inventory → launch → live PTY → detach → reconnect →
   replay → close), with `data-testid="renderer-option-<id>"`
   selectors documented in `apps/web/e2e/SMOKE.md`.
4. Bundle-size impact is captured and signed off. If the
   candidate carries an inlined WASM payload or large JS, the
   production-bundle size diff is recorded; if the increase is
   significant, the production opt-in landing path puts the
   adapter behind a dynamic import so the default-renderer bundle
   does not regress.
5. SPEC.md and `docs/spec/terminal-adapters.md` updated to
   describe the candidate's production opt-in surface (which
   views, which device class, what the user-facing label says,
   what fallback fires if the renderer fails to load).

### Gate 2 — production opt-in → production default

The bar to replace xterm as the default. All of:

1. Gate 1 holds on every target surface, not just one.
2. **Text / typography** and **Platform fit** rows are at least
   `works with caveats` on every target surface. Any `regression
   vs. baseline` row is a blocker.
3. The candidate has been the production opt-in choice for a
   stated soak period (at least two release cycles) without
   redaction or correctness regressions captured in
   `docs/agent/encountered-lessons.md` or the staging smoke log.
4. Default-flip lands as its own deliberate slice with: a
   migration plan for users who pinned xterm, an update to
   [`docs/agent/task-patterns.md`](agent/task-patterns.md) §
   "Recurring rules for renderer work" (the rule "`xterm` is the
   compatibility baseline and the default" is what changes), and an
   updated SPEC.md statement.

A candidate may stay at Gate 1 forever — production opt-in is a
fine end state for a renderer that targets a specific device class
(e.g. `wterm` for mobile / accessibility while `xterm` stays
default on desktop). The default flip is the deliberate harder
decision.

## Smoke plan

Each gate above is verified by a manual smoke. The smoke is
**deliberately manual** — committing a Playwright runner for the
renderer surface is its own slice (see "Explicitly deferred"). The
shape is the same shape used by every other staging smoke entry in
`docs/deployment/vps-staging-smoke.md`. The renderer-fairness
strategy for the rows the 2026-05-13 baseline could not exercise
(Unicode / box drawing / wide chars, copy / paste,
alternate-screen, mouse) is closed in
[`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) —
each per-candidate smoke shape below inherits its input-path
taxonomy and command matrix. The operator / Claude runbook for the
matrix lives in
[`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § "D. Renderer
evaluation smoke".

### Surfaces

> **Status (2026-05-15).** The desktop-browser surface is fully
> live: every graded renderer matrix smoke since 2026-05-14 has run
> against Firefox 1440 × 900 on staging. The Tauri and Android
> surfaces are scaffolded but have not yet hosted a renderer-evaluation
> matrix pass. The wterm-on-Android plan — which scopes how to
> exercise the matrix on surface 3 (Android Tauri WebView) and
> introduces an Android Chrome surface that prefaces it — lives in
> [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md);
> the named next slice is the Android-Chrome smoke
> (`docs/wterm-android-browser-smoke`), with the Android Tauri
> WebView smoke (`docs/wterm-android-tauri-smoke`) as the immediate
> follow-on.

1. **Desktop browser via Playwright MCP.** Firefox at 1440 × 900
   against the throwaway staging stack
   (`https://relayterm-staging.js-node.cc`). Selector hooks:
   `data-testid="renderer-option-<id>"` (already wired in the dev
   lab; production opt-in landing slice adds the matching
   production-shell hook). The procedure mirrors
   `apps/web/e2e/SMOKE.md` § "Dev renderer + production shell
   smoke."
2. **Desktop Tauri.** The path-A bundled-shell handoff (see
   `docs/spec/tauri-runtime-backend-url.md`) against the same
   staging stack. The WebView is WebKitGTK on Linux, WebView2 on
   Windows. Repro the Gate 1 / Gate 2 matrix once per OS the
   shell ships on. Cache pitfall (WebKitGTK + nginx immutable
   `/assets/*`) is documented in
   `docs/deployment/tauri-local-build.md` Troubleshooting and
   in AGENTS.md "Encountered Lessons" (2026-05-09).
3. **Android WebView.** Tauri Android shell (debug APK via
   `pnpm --filter @relayterm/mobile exec tauri android build
   --debug --apk --ci`) against staging. Pay particular attention
   to the **Platform fit** row "Mobile keyboard / soft IME" —
   this is the row `wterm` is motivated by. The
   [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
   plan also covers Android **Chrome** as a separate surface
   (cheapest *real-mobile* surface, no APK build required) ahead
   of the WebView smoke; it isolates "is wterm viable on mobile?"
   from "does Android System WebView differ from Chrome in a way
   that affects wterm?"
4. **Throwaway staging SSH target.** Pattern is fixed:
   `linuxserver/openssh-server:latest` named
   `relayterm-staging-<smoke-id>-ssh`, attached only to the
   internal Compose network, no host port published. Public-key
   pasted in via the same redaction-safe pattern the deployable-
   baseline smoke used (base64 sidecar + `atob` inside a single
   `page.evaluate`; PEM bytes never appear in any tool-call
   payload, log, Error, or DOM string). The container is
   `docker stop && docker rm`'d at teardown.

### Per-candidate smoke shape

Each candidate gets one staging entry per gate, in
`docs/deployment/vps-staging-smoke.md`, following the existing
template:

- Date + UTC time window, stack pin (web + backend image digests),
  branch, browser surface.
- Goal (gate being verified) and explicit slice boundary (no
  source / schema / API / auth / CSRF / deploy changes — same
  posture as the deployable-baseline smoke).
- Throwaway SSH target identity + DNS alias + redaction posture.
- Identity path (generated or imported), redaction sweep over
  `document.documentElement.outerHTML` for
  `BEGIN OPENSSH PRIVATE KEY`, `openssh-key-v1` magic prefix,
  `encrypted_private_key`, `session_token`, `token_hash`,
  `data_b64`, `REDACT-MARKER`.
- Renderer-switch sequence: walk the renderer radio group, click
  each `data-testid="renderer-option-<id>"`, observe the lab
  event log records ONLY `[info] renderer set to <label> (idle)`
  (no payload bytes), confirm reattach behavior across switches.
- End with the renderer left on the gate's candidate (or back on
  xterm for the baseline parity step). Cleanup: kill the smoke
  SSH container, shred any local key material.

### Browser-side smoke selectors

The dev lab already exposes the renderer radio hooks
(`renderer-option-xterm`, `renderer-option-ghostty-web`,
`renderer-option-restty`, `renderer-option-wterm`). The Gate 1
slice for each candidate adds the matching production-shell
selector under a stable name; the concrete next slice that lands
that selector is captured in [§ "Recommended next implementation
slice"](#recommended-next-implementation-slice) below — that slice
is not part of the current renderer-evaluation plan slice.

### Recommended next implementation slice

(Suggested branch: `feat/experimental-renderer-production-selector`
— name is illustrative; the slice can pick its own.) Concrete next
slice to unblock production-side renderer smokes for the
ghostty-web / restty / wterm experimental adapters on the same
surface the 2026-05-13 xterm baseline smoke established. Surfaced
here so the renderer-evaluation track has a named successor; the
slice itself is **not** part of this docs-only plan slice and is
**not** authorised by this entry — it is described so the next
deliberate slice has a starting point.

**Goal.** Add a production-shell renderer selector that exposes
the experimental renderers (ghostty-web, restty, wterm) **only when
explicitly opted in**, so a future operator can run the renderer
evaluation runbook (`apps/web/e2e/SMOKE.md` § "D. Renderer
evaluation smoke") against each experimental candidate on the
**same surface** as the 2026-05-13 xterm production-baseline smoke
(`docs/deployment/vps-staging-smoke.md` § "2026-05-13 · Xterm
production-baseline renderer smoke"). The selector is the missing
piece per the runbook's "Renderer path confirmation" section.

**Slice boundary (in scope).**

- A production-shell experimental-renderer selector behind an
  **explicit experimental / local-only / operator-enabled gate**.
  Concrete mechanism is for the implementation slice to pick from
  e.g. a build-time env flag, an operator-set runtime config row,
  a hidden URL parameter, or a hidden settings switch — this entry
  proposes the boundary, not the mechanism.
- **xterm stays the production default.** The selector is opt-in,
  never changes the default, and closing or clearing the selector
  returns the production shell to xterm.
- **Do not promote ghostty-web** (or restty or wterm). Gate 1
  (production opt-in) is the ceiling for this next slice; Gate 2
  (default flip) is a separate, later, deliberate slice gated on
  the criteria already in [§ "Promotion criteria"](#promotion-criteria).
- **Do not change backend protocol, session, or orchestrator
  behaviour.** The wire `Output` / `Input` envelope, the replay
  ring, the detach TTL, `terminal-core`, and the renderer-neutral
  seam stay byte-identical. Rule (1) of [§ "Non-negotiable
  architecture rules"](#non-negotiable-architecture-rules) is
  load-bearing for this next slice.
- **Do not expose the experimental renderers to ordinary users.**
  The selector MUST be hidden by default — the default UX must
  not show, hint at, or document a renderer-switch affordance in
  any user-visible surface unless the operator gate is explicitly
  flipped.
- **Selector usable from the staging smoke surface.** The Gate 1
  state of the selector (operator gate flipped, or however the
  slice models opt-in) MUST be reachable from a staging smoke
  context so the renderer evaluation runbook can drive each
  candidate end-to-end against the production shell. The
  data-testid hook is `data-testid="renderer-option-<id>"` to
  match the existing dev-lab selectors.
- **Dynamic import for each experimental adapter.** Each adapter
  remains a dynamic `import()` gated by the selector being
  explicitly enabled, so the default-renderer bundle does not
  regress (the inlined WASM payloads stay tree-shaken on the
  common path).

**Non-goals (explicit out-of-scope for the next slice).**

- No backend / WebSocket protocol changes (rule (1) above).
- No renderer-specific UI inside `apps/web/src/lib/app/` beyond
  the selector itself and the matching `TerminalRenderer`-bridged
  workspace.
- No persistent per-user / per-device renderer preference; the
  current "Explicitly deferred" entry on that row stays deferred.
- No default flip away from xterm (Gate 2 belongs to its own
  slice).
- No promotion decision. One production-side renderer smoke per
  candidate is a data point, **not** a Gate 2 default flip —
  Gate 2 still requires the soak window and per-surface coverage
  already specified.
- No change to the runbook's posture that a dev-lab pass is not a
  production-side renderer pass; the selector unlocks the
  production-side path, it does not retroactively reclassify
  earlier dev-lab passes.

**After the slice lands.**

- The runbook's "Renderer path confirmation" step can identify
  ghostty-web (and restty, wterm) on the production shell at
  staging instead of marking it `deferred — renderer not
  identified`.
- A separate docs / smoke slice can run the renderer evaluation
  matrix against ghostty-web on the production shell and record a
  peer entry under `docs/deployment/vps-staging-smoke.md` § with
  matrix results comparable, row-for-row, to the 2026-05-13 xterm
  production-baseline entry.
- ghostty-web (and the other experimental adapters) remain
  experimental and unpromoted until and unless [§ "Promotion
  criteria" → Gate 2](#gate-2--production-opt-in--production-default)
  is met as its own deliberate slice.

## Explicitly deferred

These are out of scope for the renderer evaluation track. Each
will be its own slice if and when it ships:

- **tmux clone features.** Multi-pane workspace, window layouts,
  copy-mode keybindings, named-window UX, status-line bars. None
  of these are part of RelayTerm's product.
- **Multi-pane workspace.** Even without tmux semantics, a tiling
  multi-pane layout is its own design surface and is deferred.
- **True backend-restart persistence.** `tmux` / `screen` /
  managed-agent integration to survive a RelayTerm backend
  restart — see [`docs/persistent-sessions.md`](persistent-sessions.md)
  Option C / Option D. Not unlocked by renderer work; not blocked
  by renderer work.
- **VT snapshot persistence.** Phase 2 of the persistent-sessions
  roadmap (libghostty-vt-driven snapshot on detach to reconstruct
  display state on resume) is independent of renderer choice and
  remains deferred.
- **Renderer production-default switch.** Flipping the default
  away from xterm is a deliberate later slice gated on Gate 2;
  this plan does not flip it.
- **Performance benchmark automation.** A committed benchmark
  harness (microbenchmarks of `write` throughput, reflow cost,
  scrollback memory pressure) is deferred. The matrix above is
  graded by human evaluators in the first pass.
- **Committed Playwright runner for renderer smokes.** The smoke
  remains manual per `apps/web/e2e/SMOKE.md`. Promoting it to a
  CI-driven runner is its own slice.
- **Passphrase-protected private-key import.** Tracked in
  [`docs/private-key-import.md`](private-key-import.md) § 10;
  unrelated to renderer evaluation.
- **`ssh-copy-id` automation.** Unrelated to renderer evaluation;
  out of scope of v1 inventory work per
  [`docs/spec/auth.md`](spec/auth.md).
- **Per-session-per-device renderer preference persistence.**
  Today the dev lab defaults to xterm on every page load; a
  persistent per-user / per-device renderer preference is a
  production UX slice that lands with or after Gate 1, not as
  part of this plan.

## See also

- [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  — a snapshot scorecard of current production-shell evidence for
  all four adapters, with a recommended next development lane.
- [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
  — the mobile / WebView smoke plan that scopes how the matrix
  extends onto surface 3 (Android WebView) and the new Android
  Chrome surface that prefaces it.
- [`SPEC.md`](../SPEC.md) — architectural invariants and surface
  index.
- [`docs/spec/terminal.md`](spec/terminal.md) — terminal session
  lifecycle, WebSocket attach/detach, replay ring buffer, detach
  TTL.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md) —
  the per-adapter contract for every candidate.
- [`docs/persistent-sessions.md`](persistent-sessions.md) —
  long-term persistence roadmap, including the deferred host-side
  multiplexer option.
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — staging smoke template and history.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) — manual
  smoke procedure with the dev-lab + production-shell selector
  table.
- [`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) —
  the renderer-smoke input-harness plan that carries the
  2026-05-13 baseline's deferred matrix rows forward (Unicode /
  paste / alt-screen / mouse), including input-path taxonomy,
  what each path proves, command matrix, and recommended
  follow-up slice options.
- AGENTS.md § "Task patterns" → renderer adapter task pattern
  (long form in [`docs/agent/task-patterns.md`](agent/task-patterns.md)
  § 1) — the recurring rules for renderer work.
