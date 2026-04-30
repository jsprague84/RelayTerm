<script lang="ts">
  /**
   * Dev-only lab for exercising a live SSH PTY through the
   * `@relayterm/terminal-core` client against any renderer adapter that
   * implements `TerminalRenderer`. The lab can switch between the
   * baseline `XtermRenderer` (`@relayterm/terminal-xterm`), the
   * experimental `GhosttyWebRenderer` (`@relayterm/terminal-ghostty-web`),
   * the experimental `ResttyRenderer` (`@relayterm/terminal-restty`),
   * and the experimental `WtermRenderer` (`@relayterm/terminal-wterm`)
   * at runtime — switching disposes the previous renderer and remounts.
   * This is NOT the production terminal UI; it exists to prove the
   * live-PTY data path renders end-to-end and that the renderer-neutral
   * seam holds across adapter implementations.
   *
   * Gated behind `import.meta.env.DEV`. The production bundle's
   * dead-code elimination drops the JS branch — terminal-ghostty-web
   * and terminal-restty declare `sideEffects: false`, while
   * terminal-xterm and terminal-wterm pin only their `src/styles.ts`
   * file (and any CSS it pulls in) as side-effectful so Rollup can
   * tree-shake the JS surface even though each adapter re-exports
   * an upstream CSS stylesheet via its own `/styles` entry.
   * ghostty-web and restty ship no CSS at all; xterm and wterm both
   * ship optional CSS (xterm's grid sheet, wterm's `.wterm` host
   * class, theme variables, and selection styling). The lab imports
   * the CSS via the adapter packages
   * (`@relayterm/terminal-xterm/styles`,
   * `@relayterm/terminal-wterm/styles`) so apps/web does not depend
   * on the upstream CSS path directly — pnpm strict mode would
   * otherwise refuse to resolve a transitive-only import.
   *
   * Contracts re-asserted in this file:
   *  - Renderer-neutral: the lab touches every renderer ONLY through
   *    the shared `TerminalRenderer` interface. No `@xterm/xterm`,
   *    `ghostty-web`, `restty`, or `@wterm/dom` import here — those
   *    are encapsulated by the adapter packages.
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
    type TerminalRenderer,
    type TerminalSessionState,
  } from "@relayterm/terminal-core";
  import { XtermRenderer } from "@relayterm/terminal-xterm";
  import "@relayterm/terminal-xterm/styles";
  import { GhosttyWebRenderer } from "@relayterm/terminal-ghostty-web";
  import { ResttyRenderer } from "@relayterm/terminal-restty";
  import { WtermRenderer } from "@relayterm/terminal-wterm";
  // wterm renders into the DOM via CSS-themed cells; the side-effect
  // import wires the `.wterm` host class, theme variables, and
  // selection styling. The adapter package re-exports the upstream
  // CSS through its own `/styles` entry — apps/web does not depend
  // on `@wterm/dom` directly, so importing it here would crash pnpm's
  // strict resolver. Restricted to the dev-lab module so a production
  // build without the lab tree-shakes the import.
  import "@relayterm/terminal-wterm/styles";
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
  import {
    createRendererDiagnostics,
    markDispose,
    markMountEnd,
    markMountStart,
    recordAttached,
    recordClosed,
    recordDetached,
    recordError,
    recordInput,
    recordLastSeenSeq,
    recordOutput,
    recordPing,
    recordPong,
    recordReplayEnd,
    recordReplayStart,
    recordReplayWindowLost,
    recordResizeAck,
    recordResizeSend,
    rendererLabel as diagnosticsRendererLabel,
    resetRendererDiagnostics,
    setClientState as setDiagnosticsClientState,
    setRenderer as setDiagnosticsRenderer,
    summarizeDiagnosticsAsJson,
    type RendererId,
  } from "./rendererDiagnostics.js";

  /**
   * Optional caller-controlled inputs. When `DevTerminalWorkbench`
   * launches a session it remounts this component via `{#key sessionId}`
   * so the props seed the form on first render only — the lab continues
   * to own its `$state` after that. No `$derived` here on purpose: a
   * later prop change must not silently overwrite a session id the
   * operator has been editing. The workbench remounts to push a new id.
   */
  /**
   * Stable identifiers for the swappable renderer adapters. xterm
   * remains the compatibility baseline; ghostty-web is an experimental
   * libghostty-vt-via-WASM adapter; restty is an experimental
   * libghostty-vt + WebGPU/WebGL2 adapter via its xterm-compat shim;
   * wterm is the experimental DOM/mobile/accessibility-oriented
   * adapter built on `@wterm/dom`'s Zig+WASM core wrapped by a
   * CSS-themed grid renderer. The adapter contract (`TerminalRenderer`)
   * is identical for all of them — switching only flips which
   * constructor we call at attach time. The id type and the
   * operator-facing label both come from `rendererDiagnostics.ts` so
   * the diagnostics summary and the lab UI never disagree on names.
   */
  type RendererChoice = RendererId;
  const rendererLabel = diagnosticsRendererLabel;

  function newRenderer(
    choice: RendererChoice,
    grid: { cols: number; rows: number },
  ): TerminalRenderer {
    const themed = {
      fontFamily:
        'ui-monospace, "JetBrains Mono", "Fira Code", "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace',
      fontSize: 13,
      cursorBlink: true,
      cursorStyle: "block" as const,
      scrollbackLines: 2000,
      theme: {
        background: "#0a0a0a",
        foreground: "#e4e4e7",
        cursor: "#e4e4e7",
      },
    };
    switch (choice) {
      case "xterm":
        return new XtermRenderer(themed);
      case "ghostty-web":
        // ghostty-web has no analogue for `lineHeight`; the adapter
        // accepts it on the neutral surface and silently drops it.
        return new GhosttyWebRenderer(themed);
      case "restty":
        // restty's xterm-compat shim accepts cols/rows on construction;
        // the neutral cosmetic knobs (font/cursor/theme/scrollback) are
        // documented as silently dropped on this adapter — see
        // `packages/terminal-restty/src/options.ts`.
        return new ResttyRenderer({ ...themed, cols: grid.cols, rows: grid.rows });
      case "wterm":
        // wterm theming/typography goes through CSS variables on the
        // `.wterm` host element rather than constructor options; the
        // adapter accepts the neutral cosmetic knobs and silently
        // drops them — see `packages/terminal-wterm/src/options.ts`.
        // `cursorBlink` is the one cosmetic knob wterm consumes via
        // the constructor (it toggles a CSS class). `autoResize`
        // defaults to `false` so the lab's explicit cols/rows controls
        // drive sizing for parity with the other adapters.
        return new WtermRenderer({ ...themed, cols: grid.cols, rows: grid.rows });
    }
  }

  interface Props {
    initialSessionId?: string;
    initialCols?: number;
    initialRows?: number;
    autoConnect?: boolean;
    initialRenderer?: RendererChoice;
  }
  let {
    initialSessionId = "",
    initialCols = 80,
    initialRows = 24,
    autoConnect = false,
    initialRenderer = "xterm",
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
  // svelte-ignore state_referenced_locally
  let rendererChoice = $state<RendererChoice>(initialRenderer);
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
  /**
   * Renderer/session diagnostics counters. The state record lives in
   * `rendererDiagnostics.ts` and is intentionally pure: every mutation
   * goes through a function that takes a metadata-only argument list
   * (byte counts, seq numbers, never payloads). The Svelte 5 runic
   * proxy detects deep mutations so the panel re-renders without us
   * threading a `snapshot()` getter through the template.
   *
   * This panel is dev diagnostic tooling — NOT a benchmark suite.
   * Browser, machine, renderer, font, and workload all affect numbers;
   * the lab UI repeats this disclaimer next to any timing.
   */
  // svelte-ignore state_referenced_locally
  let diagnostics = $state(
    createRendererDiagnostics({ renderer: initialRenderer }),
  );
  /**
   * Last clipboard-copy attempt status, surfaced inline in the panel.
   * `fallback` is the catch-all for "clipboard API unavailable OR
   * `writeText` rejected" — at the dev-lab level we don't distinguish
   * "no secure context" from "permission denied" because the operator
   * remediation is the same: copy from the event log. A separate
   * `error` state would be dead code today; if a future slice surfaces
   * a more nuanced clipboard error path, add it then.
   */
  let copyStatus = $state<"idle" | "ok" | "fallback">("idle");
  let client: TerminalSessionClient | null = null;
  let renderer: TerminalRenderer | null = null;
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

    const choice = rendererChoice;
    const r = newRenderer(choice, { cols, rows });
    // `mount` may return a Promise for renderers that load WASM
    // (ghostty-web). Awaiting unconditionally is safe — the xterm
    // adapter is sync and resolves immediately. Diagnostics bracket
    // the mount call so the panel can show "this renderer took N ms
    // to mount" — a coarse, dev-only signal, not a benchmark.
    setDiagnosticsRenderer(diagnostics, choice);
    markMountStart(diagnostics);
    await r.mount(mountTarget);
    markMountEnd(diagnostics);
    r.focus();
    renderer = r;
    append(
      "info",
      `renderer mounted: ${rendererLabel(choice)} (${diagnostics.mountDurationMs ?? "?"}ms)`,
    );

    const transport = new WebSocketTerminalTransport();
    const next = new TerminalSessionClient({ transport });
    next.on("state_change", (s) => {
      clientState = s;
      setDiagnosticsClientState(diagnostics, s);
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
      recordAttached(diagnostics);
      append("in", describeNonOutputServerMsg(m));
      // Drive the renderer to the user-supplied cell grid; xterm fires
      // `onResize` synchronously inside `Terminal.resize`, and the
      // subscriber wired below is the single place that calls
      // `client.sendResize`. Calling `client.sendResize` directly here
      // would emit a duplicate frame.
      r.resize(cols, rows);
    });
    next.on("detached", (m) => {
      recordDetached(diagnostics);
      append("in", describeNonOutputServerMsg(m));
    });
    next.on("closed", (m) => {
      recordClosed(diagnostics);
      append("in", describeNonOutputServerMsg(m));
    });
    next.on("ack", (m) => {
      // The protocol's only ack kind today is `resize`. `recordResizeAck`
      // is keyed off `kind` defensively so a future ack kind doesn't
      // silently inflate the resize counter.
      if (m.kind === "resize") {
        recordResizeAck(diagnostics);
      }
      append("in", describeNonOutputServerMsg(m));
    });
    next.on("pong", (m) => {
      recordPong(diagnostics);
      append("in", describeNonOutputServerMsg(m));
    });
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
      // Diagnostics counter takes ONLY seq + byteLength; the bytes
      // themselves never enter the diagnostics surface. The renderer
      // is the only consumer of the decoded payload below.
      recordOutput(diagnostics, m.seq, decoded.bytes.byteLength);
      r.write(decoded.bytes);
      // Mirror the client's bookmark into local state so the
      // reconnect-with-last-seen-seq button reflects what the operator
      // saw without us having to plumb a getter call into the template.
      if (m.seq > lastSeenSeq) {
        lastSeenSeq = m.seq;
      }
      recordLastSeenSeq(diagnostics, m.seq);
    });
    next.on("replay_start", (m) => {
      replayActive = true;
      recordReplayStart(diagnostics);
      append("in", describeNonOutputServerMsg(m));
    });
    next.on("replay_end", (m) => {
      replayActive = false;
      recordReplayEnd(diagnostics);
      append("in", describeNonOutputServerMsg(m));
      if (m.latest_seq > lastSeenSeq) {
        lastSeenSeq = m.latest_seq;
      }
      recordLastSeenSeq(diagnostics, m.latest_seq);
    });
    next.on("replay_window_lost", (m) => {
      replayActive = false;
      recordReplayWindowLost(diagnostics);
      append("in", describeNonOutputServerMsg(m));
      if (m.latest_seq > lastSeenSeq) {
        lastSeenSeq = m.latest_seq;
      }
      recordLastSeenSeq(diagnostics, m.latest_seq);
    });
    next.on("input_rejected_or_stubbed", (rej) =>
      append("info", `${rej.attempted} rejected: ${rej.reason}`),
    );
    next.on("error", (e) => {
      recordError(diagnostics);
      append("error", describeError(e));
    });

    // Renderer → client. Length is computed off the payload before we
    // hand it to `sendInput`; the redacted log line never sees the
    // bytes themselves. Strings are reported as their UTF-8 byte count
    // (matching what the wire frame would carry); a future binary
    // payload would arrive as `Uint8Array` and the length is its
    // `byteLength`.
    unsubInput = r.onInput((data) => {
      const bytes = inputByteLength(data);
      // Diagnostics counter takes ONLY the byte count; the payload is
      // forwarded to the client below but never enters the
      // diagnostics surface. Same redaction rule as `redactInputLogText`.
      recordInput(diagnostics, bytes);
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
        recordResizeSend(diagnostics);
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
    // `markDispose` is only stamped when a renderer actually existed.
    // Calling `teardown()` from a never-mounted state (e.g. `connect`
    // bailed before `r.mount` resolved) must not inflate the dispose
    // counter — the diagnostics summary tracks adapter dispose calls,
    // not lab-internal cleanup hops.
    if (renderer !== null) {
      markDispose(diagnostics);
    }
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
    recordPing(diagnostics);
    append("out", "ping");
  }

  function applyResize() {
    const v = validateCellGrid(cols, rows);
    if (!v.ok) {
      append("error", `resize refused: ${v.reason}`);
      return;
    }
    // Renderer resize fires the renderer's `onResize` synchronously
    // (xterm, ghostty-web, restty, and wterm all fan out within
    // their underlying terminal's `resize` call), which the
    // subscriber translates into `client.sendResize`. We don't fire
    // `client.sendResize` here too — that would double the wire
    // frame. If the renderer isn't mounted (no client either) there
    // is nothing to send; the resize button stays disabled in that
    // state.
    renderer?.resize(cols, rows);
    append("out", `manual resize cols=${cols} rows=${rows}`);
  }

  /**
   * Switch renderer adapters. While idle the choice is recorded for the
   * next `connect()`. While attached, we tear down the current
   * client+renderer and immediately reconnect with the new renderer
   * choice, exercising the renderer-neutral seam end-to-end. The event
   * log records ONLY the new renderer name — never any payload — so
   * the redaction rule still holds.
   *
   * `reconnectInFlight` is cleared in a `finally` so a synchronous
   * throw out of `connect()` (for example a renderer `mount()` that
   * rejects because ghostty-web's or wterm's WASM init failed)
   * cannot leave the UI permanently stuck in "reconnecting…".
   * `connect()`'s own
   * happy-path resets `reconnectInFlight` via the `state_change →
   * attached` handler; the `finally` here is a belt-and-suspenders
   * safety net for the throw paths it doesn't cover.
   *
   * Race note: rapid consecutive switches can fire two overlapping
   * `connect()` calls (the second sees `client === null` after the
   * first's teardown but before the first's `attach` resolves). For a
   * dev lab this is acceptable — the operator can stop and reset.
   * Productizing renderer-swap UX is out of scope for this slice.
   */
  async function setRendererChoice(next: RendererChoice) {
    if (next === rendererChoice) return;
    rendererChoice = next;
    // Mirror the operator's choice into the diagnostics panel
    // immediately. Without this, the panel's `renderer` field would
    // continue to show the last MOUNTED renderer until the next
    // `connect()`, which contradicts the field's docstring ("currently
    // selected renderer"). On the IDLE branch below, the mount-duration
    // counters are not disturbed because no mount happens. On the
    // LIVE-RECONNECT branch the subsequent `connect()` will write the
    // same renderer id again at the mount-time call site AND then call
    // `markMountStart`/`markMountEnd`, which is the mount-duration
    // update path; the early write here is harmlessly redundant in that
    // case (idempotent), it is not a substitute for the mount-time
    // bookkeeping.
    setDiagnosticsRenderer(diagnostics, next);
    if (!client) {
      append("info", `renderer set to ${rendererLabel(next)} (idle)`);
      return;
    }
    reconnectInFlight = true;
    teardown({ keepDetachClock: false });
    append(
      "info",
      `switching renderer to ${rendererLabel(next)}; reconnecting`,
    );
    try {
      await connect();
    } catch (err) {
      append(
        "error",
        `renderer switch failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    } finally {
      reconnectInFlight = false;
    }
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

  /**
   * Reset the diagnostics counters without disposing the current
   * renderer/client. The currently selected renderer and observed
   * client state are preserved — see `resetRendererDiagnostics`. Used
   * by the operator to start a fresh measurement window mid-session.
   */
  function resetDiagnostics() {
    resetRendererDiagnostics(diagnostics);
    copyStatus = "idle";
    append("info", "diagnostics counters reset");
  }

  /**
   * Copy the diagnostics summary to the clipboard as JSON. The summary
   * is metadata-only — see `summarizeDiagnostics` — so the resulting
   * clipboard string carries no payload bytes by construction. The
   * `navigator.clipboard` API can fail (no secure context, denied
   * permission); in that case the lab logs the JSON to the event log
   * so the operator can copy it from there as a fallback.
   */
  async function copyDiagnostics() {
    const json = summarizeDiagnosticsAsJson(diagnostics);
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      try {
        await navigator.clipboard.writeText(json);
        copyStatus = "ok";
        append("info", "diagnostics summary copied to clipboard");
        return;
      } catch {
        // fall through to the fallback path
      }
    }
    copyStatus = "fallback";
    // The summary itself is metadata-only. Logging it is safe — the
    // redaction rule still holds because no payload byte was ever in
    // the summary in the first place.
    append("info", `diagnostics summary (clipboard unavailable): ${json}`);
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

<section
  class="rounded-md border border-amber-700/60 bg-amber-950/30 p-4 text-sm"
  data-testid="xterm-live-terminal-lab"
>
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

  <div
    class="mt-3 flex flex-wrap items-baseline gap-2 text-xs"
    data-testid="renderer-selector"
  >
    <span class="text-zinc-400">renderer:</span>
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="renderer"
        value="xterm"
        data-testid="renderer-option-xterm"
        checked={rendererChoice === "xterm"}
        onchange={() => void setRendererChoice("xterm")}
      />
      <span class="font-mono text-zinc-200">xterm baseline</span>
    </label>
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="renderer"
        value="ghostty-web"
        data-testid="renderer-option-ghostty-web"
        checked={rendererChoice === "ghostty-web"}
        onchange={() => void setRendererChoice("ghostty-web")}
      />
      <span class="font-mono text-amber-300">ghostty-web (experimental)</span>
    </label>
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="renderer"
        value="restty"
        data-testid="renderer-option-restty"
        checked={rendererChoice === "restty"}
        onchange={() => void setRendererChoice("restty")}
      />
      <span class="font-mono text-amber-300">restty (experimental)</span>
    </label>
    <label class="inline-flex items-center gap-1">
      <input
        type="radio"
        name="renderer"
        value="wterm"
        data-testid="renderer-option-wterm"
        checked={rendererChoice === "wterm"}
        onchange={() => void setRendererChoice("wterm")}
      />
      <span class="font-mono text-amber-300">wterm (experimental)</span>
    </label>
    <span class="text-zinc-500">— switching disposes the current renderer and remounts</span>
    <p class="basis-full text-xs text-zinc-500">
      <strong>wterm</strong> renders into the DOM (Zig+WASM core, CSS-themed grid),
      so selection, copy/paste, IME composition, and mobile soft
      keyboards flow through the platform's native text-handling
      primitives — that's the entire reason wterm is the
      mobile/accessibility-oriented experiment. Theming and font
      controls go through the <code>.wterm</code> CSS host (see
      <code>@wterm/dom/src/terminal.css</code>); the neutral cosmetic
      options accepted by the adapter are silently dropped on this
      renderer.
    </p>
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

  <section
    class="mt-3 rounded-sm border border-zinc-800 bg-zinc-950/60 p-2 text-xs"
    aria-label="renderer diagnostics"
    data-testid="renderer-diagnostics"
  >
    <header class="flex flex-wrap items-baseline justify-between gap-2">
      <span class="font-semibold text-zinc-200">renderer diagnostics</span>
      <span class="text-zinc-500">
        dev diagnostics, not a benchmark — browser/machine/renderer/font/workload all affect numbers
      </span>
    </header>
    <dl class="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 font-mono sm:grid-cols-3 lg:grid-cols-4">
      <div>
        <dt class="text-zinc-400">renderer</dt>
        <dd class="text-zinc-200">
          {diagnostics.rendererId === null
            ? "none"
            : rendererLabel(diagnostics.rendererId)}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">phase</dt>
        <dd class={TONE_CLASS[toneForPhase(phase)]}>{labelForPhase(phase)}</dd>
      </div>
      <div>
        <dt class="text-zinc-400">client_state</dt>
        <dd class="text-zinc-200">{diagnostics.clientState ?? "—"}</dd>
      </div>
      <div>
        <dt class="text-zinc-400">mount duration</dt>
        <dd class="text-zinc-200">
          {diagnostics.mountDurationMs === null
            ? "—"
            : `${diagnostics.mountDurationMs}ms`}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">mounts / disposes</dt>
        <dd class="text-zinc-200">
          {diagnostics.mountCount} / {diagnostics.disposeCount}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">input frames / bytes</dt>
        <dd class="text-zinc-200">
          {diagnostics.inputFrames} / {diagnostics.inputBytes}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">output frames / bytes</dt>
        <dd class="text-zinc-200">
          {diagnostics.outputFrames} / {diagnostics.outputBytes}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">last_output_seq</dt>
        <dd class="text-zinc-200">{diagnostics.lastOutputSeq}</dd>
      </div>
      <div>
        <dt class="text-zinc-400">last_seen_seq</dt>
        <dd class="text-zinc-200">{diagnostics.lastSeenSeq}</dd>
      </div>
      <div>
        <dt class="text-zinc-400">resize sends / acks</dt>
        <dd class="text-zinc-200">
          {diagnostics.resizeSends} / {diagnostics.resizeAcks}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">ping / pong</dt>
        <dd class="text-zinc-200">
          {diagnostics.pingCount} / {diagnostics.pongCount}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">replay s/e/lost</dt>
        <dd class="text-zinc-200">
          {diagnostics.replayStartCount}/{diagnostics.replayEndCount}/{diagnostics.replayWindowLostCount}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">attach / detach / close</dt>
        <dd class="text-zinc-200">
          {diagnostics.attachCount}/{diagnostics.detachCount}/{diagnostics.closeCount}
        </dd>
      </div>
      <div>
        <dt class="text-zinc-400">errors</dt>
        <dd class={diagnostics.errorCount > 0 ? "text-rose-300" : "text-zinc-200"}>
          {diagnostics.errorCount}
        </dd>
      </div>
    </dl>
    <p class="mt-2 text-zinc-500">
      Renderer scrollback is NOT preserved across renderer switches in this slice — switching disposes the previous renderer's grid.
    </p>
    <div class="mt-2 flex flex-wrap gap-2">
      <button
        type="button"
        class="rounded-sm bg-zinc-700 px-2 py-1 text-xs hover:bg-zinc-600"
        onclick={resetDiagnostics}
      >
        reset diagnostics
      </button>
      <button
        type="button"
        class="rounded-sm bg-zinc-700 px-2 py-1 text-xs hover:bg-zinc-600"
        onclick={() => void copyDiagnostics()}
        title="copy a metadata-only summary to the clipboard (no payload bytes)"
      >
        copy diagnostics JSON
      </button>
      {#if copyStatus === "ok"}
        <span class="self-center text-emerald-300">copied</span>
      {:else if copyStatus === "fallback"}
        <span class="self-center text-amber-300">clipboard unavailable — see event log</span>
      {/if}
    </div>
  </section>

  <div
    bind:this={mountTarget}
    class="mt-2 h-72 overflow-hidden rounded-sm border border-zinc-800 bg-black"
  ></div>

  <div
    class="mt-2 max-h-48 overflow-auto rounded-sm border border-zinc-800 bg-zinc-950 p-2 font-mono text-xs"
    data-testid="lab-event-log"
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
