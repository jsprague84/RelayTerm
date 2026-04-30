<script lang="ts">
  /**
   * Dev-only lab for exercising a live SSH PTY through the
   * `@relayterm/terminal-core` client and `@relayterm/terminal-xterm`
   * renderer. This is NOT the production terminal UI; it exists to prove
   * the live-PTY data path renders end-to-end so the production UI slice
   * can be built on top without re-validating the seam.
   *
   * Gated behind `import.meta.env.DEV`. The production bundle's dead-code
   * elimination drops the JS branch (terminal-xterm's `sideEffects` field
   * tree-shakes xterm itself away); the css side-effect import is the
   * documented compromise — see App.svelte.
   *
   * Contracts re-asserted in this file:
   *  - Renderer-neutral: the lab only touches `XtermRenderer` through the
   *    `TerminalRenderer` adapter package. No `@xterm/xterm` import here.
   *  - Output decode is centralised in `@relayterm/terminal-core` via the
   *    `decodeOutputData` helper, wrapped here by `safeDecodeOutput` so a
   *    malformed frame collapses to a typed log line, never an exception.
   *  - Input/output redaction: the diagnostic log NEVER carries raw input
   *    bytes (renderer-driven keystrokes) or raw output bytes (PTY frames).
   *    Length is the only payload-correlated value the log records. The
   *    redaction rule is enforced both here and inside the renderer
   *    adapter, and pinned by tests in `apps/web/tests/labLog.test.ts`,
   *    `apps/web/tests/liveTerminalState.test.ts`, and
   *    `packages/terminal-xterm/tests/xtermRenderer.test.ts`.
   *  - State / button enablement / TTL text / replay formatting all flow
   *    through pure helpers in `liveTerminalState.ts`. The Svelte file
   *    keeps the imperative glue; anything with a contract worth pinning
   *    sits in the helper module.
   */
  import { onDestroy, onMount } from "svelte";
  import {
    TerminalSessionClient,
    WebSocketTerminalTransport,
    type ServerMsg,
    type TerminalClientError,
    type TerminalSessionState,
  } from "@relayterm/terminal-core";
  import { XtermRenderer } from "@relayterm/terminal-xterm";
  import "@relayterm/terminal-xterm/styles";
  import {
    CELL_GRID_MAX,
    CELL_GRID_MIN,
    inputByteLength,
    outputLogText,
    redactInputLogText,
    safeDecodeOutput,
    validateCellGrid,
  } from "./labLog";
  import {
    DETACHED_TTL_MS,
    computeEnablement,
    derivePhase,
    describeTtlWindow,
    formatReplayEnd,
    formatReplayStart,
    formatReplayWindowLost,
    labelForPhase,
    toneForPhase,
    type LabPhase,
  } from "./liveTerminalState.js";

  /**
   * Optional caller-controlled inputs. When `DevTerminalWorkbench`
   * launches a session it remounts this component via `{#key sessionId}`
   * so the props seed the form on first render only — the lab continues
   * to own its `$state` after that. No `$derived` here on purpose: a
   * later prop change must not silently overwrite a session id the
   * operator has been editing. The workbench remounts to push a new id.
   */
  interface Props {
    initialSessionId?: string;
    initialCols?: number;
    initialRows?: number;
    autoConnect?: boolean;
  }
  let {
    initialSessionId = "",
    initialCols = 80,
    initialRows = 24,
    autoConnect = false,
  }: Props = $props();

  interface LogLine {
    id: number;
    direction: "in" | "out" | "info" | "error";
    text: string;
  }

  // svelte-ignore state_referenced_locally
  let sessionId = $state(initialSessionId);
  // svelte-ignore state_referenced_locally
  let cols = $state(initialCols);
  // svelte-ignore state_referenced_locally
  let rows = $state(initialRows);
  let clientState = $state<TerminalSessionState>("idle");
  let log = $state<LogLine[]>([]);
  let nextId = 0;
  /**
   * Highest output `seq` mirrored from the client. Updated from the
   * `output` and `replay_end` event handlers; `0` until the first frame
   * lands. The "reconnect with last seen seq" button reads this so an
   * operator can manually exercise the replay handshake without
   * tracking the bookmark by hand.
   */
  let lastSeenSeq = $state(0);
  /**
   * Set on `replay_start`, cleared on `replay_end` or
   * `replay_window_lost`. Distinguishes "live attached" from "attached
   * but currently catching up the buffered window" in the lab phase.
   * Live `output` frames during replay still flow into the renderer —
   * the orchestrator is the one tagging order, not us.
   */
  let replayActive = $state(false);
  /**
   * Wall-clock instant when the lab observed a detach (server frame OR
   * local disconnect-without-close). Drives the local TTL countdown.
   * Cleared on a fresh `attach`, on explicit `close`, and on `dispose`
   * so a stale countdown doesn't outlive its trigger. The value is a
   * LOCAL clock — `describeTtlWindow` is honest that the backend's
   * exact remaining TTL is not on the wire.
   */
  let detachedAtMs = $state<number | null>(null);
  /**
   * `nowMs` is ticked once per second whenever a TTL window is active.
   * Decoupled from `Date.now()` references in the template so the
   * countdown re-renders on a deterministic cadence; tests don't need
   * a Svelte runtime to exercise the helper because the helper is
   * pure and the component just feeds it `nowMs`.
   */
  let nowMs = $state(Date.now());
  /**
   * Marks the brief window between `teardown()` and the next `attach`
   * resolving. The lab uses it so the phase shows `reconnecting`
   * instead of momentarily flashing `idle`.
   */
  let reconnectInFlight = $state(false);
  let client: TerminalSessionClient | null = null;
  let renderer: XtermRenderer | null = null;
  let unsubInput: (() => void) | null = null;
  let unsubResize: (() => void) | null = null;
  let mountTarget: HTMLDivElement | null = null;

  const phase = $derived<LabPhase>(
    derivePhase({
      clientState,
      replayActive,
      detachedAtMs,
      nowMs,
      reconnectInFlight,
    }),
  );

  const enablement = $derived(
    computeEnablement({
      phase,
      hasSessionId: sessionId.trim().length > 0,
      lastSeenSeq,
    }),
  );

  const ttlText = $derived(describeTtlWindow({ detachedAtMs, nowMs }));

  /**
   * While a TTL countdown is active, tick `nowMs` once a second so the
   * `$derived` countdown re-renders. The interval is owned by this
   * effect — Svelte 5's `$effect` cleanup makes the lifecycle obvious
   * without us reaching for `onDestroy` for this one timer. The null
   * branch returns early WITHOUT a cleanup closure: when the previous
   * run had no interval, there is nothing to tear down; when it did,
   * the previous run's returned closure already ran before this body
   * re-fired (Svelte 5 lifecycle rule).
   */
  $effect(() => {
    if (detachedAtMs === null) {
      return;
    }
    nowMs = Date.now();
    const handle = setInterval(() => {
      nowMs = Date.now();
    }, 1_000);
    return () => {
      clearInterval(handle);
    };
  });

  function append(direction: LogLine["direction"], text: string) {
    log = [...log.slice(-199), { id: nextId++, direction, text }];
  }

  function describeNonOutputServerMsg(msg: Exclude<ServerMsg, { type: "output" }>): string {
    switch (msg.type) {
      case "session_attached":
        return `session_attached (${msg.status}): ${msg.message}`;
      case "ack":
        return `ack ${msg.kind}`;
      case "session_detached":
        return `session_detached attachment=${msg.attachment_id}`;
      case "session_closed":
        return "session_closed";
      case "replay_start":
        return formatReplayStart(msg);
      case "replay_end":
        return formatReplayEnd(msg);
      case "replay_window_lost":
        return formatReplayWindowLost(msg);
      case "error":
        return `error ${msg.code}: ${msg.message}`;
      case "pong":
        return "pong";
    }
  }

  function describeError(err: TerminalClientError): string {
    if (err.kind === "server_error" && err.code) {
      return `server_error code=${err.code} message="${err.message}"`;
    }
    return `${err.kind}: ${err.message}`;
  }

  function buildWsUrl(id: string): string {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const path = `/api/v1/terminal-sessions/${encodeURIComponent(id)}/ws`;
    return `${proto}//${window.location.host}${path}`;
  }

  async function connect(opts: { resumeFromBookmark?: boolean } = {}) {
    if (client) {
      append("info", "already connected; disconnect first");
      return;
    }
    if (!sessionId.trim()) {
      append("error", "session id is required");
      return;
    }
    if (!mountTarget) {
      append("error", "renderer mount point not yet available");
      return;
    }
    const initial = validateCellGrid(cols, rows);
    if (!initial.ok) {
      append("error", `initial cols/rows invalid: ${initial.reason}`);
      return;
    }
    const bookmark =
      opts.resumeFromBookmark && lastSeenSeq > 0 ? lastSeenSeq : undefined;

    const r = new XtermRenderer({
      fontFamily:
        'ui-monospace, "JetBrains Mono", "Fira Code", "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace',
      fontSize: 13,
      cursorBlink: true,
      cursorStyle: "block",
      scrollbackLines: 2000,
      theme: {
        background: "#0a0a0a",
        foreground: "#e4e4e7",
        cursor: "#e4e4e7",
      },
    });
    r.mount(mountTarget);
    r.focus();
    renderer = r;

    const transport = new WebSocketTerminalTransport();
    const next = new TerminalSessionClient({ transport });
    next.on("state_change", (s) => {
      clientState = s;
      append("info", `state → ${s}`);
      if (s === "attached") {
        // A fresh attachment owns the TTL clock now; clear any stale
        // countdown left over from the prior detach. `reconnectInFlight`
        // is reset in the same hop because the outer `attach()` call
        // resolved.
        detachedAtMs = null;
        replayActive = false;
        reconnectInFlight = false;
      } else if (s === "detached" || s === "closed" || s === "error") {
        replayActive = false;
      }
      if (s === "detached" && detachedAtMs === null) {
        // Server-frame detach starts the TTL clock too. The lab uses
        // the same clock for "I dropped the socket myself" and "the
        // server told me you detached."
        detachedAtMs = Date.now();
      }
      if (s === "closed") {
        detachedAtMs = null;
      }
    });
    next.on("attached", (m) => {
      append("in", describeNonOutputServerMsg(m));
      // Drive the renderer to the user-supplied cell grid; xterm fires
      // `onResize` synchronously inside `Terminal.resize`, and the
      // subscriber wired below is the single place that calls
      // `client.sendResize`. Calling `client.sendResize` directly here
      // would emit a duplicate frame.
      r.resize(cols, rows);
    });
    next.on("detached", (m) => append("in", describeNonOutputServerMsg(m)));
    next.on("closed", (m) => append("in", describeNonOutputServerMsg(m)));
    next.on("ack", (m) => append("in", describeNonOutputServerMsg(m)));
    next.on("pong", (m) => append("in", describeNonOutputServerMsg(m)));
    next.on("output", (m) => {
      // Decode base64 → bytes via the centralised helper. A decode
      // failure must never echo the offending payload — the lab logs a
      // static error line and drops the frame so a malformed peer can't
      // crash the renderer.
      const decoded = safeDecodeOutput(m.data);
      if (!decoded.ok) {
        append(
          "error",
          `output seq=${m.seq} discarded: ${decoded.reason}`,
        );
        return;
      }
      append("in", outputLogText(m.seq, decoded.bytes.byteLength));
      r.write(decoded.bytes);
      // Mirror the client's bookmark into local state so the
      // reconnect-with-last-seen-seq button reflects what the operator
      // saw without us having to plumb a getter call into the template.
      if (m.seq > lastSeenSeq) {
        lastSeenSeq = m.seq;
      }
    });
    next.on("replay_start", (m) => {
      replayActive = true;
      append("in", describeNonOutputServerMsg(m));
    });
    next.on("replay_end", (m) => {
      replayActive = false;
      append("in", describeNonOutputServerMsg(m));
      if (m.latest_seq > lastSeenSeq) {
        lastSeenSeq = m.latest_seq;
      }
    });
    next.on("replay_window_lost", (m) => {
      replayActive = false;
      append("in", describeNonOutputServerMsg(m));
      if (m.latest_seq > lastSeenSeq) {
        lastSeenSeq = m.latest_seq;
      }
    });
    next.on("input_rejected_or_stubbed", (rej) =>
      append("info", `${rej.attempted} rejected: ${rej.reason}`),
    );
    next.on("error", (e) => append("error", describeError(e)));

    // Renderer → client. Length is computed off the payload before we
    // hand it to `sendInput`; the redacted log line never sees the
    // bytes themselves. Strings are reported as their UTF-8 byte count
    // (matching what the wire frame would carry); a future binary
    // payload would arrive as `Uint8Array` and the length is its
    // `byteLength`.
    unsubInput = r.onInput((data) => {
      const bytes = inputByteLength(data);
      append("out", redactInputLogText(bytes));
      next.sendInput(
        typeof data === "string" ? data : new TextDecoder().decode(data),
      );
    });
    unsubResize =
      r.onResize?.((size) => {
        cols = size.cols;
        rows = size.rows;
        next.sendResize(size.cols, size.rows);
        append("out", `renderer resize cols=${size.cols} rows=${size.rows}`);
      }) ?? null;

    client = next;
    try {
      await next.attach({
        url: buildWsUrl(sessionId.trim()),
        sessionId: sessionId.trim(),
        clientId: "xterm-live-terminal-lab",
        lastSeenSeq: bookmark,
      });
      append(
        "out",
        bookmark === undefined
          ? "attach frame sent"
          : `attach frame sent with last_seen_seq=${bookmark}`,
      );
    } catch (err) {
      append(
        "error",
        `attach failed: ${err instanceof Error ? err.message : String(err)}`,
      );
      reconnectInFlight = false;
      teardown({ keepDetachClock: false });
    }
  }

  /**
   * Tear down the local client + renderer. `keepDetachClock` controls
   * whether `detachedAtMs` is preserved — `true` for "drop the socket
   * but keep the TTL countdown ticking" (disconnect-no-close); `false`
   * for "we're done with this session" (explicit dispose, after
   * close, after error). The clock is also cleared by the
   * `state_change` handler when a fresh attach lands.
   */
  function teardown(opts: { keepDetachClock?: boolean } = {}) {
    unsubInput?.();
    unsubResize?.();
    unsubInput = null;
    unsubResize = null;
    client?.dispose();
    client = null;
    renderer?.dispose();
    renderer = null;
    clientState = "idle";
    replayActive = false;
    if (!opts.keepDetachClock) {
      detachedAtMs = null;
    }
  }

  function disconnect() {
    teardown({ keepDetachClock: false });
    append("info", "client + renderer disposed");
  }

  /**
   * Drop the WebSocket without sending `Close`, so the session enters
   * the bounded detached-TTL window on the backend. The lab then
   * surfaces the `reconnect with last_seen_seq` button as the resume
   * affordance — exercising the TTL path the operator can use to
   * verify replay end-to-end.
   *
   * Code-wise this is the same teardown sequence as `disconnect()` —
   * the wire-side distinction is only that NEITHER button calls
   * `client.close()` (the wire `Close` frame). The lab labels them
   * separately so an operator can communicate intent in the event
   * log; `client.dispose()` closes the underlying socket cleanly,
   * which the backend treats as a socket-drop (final detach + TTL
   * scheduled), exactly like a normal browser tab close. To actually
   * close the session, use the `close` button (wire `Close` frame).
   *
   * `keepDetachClock=true` so the local TTL countdown starts from
   * NOW. The backend's true remaining TTL is not on the wire; the
   * countdown is labelled `approximate` for that reason.
   */
  function disconnectWithoutClose() {
    if (!client) {
      append("info", "not connected; nothing to disconnect");
      return;
    }
    const at = Date.now();
    teardown({ keepDetachClock: true });
    // Assign AFTER teardown: with `keepDetachClock: true` teardown
    // preserves the existing `detachedAtMs`, but it does NOT seed a
    // new one for a fresh disconnect. We seed it here so the local
    // TTL countdown starts ticking from the moment the operator
    // clicked the button.
    detachedAtMs = at;
    append(
      "info",
      `disconnected without close — server enters TTL window (~${Math.round(
        DETACHED_TTL_MS / 1000,
      )}s); reconnect via lastSeenSeq within that window`,
    );
  }

  /**
   * Tear down the current attachment and immediately reconnect with the
   * highest output seq the lab has observed. Exercises the replay
   * handshake end-to-end without the operator having to track the
   * bookmark by hand. The connect path passes `lastSeenSeq` only when
   * `resumeFromBookmark` is set AND the bookmark is positive; a
   * never-streamed session won't issue a no-op replay request.
   */
  async function reconnectWithBookmark() {
    if (lastSeenSeq <= 0) {
      append("info", "no last_seen_seq yet — nothing to resume from");
      return;
    }
    reconnectInFlight = true;
    teardown({ keepDetachClock: true });
    append("info", `reconnecting with last_seen_seq=${lastSeenSeq}`);
    await connect({ resumeFromBookmark: true });
  }

  /**
   * Reconnect WITHOUT requesting replay. Useful when the operator
   * deliberately wants a fresh attach (after a `replay_window_lost`,
   * after a TTL-elapsed local clock, or just to diff a no-replay path
   * vs the bookmark path). Importantly: this still issues a wire
   * `attach` frame, but with `last_seen_seq: null` — the server will
   * NOT dump pre-attach scrollback (that's product policy per SPEC).
   */
  async function reconnectWithoutBookmark() {
    reconnectInFlight = true;
    teardown({ keepDetachClock: true });
    append("info", "reconnecting without bookmark (no replay request)");
    await connect({ resumeFromBookmark: false });
  }

  function ping() {
    client?.sendPing();
    append("out", "ping");
  }

  function applyResize() {
    const v = validateCellGrid(cols, rows);
    if (!v.ok) {
      append("error", `resize refused: ${v.reason}`);
      return;
    }
    // Renderer resize fires xterm's `onResize` synchronously, which the
    // subscriber translates into `client.sendResize`. We don't fire
    // `client.sendResize` here too — that would double the wire frame.
    // If the renderer isn't mounted (no client either) there is nothing
    // to send; the resize button stays disabled in that state.
    renderer?.resize(cols, rows);
    append("out", `manual resize cols=${cols} rows=${rows}`);
  }

  function detach() {
    client?.detach();
    append("out", "detach");
  }

  function closeSession() {
    client?.close();
    append("out", "close");
  }

  function clearLog() {
    log = [];
  }

  onDestroy(() => {
    teardown({ keepDetachClock: false });
  });

  // Workbench-driven auto-connect: when the parent has just created a
  // session and wants the lab to attach without a manual click, it
  // passes `autoConnect=true` alongside `initialSessionId`. The mount
  // target ref is bound synchronously, so calling `connect()` from
  // `onMount` is the first frame the renderer can mount into. We do
  // NOT re-fire on subsequent prop changes — the workbench remounts
  // this component via `{#key}` when it wants a fresh session.
  onMount(() => {
    if (autoConnect && sessionId.trim().length > 0) {
      void connect();
    }
  });

  // Tone -> Tailwind text class. Kept inline so the lab is the only
  // place that maps the tone enum to a colour; helper module stays
  // pure and renderer-neutral.
  const TONE_CLASS: Record<ReturnType<typeof toneForPhase>, string> = {
    neutral: "text-zinc-200",
    info: "text-sky-300",
    ok: "text-emerald-300",
    warn: "text-amber-300",
    error: "text-rose-300",
  };
