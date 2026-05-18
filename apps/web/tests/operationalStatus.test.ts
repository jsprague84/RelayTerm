import { describe, expect, it } from "vitest";
import {
  buildSessionPolicySummary,
  describeAuthSessions,
  describeAutofit,
  describeDetachedTtlIndicator,
  describeEffectiveRenderer,
  describeExperimentalGate,
  describeQuotaIndicator,
  describeTerminalSessions,
  summarizeAccount,
  summarizeAuthSessions,
  summarizeHealth,
  summarizeTerminalDefaults,
  summarizeTerminalSessions,
  TERMINAL_SESSION_STATUS_DISPLAY_ORDER,
  terminalSessionStatusLabel,
  toneClass,
  type AuthSessionsLoadResult,
} from "../src/lib/app/settings/operationalStatus.js";
import type { AuthSession } from "../src/lib/api/auth.js";
import type { LoadResult } from "../src/lib/api/apiErrors.js";
import type { SessionPolicy } from "../src/lib/api/sessionPolicy.js";
import type {
  TerminalSession,
  TerminalSessionStatus,
} from "../src/lib/api/terminalSessions.js";
import {
  defaultTerminalSettings,
  type TerminalSettings,
} from "../src/lib/app/settings/terminalSettings.js";

/**
 * Sentinels that MUST NEVER appear in any rendered summary the
 * operational-status helpers produce. Mirrors the redaction posture of
 * the dashboard / auth / inventory test suites.
 */
const SENTINELS = [
  "RELAY_SENTINEL_PRIVATE_KEY_BYTES_OP9101",
  "RELAY_SENTINEL_OPERATOR_DETAIL_OP9102",
  "private_key",
  "encrypted_private_key",
  "BEGIN OPENSSH PRIVATE KEY",
  "password_hash",
  "session_token",
  "token_hash",
  "bootstrap_token",
  "client_info",
  "remote_addr",
  "user_agent",
  "data_b64",
] as const;

function authSession(
  overrides: Partial<AuthSession> = {},
): AuthSession {
  return {
    id: "00000000-0000-0000-0000-000000000001",
    created_at: "2026-04-01T00:00:00Z",
    last_seen_at: "2026-04-01T00:00:01Z",
    expires_at: "2026-05-01T00:00:00Z",
    revoked_at: null,
    current: false,
    status: "active",
    ...overrides,
  };
}

function terminalSession(
  status: TerminalSessionStatus,
  id = "11111111-1111-1111-1111-111111111111",
): TerminalSession {
  return {
    id,
    server_profile_id: "22222222-2222-2222-2222-222222222222",
    status,
    cols: 80,
    rows: 24,
    created_at: "2026-04-01T00:00:00Z",
    last_seen_at: "2026-04-01T00:00:00Z",
    closed_at: status === "closed" ? "2026-04-01T00:01:00Z" : null,
  };
}

function ok<T>(data: T): LoadResult<T> {
  return { ok: true, data };
}

function http<T>(): LoadResult<T> {
  return {
    ok: false,
    error: {
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: "RELAY_SENTINEL_OPERATOR_DETAIL_OP9102",
    },
  };
}

function transport<T>(): LoadResult<T> {
  return {
    ok: false,
    error: { kind: "transport", message: "RELAY_SENTINEL_OPERATOR_DETAIL_OP9102" },
  };
}

function authOk(sessions: AuthSession[]): AuthSessionsLoadResult {
  return { ok: true, sessions };
}

function authHttp(): AuthSessionsLoadResult {
  return {
    ok: false,
    error: {
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: "RELAY_SENTINEL_OPERATOR_DETAIL_OP9102",
    },
  };
}

function authTransport(): AuthSessionsLoadResult {
  return {
    ok: false,
    error: { kind: "transport" },
  };
}

describe("summarizeHealth", () => {
  it("collapses 'unknown' to loading", () => {
    expect(summarizeHealth("unknown")).toEqual({ kind: "loading" });
  });

  it("renders 'ok' as a positive indicator with neutral copy", () => {
    expect(summarizeHealth("ok")).toEqual({
      kind: "ready",
      value: "Backend reachable",
      tone: "ok",
    });
  });

  it("renders 'down' as unavailable with safe copy (no transport detail)", () => {
    const s = summarizeHealth("down");
    expect(s.kind).toBe("unavailable");
    if (s.kind === "unavailable") {
      expect(s.summary).toBe("Backend did not respond to a health probe.");
      for (const sentinel of SENTINELS) {
        expect(s.summary).not.toContain(sentinel);
      }
    }
  });
});

