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
  /** Bootstrap picker should render. No navigation. */
  | { kind: "show_picker"; reason: "not_tauri_runtime" | "no_config" }
  /**
   * Configured backend origin found and validated. The caller (the
   * gate) is responsible for invoking `navigation.assign(targetUrl)`.
   * The decision is split from the side effect so tests can inspect
   * the chosen URL without a navigation actually firing.
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
}

/**
 * Compute the handoff decision without performing navigation.
 *
 * Behaviour:
 *  - If the runtime is NOT a built Tauri shell, returns
 *    `{ kind: "show_picker", reason: "not_tauri_runtime" }` — a
 *    sentinel for the caller to render the regular browser path. In
 *    practice the gate uses `isTauriBootstrapEnabled` directly and
 *    only calls this function when that returned `true`; the
 *    `not_tauri_runtime` branch is a safety belt for misuse.
 *  - If the runtime is built-Tauri but no valid config exists in
 *    storage, returns `{ kind: "show_picker", reason: "no_config" }`.
 *    Drift on read (canonical-shape mismatch, version mismatch,
 *    invalid origin) collapses to "no config" by way of
 *    `loadBackendConfig`'s drop-on-drift policy (design § 8).
 *  - Otherwise returns `{ kind: "navigate", targetUrl, config }` for
 *    the caller to assign onto the live `window.location`.
 */
export function decideHandoff(input: DecideHandoffInput): HandoffDecision {
  if (!input.isTauriBootstrapEnabled()) {
    return { kind: "show_picker", reason: "not_tauri_runtime" };
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
