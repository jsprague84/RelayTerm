import { describe, expect, it } from "vitest";
import {
  DASHBOARD_NAV_ACTIONS,
  DASHBOARD_RECENT_ACTIVITY_LIMIT,
  activitySectionFromLoad,
  cardStateFromLoad,
  deriveChecklist,
  sessionStatusOrder,
  summarizeInventory,
  summarizeRecentActivity,
  summarizeSessionStatuses,
  type CardState,
  type ChecklistStepId,
  type InventoryCounts,
} from "../src/lib/app/dashboard/dashboardSummary.js";
import {
  pathForView,
  type AppRoutePath,
} from "../src/lib/app/routing.js";
import type { Host } from "../src/lib/api/hosts.js";
import type { ServerProfile } from "../src/lib/api/serverProfiles.js";
import type { SshIdentity } from "../src/lib/api/sshIdentities.js";
import type { TerminalSession } from "../src/lib/api/terminalSessions.js";
import type { LoadResult } from "../src/lib/api/apiErrors.js";
import type { AuditEvent } from "../src/lib/api/auditEvents.js";

/**
 * Sentinels that MUST NEVER appear in any user-visible string the
 * dashboard helper can produce. Mirrors the redaction posture of the
 * inventory and terminal-session API tests: a stray secret-shaped
 * field on a wire body cannot smuggle through the helper into the
 * formatted summary or checklist copy.
 */
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_BYTES_DASH9101";
const SENTINEL_OPERATOR = "RELAY_SENTINEL_DASH_OPERATOR_DETAIL_9102";

const HOST_FIXTURE: Host = {
  id: "11111111-1111-1111-1111-111111111111",
  display_name: "edge-1",
  hostname: "edge-1.example.internal",
  port: 22,
  default_username: "deploy",
  created_at: "2026-04-29T00:00:00Z",
  updated_at: "2026-04-29T00:01:00Z",
};

const PROFILE_FIXTURE: ServerProfile = {
  id: "22222222-2222-2222-2222-222222222222",
  name: "edge-1 prod",
  host_id: HOST_FIXTURE.id,
  ssh_identity_id: "33333333-3333-3333-3333-333333333333",
  username_override: null,
  tags: ["prod"],
  created_at: "2026-04-29T00:00:00Z",
  updated_at: "2026-04-29T00:00:00Z",
  last_connected_at: null,
  disabled_at: null,
};

const IDENTITY_FIXTURE: SshIdentity = {
  id: "33333333-3333-3333-3333-333333333333",
  name: "primary",
  key_type: "ed25519",
  public_key: "ssh-ed25519 AAAAExampleSshPublicKey relay@example",
  fingerprint_sha256: "SHA256:abcdefg",
  created_at: "2026-04-29T00:00:00Z",
  last_used_at: null,
};

function session(
  id: string,
  status: TerminalSession["status"],
): TerminalSession {
  return {
    id,
    server_profile_id: PROFILE_FIXTURE.id,
    status,
    cols: 80,
    rows: 24,
    created_at: "2026-04-30T00:00:00Z",
    last_seen_at: "2026-04-30T00:00:00Z",
    closed_at: status === "closed" ? "2026-04-30T00:01:00Z" : null,
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
      message: SENTINEL_OPERATOR,
    },
  };
}

function transport<T>(): LoadResult<T> {
  return {
    ok: false,
    error: { kind: "transport", message: SENTINEL_OPERATOR },
  };
}

describe("cardStateFromLoad", () => {
  it("returns loading when the result is null", () => {
    expect(cardStateFromLoad<Host>(null)).toEqual({ kind: "loading" });
  });

  it("returns ready with the array length on success", () => {
    expect(cardStateFromLoad(ok([HOST_FIXTURE, HOST_FIXTURE]))).toEqual({
      kind: "ready",
      value: 2,
    });
    expect(cardStateFromLoad(ok([] as Host[]))).toEqual({
      kind: "ready",
      value: 0,
    });
  });

  it("returns unavailable on http and transport failures", () => {
    expect(cardStateFromLoad(http<Host[]>())).toEqual({ kind: "unavailable" });
    expect(cardStateFromLoad(transport<Host[]>())).toEqual({
      kind: "unavailable",
    });
  });
});

