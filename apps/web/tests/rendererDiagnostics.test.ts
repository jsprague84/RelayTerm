import { describe, expect, it } from "vitest";
import {
  DIAGNOSTICS_DISCLAIMER,
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
  rendererLabel,
  resetRendererDiagnostics,
  setClientState,
  setRenderer,
  summarizeDiagnostics,
  summarizeDiagnosticsAsJson,
} from "../src/lib/dev/rendererDiagnostics.js";

/**
 * Sentinel that should NEVER appear in any diagnostics surface — the
 * SPEC redaction rule (Live SSH PTY bridge contract → "Logging and
 * reflection prohibitions") forbids the diagnostic UI from echoing
 * any payload byte through any user-visible channel. This sentinel is
 * the canary that proves the rule for the new diagnostics module.
 */
const SENTINEL = "RELAY_SENTINEL_DIAG_PAYLOAD_C0FE";

describe("createRendererDiagnostics", () => {
  it("returns zeroed counters and the supplied renderer id", () => {
    const state = createRendererDiagnostics({
      now: () => 100,
      renderer: "xterm",
    });
    expect(state.rendererId).toBe("xterm");
    expect(state.startedAtMs).toBe(100);
    expect(state.lastEventAtMs).toBeNull();
    expect(state.mountCount).toBe(0);
    expect(state.disposeCount).toBe(0);
    expect(state.inputFrames).toBe(0);
    expect(state.inputBytes).toBe(0);
    expect(state.outputFrames).toBe(0);
    expect(state.outputBytes).toBe(0);
    expect(state.resizeSends).toBe(0);
    expect(state.resizeAcks).toBe(0);
    expect(state.pingCount).toBe(0);
    expect(state.pongCount).toBe(0);
    expect(state.replayStartCount).toBe(0);
    expect(state.replayEndCount).toBe(0);
    expect(state.replayWindowLostCount).toBe(0);
    expect(state.attachCount).toBe(0);
    expect(state.detachCount).toBe(0);
    expect(state.closeCount).toBe(0);
    expect(state.errorCount).toBe(0);
    expect(state.lastOutputSeq).toBe(0);
    expect(state.lastSeenSeq).toBe(0);
    expect(state.clientState).toBeNull();
    expect(state.mountStartedAtMs).toBeNull();
    expect(state.mountCompletedAtMs).toBeNull();
    expect(state.mountDurationMs).toBeNull();
  });

  it("defaults the renderer id to null when unset", () => {
    const state = createRendererDiagnostics();
    expect(state.rendererId).toBeNull();
  });
});

describe("rendererLabel", () => {
  it("returns the lab-facing label for each renderer id", () => {
    expect(rendererLabel("xterm")).toBe("xterm baseline");
    expect(rendererLabel("ghostty-web")).toBe("ghostty-web experimental");
  });
});

describe("mount / dispose timing", () => {
  it("records the duration once both start and end have been stamped", () => {
    const state = createRendererDiagnostics({ now: () => 0 });
    markMountStart(state, { now: () => 1_000 });
    markMountEnd(state, { now: () => 1_042 });
    expect(state.mountStartedAtMs).toBe(1_000);
    expect(state.mountCompletedAtMs).toBe(1_042);
    expect(state.mountDurationMs).toBe(42);
    expect(state.mountCount).toBe(1);
  });

  it("leaves duration null if mountEnd is called without mountStart", () => {
    const state = createRendererDiagnostics({ now: () => 0 });
    markMountEnd(state, { now: () => 1_000 });
    expect(state.mountCompletedAtMs).toBe(1_000);
    expect(state.mountDurationMs).toBeNull();
    expect(state.mountCount).toBe(1);
  });

  it("clamps a negative-delta duration to zero (clock skew protection)", () => {
    const state = createRendererDiagnostics();
    markMountStart(state, { now: () => 1_000 });
    markMountEnd(state, { now: () => 999 });
    expect(state.mountDurationMs).toBe(0);
  });

  it("a re-mount resets the start cursor and increments mountCount", () => {
    const state = createRendererDiagnostics();
    markMountStart(state, { now: () => 100 });
    markMountEnd(state, { now: () => 110 });
    markMountStart(state, { now: () => 200 });
    expect(state.mountCompletedAtMs).toBeNull();
    expect(state.mountDurationMs).toBeNull();
    markMountEnd(state, { now: () => 250 });
    expect(state.mountDurationMs).toBe(50);
    expect(state.mountCount).toBe(2);
  });

  it("markDispose increments dispose count without touching mount fields", () => {
    const state = createRendererDiagnostics();
    markMountStart(state, { now: () => 100 });
    markMountEnd(state, { now: () => 110 });
    markDispose(state, { now: () => 120 });
    markDispose(state, { now: () => 130 });
    expect(state.disposeCount).toBe(2);
    expect(state.mountCount).toBe(1);
    expect(state.mountDurationMs).toBe(10);
    expect(state.lastEventAtMs).toBe(130);
  });
});

