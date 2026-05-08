/**
 * Tauri runtime detection for the bootstrap-picker / handoff slice.
 *
 * Scope: pure runtime checks. The browser deployment must NEVER see
 * the picker; `tauri:dev` / `tauri:android:dev` must NEVER see the
 * picker (the Vite proxy already routes `/api` and `/healthz` to the
 * backend, and navigating away would lose HMR + the proxy).
 *
 * Two discriminators (design § 13):
 *   isTauri()                 — runtime "are we inside a Tauri WebView?"
 *   isTauriBootstrapEnabled() — `isTauri() && !import.meta.env.DEV`
 *                               (a built Tauri shell, never browser, never dev)
 *
 * The Tauri global names (`__TAURI_INTERNALS__`, `__TAURI__`,
 * `isTauri`) are Tauri-internal — isolating access here means a future
 * rename is one diff. Every `window` access is guarded so SSR / Node
 * test environments stay clean.
 *
 * `import.meta.env.DEV` is statically `true` under `vite dev`/`vitest`
 * and `false` for `vite build`. Vite inlines the constant, so the
 * dev-mode short-circuit dead-code-eliminates from the production
 * bundle without any runtime cost.
 */

/**
 * Returns true when the SPA is running inside a Tauri WebView.
 *
 * Tauri v2 injects an `__TAURI_INTERNALS__` object on `window` for
 * every WebView (both built shells and `tauri:dev`/`tauri:android:dev`).
 * The legacy `__TAURI__` global is also accepted for resilience against
 * a future rename. `window.isTauri` is a v2.1+ shorthand that is
 * present on all platforms when Tauri is hosting the page.
 *
 * Returns false in:
 *  - the browser deployment (`window` exists, no Tauri globals)
 *  - SSR / Node / vitest (no `window` at all)
 */
export function isTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  const w = window as Window & {
    __TAURI_INTERNALS__?: unknown;
    __TAURI__?: unknown;
    isTauri?: unknown;
  };
  return (
    typeof w.__TAURI_INTERNALS__ !== "undefined" ||
    typeof w.__TAURI__ !== "undefined" ||
    w.isTauri === true
  );
}

/**
 * Returns true when the bootstrap picker / handoff path should run.
 *
 * The picker is built-Tauri-shell-only:
 *  - `isTauriRuntime()` keeps it out of the browser deployment.
 *  - `!import.meta.env.DEV` keeps it out of `tauri:dev` and
 *    `tauri:android:dev`, where the Vite proxy already routes the
 *    backend traffic and a navigate-away would lose HMR.
 *
 * `import.meta.env.DEV` is read lazily inside the function so the
 * bundler still inlines it; do not cache it at module scope, that
 * would break unit tests that mock the runtime.
 */
export function isTauriBootstrapEnabled(): boolean {
  if (!isTauriRuntime()) return false;
  return !import.meta.env.DEV;
}
