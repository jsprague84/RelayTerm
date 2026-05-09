<script lang="ts">
  /**
   * Top-level Tauri gate (design § 5, § 13).
   *
   * Behaviour:
   *  - Browser deployment (`!isTauriBootstrapEnabled()`) — render
   *    `children` immediately; AuthGate / AppShell mount unchanged.
   *  - Built Tauri shell with no valid stored config — render the
   *    bootstrap picker. The browser deployment never reaches this
   *    branch because the predicate short-circuits.
   *  - Built Tauri shell with a valid stored config — schedule a
   *    `window.location.assign(${origin}/)` and render a brief
   *    "Connecting…" affordance with a "Change server" button. The
   *    button cancels the pending navigation, clears the persisted
   *    config, and returns to the picker so the operator can pick a
   *    different backend without uninstalling the app or hand-editing
   *    `localStorage`. Once navigation actually fires, the SPA loads
   *    from the configured backend and runs AuthGate / AppShell
   *    same-site (cookies, CSRF, Origin allowlist all work without
   *    changes).
   *
   * This is the SINGLE production component that consumes the Tauri
   * runtime + handoff helpers. Keeping the runtime branch here means
   * AuthGate / AppShell / api helpers stay Tauri-unaware.
   *
   * Redaction posture: the saved origin is public config; the
   * component never logs it. The Change Server reset path only
   * touches `BACKEND_CONFIG_STORAGE_KEY` (via `clearBackendConfig`),
   * never any auth / session / SSH-credential storage. The picker
   * component's onSaved callback triggers the handoff via
   * `window.location.assign`, never via a thrown Error or console
   * call.
   */
  import type { Snippet } from "svelte";
  import {
    decideHandoff,
    type NavigationTarget,
  } from "./backendHandoff.js";
  import { isTauriBootstrapEnabled as isTauriBootstrapEnabledDefault } from "./tauriRuntime.js";
  import {
    clearBackendConfig,
    type BackendConfigStorage,
  } from "./backendConfig.js";
  import TauriBackendBootstrap from "./TauriBackendBootstrap.svelte";

  /**
   * Internal phase state. Mirrors the three branches of `decideHandoff`
   * but adds an explicit `connecting` form so the Change Server
   * affordance can transition straight back to `picker` without
   * re-running `decideHandoff` (which would still return `navigate`
   * until storage is cleared, racing the operator's click).
   */
  type Phase =
    | { kind: "passthrough" }
    | { kind: "picker" }
    | { kind: "connecting"; targetUrl: string };

  interface Props {
    /** What to render once a config is in place (or in the browser
     * deployment, immediately). Production passes the existing
     * AuthGate + AppShell tree. */
    children: Snippet;
    /** Override the runtime predicate for tests. */
    isTauriBootstrapEnabled?: () => boolean;
    /** Override the storage for tests. */
    storage?: BackendConfigStorage;
    /** Override the navigation target for tests. */
    navigation?: NavigationTarget;
    /** Override the WebView's current page origin for tests. Defaults
     * to `window.location.origin`. Read once at component init so the
     * same-origin short-circuit (`already_at_backend`) is stable
     * across phase recomputes. */
    currentOrigin?: string;
    /** Delay (ms) before navigation is initiated. Defaults to 0 — the
     * timer fires on the next event-loop tick so the Connecting splash
     * mounts before the WebView reloads. Tests inject a positive value
     * so the Change Server reset path can be exercised before the
     * timer fires. */
    navigationDelayMs?: number;
  }

  const {
    children,
    isTauriBootstrapEnabled = isTauriBootstrapEnabledDefault,
    storage = (typeof window !== "undefined"
      ? window.localStorage
      : undefined) as BackendConfigStorage | undefined,
    navigation = (typeof window !== "undefined"
      ? window.location
      : undefined) as NavigationTarget | undefined,
    currentOrigin = typeof window !== "undefined"
      ? window.location.origin
      : "",
    navigationDelayMs = 0,
  }: Props = $props();

  function computePhase(): Phase {
    // `storage === undefined` is the SSR / Node / unit-test misuse case;
    // the production gate is always supplied with `window.localStorage`.
    // Falling through to `passthrough` (render children) matches the
    // "browser deployment never sees the picker" guarantee the rest of
    // the slice rests on (design § 13). Falling through to `picker`
    // would mount `TauriBackendBootstrap` without a storage backing.
    if (storage === undefined) return { kind: "passthrough" };
    const decision = decideHandoff({
      isTauriBootstrapEnabled,
      storage,
      currentOrigin,
    });
    if (decision.kind === "passthrough") return { kind: "passthrough" };
    if (decision.kind === "show_picker") return { kind: "picker" };
    return { kind: "connecting", targetUrl: decision.targetUrl };
  }

  let phase = $state<Phase>(computePhase());
  // Held outside `$state` so reads inside the effect don't establish a
  // reactive subscription on the timer handle itself; the effect should
  // re-run on phase changes only, never on timer-handle assignments.
  let pendingNavigationTimer: ReturnType<typeof setTimeout> | null = null;

  function cancelPendingNavigation() {
    if (pendingNavigationTimer !== null) {
      clearTimeout(pendingNavigationTimer);
      pendingNavigationTimer = null;
    }
  }

  $effect(() => {
    // Schedule navigation as a side effect of entering the connecting
    // phase. The timer handle lets the Change Server affordance cancel
    // the pending handoff before `assign` fires. The returned teardown
    // also clears the timer if the effect re-runs (phase change) or
    // the component unmounts — Svelte 5 idiom for resource-owning
    // effects (per AGENTS.md "Critical gotchas" — `$effect` replaces
    // `onMount` for derivations, with cleanup via the returned
    // function).
    if (
      phase.kind === "connecting" &&
      navigation !== undefined &&
      pendingNavigationTimer === null
    ) {
      const target = phase.targetUrl;
      const nav = navigation;
      pendingNavigationTimer = setTimeout(() => {
        pendingNavigationTimer = null;
        nav.assign(target);
      }, navigationDelayMs);
      return () => cancelPendingNavigation();
    }
  });

  function handleSaved(_origin: string) {
    if (storage === undefined) return;
    phase = computePhase();
  }

  function handleChangeServer() {
    cancelPendingNavigation();
    if (storage !== undefined) {
      clearBackendConfig(storage);
    }
    phase = { kind: "picker" };
  }
</script>

{#if phase.kind === "picker"}
  <TauriBackendBootstrap {storage} onSaved={handleSaved} />
{:else if phase.kind === "connecting"}
  <div
    class="flex min-h-screen flex-col items-center justify-center gap-6 bg-zinc-900 px-4 text-zinc-400"
    data-testid="tauri-bootstrap-connecting"
  >
    <span class="font-mono text-xs uppercase tracking-wide">Connecting…</span>
    <button
      type="button"
      onclick={handleChangeServer}
      data-testid="tauri-bootstrap-change-server"
      class="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-xs font-medium text-zinc-300 transition hover:border-zinc-500 hover:text-zinc-100"
    >
      Change server
    </button>
  </div>
{:else}
  {@render children()}
{/if}
