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
 * ghostty-web ships its WASM payload TWO ways in `ghostty-web@0.4.0`:
 *   1. inlined as a `data:application/wasm;base64,…` URL inside the
 *      shipped JS bundle (the no-arg `init()` sugar uses this);
 *   2. as a sibling `./ghostty-vt.wasm` file the package's `exports`
 *      map exposes at the subpath `ghostty-web/ghostty-vt.wasm`.
 *
 * The adapter takes path 2: `./wasmUrl.ts` imports the upstream subpath
 * with Vite's `?url` suffix so the production build emits a
 * fingerprinted same-origin asset, and `GhosttyWebRenderer.mount`
 * passes the loaded `Ghostty` instance through `Terminal({ ghostty })`
 * so the inlined data URL — incompatible with RelayTerm's
 * `default-src 'self'` CSP — is never reached. See the file header on
 * `GhosttyWebRenderer.ts` for the full posture and the
 * `wasm-unsafe-eval` caveat. A `styles` side-effect entrypoint is
 * intentionally absent here (the xterm adapter needs one for xterm's
 * CSS; ghostty-web ships none).
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
//  - `__resetGhosttyLoadPromiseForTesting` and
//    `__setGhosttyLoaderForTesting` from `GhosttyWebRenderer.ts`. They
//    are test seams for swapping the shared WASM load out from under
//    the cached promise; production callers must not be able to reach
//    them. Tests import them via the relative module path.
//  - `ghosttyWasmUrl` from `wasmUrl.ts`. The same-origin asset URL is
//    an internal implementation detail of how the adapter loads its
//    WASM; package consumers must not depend on it.
//  - `toGhosttyOptions` / `toGhosttyTheme` from `options.ts`. They
//    return ghostty-web-shaped types (`ITerminalOptions`, `ITheme`)
//    which would re-introduce ghostty-web shapes into the consumer
//    API surface, defeating the renderer-neutral rule. Tests import
//    them via the relative path; future renderer adapters should not
//    be reaching for them at all.