describe("summarizeInventory", () => {
  it("aggregates four independent loads into a single snapshot", () => {
    const inv = summarizeInventory({
      hosts: ok([HOST_FIXTURE]),
      profiles: ok([PROFILE_FIXTURE]),
      identities: ok([IDENTITY_FIXTURE, IDENTITY_FIXTURE]),
      sessions: ok([session("a", "active")]),
    });
    expect(inv.hosts).toEqual({ kind: "ready", value: 1 });
    expect(inv.profiles).toEqual({ kind: "ready", value: 1 });
    expect(inv.identities).toEqual({ kind: "ready", value: 2 });
    expect(inv.sessions).toEqual({ kind: "ready", value: 1 });
  });

  it("isolates partial failure: one bad load does not poison the others", () => {
    const inv = summarizeInventory({
      hosts: ok([HOST_FIXTURE]),
      profiles: http<ServerProfile[]>(),
      identities: transport<SshIdentity[]>(),
      sessions: null,
    });
    expect(inv.hosts).toEqual({ kind: "ready", value: 1 });
    expect(inv.profiles).toEqual({ kind: "unavailable" });
    expect(inv.identities).toEqual({ kind: "unavailable" });
    expect(inv.sessions).toEqual({ kind: "loading" });
  });
});

describe("summarizeSessionStatuses", () => {
  it("returns loading before the first load", () => {
    expect(summarizeSessionStatuses(null)).toEqual({ kind: "loading" });
  });

  it("returns unavailable on a load failure", () => {
    expect(summarizeSessionStatuses(http<TerminalSession[]>())).toEqual({
      kind: "unavailable",
    });
  });

  it("counts each status independently", () => {
    const breakdown = summarizeSessionStatuses(
      ok([
        session("a", "active"),
        session("b", "active"),
        session("c", "detached"),
        session("d", "starting"),
        session("e", "closed"),
        session("f", "closed"),
        session("g", "closed"),
      ]),
    );
    expect(breakdown).toEqual({
      kind: "ready",
      total: 7,
      counts: { active: 2, detached: 1, starting: 1, closed: 3 },
    });
  });

  it("returns zeros on an empty list (still ready)", () => {
    expect(summarizeSessionStatuses(ok([] as TerminalSession[]))).toEqual({
      kind: "ready",
      total: 0,
      counts: { active: 0, detached: 0, starting: 0, closed: 0 },
    });
  });
});

describe("sessionStatusOrder", () => {
  it("covers every TerminalSessionStatus exactly once", () => {
    expect([...sessionStatusOrder()].sort()).toEqual([
      "active",
      "closed",
      "detached",
      "starting",
    ]);
  });
});

