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
   this is the row `wterm` is motivated by.
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
