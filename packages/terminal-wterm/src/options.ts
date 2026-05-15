/**
 * Renderer-neutral options for the wterm adapter.
 *
 * The shared option/theme/cursor types live in
 * `@relayterm/terminal-core`; this adapter extends
 * `BaseTerminalRendererOptions` with a local `wtermOnly` escape hatch
 * for wterm-specific knobs that have no portable analogue.
 *
 * wterm@0.2.x's `WTermOptions` accepts a small fixed surface — `cols`,
 * `rows`, `wasmUrl`, `autoResize`, `cursorBlink`, `debug`, plus
 * `onData`/`onTitle`/`onResize` callbacks. The cosmetic neutral knobs
 * (`fontFamily`, `fontSize`, `lineHeight`, `cursorStyle`, `scrollbackLines`,
 * `theme`) are styled via CSS custom properties on the `.wterm` host
 * element (`--term-font-family`, `--term-font-size`, etc. — see
 * `@wterm/dom/src/terminal.css`) rather than constructor arguments. The
 * adapter therefore accepts the shared neutral surface for cross-renderer
 * parity and silently drops cosmetic fields during the option mapping;
 * theming goes through CSS, not the WTerm constructor.
 */

import type { BaseTerminalRendererOptions } from "@relayterm/terminal-core";

/**
 * Adapter-local escape hatch for wterm's non-portable knobs. Kept in a
 * separate type so the neutral surface stays renderer-neutral; consumers
 * who need these knobs reach for them deliberately. None of these are
 * forwarded to a different renderer if the caller swaps adapters.
 */
export interface WtermOnlyOptions {
  /**
   * Override URL the underlying `WasmBridge` fetches the WASM module
   * from. `@wterm/core@0.2.1` inlines the WASM as a base64 module so
   * this is `undefined` by default. Provide a URL only if a future
   * deployment serves the WASM as a separate asset.
   */
  wasmUrl?: string;
  /**
   * `WTerm` defaults this to `true` and attaches a `ResizeObserver` to
   * the host element that auto-fits the cell grid to the container.
   * The adapter defaults it to `false` so the caller drives sizing
   * explicitly via `renderer.resize(cols, rows)` — matching the
   * `XtermRenderer`, `GhosttyWebRenderer`, and `ResttyRenderer`
   * convention. Set this `true` when you want viewport-driven autofit.
   */
  autoResize?: boolean;
  /**
   * Enables the wterm DebugAdapter. Off by default; the adapter does
   * NOT surface debug data through any public API. Forwarded only as
   * a wtermOnly knob for dev-lab experimentation. Do NOT enable this in
   * a path that captures terminal input or output for redaction
   * reasons — debug traces include the bytes the bridge processed.
   */
  debug?: boolean;
}

/**
 * Portable option set the wterm adapter accepts. Equals the shared
 * `BaseTerminalRendererOptions` plus the local wterm-only escape
 * hatch. The shared cosmetic fields without a wterm analogue are
 * accepted for shape-compatibility and silently dropped during the
 * option mapping — see file header. Theming for wterm goes through CSS
 * variables on the `.wterm` host, not through constructor arguments.
 *
 * `cursorBlink` IS honoured by wterm — it's the one cosmetic knob
 * wterm consumes via the constructor (it toggles a CSS class on the
 * host).
 */
export interface WtermRendererOptions extends BaseTerminalRendererOptions {
  /**
   * Adapter-local escape hatch for wterm-only options that have no
   * portable analogue. DO NOT put portable knobs here — extend the
   * shared neutral surface in `terminal-core` instead. Anything set via
   * this hatch is explicitly NOT promised to behave the same on a
   * different renderer adapter.
   */
  wtermOnly?: WtermOnlyOptions;
}

/**
 * Initial cell grid for the wterm `WTerm`. Kept separate from the
 * neutral option bag because the neutral options surface stays purely
 * renderer-cosmetic; cell-grid sizing belongs on the constructor next to
 * it where the lab can pass a numeric `cols`/`rows`.
 */
export interface WtermInitialGrid {
  cols?: number;
  rows?: number;
}

/**
 * Shape of the option bag the adapter forwards to wterm's `WTerm`
 * constructor. Mirrors `WTermOptions` minus the callback fields; the
 * adapter wires `onData`/`onResize` itself in `mount`. `onTitle` is not
 * surfaced — wterm only fires it during `_doRender` and the adapter
 * does not promise a title-change channel on the renderer-neutral
 * interface.
 *
 * Default precedence (deliberate):
 *   - `cols` / `rows` come from `initialGrid`. wterm itself defaults
 *     these to 80×24 if omitted; we forward only what the lab passed.
 *   - `cursorBlink` is honoured because it's the one cosmetic knob
 *     wterm actually consumes via the constructor (it toggles a CSS
 *     class on the host).
 *   - `wtermOnly.autoResize` overrides wterm's default of `true`. The
 *     adapter defaults it to `false` so the caller drives sizing
 *     explicitly — see `WtermOnlyOptions.autoResize`.
 *   - `wtermOnly.wasmUrl` and `wtermOnly.debug` flow through unchanged.
 */
export interface MappedWtermOptions {
  cols?: number;
  rows?: number;
  cursorBlink?: boolean;
  autoResize: boolean;
  wasmUrl?: string;
  debug?: boolean;
}

/**
 * Map the neutral option object into the bag the adapter feeds to
 * `WTerm`'s constructor. Cosmetic fields without a wterm analogue are
 * dropped here so we don't pretend they did anything. The returned
 * object is plain (no class, no `unknown` index signature) — the
 * adapter is the only place that imports any wterm type.
 */
export function toWtermOptions(
  opts: WtermRendererOptions,
  initialGrid: WtermInitialGrid = {},
): MappedWtermOptions {
  const wtermOnly = opts.wtermOnly ?? {};
  // Resolve `autoResize` with three layers of precedence (mount-time,
  // not runtime — wterm's autoResize is decided in `init()`):
  //
  //   1. `wtermOnly.autoResize` — explicit non-portable escape hatch;
  //      wins when set so a caller who reaches for the raw knob is not
  //      silently overridden by the portable setting.
  //   2. `opts.autofit` — the renderer-neutral
  //      `BaseTerminalRendererOptions.autofit` (mount-time, declarative
  //      "keep me fitted to my container"). Honoured when the explicit
  //      hatch is absent.
  //   3. Adapter default `false` — matches the
  //      `XtermRenderer` / `GhosttyWebRenderer` / `ResttyRenderer`
  //      "caller drives sizing" parity, so a renderer-id switch from
  //      one of those to wterm does not silently change behaviour.
  const autoResize =
    wtermOnly.autoResize !== undefined
      ? wtermOnly.autoResize
      : (opts.autofit ?? false);
  const mapped: MappedWtermOptions = { autoResize };
  if (initialGrid.cols !== undefined) mapped.cols = initialGrid.cols;
  if (initialGrid.rows !== undefined) mapped.rows = initialGrid.rows;
  if (opts.cursorBlink !== undefined) mapped.cursorBlink = opts.cursorBlink;
  if (wtermOnly.wasmUrl !== undefined) mapped.wasmUrl = wtermOnly.wasmUrl;
  if (wtermOnly.debug !== undefined) mapped.debug = wtermOnly.debug;
  return mapped;
}
