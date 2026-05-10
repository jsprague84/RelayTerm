<script lang="ts">
  // Production app shell. This module MUST NOT import anything from
  // `lib/dev/` — dev-lab code is pulled in only via the dev-only branch
  // in `App.svelte`, where `import.meta.env.DEV` lets Vite tree-shake
  // the entire dev surface out of the production bundle.
  //
  // The shell owns the cross-view "active terminal launch" state so
  // that pressing "Launch terminal" inside the Servers view can switch
  // to the Terminal view AND hand off the session id without a routing
  // library.

  import SidebarNav from "./SidebarNav.svelte";
  import TopBar from "./TopBar.svelte";
  import DashboardView from "./views/DashboardView.svelte";
  import TerminalView from "./views/TerminalView.svelte";
  import type { ActiveLaunch } from "./terminal/activeLaunch.js";
  import {
    activeSessionFromLaunch,
    clearActiveSession,
    saveActiveSession,
    updateActiveSessionSeq,
  } from "./terminal/activeSessionStore.js";
  import SessionsView from "./views/SessionsView.svelte";
  import ServersView from "./views/ServersView.svelte";
  import IdentitiesView from "./views/IdentitiesView.svelte";
  import SettingsView from "./views/SettingsView.svelte";
  import RecordingReplayView from "./views/RecordingReplayView.svelte";
  import {
    findNavItem,
    type AppViewId,
  } from "./navigation.js";
  import {
    isKnownAppPath,
    pathForView,
    viewForPath,
  } from "./routing.js";
  import { logout as logoutApi, type CurrentUser } from "../api/auth.js";
  import type { Snippet } from "svelte";

  interface Props {
    devMode?: boolean;
    /** Optional dev-tools panel rendered below the main view. The shell
     * itself never imports the dev lab; the host (`App.svelte`) passes a
     * snippet that's only constructed when `import.meta.env.DEV` is true,
     * keeping dev code out of the production bundle. */
    devTools?: Snippet;
    /** Authenticated user resolved by `AuthGate`. Optional so existing
     * tests that mount `AppShell` directly do not have to fabricate a
     * user; in production `AuthGate` always supplies it. */
    user?: CurrentUser | null;
    /** Reset the auth gate to the login screen. Invoked after the
     * `POST /auth/logout` round-trip AND after local cleanup. */
    signOut?: () => void;
  }

  let { devMode = false, devTools, user = null, signOut }: Props = $props();

  // Initial view comes from the URL, with unknown paths collapsing to
  // the default. Production deployments must serve `index.html` for
  // every app route — see SPEC.md "URL-driven production view routing".
  // A reactive shell-state read of `window.location` only happens here
  // (initial mount) and inside the `popstate` listener wired below;
  // mid-life URL state is mirrored from `selected` via `pushState`.
  function initialView(): AppViewId {
    if (typeof window === "undefined") return "dashboard";
    return viewForPath(window.location.pathname);
  }
  let selected = $state<AppViewId>(initialView());
  let devToolsOpen = $state(false);
  let current = $derived(findNavItem(selected));

  $effect(() => {
    // Wire popstate + canonicalize the initial URL. The effect tracks
    // `selected` (read inside `pathForView(selected)`) and re-runs on
    // every `navigate()`, but the canonicalize branch is gated by
    // `isKnownAppPath(window.location.pathname)`: after `navigate()`
    // pushState's a canonical path, the gate evaluates true and the
    // replaceState is skipped. Net effect: replaceState fires only when
    // the live URL is genuinely unknown (initial mount on a stale link).
    if (typeof window === "undefined") return;
    const onPopState = () => {
      // Browser back/forward is semantically equivalent to a sidebar
      // nav click — same rule as `navigate()`: drop the transient
      // replay overlay so the operator does not end up on a different
      // route with a stale full-pane viewer still mounted.
      activeReplaySessionId = null;
      activeReplayLabel = null;
      selected = viewForPath(window.location.pathname);
    };
    if (!isKnownAppPath(window.location.pathname)) {
      window.history.replaceState(null, "", pathForView(selected));
    }
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  });

  function navigate(id: AppViewId) {
    // Any nav click clears an in-flight replay overlay. The viewer is
    // a transient, full-pane surface that does not own a sidebar
    // entry — landing on a different view drops it.
    activeReplaySessionId = null;
    activeReplayLabel = null;
    if (id === selected) return;
    selected = id;
    if (typeof window === "undefined") return;
    const next = pathForView(id);
    if (window.location.pathname !== next) {
      // pushState lets browser back/forward step through the in-app
      // history without a full page reload.
      window.history.pushState(null, "", next);
    }
  }
  /**
   * Active terminal launch. `null` until a profile-row "Launch terminal"
   * action creates a session. Lives at the shell so navigating away
   * from the Terminal view (without explicitly closing) preserves the
   * attachment for the brief detached-TTL window — the next visit
   * remounts `ProductionTerminal`, which re-attaches the WebSocket and
   * passes its captured `lastSeenSeq` for replay.
   *
   * Resetting to `null` on the "Back to servers" exit and on explicit
   * disposal in the Terminal view is intentional: the shell does not
   * persist a closed session as if it were still launchable.
   */
  let activeLaunch = $state<ActiveLaunch | null>(null);

  /**
   * Active recording replay session id. Set when the operator clicks
   * "View recording" from the Sessions list. While non-null, the
   * shell renders {@link RecordingReplayView} INSTEAD of the
   * navigation-selected view — replay is a transient, full-pane
   * surface that does not own a sidebar entry. Cleared via
   * "Back to sessions" or any nav click that calls `navigate()`.
   *
   * The session id is held in memory only — never persisted, never
   * mirrored into the URL (recording chunk bytes are sensitive; we
   * keep them off any externally observable surface). A page
   * navigation drops the replay state entirely.
   */
  let activeReplaySessionId = $state<string | null>(null);
  /** Optional human label (usually the originating profile name) so
   * the replay header is readable without a profile lookup. */
  let activeReplayLabel = $state<string | null>(null);

  function handleViewRecording(sessionId: string, profileLabel: string | null) {
    activeReplaySessionId = sessionId;
    activeReplayLabel = profileLabel;
  }

  function handleReplayExit() {
    activeReplaySessionId = null;
    activeReplayLabel = null;
  }

  function handleLaunch(launch: ActiveLaunch) {
    activeLaunch = launch;
    activeReplaySessionId = null;
    activeReplayLabel = null;
    navigate("terminal");
    // Persist a local pointer at the just-launched session so a
    // navigation-away / reload during the bounded detached-TTL window
    // can offer an explicit "Reconnect last session" affordance. The
    // saved record carries safe metadata only — see
    // `activeSessionStore.ts` for the contract.
    saveActiveSession(activeSessionFromLaunch(launch));
  }

  function handleTerminalExit() {
    // "Back to servers" leaves the saved record alone — the operator
    // may want to reconnect within the detached-TTL window. The
    // production terminal has already disposed the local client; the
    // backend keeps the PTY alive briefly per its bounded TTL.
    activeLaunch = null;
    navigate("servers");
  }

  function handleSessionClosed() {
    // Wire-confirmed close (server `SessionClosed`, post-`End session`,
    // etc.). The backend runtime is gone and a reconnect would fail —
    // drop the local pointer so the empty-state Terminal view does not
    // tempt the operator with a stale "Reconnect last session" button.
    clearActiveSession();
    activeLaunch = null;
  }

  function handleLastSeenSeqUpdate(seq: number) {
    if (!activeLaunch) return;
    updateActiveSessionSeq(activeLaunch.sessionId, seq);
  }

  function handleReconnectLastSession(launch: ActiveLaunch) {
    // Same as `handleLaunch`, but called from the empty-state Terminal
    // view's "Reconnect last session" affordance. Routing through the
    // shared launch path keeps the saved-record refresh + view
    // transition in one place.
    handleLaunch(launch);
  }

  function handleForgetLastSession() {
    // "Forget saved session" affordance: an explicit user action to
    // drop the local pointer without attempting a reconnect. Useful
    // when the saved session is stale (e.g. backend was restarted,
    // TTL expired) and the operator does not want to retry.
    clearActiveSession();
  }

  let signingOut = $state(false);

  /**
   * Local cleanup shared between the explicit "Sign out" button and
   * the Settings panel's "revoke current session" path. The active-
   * terminal pointer drop pins SPEC.md "Frontend authentication UI
   * plan" Phase 4: re-login within the detached-TTL window does not
   * get to silently reattach to a session belonging to (now-revoked)
   * credentials.
   */
  function performLocalSignOut() {
    clearActiveSession();
    activeLaunch = null;
    signOut?.();
  }

  async function handleSignOut() {
    if (signingOut) return;
    signingOut = true;
    try {
      // Best-effort wire revocation. The backend is idempotent (missing
      // / unknown / already-revoked cookies all return 204), so a wire
      // failure here does NOT trap the user — local cleanup still runs
      // and the gate flips to the login screen. The auth crate's
      // sweeper (future work) reaps any orphan row.
      await logoutApi();
    } finally {
      // Local cleanup ALWAYS runs, regardless of the wire outcome.
      signingOut = false;
      performLocalSignOut();
    }
  }

  /**
   * Hand-off from the Settings session-management panel after a
   * successful current-session revoke. The backend has already cleared
   * the cookie via the revoke route's `Set-Cookie` header, so we skip
   * `POST /auth/logout` and run only local cleanup + gate flip — no
   * duplicated logout logic.
   */
  function handleCurrentSessionRevoked() {
    performLocalSignOut();
  }