describe("summarizeAuthSessions", () => {
  it("returns loading before the first load", () => {
    expect(summarizeAuthSessions(null)).toEqual({ kind: "loading" });
  });

  it("counts each status independently", () => {
    const summary = summarizeAuthSessions(
      authOk([
        authSession({ id: "a", status: "active", current: true }),
        authSession({ id: "b", status: "active" }),
        authSession({ id: "c", status: "expired" }),
        authSession({
          id: "d",
          status: "revoked",
          revoked_at: "2026-04-01T00:00:30Z",
        }),
      ]),
    );
    expect(summary).toEqual({
      kind: "ready",
      counts: {
        total: 4,
        active: 2,
        expired: 1,
        revoked: 1,
        has_current: true,
      },
    });
  });

  it("returns ready with zeros on an empty list", () => {
    expect(summarizeAuthSessions(authOk([]))).toEqual({
      kind: "ready",
      counts: { total: 0, active: 0, expired: 0, revoked: 0, has_current: false },
    });
  });

  it("collapses an HTTP failure to a typed safe summary (no wire message)", () => {
    const s = summarizeAuthSessions(authHttp());
    expect(s.kind).toBe("unavailable");
    if (s.kind === "unavailable") {
      expect(s.summary).not.toContain("RELAY_SENTINEL_OPERATOR_DETAIL_OP9102");
      expect(s.summary).not.toContain("service_unavailable");
    }
  });

  it("collapses a transport failure to a typed safe summary", () => {
    const s = summarizeAuthSessions(authTransport());
    expect(s.kind).toBe("unavailable");
  });
});

describe("describeAuthSessions", () => {
  it("collapses ready/zero to a warn-toned 'no sessions' line", () => {
    const indicator = describeAuthSessions({
      kind: "ready",
      counts: { total: 0, active: 0, expired: 0, revoked: 0, has_current: false },
    });
    expect(indicator).toEqual({
      kind: "ready",
      value: "0 active sessions",
      tone: "warn",
    });
  });

  it("renders singular vs plural correctly and adds the total tail when needed", () => {
    expect(
      describeAuthSessions({
        kind: "ready",
        counts: { total: 1, active: 1, expired: 0, revoked: 0, has_current: true },
      }),
    ).toEqual({ kind: "ready", value: "1 active session", tone: "info" });

    expect(
      describeAuthSessions({
        kind: "ready",
        counts: { total: 3, active: 2, expired: 1, revoked: 0, has_current: true },
      }),
    ).toEqual({
      kind: "ready",
      value: "2 active sessions (3 total including expired or revoked)",
      tone: "info",
    });
  });
});

describe("summarizeTerminalSessions", () => {
  it("aggregates by status (delegates to dashboard helper)", () => {
    const s = summarizeTerminalSessions(
      ok([
        terminalSession("active", "a"),
        terminalSession("active", "b"),
        terminalSession("detached", "c"),
        terminalSession("closed", "d"),
        terminalSession("closed", "e"),
        terminalSession("starting", "f"),
      ]),
    );
    expect(s).toEqual({
      kind: "ready",
      total: 6,
      counts: { active: 2, detached: 1, starting: 1, closed: 2 },
    });
  });

  it("returns zeros on an empty list (still ready)", () => {
    expect(summarizeTerminalSessions(ok([] as TerminalSession[]))).toEqual({
      kind: "ready",
      total: 0,
      counts: { active: 0, detached: 0, starting: 0, closed: 0 },
    });
  });

  it("returns unavailable on http/transport errors", () => {
    expect(summarizeTerminalSessions(http<TerminalSession[]>())).toEqual({
      kind: "unavailable",
    });
    expect(summarizeTerminalSessions(transport<TerminalSession[]>())).toEqual({
      kind: "unavailable",
    });
  });
});

