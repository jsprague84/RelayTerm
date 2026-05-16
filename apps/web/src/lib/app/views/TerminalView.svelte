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
  import {
    validateSavedSession,
    type SavedSessionValidation,
  } from "../../api/terminalSessions.js";
  import {
    DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
    formatDetachedTtl,
    loadSessionPolicy,
  } from "../../api/sessionPolicy.js";

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
   * affordance. Read once at mount-time. We don't poll storage —
   * AppShell wraps this component in `{#key activeLaunch?.sessionId
   * ?? "empty"}` so every launch transition (post wire-close, post
   * launch, post reconnect-from-Sessions) unmounts and remounts this
   * component, which re-reads. Without that AppShell wrapper, this
   * cache would stay stale after `handleSessionClosed` runs
   * `clearActiveSession()` — see the comment in `AppShell.svelte` and
   * the regression pin in `tests/appShellIsolation.test.ts`.
   */
  let saved = $state<ActiveSessionRecord | null>(loadActiveSession());

  /**
   * Effective detached-live-PTY TTL window in seconds. Seeded from the
   * SPEC-pinned default so the empty-state copy renders honest text on
   * first paint; overwritten once `loadSessionPolicy()` resolves. The
   * loader is failure-safe (default fallback on transport / HTTP /
   * parse failure), so this state NEVER blocks the empty state.
   */
  let detachedTtlSeconds = $state(DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS);

  $effect(() => {
    void loadSessionPolicy().then((policy) => {
      detachedTtlSeconds = policy.detached_live_pty_ttl_seconds;
    });
  });

  /**
   * Outcome of the optional saved-session validation pass against the
   * backend. Drives whether the affordance is offered, gated, or
   * suppressed. Variants:
   *   - `idle`        → no pointer to validate.
   *   - `checking`    → request in flight; UI shows "Checking…" and
   *                     hides the Reconnect button.
   *   - `reconnectable` → backend confirmed alive + reconnectable.
   *                     UI offers the affordance with no caveat.
   *   - `stale`       → backend says the row is gone or closed. The
   *                     local pointer is cleared (the shell handler
   *                     drops localStorage); the UI shows safe copy
   *                     and no Reconnect button.
   *   - `uncertain`   → transport blip / surprising HTTP / backend
   *                     malformed / row is `starting`. The pointer is
   *                     PRESERVED — operators can still try a manual
   *                     reconnect; the WebSocket will surface its own
   *                     failure if applicable.
   */
  type ValidationState =
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "reconnectable" }
    | { kind: "stale"; summary: string }
    | { kind: "uncertain"; summary: string };

  let validation = $state<ValidationState>({ kind: "idle" });

  /**
   * Validate the saved pointer against the backend at most once per mount.
   * Runs in the background; the affordance renders progressively as the
   * state transitions. NEVER auto-connects (the reconnect attempt is
   * always gated by the explicit button).
   *
   * Failure modes are deliberately split:
   *   - `stale` clears the local pointer (the shell drops localStorage
   *     via `onForgetLastSession`).
   *   - `uncertain` LEAVES the pointer alone — a network blip should
   *     not cost the operator their saved record.
   *
   * Re-run discipline: the effect tracks `saved` because the async branch
   * reads `saved.session_id` synchronously. Within a single mount, `saved`
   * only ever transitions `non-null → null` (forget click, stale outcome),
   * never `non-null → different non-null` — AppShell owns the saved
   * record's identity and remounts this component on every launch
   * transition via the `{#key activeLaunch?.sessionId ?? "empty"}`
   * wrapper around `<TerminalView>`. If a future revision starts mutating
   * `saved` in place to a different session id within one mount, the
   * effect would re-fire and a second validation would race the first;
   * the cancellation flag below would let the second one win cleanly.
   */
  $effect(() => {
    if (saved === null) {
      validation = { kind: "idle" };
      return;
    }
    let cancelled = false;
    validation = { kind: "checking" };
    void (async () => {
      const result: SavedSessionValidation = await validateSavedSession(
        saved.session_id,
      );
      if (cancelled) return;
      if (result.kind === "reconnectable") {
        validation = { kind: "reconnectable" };
        return;
      }
      if (result.kind === "stale") {
        validation = { kind: "stale", summary: result.summary };
        // Drop the persisted pointer — the row is gone on the backend.
        // We KEEP `saved` in memory for one render so the stale notice
        // can show the dropped record's profile/id; the next mount of
        // this view will read `null` from storage and render nothing.
        onForgetLastSession?.();
        return;
      }
      // uncertain: keep the pointer, surface a cautious message.
      validation = { kind: "uncertain", summary: result.summary };
    })();
    return () => {
      cancelled = true;
    };
  });

  function reconnectLastClicked() {
    if (saved === null) return;
    onReconnectLastSession?.(buildReconnectAttempt(saved));
  }

  function forgetLastClicked() {
    onForgetLastSession?.();
    saved = null;
    validation = { kind: "idle" };
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
      timing={launch.timing}
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
        <span
          data-testid="terminal-empty-detached-blurb"
          data-detached-ttl-seconds={detachedTtlSeconds}
        >
          Detached sessions survive for {formatDetachedTtl(
            detachedTtlSeconds,
          )} after the last client drop; replay is in-memory and does
          not survive a backend restart.
        </span>
      </li>
    </ul>

    {#if validation.kind === "stale" && saved}
      <div
        class="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-3 text-xs text-zinc-300"
        data-testid="terminal-empty-saved-stale"
        data-saved-session-id={saved.session_id}
      >
        <p class="text-zinc-300">
          <strong class="font-semibold text-zinc-200">{validation.summary}</strong>
          The local pointer at your most recent terminal session was
          dropped because the backend reports it as gone or already
          closed. Launch a new session from the Server profiles view.
        </p>
        {#if saved.profile_label}
          <p class="text-[11px] text-zinc-500">
            <span class="font-medium">Profile:</span>
            {saved.profile_label}
          </p>
        {/if}
        <p class="text-[11px] text-zinc-500">
          <span class="font-medium">Session:</span>
          <span class="font-mono" title={saved.session_id}>
            {saved.session_id.length > 8
              ? saved.session_id.slice(0, 8)
              : saved.session_id}
          </span>
        </p>
      </div>
    {:else if shouldOfferReconnect(saved, null) && saved}
      <div
        class="flex flex-col gap-2 rounded-md border border-indigo-900/40 bg-indigo-950/20 px-3 py-3 text-xs text-indigo-100/90"
        data-testid="terminal-empty-saved"
        data-saved-session-id={saved.session_id}
        data-validation={validation.kind}
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
        {#if validation.kind === "checking"}
          <p
            class="rounded-md border border-indigo-900/30 bg-indigo-950/40 px-2.5 py-1.5 text-[11px] text-indigo-200/80"
            data-testid="terminal-empty-saved-checking"
          >
            Checking saved session against the backend…
          </p>
        {:else if validation.kind === "uncertain"}
          <p
            class="rounded-md border border-amber-900/40 bg-amber-950/20 px-2.5 py-1.5 text-[11px] text-amber-200/80"
            data-testid="terminal-empty-saved-uncertain"
          >
            {validation.summary} You can still try the reconnect — the
            saved pointer was kept because the failure may be transient.
          </p>
        {/if}
        <div class="flex flex-wrap items-center gap-2">
          <button
            type="button"
            class="rounded-md border border-indigo-700 bg-indigo-900/40 px-3 py-1 text-xs text-indigo-100 transition hover:border-indigo-600 hover:bg-indigo-900/60 disabled:cursor-not-allowed disabled:opacity-50"
            onclick={reconnectLastClicked}
            disabled={validation.kind === "checking"}
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
