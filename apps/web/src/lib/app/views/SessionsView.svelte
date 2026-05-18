<script lang="ts">
  /**
   * Production Terminal Sessions list/status view.
   *
   * Read-only inventory + per-row Reconnect/Close actions. Honesty rules
   * (mirrored in `sessionStatus.ts` and SPEC.md):
   *  - Closed rows cannot be reconnected. The reconnect button is
   *    disabled and the copy never implies otherwise.
   *  - Detached rows include a TTL disclaimer: the remote PTY only
   *    survives the deployment's configured detach-TTL window (read
   *    via `loadSessionPolicy()` from
   *    `GET /api/v1/config/session-policy`; default 30 s, operator-
   *    tunable to 24 h) past the last detach, replay is in-memory, and
   *    a backend restart drops everything.
   *  - The view never shows raw terminal output, replay buffer contents,
   *    or any field that could carry input bytes. Only safe public
   *    metadata (id, profile_id, status, dims, timestamps).
   *
   * One active terminal at a time: the AppShell holds a single
   * `ActiveLaunch`. Reconnect from this list overwrites it. There is no
   * multi-tab workspace; that lands in a later slice.
   */
  import {
    closeTerminalSession,
    describeCloseSessionError,
    describeSessionLoadError,
    listTerminalSessions,
    validateSavedSession,
    type TerminalSession,
  } from "../../api/terminalSessions.js";
  import {
    listServerProfiles,
    type ServerProfile,
  } from "../../api/serverProfiles.js";
  import {
    DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
    describeDetachedTtl,
    formatDetachedTtl,
    loadSessionPolicy,
  } from "../../api/sessionPolicy.js";
  import {
    canClose,
    canReconnect,
    describeSessionStatus,
    showsTtlHint,
    statusLabel,
    statusTone,
  } from "../terminal/sessionStatus.js";
  import type { ActiveLaunch } from "../terminal/activeLaunch.js";

  interface Props {
    /**
     * Hand a session back to the parent shell for attach. The shell
     * sets `ActiveLaunch` and switches to the Terminal view; this
     * component only owns the create/close/load calls.
     */
    onReconnect?: (launch: ActiveLaunch) => void;
    /**
     * Hand a session id back to the parent shell to open the durable
     * recording replay viewer. The shell sets the replay state and
     * renders {@link RecordingReplayView}; this component only owns
     * the click. The button is offered on rows that COULD have a
     * recording (detached / closed) — the actual `has_recording`
     * gate happens inside the replay viewer itself, so we avoid a
     * per-row metadata fetch (no N+1) and surface "No recording
     * available" honestly when the operator opens an empty one.
     */
    onViewRecording?: (sessionId: string, profileLabel: string | null) => void;
    /** Currently active launch, if any. Used to mark the row in the list
     * so the operator knows which session is already attached and to
     * suppress "Reconnect" on the same id. */
    activeSessionId?: string | null;
  }

  let {
    onReconnect,
    onViewRecording,
    activeSessionId = null,
  }: Props = $props();

  /**
   * Whether a row may have a durable recording worth opening the
   * viewer for. The Sessions list deliberately does NOT pre-fetch
   * recording metadata for every row (would be N+1 against
   * `/recording/metadata`); instead we surface the affordance for
   * `detached` / `closed` rows only and let the viewer's metadata
   * gate render "No recording available" if the row turns out
   * empty.
   *
   * `starting` is excluded — the row has no recording yet by
   * definition. `active` is excluded too — the operator should
   * `Open` an active row to attach to the live session, not open a
   * "Replay only" view of the partial recording while the same
   * session is still streaming.
   */
  function offersRecording(status: TerminalSession["status"]): boolean {
    return status === "detached" || status === "closed";
  }

  type LoadState =
    | { kind: "idle" }
    | { kind: "loading" }
    | {
        kind: "ready";
        sessions: TerminalSession[];
        profiles: ServerProfile[];
      }
    | { kind: "error"; summary: string };

  type CloseState =
    | { kind: "submitting" }
    | { kind: "error"; summary: string };

  type OpenState =
    | { kind: "verifying" }
    | { kind: "error"; summary: string };

  let view = $state<LoadState>({ kind: "idle" });
  let closing = $state<Record<string, CloseState>>({});
  /**
   * Effective detached-live-PTY TTL window in seconds. Seeded from the
   * SPEC-pinned default so the view renders honest copy on first paint
   * before the policy fetch resolves; overwritten once
   * `loadSessionPolicy()` lands (falls back to the same default on
   * fetch failure, so this never blocks the view). Multiple consumers
   * share the module-level cache inside `sessionPolicy.ts` — three
   * views mounting at once issue one wire round-trip total.
   */
  let detachedTtlSeconds = $state(DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS);
  /**
   * Per-row state for the Open/Reconnect action. Independent of `closing`
   * so a row may be in mid-verify while another is mid-close. The
   * `verifying` state is short-lived (one HTTP round-trip via
   * {@link validateSavedSession}); the `error` state is dismissable and
   * never echoes wire-side detail (the formatter is a function of
   * `kind`+`status`+`code` ONLY).
   */
  let opening = $state<Record<string, OpenState>>({});

  async function load() {
    view = { kind: "loading" };
    // Fetch sessions + profiles in parallel — the profile list is a
    // best-effort lookup for a friendlier label and is not load-bearing.
    const [sessionsResult, profilesResult] = await Promise.all([
      listTerminalSessions(),
      listServerProfiles(),
    ]);
    if (!sessionsResult.ok) {
      view = {
        kind: "error",
        summary: describeSessionLoadError(sessionsResult.error),
      };
      return;
    }
    // Profiles list failure is silent: we still render the sessions and
    // fall back to the short id form. Surfacing the profile error here
    // would be misleading — the page is about terminal sessions.
    const profiles = profilesResult.ok ? profilesResult.data : [];
    view = { kind: "ready", sessions: sessionsResult.data, profiles };
  }

  $effect(() => {
    void load();
  });

  // Resolve the deployment's configured detached-PTY TTL once and pin
  // the resulting seconds onto state so the header + per-row hint stay
  // honest about what window the orchestrator actually enforces.
  // `loadSessionPolicy` never throws; on transport / HTTP / parse
  // failure it falls back to the SPEC-pinned default, so this $effect
  // CANNOT block or break the view.
  $effect(() => {
    void loadSessionPolicy().then((policy) => {
      detachedTtlSeconds = policy.detached_live_pty_ttl_seconds;
    });
  });

  function profileLabel(profile_id: string, profiles: readonly ServerProfile[]): string {
    const found = profiles.find((p) => p.id === profile_id);
    return found ? found.name : shortId(profile_id);
  }

  /**
   * Render an id as the first 8 hex chars. UUIDs are too long for an
   * inventory row; the short form is enough to disambiguate at a glance
   * and operators can hover the cell for the full id (the `title`
   * attribute on the `<span>`).
   */
  function shortId(id: string): string {
    if (id.length <= 8) return id;
    return id.slice(0, 8);
  }

  async function reconnectClicked(session: TerminalSession, profileName: string) {
    if (!canReconnect(session.status)) return;
    // Pre-handoff validation: a row's local status can be stale (the
    // backend may have closed the session since the last load — explicit
    // close from another tab, PTY exit, TTL elapsed). Verify against the
    // backend BEFORE the handoff so the operator does not watch a
    // WebSocket attach fail seconds later. The check is one cheap HTTP
    // round-trip; the operator sees a brief "Verifying…" state.
    opening = { ...opening, [session.id]: { kind: "verifying" } };
    const validation = await validateSavedSession(session.id);

    if (validation.kind === "reconnectable") {
      // Refresh the row in place so any operator who navigates back to
      // the list sees the freshest status the verify call observed.
      const fresh = validation.session;
      if (view.kind === "ready") {
        view = {
          kind: "ready",
          sessions: view.sessions.map((s) => (s.id === fresh.id ? fresh : s)),
          profiles: view.profiles,
        };
      }
      const next = { ...opening };
      delete next[session.id];
      opening = next;
      onReconnect?.({
        sessionId: fresh.id,
        cols: fresh.cols,
        rows: fresh.rows,
        profileLabel: profileName,
      });
      return;
    }

    if (validation.kind === "uncertain" && validation.reason !== "starting") {
      // Transport / surprising HTTP / malformed response: don't punish
      // the operator for a network blip. Proceed with the handoff and
      // let the WebSocket attach surface its own failure if applicable.
      const next = { ...opening };
      delete next[session.id];
      opening = next;
      onReconnect?.({
        sessionId: session.id,
        cols: session.cols,
        rows: session.rows,
        profileLabel: profileName,
      });
      return;
    }

    // Stale (closed / not_found) OR uncertain "starting": refuse the
    // handoff, surface the reason inline, and refresh the whole list so
    // every row re-syncs against the backend.
    opening = {
      ...opening,
      [session.id]: { kind: "error", summary: validation.summary },
    };
    void load();
  }

  function dismissOpenError(sessionId: string) {
    if (opening[sessionId]?.kind !== "error") return;
    const next = { ...opening };
    delete next[sessionId];
    opening = next;
  }

  async function closeClicked(session: TerminalSession) {
    if (!canClose(session.status)) return;
    closing = { ...closing, [session.id]: { kind: "submitting" } };
    const result = await closeTerminalSession(session.id);
    if (!result.ok) {
      closing = {
        ...closing,
        [session.id]: {
          kind: "error",
          summary: describeCloseSessionError(result.error),
        },
      };
      return;
    }
    // Drop the per-row close state and refresh the row from the parsed
    // close response. The backend returns the post-close session; we
    // replace the local row in place rather than refetching the whole
    // list, which would steal focus and reset scroll on a long list.
    const next = { ...closing };
    delete next[session.id];
    closing = next;
    if (view.kind === "ready") {
      view = {
        kind: "ready",
        sessions: view.sessions.map((s) =>
          s.id === session.id ? result.result.session : s,
        ),
        profiles: view.profiles,
      };
    }
  }

  function dismissCloseError(sessionId: string) {
    if (closing[sessionId]?.kind !== "error") return;
    const next = { ...closing };
    delete next[sessionId];
    closing = next;
  }

  const TONE_DOT_CLASS = {
    neutral: "bg-zinc-500",
    info: "bg-sky-400",
    ok: "bg-emerald-400",
    warn: "bg-amber-400",
    error: "bg-rose-500",
  } as const;

  const TONE_TEXT_CLASS = {
    neutral: "text-zinc-400",
    info: "text-sky-300",
    ok: "text-emerald-300",
    warn: "text-amber-300",
    error: "text-rose-300",
  } as const;
