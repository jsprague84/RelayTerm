/**
 * Pure diagnostics state model for the dev-only renderer comparison lab.
 *
 * Why this exists: comparing xterm vs ghostty-web on the same RelayTerm
 * session path is only useful if the lab can show counters that are
 * computed identically across renderer switches and redact identically
 * across both adapters. A pure state record + pure update functions
 * keeps the rules unit-testable without a Svelte runtime; the lab wraps
 * the state in `$state(...)` so mutations propagate to the panel.
 *
 * Critical contracts (mirrored from `labLog.ts` and pinned by tests):
 *  - {@link recordInput} / {@link recordOutput} NEVER take the payload
 *    itself. The function signature is the redaction rule: callers
 *    cannot leak bytes through this surface because no function in this
 *    module accepts payload bytes as an argument.
 *  - {@link summarizeDiagnostics} returns ONLY metadata (counts, seqs,
 *    durations, renderer name, current state). No payload-shaped fields.
 *    The "copy as JSON" affordance in the lab UI runs through this
 *    helper so an operator-pasted summary never surfaces wire bytes.
 *  - Counters are diagnostic and approximate. They are not benchmarks.
 *    Browser, machine, renderer, font, and workload all affect timings.
 *    The wording in the lab UI is honest about that — see
 *    `XtermLiveTerminalLab.svelte`.
 *
 * Why pure functions instead of a class: tests can build a state and
 * call functions directly without instantiation glue, AND the lab can
 * wrap the state in Svelte 5's `$state(...)` proxy so reactive panels
 * pick up mutations without us having to plumb a `snapshot()` getter.
 */

import type { TerminalSessionState } from "@relayterm/terminal-core";

/**
 * Stable identifiers for the swappable renderer adapters. Mirrors the
 * `RendererChoice` type used by the lab — duplicated here so this
 * module has no dependency on the Svelte component that consumes it.
 * If a future renderer is added (wterm / native), extend both lists.
 */
export type RendererId = "xterm" | "ghostty-web" | "restty";

/**
 * Operator-facing label for a renderer id. Lives next to the id so the
 * lab does not have to maintain its own switch — and tests pin the
 * exact wording so a future "experimental" → "stable" promotion is a
 * deliberate change.
 */
export function rendererLabel(id: RendererId): string {
  switch (id) {
    case "xterm":
      return "xterm baseline";
    case "ghostty-web":
      return "ghostty-web experimental";
    case "restty":
      return "restty experimental";
  }
}

export interface RendererDiagnosticsState {
  /**
   * Currently selected renderer. `null` until the first
   * {@link setRenderer} call — the lab seeds it on construction so
   * `null` should not appear in normal flow, but tests can exercise
   * the unset case.
   */
  rendererId: RendererId | null;

  /** Wall-clock instant the most recent `mount()` call started. */
  mountStartedAtMs: number | null;
  /** Wall-clock instant the most recent `mount()` call resolved. */
  mountCompletedAtMs: number | null;
  /**
   * `mountCompletedAtMs - mountStartedAtMs` for the most recent mount.
   * The pair fields above are kept so the lab can show "still mounting"
   * if a mount is in flight without a completion.
   */
  mountDurationMs: number | null;
  /** Number of completed mounts on this state object. */
  mountCount: number;
  /** Number of `dispose()` calls observed. */
  disposeCount: number;

  // --- Hot data path counters ---------------------------------------------

  /** Renderer → client input frames sent. */
  inputFrames: number;
  /** Sum of `byteLength` for input frames. UTF-8 bytes for strings. */
  inputBytes: number;
  /** Server → client output frames received and decoded successfully. */
  outputFrames: number;
  /** Sum of `byteLength` for decoded output frames. */
  outputBytes: number;

  // --- Control-plane counters ---------------------------------------------

  /** Renderer-driven `resize` frames the lab forwarded. */
  resizeSends: number;
  /** Server `ack { kind: "resize" }` frames observed. */
  resizeAcks: number;

  pingCount: number;
  pongCount: number;

  replayStartCount: number;
  replayEndCount: number;
  replayWindowLostCount: number;

  attachCount: number;
  detachCount: number;
  closeCount: number;
  errorCount: number;

  /** Highest observed `seq` on `output`. `0` until the first frame. */
  lastOutputSeq: number;
  /**
   * Highest seq mirrored to the lab's reconnect bookmark. Tracked
   * here so the diagnostics summary is self-contained — the operator
   * can copy it without pulling state from the Svelte component.
   */
  lastSeenSeq: number;

  /**
   * Most recent `TerminalSessionState` observed on the client. `null`
   * before any state_change fires.
   */
  clientState: TerminalSessionState | null;

  /** When this state object was created (or last reset). */
  startedAtMs: number;
  /** When the most recent counter mutation occurred. */
  lastEventAtMs: number | null;
}

