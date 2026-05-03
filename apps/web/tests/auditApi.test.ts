import { describe, expect, it } from "vitest";
import {
  describeAuditEventKind,
  listRecentAuditEvents,
  parseAuditEvent,
  summarizeAuditEvent,
  type AuditEvent,
} from "../src/lib/api/auditEvents.js";

/**
 * Sentinels that MUST NEVER survive parsing or surface in any UI string
 * derived from an audit event. The redaction rule mirrors the backend
 * `AUDIT_FORBIDDEN_SUBSTRINGS` list and the API integration test
 * `audit_events_recent_redacts_secret_shaped_payload_fields` — the
 * client-side parser is the second-line defence.
 */
const FORBIDDEN = [
  "encrypted_private_key",
  "private_key",
  "BEGIN OPENSSH PRIVATE KEY",
  "client_info",
  "remote_addr",
  "user_agent",
] as const;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const LIFECYCLE_FIXTURE: AuditEvent = {
  id: "11111111-1111-1111-1111-111111111111",
  kind: "server_profile_created",
  recorded_at: "2026-05-01T12:34:56Z",
  summary: {
    kind: "server_profile_lifecycle",
    server_profile_id: "22222222-2222-2222-2222-222222222222",
    name: "edge-1 prod",
    host_id: "33333333-3333-3333-3333-333333333333",
    ssh_identity_id: "44444444-4444-4444-4444-444444444444",
    disabled_at: null,
  },
};

describe("parseAuditEvent", () => {
  it("accepts a well-formed lifecycle event", () => {
    const parsed = parseAuditEvent(LIFECYCLE_FIXTURE);
    expect(parsed).toEqual(LIFECYCLE_FIXTURE);
  });

  it("accepts a generic summary", () => {
    const parsed = parseAuditEvent({
      id: "x",
      kind: "session_opened",
      recorded_at: "2026-05-01T00:00:00Z",
      summary: { kind: "generic" },
    });
    expect(parsed).not.toBeNull();
    expect(parsed?.summary).toEqual({ kind: "generic" });
  });

  it("rejects malformed top-level shape", () => {
    expect(parseAuditEvent(null)).toBeNull();
    expect(parseAuditEvent("nope")).toBeNull();
    expect(parseAuditEvent({})).toBeNull();
    expect(
      parseAuditEvent({ id: 42, kind: "x", recorded_at: "x", summary: { kind: "generic" } }),
    ).toBeNull();
    expect(
      parseAuditEvent({ id: "x", kind: 42, recorded_at: "x", summary: { kind: "generic" } }),
    ).toBeNull();
    expect(
      parseAuditEvent({ id: "x", kind: "x", recorded_at: 42, summary: { kind: "generic" } }),
    ).toBeNull();
  });

  it("falls back to generic for an unknown summary variant", () => {
    // Forward compatibility: a backend that ships a new sanitizer arm
    // before the frontend updates MUST NOT collapse the whole feed to
    // `malformed_response`. The client treats the row as a generic
    // audit event and drops the unknown payload data.
    const raw = {
      id: "x",
      kind: "x",
      recorded_at: "2026-05-01T00:00:00Z",
      summary: {
        kind: "future_unknown_variant",
        private_key: "leak",
        encrypted_private_key: "leak",
      },
    };
    const parsed = parseAuditEvent(raw);
    expect(parsed).not.toBeNull();
    expect(parsed?.summary).toEqual({ kind: "generic" });
    const serialized = JSON.stringify(parsed);
    for (const f of FORBIDDEN) {
      expect(serialized).not.toContain(f);
    }
  });

  it("rejects a missing summary", () => {
    expect(
      parseAuditEvent({
        id: "x",
        kind: "server_profile_created",
        recorded_at: "2026-05-01T00:00:00Z",
      }),
    ).toBeNull();
  });

  it("rejects a lifecycle summary with non-string fields", () => {
    const raw = {
      id: "x",
      kind: "server_profile_created",
      recorded_at: "2026-05-01T00:00:00Z",
      summary: {
        kind: "server_profile_lifecycle",
        server_profile_id: 42,
        name: "x",
        host_id: null,
        ssh_identity_id: null,
        disabled_at: null,
      },
    };
    expect(parseAuditEvent(raw)).toBeNull();
  });

  it("drops smuggled secret-shaped payload fields", () => {
    // The wire row crafts a payload with every forbidden name; none of
    // them are part of the parsed DTO contract, so they MUST disappear.
    const raw = {
      id: "x",
      kind: "server_profile_created",
      recorded_at: "2026-05-01T00:00:00Z",
      summary: {
        kind: "server_profile_lifecycle",
        server_profile_id: "p",
        name: "p",
        host_id: "h",
        ssh_identity_id: "i",
        disabled_at: null,
      },
      // Top-level smuggling — must not survive.
      private_key: "PEM bytes",
      encrypted_private_key: "BEGIN OPENSSH PRIVATE KEY...",
      client_info: "Mozilla/5.0",
      remote_addr: "203.0.113.7",
      user_agent: "tauri/2",
    };
    const parsed = parseAuditEvent(raw);
    expect(parsed).not.toBeNull();
    const serialized = JSON.stringify(parsed);
    for (const f of FORBIDDEN) {
      expect(serialized).not.toContain(f);
    }
  });

  it("ignores extra fields inside a lifecycle summary", () => {
    // Even within the `server_profile_lifecycle` variant, only the
    // allow-listed keys make it onto the DTO. Forbidden names smuggled
    // alongside are dropped at construction.
    const raw = {
      id: "x",
      kind: "server_profile_created",
      recorded_at: "2026-05-01T00:00:00Z",
      summary: {
        kind: "server_profile_lifecycle",
        server_profile_id: "p",
        name: "ok",
        host_id: "h",
        ssh_identity_id: "i",
        disabled_at: null,
        private_key: "leak",
        encrypted_private_key: "leak",
        client_info: "leak",
      },
    };
    const parsed = parseAuditEvent(raw);
    const serialized = JSON.stringify(parsed);
    for (const f of FORBIDDEN) {
      expect(serialized).not.toContain(f);
    }
  });
});

