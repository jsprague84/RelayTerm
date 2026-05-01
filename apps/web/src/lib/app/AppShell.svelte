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
  import {
    DEFAULT_VIEW,
    findNavItem,
    type AppViewId,
  } from "./navigation.js";
  import type { Snippet } from "svelte";

  interface Props {
    devMode?: boolean;
    /** Optional dev-tools panel rendered below the main view. The shell
     * itself never imports the dev lab; the host (`App.svelte`) passes a
     * snippet that's only constructed when `import.meta.env.DEV` is true,
     * keeping dev code out of the production bundle. */
    devTools?: Snippet;
  }

  let { devMode = false, devTools }: Props = $props();

  let selected = $state<AppViewId>(DEFAULT_VIEW);
  let devToolsOpen = $state(false);
  let current = $derived(findNavItem(selected));
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

  function handleLaunch(launch: ActiveLaunch) {
    activeLaunch = launch;
    selected = "terminal";
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
    selected = "servers";
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
</script>

<div class="flex h-full min-h-screen bg-zinc-900 text-zinc-100">
  <SidebarNav
    {selected}
    onselect={(id) => (selected = id)}
    showDevTools={devMode && devTools !== undefined}
    devToolsOpen={devToolsOpen}
    onToggleDevTools={() => (devToolsOpen = !devToolsOpen)}
  />
  <div class="flex min-w-0 flex-1 flex-col">
    <TopBar {current} {devMode} />
    <main
      class="flex-1 overflow-y-auto px-6 py-6"
      data-testid="app-shell-main"
      data-view={selected}
    >
      <div class="mx-auto flex max-w-4xl flex-col gap-6">
        {#if selected === "dashboard"}
          <DashboardView />
        {:else if selected === "terminal"}
          <TerminalView
            launch={activeLaunch}
            onExit={handleTerminalExit}
            onSessionClosed={handleSessionClosed}
            onLastSeenSeqUpdate={handleLastSeenSeqUpdate}
            onReconnectLastSession={handleReconnectLastSession}
            onForgetLastSession={handleForgetLastSession}
          />
        {:else if selected === "sessions"}
          <SessionsView
            onReconnect={handleLaunch}
            activeSessionId={activeLaunch?.sessionId ?? null}
          />

        {:else if selected === "servers"}
          <ServersView onLaunch={handleLaunch} />
        {:else if selected === "identities"}
          <IdentitiesView />
        {:else if selected === "settings"}
          <SettingsView />
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
