import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  LAUNCH_TIMING_EVENT_LABELS,
  LAUNCH_TIMING_EVENT_NAMES,
  LaunchTimingRecorder,
  formatRelativeMs,
  type LaunchTimingErrorKind,
  type LaunchTimingEventName,
  type LaunchTimingSnapshot,
} from "../src/lib/app/terminal/terminalLaunchTiming.js";

/**
 * Sentinel canary that should NEVER appear in any timing surface. The
 * recorder's contract is "names and relative-ms only — no payload
 * bytes, no server messages, no URLs, no headers"; if a future
 * regression starts smuggling a free-form `message` through `markError`
 * or `snapshot()`, this sentinel pinned in the redaction test fires.
 */
const SENTINEL = "RELAY_SENTINEL_TIMING_PAYLOAD_7A11";

/**
 * Deterministic monotonic clock helper. Returns a `now()` closure that
 * advances by the supplied step each call AND an `advance(ms)` setter
 * for explicit jumps.
 */
function fakeClock(start = 1_000): {
  now: () => number;
  advance: (ms: number) => void;
  current: () => number;
} {
  let t = start;
  return {
    now: () => t,
    advance: (ms: number) => {
      t += ms;
    },
    current: () => t,
  };
}

describe("LAUNCH_TIMING_EVENT_NAMES", () => {
  it("includes every documented lifecycle event", () => {
    expect(LAUNCH_TIMING_EVENT_NAMES).toEqual([
      "launch_started",
      "create_session_post_started",
      "create_session_post_resolved",
      "ws_connect_started",
      "ws_open",
      "first_server_message",
      "first_output",
      "attached",
      "detach_requested",
      "close_requested",
      "ws_close",
      "error",
    ]);
  });

  it("has a label for every event name (closed vocabulary)", () => {
    for (const name of LAUNCH_TIMING_EVENT_NAMES) {
      expect(LAUNCH_TIMING_EVENT_LABELS[name]).toMatch(/.+/);
    }
  });
});

