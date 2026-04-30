/**
 * Renderer-neutral option/theme/cursor shapes shared by every adapter
 * implementing `TerminalRenderer`.
 *
 * The architectural rule from the project AGENTS.md applies here: this
 * module stays renderer-neutral. Anything xterm-specific, ghostty-web-
 * specific, restty-specific, or wterm-specific belongs behind that
 * adapter's own `<renderer>Only` escape hatch — never in this file. If
 * you find yourself adding a knob "because three of four renderers
 * happen to honour it," push it back into the adapters as a local
 * extension instead. The counterpart rule: if three of four adapters
 * duplicate the same local type verbatim, it belongs HERE — that's
 * exactly the duplication this module exists to prevent.
 *
 * This is configuration shape only. Persistence/storage of user
 * preferences is a separate slice and is not promised by these types.
 */

/**
 * Renderer-neutral cursor styles. Adapters map these to whatever shape
 * their underlying renderer expects (xterm's `block | underline | bar`,
 * ghostty-web's matching tokens, CSS-driven cursor classes for wterm,
 * etc.). Renderers without a portable analogue are free to drop the
 * field during option mapping.
 */
export type RendererCursorStyle = "block" | "underline" | "bar";

/**
 * 16-slot ANSI palette, named rather than indexed. The named shape is
 * intentionally the lowest-common-denominator across adapter packages
 * even when an underlying renderer has no concept of an indexed palette
 * (the field is then dropped during mapping). The named form is
 * preferred over a `readonly string[]` because it's self-documenting at
 * the call site and survives editor renames.
 */
export interface RendererThemeAnsi {
  black?: string;
  red?: string;
  green?: string;
  yellow?: string;
  blue?: string;
  magenta?: string;
  cyan?: string;
  white?: string;
  brightBlack?: string;
  brightRed?: string;
  brightGreen?: string;
  brightYellow?: string;
  brightBlue?: string;
  brightMagenta?: string;
  brightCyan?: string;
  brightWhite?: string;
}

/**
 * Renderer-neutral theme. Adapters map these onto their renderer's own
 * theme type during option translation; the consumer never sees a
 * renderer-specific theme shape.
 */
export interface RendererTheme extends RendererThemeAnsi {
  background?: string;
  foreground?: string;
  cursor?: string;
  selectionBackground?: string;
}

/**
 * Lowest-common-denominator option set every renderer adapter accepts
 * on its public surface. Adapters extend this with an adapter-local
 * `<renderer>Only` escape hatch for knobs that have no portable
 * analogue.
 *
 * Fields a renderer cannot honour are still accepted on the neutral
 * surface (so a single option object can flow into any adapter) and
 * silently dropped during the adapter's option mapping. Each adapter
 * documents which fields it actually honours.
 */
export interface BaseTerminalRendererOptions {
  fontFamily?: string;
  fontSize?: number;
  /**
   * Multiplier on the line box. Honoured by adapters whose renderer has
   * a line-height knob; dropped where the renderer styles via CSS or
   * has no analogue.
   */
  lineHeight?: number;
  cursorStyle?: RendererCursorStyle;
  cursorBlink?: boolean;
  /** Visible scrollback in lines. */
  scrollbackLines?: number;
  theme?: RendererTheme;
}
