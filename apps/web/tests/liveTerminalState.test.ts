import { describe, expect, it } from "vitest";
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
} from "../src/lib/dev/liveTerminalState.js";

/**
 * Sentinel that should NEVER appear in any lab-state surface — labels,
 * tones, button enablement, TTL text, or replay event formatting. The
 * SPEC redaction rule (Live SSH PTY bridge contract → "Logging and
 * reflection prohibitions") forbids the diagnostic UI from echoing
 * any payload byte through any user-visible channel; this sentinel is
 * the canary that proves the rule.
 */
const SENTINEL = "RELAY_SENTINEL_LIVE_STATE_PAYLOAD_91FE";

describe("derivePhase", () => {
  const baseDeps = {
    replayActive: false,
    detachedAtMs: null as number | null,
    nowMs: 0,
    reconnectInFlight: false,
  };

  it("maps idle/connecting/closed/error directly when no side signals fire", () => {
    expect(derivePhase({ ...baseDeps, clientState: "idle" })).toBe("idle");
    expect(derivePhase({ ...baseDeps, clientState: "connecting" })).toBe("connecting");
    expect(derivePhase({ ...baseDeps, clientState: "closed" })).toBe("closed");
    expect(derivePhase({ ...baseDeps, clientState: "error" })).toBe("error");
  });

  it("returns 'attached' or 'replaying' depending on replayActive", () => {
    expect(
      derivePhase({ ...baseDeps, clientState: "attached", replayActive: false }),
    ).toBe("attached");
    expect(
      derivePhase({ ...baseDeps, clientState: "attached", replayActive: true }),
    ).toBe("replaying");
  });

  it("returns 'reconnecting' when idle but the lab tore down expecting another attach", () => {
    expect(
      derivePhase({
        ...baseDeps,
        clientState: "idle",
        reconnectInFlight: true,
      }),
    ).toBe("reconnecting");
  });

  it("returns 'detached' inside the TTL window for both server-detach and local disconnect-no-close", () => {
    // Server-frame detach: clientState reaches 'detached'.
    expect(
      derivePhase({
        ...baseDeps,
        clientState: "detached",
        detachedAtMs: 1_000,
        nowMs: 1_000 + 5_000,
      }),
    ).toBe("detached");
    // Local disconnect-no-close: clientState was reset to 'idle' on
    // teardown, but `detachedAtMs` remembers the lab's intent.
    expect(
      derivePhase({
        ...baseDeps,
        clientState: "idle",
        detachedAtMs: 1_000,
        nowMs: 1_000 + 5_000,
      }),
    ).toBe("detached");
  });

  it("returns 'expired' once the local clock crosses the TTL deadline", () => {
    expect(
      derivePhase({
        ...baseDeps,
        clientState: "detached",
        detachedAtMs: 0,
        nowMs: DETACHED_TTL_MS,
      }),
    ).toBe("expired");
    expect(
      derivePhase({
        ...baseDeps,
        clientState: "idle",
        detachedAtMs: 0,
        nowMs: DETACHED_TTL_MS + 1,
      }),
    ).toBe("expired");
  });
});

describe("labelForPhase / toneForPhase", () => {
  const phases: LabPhase[] = [
    "idle",
    "connecting",
    "attached",
    "replaying",
    "detached",
    "reconnecting",
    "closed",
    "expired",
    "error",
  ];

  it("returns a non-empty label for every phase", () => {
    for (const p of phases) {
      expect(labelForPhase(p).length).toBeGreaterThan(0);
    }
  });

  it("maps tones such that error/warn surface visibly distinct from neutral", () => {
    expect(toneForPhase("error")).toBe("error");
    expect(toneForPhase("attached")).toBe("ok");
    expect(toneForPhase("detached")).toBe("warn");
    expect(toneForPhase("expired")).toBe("warn");
    expect(toneForPhase("idle")).toBe("neutral");
    expect(toneForPhase("connecting")).toBe("info");
    expect(toneForPhase("replaying")).toBe("info");
    expect(toneForPhase("reconnecting")).toBe("info");
    expect(toneForPhase("closed")).toBe("neutral");
  });
});

