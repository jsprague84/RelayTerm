# Renderer comparison scorecard

> A snapshot of what RelayTerm actually knows today about its four
> terminal-renderer adapters — `xterm`, `ghostty-web`, `wterm`,
> `restty` — based on the production-shell staging smokes recorded
> in [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
> and the per-adapter contracts in
> [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md).
> The evaluation track this scorecard summarises lives in
> [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md);
> the renderer-fairness input strategy the smokes inherit lives in
> [`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md).

## 1. Purpose / status

- This is a **snapshot of current evidence**, current as of
  2026-05-14. It exists to make the next development slice an
  informed choice rather than a guess.
- It is **not a renderer promotion.** No adapter is promoted by this
  doc. Gate 1 / Gate 2 in
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "Promotion criteria" are unchanged.
- It **does not change the xterm default.** xterm.js remains the
  production compatibility baseline and the default renderer on
  every surface.
- It is **docs-only.** No renderer adapter, CSP, backend / session /
  orchestrator behaviour, terminal protocol, or CI / deploy config
  was touched to produce it.
- "Potential" and "verified" are kept separate throughout. A
  renderer being *promising* for a use case is not the same as that
  use case being *measured*.

## 2. Renderer status table

| | xterm | ghostty-web | wterm | restty |
|---|---|---|---|---|
| **Current role** | Production default + compatibility baseline; recovery / control renderer in every renderer smoke | Experimental, not promoted | Experimental, not promoted | Experimental / research, not promoted |
| **Production-shell availability** | Native; the default attach path | Gated lazy loader only (operator gate + renderer id) | Gated lazy loader only | Gated lazy loader only |
| **Staging smoke status** | Production-baseline smoke 2026-05-13; exercised as recovery renderer through 2026-05-14i | First **graded** matrix smoke 2026-05-14e | Gate smoke 2026-05-14g; **graded** matrix smoke 2026-05-14i | Gate smoke 2026-05-14f; **no matrix row graded** |
| **Input-path status** | Renderer-fair input works (`focusTarget()` → helper textarea) | Renderer-fair input works (`focusTarget()` → contenteditable host) | Renderer-fair input works (`focusTarget()` landed `bde039e` → hidden keyboard textarea) | `focusTarget()` **missing** → `data-renderer-input="none"`; renderer-fair seam unavailable |
| **CSP / runtime requirements** | Runs under strict `default-src 'self'`. Pre-existing `style-src` inline-style console noise (see §3) | Needs `script-src 'self' 'wasm-unsafe-eval'`; same-origin WASM asset after adapter fix `aa6bf9f` | Needs `'wasm-unsafe-eval'` (inlined WASM in `@wterm/core`); DOM-rendered — no canvas / WebGPU / font-CDN deps | Needs `'wasm-unsafe-eval'` **plus** `style-src 'unsafe-inline'` **plus** a `connect-src` / font allowance; WebGPU adapter unavailable headless |
| **Resize / fit status** | Full `fit()` via `@xterm/addon-fit`; production "Fit" control is xterm-specific and works. **Adapter-owned `ResizeObserver` + `FitAddon` autofit** also lands under `BaseTerminalRendererOptions.autofit = true` (operator opt-in via Settings); the 2026-05-15 staging resmoke confirmed PTY reflow on container resize (`24 80` → `26 40` at 390×844). Off by default. Workspace `data-renderer-autofit="active"` post-mount under `autofitEnabled = true` verified on the production shell by the 2026-05-15b diagnostic resmoke. | `works with caveats` — no `fit()`, no grid reflow on resize. Accepts `autofit` and no-ops it honestly (`autofitActive() → false` → workspace `data-renderer-autofit="unsupported"` when autofit enabled) | `works with caveats` — no `fit()`, `autoResize` defaults `false`; **autofit shipped 2026-05-15** (`a2c806b feat(web): add renderer-neutral autofit`) wires `autofit → WTerm.autoResize`; the 2026-05-15 staging resmoke verified real PTY reflow under operator opt-in (`24 80` → `24 35` at 390×844, `24 103` after restoring to 1440×900). The workspace `data-renderer-autofit` diagnostic now lands on `"active"` post-mount under `autofitEnabled = true` — the prior Svelte 5 reactivity bug (plain `let renderer` instead of a reactive rune) was fixed in `79c216b fix(web): update autofit diagnostic after renderer mount` and verified on the production shell by the 2026-05-15b diagnostic resmoke for both wterm AND xterm. | Unknown — never rendered |
| **Mobile / browser-native UX potential** | Baseline; canvas-rendered, mobile behaviour noted as potentially rougher | Unknown; canvas-style, no mobile smoke | **Strongest candidate** — DOM-rendered grid → native selection / copy / paste / IME / soft keyboard. *Renderer-mount verified on Android Chrome (2026-05-15c); live-session attach on mobile is an open workspace-side question — see scorecard "Known concerns" below* | Unknown |
| **Correctness / VT potential** | Mature baseline; not the differentiating engine on its own | **Strong** — libghostty-vt parser via WASM | Promising — Zig + WASM core; alt-screen verified | Unknown — highest text-shaping ambition, never measured |
| **Known blockers** | None | Production CSP (`wasm-unsafe-eval`); open resize/reflow decision | Production CSP (`wasm-unsafe-eval`); the **first surface-2 (Android Chrome) smoke landed 2026-05-15c** ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) § "2026-05-15c") with a *mixed* result — renderer mount + rotation pass, but the workspace session lifecycle reproducibly reached `detached (TTL window) seq=0` and never went live; cause not yet attributed between renderer-side, workspace-side, or network path. Surface-3 (Tauri Android WebView) smoke is **deferred** until that Row 12 question has a hypothesis (the next slice runs an xterm control rerun first to make the attribution). Resize/reflow is substantively closed under operator opt-in (off by default; turning it on by default is its own later decision). | Inline-style CSP; external font fetch; WebGPU unavailable headless; `focusTarget()` missing |
| **Next recommended action** | Optional baseline-hardening lane | Correctness / modern-VT lane (resize/reflow, advanced VT) | Product / mobile UX lane — fit/reflow investigation landed (2026-05-14j, docs-only); renderer-neutral autofit **design** AND **implementation** landed (`feat/renderer-neutral-autofit`, 2026-05-15 — wterm maps `autofit` → `WTerm.autoResize`; ships off by default per [`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md) § 1); the autofit staging resmoke (2026-05-15) and the `data-renderer-autofit` diagnostic resmoke (2026-05-15b) have **both landed**; the **mobile / WebView smoke plan has now landed** in [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md). The Android-Chrome smoke (`docs/wterm-android-browser-smoke`) **landed 2026-05-15c** ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md) § "2026-05-15c") with a mixed result; the xterm-control rerun (`docs/wterm-android-browser-resmoke`) **landed 2026-05-16** and reclassified the surface-2 Row 12 finding as workspace-bound + transient — wterm is exonerated as the cause. The next executable slice is **workspace-side** (`docs/mobile-first-launch-ws-investigation`, working title), not another wterm surface-2 / surface-3 attempt. The mobile-smoke runbook now defaults to **Playwright mobile emulation + server-side inspection**, with real-phone operator work reserved for hardware-dependent rows — see [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md) § 5 ("2026-05-16 methodology update") and [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § D → "Mobile smoke methodology". **Surface 3 (Android Tauri WebView) stays deferred** until the workspace-side fix lands | Separate viability decision — not promotion work |

## 3. Evidence summary per renderer

### xterm

- **Verified.** The 2026-05-13 production-baseline smoke exercised
  launch, basic I/O, in-session resize / fit, a 300-line burst,
  wire-side detach / reconnect inside the TTL, mobile-width
  workspace, and clean close, with zero sentinel leakage. xterm
  has since served as the **recovery / control renderer** in every
  experimental-renderer smoke (2026-05-13 through 2026-05-14i) —
  gate OFF → relaunch → xterm attaches and round-trips commands.
- **Renderer-fair input works.** `focusTarget()` returns xterm's
  hidden helper `<textarea>`; the production workspace stamps
  `data-renderer-input="marked"`.
- **CSP.** Runs under the strict `default-src 'self'` policy with no
  relaxation. The six `style-src` inline-style console errors
  observed on the staging surface are **pre-existing** (first
  recorded 2026-05-14c, carried forward through 14e/f/g/i) and are
  **not a regression** — they are documented as a known baseline
  artifact under the staging CSP.
- **Role.** xterm is the safe default and the control renderer, but
  it is **not by itself the product differentiator** — it is
  xterm.js, a known quantity.

### ghostty-web

- **Experimental, not promoted.**
- **Production-shell matrix smoke completed (2026-05-14e)** — the
  first graded experimental-renderer matrix run. Core-correctness
  rows (basic I/O, long output, trusted paste through the
  production paste-safety pipeline, detach / reconnect / replay)
  all `pass`; unicode / box / wide output and the alternate-screen
  probe render correctly.
- **CSP / runtime.** Requires the staging-only `'wasm-unsafe-eval'`
  relaxation to mount. After the `aa6bf9f` adapter fix it loads its
  WASM via a **same-origin Vite-emitted asset** (no `data:` URL, no
  `connect-src` widening).
- **Renderer-fair input path works** — `focusTarget()` returns its
  `contenteditable` host element.
- **Resize / fit caveat documented** in
  [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
  § "Production-shell evaluation status and resize/fit caveat":
  no xterm-style `fit()`, no grid reflow on container resize.
  Documented adapter behaviour, *not* a `regression vs. baseline`.
- **Strong correctness / modern-VT potential** — it is the
  libghostty-vt parser.
- **Possible next work:** resize / reflow investigation, an
  advanced-VT / curses-app smoke, a mobile smoke, a performance
  benchmark. The named follow-on
  [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
  is **wterm-focused** by design — once the wterm Android-Chrome
  smoke lands, that smoke's harness shape transfers cleanly to a
  ghostty-web mobile smoke if it is run.

### wterm

- **Experimental, not promoted.**
- **Production-shell gate passed (2026-05-14g)** — mounts cleanly
  *and* renders functionally on the staging surface, unlike restty.
- **`focusTarget()` landed (`bde039e`)** — returns wterm's hidden
  keyboard `<textarea>`. (Recorded under the `2026-05-14h` dated
  entry in
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md),
  a code-change entry — not a `vps-staging-smoke.md` smoke entry;
  the date-label space spans both files.)
- **Production-shell matrix smoke completed (2026-05-14i)** — the
  renderer-fair matrix smoke has **already landed** (the second
  graded experimental matrix, and the only DOM-rendered one).
  Core-correctness rows (basic I/O, long output, trusted paste
  through wterm's DOM textarea paste handler, wire-side detach /
  reconnect / replay) all `pass`; unicode / box / wide-CJK output
  and the alternate-screen probe render correctly.
- **DOM-rendered + Zig / WASM core.** The cell grid is ordinary DOM
  nodes — no canvas, no WebGPU, no runtime font-CDN `fetch`.
- **Likely the strongest browser-native / mobile UX candidate** —
  text selection, copy, paste, IME composition and mobile soft
  keyboards flow through platform-native text handling. This is
  **potential**, motivated by the rendering style; the mobile /
  Android / Tauri smokes that would *verify* it have not run.
- **Not a GPU / WebGPU renderer.** wterm's advantage is DOM
  integration, not graphics acceleration — do not describe it as
  GPU-accelerated.
- **Known concerns:** needs the `'wasm-unsafe-eval'` relaxation to
  mount; the **first surface-2 (Android Chrome) execution of the
  mobile smoke plan landed 2026-05-15c**
  ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § "2026-05-15c · wterm Android Chrome (surface 2) browser
  smoke") and produced a mixed signal: the wterm renderer *mounts*
  cleanly on Android Chrome and survives rotation, but the
  workspace session lifecycle did not reach a live PTY in two
  consecutive attempts (`detached (TTL window) seq=0`; backend
  nginx confirmed WS upgrade at HTTP 101). **The 2026-05-16
  xterm-control resmoke**
  ([`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  § "2026-05-16 · `docs/wterm-android-browser-resmoke` (surface
  2, xterm control)") reproduced the same 68 s POST→WS gap +
  detach-at-seq-0 pattern on its very first launch *with the
  xterm production baseline*, then went live within ≈ 2 s of
  POST on launches 2 and 3 against the same throwaway. The
  detach pattern is therefore **workspace-bound + transient**,
  not wterm-specific — wterm is exonerated as the cause. The
  next executable slice is workspace-side
  (`docs/mobile-first-launch-ws-investigation`, working title),
  not another wterm surface-2 / surface-3 attempt. The
  2026-05-15c read of "russh never dialed the SSH target" is
  also corrected: it was based on `docker logs` of the
  linuxserver/openssh-server throwaway, which only writes
  init / boot lines to stdout; `netstat -tn` inside the
  throwaway is the correct probe. Tauri Android (surface 3)
  smoke stays deferred until the workspace-side fix lands.
  The earlier resize / fit / reflow
  concern (`works with caveats`, no `fit()`, `autoResize` defaults
  `false`, no grid reflow) is **substantively closed under operator
  opt-in** by the autofit implementation + 2026-05-15 resmoke +
  2026-05-15b diagnostic fix; it remains "off by default" so a
  default-flip story still owns the decision of when to make
  autofit on by default.
- **Resize/reflow root cause established (2026-05-14j,
  docs-only investigation).** wterm is *not* the blocker —
  `WTerm.resize(cols, rows)` genuinely reflows the grid,
  and wterm's `autoResize` `ResizeObserver` self-fits. The
  production non-reflow is a RelayTerm-side abstraction
  gap: the adapter defaults `autoResize` to `false`, the
  `wtermOnly.autoResize` opt-in is structurally
  unreachable from the production shell, and `safeFit()`
  duck-types for an xterm-`FitAddon`-shaped synchronous
  `fit()` that wterm cannot satisfy. The fix is a
  deliberate renderer-neutral, **observer-shaped** autofit
  capability — **now designed** in
  [`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md)
  (mount-time `autofit` option + `autofitActive()` query;
  implementation is the named `feat/renderer-neutral-autofit`
  slice). See also
  [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
  § "Resize / fit / reflow — investigation findings" and
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "2026-05-14j".

### restty

- **Experimental / research, not promoted.**
- **Loader / mount path reached but non-functional.** On the
  2026-05-14f gate smoke the gated `import()` resolved, the
  constructor ran, `mount()` **resolved**, and the backend session
  attached — but nothing rendered: the `<canvas>` stayed at 1 × 1 px
  and `last_seen_seq` stayed `0`. Because `mount()` *resolved*
  rather than rejected, the loader's closed fallback taxonomy could
  not describe the failure and **no operator-visible error panel
  appeared** (a recorded taxonomy gap).
- **Blockers (as observed on the evaluated staging surface):**
  - restty applies **inline styles** for layout — blocked by the
    CSP `default-src 'self'` fallback (no `style-src 'unsafe-inline'`),
    so the canvas never sized.
  - restty's text-shaper **`fetch()`es a font stack from
    `cdn.jsdelivr.net`** — blocked by the same `connect-src`
    fallback.
  - the headless evaluation browser exposed **no WebGPU adapter**
    (`No available adapters`).
  - `ResttyRenderer` does **not** implement `focusTarget()`, so the
    renderer-fair input seam is unavailable regardless.
  The exact restty-internal mechanism (which inline styles are
  load-bearing, whether a self-hosted font bundle changes
  behaviour) is **not** established by the smoke and is not
  asserted here — see
  [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
  § "Production-shell evaluation status and CSP caveat".
- **Highest GPU / text-shaping ambition, highest integration risk.**
  restty pairs libghostty-vt with WebGPU/WebGL2 and a TypeScript
  text shaper, and ships ~3 MB JS plus inlined WASM. None of that
  capability has been *measured* — it cannot render on the
  evaluated surface.
- **Next action should be a feasibility / viability decision**, not
  immediate promotion work. Whether to widen the staging CSP
  (`style-src 'unsafe-inline'`, a font allowance) is a separate,
  deliberate decision weighed against the page-level-CSP tradeoffs
  in [`docs/ghostty-web-wasm-csp.md`](ghostty-web-wasm-csp.md).

## 4. Comparison categories

Qualitative labels (the repo has no numeric scoring convention):

- **strong** — verified working on the production shell with no
  material caveat.
- **promising** — verified working but with a documented caveat, or
  a credible advantage not yet fully measured.
- **baseline** — xterm's reference behaviour; the bar others are
  measured against.
- **blocked** — a known blocker prevents this from working on the
  evaluated surface today.
- **unknown** — not yet measured; no smoke has exercised it.
- **deferred** — *measured*, but the result is a documented caveat
  or an open decision that a named slice owns. Distinct from
  *unknown*: the behaviour is observed, the *decision* about it is
  what is outstanding.

| Category | xterm | ghostty-web | wterm | restty |
|---|---|---|---|---|
| Mount reliability | baseline | promising (clean under `wasm-unsafe-eval`; prior mount-failure history closed) | strong (mounts + renders functionally) | blocked (mounts but non-functional; taxonomy gap) |
| Renderer-fair input | baseline | strong (verified in matrix) | strong (verified in matrix) | blocked (no `focusTarget()`) |
| Basic I/O | baseline | strong | strong | unknown |
| Long output | baseline | strong (300-line burst) | strong (300-line burst) | unknown |
| Unicode / box / wide chars | baseline | promising (renders legibly; typography precision not measured) | promising (correct codepoints; per-glyph cell width not measured) | unknown |
| Paste | baseline | strong (trusted Ctrl+V → paste-safety pipeline) | strong (DOM textarea paste handler → paste-safety pipeline) | unknown |
| Alternate screen | baseline | promising (enter / leave verified; no `tput` on target) | promising (switch + restore verified) | unknown |
| Detach / reconnect / replay | baseline | strong (wire-side) | strong (wire-side; fresh remount) | unknown |
| Resize / fit / reflow | baseline (full `fit()` + adapter-owned autofit under opt-in) | deferred (no `fit()`, no reflow — open Gate-1 decision) | promising under operator opt-in (autofit maps to `WTerm.autoResize`; PTY reflow verified 2026-05-15) | unknown |
| Narrow / mobile viewport | baseline | deferred (same reflow caveat) | promising under operator opt-in (390 × 844 reflowed to `24 35` in the 2026-05-15 desktop-browser resmoke; real-mobile rows still **unmeasured** — see [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)) | unknown |
| Copy / select / find potential | baseline (canvas selection model) | unknown (canvas) | promising (DOM nodes → native selection) | unknown |
| Accessibility potential | baseline | unknown | promising (DOM-rendered output) | unknown |
| Bundle / runtime cost | baseline (main chunk) | promising (lazy chunk + ~423 KB WASM asset, off the default path) | strong (~41 KB lazy chunk incl. inlined WASM) | deferred (~3 MB JS + inlined WASM; lazy, off the default path) |
| CSP / deploy friction | baseline (runs strict) | blocked for strict production (needs `wasm-unsafe-eval`) | blocked for strict production (needs `wasm-unsafe-eval`; otherwise light) | blocked (needs `wasm-unsafe-eval` + `style-src` + font / `connect-src` + WebGPU) |
| Desktop / Tauri risk | baseline (WebKitGTK known) | unknown (no Tauri smoke) | unknown (no Tauri smoke) | unknown (+ WebGPU availability concern) |
| Android / mobile risk | baseline (canvas, potentially rougher) | unknown | promising-but-unknown (motivating story; unverified) | unknown |
| Promotion readiness | baseline (it *is* the default) | deferred (one graded matrix; Gate 1 not formally reviewed) | deferred (one graded matrix; Gate 1 not formally reviewed) | blocked (cannot clear Gate 1 on the evaluated surface) |

## 5. Product interpretation

- **xterm is the safest default.** It is mature, runs under the
  strict production CSP, and is the proven recovery renderer. It
  stays the default on every surface. But it is xterm.js — keeping
  it is the *low-risk* choice, not the *differentiating* one.
- **ghostty-web is the best correctness / modern terminal-engine
  candidate.** It carries the libghostty-vt parser and has one
  graded production-shell matrix behind it. Its open question is
  resize / reflow and depth of VT correctness, not viability.
- **wterm is the best web-native / mobile UX candidate.** Because it
  renders into DOM nodes, selection / copy / paste / IME / soft
  keyboards can use platform-native handling. It has a graded
  production-shell matrix *and* `focusTarget()` landed. Its
  advantage is **DOM integration, not GPU acceleration** — do not
  conflate the two.
- **restty is a research track** until its viability blockers
  (inline-style CSP, external font fetch, WebGPU availability,
  missing `focusTarget()`) are resolved. Its GPU / text-shaping
  ambition is real but **entirely unmeasured** on the production
  shell — ambition is not readiness.
- **The renderer is not the whole differentiator.** RelayTerm's
  load-bearing product value is *renderer choice* **plus**
  backend-managed SSH sessions, sequence-numbered reconnect /
  replay, server profiles, key and host-key trust, audit /
  redaction posture, and web / desktop / mobile access. The
  renderer is one swappable layer on top of that — picking a
  renderer lane sharpens the experience; it does not by itself
  define the product.

## 6. Recommended next development lane

**Primary: wterm product / mobile UX lane.**

- wterm is DOM-rendered, which makes it the strongest candidate for
  selection / copy / find / accessibility / mobile-keyboard
  behaviour — the dimensions a web/desktop/mobile SSH client most
  needs to get right and that xterm's canvas model handles less
  naturally.
- The renderer-fair production-shell matrix smoke landed 2026-05-14i;
  the fit / reflow investigation landed 2026-05-14j; the
  renderer-neutral autofit **design** landed
  ([`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md))
  and the **implementation** landed in `feat/renderer-neutral-autofit`
  (2026-05-15). The staging resmoke (2026-05-15) verified PTY reflow
  end-to-end (1440 × 900 → `24 80`, 390 × 844 → `24 35`); the
  diagnostic resmoke (2026-05-15b) confirmed
  `data-renderer-autofit="active"` from the production shell.
- The **mobile / WebView smoke plan** has now landed
  ([`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)),
  docs-only: it scopes the surfaces (desktop narrow viewport,
  Android Chrome, Android Tauri WebView, desktop Tauri sanity, iOS
  explicitly out of scope), the 17 candidate rows, the
  instrumentation / redaction posture, and the decision outputs.
  The next slice is the **Android-Chrome smoke**
  (`docs/wterm-android-browser-smoke`) per § 11 of that plan;
  Android-Tauri (`docs/wterm-android-tauri-smoke`) is the immediate
  follow-on once the Chrome surface is green.

**Secondary (backup): ghostty-web correctness lane.**

- Focus on the resize / reflow caveat and advanced VT / curses-app
  cases (`vim`, `less`, `htop` once a larger-tooling target image
  is available). ghostty-web's libghostty-vt parser is the reason
  to push correctness depth here.

**Defer restty** until a separate restty viability decision: it
requires broader CSP / font / WebGPU choices before any matrix
evaluation is even possible. Do not spend a renderer-evaluation
slice on restty until that decision is made.

## 7. Next slice proposals (ranked)

1. **`docs/mobile-first-launch-ws-investigation`** (working
   title) — workspace-side instrumentation of the mobile-Chrome
   WS-open path so the 60-68 s first-launch POST→WS gap surfaced
   by the 2026-05-15c surface-2 wterm smoke and reproduced under
   the xterm production baseline in the 2026-05-16 resmoke can be
   attributed to (a) the mobile-Chrome side, (b) the
   Cloudflare → origin tail, or (c) the workspace's own client
   state machine, plus a workspace-visible diagnostic the
   operator can see *during* the gap. *Primary lane.* Until this
   lands, every mobile renderer smoke (wterm OR xterm OR
   ghostty-web) re-collects the same intermittent first-launch
   detach pattern as evidence of the wrong question; running a
   surface-3 Tauri smoke first would only double the redaction
   surface for no incremental signal. The mobile smoke methodology
   that re-tests will sit on top of (Playwright mobile emulation +
   server-side inspection + narrow real-phone scope, per
   [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
   § 5 "2026-05-16 methodology update" and
   [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § D →
   "Mobile smoke methodology") is in place; the workspace slice
   is the missing prerequisite, not the methodology.
2. **`docs/wterm-android-tauri-smoke`** — surface-3 Tauri
   Android WebView smoke. **Deferred** until the workspace
   slice above lands; the same workspace-bound first-launch
   detach pattern would re-appear here and contaminate the
   row sweep. When it does run, it runs under the
   Playwright-first / real-phone-narrow methodology — the
   APK piggy-backs on the 2026-05-09 Android Tauri shell
   infrastructure (Galaxy S10e, bundled-shell handoff already
   proven) for the genuinely-hardware rows, and the
   `data-*` / `session_events` / `audit_events` rows are
   driven from the desktop side against the same staging
   backend the APK talks to.
3. **`docs/ghostty-web-advanced-vt-smoke-plan`** — plan an
   advanced-VT / curses-app smoke (needs a larger-tooling target
   image; the current target lacks `tput` / `tmux`).
4. **`docs/restty-production-viability-decision`** — a focused
   decision doc on whether (and how) to unblock restty: staging
   CSP `style-src` / font choices, `focusTarget()`, WebGPU
   availability. Decision, not implementation.
5. **`docs/renderer-benchmark-plan`** — plan the deferred
   performance / benchmark harness so future renderer comparisons
   have a measured throughput / reflow-cost / memory axis instead
   of human-readable observations.

## 8. Non-goals

- **No promotion.** This doc promotes no renderer.
- **No xterm-default flip.** xterm stays the default on every
  surface.
- **No production CSP change.** Production deploy templates stay
  strict; the staging `'wasm-unsafe-eval'` relaxation is not
  extended here.
- **No desktop / mobile smoke in this doc.** Tauri / Android smokes
  are named as future slices, not performed.
- **No benchmark automation yet.** The benchmark harness stays
  deferred; it is listed as a proposed future slice.
- **No tmux / screen persistence work.** Host-side multiplexer
  persistence is independent of the renderer track and stays
  deferred per
  [`docs/persistent-sessions.md`](persistent-sessions.md).

## See also

- [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  — the renderer-evaluation plan, the per-date smoke history, and
  the Gate 1 / Gate 2 promotion criteria.
- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md) —
  the four renderer-adapter contracts and the per-adapter
  "Production-shell evaluation status" caveats.
- [`docs/renderer-neutral-autofit.md`](renderer-neutral-autofit.md)
  — the observer-shaped renderer-neutral autofit design + the
  per-adapter implemented contract (now landed).
- [`docs/wterm-mobile-smoke-plan.md`](wterm-mobile-smoke-plan.md)
  — the mobile / WebView smoke plan (surfaces, rows,
  instrumentation, redaction posture, decision outputs); the basis
  for § 7's ranked next slice.
- [`docs/renderer-smoke-harness.md`](renderer-smoke-harness.md) —
  the input-path taxonomy and command matrix the smokes inherit.
- [`docs/deployment/vps-staging-smoke.md`](deployment/vps-staging-smoke.md)
  — the staging smoke entries this scorecard summarises
  (2026-05-13 through 2026-05-15b; the next renderer-evaluation
  entry will be the wterm Android-Chrome smoke).
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § "D. Renderer
  evaluation smoke" — the operator runbook for the matrix.
- [`docs/ghostty-web-wasm-csp.md`](ghostty-web-wasm-csp.md) — the
  WASM / CSP decision doc behind the staging `'wasm-unsafe-eval'`
  relaxation.