</script>

<div class="flex h-full min-h-screen bg-zinc-900 text-zinc-100">
  <SidebarNav
    {selected}
    onselect={(id) => navigate(id)}
    showDevTools={devMode && devTools !== undefined}
    devToolsOpen={devToolsOpen}
    onToggleDevTools={() => (devToolsOpen = !devToolsOpen)}
  />
  <div class="flex min-w-0 flex-1 flex-col">
    <TopBar
      {current}
      {devMode}
      {user}
      onSignOut={signOut ? handleSignOut : undefined}
      signingOut={signingOut}
    />
    <main
      class="flex-1 overflow-y-auto px-6 py-6"
      data-testid="app-shell-main"
      data-view={selected}
    >
      <div class="mx-auto flex max-w-4xl flex-col gap-6">
        {#if activeReplaySessionId}
          {#key activeReplaySessionId}
            <RecordingReplayView
              sessionId={activeReplaySessionId}
              profileLabel={activeReplayLabel ?? undefined}
              onExit={handleReplayExit}
            />
          {/key}
        {:else if selected === "dashboard"}
          <DashboardView onNavigate={(id) => navigate(id)} />
        {:else if selected === "terminal"}
          <!--
            {#key} on activeLaunch.sessionId so a launch transition
            (non-null → null on wire-close, null → new id on launch,
            id → different id on reconnect-from-Sessions) unmounts and
            remounts TerminalView. Without this, TerminalView's
            `let saved = $state(loadActiveSession())` is captured at
            first mount and stays stale even after handleSessionClosed
            calls clearActiveSession() — so the empty-state
            "Reconnect last session" button surfaces a pointer at the
            just-closed session and a click produces a doomed
            connection error. Pinned by tests/appShellIsolation.test.ts
            so a regression that drops this wrapper trips the suite.

            Note: when activeLaunch transitions null → null (a
            redundant onSessionClosed firing on an already-empty
            shell), the key value stays "empty" → "empty" and
            TerminalView is NOT remounted. That is safe by design —
            clearActiveSession() already ran on the first close, so
            saved is already null inside the still-mounted
            TerminalView.
          -->
          {#key activeLaunch?.sessionId ?? "empty"}
            <TerminalView
              launch={activeLaunch}
              onExit={handleTerminalExit}
              onSessionClosed={handleSessionClosed}
              onLastSeenSeqUpdate={handleLastSeenSeqUpdate}
              onReconnectLastSession={handleReconnectLastSession}
              onForgetLastSession={handleForgetLastSession}
            />
          {/key}
        {:else if selected === "sessions"}
          <SessionsView
            onReconnect={handleLaunch}
            onViewRecording={handleViewRecording}
            activeSessionId={activeLaunch?.sessionId ?? null}
          />

        {:else if selected === "servers"}
          <ServersView onLaunch={handleLaunch} />
        {:else if selected === "identities"}
          <IdentitiesView />
        {:else if selected === "settings"}
          <SettingsView
            onCurrentSessionRevoked={signOut
              ? handleCurrentSessionRevoked
              : undefined}
          />
        {/if}

        {#if devMode && devTools && devToolsOpen}
          <section
            class="flex flex-col gap-3 rounded-lg border border-amber-900/40 bg-amber-950/10 p-4"
            data-testid="dev-tools-panel"
          >
            <header class="flex items-center justify-between">
              <h2
                class="font-mono text-xs uppercase tracking-wide text-amber-200/80"
              >
                Developer tools
              </h2>
              <span class="text-[11px] text-amber-200/60">
                dev-only · not part of the production build
              </span>
            </header>
            {@render devTools()}
          </section>
        {/if}
      </div>
    </main>
  </div>
</div>
