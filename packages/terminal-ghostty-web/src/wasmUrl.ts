/**
 * Same-origin asset URL for ghostty-web's `ghostty-vt.wasm` payload.
 *
 * Upstream `ghostty-web@0.4.0` ships its WASM two ways:
 *
 *   1. Inlined inside `dist/ghostty-web.js` as a giant
 *      `data:application/wasm;base64,…` URL the no-arg `init()` sugar
 *      hands to `WebAssembly.compile()`. That path is incompatible with
 *      RelayTerm's production CSP (`default-src 'self'` with no
 *      `connect-src` override permits same-origin fetches only, not
 *      `data:`).
 *   2. As a sibling `./ghostty-vt.wasm` file the upstream package's
 *      `exports` map exposes as the subpath `ghostty-web/ghostty-vt.wasm`.
 *      Vite's `?url` suffix copies that file into the bundle's asset
 *      directory and substitutes a fingerprinted same-origin URL string
 *      at build time (e.g. `/assets/ghostty-vt-<hash>.wasm`).
 *
 * The adapter loads ghostty-web's WASM via path 2 and hands the resulting
 * `Ghostty` instance into `new Terminal({ ghostty })` so the no-arg
 * `init()` is never reached and the inlined data URL never fires inside
 * the production bundle.
 *
 * `WebAssembly.compile()` itself still requires `'wasm-unsafe-eval'` in
 * the deployment's CSP `script-src` — that is upstream-baked into
 * `Ghostty.loadFromPath`. Closing that gap is a separate, deploy-side
 * slice and explicitly NOT done here; this file removes the `data:` /
 * `connect-src` half of the CSP incompatibility.
 */
import wasmUrl from "ghostty-web/ghostty-vt.wasm?url";

export const ghosttyWasmUrl: string = wasmUrl;
