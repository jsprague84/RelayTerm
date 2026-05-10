import { describe, it, expect, vi } from "vitest";
import {
  buildAttachWsUrl,
  classifyReconnectAttempt,
  computeWorkspaceEnablement,
  DETACHED_TTL_MS,
  derivePhase,
  describeLaunchError,
  describeWorkspaceError,
  phaseLabel,
  phaseTone,
  RECONNECT_CLOSED_MESSAGE,
  RECONNECT_INELIGIBLE_MESSAGE,
  safeClearViewport,
  safeFit,
  safeFocus,
  TERMINAL_UX_COPY,
  type WorkspacePhase,
} from "../src/lib/app/terminal/terminalLaunch.js";

/**
 * Sentinel that should NEVER appear in any user-visible launch summary.
 * Mirrors the `terminalSessionsApi.test.ts` redaction canary so a future
 * "be helpful and include the wire message" regression in either
 * formatter trips a test.
 */
const SENTINEL = "RELAY_SENTINEL_LAUNCH_OPERATOR_DETAIL_5511";

describe("DETACHED_TTL_MS", () => {
  it("matches the backend's pinned DETACHED_LIVE_PTY_TTL", () => {
    // The constant is duplicated in
    // `apps/web/src/lib/app/terminal/terminalLaunch.ts` (and again in
    // `apps/web/src/lib/dev/liveTerminalState.ts` for the dev lab).
    // The backend pins `relayterm_terminal::DETACHED_LIVE_PTY_TTL` at
    // 30s. Drift is a bug — both copies must move in lockstep with
    // the Rust constant. This test fails loudly if either side bumps
    // the value without the other.
    expect(DETACHED_TTL_MS).toBe(30_000);
  });
});

describe("derivePhase", () => {
  it("idle when client state is null and not creating", () => {
    expect(
      derivePhase({ clientState: null, replayActive: false, creating: false }),
    ).toBe<WorkspacePhase>("idle");
  });

  it("creating overrides everything else", () => {
    expect(
      derivePhase({
        clientState: "attached",
        replayActive: false,
        creating: true,
      }),
    ).toBe<WorkspacePhase>("creating");
  });

  it("attached + replayActive becomes replaying", () => {
    expect(
      derivePhase({
        clientState: "attached",
        replayActive: true,
        creating: false,
      }),
    ).toBe<WorkspacePhase>("replaying");
  });

  it("maps the remaining client states 1:1", () => {
    const cases: Array<["idle" | "connecting" | "detached" | "closed" | "error", WorkspacePhase]> = [
      ["idle", "idle"],
      ["connecting", "connecting"],
      ["detached", "detached"],
      ["closed", "closed"],
      ["error", "error"],
    ];
    for (const [state, expected] of cases) {
      expect(
        derivePhase({
          clientState: state,
          replayActive: false,
          creating: false,
        }),
      ).toBe(expected);
    }
  });
});

describe("phase label/tone tables", () => {
  it("returns a stable label for every phase", () => {
    const phases: WorkspacePhase[] = [
      "idle",
      "creating",
      "connecting",
      "attached",
      "replaying",
      "detached",
      "closed",
      "error",
    ];
    for (const p of phases) {
      expect(phaseLabel(p)).toMatch(/.+/);
      expect(phaseTone(p)).toMatch(/^(neutral|info|ok|warn|error)$/);
    }
  });
});