</script>

<section class="rounded-md border border-amber-700/60 bg-amber-950/30 p-4 text-sm">
  <header class="flex items-baseline justify-between">
    <h2 class="text-base font-semibold text-amber-200">
      Xterm Live Terminal Lab
    </h2>
    <span class="font-mono text-xs text-amber-400">
      dev-only diagnostic — not the product UI
    </span>
  </header>
  <p class="mt-1 text-xs text-amber-200/80">
    Wires <code>@relayterm/terminal-xterm</code> through
    <code>TerminalSessionClient</code> against a live
    <code>/api/v1/terminal-sessions/:id/ws</code>. Create a session via the
    API first; this lab attaches to an existing id. Terminal data flows on
    the binary <code>RTB1</code> envelope (input + output); JSON carries
    only the control plane (attach/detach/resize/replay/lifecycle). The
    event log redacts both input and output payloads (length + seq only).
  </p>
  <ul class="mt-2 list-disc pl-5 text-xs text-amber-200/70">
    <li>
      <strong>disconnect (no close)</strong> drops the socket without sending
      <code>Close</code>; the server keeps the PTY alive in a bounded
      ~{Math.round(DETACHED_TTL_MS / 1000)}s detached-TTL window.
    </li>
    <li>
      <strong>close</strong> sends the wire <code>Close</code> frame and ends
      the PTY immediately — TTL is bypassed.
    </li>
    <li>
      <strong>reconnect with last_seen_seq</strong> requests buffered output
      newer than the last observed <code>seq</code>. A bookmark older than
      the bounded server-side buffer surfaces as <code>replay_window_lost</code>.
    </li>
    <li>
      <strong>reconnect without bookmark</strong> issues a fresh attach (no
      replay request); the server resumes live fanout from
      <code>latest_seq + 1</code>.
    </li>
    <li>
      A backend restart drops every detached PTY and its replay buffer; a
      reconnect after that always lands on a fresh attach (or a
      <code>409</code> if the row was closed).
    </li>
  </ul>

  <div class="mt-3 grid grid-cols-1 gap-2 sm:grid-cols-3">
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">terminal_session_id</span>
      <input
        type="text"
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        placeholder="uuid"
        bind:value={sessionId}
      />
    </label>
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">cols ({CELL_GRID_MIN}–{CELL_GRID_MAX})</span>
      <input
        type="number"
        min={CELL_GRID_MIN}
        max={CELL_GRID_MAX}
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        bind:value={cols}
      />
    </label>
    <label class="flex flex-col gap-1">
      <span class="text-xs text-zinc-400">rows ({CELL_GRID_MIN}–{CELL_GRID_MAX})</span>
      <input
        type="number"
        min={CELL_GRID_MIN}
        max={CELL_GRID_MAX}
        class="rounded-sm border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono"
        bind:value={rows}
      />
    </label>
  </div>

  <div class="mt-3 flex flex-wrap gap-2">
    <button
      type="button"
      class="rounded-sm bg-emerald-700 px-3 py-1 text-xs hover:bg-emerald-600 disabled:opacity-50"
      onclick={() => void connect()}
      disabled={!enablement.connect}
    >
      connect + attach + mount renderer
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-700 px-3 py-1 text-xs hover:bg-zinc-600 disabled:opacity-50"
      onclick={ping}
      disabled={!enablement.ping}
    >
      ping
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-700 px-3 py-1 text-xs hover:bg-zinc-600 disabled:opacity-50"
      onclick={applyResize}
      disabled={!enablement.applyResize}
    >
      apply resize
    </button>
    <button
      type="button"
      class="rounded-sm bg-amber-700 px-3 py-1 text-xs hover:bg-amber-600 disabled:opacity-50"
      onclick={detach}
      disabled={!enablement.detach}
      title="send wire `Detach` — server replies SessionDetached and starts the TTL window"
    >
      detach
    </button>
    <button
      type="button"
      class="rounded-sm bg-rose-700 px-3 py-1 text-xs hover:bg-rose-600 disabled:opacity-50"
      onclick={closeSession}
      disabled={!enablement.close}
      title="send wire `Close` — ends the PTY immediately, no TTL window"
    >
      close
    </button>
    <button
      type="button"
      class="rounded-sm bg-zinc-800 px-3 py-1 text-xs hover:bg-zinc-700 disabled:opacity-50"
      onclick={disconnect}
      disabled={!enablement.dispose}
    >
      dispose renderer + client
    </button>
    <button
      type="button"
      class="rounded-sm bg-amber-800 px-3 py-1 text-xs hover:bg-amber-700 disabled:opacity-50"
      onclick={disconnectWithoutClose}
      disabled={!enablement.disconnectNoClose}
      title="drop the socket without sending Close — session enters TTL window"
    >
      disconnect (no close)
    </button>
    <button
      type="button"
      class="rounded-sm bg-indigo-700 px-3 py-1 text-xs hover:bg-indigo-600 disabled:opacity-50"
      onclick={() => void reconnectWithBookmark()}
      disabled={!enablement.reconnectWithBookmark}
      title="re-attach with the highest seq seen so far; exercises replay handshake"
    >
      reconnect with last_seen_seq
    </button>
    <button
      type="button"
      class="rounded-sm bg-indigo-800 px-3 py-1 text-xs hover:bg-indigo-700 disabled:opacity-50"
      onclick={() => void reconnectWithoutBookmark()}
      disabled={!enablement.reconnectWithoutBookmark}
      title="re-attach with last_seen_seq=null (no replay request)"
    >
      reconnect without bookmark
    </button>
    <button
      type="button"
      class="ml-auto rounded-sm bg-zinc-800 px-3 py-1 text-xs hover:bg-zinc-700"
      onclick={clearLog}
    >
      clear log
    </button>
  </div>

  <div class="mt-3 flex flex-wrap items-baseline gap-4 text-xs text-zinc-400">
    <span>
      phase:
      <span class={`font-mono ${TONE_CLASS[toneForPhase(phase)]}`}>
        {labelForPhase(phase)}
      </span>
    </span>
    <span>
      client_state: <span class="font-mono text-zinc-200">{clientState}</span>
    </span>
    <span>
      last_seen_seq: <span class="font-mono text-zinc-200">{lastSeenSeq}</span>
    </span>
    {#if ttlText}
      <span class="font-mono text-amber-300">{ttlText.label}</span>
    {/if}
  </div>

  <div
    bind:this={mountTarget}
    class="mt-2 h-72 overflow-hidden rounded-sm border border-zinc-800 bg-black"
  ></div>

  <div
    class="mt-2 max-h-48 overflow-auto rounded-sm border border-zinc-800 bg-zinc-950 p-2 font-mono text-xs"
  >
    {#each log as line (line.id)}
      <div
        class:text-emerald-400={line.direction === "in"}
        class:text-sky-300={line.direction === "out"}
        class:text-zinc-400={line.direction === "info"}
        class:text-rose-400={line.direction === "error"}
      >
        <span class="select-none">[{line.direction}]</span>
        {line.text}
      </div>
    {/each}
    {#if log.length === 0}
      <div class="text-zinc-600">no events yet</div>
    {/if}
  </div>
</section>
