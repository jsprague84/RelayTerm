# SPEC — Terminal renderer adapters

> Concrete renderer adapter contracts split out of [`terminal.md`](terminal.md)
> for context efficiency. The terminal lifecycle, WebSocket attach/detach,
> wire protocol, replay buffer, live PTY bridge, production terminal UI,
> paste safety, local recovery, and every other renderer-independent
> contract still live in `terminal.md`. This file is the long form for
> the four renderer adapter packages under `packages/terminal-<name>/`.
>
> AGENTS.md governs *how* code is written; this doc governs *what* the
> renderer adapter packages do. Drift from any rule here is a spec bug.
>
> **Production baseline / experimental rule (load-bearing).** xterm.js is
> the **production compatibility baseline** and the production default
> renderer. The other three adapters (ghostty-web, restty, wterm) are
> **experimental and not promoted** — every production attach defaults
> to xterm.
>
> Production shell components MUST NOT statically import any experimental
> renderer adapter. The static-import rule is pinned by
> `apps/web/tests/appShellIsolation.test.ts`.
>
> **Production-shell experimental access path (since 2026-05-13).** The
> production shell may reach an experimental adapter ONLY through the
> gated lazy loader at
> `apps/web/src/lib/app/terminal/rendererLoader.ts`. The loader uses
> `dynamic import()` so Vite/Rollup chunk-splits each experimental
> adapter into its own asset; the default-renderer attach path never
> fetches an experimental WASM payload. The loader instantiates an
> experimental adapter only when (a) the operator has flipped the
> `experimentalRendererEvaluationEnabled` gate in Settings AND (b) the
> operator has picked the matching renderer id. Any other path (gate
> off, unknown id, dynamic-import or constructor failure) collapses to
> xterm with a typed fallback reason surfaced on
> `data-renderer-fallback`. This access path is NOT a promotion of
> ghostty-web / restty / wterm — see
> [`docs/terminal-renderer-evaluation.md`](../terminal-renderer-evaluation.md)
> § "Promotion criteria" for the Gate 1 / Gate 2 path.
>
> The isolation test also pins that the experimental adapter package
> names are referenced ONLY inside the renderer loader file AND only
> inside `dynamic import()` expressions — a static
> `from "@relayterm/terminal-ghostty-web"` line anywhere in
> `apps/web/src/lib/app/` (including the loader file itself) is a
> regression. Renderer-neutral rules (`terminal-core` imports nothing
> renderer-specific; the wire protocol stays RelayTerm-shaped) live in
> [`terminal.md`](terminal.md) § "Frontend terminal-core contract" and
> are re-affirmed below per adapter for forensic clarity.

## Contents