describe("computeWorkspaceEnablement", () => {
  it("enables detach/close/dispose/focus/fit/clear only while attached", () => {
    const e = computeWorkspaceEnablement({ phase: "attached", lastSeenSeq: 0 });
    expect(e).toEqual({
      detach: true,
      close: true,
      reconnect: false,
      dispose: true,
      focus: true,
      fit: true,
      clear: true,
    });
  });

  it("treats replaying as a sub-state of attached for action enablement", () => {
    const e = computeWorkspaceEnablement({ phase: "replaying", lastSeenSeq: 4 });
    expect(e.detach).toBe(true);
    expect(e.close).toBe(true);
    expect(e.dispose).toBe(true);
    expect(e.reconnect).toBe(false);
    expect(e.focus).toBe(true);
    expect(e.fit).toBe(true);
    expect(e.clear).toBe(true);
  });

  it("enables reconnect from detached/error only when lastSeenSeq > 0", () => {
    for (const p of ["detached", "error"] as const) {
      expect(
        computeWorkspaceEnablement({ phase: p, lastSeenSeq: 0 }).reconnect,
      ).toBe(false);
      expect(
        computeWorkspaceEnablement({ phase: p, lastSeenSeq: 1 }).reconnect,
      ).toBe(true);
    }
  });

  it("disables reconnect from a closed phase regardless of lastSeenSeq", () => {
    // Closed sessions cannot be re-attached; the orchestrator dropped
    // the runtime. A reconnect would open a WebSocket that fails. The
    // production workspace must keep the affordance disabled across
    // every bookmark value — the staging-smoke "End session → Reconnect
    // → connection error" UX bug came from a closed phase still
    // satisfying the reconnect predicate when `lastSeenSeq > 0`.
    for (const seq of [0, 1, 99, 1_000_000]) {
      expect(
        computeWorkspaceEnablement({ phase: "closed", lastSeenSeq: seq })
          .reconnect,
      ).toBe(false);
    }
  });

  it("disables reconnect while live or pending", () => {
    for (const p of ["attached", "replaying", "creating", "connecting"] as const) {
      expect(
        computeWorkspaceEnablement({ phase: p, lastSeenSeq: 99 }).reconnect,
      ).toBe(false);
    }
  });

  it("disables every action while idle", () => {
    expect(
      computeWorkspaceEnablement({ phase: "idle", lastSeenSeq: 0 }),
    ).toEqual({
      detach: false,
      close: false,
      reconnect: false,
      dispose: false,
      focus: false,
      fit: false,
      clear: false,
    });
  });

  it("disables dispose while creating (the create call is owned by the parent)", () => {
    expect(
      computeWorkspaceEnablement({ phase: "creating", lastSeenSeq: 0 }).dispose,
    ).toBe(false);
  });

  it("disables focus/fit/clear unless live", () => {
    for (const p of [
      "idle",
      "creating",
      "connecting",
      "detached",
      "closed",
      "error",
    ] as const) {
      const e = computeWorkspaceEnablement({ phase: p, lastSeenSeq: 5 });
      expect(e.focus).toBe(false);
      expect(e.fit).toBe(false);
      expect(e.clear).toBe(false);
    }
  });
});

describe("classifyReconnectAttempt (launch-guard)", () => {
  // Defence in depth: even if a stale click slips past the disabled
  // Reconnect button (concurrent state-change race, future regression
  // that re-enables it), the imperative click handler MUST refuse to
  // open a WebSocket against a closed session. The classifier is the
  // pure boundary the click handler delegates to; the test pins both
  // the decision table AND the user-facing copy.

  it("blocks a closed-phase reconnect with a non-technical message", () => {
    const result = classifyReconnectAttempt({ phase: "closed" });
    expect(result.kind).toBe("blocked");
    if (result.kind !== "blocked") return;
    expect(result.summary).toBe(RECONNECT_CLOSED_MESSAGE);
    // The message must be honest about the cause and avoid the generic
    // "connection error" shape the bug originally produced.
    expect(result.summary.toLowerCase()).toContain("closed");
    expect(result.summary.toLowerCase()).toContain("cannot be reconnected");
    expect(result.summary.toLowerCase()).not.toContain("websocket");
  });

  it("permits a reconnect from detached/error", () => {
    for (const p of ["detached", "error"] as const) {
      expect(classifyReconnectAttempt({ phase: p }).kind).toBe("permit");
    }
  });

  it("blocks a reconnect from idle/creating/connecting/attached/replaying with the generic ineligible message", () => {
    for (const p of [
      "idle",
      "creating",
      "connecting",
      "attached",
      "replaying",
    ] as const) {
      const result = classifyReconnectAttempt({ phase: p });
      expect(result.kind).toBe("blocked");
      if (result.kind !== "blocked") return;
      // Closed gets a phase-specific message; the rest fall back to a
      // generic "not eligible" string so a stale click on a transient
      // phase does not produce a misleading "session closed" copy.
      expect(result.summary).toBe(RECONNECT_INELIGIBLE_MESSAGE);
      expect(result.summary).not.toBe(RECONNECT_CLOSED_MESSAGE);
    }
  });
});

