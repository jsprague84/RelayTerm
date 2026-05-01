<script lang="ts">
  /**
   * Production terminal workspace view. Two states only:
   *
   *  1. No active launch — show an honest empty state pointing the
   *     operator at the Server profiles view, where the launch action
   *     lives. If the local active-session store has a record from a
   *     previous session, ALSO offer an explicit "Reconnect last
   *     session" affordance — local browser convenience only, gated by
   *     the backend's detached-TTL window. There is NO auto-reconnect:
   *     the affordance always requires an explicit click.
   *  2. Active launch — render `ProductionTerminal`, keyed by
   *     `sessionId` so a fresh launch tears down the previous renderer
   *     and client cleanly.
   *
   * The view is intentionally thin: the per-session lifecycle and the
   * xterm wiring live in `ProductionTerminal.svelte`. This keeps the
   * `AppViewId` switch in `AppShell.svelte` decoupled from the
   * imperative renderer plumbing.
   */
  import ProductionTerminal from "../terminal/ProductionTerminal.svelte";
  import type { ActiveLaunch } from "../terminal/activeLaunch.js";
  import {
    buildReconnectAttempt,
    loadActiveSession,
    shouldOfferReconnect,
    type ActiveSessionRecord,
  } from "../terminal/activeSessionStore.js";
  import { TERMINAL_UX_COPY } from "../terminal/terminalLaunch.js";

  interface Props {
    launch: ActiveLaunch | null;
    onExit?: () => void;
    /**
     * Wire-confirmed close passed up from `ProductionTerminal`. The
     * shell uses it to clear the local active-session pointer.
     */
    onSessionClosed?: () => void;
    /**
     * Replay-bookmark transition passed up from `ProductionTerminal`.
     * The shell uses it to refresh the saved record's `last_seen_seq`.
     */
    onLastSeenSeqUpdate?: (seq: number) => void;
    /**
     * Called when the operator clicks "Reconnect last session". The
     * shell wires this to its launch handler so the same path that
     * services a profile-row launch services the saved-record reconnect
     * (idempotent saved-record refresh, view transition, etc.).
     */
    onReconnectLastSession?: (launch: ActiveLaunch) => void;
    /**
     * Called when the operator clicks "Forget saved session". The shell
     * clears the local pointer; the view re-reads from storage.
     */
    onForgetLastSession?: () => void;
  }

  let {
    launch,
    onExit,
    onSessionClosed,
    onLastSeenSeqUpdate,
    onReconnectLastSession,
    onForgetLastSession,
  }: Props = $props();

  /**
   * The saved-record snapshot the empty state offers as a reconnect
   * affordance. Read once at mount-time — re-navigating between views
   * remounts the AppShell view branch, which remounts this component,
   * which re-reads. We don't poll storage.
   */
  let saved = $state<ActiveSessionRecord | null>(loadActiveSession());

  function reconnectLastClicked() {
    if (saved === null) return;
    onReconnectLastSession?.(buildReconnectAttempt(saved));
  }

  function forgetLastClicked() {
    onForgetLastSession?.();
    saved = null;
  }
</script>

{#if launch}
  {#key launch.sessionId}
    <ProductionTerminal
      sessionId={launch.sessionId}
      cols={launch.cols}
      rows={launch.rows}
      profileLabel={launch.profileLabel}
      initialLastSeenSeq={launch.lastSeenSeq}
      {onExit}
      {onSessionClosed}
      {onLastSeenSeqUpdate}
    />
  {/key}
{:else}
  <section
    class="flex flex-col gap-4 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
    data-testid="production-view-terminal"
  >
    <header class="flex flex-col gap-1">
      <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
        Terminal workspace
      </h2>
      <p class="text-sm text-zinc-400">
        Launch a terminal from a server profile.
      </p>
    </header>
    <ul class="flex flex-col gap-2 text-sm text-zinc-300">
      <li class="flex items-start gap-2">
        <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"></span>
        <span>
          Use the <strong>Server profiles</strong> view to pick a profile,
          then press <strong>Launch terminal</strong>.
        </span>
      </li>
      <li class="flex items-start gap-2">
        <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"></span>
        <span>
          Run host-key trust and SSH auth-check on the profile first; the
          backend will refuse the launch otherwise.
        </span>
      </li>
      <li class="flex items-start gap-2">
        <span class="mt-1 h-1.5 w-1.5 shrink-0 rounded-full bg-zinc-600"></span>
        <span>
          Detached sessions survive only briefly (~30s); replay is
          in-memory and does not survive a backend restart.
        </span>
      </li>
    </ul>

    {#if shouldOfferReconnect(saved, null) && saved}
      <div
        class="flex flex-col gap-2 rounded-md border border-indigo-900/40 bg-indigo-950/20 px-3 py-3 text-xs text-indigo-100/90"
        data-testid="terminal-empty-saved"
        data-saved-session-id={saved.session_id}
      >
        <p class="text-indigo-100/90">
          <strong class="font-semibold">Reconnect last session.</strong>
          Local-only convenience: a saved pointer at your most recent
          terminal session. Reconnect only succeeds while the backend
          runtime is still alive (the bounded detached-TTL window —
          replay is in-memory and does not survive a backend restart).
        </p>
        {#if saved.profile_label}
          <p class="text-[11px] text-indigo-200/70">
            <span class="font-medium">Profile:</span>
            {saved.profile_label}
          </p>
        {/if}
        <p class="text-[11px] text-indigo-200/70">
          <span class="font-medium">Session:</span>
          <span class="font-mono" title={saved.session_id}>
            {saved.session_id.length > 8
              ? saved.session_id.slice(0, 8)
              : saved.session_id}
          </span>
        </p>
        <div class="flex flex-wrap items-center gap-2">
          <button
            type="button"
            class="rounded-md border border-indigo-700 bg-indigo-900/40 px-3 py-1 text-xs text-indigo-100 transition hover:border-indigo-600 hover:bg-indigo-900/60"
            onclick={reconnectLastClicked}
            data-testid="terminal-empty-reconnect-last"
            title="Re-attach the WebSocket to the saved session id; succeeds only while the backend runtime is still alive"
          >
            Reconnect last session
          </button>
          <button
            type="button"
            class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-300 transition hover:border-zinc-600 hover:bg-zinc-800"
            onclick={forgetLastClicked}
            data-testid="terminal-empty-forget-last"
            title="Drop the local pointer without attempting a reconnect"
          >
            Forget saved session
          </button>
        </div>
      </div>
    {/if}

    <div class="grid grid-cols-1 gap-2 text-[11px] text-zinc-500 md:grid-cols-2">
      <p
        class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
        data-testid="terminal-empty-settings-note"
      >
        <span class="font-medium text-zinc-400">Appearance.</span>
        {TERMINAL_UX_COPY.settingsApplyNote}
      </p>
      <p
        class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
        data-testid="terminal-empty-copy-paste-note"
      >
        <span class="font-medium text-zinc-400">Copy &amp; paste.</span>
        {TERMINAL_UX_COPY.copyPasteNote}
      </p>
    </div>
    <p
      class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
    >
      <span class="font-mono uppercase tracking-wide">future work</span> ·
      Multi-tab workspace, durable session list, and a renderer selector
      land in later slices. Today the workspace shows one session at a
      time and uses the xterm baseline only.
    </p>
  </section>
{/if}
