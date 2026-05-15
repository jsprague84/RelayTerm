# Renderer-neutral autofit — design

> Design for a renderer-neutral way to express "keep the terminal sized
> to its container" without leaking xterm's `FitAddon` shape into
> `terminal-core` or `ProductionTerminal.svelte`.
>
> This is a **design doc**. It changes no adapter, `terminal-core`,
> production-shell, protocol, backend / session / orchestrator, CSP, or
> CI / deploy code. It is the follow-on to the now-landed
> `feat/wterm-fit-reflow-investigation` (2026-05-14j) recorded in
> [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
> § "Resize / fit / reflow — investigation findings".

## 1. Status / decision summary

- **Status:** design only — no code in this slice. The implementation
  is the named follow-on slice `feat/renderer-neutral-autofit`
  (§ 14).
- **Not a renderer promotion.** No adapter is promoted by this doc.
  Gate 1 / Gate 2 in
  [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  § "Promotion criteria" are unchanged.
- **Does not change the xterm default.** xterm.js stays the production
  compatibility baseline and the default renderer on every surface.
- **Chosen design (§ 7):** a **mount-time, renderer-neutral `autofit`
  option** on `BaseTerminalRendererOptions`, plus an optional
  **`autofitActive()` query** on the `TerminalRenderer` interface for
  honest diagnostics. The capability is **observer-shaped** — "observe
  the mount container and keep the cell grid fitted to it, emitting
  `onResize` on each change" — not a synchronous one-shot `fit()`.
  Each adapter owns its implementation; unsupported renderers report
  unsupported honestly.
- **Default off.** `autofit` defaults to `false`, so the
  implementation slice ships **zero behaviour change** until an
  operator opts in. xterm's current fixed-grid + manual-Fit behaviour
  is untouched on the default path.
- **Rejected:** a synchronous neutral `fit(): { cols, rows }` (xterm-
  `FitAddon`-shaped leak), a runtime observer-handle method
  (`autofit(): Unsubscribe`) (wterm's `autoResize` is `init()`-time,
  not runtime-toggleable), and a `ProductionTerminal`-owned
  `ResizeObserver` (would push per-renderer cell-pixel math up into the
  shell). See § 6.

## 2. Problem statement

RelayTerm's production terminal has an **xterm-`FitAddon`-shaped fit
path**:

- `XtermRenderer` exposes an xterm-only `fit(): { cols, rows } | null`
  method (backed by `@xterm/addon-fit`).
- The production workspace's "Fit" button routes through `safeFit()`
  in `apps/web/src/lib/app/terminal/terminalLaunch.ts`, which
  duck-types for a synchronous `fit()` method and is a clean runtime
  no-op on a renderer that does not expose one.
- `fit()` is **not** on the renderer-neutral `TerminalRenderer`
  surface — and that is correct, because it is an xterm-specific
  shape.

That works for xterm. It does **not** model renderers whose
container-fit behaviour is observer-driven (wterm) or absent
(ghostty-web, restty today). The 2026-05-14i wterm matrix smoke showed
wterm stayed at its mounted column count on a container/viewport
resize, and a narrowed viewport clipped rather than reflowed. The
product needs a **renderer-neutral way to say "stay fitted to the
container"** that:

1. lets xterm keep using `FitAddon` internally;
2. lets wterm use its own `autoResize` / `ResizeObserver` path;
3. lets ghostty-web / restty report "unsupported" honestly until they
   grow a fit path;
4. never leaks any one renderer's fit *mechanism* into `terminal-core`
   or `ProductionTerminal.svelte`.

## 3. Evidence from the wterm fit investigation

From [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md)
§ "Resize / fit / reflow — investigation findings (2026-05-14j)" — a
code-reading investigation of `@wterm/dom@0.2.1`, `WtermRenderer`,
`rendererLoader.ts`, and `settingsToRendererOptions`:

- **wterm *can* reflow.** `WTerm.resize(cols, rows)` — public, wired
  through `WtermRenderer.resize()` — calls `bridge.resize()` **and**
  `renderer.setup(cols, rows)`, a genuine cell-grid + PTY-geometry
  reflow.
- **wterm ships a native fit-to-container path.** With
  `autoResize: true` (wterm's own default), `WTerm.init()` runs
  `_setupResizeObserver()`: it measures char/row pixel size and
  attaches a `ResizeObserver` to the host that recomputes
  `cols = floor(width/charWidth)` / `rows = floor(height/rowHeight)`
  and calls `resize()`, fanning out through `onResize`. With
  `autoResize: false`, `init()` runs `_lockHeight()` instead and never
  observes the container. **This is decided at `init()` time** — it is
  not a post-mount runtime toggle.
- **The production non-reflow is a RelayTerm-side abstraction gap, not
  an upstream limitation.** It is the sum of three RelayTerm-side
  facts:
  1. `WtermRenderer` defaults `wtermOnly.autoResize` to `false` (a
     deliberate "caller drives sizing" parity choice with
     xterm/ghostty-web/restty), so `_lockHeight()` runs.
  2. The `wtermOnly.autoResize` opt-in is **structurally unreachable
     from the production shell** — `settingsToRendererOptions()`
     returns `Required<BaseTerminalRendererOptions>`, `wtermOnly` lives
     on `WtermRendererOptions` (not the base type), and
     `rendererLoader.ts` only forwards the base options plus
     `cols`/`rows`.
  3. The production "Fit" affordance and `safeFit()` are
     xterm-`FitAddon`-shaped — they duck-type for a synchronous
     `fit(): { cols, rows }`, which wterm exposes no public analogue
     for (`_measureCharSize` / `_container` / `_setupResizeObserver` /
     `_lockHeight` are all private).
- **The investigation's recommendation:** design the renderer-neutral
  capability **observer-shaped** — "observe my container and self-fit,
  emitting `onResize`" — rather than xterm's synchronous one-shot
  `fit()`. "xterm's `FitAddon` can be wrapped to that shape; wterm's
  `autoResize` `ResizeObserver` *already is* that shape; ghostty-web
  can no-op until it grows one."

This design doc is that observer-shaped design.

## 4. Current xterm-shaped behaviour (what exists today)

- **`terminal-core`** — `TerminalRenderer` (`renderer.ts`) has
  `resize(cols, rows)` (caller drives the visible cell grid) and an
  optional `onResize?(cb)` (renderer-driven cell-grid resize the
  caller subscribes to). There is **no fit / autofit concept** on the
  neutral surface, deliberately. `BaseTerminalRendererOptions`
  (`rendererOptions.ts`) carries only cosmetic knobs (`fontFamily`,
  `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`,
  `scrollbackLines`, `theme`).
- **`XtermRenderer`** — exposes an **xterm-only** `fit(): { cols,
  rows } | null` (not on the neutral interface), backed by
  `@xterm/addon-fit`. `fit()` calls `FitAddon.fit()`, which fans out
  synchronously through xterm's `onResize` → the adapter's `onResize`
  listeners.
- **`WtermRenderer`** — `wtermOnly.autoResize` defaults to `false`;
  when `true`, the WTerm constructor's `autoResize` is set and wterm's
  own `ResizeObserver` does the work, fanning through `onResize`. No
  `fit()` method.
- **`GhosttyWebRenderer` / `ResttyRenderer`** — no `fit()` method, no
  container-observation path.
- **`safeFit(renderer)`** (`terminalLaunch.ts`) — duck-types for a
  synchronous `fit()`; clean no-op when absent. The workspace "Fit"
  button (`production-terminal-fit`) calls it; the resulting wire
  `resize` frame is driven by the renderer's own `onResize` fanout
  (the single `onResize → client.sendResize` subscription in
  `ProductionTerminal.svelte` — the double-emit rule recorded in
  [`docs/agent/encountered-lessons.md`](agent/encountered-lessons.md)
  2026-04-29).
- **Settings** — `TerminalSettings` (`terminalSettings.ts`) is a
  local-only browser preference snapshot; `settingsToRendererOptions`
  maps it onto `BaseTerminalRendererOptions`. There is no autofit /
  resize preference today.
- **Diagnostics** — `ProductionTerminal.svelte` already mirrors
  renderer state onto `data-renderer*` attributes (`data-renderer`,
  `data-renderer-experimental`, `data-renderer-fallback`,
  `data-renderer-gate`, `data-renderer-input`). There is no autofit
  diagnostic.

## 5. Renderer-by-renderer requirements

| Renderer | Fit model today | Target under this design |
|---|---|---|
| **xterm** | Synchronous one-shot `fit()` via `@xterm/addon-fit`; manual "Fit" button only | When neutral `autofit` is on, the adapter owns a `ResizeObserver` on the mount element and calls `FitAddon.fit()` per callback (rAF-coalesced). `autofitActive()` → `true`. The xterm-only one-shot `fit()` stays for the manual button. |
| **wterm** | `WTerm.autoResize` `ResizeObserver` exists but defaults `false`; the opt-in is unreachable from production | Neutral `autofit` maps to `WTermOptions.autoResize = true` at construct time. wterm's own `ResizeObserver` does the work, fanning through `onResize`. `autofitActive()` reflects the resolved `autoResize`. `wtermOnly.autoResize` stays as the raw non-portable escape hatch (precedence in § 8). |
| **ghostty-web** | No `fit()`, no container-observation path | Accepts the neutral `autofit` option on its surface and **silently no-ops** it (same pattern as the cosmetic knobs it already drops). `autofitActive()` → `false`. Documented; revisited if ghostty-web grows a reflow path. |
| **restty** | No `fit()`; also non-functional under the strict staging CSP | Accepts `autofit`, **silently no-ops** it, `autofitActive()` → `false`. Documented; restty is independently blocked (CSP / WebGPU / `focusTarget()` — see the scorecard) so autofit is moot until that viability decision lands. |

## 6. Candidate designs

### Candidate 1 — synchronous neutral `fit(): { cols, rows }` on `TerminalRenderer`

Promote xterm's `fit()` shape to the neutral interface.

**Rejected.** This is the xterm-`FitAddon` shape. wterm has no public
synchronous one-shot container-measurement method (its char-size
measurement is private and runs *inside* its async `ResizeObserver`
callback); ghostty-web has none either. Formalising the synchronous
shape would give xterm a neutral home but leave wterm and ghostty-web
unable to honour it — an xterm-shaped abstraction leak into
`terminal-core`. The 2026-05-14j investigation already rejected this
for the same reason.

### Candidate 2 — runtime observer-handle method `autofit(): Unsubscribe | null`

A method the workspace calls post-mount to *start* observing; returns
a stop function.

**Rejected.** wterm's `autoResize` is decided at `WTerm.init()` time
(`_setupResizeObserver()` vs `_lockHeight()`); it is **not** a
post-mount runtime toggle. Honouring a runtime `autofit()` on wterm
would force the adapter to either (a) re-mount the renderer on toggle,
or (b) own its own `ResizeObserver` *and* reimplement wterm's private
`_measureCharSize` against its private `_container` and CSS class
names — a fragile internals leak far beyond the single narrow
`focusTarget()` structural cast the adapter already contains. The
runtime-method shape buys flexibility no renderer can actually deliver
cleanly.

### Candidate 3 — `ProductionTerminal`-owned `ResizeObserver`

The workspace observes the viewport element itself, computes
`(cols, rows)`, and calls `renderer.resize(cols, rows)`.

**Rejected.** Computing `(cols, rows)` from a pixel box requires
per-renderer cell-pixel metrics (char width, row height) that only the
renderer knows. The workspace would have to reach into renderer
internals — exactly the "renderer-specific hacks in
`ProductionTerminal`" the architecture forbids. It also duplicates
logic xterm's `FitAddon` and wterm's `ResizeObserver` already
implement correctly.

### Candidate 4 — mount-time neutral `autofit` option + `autofitActive()` query *(chosen)*

A renderer-neutral **boolean option** (`autofit`) that says "keep me
fitted to my container", honoured at mount/construct time by each
adapter using its own mechanism, plus an optional **`autofitActive()`**
query for honest post-mount diagnostics. Fitting changes still surface
through the **existing** `onResize` seam — no new event channel.

**Chosen.** It is observer-shaped (the option *describes the intent*,
not a mechanism), it matches wterm's mount-time reality, xterm's
`FitAddon` wraps cleanly to it, ghostty-web/restty no-op it honestly,
and it threads through the existing `settingsToRendererOptions` →
`rendererLoader` → adapter plumbing with no new loader code. Detail in
§ 7–§ 9.

## 7. Chosen design

A renderer expresses container-fit through **one neutral option** and
**one optional neutral query**:

1. **`BaseTerminalRendererOptions.autofit?: boolean`** (new,
   `terminal-core`). Renderer-neutral. Semantics: *"observe the mount
   container and keep the cell grid fitted to it, emitting `onResize`
   on each change."* Default `false`. It describes the **intent**, not
   the mechanism — each adapter implements it however its renderer
   naturally does (or drops it, like the cosmetic knobs a renderer
   can't honour).
2. **`TerminalRenderer.autofitActive?(): boolean`** (new optional
   method, `terminal-core`). Reports the **post-mount truth**: `true`
   only when autofit is genuinely wired (xterm: live `ResizeObserver` +
   `FitAddon`; wterm: resolved `WTerm.autoResize === true`). `false`
   before mount, after dispose, when `autofit` was `false`, or for a
   renderer that accepts the option but no-ops it. A renderer that
   omits the method entirely is treated as "autofit unsupported". This
   mirrors the existing optional `focusTarget?()` precedent — neutral,
   diagnostic-only, never carries payload bytes.

**Why mount-time and not runtime:** wterm's autofit is `init()`-time
(§ 3). Making the neutral capability mount-time keeps the contract
honest for every adapter — xterm *could* toggle at runtime, but a
contract only one renderer can satisfy is not a neutral contract. A
renderer-id / autofit change takes effect on the **next attach**, the
same per-attach model `ProductionTerminal` already uses for renderer
selection and cosmetic settings.

**Why `onResize` stays the signal channel:** the workspace already has
exactly one `onResize → client.sendResize` subscription. Whether the
resize was driven by a manual `resize()` call, xterm's `FitAddon`, or
wterm's `ResizeObserver`, it arrives through `onResize`. Autofit
therefore needs **no new event** — it just makes `onResize` fire on
container changes. The single-subscription / no-double-emit rule
recorded in
[`docs/agent/encountered-lessons.md`](agent/encountered-lessons.md)
(2026-04-29) is preserved unchanged.

## 8. Capability / API sketch

> Illustrative TypeScript — the implementation slice owns the final
> shape, doc comments, and tests.

### `terminal-core` — `rendererOptions.ts`

```ts
export interface BaseTerminalRendererOptions {
  // ...existing cosmetic knobs...
  /**
   * Keep the cell grid fitted to the mount container: observe the
   * container and re-fit on resize, emitting `onResize` on each
   * change. Renderer-NEUTRAL intent, not a mechanism — each adapter
   * honours it with its own container-observation path (xterm:
   * ResizeObserver + FitAddon; wterm: WTerm.autoResize). A renderer
   * with no container-fit path accepts the field and silently drops
   * it, exactly like the cosmetic knobs it cannot honour. Default
   * false: the caller drives sizing explicitly via `resize()`.
   */
  autofit?: boolean;
}
```

### `terminal-core` — `renderer.ts`

```ts
export interface TerminalRenderer {
  // ...existing members...
  /**
   * Optional: report whether autofit (`BaseTerminalRendererOptions.
   * autofit`) is genuinely wired for this mounted renderer. `true`
   * only when a real container-observing fit is active; `false`
   * before mount, after dispose, when autofit was not requested, or
   * for a renderer that accepts the option but no-ops it. A renderer
   * that omits this method is treated as "autofit unsupported".
   *
   * Diagnostic-only — like `focusTarget()`, it never reads or carries
   * payload bytes. Fitting changes still flow through `onResize`.
   */
  autofitActive?(): boolean;
}
```

### Adapter behaviour

- **`XtermRenderer`** — in `mount()`, when `options.autofit`, create a
  `ResizeObserver` on the mount element; on each callback (rAF-
  coalesced) call `FitAddon.fit()`. `FitAddon.fit()` already fans out
  through xterm's `onResize` → the adapter's `onResize` listeners, so
  no extra wiring. Tear the observer down in `dispose()`.
  `autofitActive()` returns `true` while that observer is live. The
  xterm-only `fit()` one-shot is **unchanged** and still used by the
  manual button.
- **`WtermRenderer`** — `toWtermOptions` resolves
  `autoResize = wtermOnly.autoResize ?? options.autofit ?? false`. The
  explicit non-portable `wtermOnly.autoResize` still wins when set
  (it is the deliberate escape hatch); otherwise the **portable**
  `autofit` drives it; default `false` unchanged. wterm's own
  `ResizeObserver` does the work and fans through `onResize`.
  `autofitActive()` returns the resolved `autoResize` value
  post-mount.
- **`GhosttyWebRenderer` / `ResttyRenderer`** — accept `autofit` on
  the neutral surface, **silently drop it** during option mapping
  (documented adapter behaviour, same as the cosmetic knobs they
  drop). `autofitActive()` returns `false` (or the method is omitted).

### Settings plumbing (local-only)

- **`TerminalSettings`** gains `autofitEnabled: boolean`, default
  `false`. Stored in the same local-only `localStorage` snapshot;
  **never** server-side. Adding a field is a v1→v2 storage-key bump
  per the existing `terminalSettings.ts` contract — the implementation
  slice owns that migration.
- **`settingsToRendererOptions`** maps `autofitEnabled → autofit`. The
  returned object is still assignable to `BaseTerminalRendererOptions`,
  so **`rendererLoader.ts` needs no change** — it already forwards the
  base options to every adapter constructor.

## 9. UI and diagnostics

### Settings view

A new local-only checkbox: **"Fit terminal to its container
(autofit)"**, default off. Same "applies on next session" model as the
other terminal settings (the `TERMINAL_UX_COPY.settingsApplyNote`
copy). Honest sub-copy that autofit is honoured by xterm and wterm
today and is a no-op on ghostty-web / restty.

### The production "Fit" button

The Fit button stays a **best-effort one-shot refit** via the existing
`safeFit()` (the xterm-only `fit()` — unchanged). It is **not**
rewired to drive the neutral `autofit` capability, because `autofit`
is an observer-shaped mount-time capability and the button is a
one-shot action — conflating them would re-introduce the xterm-shaped
assumption this design removes. Instead the button is made **honest
about what it does**, informed by `autofitActive()`:

- When `autofitActive()` is `true` (autofit is doing continuous
  fitting): the one-shot button is redundant — disable it with a
  tooltip ("Autofit is keeping the terminal sized to its container"),
  or hide it. The implementation slice picks one; disable-with-tooltip
  is the lower-surprise choice.
- When `autofitActive()` is `false` and the renderer exposes `fit()`
  (xterm without autofit): the button behaves exactly as today.
- When `autofitActive()` is `false` and the renderer exposes no
  `fit()` (ghostty-web / restty / wterm without autofit): the button
  is a clean no-op today — disable it with honest "not supported by
  this renderer" copy rather than presenting a button that does
  nothing.

So the button **reads** the neutral capability for its enablement /
copy; it does not **drive** it. That is the honest reconciliation of
"the Fit button should use the renderer-neutral capability" with the
observer-shaped reality.

### Diagnostic attribute

A new attribute on `production-terminal`, **`data-renderer-autofit`**,
closed vocabulary:

| Value | Meaning |
|---|---|
| `off` | Operator did not enable autofit (`autofit` option `false`). |
| `active` | Autofit enabled **and** the mounted renderer wired it (`autofitActive() === true`). |
| `unsupported` | Autofit enabled **but** the mounted renderer no-ops it (`autofitActive()` is `false` or absent). |

This sits alongside the existing `data-renderer*` family and gives a
future smoke a stable, payload-free way to prove autofit was actually
active for a given renderer — without visual guessing. It is
operator-facing taxonomy only; it never carries bytes.

## 10. Testing strategy

For the implementation slice (named here so the design is bounded):

- **`terminal-core`** — type-level only (the new option + optional
  method are interface shape). No runtime behaviour in core.
- **Adapter unit tests** (each `packages/terminal-*/tests/`):
  - `XtermRenderer` — `autofit: true` attaches a `ResizeObserver`
    (mocked) and a simulated container resize calls `FitAddon.fit()`
    and fans out through `onResize`; `dispose()` disconnects the
    observer; `autofitActive()` is `true` while live, `false` after
    dispose / before mount / when `autofit` is `false`.
  - `WtermRenderer` — `autofit: true` resolves `WTermOptions.autoResize
    = true`; `wtermOnly.autoResize` still overrides; `autofitActive()`
    reflects the resolved value.
  - `GhosttyWebRenderer` / `ResttyRenderer` — `autofit: true` is
    accepted and dropped (not echoed into the underlying constructor's
    options blob — extend the existing sentinel/redaction option-blob
    tests); `autofitActive()` is `false` / absent.
  - **Redaction tests stay green verbatim** — `autofit` is a boolean;
    it must not appear in any thrown error, `console.*`, or options
    blob beyond the documented mapping. Extend each adapter's existing
    sentinel test rather than adding a parallel suite.
- **`terminalSettings.test.ts`** — `autofitEnabled` parses, clamps to
  a boolean, defaults `false`, survives the v1→v2 migration, and the
  redaction sentinel tests still pin no secret in `serializeSettings`.
- **`terminalLaunch.test.ts`** — the Fit-button enablement/copy
  helper, driven by `autofitActive()`, is pinned for the three states
  in § 9.
- **`appShellIsolation.test.ts`** — unchanged and must stay green: no
  new static import of an experimental adapter, the renderer loader
  stays the single dynamic-import seam.

## 11. Staging smoke strategy

- This design slice is **docs-only** and does not run a smoke.
- `apps/web/e2e/SMOKE.md` is **deliberately not modified by this
  slice** — the `data-renderer-autofit` selector and the autofit smoke
  rows describe a capability that does not exist in code yet, and
  documenting a non-existent selector would be a stale-selector trap.
  The **implementation slice** (`feat/renderer-neutral-autofit`) adds:
  the `data-renderer-autofit` selector row to SMOKE.md § D's selector
  vocabulary, and an autofit step to the "Resize / fit" matrix row
  (autofit on → resize the viewport → `stty size` tracks the new
  geometry → `data-renderer-autofit="active"` for xterm/wterm,
  `"unsupported"` for ghostty-web).
- The **resmoke** is the already-named follow-on
  `docs/wterm-fit-reflow-resmoke` — a staging resmoke run *once
  behaviour actually changes*, i.e. after the implementation slice
  lands. It re-runs the wterm production-shell resize/fit matrix row
  with autofit enabled and records whether wterm now reflows to a
  narrowed container.

## 12. Security / redaction posture

- **No backend / session / orchestrator / protocol change.** Autofit
  changes the *cell grid the renderer reports*; the wire `resize`
  frame it produces flows through the **existing** `onResize →
  client.sendResize` seam. No new wire message, no protocol variant.
- **No CSP change.** A `ResizeObserver` needs no CSP relaxation.
- **No payload exposure.** `autofit` is a boolean; `autofitActive()`
  returns a boolean; `data-renderer-autofit` is closed-vocabulary
  taxonomy. None of them read, log, or carry terminal input/output,
  paste content, identities, or session tokens. The adapter redaction
  rule (no `console.*`, no payload bytes in thrown errors, no
  neutral-knob echo into the underlying options blob) is **unchanged**
  and re-pinned by the extended sentinel tests.
- **No persisted server-side preference.** `autofitEnabled` lives in
  the same local-only browser `localStorage` snapshot as every other
  terminal preference. It is never sent to or stored by the backend.
- **No new renderer-internals reach.** xterm uses its public
  `FitAddon`; wterm uses its public `autoResize` constructor option;
  the design adds **no** new private-field structural cast (the wterm
  adapter keeps only its existing narrow `focusTarget()` cast).

## 13. Explicit non-goals

- **No synchronous neutral `fit()`.** The xterm-only `fit()` stays
  xterm-only and off the neutral interface.
- **No renderer promotion.** xterm stays the default and the
  compatibility baseline on every surface; the experimental gate
  posture is unchanged.
- **No backend / session / orchestrator / protocol / CSP / CI /
  deploy change.**
- **No mid-session autofit toggle.** Autofit is resolved per attach,
  like renderer selection and cosmetic settings.
- **No ghostty-web / restty fit implementation.** They no-op autofit
  honestly; growing a real fit path for either is its own future
  slice.
- **No mobile / Tauri smoke.** The Android-WebView smoke that would
  *verify* wterm's mobile UX stays the separately-named follow-on
  (`docs/wterm-mobile-smoke-plan`).
- **No custom per-server-profile or per-account autofit override.**
  Local-only browser preference, full stop.
- **No `safeFit()` removal.** The one-shot path stays for the manual
  button; only its enablement/copy becomes `autofitActive()`-informed.

## 14. Next implementation slice

**`feat/renderer-neutral-autofit`** — feature branch. It changes a
shared interface (`terminal-core`'s `TerminalRenderer` /
`BaseTerminalRendererOptions`) and touches every adapter package plus
the production web shell, so it is branch-worthy per the Git workflow.
It touches **no** backend / protocol / session / orchestrator code.

Scope:

1. `terminal-core` — add `BaseTerminalRendererOptions.autofit?` and
   `TerminalRenderer.autofitActive?()`.
2. `XtermRenderer` — `ResizeObserver` + `FitAddon` wiring behind
   `autofit`; `autofitActive()`.
3. `WtermRenderer` / `toWtermOptions` — map `autofit` →
   `WTermOptions.autoResize` with the `wtermOnly.autoResize`
   precedence rule; `autofitActive()`.
4. `GhosttyWebRenderer` / `ResttyRenderer` — accept-and-drop `autofit`;
   `autofitActive()` → `false` (or omit).
5. `terminalSettings.ts` — `autofitEnabled` field + v1→v2 storage-key
   migration; `settingsToRendererOptions` mapping.
6. `SettingsView.svelte` — the local-only autofit checkbox.
7. `ProductionTerminal.svelte` — `data-renderer-autofit` attribute;
   Fit-button enablement/copy driven by `autofitActive()`.
8. `terminalLaunch.ts` — the Fit-button enablement helper.
9. Tests per § 10.
10. `apps/web/e2e/SMOKE.md` — `data-renderer-autofit` selector row +
    autofit matrix step (§ 11).
11. `docs/spec/terminal-adapters.md` — replace the "design is its own
    slice" pointers with the implemented contract per adapter.

Follow-on: **`docs/wterm-fit-reflow-resmoke`** — staging resmoke once
the behaviour actually changes (§ 11).

## See also

- [`docs/spec/terminal-adapters.md`](spec/terminal-adapters.md) — the
  four renderer-adapter contracts; § "Resize / fit / reflow —
  investigation findings (2026-05-14j)" is the evidence base for this
  design.
- [`docs/renderer-comparison-scorecard.md`](renderer-comparison-scorecard.md)
  — § 7 names this design as the ranked next slice.
- [`docs/terminal-renderer-evaluation.md`](terminal-renderer-evaluation.md)
  — Gate 1 / Gate 2 promotion criteria; the open resize/fit decision
  this design feeds.
- [`apps/web/e2e/SMOKE.md`](../apps/web/e2e/SMOKE.md) § "D. Renderer
  evaluation smoke" — the matrix the implementation slice extends.