/**
 * Construct a fresh diagnostics state. `now` is injectable so tests can
 * pin a deterministic clock; production code passes the default.
 */
export function createRendererDiagnostics(
  init: { now?: () => number; renderer?: RendererId } = {},
): RendererDiagnosticsState {
  const now = init.now ?? Date.now;
  return {
    rendererId: init.renderer ?? null,
    mountStartedAtMs: null,
    mountCompletedAtMs: null,
    mountDurationMs: null,
    mountCount: 0,
    disposeCount: 0,
    inputFrames: 0,
    inputBytes: 0,
    outputFrames: 0,
    outputBytes: 0,
    resizeSends: 0,
    resizeAcks: 0,
    pingCount: 0,
    pongCount: 0,
    replayStartCount: 0,
    replayEndCount: 0,
    replayWindowLostCount: 0,
    attachCount: 0,
    detachCount: 0,
    closeCount: 0,
    errorCount: 0,
    lastOutputSeq: 0,
    lastSeenSeq: 0,
    clientState: null,
    startedAtMs: now(),
    lastEventAtMs: null,
  };
}

/**
 * Reset all counters and mount/dispose telemetry while preserving the
 * currently selected renderer and the most recent client state. Reset
 * is for "I want to start a new measurement window without re-creating
 * the panel" — the operator's renderer choice and live attach status
 * should not surprise-flip.
 */
export function resetRendererDiagnostics(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  const now = opts.now ?? Date.now;
  state.mountStartedAtMs = null;
  state.mountCompletedAtMs = null;
  state.mountDurationMs = null;
  state.mountCount = 0;
  state.disposeCount = 0;
  state.inputFrames = 0;
  state.inputBytes = 0;
  state.outputFrames = 0;
  state.outputBytes = 0;
  state.resizeSends = 0;
  state.resizeAcks = 0;
  state.pingCount = 0;
  state.pongCount = 0;
  state.replayStartCount = 0;
  state.replayEndCount = 0;
  state.replayWindowLostCount = 0;
  state.attachCount = 0;
  state.detachCount = 0;
  state.closeCount = 0;
  state.errorCount = 0;
  state.lastOutputSeq = 0;
  state.lastSeenSeq = 0;
  state.startedAtMs = now();
  state.lastEventAtMs = null;
}

export function setRenderer(
  state: RendererDiagnosticsState,
  id: RendererId,
): void {
  state.rendererId = id;
}