- [xterm.js baseline renderer adapter](#xtermjs-baseline-renderer-adapter)
- [ghostty-web experimental renderer adapter](#ghostty-web-experimental-renderer-adapter)
- [restty experimental renderer adapter](#restty-experimental-renderer-adapter)
- [wterm experimental renderer adapter](#wterm-experimental-renderer-adapter)

---

## xterm.js baseline renderer adapter

`@relayterm/terminal-xterm` is the first concrete `TerminalRenderer` implementation. xterm.js is the **compatibility baseline**, not the architecture: the protocol stays RelayTerm-shaped, the session client never sees xterm types, and the adapter is one of N planned renderers (ghostty-web, restty, wterm, future native/Tauri).

**Scope (load-bearing — this slice).** A successful integration attests ONLY to the renderer interface bridging xterm.js bidirectionally — `mount`/`write`/`focus`/`resize`/`dispose`/`onInput`/`onResize` all flow through xterm cleanly. It does **NOT** mean PTY bytes stream end-to-end (the backend still rejects `input` with `pty_not_implemented`), it does **NOT** include the replay buffer, and the production terminal UI is still not implemented — only a dev-only renderer lab consumes the adapter today.

### Package layout

`packages/terminal-xterm/` is a workspace package alongside `terminal-core`. Its only neighbors today are the protocol/client core; future renderers live as siblings. Keys:

- `src/XtermRenderer.ts` — the only file in the repo that imports `@xterm/xterm`. Implements `TerminalRenderer` from `terminal-core` and exposes a `fit()` helper for callers that own the container.
- `src/options.ts` — `XtermRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). The shared `RendererTheme` shape (background/foreground/cursor/selectionBackground + 16 named ANSI slots) lives in `terminal-core/src/rendererOptions.ts`. A local `xtermOnly` escape hatch passes raw `ITerminalOptions` through and is documented as **non-portable**.
- `src/styles.ts` — side-effect entry that imports `@xterm/xterm/css/xterm.css`. Split out of `index.ts` so Node consumers (vitest) can import the renderer without bundler help. Browser consumers do `import "@relayterm/terminal-xterm/styles"` once at app boot.
- `package.json` declares `"sideEffects": ["./src/styles.ts", "**/*.css"]` so Rollup tree-shakes unused JS in non-dev builds while preserving the styles side-effect import for callers that explicitly want it.

### Adapter contract

- `XtermRenderer` is the **only** xterm.js consumer in the repo. `terminal-core` does not depend on `@xterm/xterm`. `apps/web` depends on `@relayterm/terminal-xterm` (workspace) — never directly on `@xterm/xterm`.
- Constructor takes `XtermRendererOptions` only; the underlying `Terminal` instance is private.
- `mount` is allowed exactly once per renderer instance. Re-mount throws — silent re-attach would mask a misuse. Calls to `write` before `mount` are queued and flushed on mount; calls to `write` after `dispose` are silent no-ops.
- `dispose` is idempotent and tears down the Terminal, addons (FitAddon, WebLinksAddon), the `onData`/`onResize` subscriptions, and the listener sets in one shot.
- A throwing user listener inside `onInput` is caught and dropped — it MUST NOT interrupt sibling listeners or surface the input bytes through the error envelope (the redaction rule is enforced inside the adapter and re-asserted by tests in `tests/xtermRenderer.test.ts`).

### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `@xterm/*` and the protocol stays RelayTerm-shaped, never xterm-shaped.
- `XtermRendererOptions` is the **first** concrete shape future renderer adapters are expected to honor 1:1 for the portable knobs. Renderer-only escape-hatch fields (the `xtermOnly` block) are explicitly NOT promised to behave the same on a future adapter.

### Diagnostic UI

The dev-only live-terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — is the manual exercise surface for the renderer adapter; see [`terminal.md`](terminal.md) § "Live SSH PTY bridge contract → Diagnostic UI" for its contract. The xterm baseline renderer adapter has no separate dev lab — the protocol-only `TerminalProtocolLab` covers the renderer-less wire path, and the live-terminal lab covers the renderer-bridged path. Both labs gate on `import.meta.env.DEV`; the production bundle drops the JS via Rollup tree-shaking (JS bundle is ~28KB without the labs vs. ~322KB with the renderer eagerly included before the `sideEffects` marker landed).

### Future work (explicit out-of-scope for this slice)

Real PTY byte streaming through `output` frames; ghostty-web / restty / wterm renderer adapters; renderer benchmarking harness; persistent per-renderer preferences; production terminal UI; renderer-swap UX; mobile/Tauri shell integration. Each is a separate, deliberate slice.

## ghostty-web experimental renderer adapter

`@relayterm/terminal-ghostty-web` is the second concrete `TerminalRenderer` implementation. It is **experimental** — xterm.js remains the compatibility baseline. The adapter wraps `ghostty-web`, which embeds Ghostty's libghostty-vt parser via WebAssembly and exposes an xterm.js-API-compatible `Terminal` class. Landing this adapter proves the renderer-neutral seam holds end-to-end without backend protocol or `terminal-core` changes.

**Scope (load-bearing — this slice).** A successful integration attests ONLY to:

1. The same `TerminalRenderer` interface from `@relayterm/terminal-core` (`mount` / `write` / `focus` / `resize` / `dispose` / `onInput` / `onResize`) bridges ghostty-web bidirectionally, with the WASM module loaded via `Ghostty.load(wasmUrl)` and resolved before `Terminal` construction.
2. `apps/web`'s dev-only live terminal lab can switch between xterm baseline and ghostty-web experimental at runtime; switching disposes the previous renderer and remounts the new one without tearing down the `TerminalSessionClient` or the wire protocol.
3. The backend protocol, the session client, and `terminal-core` remain unchanged and renderer-neutral.

It does **NOT** yet:

- Replace xterm as the production renderer. The production terminal UI is still not built; the dev lab is the only consumer.
- Persist a per-renderer preference. The lab defaults to xterm on every page load.
- Validate ghostty-web behavior in jsdom. Vitest exercises the adapter against a mocked `ghostty-web` module — the real WASM runtime is verified only in a browser dev session. The mock pins option mapping, init memoization, the pre-mount write queue, idempotent dispose, the dispose-during-pending-mount cancellation path, and the input-redaction rule.

### Package layout

`packages/terminal-ghostty-web/` is a workspace package alongside `terminal-core` and `terminal-xterm`. Keys:

- `src/GhosttyWebRenderer.ts` — the only file in the repo that imports `ghostty-web`. Implements `TerminalRenderer`. `mount` is async because the WASM module must be compiled and instantiated before any `Terminal` can be constructed; the loaded `Ghostty` instance promise is memoized at module scope so multiple renderer instances share one load.
- `src/wasmUrl.ts` — imports `ghostty-web/ghostty-vt.wasm?url` and re-exports the resulting same-origin asset URL as `ghosttyWasmUrl`. Vite's `?url` plugin copies the upstream package's sibling `.wasm` file (exposed by ghostty-web's `exports` map at `./ghostty-vt.wasm`) into the production build's `dist/assets/` directory with a fingerprinted filename and substitutes the URL string at build time. The adapter calls `Ghostty.load(ghosttyWasmUrl)` directly and passes the resulting instance into `new Terminal({ ghostty })` so upstream's no-arg `init()` sugar — the only path that consumes the inlined `data:application/wasm;base64,…` URL — is never reached. The CSP rationale is on the file header in `GhosttyWebRenderer.ts` and pinned by `tests/wasmAssetSource.test.ts`. A `vite-shims.d.ts` ambient declares the `?url` and `?raw` import suffixes so `tsc` can typecheck the adapter without dragging in the full `vite/client` surface.
- `src/options.ts` — `GhosttyWebRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). `lineHeight` has no analogue in ghostty-web's `ITerminalOptions` and is silently dropped during the option mapping; this is documented adapter behavior, not a regression. A local `ghosttyOnly` escape hatch passes raw ghostty-web options through and is documented as **non-portable**.
- `package.json` declares `"sideEffects": false`. Combined with the `sideEffects: false` marker on this adapter, the production `apps/web` bundle tree-shakes both ghostty-web and this adapter on any code path that doesn't reach the dynamically-imported adapter. The upstream package still inlines its WASM payload as a base64 data URL inside its shipped JS bundle, but with the asset-URL load path above that branch is dead code at runtime — it never `fetch`es, never reaches `WebAssembly.compile`, and never triggers a `connect-src` CSP rejection.

### Adapter contract

- `GhosttyWebRenderer` is the **only** `ghostty-web` consumer in the repo. `terminal-core` does not depend on `ghostty-web`. `terminal-xterm` does not depend on `ghostty-web`. `apps/web` depends on `@relayterm/terminal-ghostty-web` (workspace) — never directly on `ghostty-web`.
- Constructor takes `GhosttyWebRendererOptions` only; the underlying `Terminal` instance is private.
- `mount` is `async`. Calling it more than once on a live renderer rejects with `already mounted`. Calling it after `dispose` rejects with `cannot mount after dispose`. A synchronous `dispose()` issued **during** the awaited `Ghostty.load(wasmUrl)` cancels the open silently — no `Terminal` is constructed and no DOM is touched after disposal.
- `write` before `mount` queues; the queue is flushed on `mount` resolution. `write` after `dispose` is a silent no-op.
- `dispose` is synchronous and idempotent. It tears down the WASM-backed `Terminal`, the `onData`/`onResize` subscriptions, the pre-mount write queue, and the listener sets. The shared `Ghostty` WASM module stays loaded — re-disposing it would tear it out from under any other live `Terminal` on the page.
- A throwing user listener inside `onInput` is caught and dropped, identical to `XtermRenderer` — it MUST NOT interrupt sibling listeners or surface the input bytes through the error envelope. `tests/ghosttyWebRenderer.test.ts` pins the redaction rule with the same sentinel-string approach as the xterm adapter.

### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `ghostty-web` (or `@xterm/*`).
- `GhosttyWebRendererOptions` is shape-compatible with `XtermRendererOptions` for the portable knobs, so an app can swap renderers by changing only the import. Renderer-only escape-hatch fields (`xtermOnly`, `ghosttyOnly`) are explicitly NOT promised to behave the same across adapters.
- The wire protocol stays RelayTerm-shaped. A live PTY's `Output` bytes hand identical payloads to either renderer; `Input` flows back through the same `TerminalSessionClient`.

### Diagnostic UI

The dev-only live terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — exposes a `renderer:` radio group switching between xterm baseline (default) and ghostty-web experimental. Switching while attached tears down the current renderer and `TerminalSessionClient` and immediately reconnects with the new renderer; switching while idle records the choice for the next `connect()`. The event log records ONLY the renderer name on switch — no payload bytes. The redaction rules pinned by `apps/web/tests/labLog.test.ts`, `tests/xtermRenderer.test.ts`, and `tests/ghosttyWebRenderer.test.ts` continue to hold across renderer switches.

### Production bundle behavior

The production `apps/web` build (`pnpm -r build`) emits the xterm baseline in the main entry chunk and splits each experimental adapter (ghostty-web / restty / wterm) into its own lazy chunk via the gated dynamic `import()`s in `rendererLoader.ts`. The default-renderer attach path therefore never fetches an experimental WASM payload, even though the adapter packages are workspace dependencies of `apps/web`. Both adapter packages declare `sideEffects: false` (xterm pins only `./src/styles.ts` and `**/*.css` as side-effectful). Only the xterm CSS side-effect import remains in the prod CSS bundle; ghostty-web ships no CSS so its adapter contributes nothing to the styles bundle. For ghostty-web specifically: the upstream `dist/ghostty-web.js` still inlines its WASM payload as a `data:application/wasm;base64,…` URL in the no-arg `Ghostty.load()` branch, and Rollup cannot statically prove that branch is unreachable, so the literal sits in the lazy ghostty-web chunk as dead code; the live code path imports `ghostty-web/ghostty-vt.wasm?url` and Vite emits the bytes as a separately-fetched fingerprinted asset (e.g. `dist/assets/ghostty-vt-<hash>.wasm`) the adapter then hands to `Ghostty.load(wasmUrl)`. The runtime never reaches the data URL.

### Future work (explicit out-of-scope for this slice)

Production terminal UI; persistent per-renderer preference; renderer benchmarking harness; mobile/Tauri shell integration of the experimental renderer; jsdom/headless-browser verification of the real ghostty-web WASM runtime. Each is a separate, deliberate slice.

## restty experimental renderer adapter

`@relayterm/terminal-restty` is the third concrete `TerminalRenderer` implementation. It is **experimental** — xterm.js remains the compatibility baseline; `@relayterm/terminal-ghostty-web` remains the libghostty-vt-via-WASM experiment; this adapter wraps `restty` (npm `restty@0.1.x`), a more ambitious modern renderer powered by libghostty-vt (WASM), WebGPU/WebGL2, and TypeScript text shaping. Landing this adapter proves a substantively different renderer experiment can drop in behind the renderer-neutral seam without backend protocol or `terminal-core` changes.

**Scope (load-bearing — this slice).** A successful integration attests ONLY to:

1. The same `TerminalRenderer` interface from `@relayterm/terminal-core` (`mount` / `write` / `focus` / `resize` / `dispose` / `onInput` / `onResize`) bridges restty's `restty/xterm` compatibility shim bidirectionally.
2. `apps/web`'s dev-only live terminal lab can switch between xterm baseline (default), ghostty-web experimental, and restty experimental at runtime; switching disposes the previous renderer and remounts the new one without tearing down the wire protocol.
3. The backend protocol, the session client, and `terminal-core` remain unchanged and renderer-neutral.

It does **NOT** yet:

- Replace xterm as the production renderer. The production terminal UI is still not built; the dev lab is the only consumer.
- Persist a per-renderer preference. The lab defaults to xterm on every page load.
- Validate restty behavior in jsdom. Vitest exercises the adapter against a mocked `restty/xterm` module — the real WASM/WebGPU runtime is verified only in a browser dev session. The mock pins option mapping, the pre-mount write queue, idempotent dispose, the dispose-during-pending-mount cancellation path, the UTF-8 decode of `Uint8Array` writes, and the input-redaction rule.
- Honor restty's native pane / plugin / shader-stage surface. The adapter binds to the focused `restty/xterm` compatibility shim, not the full `Restty` class. Promoting any of those surfaces is future work.

### Package layout

`packages/terminal-restty/` is a workspace package alongside `terminal-core`, `terminal-xterm`, and `terminal-ghostty-web`. Keys:

- `src/ResttyRenderer.ts` — the only file in the repo that imports from `restty`. Implements `TerminalRenderer`. Binds against `restty/xterm`'s `Terminal` class for shape-parity with the existing adapters; restty's WASM/WebGPU runtime initializes lazily inside the underlying `Restty` instance the first time `Terminal.open` is called. `mount` is `async` for parity with the ghostty-web adapter and to give restty room to grow into a future async init step without changing the adapter contract.
- `src/options.ts` — `ResttyRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). The `restty/xterm` shim does not interpret these cosmetic knobs (the underlying `Restty` exposes `setFontSize` / `setLigatures` / `applyTheme` etc. as native APIs); the adapter accepts them on the neutral surface for cross-adapter shape-parity and silently drops them during the option mapping. Honoring them via `Restty`'s native APIs is future work. A local `resttyOnly` escape hatch passes raw restty-compat option keys through and is documented as **non-portable**. An optional `cols` / `rows` initial cell grid is accepted on the constructor and forwarded into the restty `Terminal`.
- `package.json` declares `"sideEffects": false`. restty ships a sizeable WASM/WebGPU payload (~3 MB JS plus an inlined WASM binary); combined with the `sideEffects: false` marker the production `apps/web` bundle tree-shakes both restty and this adapter when the dev lab is dead-code-eliminated. Caveat: restty 0.1.x itself does not declare `sideEffects` in its `package.json`, so if a future code change made the adapter reachable from a non-dev path the WASM payload would land in the prod JS bundle.

### Adapter contract

- `ResttyRenderer` is the **only** `restty` consumer in the repo. `terminal-core` does not depend on `restty`. `terminal-xterm` and `terminal-ghostty-web` do not depend on `restty`. `apps/web` depends on `@relayterm/terminal-restty` (workspace) — never directly on `restty`.
- Constructor takes `ResttyRendererCtorOptions` (the neutral options plus optional `cols` / `rows`); the underlying restty `Terminal` instance is private.
- `mount` is `async`. Calling it more than once on a live renderer rejects with `already mounted`. Calling it after `dispose` rejects with `cannot mount after dispose`. A synchronous `dispose()` issued **during** the awaited microtask cancels the open silently — no `Terminal` is constructed and no DOM is touched after disposal.
- `write` accepts `string | Uint8Array`. `restty/xterm`'s `Terminal.write(data: string)` accepts strings only; the adapter UTF-8-decodes `Uint8Array` payloads with replacement-on-error before forwarding. UTF-8 is the correct decoding for SSH PTY output; a future binary frame format is out of scope here. `write` before `mount` queues; the queue is flushed on `mount` resolution. `write` after `dispose` is a silent no-op.
- `dispose` is synchronous and idempotent. It tears down the underlying `Restty` instance via `Terminal.dispose()` (canvas, IME input, render loop, pane manager), the `onData`/`onResize` subscriptions, the pre-mount write queue, and the listener sets. The restty WASM module itself stays loaded for the page.
- A throwing user listener inside `onInput` is caught and dropped, identical to `XtermRenderer` and `GhosttyWebRenderer` — it MUST NOT interrupt sibling listeners or surface the input bytes through the error envelope. `tests/resttyRenderer.test.ts` pins the redaction rule with the same sentinel-string approach as the sibling adapters.

### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `restty` (or `@xterm/*` / `ghostty-web`).
- `ResttyRendererOptions` is shape-compatible with `XtermRendererOptions` and `GhosttyWebRendererOptions` for the portable knobs, so an app can swap renderers by changing only the import. Renderer-only escape-hatch fields (`xtermOnly`, `ghosttyOnly`, `resttyOnly`) are explicitly NOT promised to behave the same across adapters. Cosmetic knobs (font, cursor, theme, scrollback) are accepted by `ResttyRendererOptions` for shape-parity but silently dropped during the mapping — see "Package layout."
- The wire protocol stays RelayTerm-shaped. A live PTY's `Output` bytes hand identical payloads to all three renderers; `Input` flows back through the same `TerminalSessionClient`.

### Diagnostic UI

The dev-only live terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — exposes a `renderer:` radio group switching between xterm baseline (default), ghostty-web experimental, and restty experimental. Switching while attached tears down the current renderer and `TerminalSessionClient` and immediately reconnects with the new renderer; switching while idle records the choice for the next `connect()`. The event log records ONLY the renderer name on switch — no payload bytes. The redaction rules pinned by `apps/web/tests/labLog.test.ts`, `tests/xtermRenderer.test.ts`, `tests/ghosttyWebRenderer.test.ts`, and `tests/resttyRenderer.test.ts` continue to hold across renderer switches.

### Production bundle behavior

The dev lab is gated behind `import.meta.env.DEV`, which Vite inlines as a constant; Rollup eliminates the dead branch, which makes the `apps/web` imports of `@relayterm/terminal-xterm`, `@relayterm/terminal-ghostty-web`, and `@relayterm/terminal-restty` unreachable. All three adapter packages declare `sideEffects: false` (xterm pins only `./src/styles.ts` and `**/*.css` as side-effectful), so Rollup drops the wrappers, which in turn drops the underlying libraries — xterm.js's parser/renderer, ghostty-web's WASM data URL, and restty's WASM/WebGPU payload. Only the xterm CSS side-effect import remains in the prod CSS bundle; ghostty-web and restty ship no CSS so their adapters contribute nothing to the styles bundle. Caveat: neither ghostty-web 0.4.0 nor restty 0.1.x declares `sideEffects` in its own `package.json`, so if a future code change made either adapter reachable from a non-dev path, the corresponding WASM payload would land in the prod JS bundle.

### Future work (explicit out-of-scope for this slice)

Production terminal UI; persistent per-renderer preference; renderer benchmarking harness; mobile/Tauri shell integration of the experimental renderer; jsdom/headless-browser verification of the real restty WASM/WebGPU runtime; honoring the neutral cosmetic knobs (font, cursor, theme, scrollback) via `Restty`'s native APIs; restty pane / plugin / shader-stage surface integration. Each is a separate, deliberate slice.

## wterm experimental renderer adapter

`@relayterm/terminal-wterm` is the fourth concrete `TerminalRenderer` implementation. It is **experimental** — xterm.js remains the compatibility baseline; `@relayterm/terminal-ghostty-web` and `@relayterm/terminal-restty` remain the two libghostty-vt-based experiments; this adapter wraps `@wterm/dom` (npm `@wterm/dom@0.2.x`, depending transitively on `@wterm/core@0.2.x`), a DOM-rendered terminal emulator with a Zig+WASM core. The adapter is the **DOM/mobile/accessibility-oriented** experiment in the renderer lineup: text selection, copy, paste, IME composition, and mobile soft keyboards flow through the platform's native text-handling primitives because the cell grid renders into ordinary DOM nodes (`.term-row > span`), not a canvas/WebGPU surface. Landing this adapter proves a substantively different rendering style can drop in behind the renderer-neutral seam without backend protocol or `terminal-core` changes.

### Adapter contract

1. The same `TerminalRenderer` interface from `@relayterm/terminal-core` (`mount` / `write` / `focus` / `resize` / `dispose` / `onInput` / `onResize`) bridges wterm's `WTerm` orchestrator bidirectionally.
2. `apps/web`'s dev-only live terminal lab can switch between xterm baseline (default), ghostty-web experimental, restty experimental, and wterm experimental at runtime; switching disposes the previous renderer and remounts the new one without tearing down the wire protocol.
3. The same redaction rule pinned by the sibling adapters (`tests/xtermRenderer.test.ts`, `tests/ghosttyWebRenderer.test.ts`, `tests/resttyRenderer.test.ts`) holds verbatim — no `console.*` in the adapter, no payload bytes inside thrown errors, no neutral-knob echo into the underlying constructor's options blob. `tests/wtermRenderer.test.ts` pins the rule with the same sentinel-string approach.

What this slice does NOT promise:

- A polished terminal UI. The wterm adapter is wired up only inside the dev lab; production builds tree-shake it out.
- Full theming parity with `XtermRenderer`. wterm consumes typography/theme via CSS custom properties on the `.wterm` host element (see `@wterm/dom/src/terminal.css`), not via `WTermOptions`; the adapter accepts the neutral cosmetic knobs (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `scrollbackLines`, `theme`) for cross-renderer shape-parity and silently drops them during the option mapping. `cursorBlink` is the one cosmetic knob that flows through to the `WTerm` constructor.
- Validation of wterm behavior in jsdom. Vitest exercises the adapter against a mocked `@wterm/dom` module — the real WASM/DOM runtime is verified only in a browser dev session. The mock pins option mapping, the pre-mount write queue, the pre-mount latest-resize cache, idempotent dispose, the dispose-during-pending-init cancellation path (which destroys the just-constructed `WTerm` instead of leaking a render loop), the static init-failure error message, and the input-redaction rule.
- Honoring `WTerm`'s `onTitle` callback. Title-change is not a channel on the renderer-neutral interface; the adapter does not wire it. Adding it later is a deliberate change.
- Surfacing wterm's `DebugAdapter` in the dev lab UI. The `wtermOnly.debug` knob passes through to `WTermOptions.debug` for adapter-local experimentation, but enabling it makes wterm's own `DebugAdapter` log render-path traces (including bytes the bridge processed) outside the adapter's redaction surface. The dev lab UI does NOT expose a debug checkbox today; if a future slice adds one, it must NOT be wired into any path that captures real terminal input or output, and the adapter test suite must continue to pin that the adapter itself surfaces zero console output regardless of `debug` value.

### Package layout

`packages/terminal-wterm/` is a workspace package alongside `terminal-core`, `terminal-xterm`, `terminal-ghostty-web`, and `terminal-restty`. Keys:

- `src/WtermRenderer.ts` — the only file in the repo that imports `@wterm/dom`. Implements `TerminalRenderer`. `mount` is async because `WTerm.init()` loads the WASM bridge before the renderer can write or render. The adapter constructs the `WTerm` synchronously inside `mount(element)` (because `WTerm`'s constructor takes the host element and immediately mutates it — appending a child grid div and adding the `.wterm` class) and then awaits `init()` before flushing the pre-mount write queue. A synchronous `dispose()` issued during the awaited `init()` destroys the just-constructed `WTerm` and skips the queue flush.
- `src/options.ts` — `WtermRendererOptions` extends `BaseTerminalRendererOptions` from `@relayterm/terminal-core` (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `cursorBlink`, `scrollbackLines`, `theme`). `cursorBlink` is forwarded to the `WTerm` constructor (it toggles a CSS class on the host); the rest are accepted on the neutral surface for cross-adapter shape-parity and silently dropped during the option mapping. Theming/typography for wterm is documented as going through CSS variables on the `.wterm` host (`--term-fg`, `--term-bg`, `--term-color-{0..15}`, `--term-font-family`, `--term-font-size`, `--term-line-height`, `--term-row-height`) rather than constructor arguments. A local `wtermOnly` escape hatch carries adapter-local knobs (`autoResize`, `wasmUrl`, `debug`) and is documented as **non-portable**. The `autoResize` default flips from wterm's own `true` to `false` on the adapter, so the caller drives sizing explicitly via `renderer.resize(cols, rows)` for parity with xterm/ghostty-web/restty; opt back into wterm's `ResizeObserver`-driven auto-fit by setting `wtermOnly.autoResize: true`. An optional `cols` / `rows` initial cell grid is accepted on the constructor and forwarded into the `WTerm` constructor.
- `package.json` declares `"sideEffects": false`. `@wterm/core@0.2.x` inlines its WASM payload as a base64 module inside the shipped JS (`wasm-inline.js`, ~17 KB), so no separate asset wiring is required for Vite consumers; combined with the `sideEffects: false` marker on this adapter, the production `apps/web` bundle tree-shakes both `@wterm/dom`/`@wterm/core` and this adapter when the dev lab is dead-code-eliminated. Caveat: `@wterm/dom` does not declare `sideEffects` in its own `package.json`, so if a future code change made the adapter reachable from a non-dev path the WASM payload would land in the prod JS bundle.

### Renderer-neutral rule (re-affirmed)

- `terminal-core` still imports nothing from `@wterm/*` (or `@xterm/*`, `ghostty-web`, `restty`).
- `WtermRenderer` is the **only** `@wterm/dom` consumer in the repo. `terminal-core` does not depend on `@wterm/dom`. `terminal-xterm`, `terminal-ghostty-web`, and `terminal-restty` do not depend on `@wterm/dom`. `apps/web` depends on `@relayterm/terminal-wterm` (workspace) — never directly on `@wterm/dom`.
- Constructor takes `WtermRendererCtorOptions` (the neutral options plus optional `cols` / `rows`); the underlying `WTerm` instance is private.
- `write` accepts `string | Uint8Array`. `WTerm.write(data)` accepts both directly via the `WasmBridge` (`writeString` UTF-8-encodes; `writeRaw` takes bytes), so the adapter forwards both shapes unchanged — no UTF-8 decode step inside the adapter (unlike `restty/xterm`). `write` before `mount` queues; the queue is flushed on `mount` resolution. `write` after `dispose` is a silent no-op.
- `dispose` is synchronous and idempotent. It tears down the underlying `WTerm` via `destroy()` (which clears the host element's `innerHTML`, detaches the click listener, disconnects the optional internal `ResizeObserver`, and tears down the `InputHandler`), the pre-mount write queue, the cached pre-mount resize, and the listener sets. The `@wterm/core` WASM module itself stays loaded for the page; that's intentional — re-initialising it would tear shared state out from under any future renderer instance.
- The wire protocol stays RelayTerm-shaped. A live PTY's `Output` bytes hand identical payloads to all four renderers; `Input` flows back through the same `TerminalSessionClient`.

### Diagnostic UI

The dev-only live terminal lab — `apps/web/src/lib/dev/XtermLiveTerminalLab.svelte` — adds a `wterm experimental` choice to the `renderer:` radio group. Switching while attached tears down the current renderer and `TerminalSessionClient` and immediately reconnects with the new renderer; switching while idle records the choice for the next `connect()`. The event log records ONLY the renderer name on switch — no payload bytes. The redaction rules pinned by `apps/web/tests/labLog.test.ts`, `tests/xtermRenderer.test.ts`, `tests/ghosttyWebRenderer.test.ts`, `tests/resttyRenderer.test.ts`, and `tests/wtermRenderer.test.ts` continue to hold across renderer switches. The lab's helper text calls out that wterm's DOM rendering changes the selection / copy-paste / IME / mobile-keyboard model relative to canvas/WebGPU adapters.

### Production bundle behavior

The dev lab is gated behind `import.meta.env.DEV`, which Vite inlines as a constant; Rollup eliminates the dead branch, which makes the `apps/web` imports of `@relayterm/terminal-xterm`, `@relayterm/terminal-ghostty-web`, `@relayterm/terminal-restty`, and `@relayterm/terminal-wterm` unreachable. The xterm and wterm adapter packages pin `./src/styles.ts` and `**/*.css` as side-effectful (because they re-export an upstream CSS file via a dedicated `/styles` entry); ghostty-web and restty declare `sideEffects: false` outright. So Rollup drops the JS wrappers, which in turn drops the underlying libraries — xterm.js's parser/renderer, ghostty-web's WASM data URL, restty's WASM/WebGPU payload, and `@wterm/dom`/`@wterm/core`'s DOM/WASM bundle — and a check of the production bundle shows zero JS references to `WTerm`/`WasmBridge`. The `@relayterm/terminal-wterm/styles` side-effect import is the same documented compromise xterm has: routing the CSS through the adapter package (rather than `@wterm/dom/css` directly) is necessary because pnpm's strict resolver refuses an `apps/web` import of an undeclared transitive dep, and the CSS side-effect itself is not eliminated by Rollup the way the JS branch is. Both xterm's grid sheet and wterm's `.wterm` host stylesheet land in the prod CSS bundle today; ghostty-web and restty ship no CSS so their adapters contribute nothing. Caveat: none of `@xterm/xterm`, ghostty-web 0.4.0, restty 0.1.x, or `@wterm/dom` 0.2.x declare `sideEffects` in their own `package.json`, so if a future code change made any of these adapters reachable from a non-dev path the corresponding payload would land in the prod JS bundle.

### Future work (explicit out-of-scope for this slice)

Production terminal UI; persistent per-renderer preference; renderer benchmarking harness; mobile/Tauri shell integration of the experimental renderer; jsdom/headless-browser verification of the real wterm WASM/DOM runtime; honoring the neutral cosmetic knobs (font, cursor, theme, scrollback) via wterm's CSS custom properties; surfacing wterm's `onTitle` channel; wiring wterm's `DebugAdapter` into the dev lab. Each is a separate, deliberate slice.