describe("describeLaunchError", () => {
  it("formats validation errors with the structured reason", () => {
    expect(
      describeLaunchError({ kind: "validation", reason: "missing_server_profile_id" }),
    ).toBe("Could not start terminal: missing_server_profile_id");
  });

  it("formats http errors with status + code only", () => {
    expect(
      describeLaunchError({
        kind: "http",
        status: 409,
        code: "host_key",
        message: `peer banner ${SENTINEL}`,
      }),
    ).toBe("Could not start terminal: HTTP 409 host_key");
  });

  it("never echoes the wire message of an http error", () => {
    const summary = describeLaunchError({
      kind: "http",
      status: 502,
      code: "bad_gateway",
      message: SENTINEL,
    });
    expect(summary).not.toContain(SENTINEL);
  });

  it("formats transport errors without echoing the thrown Error.message", () => {
    const summary = describeLaunchError({
      kind: "transport",
      message: `request to https://example.com/path with headers ${SENTINEL}`,
    });
    expect(summary).toBe("Could not start terminal: transport error");
    expect(summary).not.toContain(SENTINEL);
    expect(summary).not.toContain("https://");
  });

  it("formats malformed_response without echoing payload bytes", () => {
    expect(describeLaunchError({ kind: "malformed_response" })).toBe(
      "Could not start terminal: malformed response",
    );
  });
});

describe("describeWorkspaceError", () => {
  it("formats every TerminalClientError kind without leaking server message", () => {
    expect(describeWorkspaceError({ kind: "transport", message: SENTINEL })).toBe(
      "Connection error",
    );
    expect(describeWorkspaceError({ kind: "decode", message: SENTINEL })).toBe(
      "Protocol decode error",
    );
    expect(
      describeWorkspaceError({ kind: "unexpected_first_frame", message: SENTINEL }),
    ).toBe("Unexpected protocol handshake");
    expect(
      describeWorkspaceError({ kind: "send_before_attached", message: SENTINEL }),
    ).toBe("Send attempted before attach");
    expect(
      describeWorkspaceError({ kind: "send_after_terminal", message: SENTINEL }),
    ).toBe("Send attempted after session ended");
  });

  it("includes the wire-stable error code on server errors but not the message", () => {
    expect(
      describeWorkspaceError({
        kind: "server_error",
        code: "ssh_start_failed",
        message: SENTINEL,
      }),
    ).toBe("Server error: ssh_start_failed");
    expect(
      describeWorkspaceError({
        kind: "server_error",
        message: SENTINEL,
      }),
    ).toBe("Server error");
  });

  it("never includes the SENTINEL across the full enum", () => {
    const inputs = [
      { kind: "transport" as const, message: SENTINEL },
      { kind: "decode" as const, message: SENTINEL },
      { kind: "unexpected_first_frame" as const, message: SENTINEL },
      { kind: "send_before_attached" as const, message: SENTINEL },
      { kind: "send_after_terminal" as const, message: SENTINEL },
      { kind: "server_error" as const, message: SENTINEL },
      { kind: "server_error" as const, code: "ssh_start_failed", message: SENTINEL },
    ];
    for (const err of inputs) {
      expect(describeWorkspaceError(err)).not.toContain(SENTINEL);
    }
  });
});

describe("safeFocus", () => {
  it("returns false for null/undefined renderer", () => {
    expect(safeFocus(null)).toBe(false);
    expect(safeFocus(undefined)).toBe(false);
  });

  it("calls focus and returns true on a present renderer", () => {
    const focus = vi.fn();
    expect(safeFocus({ focus })).toBe(true);
    expect(focus).toHaveBeenCalledTimes(1);
  });

  it("returns false when renderer.focus throws (dispose race)", () => {
    const focus = vi.fn(() => {
      throw new Error("disposed");
    });
    expect(safeFocus({ focus })).toBe(false);
    expect(focus).toHaveBeenCalledTimes(1);
  });
});

