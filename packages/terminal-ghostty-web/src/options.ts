/**
 * Renderer-neutral options for the ghostty-web adapter.
 *
 * The shared option/theme/cursor types live in
 * `@relayterm/terminal-core`; this adapter extends
 * `BaseTerminalRendererOptions` with a local `ghosttyOnly` escape hatch
 * for ghostty-web-specific knobs that have no portable analogue.
 *
 * NOTE: ghostty-web exposes an `ITerminalOptions` shape that happens to
 * be near-identical to xterm.js — same `cursorStyle`, `cursorBlink`,
 * `theme`, `scrollback`, `fontSize`, `fontFamily` keys. We don't
 * re-export ghostty-web types at the package boundary; consumers see
 * only `GhosttyWebRendererOptions`. The conversion helper is internal
 * by the same rule that keeps `toXtermOptions` adapter-private.
 *
 * `lineHeight` has no analogue in ghostty-web's `ITerminalOptions`;
 * the field is accepted on the shared neutral surface and silently
 * dropped when mapping. This is documented behaviour of the
 * experimental adapter, not a regression.
 */
import type {
  BaseTerminalRendererOptions,
  RendererTheme,
} from "@relayterm/terminal-core";
import type { ITerminalOptions, ITheme } from "ghostty-web";

/**
 * Portable option set the ghostty-web adapter honours. Equals the shared
 * `BaseTerminalRendererOptions` plus the local ghostty-only escape
 * hatch. `lineHeight` is accepted but silently ignored — see file
 * header.
 */
export interface GhosttyWebRendererOptions extends BaseTerminalRendererOptions {
  /**
   * Adapter-local escape hatch for ghostty-web-only options that have
   * no portable analogue. DO NOT put portable knobs here — extend the
   * shared neutral surface in `terminal-core` instead. Anything set via
   * this hatch is explicitly NOT promised to behave the same on a
   * different renderer adapter.
   */
  ghosttyOnly?: ITerminalOptions;
}

/**
 * Map the neutral option object onto ghostty-web's `ITerminalOptions`.
 * Only keys the caller actually set are forwarded. The escape-hatch
 * `ghosttyOnly` block is merged AFTER the portable mapping so callers
 * can override mapped fields if they have a hard reason to.
 */
export function toGhosttyOptions(
  opts: GhosttyWebRendererOptions,
): ITerminalOptions {
  const mapped: ITerminalOptions = {};
  if (opts.fontFamily !== undefined) mapped.fontFamily = opts.fontFamily;
  if (opts.fontSize !== undefined) mapped.fontSize = opts.fontSize;
  if (opts.cursorStyle !== undefined) mapped.cursorStyle = opts.cursorStyle;
  if (opts.cursorBlink !== undefined) mapped.cursorBlink = opts.cursorBlink;
  if (opts.scrollbackLines !== undefined) {
    mapped.scrollback = opts.scrollbackLines;
  }
  if (opts.theme !== undefined) mapped.theme = toGhosttyTheme(opts.theme);
  if (opts.ghosttyOnly !== undefined) {
    Object.assign(mapped, opts.ghosttyOnly);
  }
  return mapped;
}

export function toGhosttyTheme(theme: RendererTheme): ITheme {
  const out: ITheme = {};
  if (theme.background !== undefined) out.background = theme.background;
  if (theme.foreground !== undefined) out.foreground = theme.foreground;
  if (theme.cursor !== undefined) out.cursor = theme.cursor;
  if (theme.selectionBackground !== undefined) {
    out.selectionBackground = theme.selectionBackground;
  }
  if (theme.black !== undefined) out.black = theme.black;
  if (theme.red !== undefined) out.red = theme.red;
  if (theme.green !== undefined) out.green = theme.green;
  if (theme.yellow !== undefined) out.yellow = theme.yellow;
  if (theme.blue !== undefined) out.blue = theme.blue;
  if (theme.magenta !== undefined) out.magenta = theme.magenta;
  if (theme.cyan !== undefined) out.cyan = theme.cyan;
  if (theme.white !== undefined) out.white = theme.white;
  if (theme.brightBlack !== undefined) out.brightBlack = theme.brightBlack;
  if (theme.brightRed !== undefined) out.brightRed = theme.brightRed;
  if (theme.brightGreen !== undefined) out.brightGreen = theme.brightGreen;
  if (theme.brightYellow !== undefined) out.brightYellow = theme.brightYellow;
  if (theme.brightBlue !== undefined) out.brightBlue = theme.brightBlue;
  if (theme.brightMagenta !== undefined) out.brightMagenta = theme.brightMagenta;
  if (theme.brightCyan !== undefined) out.brightCyan = theme.brightCyan;
  if (theme.brightWhite !== undefined) out.brightWhite = theme.brightWhite;
  return out;
}
