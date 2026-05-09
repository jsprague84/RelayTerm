/**
 * Path A handoff — the built Tauri shell navigates the WebView from
 * the bootstrap origin (`tauri://localhost` / `http://tauri.localhost`)
 * to the configured backend origin so the SPA reaches the backend
 * same-site, with cookies and CSRF guards already wired (design §§ 4,
 * 9).
 *
 * Scope: pure decision + URL construction. Dependencies (storage,
 * navigation, runtime check) are injected so vitest can pin every
 * branch without a DOM. The single production call site lives in the
 * Tauri-only ConfiguredBackendGate; the browser deployment never
 * imports this module via a runtime path that triggers navigation.
 *
 * Redaction posture: this module never logs; it never echoes the
 * configured URL into any thrown Error; the only consumer of a
 * rejection is the picker UI, which surfaces the typed reason from
 * `validateBackendOrigin`. Storage drift collapses to "no config" so
 * the caller falls back to the picker rather than acting on stale data.
 */

import {
  loadBackendConfig,
  validateBackendOrigin,
  type BackendConfig,
  type BackendConfigStorage,
} from "./backendConfig.js";

/**
 * Minimal shape of the navigation surface the handoff needs. Models
 * `window.location.assign(url)` so the production wiring binds it to
 * `window.location` and unit tests pass an in-memory shim.
 */
export interface NavigationTarget {
  assign(url: string): void;
}

export type HandoffDecision =
  /**
   * Render `children` (AuthGate / AppShell) directly, without picker
   * or navigation. Two reasons:
   *  - `not_tauri_runtime` — browser deployment OR `tauri:dev` /
   *    `tauri:android:dev`. The picker is built-Tauri-shell-only.
   *  - `already_at_backend` — built Tauri shell where the WebView's
   *    current origin already byte-equals the saved backend origin.
   *    Path A's whole point is "the WebView IS at the backend after
   *    handoff"; once that condition holds, scheduling another
   *    `window.location.assign(${origin}/)` would loop the page (the
   *    Tauri WebView injects `__TAURI_INTERNALS__` on remote pages
   *    too, so the gate runs again at the remote origin and would
   *    re-fire the handoff against itself). Equality is RFC-6454-byte:
   *    `localhost` ≠ `127.0.0.1` ≠ `[::1]`, mirroring the
   *    `relayterm-api` `Origin` allow-list semantics in
   *    `crates/relayterm-api/src/auth/csrf.rs`.
   */
  | { kind: "passthrough"; reason: "not_tauri_runtime" | "already_at_backend" }
  /** Bootstrap picker should render. No navigation. */
  | { kind: "show_picker"; reason: "no_config" }
  /**
   * Configured backend origin found and validated, and the WebView is
   * NOT yet at that origin. The caller (the gate) is responsible for
   * invoking `navigation.assign(targetUrl)`. The decision is split
   * from the side effect so tests can inspect the chosen URL without
   * a navigation actually firing.
   */
  | { kind: "navigate"; targetUrl: string; config: BackendConfig };

/**
 * Build the handoff URL from a validated backend origin. The handoff
 * loads the SPA from the *backend* (path A in the design doc): the
 * configured origin's root, with a trailing `/` so the WebView treats
 * it as the document root. Caller MUST pass an origin that has
 * already been re-validated through {@link validateBackendOrigin} —
 * the gate does this by virtue of `loadBackendConfig` returning only
 * canonical-shape configs (drift drops to `null`).
 */
export function buildHandoffUrl(origin: string): string {
  return `${origin}/`;
}

