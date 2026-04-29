/**
 * `@relayterm/terminal-xterm` — xterm.js-backed implementation of the
 * `TerminalRenderer` interface from `@relayterm/terminal-core`.
 *
 * xterm.js is the compatibility baseline only. The architectural rule
 * lives in `terminal-core`: the protocol and the session client never
 * see xterm types. Every export from this package is renderer-neutral
 * at the interface level, even though the implementation is xterm-shaped.
 *
 * For the side-effect stylesheet (xterm's baseline CSS), import
 * `@relayterm/terminal-xterm/styles` separately. It is intentionally
 * not re-exported here so that Node consumers (vitest, future SSR) can
 * import the renderer without bundler help.
 */

export { XtermRenderer } from "./XtermRenderer.js";
export {
  type RendererCursorStyle,
  type RendererTheme,
  type RendererThemeAnsi,
  type XtermRendererOptions,
} from "./options.js";

// `toXtermOptions` / `toXtermTheme` are deliberately NOT re-exported.
// They return xterm-shaped types (`ITerminalOptions`, `ITheme`) which would
// re-introduce xterm-specific shapes into the consumer's API surface,
// defeating the renderer-neutral rule. Tests import them via the relative
// path (`./options.js`); future renderer adapters should NOT be reaching
// for them at all.
