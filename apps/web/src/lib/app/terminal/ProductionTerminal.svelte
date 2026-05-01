<script lang="ts">
  /**
   * Production terminal workspace component. Owns the renderer + client
   * lifecycle for ONE attached session; the parent (`TerminalView`)
   * remounts via `{#key sessionId}` to start a fresh attachment.
   *
   * Architectural rule (load-bearing): xterm baseline only. The
   * production shell does not import ghostty-web, restty, or wterm —
   * the experimental adapters stay in `lib/dev/`. A renderer selector
   * is explicitly out of scope for this slice.
   *
   * Redaction rule (load-bearing):
   *  - Raw input bytes (`renderer.onInput`) flow straight to
   *    `client.sendInput`. They are NEVER logged, stashed in error
   *    messages, or surfaced through the status line.
   *  - Raw output bytes are decoded inside the `output` event handler
   *    and forwarded to `renderer.write` only. The status line shows
   *    metadata (state, last_seen_seq) only — never bytes, decoded
   *    strings, or seq + length pairs that could be reconstructed
   *    into a side channel.
   *  - The wire `message` field of any server error frame is dropped at
   *    the formatter boundary (`describeWorkspaceError`). The same rule
   *    applies to the create-error formatter at the parent.
   *
   * Lifecycle contract:
   *  - On mount: build a renderer + client, attach via WebSocket. The
   *    parent has already POSTed `/api/v1/terminal-sessions` and passed
   *    us the session id; our job is the attach handshake forward.
   *  - On unmount: tear down the local client + renderer. We do NOT
   *    fire a wire `Close` frame — letting the socket drop puts the
   *    session into the bounded detached-TTL window so the operator
   *    can resume from a fresh nav. Explicit close uses the "End
   *    session" button.
   */
  import { onDestroy, onMount } from "svelte";
  import {
    TerminalSessionClient,
    WebSocketTerminalTransport,
    decodeOutputData,
    type TerminalClientError,
    type TerminalSessionState,
  } from "@relayterm/terminal-core";
  import { XtermRenderer } from "@relayterm/terminal-xterm";
  import "@relayterm/terminal-xterm/styles";
  import {
    buildAttachWsUrl,
    computeWorkspaceEnablement,
    DETACHED_TTL_MS,
    derivePhase,
    describeWorkspaceError,
    phaseLabel,
    phaseTone,
    type WorkspacePhase,
  } from "./terminalLaunch.js";
  import {
    loadTerminalSettings,
    settingsToRendererOptions,
  } from "../settings/terminalSettings.js";

  interface Props {
    sessionId: string;
    cols: number;
    rows: number;
    /**
     * Operator-facing label for the session. Just a hint for the status
     * header — usually the originating server profile name. Optional;
     * the workspace is fully usable without it.
     */
    profileLabel?: string;
    /** Called when the user presses the "Back to servers" button. */
    onExit?: () => void;
  }

  let { sessionId, cols, rows, profileLabel, onExit }: Props = $props();

  let clientState = $state<TerminalSessionState | null>(null);
  let replayActive = $state(false);
  let lastSeenSeq = $state(0);
  let lastError = $state<string | null>(null);
  /**
   * `true` once a wire `Close` frame was sent (or HTTP close acked).
   * Used to suppress the "still in TTL" hint on close — the row is gone,
   * not in TTL.
   */
  let closedExplicitly = $state(false);

  let client: TerminalSessionClient | null = null;
  let renderer: XtermRenderer | null = null;
  let unsubInput: (() => void) | null = null;
  let unsubResize: (() => void) | null = null;
  let mountTarget: HTMLDivElement | null = null;
  /**
   * Bumped on EVERY attach AND every explicit teardown so an in-flight
   * WebSocket open from a superseded attach can't reach into the new
   * client (or, in the dispose case, into a torn-down one). For the
   * client we already keep, `client.dispose()` removes the emitter
   * listeners so the generation check is belt-and-suspenders against
   * a future change that defers `dispose`. The bump is centralised in
   * `bumpGeneration()` so dispose / reconnect / fresh-attach all share
   * the same invariant.
   */
  let generation = 0;

  function bumpGeneration(): number {
    generation += 1;
    return generation;
  }

  const phase = $derived<WorkspacePhase>(
    derivePhase({
      clientState,
      replayActive,
      creating: false,
    }),
  );

  const enablement = $derived(
    computeWorkspaceEnablement({ phase, lastSeenSeq }),
  );

  function showsTtlHint(p: WorkspacePhase): boolean {
    return p === "detached" && !closedExplicitly;
  }

  async function attach(opts: { resume?: boolean } = {}) {
    // `mountTarget` is bound synchronously by `bind:this`; an early
    // return here means `attach()` was called before `onMount` fired —
    // which can't happen via the wired-up paths today, but the guard
    // is cheap and keeps the renderer construction below honest.
    if (client || !mountTarget) return;
    const myGen = bumpGeneration();

    // Settings are read once per attach. localStorage is the source of
    // truth; a parse failure or missing entry collapses to defaults
    // silently inside `loadTerminalSettings`. Mid-session live-updates
    // are explicit future work — applying font/theme to a mounted
    // xterm involves more than option-merging (re-fit, atlas reset),
    // so the slice ships "applies on next session" behaviour.
    const settings = loadTerminalSettings();
    const r = new XtermRenderer(settingsToRendererOptions(settings));
    r.mount(mountTarget);
    if (myGen !== generation) {
      r.dispose();
      return;
    }
    r.focus();
    renderer = r;

    const transport = new WebSocketTerminalTransport();
    const next = new TerminalSessionClient({ transport });

    next.on("state_change", (s) => {
      if (myGen !== generation) return;
      clientState = s;
      if (s === "attached") {
        replayActive = false;
        // `attach` cleared the explicit-close flag if it was set on a
        // prior attempt (it isn't on first mount); the TTL hint will
        // re-appear if the next state is `detached`.
        closedExplicitly = false;
        // The renderer was constructed with a default 80×24 grid; pull
        // it up to the requested dims now that the socket is live.
        // xterm fans out to `onResize` synchronously, which is the
        // single place that calls `client.sendResize` — see
        // "Encountered Lessons" in AGENTS.md.
        r.resize(cols, rows);
      }
    });
    next.on("attached", () => {
      if (myGen !== generation) return;
      lastError = null;
    });
    next.on("output", (m) => {
      if (myGen !== generation) return;
      let bytes: Uint8Array;
      try {
        bytes = decodeOutputData(m.data);
      } catch {
        // Drop malformed frames silently — surfacing the seq or length
        // would be metadata-only but adds noise without helping the
        // operator. CRITICAL: do NOT include `m.data` or any error
        // message in any log line; the offending payload may be
        // partially-base64 PTY output, and the redaction rule is the
        // load-bearing one.
        return;
      }
      r.write(bytes);
      if (m.seq > lastSeenSeq) lastSeenSeq = m.seq;
    });
    next.on("replay_start", () => {
      if (myGen !== generation) return;
      replayActive = true;
    });
    next.on("replay_end", (m) => {
      if (myGen !== generation) return;
      replayActive = false;
      if (m.latest_seq > lastSeenSeq) lastSeenSeq = m.latest_seq;
    });
    next.on("replay_window_lost", (m) => {
      if (myGen !== generation) return;
      replayActive = false;
      if (m.latest_seq > lastSeenSeq) lastSeenSeq = m.latest_seq;
    });
    next.on("error", (err: TerminalClientError) => {
      if (myGen !== generation) return;
      lastError = describeWorkspaceError(err);
    });

    unsubInput = r.onInput((data) => {
      // xterm's `onData` always emits `string` today, so the decode
      // branch is forward-safe rather than load-bearing — the
      // `RendererInput` neutral type allows `Uint8Array` so a future
      // adapter (or a binary IME path) is already handled. The
      // payload bytes are NEVER logged or surfaced.
      next.sendInput(
        typeof data === "string" ? data : new TextDecoder().decode(data),
      );
    });
    // `XtermRenderer.onResize` is always defined; the optional chain is
    // defensive coding against the renderer-neutral interface, which
    // marks `onResize` optional. Future renderers that don't expose a
    // resize signal would simply skip this subscription.
    unsubResize =
      r.onResize?.((size) => {
        next.sendResize(size.cols, size.rows);
      }) ?? null;

    client = next;

    const url = buildAttachWsUrl({
      sessionId,
      protocol: window.location.protocol,
      host: window.location.host,
    });
    try {
      await next.attach({
        url,
        sessionId,
        clientId: "relayterm-web",
        lastSeenSeq: opts.resume && lastSeenSeq > 0 ? lastSeenSeq : undefined,
      });
    } catch {
      // The transport `error` event already produced a typed
      // `lastError`; the thrown rejection here is a redundant signal
      // and its `message` is not surfaced (it could include the URL).
      if (myGen === generation) {
        teardownLocal({ keepRenderer: false });
      }
    }
  }

  /**
   * Tear down the local client + renderer without sending a wire
   * `Close` frame. The PTY survives in the backend's bounded
   * detached-TTL window; reconnect within that window resumes from
   * `lastSeenSeq` if it is positive.
   */
  function teardownLocal(opts: { keepRenderer?: boolean } = {}) {
    unsubInput?.();
    unsubResize?.();
    unsubInput = null;
    unsubResize = null;
    client?.dispose();
    client = null;
    if (!opts.keepRenderer) {
      renderer?.dispose();
      renderer = null;
    }
  }

  function detachClicked() {
    client?.detach();
  }

  function closeClicked() {
    closedExplicitly = true;
    client?.close();
  }

  function disposeClicked() {
    // Bump first so any in-flight `attach()` resolution that races with
    // dispose (e.g. dispose during the WebSocket open handshake) sees a
    // stale generation and bails before mutating state. `client.dispose`
    // also removes the emitter subscribers, so the generation check is
    // belt-and-suspenders.
    bumpGeneration();
    teardownLocal({ keepRenderer: false });
    clientState = "idle";
    replayActive = false;
  }

  async function reconnectClicked() {
    // `attach()` bumps the generation itself; we still teardown first
    // so the `client === null` guard inside attach passes.
    teardownLocal({ keepRenderer: false });
    clientState = "idle";
    replayActive = false;
    await attach({ resume: true });
  }

  onMount(() => {
    void attach();
  });

  onDestroy(() => {
    teardownLocal({ keepRenderer: false });
  });

  const PHASE_TONE_CLASS = {
    neutral: "text-zinc-300",
    info: "text-sky-300",
    ok: "text-emerald-300",
    warn: "text-amber-300",
    error: "text-rose-300",
  } as const;