interface DecideHandoffInput {
  /** Runtime predicate — usually `isTauriBootstrapEnabled` from
   * `./tauriRuntime`. Injected so tests do not need to mock the
   * Tauri globals. */
  isTauriBootstrapEnabled: () => boolean;
  /** Storage to read the persisted config from — usually
   * `window.localStorage` in production, an in-memory shim in tests. */
  storage: BackendConfigStorage;
  /** WebView's current page origin (e.g. `window.location.origin`).
   *
   * Used for the `already_at_backend` short-circuit: when the saved
   * backend origin byte-equals `currentOrigin`, the WebView is
   * already at the backend so the gate must NOT re-issue
   * `window.location.assign(${origin}/)` (that loops). Tests inject
   * an explicit origin to pin both the bundled origin
   * (`tauri://localhost`, `http://tauri.localhost`) and the remote
   * origin without a real WebView. Required (no default) so a
   * future caller forgetting to wire it fails the type checker
   * rather than silently re-introducing the loop. */
  currentOrigin: string;
}

/**
 * Compute the handoff decision without performing navigation.
 *
 * Behaviour:
 *  - If the runtime is NOT a built Tauri shell, returns
 *    `{ kind: "passthrough", reason: "not_tauri_runtime" }` — the
 *    caller renders `children` directly. In practice the gate also
 *    uses `isTauriBootstrapEnabled` itself; the `not_tauri_runtime`
 *    branch here is a safety belt for misuse.
 *  - If the runtime is built-Tauri but no valid config exists in
 *    storage, returns `{ kind: "show_picker", reason: "no_config" }`.
 *    Drift on read (canonical-shape mismatch, version mismatch,
 *    invalid origin) collapses to "no config" by way of
 *    `loadBackendConfig`'s drop-on-drift policy (design § 8).
 *  - If the runtime is built-Tauri AND a valid config exists AND the
 *    WebView's current origin byte-equals the saved backend origin,
 *    returns `{ kind: "passthrough", reason: "already_at_backend" }`.
 *    This breaks a navigate loop that the original Phase C code path
 *    had: the Tauri v2 WebView injects `__TAURI_INTERNALS__` on
 *    remote pages too, so the gate runs again at the post-handoff
 *    origin; without this short-circuit it would schedule another
 *    `window.location.assign(${origin}/)` and reload the same page
 *    indefinitely. Origin equality is RFC-6454-byte (mirrors
 *    `relayterm-api`'s `CsrfGuard` semantics): `localhost` ≠
 *    `127.0.0.1`, different ports differ, schemes differ.
 *  - Otherwise returns `{ kind: "navigate", targetUrl, config }` for
 *    the caller to assign onto the live `window.location`.
 */
export function decideHandoff(input: DecideHandoffInput): HandoffDecision {
  if (!input.isTauriBootstrapEnabled()) {
    return { kind: "passthrough", reason: "not_tauri_runtime" };
  }
  const cfg = loadBackendConfig(input.storage);
  if (cfg === null) {
    return { kind: "show_picker", reason: "no_config" };
  }
  // Defence in depth: re-validate before navigating. `loadBackendConfig`
  // already filters drift, but a future refactor that loosens that
  // path must not silently widen the navigation surface.
  const validation = validateBackendOrigin(cfg.backendOrigin);
  if (!validation.ok || validation.origin !== cfg.backendOrigin) {
    return { kind: "show_picker", reason: "no_config" };
  }
  if (cfg.backendOrigin === input.currentOrigin) {
    return { kind: "passthrough", reason: "already_at_backend" };
  }
  return {
    kind: "navigate",
    targetUrl: buildHandoffUrl(cfg.backendOrigin),
    config: cfg,
  };
}

interface PerformHandoffInput extends DecideHandoffInput {
  /** Navigation target — usually `window.location` in production. */
  navigation: NavigationTarget;
}

/**
 * Compute the decision and (when applicable) perform the navigation.
 * Returns the same {@link HandoffDecision} the caller can branch on
 * for rendering — when the kind is `navigate`, the target URL has
 * already been assigned to `navigation`. The caller renders a brief
 * "Connecting…" affordance and lets the WebView reload.
 */
export function performHandoff(input: PerformHandoffInput): HandoffDecision {
  const decision = decideHandoff(input);
  if (decision.kind === "navigate") {
    input.navigation.assign(decision.targetUrl);
  }
  return decision;
}