describe("LaunchTimingRecorder", () => {
  it("anchors launch_started at 0 ms on construction", () => {
    const clock = fakeClock(500);
    const r = new LaunchTimingRecorder({ now: clock.now });
    const snap = r.snapshot();
    expect(snap.events).toHaveLength(1);
    expect(snap.events[0]).toEqual({
      name: "launch_started",
      relativeMs: 0,
    });
    expect(snap.createPostOutcome).toBeNull();
    expect(snap.errorKind).toBeNull();
  });

  it("records relative monotonic times in mark order", () => {
    const clock = fakeClock(100);
    const r = new LaunchTimingRecorder({ now: clock.now });
    clock.advance(25);
    r.mark("create_session_post_started");
    clock.advance(75);
    r.mark("create_session_post_resolved");
    clock.advance(10);
    r.mark("ws_connect_started");
    clock.advance(40);
    r.mark("ws_open");
    const snap = r.snapshot();
    expect(snap.events.map((e) => [e.name, e.relativeMs])).toEqual([
      ["launch_started", 0],
      ["create_session_post_started", 25],
      ["create_session_post_resolved", 100],
      ["ws_connect_started", 110],
      ["ws_open", 150],
    ]);
  });

  it("treats every event as one-shot — duplicates keep the first observation", () => {
    const clock = fakeClock(0);
    const r = new LaunchTimingRecorder({ now: clock.now });
    clock.advance(10);
    expect(r.mark("ws_open")).toBe(10);
    clock.advance(50);
    expect(r.mark("ws_open")).toBeNull();
    const snap = r.snapshot();
    const opens = snap.events.filter((e) => e.name === "ws_open");
    expect(opens).toHaveLength(1);
    expect(opens[0].relativeMs).toBe(10);
  });

  it("refuses to re-anchor launch_started", () => {
    const clock = fakeClock(0);
    const r = new LaunchTimingRecorder({ now: clock.now });
    clock.advance(99);
    expect(r.mark("launch_started")).toBeNull();
    const snap = r.snapshot();
    const anchors = snap.events.filter((e) => e.name === "launch_started");
    expect(anchors).toHaveLength(1);
    expect(anchors[0].relativeMs).toBe(0);
  });

  it("clamps a negative delta to 0 (defensive against misbehaving now())", () => {
    let calls = 0;
    const r = new LaunchTimingRecorder({
      now: () => (calls++ === 0 ? 1_000 : 500),
    });
    expect(r.mark("ws_open")).toBe(0);
  });

  it("records the create-POST outcome and keeps the first one", () => {
    const clock = fakeClock(0);
    const r = new LaunchTimingRecorder({ now: clock.now });
    clock.advance(10);
    r.markCreateSessionPostResolved("ok");
    clock.advance(20);
    // Second call with a different outcome MUST NOT clobber the first.
    r.markCreateSessionPostResolved("error");
    const snap = r.snapshot();
    expect(snap.createPostOutcome).toBe("ok");
  });

  it("records an error kind from the closed vocabulary and keeps the first one", () => {
    const clock = fakeClock(0);
    const r = new LaunchTimingRecorder({ now: clock.now });
    clock.advance(5);
    r.markError("transport");
    clock.advance(5);
    r.markError("decode");
    const snap = r.snapshot();
    expect(snap.errorKind).toBe("transport");
    const errors = snap.events.filter((e) => e.name === "error");
    expect(errors).toHaveLength(1);
    expect(errors[0].relativeMs).toBe(5);
  });

  it("subscribes deliver snapshots on mark / markError / markCreateSessionPostResolved", () => {
    const clock = fakeClock(0);
    const r = new LaunchTimingRecorder({ now: clock.now });
    const seen: LaunchTimingSnapshot[] = [];
    const unsub = r.subscribe((snap) => {
      seen.push(snap);
    });
    clock.advance(1);
    r.mark("ws_connect_started");
    clock.advance(1);
    r.markCreateSessionPostResolved("ok");
    clock.advance(1);
    r.markError("server_error");
    unsub();
    clock.advance(1);
    r.mark("ws_close");
    // Three before unsubscribe; the post-unsub mark does not deliver.
    expect(seen.length).toBe(3);
    expect(seen[0].events.at(-1)?.name).toBe("ws_connect_started");
    expect(seen[1].createPostOutcome).toBe("ok");
    expect(seen[2].errorKind).toBe("server_error");
  });

  it("swallows listener exceptions instead of breaking other listeners", () => {
    const r = new LaunchTimingRecorder({ now: () => 0 });
    const calls: number[] = [];
    r.subscribe(() => {
      throw new Error("listener panic");
    });
    r.subscribe(() => {
      calls.push(1);
    });
    expect(() => r.mark("ws_open")).not.toThrow();
    expect(calls).toEqual([1]);
  });

  it("returns the relative-ms of a previously marked event", () => {
    const clock = fakeClock(0);
    const r = new LaunchTimingRecorder({ now: clock.now });
    clock.advance(42);
    r.mark("ws_open");
    expect(r.relativeMsFor("ws_open")).toBe(42);
    expect(r.relativeMsFor("ws_close")).toBeNull();
  });
});

describe("snapshot redaction (payload-free contract)", () => {
  it("never returns the underlying error message in any snapshot field", () => {
    const r = new LaunchTimingRecorder({ now: () => 0 });
    r.markError("server_error");
    const snap = r.snapshot();
    const serialized = JSON.stringify(snap);
    expect(serialized).not.toContain(SENTINEL);
    // The recorder API does not accept a free-form message at all, so
    // a sentinel can only appear if a future regression broadens the
    // surface. Defence in depth.
    expect(serialized).not.toMatch(/message/i);
    expect(serialized).not.toMatch(/http/i);
  });

  it("snapshot keys are exactly events / createPostOutcome / errorKind", () => {
    const r = new LaunchTimingRecorder({ now: () => 0 });
    r.mark("ws_open");
    r.markCreateSessionPostResolved("ok");
    r.markError("transport");
    const snap = r.snapshot();
    expect(Object.keys(snap).sort()).toEqual([
      "createPostOutcome",
      "errorKind",
      "events",
    ]);
    for (const event of snap.events) {
      expect(Object.keys(event).sort()).toEqual(["name", "relativeMs"]);
      expect(typeof event.relativeMs).toBe("number");
      expect(LAUNCH_TIMING_EVENT_NAMES).toContain(event.name);
    }
  });

  it("error kind union is exactly the documented closed vocabulary", () => {
    const allowed: LaunchTimingErrorKind[] = [
      "create_session_post",
      "transport",
      "decode",
      "unexpected_first_frame",
      "send_before_attached",
      "send_after_terminal",
      "server_error",
      "unknown",
    ];
    for (const kind of allowed) {
      const r = new LaunchTimingRecorder({ now: () => 0 });
      r.markError(kind);
      expect(r.snapshot().errorKind).toBe(kind);
    }
  });
});

