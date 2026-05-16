<script lang="ts">
  /**
   * Production terminal workspace component. Owns the renderer + client
   * lifecycle for ONE attached session; the parent (`TerminalView`)
   * remounts via `{#key sessionId}` to start a fresh attachment.
   *
   * Architectural rule (load-bearing): xterm is the production
   * compatibility baseline and the default renderer. The experimental
   * adapters (`@relayterm/terminal-{ghostty-web,restty,wterm}`) are
   * reachable from production ONLY through the gated lazy loader in
   * `./rendererLoader.ts`, and ONLY when the operator has explicitly
   * flipped the experimental-renderer-evaluation gate in Settings.
   * Every fallback path (gate off, unknown id, dynamic import failure)
   * collapses to xterm — the production shell never lands on a
   * renderer the operator did not opt into. xterm remains the only
   * statically-imported adapter; the static-import isolation rule in
   * `apps/web/tests/appShellIsolation.test.ts` pins this.
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
    type TerminalRenderer,
    type TerminalSessionState,
  } from "@relayterm/terminal-core";
  // xterm is the production compatibility baseline — kept as a static
  // import so a default-renderer attach has zero dynamic-import latency
  // and the production bundle never grows on a default path. The
  // experimental adapters are loaded behind the gate via
  // `./rendererLoader.ts`'s dynamic imports.
  import "@relayterm/terminal-xterm/styles";
  import {
    buildAttachWsUrl,
    classifyReconnectAttempt,
    computeFitButtonState,
    computeRendererAutofitStatus,
    computeWorkspaceEnablement,
    derivePhase,
    describeWorkspaceError,
    markRendererInputTarget,
    mountRendererSafely,
    phaseLabel,
    phaseTone,
    RENDERER_MOUNT_FAILED_MESSAGE,
    safeClearViewport,
    safeFit,
    safeFocus,
    TERMINAL_UX_COPY,
    unmarkRendererInputTarget,
    type RendererAutofitStatus,
    type WorkspacePhase,
  } from "./terminalLaunch.js";
  import {
    DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
    describeDetachedTtl,
    loadSessionPolicy,
  } from "../../api/sessionPolicy.js";
  import {
    effectiveRendererId,
    isExperimentalRenderer,
    loadTerminalSettings,
    rendererLabel as describeRenderer,
    settingsToRendererOptions,
    type RendererId,
  } from "../settings/terminalSettings.js";
  import {
    loadRenderer,
    type RendererLoadFallback,
  } from "./rendererLoader.js";
  import {
    evaluatePaste,
    type PasteDecision,
  } from "./pastePolicy.js";
  import {
    formatRelativeMs,
    LAUNCH_TIMING_EVENT_LABELS,
    LAUNCH_TIMING_EVENT_NAMES,
    type LaunchTimingEventName,
    type LaunchTimingRecorder,
    type LaunchTimingSnapshot,
  } from "./terminalLaunchTiming.js";

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
    /**
     * Optional replay bookmark for the very first attach. Set when the
     * mount came from a "Reconnect last session" action backed by the
     * local active-session store; left unset on a fresh launch from a
     * profile row or on a Sessions-list reconnect. The component seeds
     * its `lastSeenSeq` state from this value when it is a positive
     * integer; a `0` / missing value collapses to "no resume" and the
     * wire `attach` skips the replay handshake.
     */
    initialLastSeenSeq?: number;
    /**
     * Client-side launch-timing recorder for this launch attempt.
     * Optional — the saved-session reconnect path cannot synthesize one
     * (the recorder is anchored on the operator click, not on the saved
     * record). When supplied, the workspace marks WebSocket / client
     * events (`ws_connect_started`, `ws_open`, `first_server_message`,
     * `first_output`, `attached`, `detach_requested`, `close_requested`,
     * `ws_close`, `error`) and renders a compact diagnostic strip
     * sourced from the recorder's snapshot. Payload-free by contract —
     * see `terminalLaunchTiming.ts`'s "Redaction posture" comment.
     */
    timing?: LaunchTimingRecorder;
    /** Called when the user presses the "Back to servers" button. */
    onExit?: () => void;
    /**
     * Fires once when the wire signals that the session is closed
     * (server `SessionClosed` frame, post-`End session`, or any
     * lifecycle path that resolves to the `closed` client state). The
     * shell uses it to clear the local active-session pointer.
     */
    onSessionClosed?: () => void;
    /**
     * Fires when the workspace observes a meaningful `lastSeenSeq`
     * transition the shell should persist locally — currently the
     * detached-state edge and `onDestroy`. Called with the latest
     * non-negative seq the workspace has observed; the shell-side
     * helper additionally guards on session-id match so a stale write
     * cannot clobber a fresh launch.
     */
    onLastSeenSeqUpdate?: (seq: number) => void;
  }

  let {
    sessionId,
    cols,
    rows,
    profileLabel,
    initialLastSeenSeq,
    timing,
    onExit,
    onSessionClosed,
    onLastSeenSeqUpdate,
  }: Props = $props();

  /**
   * Reactive mirror of {@link timing}'s snapshot. Initialised to `null`
   * and seeded inside `onMount` from the recorder's current state (so
   * the strip renders the already-recorded `launch_started` +
   * create-POST events on first paint) and refreshed on every
   * `subscribe` callback. The mount-time seed pattern (rather than a
   * `$state(...)` initializer reading the prop) mirrors the
   * `lastSeenSeq` precedent above — Svelte 5's
   * `state_referenced_locally` warning fires on initializers that read
   * props, even though the parent's `{#key sessionId}` block guarantees
   * a remount on session-id change. `null` while pre-mount AND when no
   * recorder was supplied — the diagnostic strip is hidden in either
   * case.
   */
  let timingSnapshot = $state<LaunchTimingSnapshot | null>(null);

  /**
   * Helper used by every event-mark call site so the timing recorder
   * stays optional. Inlined as a closure to keep the call sites
   * readable; the compiler hoists the no-op branch out cleanly when
   * `timing` is unset.
   */
  function recordTiming(name: LaunchTimingEventName): void {
    if (!timing) return;
    timing.mark(name);
  }

  /**
   * Map a {@link TerminalClientError} `kind` to the recorder's closed
   * vocabulary AND mark `error`. The wire / transport `message` field is
   * NEVER read by the recorder — only the `kind` enum is. The recorder
   * dedupes silently; only the first error kind sticks.
   */
  function recordTimingError(kind: TerminalClientError["kind"]): void {
    if (!timing) return;
    timing.markError(kind);
  }

  let clientState = $state<TerminalSessionState | null>(null);
  let replayActive = $state(false);
  /**
   * Effective detached-live-PTY TTL window in seconds. Seeded from the
   * SPEC-pinned default ({@link DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS})
   * so the TTL hint renders honest copy on first paint; overwritten
   * once `loadSessionPolicy()` resolves. The loader falls back to the
   * same default on transport / HTTP / parse failure, so this state
   * NEVER blocks the workspace.
   */
  let detachedTtlSeconds = $state(DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS);
  /**
   * `lastSeenSeq` is seeded inside `onMount` from `initialLastSeenSeq`
   * rather than via the `$state(...)` initializer. The initializer
   * pattern would only capture the initial prop value AND would trigger
   * Svelte 5's `state_referenced_locally` warning even though the
   * parent's `{#key sessionId}` block guarantees a remount on
   * session-id change. Deferring to `onMount` makes the intent
   * explicit: this value is a one-shot seed, never reactive on the
   * prop.
   */
  let lastSeenSeq = $state(0);
  let lastError = $state<string | null>(null);
  /**
   * Local debounce: the `closed` lifecycle edge can fire from multiple
   * paths (server frame, explicit close, dispose race). The shell-level
   * `onSessionClosed` consumer is idempotent today but we still gate
   * here so a regression that adds a side-effect to the consumer cannot
   * trip a double-fire.
   */
  let closeNotified = $state(false);
  /**
   * `true` once a wire `Close` frame was sent (or HTTP close acked).
   * Used to suppress the "still in TTL" hint on close — the row is gone,
   * not in TTL.
   */
  let closedExplicitly = $state(false);
  /**
   * Decision metadata for a paste that triggered the confirm panel.
   * The actual paste content is held in the closure-scoped
   * {@link pendingPasteText} (not in `$state`) so it never enters the
   * reactive graph, never reaches `JSON.stringify(component state)`,
   * and never appears in any rendered DOM beyond the metadata fields
   * the panel exposes (line count, byte length, reason).
   */
  let pendingPasteDecision = $state<PasteDecision | null>(null);
  /**
   * Decision metadata for the most recent blocked paste. Same redaction
   * posture as {@link pendingPasteDecision}: metadata only, content is
   * dropped before this state writes.
   */
  let blockedPasteDecision = $state<PasteDecision | null>(null);

  // Deliberately a plain `let`, not `$state.raw`: no `$derived` reads
  // `client`, so its mutations need not be tracked by Svelte. The
  // asymmetry with `renderer` below is intentional — only `renderer`
  // feeds reactive derivations (`autofitStatus`, `fitButton`).
  let client: TerminalSessionClient | null = null;
  /**
   * Active renderer for the current attach. Typed against the neutral
   * {@link TerminalRenderer} surface so xterm / ghostty-web / restty /
   * wterm all sit behind one variable. `null` between mounts AND while
   * an attach is in flight.
   *
   * Reactive via `$state.raw(...)` (NOT plain `$state(...)`): the
   * downstream `$derived` blocks for `autofitStatus` and `fitButton`
   * read this reference, and a plain `let` would leave them frozen at
   * the initial `null` value (the bug the 2026-05-14 wterm autofit
   * resmoke surfaced — `data-renderer-autofit="unsupported"` even when
   * the mounted renderer's `autofitActive()` returned `true`).
   * `$state.raw` is the right rune here because the renderer adapter
   * classes (XtermRenderer, WtermRenderer, …) use `#`-prefixed private
   * fields; the deep proxy a default `$state` would install cannot
   * forward those field accesses, so any subsequent method call on the
   * adapter would throw a TypeError. `$state.raw` tracks the reference
   * reactively without proxying the underlying instance — reassignments
   * trigger the derivations, mutations on the renderer's internal state
   * are invisible (which is correct: a renderer that resizes its grid
   * or refits should not cascade a `$derived` re-run).
   */
  let renderer = $state.raw<TerminalRenderer | null>(null);
  /**
   * Diagnostic state surfaced via `data-renderer-*` attributes so the
   * staging smoke can prove which renderer was actually mounted, without
   * relying on visual cues. Mirrors {@link RendererLoadResult.rendererId}
   * (the renderer that ended up mounted) and the fallback taxonomy.
   * Never carries payload bytes — operator-facing IDs only.
   */
  let activeRendererId = $state<RendererId | null>(null);
  let activeRendererFallback = $state<RendererLoadFallback | null>(null);
  let experimentalRendererGate = $state(false);
  /**
   * Operator preference for renderer-neutral autofit (the
   * {@link BaseTerminalRendererOptions.autofit} option). Read once per
   * attach from {@link TerminalSettings.autofitEnabled} so a Settings
   * change takes effect on the NEXT attach, same per-attach model as
   * renderer selection. Drives the `data-renderer-autofit` taxonomy
   * AND the Fit-button copy.
   */
  let autofitEnabled = $state(false);
  /**
   * `true` once {@link markRendererInputTarget} has stamped the stable
   * `data-relayterm-terminal-input` marker on the mounted renderer's
   * keyboard-input element. Mirrored onto `data-renderer-input` so the
   * renderer-evaluation smoke can assert "this renderer exposes a
   * stable, renderer-neutral input target" without DOM-walking, then
   * focus + verify `[data-relayterm-terminal-input]` directly. Never
   * carries payload bytes — it is a boolean diagnostic only.
   */
  let rendererInputMarked = $state(false);
  /**
   * Plaintext paste content held between `evaluatePaste` returning a
   * `confirm` decision and the operator confirming/cancelling. Lives at
   * script scope deliberately — never `$state`, never persisted, never
   * logged. Cleared on send / cancel / detach / disconnect / unmount.
   */
  let pendingPasteText: string | null = null;
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

  /**
   * Renderer-neutral autofit status mirrored onto
   * `data-renderer-autofit`. `off` when the operator did not enable
   * autofit; `active` when enabled AND the mounted renderer wired it;
   * `unsupported` when enabled but the mounted renderer no-ops it. The
   * derivation re-runs on every relevant state edge (renderer mount /
   * dispose, mount failure, Settings reload) because the underlying
   * `autofitActive()` call is cheap and pure-boolean — the helper
   * never reads payload bytes.
   */
  const autofitStatus = $derived<RendererAutofitStatus>(
    computeRendererAutofitStatus({ autofitEnabled, renderer }),
  );

  /**
   * Fit-button enablement + tooltip. Disabled when the workspace is not
   * live, when autofit is keeping the grid sized (the one-shot button is
   * redundant), or when the mounted renderer has no `fit()` method. The
   * helper centralises the precedence; the component just consumes it.
   */
  const fitButton = $derived(
    computeFitButtonState({
      liveRenderer: enablement.fit,
      renderer,
      autofitActive: autofitStatus === "active",
    }),
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
    // renderer involves more than option-merging (re-fit, atlas reset),
    // so the slice ships "applies on next session" behaviour.
    //
    // Renderer selection is also a per-attach read: a Settings change
    // (renderer id, experimental gate) takes effect on the NEXT attach,
    // never mid-session. The loader gates every experimental path on
    // both the persisted id AND the operator's explicit
    // `experimentalRendererEvaluationEnabled` opt-in; any other path
    // (unknown id, dynamic-import failure) falls back to xterm with a
    // typed reason that surfaces on `data-renderer-fallback`.
    const settings = loadTerminalSettings();
    experimentalRendererGate = settings.experimentalRendererEvaluationEnabled;
    autofitEnabled = settings.autofitEnabled;
    const requestedRenderer = effectiveRendererId(settings);
    const loadResult = await loadRenderer({
      id: requestedRenderer,
      experimentalEnabled: settings.experimentalRendererEvaluationEnabled,
      options: settingsToRendererOptions(settings),
      cols,
      rows,
    });
    if (myGen !== generation) {
      // A teardown happened while we were awaiting the dynamic import.
      // Discard the freshly-built renderer instead of mounting into a
      // stale target.
      loadResult.renderer.dispose();
      return;
    }
    const r = loadResult.renderer;
    // `mountRendererSafely` translates an async `mount()` rejection
    // into a typed fallback. The 2026-05-13 ghostty-web staging smoke
    // landed on a wedged `data-renderer="unmounted"` /
    // `data-renderer-fallback=""` workspace because the dynamic
    // `import()` resolved cleanly but the adapter's WASM init rejected
    // later inside `r.mount(mountTarget)` (staging nginx CSP blocked
    // the inlined `data:application/wasm;base64,…` URL plus
    // `WebAssembly.compile`). The helper closes that gap; the workspace
    // surfaces the diagnostic AND a remediation message instead of
    // staying silent.
    const mountOutcome = await mountRendererSafely(r, mountTarget);
    if (myGen !== generation) {
      r.dispose();
      return;
    }
    if (mountOutcome.kind === "failed") {
      // Mount rejected. Dispose the half-built renderer so it cannot
      // hold references / DOM nodes; do NOT attempt to mount xterm
      // automatically — the persisted renderer setting belongs to the
      // operator and the recovery is documented in
      // `RENDERER_MOUNT_FAILED_MESSAGE`. Skip client construction and
      // attach entirely; the workspace stays at `idle` but now carries
      // an honest fallback + error panel rather than a silent wedge.
      r.dispose();
      activeRendererId = null;
      activeRendererFallback = mountOutcome.fallback;
      rendererInputMarked = false;
      lastError = RENDERER_MOUNT_FAILED_MESSAGE;
      return;
    }
    // Record the active-renderer diagnostic ONLY after the post-mount
    // generation guard passes — otherwise a teardown that races with
    // the mount could leave `data-renderer` pointing at a renderer the
    // workspace disposed before it ever drew a frame. The SMOKE
    // runbook reads `data-renderer` as proof of which renderer
    // actually mounted; that proof would be unreliable if we wrote it
    // pre-mount.
    activeRendererId = loadResult.rendererId;
    activeRendererFallback = loadResult.fallback ?? null;
    r.focus();
    // Stamp a stable, renderer-neutral marker
    // (`data-relayterm-terminal-input`) on the element that actually
    // receives keyboard input — xterm's hidden helper textarea, or
    // ghostty-web's contenteditable host element. The
    // renderer-evaluation smoke focuses + verifies THIS element instead
    // of guessing between the viewport DIV and a per-renderer helper
    // textarea (the focus-target ambiguity that left the ghostty-web
    // production-shell smoke unable to drive input past the first
    // keystroke). `markRendererInputTarget` only sets one boolean
    // attribute — no payload bytes cross this path; input still flows
    // exclusively through `r.onInput` below.
    rendererInputMarked = markRendererInputTarget(r) !== null;
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
        // Pull browser focus into the renderer once the socket is
        // live so an operator can start typing without an extra click.
        // `safeFocus` swallows the dispose-race case.
        safeFocus(r);
      }
      if (s === "detached") {
        // Persist the latest replay bookmark so a fresh nav can resume
        // within the bounded TTL window. The shell-side helper guards
        // on session-id match; the seq itself is metadata-only.
        onLastSeenSeqUpdate?.(lastSeenSeq);
        // Wire-side disconnect: record `ws_close` regardless of cause
        // (server `Detach`, transport drop, or post-TTL detach). The
        // recorder dedupes; reconnect within the TTL window does NOT
        // re-arm `ws_close`.
        recordTiming("ws_close");
      }
      if (s === "closed") {
        // Mark `ws_close` here AND in the `s === "detached"` branch
        // above so either lifecycle path records the wire-side close
        // moment. The recorder is one-shot per event name (see
        // `terminalLaunchTiming.ts` § "All events are one-shot"), so a
        // detached → closed transition keeps the detached timestamp;
        // a direct closed transition (server `SessionClosed` arriving
        // before any detach state) records here. Both call sites
        // independently mark `ws_close` so dropping one does not
        // silently strand a lifecycle path.
        recordTiming("ws_close");
        if (!closeNotified) {
          closeNotified = true;
          onSessionClosed?.();
        }
      }
    });
    next.on("attached", () => {
      if (myGen !== generation) return;
      lastError = null;
      // The very first server frame is, by protocol contract,
      // `session_attached` — so observing this event also marks
      // `first_server_message`. The recorder dedupes on either name.
      recordTiming("first_server_message");
      recordTiming("attached");
    });
    next.on("output", (m) => {
      if (myGen !== generation) return;
      // Defensive: an `output` frame arriving BEFORE `session_attached`
      // would be a protocol violation, but mark `first_server_message`
      // here too so the recorder dedupes correctly regardless of frame
      // ordering. The mark is name-only — no payload bytes ever reach
      // the recorder.
      recordTiming("first_server_message");
      recordTiming("first_output");
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
      // Closed-vocabulary error kind only; the wire / transport
      // `message` is intentionally NOT read by the recorder.
      recordTimingError(err.kind);
    });

    unsubInput = r.onInput((data) => {
      // xterm's `onData` always emits `string` today, so the decode
      // branch is forward-safe rather than load-bearing — the
      // `RendererInput` neutral type allows `Uint8Array` so a future
      // adapter (or a binary IME path) is already handled. The
      // payload bytes are NEVER logged or surfaced.
      const text =
        typeof data === "string" ? data : new TextDecoder().decode(data);
      const decision = evaluatePaste(text);
      if (decision.risk === "safe") {
        next.sendInput(text);
        return;
      }
      if (decision.risk === "blocked") {
        // Drop the paste; surface metadata only. A pending confirm
        // (if any) is dismissed — the operator's last clipboard
        // action takes precedence.
        pendingPasteText = null;
        pendingPasteDecision = null;
        blockedPasteDecision = decision;
        return;
      }
      // confirm: hold the original text in the closure variable until
      // the operator confirms or cancels. Replaces any prior pending
      // paste so a quick double-paste doesn't strand the first one.
      pendingPasteText = text;
      pendingPasteDecision = decision;
      blockedPasteDecision = null;
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
    // Mark `ws_connect_started` BEFORE the await so the timing snapshot
    // reflects "wire dial began" rather than "wire dial resolved". The
    // recorder NEVER receives the URL — only the event name.
    recordTiming("ws_connect_started");
    try {
      await next.attach({
        url,
        sessionId,
        clientId: "relayterm-web",
        lastSeenSeq: opts.resume && lastSeenSeq > 0 ? lastSeenSeq : undefined,
      });
      // `client.attach()` resolves immediately after the transport's
      // `connect()` promise settles (which fires on WebSocket `open`)
      // AND the attach frame has been pushed onto the wire. This is
      // the closest signal to "WebSocket open observed by the client"
      // we have without changing the transport's public surface;
      // close-vs-open differentiation is the load-bearing measurement
      // for the lifetime_X_then_close verification (see SMOKE.md
      // § "Launch timing diagnostics").
      recordTiming("ws_open");
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
      // Strip the `data-relayterm-terminal-input` marker BEFORE
      // dispose: `focusTarget()` returns null once the renderer is torn
      // down, so this is the last point the marker can be removed from
      // the renderer-owned DOM node. xterm / ghostty-web destroy the
      // element in their own dispose paths anyway, but a future adapter
      // that keeps its host element around must not strand a stale
      // marker on a reusable node.
      unmarkRendererInputTarget(renderer);
      renderer?.dispose();
      renderer = null;
      // The marked input element belonged to the disposed renderer's
      // DOM subtree; clear the diagnostic so it cannot claim a stale
      // input target survives the teardown.
      rendererInputMarked = false;
    }
    // Drop any pending paste content along with the client — without
    // a live client there is nowhere to send it. Cleared regardless of
    // `keepRenderer` since the wire send target is the client, not the
    // renderer.
    pendingPasteText = null;
    pendingPasteDecision = null;
    blockedPasteDecision = null;
  }

  function pasteConfirmClicked() {
    // Snapshot + immediately null out the closure variable so a re-
    // entry (panic-click double-tap) cannot send twice. The decision
    // metadata clears too — the panel hides on the next render.
    const text = pendingPasteText;
    pendingPasteText = null;
    pendingPasteDecision = null;
    if (text === null || !client) return;
    client.sendInput(text);
  }

  function pasteCancelClicked() {
    pendingPasteText = null;
    pendingPasteDecision = null;
  }

  function pasteBlockedDismissClicked() {
    blockedPasteDecision = null;
  }

  function detachClicked() {
    // Drop any pending / blocked paste alongside the wire `Detach` frame.
    // The Send-paste button is already disabled in `detached` because
    // `enablement.detach` flips false, but leaving the panel up is
    // misleading and contradicts the closure-scope contract documented
    // on `pendingPasteText` ("Cleared on send / cancel / detach /
    // disconnect / unmount"). The renderer + client stay alive so a
    // reconnect inside the detached-TTL window can resume.
    pendingPasteText = null;
    pendingPasteDecision = null;
    blockedPasteDecision = null;
    recordTiming("detach_requested");
    client?.detach();
  }

  function closeClicked() {
    // Same redaction posture as `detachClicked`: clear the pending paste
    // state first, then send the wire `Close` frame. The session row is
    // about to terminate, so any held paste content has nowhere to go.
    pendingPasteText = null;
    pendingPasteDecision = null;
    blockedPasteDecision = null;
    closedExplicitly = true;
    recordTiming("close_requested");
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
    // Defence in depth: the button is disabled when
    // `enablement.reconnect` is false (closed phase, idle, etc.), but a
    // state-change race could leave a stale enabled click in flight.
    // Refuse to teardown the renderer / open a fresh WebSocket on a
    // closed session; surface honest copy instead of the generic
    // "connection error" the staging-smoke bug produced.
    const decision = classifyReconnectAttempt({ phase });
    if (decision.kind === "blocked") {
      lastError = decision.summary;
      return;
    }
    // `attach()` bumps the generation itself; we still teardown first
    // so the `client === null` guard inside attach passes.
    teardownLocal({ keepRenderer: false });
    clientState = "idle";
    replayActive = false;
    await attach({ resume: true });
  }

  function focusClicked() {
    safeFocus(renderer);
  }

  function fitClicked() {
    // The renderer's `fit()` synchronously fans out to its `onResize`
    // listeners — that subscription is the single place that drives
    // `client.sendResize` (AGENTS.md "Encountered Lessons"). We
    // deliberately do NOT call `client.sendResize` here.
    safeFit(renderer);
  }

  function clearViewportClicked() {
    // Local viewport + scrollback only. No wire frame; replay buffer
    // is untouched; the remote shell is not asked to run `clear`.
    safeClearViewport(renderer);
  }

  /**
   * Active recorder subscription. `null` when no recorder was supplied
   * OR when the component is unmounting. Cleared in `onDestroy` so the
   * recorder does not retain a closure into a torn-down component.
   */
  let unsubTiming: (() => void) | null = null;

  onMount(() => {
    // Resume from the seeded bookmark when present. The wire-side
    // `attach` already gates on `lastSeenSeq > 0`, so a `0` here
    // collapses to "no resume" and the call is identical to a fresh
    // attach — the explicit `resume` flag just makes the intent clear.
    const seed = initialLastSeenSeq;
    if (typeof seed === "number" && Number.isInteger(seed) && seed > 0) {
      lastSeenSeq = seed;
    }
    void attach({ resume: lastSeenSeq > 0 });

    // Subscribe to the timing recorder so the diagnostic strip updates
    // as events accumulate. The recorder is in-memory only; this
    // listener never reads payload bytes and never writes to storage.
    if (timing) {
      timingSnapshot = timing.snapshot();
      unsubTiming = timing.subscribe((snapshot) => {
        timingSnapshot = snapshot;
      });
    }

    // Fire-and-forget policy lookup so the TTL hint copy stops
    // overclaiming when a deployment runs a non-default window. The
    // loader is failure-safe (default fallback) and module-cached
    // (one wire round-trip across all consumers), so this is cheap
    // and the workspace cannot stall on it.
    void loadSessionPolicy().then((policy) => {
      detachedTtlSeconds = policy.detached_live_pty_ttl_seconds;
    });
  });

  onDestroy(() => {
    // Best-effort persistence of the latest replay bookmark on unmount
    // (e.g. user navigated away). Only emits when we observed live
    // output during the session — `onLastSeenSeqUpdate` itself is a
    // no-op on `seq === 0`, but the shell-side helper costs a load /
    // save per call so we skip the noise.
    if (lastSeenSeq > 0 && !closeNotified) {
      onLastSeenSeqUpdate?.(lastSeenSeq);
    }
    unsubTiming?.();
    unsubTiming = null;
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
  data-renderer={activeRendererId ?? "unmounted"}
  data-renderer-experimental={activeRendererId === null
    ? "false"
    : isExperimentalRenderer(activeRendererId)
      ? "true"
      : "false"}
  data-renderer-fallback={activeRendererFallback ?? ""}
  data-renderer-gate={experimentalRendererGate ? "on" : "off"}
  data-renderer-input={rendererInputMarked ? "marked" : "none"}
  data-renderer-autofit={autofitStatus}
  data-launch-timing={timingSnapshot ? "available" : "none"}
  data-launch-timing-create-post-outcome={timingSnapshot?.createPostOutcome ?? ""}
  data-launch-timing-error-kind={timingSnapshot?.errorKind ?? ""}
  data-launch-timing-ws-open-ms={timingSnapshot?.events.find((e) => e.name === "ws_open")?.relativeMs ?? ""}
  data-launch-timing-ws-close-ms={timingSnapshot?.events.find((e) => e.name === "ws_close")?.relativeMs ?? ""}
  data-launch-timing-first-output-ms={timingSnapshot?.events.find((e) => e.name === "first_output")?.relativeMs ?? ""}
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
      class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={focusClicked}
      disabled={!enablement.focus}
      data-testid="production-terminal-focus"
      title="Move keyboard focus into the terminal viewport"
    >
      Focus terminal
    </button>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={fitClicked}
      disabled={!fitButton.enabled}
      data-testid="production-terminal-fit"
      title={fitButton.tooltip ??
        "Refit the terminal to the container; backend PTY resizes via the renderer's onResize signal"}
    >
      Fit
    </button>
    <button
      type="button"
      class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800 disabled:cursor-not-allowed disabled:opacity-50"
      onclick={clearViewportClicked}
      disabled={!enablement.clear}
      data-testid="production-terminal-clear"
      title="Clear the local viewport and scrollback only — replay buffer and remote shell are untouched"
    >
      Clear local viewport
    </button>
    <span class="mx-1 self-center text-zinc-700" aria-hidden="true">·</span>
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
      data-detached-ttl-seconds={detachedTtlSeconds}
    >
      {describeDetachedTtl(detachedTtlSeconds)}
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

  {#if pendingPasteDecision}
    <div
      class="flex flex-col gap-2 rounded-md border border-amber-700/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-100"
      data-testid="production-terminal-paste-confirm"
      data-paste-reason={pendingPasteDecision.reasonCode}
      role="dialog"
      aria-labelledby="production-terminal-paste-confirm-heading"
    >
      <p
        id="production-terminal-paste-confirm-heading"
        class="font-medium text-amber-100"
        data-testid="production-terminal-paste-confirm-heading"
      >
        {pendingPasteDecision.safeUserMessage}
      </p>
      <p
        class="text-amber-200/80"
        data-testid="production-terminal-paste-confirm-meta"
      >
        {pendingPasteDecision.lineCount} line{pendingPasteDecision.lineCount === 1 ? "" : "s"},
        {pendingPasteDecision.byteLength} byte{pendingPasteDecision.byteLength === 1 ? "" : "s"}.
      </p>
      <p class="text-amber-200/70">
        This will send text directly to the remote shell. Review the source
        before continuing — RelayTerm does not inspect the paste content.
      </p>
      <div class="flex flex-wrap gap-2">
        <!--
          `enablement.detach` is `true` exactly when the session is live
          and can receive input (`attached` or `replaying`); the
          Send-paste button shares that predicate by design. If the
          workspace detaches / closes / errors mid-confirm, the
          affordance disables. `pasteConfirmClicked` also defensively
          re-checks `client` before calling `sendInput`.
        -->
        <button
          type="button"
          class="rounded-md border border-amber-600 bg-amber-800/60 px-3 py-1 text-xs text-amber-50 transition hover:border-amber-500 hover:bg-amber-700/60 disabled:cursor-not-allowed disabled:opacity-50"
          onclick={pasteConfirmClicked}
          disabled={!enablement.detach}
          data-testid="production-terminal-paste-confirm-send"
        >
          Send paste
        </button>
        <button
          type="button"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800"
          onclick={pasteCancelClicked}
          data-testid="production-terminal-paste-confirm-cancel"
        >
          Cancel
        </button>
      </div>
    </div>
  {/if}

  {#if blockedPasteDecision}
    <div
      class="flex flex-col gap-2 rounded-md border border-rose-800/60 bg-rose-950/30 px-3 py-2 text-xs text-rose-100"
      data-testid="production-terminal-paste-blocked"
      data-paste-reason={blockedPasteDecision.reasonCode}
      role="alert"
    >
      <p
        class="font-medium text-rose-100"
        data-testid="production-terminal-paste-blocked-heading"
      >
        {blockedPasteDecision.safeUserMessage}
      </p>
      <p
        class="text-rose-200/80"
        data-testid="production-terminal-paste-blocked-meta"
      >
        {blockedPasteDecision.byteLength} byte{blockedPasteDecision.byteLength === 1 ? "" : "s"} dropped. Nothing was sent to the remote shell.
      </p>
      <div class="flex flex-wrap gap-2">
        <button
          type="button"
          class="rounded-md border border-zinc-700 bg-zinc-900 px-3 py-1 text-xs text-zinc-200 transition hover:border-zinc-600 hover:bg-zinc-800"
          onclick={pasteBlockedDismissClicked}
          data-testid="production-terminal-paste-blocked-dismiss"
        >
          Dismiss
        </button>
      </div>
    </div>
  {/if}

  <div
    bind:this={mountTarget}
    class="h-[28rem] overflow-hidden rounded-md border border-zinc-800 bg-black"
    data-testid="production-terminal-viewport"
  ></div>

  <div class="grid grid-cols-1 gap-2 text-[11px] text-zinc-500 md:grid-cols-2">
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
      data-testid="production-terminal-settings-note"
    >
      <span class="font-medium text-zinc-400">Appearance.</span>
      {TERMINAL_UX_COPY.settingsApplyNote}
    </p>
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2"
      data-testid="production-terminal-copy-paste-note"
    >
      <span class="font-medium text-zinc-400">Copy &amp; paste.</span>
      {TERMINAL_UX_COPY.copyPasteNote}
    </p>
  </div>

  {#if activeRendererId || activeRendererFallback}
    <p
      class="rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2 text-[11px] text-zinc-500"
      data-testid="production-terminal-renderer-diagnostic"
    >
      <span class="font-medium text-zinc-400">Renderer.</span>
      {#if activeRendererId}
        <span class="font-mono text-zinc-300"
          >{describeRenderer(activeRendererId)}</span
        >
        {#if isExperimentalRenderer(activeRendererId)}
          <span class="ml-1 text-amber-300">· experimental</span>
        {/if}
      {:else}
        <!--
          Mount-failure path: no renderer mounted, but the workspace
          still surfaces the typed fallback so the operator and the
          SMOKE runbook share one vocabulary across load + mount
          stages. `activeRendererId === null` is reflected in
          `data-renderer="unmounted"` on the section element.
        -->
        <span class="font-mono text-zinc-400">unmounted</span>
      {/if}
      {#if activeRendererFallback === "experimental_gate_off"}
        <span class="ml-1 text-zinc-400"
          >· experimental gate off — fell back to xterm</span
        >
      {:else if activeRendererFallback === "adapter_load_failed"}
        <span class="ml-1 text-amber-300"
          >· experimental adapter failed to load — fell back to xterm</span
        >
      {:else if activeRendererFallback === "unknown_renderer_id"}
        <span class="ml-1 text-amber-300"
          >· unknown renderer id — fell back to xterm</span
        >
      {:else if activeRendererFallback === "adapter_mount_failed"}
        <span class="ml-1 text-rose-300"
          >· renderer failed to mount — switch back to xterm in Settings and
          reopen the terminal</span
        >
      {/if}
    </p>
  {/if}

  {#if timingSnapshot}
    <!--
      Launch-timing diagnostic strip. Compact, payload-free, in-memory
      only. Designed so a smoke (or an operator) can read the per-event
      relative-ms offsets directly from the DOM without inspecting
      JavaScript state. Rendered as a definition list of event-name +
      formatted ms; events that have not been observed are listed with
      "pending" copy so the absence is unambiguous. The closed-vocabulary
      `data-launch-event` / `data-launch-event-ms` attributes are the
      stable selectors smokes target — see SMOKE.md § "Launch timing
      diagnostics".

      The block is rendered ONLY when a recorder was supplied — the
      saved-session reconnect path (`buildReconnectAttempt`) cannot
      synthesize one, so a reconnect from the empty state silently
      omits the strip. That is the correct shape: the diagnostic is
      anchored on the operator click moment, which is not available
      from a persisted record.
    -->
    <section
      class="flex flex-col gap-1 rounded-md border border-zinc-800 bg-zinc-950/40 px-3 py-2 text-[11px] text-zinc-500"
      data-testid="production-terminal-launch-timing"
      aria-labelledby="production-terminal-launch-timing-heading"
    >
      <p
        id="production-terminal-launch-timing-heading"
        class="font-medium text-zinc-400"
      >
        Launch timing
        {#if timingSnapshot.createPostOutcome}
          <span
            class="ml-1 font-mono text-[10px] text-zinc-500"
            data-testid="production-terminal-launch-timing-post-outcome"
          >· POST {timingSnapshot.createPostOutcome}</span
          >
        {/if}
        {#if timingSnapshot.errorKind}
          <span
            class="ml-1 font-mono text-[10px] text-rose-300"
            data-testid="production-terminal-launch-timing-error-kind"
          >· error: {timingSnapshot.errorKind}</span
          >
        {/if}
      </p>
      <dl
        class="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-0.5 font-mono text-[10px]"
        data-testid="production-terminal-launch-timing-list"
      >
        {#each LAUNCH_TIMING_EVENT_NAMES as eventName (eventName)}
          {@const observed = timingSnapshot.events.find(
            (e) => e.name === eventName,
          )}
          <dt
            class="text-zinc-500"
            data-launch-event={eventName}
            data-launch-event-state={observed ? "observed" : "pending"}
            data-launch-event-ms={observed ? observed.relativeMs : ""}
          >
            {LAUNCH_TIMING_EVENT_LABELS[eventName]}
          </dt>
          <dd class={observed ? "text-zinc-200" : "text-zinc-600"}>
            {observed ? formatRelativeMs(observed.relativeMs) : "pending"}
          </dd>
        {/each}
      </dl>
    </section>
  {/if}
</section>
