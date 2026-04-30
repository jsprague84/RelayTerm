/**
 * `@relayterm/terminal-wterm` — wterm-backed implementation of the
 * `TerminalRenderer` interface from `@relayterm/terminal-core`.
 *
 * This package is an EXPERIMENTAL renderer adapter. xterm.js (via
 * `@relayterm/terminal-xterm`) is the compatibility baseline;
 * `@relayterm/terminal-ghostty-web` is the libghostty-vt-via-WASM
 * experiment; `@relayterm/terminal-restty` is the libghostty-vt +
 * WebGPU/WebGL2 experiment; this package is the DOM/mobile/
 * accessibility-oriented experiment built on `@wterm/dom`'s
 * Zig+WASM core wrapped by a CSS-themed grid renderer.
 *
 * The architectural rule lives in `terminal-core`: the protocol and
 * the session client never see wterm types. Every export from this
 * package is renderer-neutral at the interface level, even though
 * the implementation is wterm-shaped.
 *
 * `@wterm/core` inlines its WASM payload (~17 KB base64) inside the
 * shipped JS, so no separate asset wiring is needed under Vite. The
 * package declares `sideEffects: false`, so a Vite/Rollup production
 * build that never reaches this adapter (today: any prod build,
 * because the dev lab is gated on `import.meta.env.DEV`) tree-shakes
 * it out. A real-browser smoke check goes through the dev lab; the
 * Vitest suite exercises this adapter against a mocked `@wterm/dom`
 * module.
 */

export { WtermRenderer } from "./WtermRenderer.js";
export type { WtermRendererCtorOptions } from "./WtermRenderer.js";
export {
  type WtermInitialGrid,
  type WtermOnlyOptions,
  type WtermRendererOptions,
} from "./options.js";

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

// `toWtermOptions` is intentionally NOT re-exported through the public
// barrel. It returns an internal `MappedWtermOptions` shape that exists
// only because the adapter needs to feed wterm's option bag. Tests
// import it via the relative path; future renderer adapters should not
// be reaching for it at all. Same pattern as `toGhosttyOptions` and
// `toResttyOptions` in the sibling adapters.