describe("describeTerminalSessions", () => {
  it("renders 'no live terminal sessions' when active+detached+starting = 0", () => {
    expect(
      describeTerminalSessions({
        kind: "ready",
        total: 0,
        counts: { active: 0, detached: 0, starting: 0, closed: 5 },
      }),
    ).toEqual({
      kind: "ready",
      value: "No live terminal sessions.",
      tone: "info",
    });
  });

  it("renders the live-count + starting tail", () => {
    expect(
      describeTerminalSessions({
        kind: "ready",
        total: 4,
        counts: { active: 2, detached: 1, starting: 1, closed: 0 },
      }),
    ).toEqual({
      kind: "ready",
      value: "3 live sessions, 1 starting.",
      tone: "info",
    });

    // Singular live + no starting
    expect(
      describeTerminalSessions({
        kind: "ready",
        total: 1,
        counts: { active: 1, detached: 0, starting: 0, closed: 0 },
      }),
    ).toEqual({
      kind: "ready",
      value: "1 live session.",
      tone: "info",
    });
  });

  it("collapses unavailable to safe copy (no wire detail)", () => {
    const s = describeTerminalSessions({ kind: "unavailable" });
    expect(s.kind).toBe("unavailable");
    if (s.kind === "unavailable") {
      expect(s.summary).toBe("Could not load terminal sessions.");
    }
  });
});

describe("TERMINAL_SESSION_STATUS_DISPLAY_ORDER", () => {
  it("covers every status exactly once", () => {
    expect([...TERMINAL_SESSION_STATUS_DISPLAY_ORDER].sort()).toEqual([
      "active",
      "closed",
      "detached",
      "starting",
    ]);
  });

  it("starts with 'active' (currently-usable first)", () => {
    expect(TERMINAL_SESSION_STATUS_DISPLAY_ORDER[0]).toBe("active");
  });
});

describe("terminalSessionStatusLabel", () => {
  it("labels each status with operator-facing copy", () => {
    expect(terminalSessionStatusLabel("active")).toBe("Active");
    expect(terminalSessionStatusLabel("detached")).toBe("Detached");
    expect(terminalSessionStatusLabel("starting")).toBe("Starting");
    expect(terminalSessionStatusLabel("closed")).toBe("Closed (history)");
  });
});

describe("summarizeTerminalDefaults", () => {
  function settings(overrides: Partial<TerminalSettings> = {}): TerminalSettings {
    return { ...defaultTerminalSettings(), ...overrides };
  }

  it("collapses xterm + gate-off to the v1 default", () => {
    const d = summarizeTerminalDefaults(settings());
    expect(d.effective_renderer).toBe("xterm");
    expect(d.selected_renderer).toBe("xterm");
    expect(d.experimental_gate_enabled).toBe(false);
    expect(d.autofit_enabled).toBe(false);
    expect(d.selection_is_experimental).toBe(false);
    expect(d.selection_currently_downgraded).toBe(false);
  });

  it("flags a stale experimental selection as currently downgraded when gate is off", () => {
    const d = summarizeTerminalDefaults(
      settings({
        rendererId: "ghostty-web",
        experimentalRendererEvaluationEnabled: false,
      }),
    );
    expect(d.selected_renderer).toBe("ghostty-web");
    expect(d.effective_renderer).toBe("xterm");
    expect(d.selection_is_experimental).toBe(true);
    expect(d.selection_currently_downgraded).toBe(true);
  });

  it("honours an experimental selection when the gate is ON", () => {
    const d = summarizeTerminalDefaults(
      settings({
        rendererId: "wterm",
        experimentalRendererEvaluationEnabled: true,
      }),
    );
    expect(d.effective_renderer).toBe("wterm");
    expect(d.selection_currently_downgraded).toBe(false);
  });

  it("autofit_enabled reflects the persisted setting", () => {
    expect(summarizeTerminalDefaults(settings({ autofitEnabled: true })).autofit_enabled).toBe(true);
    expect(summarizeTerminalDefaults(settings({ autofitEnabled: false })).autofit_enabled).toBe(false);
  });
});