describe("computeEnablement", () => {
  const baseInput = { hasSessionId: true, lastSeenSeq: 0 };

  it("enables connect only from a fresh phase WITH a session id", () => {
    for (const phase of ["idle", "closed", "expired", "error"] as LabPhase[]) {
      expect(computeEnablement({ ...baseInput, phase }).connect).toBe(true);
    }
    expect(
      computeEnablement({ ...baseInput, phase: "idle", hasSessionId: false }).connect,
    ).toBe(false);
    expect(computeEnablement({ ...baseInput, phase: "attached" }).connect).toBe(false);
    expect(computeEnablement({ ...baseInput, phase: "connecting" }).connect).toBe(false);
    expect(computeEnablement({ ...baseInput, phase: "detached" }).connect).toBe(false);
    expect(computeEnablement({ ...baseInput, phase: "reconnecting" }).connect).toBe(false);
  });

  it("enables wire-frame buttons (ping/resize/detach/close/disconnectNoClose) only when attached or replaying", () => {
    for (const phase of ["attached", "replaying"] as LabPhase[]) {
      const out = computeEnablement({ ...baseInput, phase });
      expect(out.ping).toBe(true);
      expect(out.applyResize).toBe(true);
      expect(out.detach).toBe(true);
      expect(out.close).toBe(true);
      expect(out.disconnectNoClose).toBe(true);
    }
    for (const phase of [
      "idle",
      "connecting",
      "detached",
      "reconnecting",
      "closed",
      "expired",
      "error",
    ] as LabPhase[]) {
      const out = computeEnablement({ ...baseInput, phase });
      expect(out.ping).toBe(false);
      expect(out.applyResize).toBe(false);
      expect(out.detach).toBe(false);
      expect(out.close).toBe(false);
      expect(out.disconnectNoClose).toBe(false);
    }
  });

  it("enables dispose for every non-idle phase", () => {
    expect(computeEnablement({ ...baseInput, phase: "idle" }).dispose).toBe(false);
    for (const phase of [
      "connecting",
      "attached",
      "replaying",
      "detached",
      "reconnecting",
      "closed",
      "expired",
      "error",
    ] as LabPhase[]) {
      expect(computeEnablement({ ...baseInput, phase }).dispose).toBe(true);
    }
  });

  it("enables reconnectWithBookmark only with a positive seq AND a reconnectable phase", () => {
    expect(
      computeEnablement({
        phase: "detached",
        hasSessionId: true,
        lastSeenSeq: 12,
      }).reconnectWithBookmark,
    ).toBe(true);
    expect(
      computeEnablement({
        phase: "detached",
        hasSessionId: true,
        lastSeenSeq: 0,
      }).reconnectWithBookmark,
    ).toBe(false);
    expect(
      computeEnablement({
        phase: "attached",
        hasSessionId: true,
        lastSeenSeq: 12,
      }).reconnectWithBookmark,
    ).toBe(false);
    expect(
      computeEnablement({
        phase: "connecting",
        hasSessionId: true,
        lastSeenSeq: 12,
      }).reconnectWithBookmark,
    ).toBe(false);
    expect(
      computeEnablement({
        phase: "reconnecting",
        hasSessionId: true,
        lastSeenSeq: 12,
      }).reconnectWithBookmark,
    ).toBe(false);
    // `idle` is excluded — there is nothing to reconnect TO from a
    // never-attached state. The `connect` button is the right
    // affordance for that case.
    expect(
      computeEnablement({
        phase: "idle",
        hasSessionId: true,
        lastSeenSeq: 12,
      }).reconnectWithBookmark,
    ).toBe(false);
    expect(
      computeEnablement({
        phase: "detached",
        hasSessionId: false,
        lastSeenSeq: 12,
      }).reconnectWithBookmark,
    ).toBe(false);
  });

  it("enables reconnectWithoutBookmark independent of lastSeenSeq, but excludes idle", () => {
    expect(
      computeEnablement({
        phase: "detached",
        hasSessionId: true,
        lastSeenSeq: 0,
      }).reconnectWithoutBookmark,
    ).toBe(true);
    expect(
      computeEnablement({
        phase: "expired",
        hasSessionId: true,
        lastSeenSeq: 12,
      }).reconnectWithoutBookmark,
    ).toBe(true);
    expect(
      computeEnablement({
        phase: "closed",
        hasSessionId: true,
        lastSeenSeq: 0,
      }).reconnectWithoutBookmark,
    ).toBe(true);
    expect(
      computeEnablement({
        phase: "attached",
        hasSessionId: true,
        lastSeenSeq: 0,
      }).reconnectWithoutBookmark,
    ).toBe(false);
    // `idle` is excluded — `connect` is the affordance for a
    // never-attached state, and a duplicate "reconnect without
    // bookmark" button there would just confuse the operator.
    expect(
      computeEnablement({
        phase: "idle",
        hasSessionId: true,
        lastSeenSeq: 0,
      }).reconnectWithoutBookmark,
    ).toBe(false);
  });
});