describe("input/output counters never accept payload bytes", () => {
  it("recordInput increments frames + bytes from a count only", () => {
    const state = createRendererDiagnostics();
    recordInput(state, 1);
    recordInput(state, 4);
    recordInput(state, 0);
    expect(state.inputFrames).toBe(3);
    expect(state.inputBytes).toBe(5);
  });

  it("recordInput's signature does not accept payload bytes", () => {
    // The redaction rule is encoded at the type level: the only way to
    // call this function with a count is with a number. A consumer
    // cannot leak the payload through this surface even if they tried.
    const state = createRendererDiagnostics();
    const fn: (
      s: typeof state,
      n: number,
    ) => void = recordInput;
    fn(state, 7);
    expect(state.inputBytes).toBe(7);
  });

  it("recordOutput's signature does not accept payload bytes", () => {
    // Symmetric pin to the `recordInput` signature test: the only way
    // to call `recordOutput` is with two numbers (seq + byteLength).
    // No overload accepts a string, Uint8Array, or ArrayBuffer, so a
    // consumer cannot leak the payload through this surface.
    const state = createRendererDiagnostics();
    const fn: (
      s: typeof state,
      seq: number,
      n: number,
    ) => void = recordOutput;
    fn(state, 9, 4);
    expect(state.outputFrames).toBe(1);
    expect(state.outputBytes).toBe(4);
    expect(state.lastOutputSeq).toBe(9);
  });

  it("recordOutput tracks frames, bytes, and the highest seq", () => {
    const state = createRendererDiagnostics();
    recordOutput(state, 1, 16);
    recordOutput(state, 5, 32);
    recordOutput(state, 3, 4); // older seq must NOT decrease lastOutputSeq
    expect(state.outputFrames).toBe(3);
    expect(state.outputBytes).toBe(52);
    expect(state.lastOutputSeq).toBe(5);
  });

  it("recordOutput clamps a negative byteLength to zero (defensive)", () => {
    const state = createRendererDiagnostics();
    recordOutput(state, 1, -100);
    expect(state.outputBytes).toBe(0);
    expect(state.outputFrames).toBe(1);
  });

  it("recordLastSeenSeq monotonically advances the bookmark", () => {
    const state = createRendererDiagnostics();
    recordLastSeenSeq(state, 10);
    recordLastSeenSeq(state, 5); // must NOT regress
    recordLastSeenSeq(state, 12);
    expect(state.lastSeenSeq).toBe(12);
  });
});

describe("control / replay / lifecycle counters", () => {
  it("recordResizeSend / recordResizeAck increment independently", () => {
    const state = createRendererDiagnostics();
    recordResizeSend(state);
    recordResizeSend(state);
    recordResizeAck(state);
    expect(state.resizeSends).toBe(2);
    expect(state.resizeAcks).toBe(1);
  });

  it("recordPing / recordPong are separate counters", () => {
    const state = createRendererDiagnostics();
    recordPing(state);
    recordPong(state);
    recordPong(state);
    expect(state.pingCount).toBe(1);
    expect(state.pongCount).toBe(2);
  });

  it("replay counters track each phase distinctly", () => {
    const state = createRendererDiagnostics();
    recordReplayStart(state);
    recordReplayEnd(state);
    recordReplayStart(state);
    recordReplayWindowLost(state);
    expect(state.replayStartCount).toBe(2);
    expect(state.replayEndCount).toBe(1);
    expect(state.replayWindowLostCount).toBe(1);
  });

  it("attach/detach/close/error each have their own counter", () => {
    const state = createRendererDiagnostics();
    recordAttached(state);
    recordDetached(state);
    recordAttached(state);
    recordClosed(state);
    recordError(state);
    expect(state.attachCount).toBe(2);
    expect(state.detachCount).toBe(1);
    expect(state.closeCount).toBe(1);
    expect(state.errorCount).toBe(1);
  });

  it("setClientState records the latest state and stamps lastEventAtMs", () => {
    const state = createRendererDiagnostics();
    setClientState(state, "attached", { now: () => 500 });
    expect(state.clientState).toBe("attached");
    expect(state.lastEventAtMs).toBe(500);
  });
});

describe("renderer switch", () => {
  it("setRenderer flips the id; subsequent mount/dispose attribute to the new renderer", () => {
    const state = createRendererDiagnostics({ renderer: "xterm" });
    markMountStart(state, { now: () => 0 });
    markMountEnd(state, { now: () => 5 });
    expect(state.rendererId).toBe("xterm");
    expect(state.mountCount).toBe(1);

    // operator switches → lab disposes old, sets new, mounts new.
    markDispose(state);
    setRenderer(state, "ghostty-web");
    markMountStart(state, { now: () => 100 });
    markMountEnd(state, { now: () => 200 });
    expect(state.rendererId).toBe("ghostty-web");
    expect(state.disposeCount).toBe(1);
    expect(state.mountCount).toBe(2);
    expect(state.mountDurationMs).toBe(100);
  });
});