describe("describeEffectiveRenderer", () => {
  it("xterm + gate-off mentions the v1 default explicitly", () => {
    const d = summarizeTerminalDefaults(defaultTerminalSettings());
    const text = describeEffectiveRenderer(d);
    expect(text).toContain("xterm");
    expect(text).toContain("v1 production default");
  });

  it("downgraded selection surfaces the fall-back explicitly", () => {
    const d = summarizeTerminalDefaults({
      ...defaultTerminalSettings(),
      rendererId: "ghostty-web",
      experimentalRendererEvaluationEnabled: false,
    });
    const text = describeEffectiveRenderer(d);
    expect(text).toContain("ghostty-web");
    expect(text).toContain("falls back to xterm");
  });

  it("honoured experimental renderer carries the 'evaluation only' caveat", () => {
    const d = summarizeTerminalDefaults({
      ...defaultTerminalSettings(),
      rendererId: "wterm",
      experimentalRendererEvaluationEnabled: true,
    });
    const text = describeEffectiveRenderer(d);
    expect(text).toContain("wterm");
    expect(text).toContain("evaluation only");
    expect(text).toContain("not promoted into production at v1");
  });
});

describe("describeExperimentalGate", () => {
  it("warn tone when ON", () => {
    const d = summarizeTerminalDefaults({
      ...defaultTerminalSettings(),
      experimentalRendererEvaluationEnabled: true,
    });
    expect(describeExperimentalGate(d)).toEqual({
      kind: "ready",
      value: "Experimental renderer gate: ON",
      tone: "warn",
    });
  });

  it("ok tone when off (the v1 default posture)", () => {
    const d = summarizeTerminalDefaults(defaultTerminalSettings());
    expect(describeExperimentalGate(d)).toEqual({
      kind: "ready",
      value: "Experimental renderer gate: off",
      tone: "ok",
    });
  });
});

describe("describeAutofit", () => {
  it("renders ON/off honestly", () => {
    const off = summarizeTerminalDefaults(defaultTerminalSettings());
    const on = summarizeTerminalDefaults({
      ...defaultTerminalSettings(),
      autofitEnabled: true,
    });
    expect(describeAutofit(off).kind).toBe("ready");
    if (describeAutofit(off).kind === "ready") {
      expect((describeAutofit(off) as { value: string }).value).toBe(
        "Autofit: off",
      );
    }
    if (describeAutofit(on).kind === "ready") {
      expect((describeAutofit(on) as { value: string }).value).toBe(
        "Autofit: on",
      );
    }
  });
});

describe("buildSessionPolicySummary / describe* policy indicators", () => {
  const policy: SessionPolicy = {
    detached_live_pty_ttl_seconds: 30,
    max_live_pty_sessions_per_user: 8,
    max_starting_sessions_per_user: 4,
  };

  it("null policy collapses to loading", () => {
    expect(buildSessionPolicySummary(null)).toEqual({ kind: "loading" });
  });

  it("ready policy renders TTL + quotas with parameterised copy", () => {
    const summary = buildSessionPolicySummary(policy);
    expect(summary).toEqual({ kind: "ready", policy });
    const ttl = describeDetachedTtlIndicator(summary);
    expect(ttl.kind).toBe("ready");
    if (ttl.kind === "ready") {
      expect(ttl.value).toContain("about 30 seconds");
    }
    const quotas = describeQuotaIndicator(summary);
    expect(quotas.kind).toBe("ready");
    if (quotas.kind === "ready") {
      expect(quotas.value).toContain("8 live");
      expect(quotas.value).toContain("4 starting");
    }
  });

  it("indicators render loading state when policy is loading", () => {
    expect(describeDetachedTtlIndicator({ kind: "loading" })).toEqual({
      kind: "loading",
    });
    expect(describeQuotaIndicator({ kind: "loading" })).toEqual({
      kind: "loading",
    });
  });
});