</script>

<section
  class="flex flex-col gap-3"
  data-testid="production-terminal"
  data-session-id={sessionId}
  data-phase={phase}
>
  <header class="flex flex-wrap items-baseline justify-between gap-3">
    <div class="flex flex-col gap-0.5">
      <h2 class="text-lg font-semibold tracking-tight text-zinc-100">
        Terminal session
      </h2>
      <p class="text-xs text-zinc-500">
        {profileLabel ? `${profileLabel} · ` : ""}<span
          class="font-mono">{sessionId}</span
        >
      </p>
    </div>
    <div class="flex flex-wrap items-baseline gap-3 text-xs text-zinc-400">
      <span>
        Status
        <span
          class={`ml-1 font-mono ${PHASE_TONE_CLASS[phaseTone(phase)]}`}
          data-testid="production-terminal-phase"
        >
          {phaseLabel(phase)}
        </span>
      </span>
      <span>
        last_seen_seq
        <span class="font-mono text-zinc-200">{lastSeenSeq}</span>
      </span>
    </div>
  </header>

  <div class="flex flex-wrap gap-2">
    <button
      type="button"
      class="rounded-md border border-amber-700/60 bg-amber-900/20 px-3 py-1 text-xs text-amber-100 transition hover:border-amber-600 hover:bg-amber-900/40 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={detachClicked}
      disabled={!enablement.detach}
      data-testid="production-terminal-detach"
      title="Send Detach: socket drops, PTY survives in the brief detached-TTL window"
    >
      Detach
    </button>
    <button
      type="button"
      class="rounded-md border border-rose-800/60 bg-rose-900/20 px-3 py-1 text-xs text-rose-100 transition hover:border-rose-700 hover:bg-rose-900/40 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={closeClicked}
      disabled={!enablement.close}
      data-testid="production-terminal-close"
      title="Send Close: ends the PTY immediately, no TTL window"
    >
      End session
    </button>
    <button
      type="button"
      class="rounded-md border border-indigo-800/60 bg-indigo-900/20 px-3 py-1 text-xs text-indigo-100 transition hover:border-indigo-700 hover:bg-indigo-900/40 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={() => void reconnectClicked()}
      disabled={!enablement.reconnect}
      data-testid="production-terminal-reconnect"
      title="Re-attach with last_seen_seq; replay covers the gap if the bookmark is still in the bounded buffer"
    >
      Reconnect
    </button>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-800 px-3 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-700 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={disposeClicked}
      disabled={!enablement.dispose}
      data-testid="production-terminal-dispose"
      title="Dispose the local client + renderer without changing the session row"
    >
      Disconnect
    </button>
    {#if onExit}
      <button
        type="button"
        class="ml-auto rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-300 transition hover:border-zinc-600 hover:bg-zinc-800"
        onclick={onExit}
        data-testid="production-terminal-back"
      >
        Back to servers
      </button>
    {/if}
  </div>

  {#if showsTtlHint(phase)}
    <p
      class="rounded-md border border-amber-900/40 bg-amber-950/20 px-3 py-2 text-xs text-amber-200/80"
      data-testid="production-terminal-ttl-hint"
    >
      Detached. The remote PTY remains alive only briefly (~{Math.round(
        DETACHED_TTL_MS / 1000,
      )}s) — reconnect within that window or the session is reaped. Replay is
      in-memory and not durable across a backend restart.
    </p>
  {/if}

  {#if phase === "closed"}
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2 text-xs text-zinc-400"
      data-testid="production-terminal-closed"
    >
      Session ended. Return to the server profile to launch a new one.
    </p>
  {/if}

  {#if lastError}
    <p
      class="rounded-md border border-rose-900/40 bg-rose-950/20 px-3 py-2 text-xs text-rose-200/80"
      data-testid="production-terminal-error"
    >
      {lastError}
    </p>
  {/if}

  <div
    bind:this={mountTarget}
    class="h-[28rem] overflow-hidden rounded-md border border-zinc-800 bg-black"
    data-testid="production-terminal-viewport"
  ></div>
</section>