describe("no persistent storage writes", () => {
  let setItemSpy: ReturnType<typeof vi.spyOn> | null = null;

  beforeEach(() => {
    // jsdom's localStorage AND sessionStorage share `Storage.prototype`,
    // so ONE spy on the prototype's `setItem` covers both backends. The
    // earlier shape (two separate `vi.spyOn` calls) had the second one
    // wrapping the first spy rather than the original method, which
    // confused `mockRestore()` semantics. One spy is sufficient and
    // unambiguous.
    if (typeof Storage !== "undefined") {
      setItemSpy = vi.spyOn(Storage.prototype, "setItem");
    }
  });

  afterEach(() => {
    setItemSpy?.mockRestore();
    setItemSpy = null;
  });

  it("does not call setItem on localStorage or sessionStorage from any recorder path", () => {
    const r = new LaunchTimingRecorder({ now: () => 0 });
    r.mark("create_session_post_started");
    r.markCreateSessionPostResolved("ok");
    r.mark("ws_connect_started");
    r.mark("ws_open");
    r.mark("first_server_message");
    r.mark("first_output");
    r.mark("attached");
    r.mark("detach_requested");
    r.mark("close_requested");
    r.mark("ws_close");
    r.markError("transport");
    expect(setItemSpy?.mock.calls ?? []).toEqual([]);
  });
});

describe("missing events render as pending in DOM-style consumers", () => {
  /**
   * A consumer that mirrors the production diagnostic strip's loop. The
   * test pins the contract: every name in LAUNCH_TIMING_EVENT_NAMES
   * shows up with either an observed ms or a "pending" placeholder.
   */
  function renderableRows(snap: LaunchTimingSnapshot): Array<{
    name: LaunchTimingEventName;
    state: "observed" | "pending";
    ms: number | null;
  }> {
    return LAUNCH_TIMING_EVENT_NAMES.map((name) => {
      const observed = snap.events.find((e) => e.name === name);
      return observed
        ? { name, state: "observed" as const, ms: observed.relativeMs }
        : { name, state: "pending" as const, ms: null };
    });
  }

  it("anchors render observed; other events render pending until marked", () => {
    const r = new LaunchTimingRecorder({ now: () => 0 });
    const rows = renderableRows(r.snapshot());
    const observedCount = rows.filter((row) => row.state === "observed").length;
    expect(observedCount).toBe(1);
    const launchRow = rows.find((row) => row.name === "launch_started");
    expect(launchRow?.state).toBe("observed");
    expect(launchRow?.ms).toBe(0);
    const pendingNames = rows
      .filter((row) => row.state === "pending")
      .map((row) => row.name);
    expect(pendingNames).toContain("ws_open");
    expect(pendingNames).toContain("first_output");
    expect(pendingNames).toContain("ws_close");
    expect(pendingNames).toContain("error");
  });
});

describe("formatRelativeMs", () => {
  it("formats sub-second values in ms with one decimal", () => {
    expect(formatRelativeMs(0)).toBe("0.0 ms");
    expect(formatRelativeMs(12)).toBe("12.0 ms");
    expect(formatRelativeMs(999.49)).toBe("999.5 ms");
  });

  it("formats >= 1 s values in seconds with one decimal", () => {
    expect(formatRelativeMs(1_000)).toBe("1.0 s");
    expect(formatRelativeMs(76_500)).toBe("76.5 s");
  });

  it("returns a dash for non-finite or negative input", () => {
    expect(formatRelativeMs(Number.NaN)).toBe("—");
    expect(formatRelativeMs(-1)).toBe("—");
    expect(formatRelativeMs(Number.POSITIVE_INFINITY)).toBe("—");
  });
});
