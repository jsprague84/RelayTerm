import { readFileSync } from "node:fs";
import { describe, it, expect, vi } from "vitest";
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
} from "@relayterm/terminal-core";
import {
  buildAttachWsUrl,
  classifyReconnectAttempt,
  computeWorkspaceEnablement,
  derivePhase,
  describeLaunchError,
  describeWorkspaceError,
  markRendererInputTarget,
  mountRendererSafely,
  phaseLabel,
  phaseTone,
  RECONNECT_CLOSED_MESSAGE,
  RECONNECT_INELIGIBLE_MESSAGE,
  RENDERER_MOUNT_FAILED_MESSAGE,
  safeClearViewport,
  safeFit,
  safeFocus,
  TERMINAL_INPUT_MARKER_ATTR,
  TERMINAL_UX_COPY,
  unmarkRendererInputTarget,
  type WorkspacePhase,
} from "../src/lib/app/terminal/terminalLaunch.js";
import { DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS } from "../src/lib/api/sessionPolicy.js";

/**
 * Sentinel that should NEVER appear in any user-visible launch summary.
 * Mirrors the `terminalSessionsApi.test.ts` redaction canary so a future
 * "be helpful and include the wire message" regression in either
 * formatter trips a test.
 */
const SENTINEL = "RELAY_SENTINEL_LAUNCH_OPERATOR_DETAIL_5511";

