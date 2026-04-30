/**
 * Renderer-neutral options for the restty adapter.
 *
 * The shared option/theme/cursor types live in
 * `@relayterm/terminal-core`; this adapter extends
 * `BaseTerminalRendererOptions` with a local `resttyOnly` escape hatch
 * for restty-specific knobs that have no portable analogue.
 *
 * restty's xterm-compatibility shim (`restty/xterm`) exposes a `Terminal`
 * with an xterm-style `TerminalOptions` bag — `cols`, `rows`, plus an
 * arbitrary string-keyed remainder forwarded to the underlying `Restty`
 * instance for migration ergonomics. We deliberately do NOT honour
 * xterm-only knobs such as `cursorStyle` / `cursorBlink` / `scrollback` /
 * `fontSize` / `fontFamily` / `theme` through this surface: the restty
 * shim does not interpret them, and pretending it did would lie about
 * cross-renderer parity. The neutral fields are accepted (so a single
 * shared option object can flow into any adapter) but on this adapter
 * they are dropped during the option mapping. Theme application against
 * restty goes through the native `applyTheme` API which is out of scope
 * for this slice.
 */

import type { BaseTerminalRendererOptions } from "@relayterm/terminal-core";

/**
 * Portable option set the restty adapter accepts. Equals the shared
 * `BaseTerminalRendererOptions` plus the local restty-only escape
 * hatch. The shared cosmetic fields are accepted for cross-adapter
 * shape-compatibility and silently dropped during the option mapping —
 * see file header.
 */
export interface ResttyRendererOptions extends BaseTerminalRendererOptions {
  /**
   * Adapter-local escape hatch for restty-only options that have no
   * portable analogue. DO NOT put portable knobs here — extend the
   * shared neutral surface in `terminal-core` instead. Anything set via
   * this hatch is explicitly NOT promised to behave the same on a
   * different renderer adapter.
   *
   * Typed as a loose record because restty's xterm-compat
   * `TerminalOptions` is itself open-ended (`[key: string]: unknown`);
   * leaking the restty type here would re-introduce a restty shape into
   * the consumer API surface, defeating the renderer-neutral rule.
   */
  resttyOnly?: Record<string, unknown>;
}

/**
 * Initial cell grid for the restty `Terminal`. Kept separate from the
 * neutral option bag because the neutral options surface stays purely
 * renderer-cosmetic; cell-grid sizing belongs on the constructor next to
 * it where the lab can pass a numeric `cols`/`rows`.
 */
export interface ResttyInitialGrid {
  cols?: number;
  rows?: number;
}

/**
 * Map the neutral option object into restty's xterm-compat
 * `TerminalOptions` bag. The restty xterm shim accepts any keys via its
 * `[key: string]: unknown` index signature but only interprets a small
 * `Restty`-specific subset internally — the neutral knobs that have no
 * analogue (font/cursor/theme/scrollback) are dropped here so we don't
 * stuff them into the options blob and pretend they did anything.
 *
 * Override precedence (deliberate): `initialGrid.cols` / `rows` are
 * written first; the `resttyOnly` escape hatch is merged on top last,
 * so a caller passing `resttyOnly: { cols: 132 }` deliberately
 * overrides the programmatic grid. The escape hatch is the explicit
 * "I know what I'm doing" knob — the alternative (grid wins) would
 * silently swallow a deliberate `cols` override and force the caller
 * to construct the renderer with `cols: 132` on the neutral surface
 * instead, which contradicts the "the neutral surface drops cols/rows
 * except via initialGrid" rule. If a future caller needs to set
 * cols/rows from a hot path without unsetting the grid, extend the
 * neutral surface — don't push them through `resttyOnly`.
 *
 * Returned shape is a plain object with optional `cols` / `rows`. The
 * caller hands it to restty's `Terminal` constructor along with the
 * cell grid; the adapter is the only place that imports any restty type.
 */
export function toResttyOptions(
  opts: ResttyRendererOptions,
  initialGrid: ResttyInitialGrid = {},
): Record<string, unknown> {
  const mapped: Record<string, unknown> = {};
  if (initialGrid.cols !== undefined) mapped.cols = initialGrid.cols;
  if (initialGrid.rows !== undefined) mapped.rows = initialGrid.rows;
  if (opts.resttyOnly !== undefined) {
    Object.assign(mapped, opts.resttyOnly);
  }
  return mapped;
}
