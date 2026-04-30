/**
 * `@relayterm/terminal-ghostty-web` — ghostty-web-backed implementation
 * of the `TerminalRenderer` interface from `@relayterm/terminal-core`.
 *
 * This package is an EXPERIMENTAL renderer adapter. xterm.js (via
 * `@relayterm/terminal-xterm`) is the compatibility baseline; this
 * package proves that the renderer adapter seam holds up under a
 * libghostty-vt-based parser without requiring backend protocol or
 * `terminal-core` changes.
 *
 * The architectural rule lives in `terminal-core`: the protocol and
 * the session client never see ghostty-web types. Every export from
 * this package is renderer-neutral at the interface level, even though
 * the implementation is ghostty-web-shaped.
 *
 * ghostty-web inlines its WASM payload as a base64 data URL inside the
 * shipped JS bundle, so the package is self-contained — no separate
 * asset wiring is required to consume it from a Vite app. This is why
 * a `styles` side-effect entrypoint is intentionally absent here (the
 * xterm adapter needs one for xterm's CSS; ghostty-web ships none).
 */

export { GhosttyWebRenderer } from "./GhosttyWebRenderer.js";
export { type GhosttyWebRendererOptions } from "./options.js";

// The shared renderer-neutral types live in `@relayterm/terminal-core`;
// they are re-exported here so callers consuming this adapter can import
// the full neutral surface from one place. Do NOT introduce adapter-local
// duplicates of these types — extend `BaseTerminalRendererOptions` instead.
export {
  type BaseTerminalRendererOptions,
  type RendererCursorStyle,
  type RendererTheme,
  type RendererThemeAnsi,
} from "@relayterm/terminal-core";

// The following are intentionally NOT re-exported through the public
// barrel:
//
//  - `__resetGhosttyInitPromiseForTesting` and
//    `__setGhosttyInitForTesting` from `GhosttyWebRenderer.ts`. They
//    are test seams for swapping the shared WASM init out from under
//    the cached promise; production callers must not be able to reach
//    them. Tests import them via the relative module path.
//  - `toGhosttyOptions` / `toGhosttyTheme` from `options.ts`. They
//    return ghostty-web-shaped types (`ITerminalOptions`, `ITheme`)
//    which would re-introduce ghostty-web shapes into the consumer
//    API surface, defeating the renderer-neutral rule. Tests import
//    them via the relative path; future renderer adapters should not
//    be reaching for them at all.