describe("DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS", () => {
  it("matches the backend's pinned DETACHED_LIVE_PTY_TTL fallback", () => {
    // Production UI now reads the effective detach-TTL window from
    // `GET /api/v1/config/session-policy`; this constant is the
    // safe fallback the SPA uses while the policy fetch is pending
    // OR has failed. It MUST track the backend's
    // `relayterm_terminal::DETACHED_LIVE_PTY_TTL` baseline (30 s) so
    // a not-yet-deployed policy endpoint still renders honest copy.
    // Drift is a bug — bump both sides in lockstep.
    expect(DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS).toBe(30);
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

  it("maps 429 too_many_sessions to safe parameterised copy (Phase 1B.1)", () => {
    // The refusal MUST yield the spec-pinned parameterised copy from
    // `docs/session-quotas.md` § 7.5: opening sentence carries the
    // cap, second sentence the action, third sentence the
    // detached-TTL caveat. The wire `message` is intentionally NOT
    // echoed — even if a future backend revision widened it, the
    // launcher MUST stay independent.
    const summary = describeLaunchError(
      {
        kind: "http",
        status: 429,
        code: "too_many_sessions",
        message: `wire detail ${SENTINEL}`,
      },
      { maxLivePtyPerUser: 4, detachedTtlSeconds: 30 },
    );
    expect(summary).toContain("limit of 4 concurrent terminal sessions");
    expect(summary).toContain("Close a session from the Sessions list");
    expect(summary).toContain("Detached sessions count toward this limit");
    expect(summary).toContain("about 30 seconds");
    expect(summary).not.toContain(SENTINEL);
    // Anti-overclaim register (`docs/session-quotas.md` § 7.5 + § 12).
    const lower = summary.toLowerCase();
    const forbidden = [
      "your session quota",
      "we're rate-limiting you",
      "please slow down",
      "queue",
      "always available",
      "persistent across restart",
    ];
    for (const phrase of forbidden) {
      expect(lower).not.toContain(phrase);
    }
    expect(lower).not.toMatch(/wait \d+ seconds/);
  });

  it("uses singular phrasing when the cap is 1", () => {
    const summary = describeLaunchError(
      {
        kind: "http",
        status: 429,
        code: "too_many_sessions",
        message: "",
      },
      { maxLivePtyPerUser: 1, detachedTtlSeconds: 30 },
    );
    expect(summary).toContain("limit of 1 concurrent terminal session.");
  });

  it("falls back to the default cap AND default TTL when neither is supplied", () => {
    // The launcher MAY be called before `loadSessionPolicy()`
    // resolves. The fallback defaults keep the copy honest.
    const summary = describeLaunchError({
      kind: "http",
      status: 429,
      code: "too_many_sessions",
      message: "",
    });
    expect(summary).toContain("limit of 8 concurrent terminal sessions");
    expect(summary).toContain("about 30 seconds");
  });

  it("parameterises the TTL fragment on the configured window", () => {
    const summary = describeLaunchError(
      {
        kind: "http",
        status: 429,
        code: "too_many_sessions",
        message: "",
      },
      { maxLivePtyPerUser: 8, detachedTtlSeconds: 1800 },
    );
    expect(summary).toContain("about 30 minutes");
    expect(summary).not.toContain("about 30 seconds");
  });

  it("falls through to the generic mapping for other 429 codes", () => {
    // A 429 with a different code (e.g. the login throttler's
    // `too_many_requests`) MUST NOT borrow the quota-refusal copy.
    expect(
      describeLaunchError({
        kind: "http",
        status: 429,
        code: "too_many_requests",
        message: SENTINEL,
      }),
    ).toBe("Could not start terminal: HTTP 429 too_many_requests");
  });

  it("maps 429 too_many_starting_sessions to safe copy (Phase 1B.2a)", () => {
    // The starting-burst refusal MUST yield the spec-pinned copy
    // from `docs/session-quotas.md` § 7.5: opening sentence describes
    // the in-flight nature, second sentence the action ("wait a
    // moment", NOT "wait N seconds"). The wire `message` is
    // intentionally NOT echoed. The wire body intentionally carries
    // no count or cap (§ 7.3); the cap is exposed separately via
    // `describeMaxStartingPerUser` for surfaces that want it.
    const summary = describeLaunchError({
      kind: "http",
      status: 429,
      code: "too_many_starting_sessions",
      message: `wire detail ${SENTINEL}`,
    });
    expect(summary).toBe(
      "You already have the maximum number of terminal sessions starting. Wait a moment for one to finish starting, then try again.",
    );
    expect(summary).not.toContain(SENTINEL);
    // Trailing-punctuation guard: the copy must end at the second
    // sentence's period, with no stray ". ." or trailing space.
    expect(summary).toMatch(/try again\.$/);
    expect(summary).not.toMatch(/\. \.$/);
    // Anti-overclaim register (`docs/session-quotas.md` § 7.5 + § 12).
    const lower = summary.toLowerCase();
    const forbidden = [
      "your session quota",
      "we're rate-limiting you",
      "please slow down",
      "queue",
      "always available",
      "persistent across restart",
    ];
    for (const phrase of forbidden) {
      expect(lower).not.toContain(phrase);
    }
    expect(lower).not.toMatch(/wait \d+ seconds/);
  });

  it("does not borrow live-cap copy for too_many_starting_sessions", () => {
    // The two refusal copies MUST stay distinct so the caller can
    // tell the in-flight burst case ("wait a moment for an in-flight
    // start to complete") from the live cap case ("close a session
    // before starting another"). A future refactor that collapsed
    // them would mislead the user about which action helps.
    const summary = describeLaunchError({
      kind: "http",
      status: 429,
      code: "too_many_starting_sessions",
      message: "",
    });
    expect(summary).not.toContain("Close a session from the Sessions list");
    expect(summary).not.toContain("Detached sessions count toward this limit");
  });

  it("maps 429 too_many_sessions_deployment to safe static copy (Phase 1B.2b)", () => {
    // The deployment-wide refusal MUST yield the spec-pinned STATIC
    // copy from `docs/session-quotas.md` § 7.5 — NOT parameterised on
    // a numeric cap (the deployment cap is NOT exposed via
    // session-policy, § 5.4 — operator-only, fingerprinting risk).
    // The wire `message` is intentionally NOT echoed.
    const summary = describeLaunchError({
      kind: "http",
      status: 429,
      code: "too_many_sessions_deployment",
      message: `wire detail ${SENTINEL}`,
    });
    expect(summary).toBe(
      "This RelayTerm deployment is at its live terminal session limit. Close an existing session or wait for a detached session to expire before starting another.",
    );
    expect(summary).not.toContain(SENTINEL);
    // Trailing-punctuation guard: the copy must end at the second
    // sentence's period, with no stray ". ." or trailing space.
    expect(summary).toMatch(/another\.$/);
    expect(summary).not.toMatch(/\. \.$/);
    // Anti-overclaim register (`docs/session-quotas.md` § 7.5).
    // Deployment-scope-specific additions: must NOT mention "other
    // users" (owner-scope leak), must NOT name a numeric cap (the
    // value is not exposed on the wire), must NOT include
    // wall-clock wait language.
    const lower = summary.toLowerCase();
    const forbidden = [
      "your session quota",
      "we're rate-limiting you",
      "please slow down",
      "queue",
      "always available",
      "persistent across restart",
      "other users",
      "another user",
      "retry-after",
    ];
    for (const phrase of forbidden) {
      expect(lower).not.toContain(phrase);
    }
    expect(lower).not.toMatch(/wait \d+ seconds/);
    // The copy must not surface a numeric cap value (operator-only).
    expect(summary).not.toMatch(/\b\d+\b/);
  });

  it("ignores cap/ttl options for too_many_sessions_deployment (static copy)", () => {
    // Even when the caller passes maxLivePtyPerUser / detachedTtlSeconds
    // (e.g. because the SPA already loaded session-policy for the
    // per-user surface), the deployment-cap copy MUST remain static.
    // The deployment cap is NOT exposed via session-policy and any
    // parameterisation here would invite "your session quota"-style
    // overclaim.
    const summary = describeLaunchError(
      {
        kind: "http",
        status: 429,
        code: "too_many_sessions_deployment",
        message: "",
      },
      { maxLivePtyPerUser: 12, detachedTtlSeconds: 1800 },
    );
    expect(summary).not.toMatch(/\b12\b/);
    expect(summary).not.toContain("about 30 minutes");
    expect(summary).not.toContain("about 30 seconds");
  });

  it("does not borrow per-user copy for too_many_sessions_deployment", () => {
    // The deployment copy MUST stay distinct from both per-user
    // copies. Collapsing them would mislead the user about the right
    // action (per-user → close one of YOUR sessions; deployment →
    // wait for a detached session to expire OR close one).
    const summary = describeLaunchError({
      kind: "http",
      status: 429,
      code: "too_many_sessions_deployment",
      message: "",
    });
    expect(summary).not.toContain("limit of");
    expect(summary).not.toContain("Sessions list");
    expect(summary).not.toContain("Wait a moment for one to finish starting");
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

describe("markRendererInputTarget", () => {
  it("returns null for a null / non-object renderer", () => {
    expect(markRendererInputTarget(null)).toBeNull();
    expect(markRendererInputTarget(undefined)).toBeNull();
    expect(markRendererInputTarget("xterm")).toBeNull();
  });

  it("returns null when the renderer does not implement focusTarget()", () => {
    // restty today: the optional interface method is absent. The
    // workspace must degrade cleanly — no throw, no marker.
    const renderer = { focus: vi.fn() };
    expect(markRendererInputTarget(renderer)).toBeNull();
  });

  it("returns null when focusTarget() yields no element (pre-mount / post-dispose)", () => {
    expect(markRendererInputTarget({ focusTarget: () => null })).toBeNull();
    expect(
      markRendererInputTarget({ focusTarget: () => undefined }),
    ).toBeNull();
  });

  it("returns null and swallows when focusTarget() throws (dispose race)", () => {
    const focusTarget = vi.fn(() => {
      throw new Error("renderer disposed");
    });
    expect(markRendererInputTarget({ focusTarget })).toBeNull();
    expect(focusTarget).toHaveBeenCalledTimes(1);
  });

  it("stamps the renderer-neutral marker attribute and returns the element", () => {
    // Minimal element stub: only `setAttribute` / `removeAttribute` are
    // exercised — the helper never reads value / textContent / dataset.
    const setAttribute = vi.fn();
    const removeAttribute = vi.fn();
    const element = { setAttribute, removeAttribute };
    const result = markRendererInputTarget({ focusTarget: () => element });
    expect(result).toBe(element);
    expect(setAttribute).toHaveBeenCalledTimes(1);
    expect(setAttribute).toHaveBeenCalledWith(TERMINAL_INPUT_MARKER_ATTR, "true");
  });

  it("uses a dedicated attribute that cannot collide with data-testid", () => {
    // ghostty-web's focus target IS the viewport element, which already
    // carries `data-testid="production-terminal-viewport"`. The marker
    // must be a separate attribute so it coexists rather than clobbers.
    expect(TERMINAL_INPUT_MARKER_ATTR).toBe("data-relayterm-terminal-input");
    expect(TERMINAL_INPUT_MARKER_ATTR).not.toBe("data-testid");
  });

  it("returns null when the focus target is not a settable DOM element", () => {
    // A renderer that returns something element-shaped but without
    // `setAttribute` / `removeAttribute` must not throw.
    expect(markRendererInputTarget({ focusTarget: () => ({}) })).toBeNull();
    expect(
      markRendererInputTarget({ focusTarget: () => ({ setAttribute: vi.fn() }) }),
    ).toBeNull();
  });
});

describe("unmarkRendererInputTarget", () => {
  it("is a no-op for renderers with no resolvable input element", () => {
    expect(() => unmarkRendererInputTarget(null)).not.toThrow();
    expect(() => unmarkRendererInputTarget({ focus: vi.fn() })).not.toThrow();
    expect(() =>
      unmarkRendererInputTarget({ focusTarget: () => null }),
    ).not.toThrow();
  });

  it("removes the marker attribute from the renderer's input element", () => {
    const setAttribute = vi.fn();
    const removeAttribute = vi.fn();
    const element = { setAttribute, removeAttribute };
    const renderer = { focusTarget: () => element };
    markRendererInputTarget(renderer);
    unmarkRendererInputTarget(renderer);
    expect(removeAttribute).toHaveBeenCalledTimes(1);
    expect(removeAttribute).toHaveBeenCalledWith(TERMINAL_INPUT_MARKER_ATTR);
  });

  it("swallows a throw from removeAttribute (element already gone)", () => {
    const removeAttribute = vi.fn(() => {
      throw new Error("element detached");
    });
    const renderer = {
      focusTarget: () => ({ setAttribute: vi.fn(), removeAttribute }),
    };
    expect(() => unmarkRendererInputTarget(renderer)).not.toThrow();
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

/**
 * Minimal `TerminalRenderer` stub. The renderer-neutral interface is
 * the integration seam between the loader/workspace and the concrete
 * drawing backend; the helper only needs `mount` to honour, so the
 * other methods are inert. `disposed` is a test-side counter so the
 * "helper does NOT dispose" rule can be pinned directly.
 */
/**
 * The vitest config uses the `node` environment (no jsdom), so a real
 * `HTMLElement` is not available here. `mountRendererSafely` forwards
 * the target to `renderer.mount(target)` without inspecting it, and the
 * stub renderer below ignores the target too — a placeholder cast lets
 * the helper signature stay honest without dragging jsdom in.
 */
function fakeTarget(): HTMLElement {
  return {} as HTMLElement;
}

class StubMountRenderer implements TerminalRenderer {
  public disposed = 0;
  constructor(
    private readonly mountImpl: (
      target: HTMLElement,
    ) => void | Promise<void>,
  ) {}
  mount(target: HTMLElement): void | Promise<void> {
    return this.mountImpl(target);
  }
  write(_chunk: RendererOutput): void {}
  resize(_cols: number, _rows: number): void {}
  focus(): void {}
  dispose(): void {
    this.disposed += 1;
  }
  onInput(_cb: (data: RendererInput) => void): () => void {
    return () => {};
  }
  onResize(
    _cb: (size: { cols: number; rows: number }) => void,
  ): () => void {
    return () => {};
  }
}

describe("RENDERER_MOUNT_FAILED_MESSAGE", () => {
  it("names the failure taxonomy AND the remediation path, in stable copy", () => {
    // SMOKE runbook + redaction sentinel + the workspace error panel
    // all read this exact string; a copy drift would either silently
    // confuse the operator (no remediation hint) or break the
    // structural pins below. Lock it.
    expect(RENDERER_MOUNT_FAILED_MESSAGE).toBe(
      "Renderer failed to mount. Switch back to xterm in Settings and reopen the terminal.",
    );
  });

  it("never includes the renderer id, WASM URL, or any sentinel-shaped detail", () => {
    // The 2026-05-13 ghostty-web staging smoke surfaced three distinct
    // pieces of operator noise inside the underlying `Error` chain: the
    // adapter package name, the inlined `data:application/wasm;base64,…`
    // URL, and a CSP directive verbatim. The fixed message MUST NOT
    // contain any of them — only the taxonomy + the recovery action.
    const lowered = RENDERER_MOUNT_FAILED_MESSAGE.toLowerCase();
    expect(lowered).not.toContain("ghostty");
    expect(lowered).not.toContain("restty");
    expect(lowered).not.toContain("wterm");
    expect(lowered).not.toContain("wasm");
    expect(lowered).not.toContain("data:");
    expect(lowered).not.toContain("default-src");
    expect(lowered).not.toContain(SENTINEL.toLowerCase());
  });
});

describe("mountRendererSafely — success path", () => {
  it("returns { kind: 'mounted' } when the renderer's mount resolves", async () => {
    const r = new StubMountRenderer(async () => {});
    const outcome = await mountRendererSafely(r, fakeTarget());
    expect(outcome).toEqual({ kind: "mounted" });
    expect(r.disposed).toBe(0);
  });

  it("awaits a Promise-returning mount before resolving", async () => {
    let resolved = false;
    const r = new StubMountRenderer(
      () =>
        new Promise<void>((resolve) => {
          setTimeout(() => {
            resolved = true;
            resolve();
          }, 0);
        }),
    );
    const outcome = await mountRendererSafely(
      r,
      fakeTarget(),
    );
    expect(resolved).toBe(true);
    expect(outcome.kind).toBe("mounted");
  });

  it("returns 'mounted' for a synchronous (void) mount", async () => {
    // xterm's `mount()` is sync today; helper must honour that.
    let called = false;
    const r = new StubMountRenderer(() => {
      called = true;
    });
    const outcome = await mountRendererSafely(
      r,
      fakeTarget(),
    );
    expect(called).toBe(true);
    expect(outcome).toEqual({ kind: "mounted" });
  });
});

describe("mountRendererSafely — failure path", () => {
  it("returns adapter_mount_failed when mount rejects asynchronously", async () => {
    // Models the ghostty-web 0.4.0 staging smoke: dynamic import
    // resolved, constructor ran, but `await init()` inside `mount()`
    // rejected because the staging stack's CSP blocked the
    // `data:application/wasm;base64,…` URL fetch + `WebAssembly.compile`.
    const r = new StubMountRenderer(async () => {
      throw new Error(
        "CompileError: WebAssembly.compile(): default-src 'self' violation",
      );
    });
    const outcome = await mountRendererSafely(
      r,
      fakeTarget(),
    );
    expect(outcome).toEqual({
      kind: "failed",
      fallback: "adapter_mount_failed",
    });
  });

  it("returns adapter_mount_failed when mount throws synchronously", async () => {
    const r = new StubMountRenderer(() => {
      throw new Error("synchronous mount blew up");
    });
    const outcome = await mountRendererSafely(
      r,
      fakeTarget(),
    );
    expect(outcome.kind).toBe("failed");
    if (outcome.kind === "failed") {
      expect(outcome.fallback).toBe("adapter_mount_failed");
    }
  });

  it("does NOT surface the underlying Error.message on failure", async () => {
    // Mirror of the `rendererLoader.test.ts` redaction sentinel: a
    // future "be helpful and include err.message in the outcome"
    // regression would expose a CSP directive, the WASM URL, or
    // stack frames into the renderer adapter. The outcome must stay
    // a closed-vocabulary string.
    const payload = "BEGIN OPENSSH PRIVATE KEY: leaked-via-stack";
    const r = new StubMountRenderer(async () => {
      throw new Error(payload);
    });
    const outcome = await mountRendererSafely(
      r,
      fakeTarget(),
    );
    expect(JSON.stringify(outcome)).not.toContain("OPENSSH");
    expect(JSON.stringify(outcome)).not.toContain("leaked-via-stack");
  });

  it("does NOT dispose the renderer on failure (caller owns lifecycle)", async () => {
    // The workspace's `attach()` disposes the failed renderer itself
    // and then bumps state under the generation guard. If the helper
    // also disposed, a teardown that races with the rejection would
    // double-dispose; pinning the rule here keeps the responsibility
    // explicit.
    const r = new StubMountRenderer(async () => {
      throw new Error("nope");
    });
    await mountRendererSafely(r, fakeTarget());
    expect(r.disposed).toBe(0);
  });

  it("does NOT swallow a non-Error rejection — same closed-vocabulary outcome", async () => {
    // A renderer adapter that rejects with a non-Error value
    // (e.g., a string or object literal) MUST still collapse to the
    // same typed fallback. The catch-all matches `catch` semantics:
    // any thrown value lands in the failure arm.
    const r = new StubMountRenderer(() => {
      // eslint-disable-next-line @typescript-eslint/no-throw-literal
      return Promise.reject("CSP blocked data:application/wasm;base64,...");
    });
    const outcome = await mountRendererSafely(
      r,
      fakeTarget(),
    );
    expect(outcome).toEqual({
      kind: "failed",
      fallback: "adapter_mount_failed",
    });
    expect(JSON.stringify(outcome)).not.toContain("CSP blocked");
    expect(JSON.stringify(outcome)).not.toContain("base64");
  });
});

/**
 * Structural assertions against `ProductionTerminal.svelte`. Raw-text
 * scans (same style as `appShellIsolation.test.ts`) — the goal is to
 * pin the wiring rules that a future refactor could silently break:
 *
 *  - The workspace MUST import and call `mountRendererSafely`.
 *  - The workspace MUST handle the `adapter_mount_failed` arm AND set
 *    `lastError` to `RENDERER_MOUNT_FAILED_MESSAGE`.
 *  - The diagnostic strip MUST render the `adapter_mount_failed`
 *    branch (so the SMOKE runbook's
 *    `production-terminal-renderer-diagnostic` row stays exhaustive).
 *  - The renderer diagnostic block MUST be reachable when the
 *    renderer never mounted but a fallback fired — without that, the
 *    mount-failure path would surface ONLY the rose error panel and
 *    drop its share of the closed vocabulary.
 *
 * These are intentionally NOT AST checks; they remain cheap, robust
 * to formatting drift, and false-positive-free on the small surface
 * area of one component file.
 */
describe("ProductionTerminal.svelte — mount-failure wiring", () => {
  const TERMINAL_PATH = new URL(
    "../src/lib/app/terminal/ProductionTerminal.svelte",
    import.meta.url,
  ).pathname;
  const text = readFileSync(TERMINAL_PATH, "utf8");

  it("imports and calls mountRendererSafely + RENDERER_MOUNT_FAILED_MESSAGE", () => {
    expect(text).toMatch(/\bmountRendererSafely\b/);
    expect(text).toMatch(/\bRENDERER_MOUNT_FAILED_MESSAGE\b/);
    expect(text).toMatch(/await\s+mountRendererSafely\(/);
  });

  it("handles the adapter_mount_failed outcome and sets lastError to the stable string", () => {
    // The mount-failure arm of `attach()` sets:
    //   activeRendererFallback = mountOutcome.fallback
    //   lastError = RENDERER_MOUNT_FAILED_MESSAGE
    // and disposes the renderer. The grep stays loose enough to
    // tolerate intervening comments + blank lines.
    expect(text).toMatch(/mountOutcome\.kind\s*===\s*["']failed["']/);
    expect(text).toMatch(/lastError\s*=\s*RENDERER_MOUNT_FAILED_MESSAGE/);
  });

  it("renders the adapter_mount_failed branch inside the diagnostic strip", () => {
    expect(text).toMatch(
      /activeRendererFallback\s*===\s*["']adapter_mount_failed["']/,
    );
    expect(text).toMatch(/switch back to xterm in Settings/i);
  });

  it("does NOT echo a raw Error.message anywhere inside the mount-failure path", () => {
    // Sanity sentinel against a future regression that built the
    // workspace's error copy from `err.message`. We search the whole
    // component for `err.message` / `Error.message` / `mountOutcome.message`;
    // none should appear.
    expect(text).not.toMatch(/err\.message/);
    expect(text).not.toMatch(/Error\.message/);
    expect(text).not.toMatch(/mountOutcome\.message/);
  });

  it("keeps the renderer diagnostic block reachable when only a fallback is set", () => {
    // Pinned condition: `{#if activeRendererId || activeRendererFallback}`.
    // The original `{#if activeRendererId}` form hid the diagnostic
    // entirely on the mount-failure path, which left the SMOKE
    // selector with nothing to read and reduced the
    // closed-vocabulary surface area.
    expect(text).toMatch(
      /\{#if\s+activeRendererId\s*\|\|\s*activeRendererFallback\}/,
    );
  });
});