describe("describeAuditEventKind", () => {
  it("labels server-profile lifecycle kinds", () => {
    expect(describeAuditEventKind("server_profile_created")).toBe(
      "Server profile created",
    );
    expect(describeAuditEventKind("server_profile_disabled")).toBe(
      "Server profile disabled",
    );
    expect(describeAuditEventKind("server_profile_enabled")).toBe(
      "Server profile enabled",
    );
  });

  it("labels current-user auth lifecycle kinds", () => {
    expect(describeAuditEventKind("first_user_created")).toBe(
      "First user created",
    );
    expect(describeAuditEventKind("session_revoked")).toBe(
      "Browser session revoked",
    );
    expect(describeAuditEventKind("sessions_revoked")).toBe(
      "Other browser sessions revoked",
    );
  });

  it("labels recording_purged with a meaningful, payload-free string", () => {
    // The label must read as a complete UI line for any future admin /
    // "system actions affecting your data" surface, but it MUST NOT
    // echo any payload field — no session id, no byte count, no
    // retention days, no closed_at / purged_at timestamps.
    const label = describeAuditEventKind("recording_purged");
    expect(label).toBe("Terminal recording purged by retention");
    for (const forbidden of [
      "private_key",
      "encrypted_private_key",
      "client_info",
      "data_b64",
      "payload",
      "byte_len",
      "bytes_purged",
      "retention_days",
      "closed_at",
      "purged_at",
      "target_id",
    ]) {
      expect(label).not.toContain(forbidden);
    }
  });

  it("falls through to a generic label for unknown kinds", () => {
    expect(describeAuditEventKind("future_kind")).toBe("Audit event");
    expect(describeAuditEventKind("other")).toBe("Audit event");
  });
});

describe("summarizeAuditEvent", () => {
  it("includes the profile name when present in a lifecycle summary", () => {
    expect(summarizeAuditEvent(LIFECYCLE_FIXTURE)).toBe(
      "Server profile created: edge-1 prod",
    );
  });

  it("collapses to the kind label when the lifecycle name is missing", () => {
    const event: AuditEvent = {
      ...LIFECYCLE_FIXTURE,
      summary: {
        kind: "server_profile_lifecycle",
        server_profile_id: "p",
        name: null,
        host_id: null,
        ssh_identity_id: null,
        disabled_at: null,
      },
    };
    expect(summarizeAuditEvent(event)).toBe("Server profile created");
  });

  it("renders generic summaries with the kind label only", () => {
    const event: AuditEvent = {
      id: "x",
      kind: "session_opened",
      recorded_at: "2026-05-01T00:00:00Z",
      summary: { kind: "generic" },
    };
    expect(summarizeAuditEvent(event)).toBe("Terminal session opened");
  });
});

describe("listRecentAuditEvents", () => {
  it("returns parsed events on a 2xx response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [LIFECYCLE_FIXTURE])) as unknown as typeof fetch;
    const result = await listRecentAuditEvents({ fetchImpl });
    expect(result).toEqual({ ok: true, data: [LIFECYCLE_FIXTURE] });
  });

  it("forwards the limit query parameter when supplied", async () => {
    let observedUrl = "";
    const fetchImpl = (async (input: RequestInfo | URL) => {
      observedUrl = typeof input === "string" ? input : input.toString();
      return jsonResponse(200, []);
    }) as unknown as typeof fetch;
    await listRecentAuditEvents({ fetchImpl, limit: 7 });
    expect(observedUrl).toBe("/api/v1/audit-events/recent?limit=7");
  });

  it("collapses any unparseable item to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [
        LIFECYCLE_FIXTURE,
        { id: "broken" /* missing fields */ },
      ])) as unknown as typeof fetch;
    const result = await listRecentAuditEvents({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({ kind: "malformed_response" });
    }
  });

  it("maps a 401 response to a typed http error", async () => {
    const fetchImpl = (async () =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: "unauthorized" },
      })) as unknown as typeof fetch;
    const result = await listRecentAuditEvents({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(401);
      expect(result.error.code).toBe("unauthorized");
    } else {
      expect.fail("expected http error");
    }
  });
});
