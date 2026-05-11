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
import { describeDetachedTtl } from "../src/lib/api/sessionPolicy.js";

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

  it("detached copy with no TTL arg falls back to the SPEC-pinned ~30 s default", () => {
    // When the policy fetch has not yet resolved (or failed), callers
    // omit the second argument and the helper defaults to the SPEC-
    // pinned `DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS = 30 s` so the UI
    // renders honest copy without blocking on the network round-trip.
    const copy = describeSessionStatus("detached");
    expect(copy.toLowerCase()).toContain("30 seconds");
    // Persistence disclaimer is load-bearing: replay is in-memory and
    // does not survive a backend restart.
    expect(copy.toLowerCase()).toContain("in-memory");
    expect(copy.toLowerCase()).toContain("backend restart");
  });

  it("detached copy parametises on the deployment's configured TTL", () => {
    // Operator-set 1800 s window (the 2026-05-10 long-TTL smoke value).
    // The copy MUST reflect this, not the legacy 30 s literal.
    const copy = describeSessionStatus("detached", 1800);
    expect(copy).toContain("30 minutes");
    expect(copy).not.toContain("30 seconds");
    // Disclaimer stays put regardless of the configured window.
    expect(copy.toLowerCase()).toContain("in-memory");
    expect(copy.toLowerCase()).toContain("backend restart");
  });

  it("detached copy renders hours for an operator-set multi-hour window", () => {
    const copy = describeSessionStatus("detached", 4 * 60 * 60);
    expect(copy).toContain("4 hours");
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

/**
 * Anti-overclaim register from `docs/persistent-sessions.md` § 11.7.
 *
 * The design doc names this file as the canonical home of the
 * forbidden-substring sweep for the four §11.1 UX-copy sources
 * (`sessionStatus.ts::describeSessionStatus`,
 * `SessionsView.svelte` header + row hint,
 * `ProductionTerminal.svelte` TTL hint,
 * `TerminalView.svelte` empty-state blurb). The svelte view files
 * each render the actual production user-facing strings via the two
 * pure formatters under test below
 * (`describeSessionStatus(status, ttl)` and `describeDetachedTtl(ttl)`),
 * so sweeping the formatters covers every byte that reaches the DOM.
 *
 * A future revision that loosens the disclaimer MUST update the
 * persistent-sessions design doc first.
 */
const PERSISTENCE_OVERCLAIM_FORBIDDEN_SUBSTRINGS = [
  "your session is saved",
  "always available",
  "your shell will resume automatically",
  "persistent across restart",
  "session recovery",
  "your work is preserved",
] as const;

/**
 * Configured TTL windows the sweep exercises. Covers the SPEC-pinned
 * default plus the values exercised by the long-TTL smokes (1800 s),
 * the validator hard cap (86_400 s), and a short staging override
 * (5 s). A future deployment value outside this set should still pass
 * — the forbidden phrases are static — but pinning a representative
 * spread documents intent.
 */
const TTL_SWEEP_WINDOWS = [5, 30, 300, 1800, 4 * 60 * 60, 86_400] as const;

describe("anti-overclaim forbidden-substring sweep", () => {
  it("never appears in describeSessionStatus across all statuses + configured TTLs", () => {
    for (const status of ALL_STATUSES) {
      for (const ttl of TTL_SWEEP_WINDOWS) {
        const copy = describeSessionStatus(status, ttl).toLowerCase();
        for (const phrase of PERSISTENCE_OVERCLAIM_FORBIDDEN_SUBSTRINGS) {
          expect(copy).not.toContain(phrase);
        }
      }
      // Also sweep the default-fallback branch (no TTL arg) — the
      // most likely path during a transient policy fetch.
      const fallback = describeSessionStatus(status).toLowerCase();
      for (const phrase of PERSISTENCE_OVERCLAIM_FORBIDDEN_SUBSTRINGS) {
        expect(fallback).not.toContain(phrase);
      }
    }
  });

  it("never appears in describeDetachedTtl across all configured TTL windows", () => {
    for (const ttl of TTL_SWEEP_WINDOWS) {
      const copy = describeDetachedTtl(ttl).toLowerCase();
      for (const phrase of PERSISTENCE_OVERCLAIM_FORBIDDEN_SUBSTRINGS) {
        expect(copy).not.toContain(phrase);
      }
    }
  });
});
