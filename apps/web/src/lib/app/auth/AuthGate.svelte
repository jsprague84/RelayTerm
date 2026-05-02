<script lang="ts">
  /**
   * Top-level auth gate for the production SPA.
   *
   * Behavior (load-bearing):
   *  - Mounts and immediately calls `getCurrentUser()`. While the call
   *    is in flight, renders a small loading splash.
   *  - On 200, hands the parsed user up to the parent via the
   *    `children` snippet (the parent renders `AppShell`).
   *  - On HTTP 401, renders `LoginView` (and optionally `BootstrapView`
   *    when the operator clicks "First-time setup").
   *  - On any other failure (transport, malformed, 5xx), renders an
   *    explicit "session check failed" surface with a retry button —
   *    the SPA does NOT auto-retry because the failure mode might be
   *    a wedged backend, and a retry storm would mask the operator
   *    signal.
   *  - After a successful login the gate re-resolves the current
   *    user from the wire; we deliberately do not just trust the
   *    login-response body, so a future expansion of the user DTO
   *    is picked up consistently.
   *  - On logout, the gate resets to "show LoginView" without a
   *    network round-trip. Local cleanup (active terminal pointer
   *    etc.) lives on the AppShell side.
   */
  import type { Snippet } from "svelte";
  import {
    describeAuthGateError,
    getCurrentUser,
    type CurrentUser,
  } from "../../api/auth.js";
  import LoginView from "./LoginView.svelte";
  import BootstrapView from "./BootstrapView.svelte";

  type GateState =
    | { kind: "loading" }
    | { kind: "login" }
    | { kind: "bootstrap" }
    | { kind: "error"; message: string }
    | { kind: "ready"; user: CurrentUser };

  interface Props {
    /** Rendered when the gate is in the `ready` state. The snippet
     * receives the resolved current user and a `signOut` callback
     * that resets the gate to the login screen WITHOUT a wire call
     * — the AppShell is the surface that owns `POST /auth/logout`
     * and local cleanup. */
    children: Snippet<
      [{ user: CurrentUser; signOut: () => void }]
    >;
  }

  let { children }: Props = $props();

  let state = $state<GateState>({ kind: "loading" });

  async function loadCurrentUser() {
    state = { kind: "loading" };
    const result = await getCurrentUser();
    if (result.ok) {
      state = { kind: "ready", user: result.user };
      return;
    }
    if (result.error.kind === "http" && result.error.status === 401) {
      state = { kind: "login" };
      return;
    }
    // Every other failure (transport, malformed, non-401 HTTP) routes
    // through `describeAuthGateError`, which is the single sentinel-
    // tested formatter for this surface. The function-of-status-only
    // posture matches `describeAuthError` / `describeLoadError`.
    state = { kind: "error", message: describeAuthGateError(result.error) };
  }

  $effect(() => {
    void loadCurrentUser();
  });

  function handleSignedIn(user: CurrentUser) {
    state = { kind: "ready", user };
  }

  function handleSignedOut() {
    state = { kind: "login" };
  }

  function handleRequestBootstrap() {
    state = { kind: "bootstrap" };
  }

  function handleRequestLogin() {
    state = { kind: "login" };
  }

  function handleRetry() {
    void loadCurrentUser();
  }
</script>

{#if state.kind === "loading"}
  <div
    class="flex min-h-screen items-center justify-center bg-zinc-900 text-zinc-400"
    data-testid="auth-loading"
  >
    <span class="font-mono text-xs uppercase tracking-wide">
      Checking session…
    </span>
  </div>
{:else if state.kind === "error"}
  <div
    class="flex min-h-screen items-center justify-center bg-zinc-900 px-4 py-10"
    data-testid="auth-error-screen"
  >
    <section
      class="flex w-full max-w-sm flex-col gap-4 rounded-lg border border-red-900/40 bg-red-950/20 p-6"
    >
      <header class="flex flex-col gap-1">
        <h1 class="text-base font-semibold tracking-tight text-red-100">
          Cannot reach RelayTerm
        </h1>
        <p
          class="text-sm text-red-200/80"
          data-testid="auth-error-message"
        >
          {state.message}
        </p>
      </header>
      <button
        type="button"
        data-testid="auth-error-retry"
        class="self-start rounded-md bg-red-900/60 px-3 py-1.5 text-xs font-semibold text-red-50 hover:bg-red-900"
        onclick={handleRetry}
      >
        Retry
      </button>
    </section>
  </div>
{:else if state.kind === "login"}
  <LoginView
    onSignedIn={handleSignedIn}
    onRequestBootstrap={handleRequestBootstrap}
  />
{:else if state.kind === "bootstrap"}
  <BootstrapView onRequestLogin={handleRequestLogin} />
{:else if state.kind === "ready"}
  {@render children({ user: state.user, signOut: handleSignedOut })}
{/if}