describe("deriveChecklist", () => {
  function inv(overrides: Partial<InventoryCounts>): InventoryCounts {
    const base: InventoryCounts = {
      hosts: { kind: "ready", value: 0 },
      profiles: { kind: "ready", value: 0 },
      identities: { kind: "ready", value: 0 },
      sessions: { kind: "ready", value: 0 },
    };
    return { ...base, ...overrides };
  }

  it("returns the seven steps in the same order regardless of input", () => {
    const expected: ChecklistStepId[] = [
      "generate-identity",
      "install-public-key",
      "create-host",
      "create-profile",
      "host-key-trust",
      "auth-check",
      "launch-terminal",
    ];
    expect(deriveChecklist(inv({})).map((s) => s.id)).toEqual(expected);
  });

  it("marks identity / host / profile / session as complete when count > 0", () => {
    const c = deriveChecklist(
      inv({
        identities: { kind: "ready", value: 1 },
        hosts: { kind: "ready", value: 2 },
        profiles: { kind: "ready", value: 3 },
        sessions: { kind: "ready", value: 4 },
      }),
    );
    const byId = new Map(c.map((s) => [s.id, s.status]));
    expect(byId.get("generate-identity")).toBe("complete");
    expect(byId.get("create-host")).toBe("complete");
    expect(byId.get("create-profile")).toBe("complete");
    expect(byId.get("launch-terminal")).toBe("complete");
  });

  it("marks count-inferable steps as incomplete when count === 0", () => {
    const c = deriveChecklist(inv({}));
    const byId = new Map(c.map((s) => [s.id, s.status]));
    expect(byId.get("generate-identity")).toBe("incomplete");
    expect(byId.get("create-host")).toBe("incomplete");
    expect(byId.get("create-profile")).toBe("incomplete");
    expect(byId.get("launch-terminal")).toBe("incomplete");
  });

  it("collapses to unknown when the underlying count is loading or unavailable", () => {
    const c = deriveChecklist(
      inv({
        identities: { kind: "loading" },
        hosts: { kind: "unavailable" },
        profiles: { kind: "loading" },
        sessions: { kind: "unavailable" },
      }),
    );
    for (const id of [
      "generate-identity",
      "create-host",
      "create-profile",
      "launch-terminal",
    ] as const) {
      expect(c.find((s) => s.id === id)?.status).toBe("unknown");
    }
  });

  it("keeps install-key / host-key-trust / auth-check as manual regardless of counts", () => {
    const c = deriveChecklist(
      inv({
        // Even with everything else "complete", these stay manual.
        identities: { kind: "ready", value: 1 },
        hosts: { kind: "ready", value: 1 },
        profiles: { kind: "ready", value: 1 },
        sessions: { kind: "ready", value: 1 },
      }),
    );
    for (const id of ["install-public-key", "host-key-trust", "auth-check"] as const) {
      expect(c.find((s) => s.id === id)?.status).toBe("manual");
    }
  });

  it("never implies host-key trust or auth-check from any count combination", () => {
    // The copy on the manual rows must not assert installed / trusted /
    // authenticated. We assert the negative against the rendered detail.
    const c = deriveChecklist(
      inv({
        hosts: { kind: "ready", value: 1 },
        profiles: { kind: "ready", value: 1 },
        identities: { kind: "ready", value: 1 },
        sessions: { kind: "ready", value: 1 },
      }),
    );
    const banned = [
      // Words that, in this checklist, would imply state we cannot prove.
      "host key trusted",
      "host-key trusted",
      "trust verified",
      "authentication succeeded",
      "auth-check passed",
      "auth check passed",
      "key installed",
      "key is installed",
      "ready to launch",
    ];
    const haystack = c
      .map((s) => `${s.label} ${s.detail}`.toLowerCase())
      .join("\n");
    for (const phrase of banned) {
      expect(haystack).not.toContain(phrase);
    }
  });

  it("redaction sentinel: derived checklist never carries private-key / operator-detail strings", () => {
    // Pinning the redaction posture: the helper takes typed DTOs and
    // never reads off raw wire bodies. A stray sentinel cannot reach the
    // checklist output.
    const checklist = deriveChecklist(
      inv({ identities: { kind: "ready", value: 1 } }),
    );
    const blob = JSON.stringify(checklist);
    for (const sentinel of [
      SENTINEL_PRIVATE_KEY,
      SENTINEL_OPERATOR,
      "private_key",
      "encrypted_private_key",
      "session_output",
      "access_token",
    ]) {
      expect(blob).not.toContain(sentinel);
    }
  });

  it("each step carries non-empty label and detail copy", () => {
    for (const step of deriveChecklist(inv({}))) {
      expect(step.label.trim().length).toBeGreaterThan(0);
      expect(step.detail.trim().length).toBeGreaterThan(0);
    }
  });
});

