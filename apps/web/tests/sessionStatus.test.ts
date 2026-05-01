import { describe, expect, it } from "vitest";
import {
  canClose,
  canReconnect,
  describeSessionStatus,
  showsTtlHint,
  statusLabel,
  statusTone,
} from "../src/lib/app/terminal/sessionStatus.js";
import type { TerminalSessionStatus } from "../src/lib/api/terminalSessions.js";

const ALL_STATUSES: TerminalSessionStatus[] = [
  "starting",
  "active",
  "detached",
  "closed",
];

describe("statusLabel / statusTone", () => {
  it("returns a label for every wire status", () => {
    for (const s of ALL_STATUSES) {
      expect(statusLabel(s).length).toBeGreaterThan(0);
    }
  });

  it("uses tones that match the operator's mental model", () => {
    expect(statusTone("starting")).toBe("info");
    expect(statusTone("active")).toBe("ok");
    expect(statusTone("detached")).toBe("warn");
    expect(statusTone("closed")).toBe("neutral");
  });
});

describe("canReconnect", () => {
  it("is false for closed sessions", () => {
    expect(canReconnect("closed")).toBe(false);
  });

  it("is false for starting sessions (runtime not yet bound)", () => {
    expect(canReconnect("starting")).toBe(false);
  });

  it("is true for active and detached sessions", () => {
    expect(canReconnect("active")).toBe(true);
    expect(canReconnect("detached")).toBe(true);
  });
});

describe("canClose", () => {
  it("is false for already-closed sessions (UI keeps the button disabled even though the backend close is idempotent)", () => {
    expect(canClose("closed")).toBe(false);
  });

  it("is true for starting/active/detached", () => {
    expect(canClose("starting")).toBe(true);
    expect(canClose("active")).toBe(true);
    expect(canClose("detached")).toBe(true);
  });
});

describe("showsTtlHint", () => {
  it("is true only for detached", () => {
    expect(showsTtlHint("detached")).toBe(true);
    expect(showsTtlHint("active")).toBe(false);
    expect(showsTtlHint("starting")).toBe(false);
    expect(showsTtlHint("closed")).toBe(false);
  });
});

describe("describeSessionStatus", () => {
  it("returns honest copy for every status", () => {
    for (const s of ALL_STATUSES) {
      expect(describeSessionStatus(s).length).toBeGreaterThan(0);
    }
  });

  it("detached copy mentions the ~30s TTL and the in-memory replay limit", () => {
    const copy = describeSessionStatus("detached");
    expect(copy).toContain("30s");
    // The honesty rule from SPEC: replay is in-memory and does not
    // survive a backend restart. The copy must not promise otherwise.
    expect(copy.toLowerCase()).toContain("in-memory");
  });

  it("closed copy does not promise reconnection", () => {
    const copy = describeSessionStatus("closed").toLowerCase();
    expect(copy).toContain("ended");
    // The phrase "cannot be reconnected" is the load-bearing claim. Any
    // future copy that drops that claim while keeping "closed" as the
    // status would silently mislead the operator.
    expect(copy).toContain("cannot be reconnected");
  });

  it("starting copy explains why reconnect is not yet available", () => {
    const copy = describeSessionStatus("starting").toLowerCase();
    expect(copy).toContain("starting");
  });

  it("active copy points at the close action without overpromising", () => {
    const copy = describeSessionStatus("active").toLowerCase();
    expect(copy).toContain("live");
  });
});
