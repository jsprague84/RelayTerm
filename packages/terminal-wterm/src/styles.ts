/**
 * Side-effect entrypoint that injects wterm's baseline stylesheet.
 *
 * Imported once by the consuming app (e.g. the dev lab) via
 * `import "@relayterm/terminal-wterm/styles"`. The architectural rule
 * is that `apps/web` depends on `@relayterm/terminal-wterm` (workspace)
 * and NEVER directly on `@wterm/dom`; routing the CSS through this
 * adapter package preserves that contract — pnpm strict mode would
 * otherwise refuse to resolve `@wterm/dom/css` from a consumer that did
 * not declare `@wterm/dom` itself.
 *
 * Splitting the side-effect out of `index.ts` keeps the main module
 * Node/vitest-importable; a CSS import would crash any non-bundler
 * consumer. The `package.json` `sideEffects` array pins this file
 * (and any `*.css` it pulls in) so Rollup's tree-shaker drops the
 * adapter cleanly when the dev lab is dead-code-eliminated.
 *
 * The styles ship the `.wterm` host class, the cell-grid layout, theme
 * variables (`--term-fg`, `--term-bg`, `--term-color-{0..15}`,
 * `--term-font-family`, `--term-font-size`, `--term-line-height`,
 * `--term-row-height`), the cursor styles, and three preset modifier
 * classes (`theme-solarized-dark`, `theme-monokai`, `theme-light`).
 */
import "@wterm/dom/css";