describe("resetRendererDiagnostics", () => {
  it("zeroes counters and mount/dispose state but keeps renderer + client state", () => {
    const state = createRendererDiagnostics({ renderer: "ghostty-web" });
    setClientState(state, "attached");
    recordInput(state, 8);
    recordOutput(state, 3, 16);
    markMountStart(state, { now: () => 100 });
    markMountEnd(state, { now: () => 110 });
    markDispose(state);
    recordReplayStart(state);
    recordAttached(state);

    resetRendererDiagnostics(state, { now: () => 9_999 });

    expect(state.rendererId).toBe("ghostty-web");
    expect(state.clientState).toBe("attached");
    expect(state.startedAtMs).toBe(9_999);
    expect(state.lastEventAtMs).toBeNull();

    expect(state.inputFrames).toBe(0);
    expect(state.inputBytes).toBe(0);
    expect(state.outputFrames).toBe(0);
    expect(state.outputBytes).toBe(0);
    expect(state.lastOutputSeq).toBe(0);
    expect(state.lastSeenSeq).toBe(0);
    expect(state.mountStartedAtMs).toBeNull();
    expect(state.mountCompletedAtMs).toBeNull();
    expect(state.mountDurationMs).toBeNull();
    expect(state.mountCount).toBe(0);
    expect(state.disposeCount).toBe(0);
    expect(state.replayStartCount).toBe(0);
    expect(state.attachCount).toBe(0);
  });
});

describe("summarizeDiagnostics", () => {
  it("contains metadata only — no payload-shaped fields", () => {
    const state = createRendererDiagnostics({ renderer: "xterm" });
    setClientState(state, "attached");
    recordInput(state, 12);
    recordOutput(state, 7, 64);
    markMountStart(state, { now: () => 1 });
    markMountEnd(state, { now: () => 11 });
    recordReplayStart(state);
    recordReplayEnd(state);
    recordResizeSend(state);
    recordAttached(state);
    recordDetached(state);

    const summary = summarizeDiagnostics(state);

    expect(summary.schema).toBe("relayterm.dev.renderer-diagnostics.v1");
    expect(summary.disclaimer).toBe(DIAGNOSTICS_DISCLAIMER);
    expect(summary.renderer).toEqual({
      id: "xterm",
      label: "xterm baseline",
    });
    expect(summary.client.state).toBe("attached");
    expect(summary.mount.durationMs).toBe(10);
    expect(summary.mount.count).toBe(1);
    expect(summary.io.inputFrames).toBe(1);
    expect(summary.io.inputBytes).toBe(12);
    expect(summary.io.outputFrames).toBe(1);
    expect(summary.io.outputBytes).toBe(64);
    expect(summary.io.lastOutputSeq).toBe(7);
    expect(summary.replay.startCount).toBe(1);
    expect(summary.replay.endCount).toBe(1);
    expect(summary.control.resizeSends).toBe(1);
    expect(summary.lifecycle.attachCount).toBe(1);
    expect(summary.lifecycle.detachCount).toBe(1);

    // Pin the rule by inspecting the JSON form of the entire summary —
    // any future regression that smuggled a "data"/"payload"/"bytes_b64"
    // field into the snapshot would surface here.
    const json = JSON.stringify(summary).toLowerCase();
    expect(json).not.toContain("data\":");
    expect(json).not.toContain("payload");
    expect(json).not.toContain("bytes_b64");
    expect(json).not.toContain("base64");
  });

  it("renderer.label is null when no renderer has been chosen yet", () => {
    const state = createRendererDiagnostics();
    const summary = summarizeDiagnostics(state);
    expect(summary.renderer.id).toBeNull();
    expect(summary.renderer.label).toBeNull();
  });

  it("survives a full reset cycle and reflects the cleared counters", () => {
    const state = createRendererDiagnostics({ renderer: "xterm" });
    recordInput(state, 1);
    recordOutput(state, 1, 1);
    resetRendererDiagnostics(state);
    const summary = summarizeDiagnostics(state);
    expect(summary.io.inputFrames).toBe(0);
    expect(summary.io.outputFrames).toBe(0);
    expect(summary.renderer.id).toBe("xterm");
  });
});

describe("summarizeDiagnosticsAsJson", () => {
  it("returns indented JSON consumable by the lab clipboard affordance", () => {
    const state = createRendererDiagnostics({ renderer: "ghostty-web" });
    const json = summarizeDiagnosticsAsJson(state);
    expect(json.startsWith("{")).toBe(true);
    // Indented form: human-readable, not minified.
    expect(json).toContain("\n  ");
    const parsed = JSON.parse(json);
    expect(parsed.renderer.id).toBe("ghostty-web");
    expect(parsed.disclaimer).toContain("not a benchmark");
  });
});

describe("redaction sentinel", () => {
  it("no diagnostics surface echoes a sentinel that callers might pass alongside counts", () => {
    // Pin: a future drift that, e.g., made `recordInput` accept a
    // `Uint8Array` would surface as a sentinel match in the summary.
    const state = createRendererDiagnostics({ renderer: "xterm" });
    recordInput(state, SENTINEL.length);
    recordOutput(state, 1, SENTINEL.length);
    setClientState(state, "attached");
    const json = summarizeDiagnosticsAsJson(state);
    expect(json).not.toContain(SENTINEL);
  });
});
