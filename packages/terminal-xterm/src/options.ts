/**
 * Renderer-neutral options for the xterm.js adapter.
 *
 * Two rules govern this surface:
 *  1. Adapter consumers see ONLY the renderer-neutral shapes from
 *     `@relayterm/terminal-core` plus the local `xtermOnly` escape
 *     hatch — never xterm's `ITerminalOptions` or `ITheme`. The shared
 *     option/theme/cursor types live in `terminal-core` so every
 *     adapter package speaks the same surface.
 *  2. xterm-specific knobs that have no portable analogue (e.g. WebGL
 *     fallback policy, addon enable flags, `allowProposedApi`) live
 *     behind `xtermOnly` rather than in the neutral base. Callers that
 *     reach for them are explicitly stepping outside the portable
 *     surface — `xtermOnly` is NOT promised to behave the same on a
 *     different renderer adapter.
 */
import type {
  BaseTerminalRendererOptions,
  RendererTheme,
} from "@relayterm/terminal-core";
import type { ITerminalOptions, ITheme } from "@xterm/xterm";

/**
 * The portable option set the xterm adapter honours. Equals the shared
 * `BaseTerminalRendererOptions` plus the local xterm-only escape hatch.
 * Any renderer-neutral knob added here in the future belongs in
 * `terminal-core`'s shared surface, not this file.
 */
export interface XtermRendererOptions extends BaseTerminalRendererOptions {
  /**
   * Adapter-local escape hatch for xterm-only options that have no
   * portable analogue. DO NOT put portable knobs here — extend the
   * shared neutral surface in `terminal-core` instead. Anything set via
   * this hatch is explicitly NOT promised to behave the same on a
   * future renderer.
   */
  xtermOnly?: ITerminalOptions;
}

/**
 * Map the neutral option object onto xterm's `ITerminalOptions`. Only
 * keys the caller actually set are forwarded; xterm decides defaults
 * for the rest. The escape-hatch `xtermOnly` block is merged AFTER the
 * portable mapping so callers can override mapped fields if they have
 * a hard reason to.
 */
export function toXtermOptions(opts: XtermRendererOptions): ITerminalOptions {
  const mapped: ITerminalOptions = {};
  if (opts.fontFamily !== undefined) mapped.fontFamily = opts.fontFamily;
  if (opts.fontSize !== undefined) mapped.fontSize = opts.fontSize;
  if (opts.lineHeight !== undefined) mapped.lineHeight = opts.lineHeight;
  if (opts.cursorStyle !== undefined) mapped.cursorStyle = opts.cursorStyle;
  if (opts.cursorBlink !== undefined) mapped.cursorBlink = opts.cursorBlink;
  if (opts.scrollbackLines !== undefined) mapped.scrollback = opts.scrollbackLines;
  if (opts.theme !== undefined) mapped.theme = toXtermTheme(opts.theme);
  if (opts.xtermOnly !== undefined) {
    // Last-write-wins: any key present in `xtermOnly` overrides the
    // portable mapping above. In particular, `xtermOnly.theme` will
    // replace whatever `toXtermTheme(opts.theme)` produced — that's
    // the explicit point of the escape hatch, but worth flagging
    // because callers who set both will silently lose `opts.theme`.
    Object.assign(mapped, opts.xtermOnly);
  }
  return mapped;
}

export function toXtermTheme(theme: RendererTheme): ITheme {
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
