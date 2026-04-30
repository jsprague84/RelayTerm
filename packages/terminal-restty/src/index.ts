/**
 * `@relayterm/terminal-restty` — restty-backed implementation of the
 * `TerminalRenderer` interface from `@relayterm/terminal-core`.
 *
 * This package is an EXPERIMENTAL renderer adapter. xterm.js (via
 * `@relayterm/terminal-xterm`) is the compatibility baseline;
 * `@relayterm/terminal-ghostty-web` is the libghostty-vt-via-WASM
 * experiment; this package proves the renderer adapter seam holds up
 * under a more ambitious modern renderer (libghostty-vt + WebGPU/WebGL2
 * + text-shaper) without requiring backend protocol or `terminal-core`
 * changes.
 *
 * The architectural rule lives in `terminal-core`: the protocol and
 * the session client never see restty types. Every export from this
 * package is renderer-neutral at the interface level, even though the
 * implementation is restty-shaped.
 *
 * restty consumes a sizeable WASM payload (`restty 0.1.x` ships ~3 MB
 * of bundled JS plus an inlined WASM binary). The package declares
 * `sideEffects: false` so a Vite/Rollup production build that never
 * reaches this adapter (today: any prod build, because the dev lab is
 * gated on `import.meta.env.DEV`) tree-shakes it out. A real-browser
 * smoke check of the WASM/WebGPU runtime is dev-lab only — Vitest
 * exercises this adapter against a mocked `restty/xterm` module.
 */

export { ResttyRenderer } from "./ResttyRenderer.js";
export type { ResttyRendererCtorOptions } from "./ResttyRenderer.js";
export {
  type ResttyInitialGrid,
  type ResttyRendererOptions,
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

// `toResttyOptions` is intentionally NOT re-exported through the public
// barrel. It returns a restty-compat-shaped `Record<string, unknown>`
// that, while not a restty-typed value at the API boundary, exists only
// because the adapter needs to feed restty's option bag. Tests import
// it via the relative path; future renderer adapters should not be
// reaching for it at all. Same pattern as `toGhosttyOptions` in the
// sibling adapter.