</script>

<section
  class="flex flex-col gap-6"
  data-testid="production-view-sessions"
>
  <header class="flex flex-col gap-1">
    <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
      Terminal sessions
    </h2>
    <p
      class="text-sm text-zinc-400"
      data-testid="sessions-header-blurb"
      data-detached-ttl-seconds={detachedTtlSeconds}
    >
      Live and detached terminal sessions owned by your account. The
      backend owns each session's lifecycle, sequence numbers, and a
      bounded detached reconnect window ({formatDetachedTtl(
        detachedTtlSeconds,
      )}). Replay is in-memory only — a backend restart drops every
      session.
    </p>
  </header>

  <div class="flex flex-wrap items-center gap-3">
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:opacity-50"
      onclick={load}
      disabled={view.kind === "loading"}
      data-testid="sessions-refresh-button"
    >
      {view.kind === "loading" ? "Loading…" : "Refresh"}
    </button>
    <p
      class="text-xs text-zinc-500"
      data-testid="sessions-refresh-note"
    >
      Refresh re-fetches the current backend state. There is no
      auto-refresh or live update yet — closed sessions cannot be
      recovered from this view.
    </p>
  </div>

  {#if view.kind === "idle" || view.kind === "loading"}
    <p class="text-sm text-zinc-400" data-testid="sessions-loading">
      Loading sessions…
    </p>
  {:else if view.kind === "error"}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-sm text-rose-200/80"
      data-testid="sessions-error"
    >
      {view.summary}
    </p>
  {:else if view.sessions.length === 0}
    <article
      class="flex flex-col gap-2 rounded-lg border border-zinc-800 bg-zinc-950/40 p-6"
      data-testid="sessions-empty"
    >
      <h3 class="text-sm font-semibold text-zinc-200">No terminal sessions</h3>
      <p class="text-sm text-zinc-400">
        Launch a session from a server profile under the
        <strong>Server profiles</strong> view. Sessions appear here while
        they are alive — closed rows are kept until the orchestrator
        drops them.
      </p>
    </article>
  {:else}
    <ul
      class="flex flex-col gap-3"
      data-testid="sessions-list"
    >
      {#each view.sessions as session (session.id)}
        {@const tone = statusTone(session.status)}
        {@const profileName = profileLabel(
          session.server_profile_id,
          view.profiles,
        )}
        {@const closingState = closing[session.id]}
        {@const openingState = opening[session.id]}
        {@const isActive = activeSessionId === session.id}
        {@const isVerifying = openingState?.kind === "verifying"}
        <li
          class="flex flex-col gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-4"
          data-testid="sessions-row"
          data-session-id={session.id}
          data-status={session.status}
        >
          <header class="flex flex-wrap items-baseline justify-between gap-3">
            <div class="flex flex-col gap-0.5">
              <h3 class="text-sm font-semibold text-zinc-100">
                {profileName}
              </h3>
              <p class="text-xs text-zinc-500">
                <span class="font-mono" title={session.id}>{shortId(session.id)}</span>
                · profile
                <span
                  class="font-mono"
                  title={session.server_profile_id}
                >{shortId(session.server_profile_id)}</span>
              </p>
            </div>
            <span
              class="inline-flex items-center gap-2 rounded-full border border-zinc-800 bg-zinc-900/60 px-2.5 py-1 text-xs font-medium {TONE_TEXT_CLASS[tone]}"
              data-testid="sessions-row-status"
              data-status={session.status}
            >
              <span
                class="h-2 w-2 rounded-full {TONE_DOT_CLASS[tone]}"
                aria-hidden="true"
              ></span>
              {statusLabel(session.status)}
              {#if isActive}
                <span class="ml-1 text-zinc-500">· attached here</span>
              {/if}
            </span>
          </header>

          <dl class="grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-zinc-400 sm:grid-cols-4">
            <div class="flex flex-col">
              <dt class="text-zinc-500">size</dt>
              <dd class="font-mono text-zinc-200">{session.cols}×{session.rows}</dd>
            </div>
            <div class="flex flex-col">
              <dt class="text-zinc-500">created</dt>
              <dd>
                <time
                  class="font-mono text-zinc-200"
                  datetime={session.created_at}>{session.created_at}</time>
              </dd>
            </div>
            <div class="flex flex-col">
              <dt class="text-zinc-500">last seen</dt>
              <dd>
                <time
                  class="font-mono text-zinc-200"
                  datetime={session.last_seen_at}>{session.last_seen_at}</time>
              </dd>
            </div>
            <div class="flex flex-col">
              <dt class="text-zinc-500">closed</dt>
              <dd>
                {#if session.closed_at}
                  <time
                    class="font-mono text-zinc-200"
                    datetime={session.closed_at}>{session.closed_at}</time>
                {:else}
                  <span class="font-mono text-zinc-200">—</span>
                {/if}
              </dd>
            </div>
          </dl>

          <p class="text-xs text-zinc-400" data-testid="sessions-row-description">
            {describeSessionStatus(session.status, detachedTtlSeconds)}
          </p>

          {#if showsTtlHint(session.status)}
            <p
              class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
              data-testid="sessions-row-ttl-hint"
              data-detached-ttl-seconds={detachedTtlSeconds}
            >
              {describeDetachedTtl(detachedTtlSeconds)}
            </p>
          {/if}

          <!--
            Per-row action buttons. Mobile portrait bumps min-h-9 + py-1.5
            so the affordances clear a fingertip; desktop collapses back
            to compact via `sm:` overrides. Pinned by
            `tests/mobileControlAffordance.test.ts`.
          -->
          <div class="flex flex-wrap items-center gap-2">
            <button
              type="button"
              class="min-h-9 rounded-md border border-indigo-800/60 bg-indigo-900/20 px-3 py-1.5 text-xs text-indigo-100 transition hover:border-indigo-700 hover:bg-indigo-900/40 disabled:cursor-not-allowed disabled:opacity-50 sm:min-h-0 sm:py-1"
              onclick={() => reconnectClicked(session, profileName)}
              disabled={!canReconnect(session.status) || isActive || isVerifying}
              data-testid="sessions-row-reconnect"
              title={canReconnect(session.status)
                ? isActive
                  ? "Already attached in the Terminal view"
                  : "Verifies the session is reachable, then opens it in the Terminal workspace"
                : "Closed sessions cannot be reconnected"}
            >
              {#if isVerifying}
                Verifying…
              {:else if isActive}
                Attached
              {:else}
                Open
              {/if}
            </button>
            <button
              type="button"
              class="min-h-9 rounded-md border border-rose-800/60 bg-rose-900/20 px-3 py-1.5 text-xs text-rose-100 transition hover:border-rose-700 hover:bg-rose-900/40 disabled:cursor-not-allowed disabled:opacity-50 sm:min-h-0 sm:py-1"
              onclick={() => closeClicked(session)}
              disabled={!canClose(session.status) ||
                closingState?.kind === "submitting"}
              data-testid="sessions-row-close"
              title={canClose(session.status)
                ? "Send Close: ends the PTY immediately"
                : "Already closed"}
            >
              {closingState?.kind === "submitting" ? "Closing…" : "Close"}
            </button>
            {#if onViewRecording && offersRecording(session.status)}
              <button
                type="button"
                class="min-h-9 rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1.5 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 sm:min-h-0 sm:py-1"
                onclick={() => onViewRecording?.(session.id, profileName)}
                data-testid="sessions-row-view-recording"
                title="Open the recording replay viewer for this session — read-only output, no live SSH"
              >
                View recording
              </button>
            {/if}
          </div>

          {#if closingState?.kind === "error"}
            <p
              class="flex flex-wrap items-center gap-2 rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
              data-testid="sessions-row-close-error"
            >
              <span>{closingState.summary}</span>
              <button
                type="button"
                class="ml-auto rounded-md border border-rose-800 bg-rose-900/40 px-2 py-0.5 text-[11px] text-rose-100 transition hover:bg-rose-900/60"
                onclick={() => dismissCloseError(session.id)}
              >
                Dismiss
              </button>
            </p>
          {/if}

          {#if openingState?.kind === "error"}
            <p
              class="flex flex-wrap items-center gap-2 rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
              data-testid="sessions-row-open-error"
            >
              <span>{openingState.summary}</span>
              <button
                type="button"
                class="ml-auto rounded-md border border-amber-800 bg-amber-900/40 px-2 py-0.5 text-[11px] text-amber-100 transition hover:bg-amber-900/60"
                onclick={() => dismissOpenError(session.id)}
              >
                Dismiss
              </button>
            </p>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>