export function setClientState(
  state: RendererDiagnosticsState,
  cs: TerminalSessionState,
  opts: { now?: () => number } = {},
): void {
  state.clientState = cs;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

/**
 * Stamp the start of a renderer `mount()` call. Pair with
 * {@link markMountEnd}. Calling this twice without an end resets the
 * start cursor — the lab does not pretend two overlapping mounts can
 * happen, and a stuck "in flight" mount would be misleading on the
 * panel.
 */
export function markMountStart(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  const t = (opts.now ?? Date.now)();
  state.mountStartedAtMs = t;
  state.mountCompletedAtMs = null;
  state.mountDurationMs = null;
  state.lastEventAtMs = t;
}

/**
 * Stamp completion of a renderer `mount()` call. If no start was
 * recorded the function records the completion timestamp and leaves
 * `mountDurationMs` as `null` — a duration claim without a start would
 * be a lie.
 */
export function markMountEnd(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  const t = (opts.now ?? Date.now)();
  state.mountCompletedAtMs = t;
  if (state.mountStartedAtMs !== null) {
    state.mountDurationMs = Math.max(0, t - state.mountStartedAtMs);
  }
  state.mountCount += 1;
  state.lastEventAtMs = t;
}

export function markDispose(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  const t = (opts.now ?? Date.now)();
  state.disposeCount += 1;
  state.lastEventAtMs = t;
}

/**
 * Record an outbound input frame. Takes ONLY the byte count — never
 * the payload. This is the same redaction rule {@link redactInputLogText}
 * encodes, transposed into a counter.
 */
export function recordInput(
  state: RendererDiagnosticsState,
  byteLength: number,
  opts: { now?: () => number } = {},
): void {
  state.inputFrames += 1;
  state.inputBytes += Math.max(0, byteLength | 0);
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

/**
 * Record an inbound output frame. Takes the wire `seq` and the decoded
 * `byteLength` — NEVER the payload. The seq update mirrors the rule the
 * lab applies on `output`: `lastOutputSeq` only advances. Operators
 * also bump the bookmark via {@link recordLastSeenSeq}; the two are
 * separate so a future "diff replay vs live" affordance can split them.
 */
export function recordOutput(
  state: RendererDiagnosticsState,
  seq: number,
  byteLength: number,
  opts: { now?: () => number } = {},
): void {
  state.outputFrames += 1;
  state.outputBytes += Math.max(0, byteLength | 0);
  if (seq > state.lastOutputSeq) {
    state.lastOutputSeq = seq;
  }
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

/** Mirror the lab's reconnect bookmark into diagnostics state. */
export function recordLastSeenSeq(
  state: RendererDiagnosticsState,
  seq: number,
): void {
  if (seq > state.lastSeenSeq) {
    state.lastSeenSeq = seq;
  }
}

export function recordResizeSend(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.resizeSends += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordResizeAck(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.resizeAcks += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordPing(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.pingCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordPong(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.pongCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordReplayStart(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.replayStartCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordReplayEnd(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.replayEndCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordReplayWindowLost(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.replayWindowLostCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordAttached(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.attachCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordDetached(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.detachCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordClosed(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.closeCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

export function recordError(
  state: RendererDiagnosticsState,
  opts: { now?: () => number } = {},
): void {
  state.errorCount += 1;
  state.lastEventAtMs = (opts.now ?? Date.now)();
}

/**
 * JSON-safe summary shape. The keys here are the contract for the
 * "copy diagnostics summary as JSON" affordance — adding a key that
 * could carry payload bytes (e.g. "lastOutputBytes") is a SPEC change,
 * not a bag-of-fields tweak.
 */
export interface RendererDiagnosticsSummary {
  schema: "relayterm.dev.renderer-diagnostics.v1";
  /**
   * Honesty marker: this surface exists to compare BEHAVIOR while
   * exercising the same RelayTerm protocol path, NOT to publish a
   * benchmark number. The lab UI repeats the disclaimer next to any
   * timing.
   */
  disclaimer: string;
  renderer: {
    id: RendererId | null;
    label: string | null;
  };
  client: {
    state: TerminalSessionState | null;
  };
  mount: {
    startedAtMs: number | null;
    completedAtMs: number | null;
    durationMs: number | null;
    count: number;
    disposeCount: number;
  };
  io: {
    inputFrames: number;
    inputBytes: number;
    outputFrames: number;
    outputBytes: number;
    lastOutputSeq: number;
    lastSeenSeq: number;
  };
  control: {
    resizeSends: number;
    resizeAcks: number;
    pingCount: number;
    pongCount: number;
  };
  replay: {
    startCount: number;
    endCount: number;
    windowLostCount: number;
  };
  lifecycle: {
    attachCount: number;
    detachCount: number;
    closeCount: number;
    errorCount: number;
  };
  window: {
    startedAtMs: number;
    lastEventAtMs: number | null;
  };
}

export const DIAGNOSTICS_DISCLAIMER =
  "dev diagnostics, not a benchmark — browser/machine/renderer/font/workload all affect numbers";

/**
 * Produce a JSON-safe snapshot of the diagnostics state. The returned
 * object contains ONLY metadata: counts, seqs, durations, renderer id,
 * current state. No raw input or output bytes appear anywhere — by
 * construction, because no function in this module ever accepted them.
 */
export function summarizeDiagnostics(
  state: RendererDiagnosticsState,
): RendererDiagnosticsSummary {
  return {
    schema: "relayterm.dev.renderer-diagnostics.v1",
    disclaimer: DIAGNOSTICS_DISCLAIMER,
    renderer: {
      id: state.rendererId,
      label: state.rendererId === null ? null : rendererLabel(state.rendererId),
    },
    client: {
      state: state.clientState,
    },
    mount: {
      startedAtMs: state.mountStartedAtMs,
      completedAtMs: state.mountCompletedAtMs,
      durationMs: state.mountDurationMs,
      count: state.mountCount,
      disposeCount: state.disposeCount,
    },
    io: {
      inputFrames: state.inputFrames,
      inputBytes: state.inputBytes,
      outputFrames: state.outputFrames,
      outputBytes: state.outputBytes,
      lastOutputSeq: state.lastOutputSeq,
      lastSeenSeq: state.lastSeenSeq,
    },
    control: {
      resizeSends: state.resizeSends,
      resizeAcks: state.resizeAcks,
      pingCount: state.pingCount,
      pongCount: state.pongCount,
    },
    replay: {
      startCount: state.replayStartCount,
      endCount: state.replayEndCount,
      windowLostCount: state.replayWindowLostCount,
    },
    lifecycle: {
      attachCount: state.attachCount,
      detachCount: state.detachCount,
      closeCount: state.closeCount,
      errorCount: state.errorCount,
    },
    window: {
      startedAtMs: state.startedAtMs,
      lastEventAtMs: state.lastEventAtMs,
    },
  };
}

/**
 * Stringify the summary for clipboard use. Indented for readability —
 * the operator is the only consumer.
 */
export function summarizeDiagnosticsAsJson(
  state: RendererDiagnosticsState,
): string {
  return JSON.stringify(summarizeDiagnostics(state), null, 2);
}