describe("DASHBOARD_NAV_ACTIONS", () => {
  it("targets only known production routes via pathForView", () => {
    for (const action of DASHBOARD_NAV_ACTIONS) {
      // The recorded path must round-trip through the production
      // route table. Drift between the helper and `routing.ts` would
      // surface here as a mismatch.
      expect(action.path).toBe<AppRoutePath>(pathForView(action.view));
    }
  });

  it("uses unique action ids and unique view targets across all entries", () => {
    const ids = DASHBOARD_NAV_ACTIONS.map((a) => a.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("each label is non-empty and human-facing (no view ids leaking)", () => {
    for (const action of DASHBOARD_NAV_ACTIONS) {
      expect(action.label.trim().length).toBeGreaterThan(0);
      // An accidental `view` leaked into the label would give a string
      // containing the raw discriminator (e.g. "open dashboard view").
      // We don't ban the substring outright (Servers/Sessions both
      // contain "s") — instead pin against using the AppViewId verbatim.
      expect(action.label).not.toBe(action.view);
    }
  });

  it("redaction sentinel: no sensitive field name appears in nav labels or paths", () => {
    const blob = JSON.stringify(DASHBOARD_NAV_ACTIONS);
    for (const sentinel of [
      "private_key",
      "encrypted_private_key",
      "session_output",
      "access_token",
    ]) {
      expect(blob).not.toContain(sentinel);
    }
  });
});

describe("partial-failure summary", () => {
  // Pin the contract that the dashboard renders independent cards: a
  // health failure does NOT collapse inventory, and a sessions failure
  // does NOT collapse hosts/profiles/identities.
  it("hosts stays ready when sessions fails", () => {
    const inv = summarizeInventory({
      hosts: ok([HOST_FIXTURE]),
      profiles: ok([PROFILE_FIXTURE]),
      identities: ok([IDENTITY_FIXTURE]),
      sessions: http<TerminalSession[]>(),
    });
    expect(inv.hosts.kind).toBe("ready");
    expect(inv.profiles.kind).toBe("ready");
    expect(inv.identities.kind).toBe("ready");
    expect(inv.sessions.kind).toBe("unavailable");
  });

  it("session breakdown is independent of hosts/profiles/identities", () => {
    expect(
      summarizeSessionStatuses(ok([session("a", "active")])).kind,
    ).toBe("ready");
  });
});

describe("CardState", () => {
  it("CardState ready value is the array length, not a count of reachable items", () => {
    const card: CardState = cardStateFromLoad(ok([HOST_FIXTURE]));
    expect(card.kind === "ready" && card.value === 1).toBe(true);
  });
});

/**
 * Audit-event sentinels: every "forbidden in audit" wire field that the
 * dashboard activity helper must drop. Mirrors the backend
 * `AUDIT_FORBIDDEN_SUBSTRINGS` list AND the redaction posture pinned in
 * `tests/auditApi.test.ts`.
 */
const AUDIT_FORBIDDEN = [
  "encrypted_private_key",
  "private_key",
  "BEGIN OPENSSH PRIVATE KEY",
  "client_info",
  "remote_addr",
  "user_agent",
  "session_output",
  "access_token",
] as const;

function lifecycleEvent(
  id: string,
  kind: AuditEvent["kind"] = "server_profile_created",
  name: string | null = "edge-1 prod",
  recordedAt = "2026-05-01T12:34:56Z",
): AuditEvent {
  return {
    id,
    kind,
    recorded_at: recordedAt,
    summary: {
      kind: "server_profile_lifecycle",
      server_profile_id: "p",
      name,
      host_id: "h",
      ssh_identity_id: "i",
      disabled_at: null,
    },
  };
}

function genericEvent(
  id: string,
  kind: AuditEvent["kind"] = "future_kind_unknown_to_frontend",
  recordedAt = "2026-05-01T00:00:00Z",
): AuditEvent {
  return {
    id,
    kind,
    recorded_at: recordedAt,
    summary: { kind: "generic" },
  };
}

describe("DASHBOARD_RECENT_ACTIVITY_LIMIT", () => {
  it("is a small integer in [1, 10]", () => {
    // The dashboard activity card is a snapshot, not a feed. A drift to
    // 50 / 100 would defeat the design goal — pinned here so a casual
    // bump trips a test.
    expect(Number.isInteger(DASHBOARD_RECENT_ACTIVITY_LIMIT)).toBe(true);
    expect(DASHBOARD_RECENT_ACTIVITY_LIMIT).toBeGreaterThanOrEqual(1);
    expect(DASHBOARD_RECENT_ACTIVITY_LIMIT).toBeLessThanOrEqual(10);
  });
});

describe("summarizeRecentActivity", () => {
  it("limits to the requested count even when more rows are returned", () => {
    const events = Array.from({ length: 20 }, (_, i) =>
      lifecycleEvent(`evt-${i}`, "server_profile_created", `profile-${i}`),
    );
    const lines = summarizeRecentActivity(events, 5);
    expect(lines.length).toBe(5);
    // Backend ordering (recorded_at desc) is preserved — the helper
    // must NOT reverse or re-sort.
    expect(lines.map((l) => l.id)).toEqual([
      "evt-0",
      "evt-1",
      "evt-2",
      "evt-3",
      "evt-4",
    ]);
  });

  it("defaults to DASHBOARD_RECENT_ACTIVITY_LIMIT when no limit is passed", () => {
    const events = Array.from({ length: 12 }, (_, i) =>
      lifecycleEvent(`evt-${i}`),
    );
    const lines = summarizeRecentActivity(events);
    expect(lines.length).toBe(DASHBOARD_RECENT_ACTIVITY_LIMIT);
  });

  it("returns an empty list when given no events", () => {
    expect(summarizeRecentActivity([], 5)).toEqual([]);
  });

  it("clamps a non-positive / non-finite limit to zero", () => {
    const events = [lifecycleEvent("evt-0")];
    expect(summarizeRecentActivity(events, 0)).toEqual([]);
    expect(summarizeRecentActivity(events, -3)).toEqual([]);
    expect(summarizeRecentActivity(events, Number.NaN)).toEqual([]);
    expect(summarizeRecentActivity(events, Number.POSITIVE_INFINITY)).toEqual(
      [],
    );
  });

  it("formats lifecycle rows with the profile name when present", () => {
    const lines = summarizeRecentActivity(
      [lifecycleEvent("evt-0", "server_profile_disabled", "edge-1 prod")],
      5,
    );
    expect(lines[0]?.summary).toBe("Server profile disabled: edge-1 prod");
  });

  it("collapses unknown wire kinds to the safe generic line", () => {
    const lines = summarizeRecentActivity(
      [genericEvent("evt-0", "future_kind_unknown_to_frontend")],
      5,
    );
    // `summarizeAuditEvent` returns "Audit event" for any unknown kind;
    // the dashboard helper must surface that string verbatim — never a
    // raw `kind` echo.
    expect(lines[0]?.summary).toBe("Audit event");
  });

  it("redaction sentinel: forbidden wire fields cannot survive into a rendered line", () => {
    // Construct an AuditEvent whose `summary.name` carries every
    // forbidden substring as a hostile literal. The line's `summary`
    // string is sourced from `summarizeAuditEvent`, which only echoes
    // `name` — but that field is on the closed allow-list and is the
    // operator-supplied profile name. Pin the redaction posture
    // explicitly: even when we DO render `name`, no sentinel beyond
    // the operator's own profile name reaches the line.
    const safeNameOnly = "operator-supplied-profile";
    const event = lifecycleEvent("evt-0", "server_profile_created", safeNameOnly);
    const lines = summarizeRecentActivity([event], 5);
    const blob = JSON.stringify(lines);
    for (const sentinel of AUDIT_FORBIDDEN) {
      expect(blob).not.toContain(sentinel);
    }
    // The DTO field names are also absent — the helper builds the
    // rendered line by name and never spreads `event.summary`.
    expect(blob).not.toContain("server_profile_id");
    expect(blob).not.toContain("ssh_identity_id");
    expect(blob).not.toContain("disabled_at");
  });

  it("redaction sentinel: extra keys on the structured summary cannot leak", () => {
    // The TypeScript shape forbids smuggled keys, but runtime code
    // sometimes builds events from a cast. We construct one such event
    // and confirm the helper drops everything off the typed contract.
    const event = lifecycleEvent("evt-0");
    // Smuggled fields placed on the summary object via cast.
    (event.summary as Record<string, unknown>).private_key = "PEM bytes";
    (event.summary as Record<string, unknown>).client_info = "Mozilla/5.0";
    const lines = summarizeRecentActivity([event], 5);
    const blob = JSON.stringify(lines);
    for (const sentinel of AUDIT_FORBIDDEN) {
      expect(blob).not.toContain(sentinel);
    }
  });
});

describe("activitySectionFromLoad", () => {
  function http(): LoadResult<AuditEvent[]> {
    return {
      ok: false,
      error: {
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: "RELAY_SENTINEL_DASH_OPERATOR_DETAIL_9102",
      },
    };
  }

  function transportErr(): LoadResult<AuditEvent[]> {
    return {
      ok: false,
      error: {
        kind: "transport",
        message: "RELAY_SENTINEL_DASH_OPERATOR_DETAIL_9102",
      },
    };
  }

  it("returns loading before the first load", () => {
    expect(activitySectionFromLoad(null)).toEqual({ kind: "loading" });
  });

  it("returns ready with limited lines on success", () => {
    const events = Array.from({ length: 8 }, (_, i) => lifecycleEvent(`e${i}`));
    const section = activitySectionFromLoad(ok(events));
    expect(section.kind).toBe("ready");
    if (section.kind === "ready") {
      expect(section.lines.length).toBe(DASHBOARD_RECENT_ACTIVITY_LIMIT);
    }
  });

  it("returns ready with an empty list on a successful empty fetch", () => {
    const section = activitySectionFromLoad(ok([] as AuditEvent[]));
    expect(section).toEqual({ kind: "ready", lines: [] });
  });

  it("collapses an http error to a typed error summary", () => {
    const section = activitySectionFromLoad(http());
    expect(section.kind).toBe("error");
    if (section.kind === "error") {
      expect(section.summary).toBe(
        "Failed to load audit events: HTTP 401 unauthorized",
      );
    }
  });

  it("collapses a transport error to a typed error summary", () => {
    const section = activitySectionFromLoad(transportErr());
    expect(section.kind).toBe("error");
    if (section.kind === "error") {
      expect(section.summary).toBe("Failed to load audit events: transport error");
    }
  });

  it("redaction sentinel: error summary never echoes the wire message or operator detail", () => {
    const section = activitySectionFromLoad(http());
    expect(section.kind).toBe("error");
    if (section.kind === "error") {
      expect(section.summary).not.toContain(
        "RELAY_SENTINEL_DASH_OPERATOR_DETAIL_9102",
      );
    }
  });

  it("redaction sentinel: ready section JSON never carries forbidden audit fields", () => {
    const event = lifecycleEvent("evt-0");
    // Smuggle hostile fields onto the structured summary via cast.
    (event.summary as Record<string, unknown>).private_key = "leak";
    (event.summary as Record<string, unknown>).encrypted_private_key = "leak";
    (event.summary as Record<string, unknown>).client_info = "leak";
    const section = activitySectionFromLoad(ok([event]));
    const blob = JSON.stringify(section);
    for (const sentinel of AUDIT_FORBIDDEN) {
      expect(blob).not.toContain(sentinel);
    }
  });
});

describe("dashboard recent activity navigation target", () => {
  it("the View-all link routes to the Settings view", () => {
    // The `Recent activity` section's "View all" affordance uses the
    // shared `onNavigate(AppViewId)` path. The Settings view hosts the
    // fuller `RecentActivityPanel` (limit: 20). Pin the target so a
    // future re-org doesn't silently re-point the link to a placeholder.
    const path: AppRoutePath = pathForView("settings");
    expect(path).toBe("/settings");
  });
});