describe("summarizeAccount", () => {
  it("returns null for a missing user", () => {
    expect(summarizeAccount(null)).toBeNull();
  });

  it("copies only the public-safe fields", () => {
    const summary = summarizeAccount({
      email: "ops@example.com",
      display_name: "Ops",
      created_at: "2026-04-01T00:00:00Z",
      last_login_at: "2026-04-30T12:00:00Z",
    });
    expect(summary).toEqual({
      email: "ops@example.com",
      display_name: "Ops",
      account_created_at: "2026-04-01T00:00:00Z",
      last_login_at: "2026-04-30T12:00:00Z",
    });
  });

  it("preserves a null last_login_at for a freshly-bootstrapped user", () => {
    const summary = summarizeAccount({
      email: "ops@example.com",
      display_name: "Ops",
      created_at: "2026-04-01T00:00:00Z",
      last_login_at: null,
    });
    expect(summary?.last_login_at).toBeNull();
  });

  it("redaction sentinel: a smuggled secret field cannot survive into the summary", () => {
    // Cast to inject hostile-shaped fields onto the input the helper
    // does NOT declare. Field-by-field copy is the redaction backstop.
    const hostile = {
      email: "ops@example.com",
      display_name: "Ops",
      created_at: "2026-04-01T00:00:00Z",
      last_login_at: null,
      password_hash: "$argon2id$...$",
      token_hash: "abc",
      bootstrap_token: "btk",
      private_key: "PEM",
      encrypted_private_key: "vault-bytes",
      session_token: "raw",
      client_info: "Mozilla/5.0",
    } as unknown as Parameters<typeof summarizeAccount>[0];
    const summary = summarizeAccount(hostile);
    const blob = JSON.stringify(summary);
    for (const sentinel of SENTINELS) {
      expect(blob).not.toContain(sentinel);
    }
  });
});

describe("toneClass", () => {
  it("returns a class string for every tone", () => {
    for (const tone of ["ok", "info", "warn", "bad"] as const) {
      const c = toneClass(tone);
      expect(typeof c).toBe("string");
      expect(c.length).toBeGreaterThan(0);
    }
  });

  it("ok tone uses emerald, warn uses amber, bad uses rose (design contract)", () => {
    expect(toneClass("ok")).toContain("emerald");
    expect(toneClass("warn")).toContain("amber");
    expect(toneClass("bad")).toContain("rose");
    expect(toneClass("info")).toContain("zinc");
  });
});

describe("redaction sentinel: every helper drops smuggled secret fields", () => {
  // Run every helper that takes wire data through a hostile fixture and
  // confirm no sentinel survives into the rendered output. This is the
  // load-bearing "you cannot leak a key through the operational status
  // panel" pin.
  it("smuggled fields on auth sessions never survive the summary", () => {
    const hostile = {
      id: "x",
      created_at: "2026-04-01T00:00:00Z",
      last_seen_at: "2026-04-01T00:00:00Z",
      expires_at: "2026-05-01T00:00:00Z",
      revoked_at: null,
      current: false,
      status: "active",
      password_hash: "$argon2id$...$",
      token_hash: "abc",
      bootstrap_token: "btk",
    } as unknown as AuthSession;
    const summary = summarizeAuthSessions(authOk([hostile]));
    const indicator = describeAuthSessions(summary);
    const blob = JSON.stringify({ summary, indicator });
    for (const sentinel of SENTINELS) {
      expect(blob).not.toContain(sentinel);
    }
  });

  it("smuggled fields on terminal sessions never survive", () => {
    const hostile = {
      id: "x",
      server_profile_id: "y",
      status: "active",
      cols: 80,
      rows: 24,
      created_at: "2026-04-01T00:00:00Z",
      last_seen_at: "2026-04-01T00:00:00Z",
      closed_at: null,
      private_key: "PEM",
      encrypted_private_key: "vault",
      data_b64: "AAAA",
      client_info: "Mozilla/5.0",
    } as unknown as TerminalSession;
    const summary = summarizeTerminalSessions(ok([hostile]));
    const indicator = describeTerminalSessions(summary);
    const blob = JSON.stringify({ summary, indicator });
    for (const sentinel of SENTINELS) {
      expect(blob).not.toContain(sentinel);
    }
  });

  it("smuggled fields on the session policy cannot survive (parser already redacted)", () => {
    // The parser at the api layer would have collapsed a hostile body
    // to `null`; the helper here only sees typed values. We still test
    // the negative shape — if the helper were widened to accept raw
    // wire bodies, the sentinel set would surface the regression.
    const policy: SessionPolicy = {
      detached_live_pty_ttl_seconds: 30,
      max_live_pty_sessions_per_user: 8,
      max_starting_sessions_per_user: 4,
    };
    const summary = buildSessionPolicySummary(policy);
    const blob = JSON.stringify({
      summary,
      ttl: describeDetachedTtlIndicator(summary),
      quotas: describeQuotaIndicator(summary),
    });
    for (const sentinel of SENTINELS) {
      expect(blob).not.toContain(sentinel);
    }
  });
});
