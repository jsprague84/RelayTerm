/**
 * Side-effect entrypoint that injects xterm.js' baseline stylesheet.
 *
 * Imported once by the consuming app (e.g. the dev lab) via
 * `import "@relayterm/terminal-xterm/styles"`. Splitting it out keeps
 * the main module Node/vitest-importable — a CSS import would crash
 * any non-bundler consumer.
 */
import "@xterm/xterm/css/xterm.css";
