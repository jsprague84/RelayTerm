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
   *  - Built Tauri shell with a valid stored config — assign
   *    `window.location` to the configured origin's root and render a
   *    short "Connecting…" affordance while the WebView reloads. The
   *    SPA that loads from the configured backend then runs AuthGate
   *    / AppShell same-site (cookies, CSRF, Origin allowlist all
   *    work without changes).
   *
   * This is the SINGLE production component that consumes the Tauri
   * runtime + handoff helpers. Keeping the runtime branch here means
   * AuthGate / AppShell / api helpers stay Tauri-unaware.
   *
   * Redaction posture: the saved origin is public config; the
   * component never logs it. The picker component's onSaved callback
   * triggers the handoff via `window.location.assign`, never via a
   * thrown Error or console call.
   */
  import type { Snippet } from "svelte";
  import {
    decideHandoff,
    type HandoffDecision,
    type NavigationTarget,
  } from "./backendHandoff.js";
  import { isTauriBootstrapEnabled as isTauriBootstrapEnabledDefault } from "./tauriRuntime.js";
  import type { BackendConfigStorage } from "./backendConfig.js";
  import TauriBackendBootstrap from "./TauriBackendBootstrap.svelte";

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
  }: Props = $props();

  // Initial decision is computed once at mount. Re-evaluating on
  // every change would race the navigation kick-off; the picker's
  // onSaved callback drives the explicit transition instead.
  function initialDecision(): HandoffDecision {
    if (storage === undefined) {
      return { kind: "show_picker", reason: "not_tauri_runtime" };
    }
    return decideHandoff({ isTauriBootstrapEnabled, storage });
  }

  let decision = $state<HandoffDecision>(initialDecision());
  let connecting = $state(false);

  // Fire navigation as a side effect of entering the `navigate`
  // branch. The flag prevents a re-fire if the same decision is
  // re-emitted (e.g. re-render before the WebView reload completes).
  $effect(() => {
    if (decision.kind === "navigate" && navigation !== undefined && !connecting) {
      connecting = true;
      navigation.assign(decision.targetUrl);
    }
  });

  function handleSaved(_origin: string) {
    if (storage === undefined) return;
    decision = decideHandoff({ isTauriBootstrapEnabled, storage });
  }
</script>

{#if decision.kind === "show_picker" && decision.reason === "no_config"}
  <TauriBackendBootstrap {storage} onSaved={handleSaved} />
{:else if decision.kind === "navigate"}
  <div
    class="flex min-h-screen items-center justify-center bg-zinc-900 text-zinc-400"
    data-testid="tauri-bootstrap-connecting"
  >
    <span class="font-mono text-xs uppercase tracking-wide">Connecting…</span>
  </div>
{:else}
  {@render children()}
{/if}