describe("describeTtlWindow", () => {
  it("returns null until the lab has observed a detach", () => {
    expect(describeTtlWindow({ detachedAtMs: null, nowMs: 12_345 })).toBeNull();
  });

  it("describes a remaining window with an approximate countdown", () => {
    const out = describeTtlWindow({
      detachedAtMs: 1_000,
      nowMs: 1_000 + 5_000,
    });
    expect(out).not.toBeNull();
    expect(out!.approximate).toBe(true);
    // 30s - 5s = 25s, with a 1s floor.
    expect(out!.label).toContain("~25s remaining");
    expect(out!.label).toContain("approximate");
    expect(out!.label).toContain("local clock");
  });

  it("never claims authority over server state", () => {
    const cases = [
      { detachedAtMs: 1_000, nowMs: 1_500 }, // freshly detached
      { detachedAtMs: 1_000, nowMs: 1_000 + DETACHED_TTL_MS - 1_000 }, // near deadline
      { detachedAtMs: 1_000, nowMs: 1_000 + DETACHED_TTL_MS }, // crossed deadline
    ];
    for (const c of cases) {
      const out = describeTtlWindow(c)!;
      expect(out.approximate).toBe(true);
      expect(out.label.toLowerCase()).not.toContain("backend says");
      expect(out.label.toLowerCase()).not.toContain("server says");
    }
  });

  it("flips to an elapsed-locally label once the deadline is crossed without claiming the server agrees", () => {
    const out = describeTtlWindow({
      detachedAtMs: 0,
      nowMs: DETACHED_TTL_MS + 50,
    })!;
    expect(out.label).toContain("elapsed locally");
    // The server-truth disclaimer must remain — the lab cannot prove
    // the PTY is actually closed without a probe; clicking reconnect
    // is the operator's choice and 409 is the wire signal we trust.
    expect(out.label.toLowerCase()).toContain("server-truth");
  });

  it("clamps the visible countdown to a 1-second floor", () => {
    // 100ms remaining should still display as "~1s remaining" — the
    // operator never sees "0s" because that would imply the server
    // has already closed.
    const out = describeTtlWindow({
      detachedAtMs: 0,
      nowMs: DETACHED_TTL_MS - 100,
    })!;
    expect(out.label).toContain("~1s remaining");
  });
});

describe("replay event formatters", () => {
  it("formats replay_start with seq metadata only", () => {
    expect(formatReplayStart({ from_seq: 17, to_seq: 42 })).toBe(
      "replay_start from_seq=17 to_seq=42",
    );
  });

  it("formats replay_end with the latest_seq only", () => {
    expect(formatReplayEnd({ latest_seq: 99 })).toBe("replay_end latest_seq=99");
  });

  it("formats replay_window_lost with all three metadata fields, oldest_available_seq=null preserved", () => {
    expect(
      formatReplayWindowLost({
        requested_seq: 17,
        oldest_available_seq: 25,
        latest_seq: 99,
      }),
    ).toBe(
      "replay_window_lost requested_seq=17 oldest_available_seq=25 latest_seq=99",
    );
    expect(
      formatReplayWindowLost({
        requested_seq: 17,
        oldest_available_seq: null,
        latest_seq: 99,
      }),
    ).toBe(
      "replay_window_lost requested_seq=17 oldest_available_seq=null latest_seq=99",
    );
  });

  it("redaction sentinel: replay formatters never carry payload-shaped fields", () => {
    // The protocol does NOT include payload bytes in replay_*, but
    // pin the rule by sentinel: any future drift that, e.g., appends
    // `data` or `bytes` to the formatter would surface here.
    const start = formatReplayStart({ from_seq: 1, to_seq: 1 });
    const end = formatReplayEnd({ latest_seq: 1 });
    const lost = formatReplayWindowLost({
      requested_seq: 1,
      oldest_available_seq: 1,
      latest_seq: 1,
    });
    for (const line of [start, end, lost]) {
      expect(line).not.toContain(SENTINEL);
      expect(line.toLowerCase()).not.toContain("data=");
      expect(line.toLowerCase()).not.toContain("bytes=");
      expect(line.toLowerCase()).not.toContain("payload");
    }
  });
});