describe("safeFit", () => {
  it("returns null for missing renderer", () => {
    expect(safeFit(null)).toBeNull();
    expect(safeFit(undefined)).toBeNull();
  });

  it("forwards the renderer's fit dims when defined", () => {
    expect(safeFit({ fit: () => ({ cols: 132, rows: 50 }) })).toEqual({
      cols: 132,
      rows: 50,
    });
  });

  it("returns null when renderer declines (pre-mount) without throwing", () => {
    expect(safeFit({ fit: () => null })).toBeNull();
  });

  it("returns null when renderer.fit throws", () => {
    expect(
      safeFit({
        fit: () => {
          throw new Error("disposed");
        },
      }),
    ).toBeNull();
  });
});

describe("safeClearViewport", () => {
  it("returns false for missing renderer", () => {
    expect(safeClearViewport(null)).toBe(false);
    expect(safeClearViewport(undefined)).toBe(false);
  });

  it("calls clear and never invokes any wire-frame surface", () => {
    const clear = vi.fn();
    const surface = { clear };
    expect(safeClearViewport(surface)).toBe(true);
    expect(clear).toHaveBeenCalledTimes(1);
    // No backend client surface is reachable from the helper signature
    // — this test pins the contract by structure: `safeClearViewport`
    // takes a `ClearableRenderer` only, never a session client or
    // transport. Adding a wire-side call would require widening the
    // type, which would trip review.
  });

  it("returns false when renderer.clear throws", () => {
    expect(
      safeClearViewport({
        clear: () => {
          throw new Error("disposed");
        },
      }),
    ).toBe(false);
  });
});

describe("TERMINAL_UX_COPY", () => {
  it("settings note names the apply-on-new-session limitation", () => {
    expect(TERMINAL_UX_COPY.settingsApplyNote.toLowerCase()).toContain(
      "new terminal sessions",
    );
  });

  it("copy/paste note flags bracketed paste / OSC 52 as future work", () => {
    const note = TERMINAL_UX_COPY.copyPasteNote.toLowerCase();
    expect(note).toContain("future work");
    expect(note).toContain("bracketed");
    expect(note).toContain("osc 52");
  });

  it("static UX copy never contains operator-detail / bytes / private-key sentinels", () => {
    const blob = Object.values(TERMINAL_UX_COPY).join("\n");
    for (const banned of [
      SENTINEL,
      "private_key",
      "encrypted_private_key",
      "BEGIN OPENSSH",
      "session_output",
    ]) {
      expect(blob).not.toContain(banned);
    }
  });
});

describe("buildAttachWsUrl", () => {
  it("uses wss when the page protocol is https", () => {
    expect(
      buildAttachWsUrl({
        sessionId: "11111111-1111-1111-1111-111111111111",
        protocol: "https:",
        host: "relay.example:8443",
      }),
    ).toBe(
      "wss://relay.example:8443/api/v1/terminal-sessions/11111111-1111-1111-1111-111111111111/ws",
    );
  });

  it("uses ws on plain http", () => {
    expect(
      buildAttachWsUrl({
        sessionId: "abc",
        protocol: "http:",
        host: "localhost:5173",
      }),
    ).toBe("ws://localhost:5173/api/v1/terminal-sessions/abc/ws");
  });

  it("encodes path-unsafe characters in the session id", () => {
    // The backend extracts the id via axum's `Path<TerminalSessionId>`
    // (UUID-typed); a non-UUID id would 404 server-side. The helper's
    // job is to NOT smuggle a `/` or `?` in the path — encoding makes
    // a malformed id surface as a clean 404 rather than as a different
    // route.
    const url = buildAttachWsUrl({
      sessionId: "abc/../def?x=1",
      protocol: "http:",
      host: "localhost",
    });
    expect(url).toBe(
      "ws://localhost/api/v1/terminal-sessions/abc%2F..%2Fdef%3Fx%3D1/ws",
    );
  });
});
