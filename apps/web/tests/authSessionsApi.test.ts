import { describe, expect, it, vi } from "vitest";
import {
  describeAuthSessionStatus,
  describeAuthSessionsError,
  listAuthSessions,
  parseAuthSession,
  revokeAllAuthSessionsExceptCurrent,
  revokeAuthSession,
  type AuthSession,
} from "../src/lib/api/auth.js";

/**
 * Sentinels that MUST NEVER appear in user-visible UI strings, parsed
 * DTOs, or formatted summaries. The redaction rule for the session-
 * management surface is the same as the inventory and audit surfaces:
 * operator-facing detail and sensitive request inputs do not reach any
 * string the SPA renders.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_SESSIONS_OPERATOR_DETAIL_9101";
const SENTINEL_TOKEN_HASH = "RELAY_SENTINEL_TOKEN_HASH_9102";
const SENTINEL_SESSION_TOKEN = "RELAY_SENTINEL_SESSION_TOKEN_9103";
const SENTINEL_PASSWORD_HASH = "RELAY_SENTINEL_PASSWORD_HASH_9104";
const SENTINEL_BOOTSTRAP_TOKEN = "RELAY_SENTINEL_BOOTSTRAP_TOKEN_9105";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_9106";
const SENTINEL_ENCRYPTED_PRIVATE_KEY =
  "RELAY_SENTINEL_ENCRYPTED_PRIVATE_KEY_9107";
const SENTINEL_ACCESS_TOKEN = "RELAY_SENTINEL_ACCESS_TOKEN_9108";
const SENTINEL_SESSION_OUTPUT = "RELAY_SENTINEL_SESSION_OUTPUT_9109";
const SENTINEL_REMOTE_ADDR = "RELAY_SENTINEL_REMOTE_ADDR_9110";
const SENTINEL_USER_AGENT = "RELAY_SENTINEL_USER_AGENT_9111";
const SENTINEL_TRANSPORT_DETAIL = "RELAY_SENTINEL_TRANSPORT_DETAIL_9112";

const SESSION_FIXTURE: AuthSession = {
  id: "11111111-1111-1111-1111-111111111111",
  created_at: "2026-04-30T00:00:00Z",
  last_seen_at: "2026-05-01T01:23:45Z",
  expires_at: "2026-05-30T00:00:00Z",
  revoked_at: null,
  current: true,
  status: "active",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function noContent(): Response {
  return new Response(null, { status: 204 });
}

interface CapturedCall {
  url: string;
  init: RequestInit;
}

function captureFetch(
  responder: (call: CapturedCall) => Response | Promise<Response>,
): { fetchImpl: typeof fetch; calls: CapturedCall[] } {
  const calls: CapturedCall[] = [];
  const fetchImpl = (async (input: string | URL | Request, init: RequestInit = {}) => {
    const url = String(input);
    const captured: CapturedCall = { url, init };
    calls.push(captured);
    return await responder(captured);
  }) as unknown as typeof fetch;
  return { fetchImpl, calls };
}

// ---------------------------------------------------------------------
// parseAuthSession
// ---------------------------------------------------------------------

describe("parseAuthSession", () => {
  it("accepts a well-formed session", () => {
    expect(parseAuthSession(SESSION_FIXTURE)).toEqual(SESSION_FIXTURE);
  });

  it("treats `revoked_at: null` as null", () => {
    expect(
      parseAuthSession({ ...SESSION_FIXTURE, revoked_at: null }),
    ).toEqual({ ...SESSION_FIXTURE, revoked_at: null });
  });

  it("accepts a revoked session row with an ISO timestamp", () => {
    const row = {
      ...SESSION_FIXTURE,
      current: false,
      status: "revoked" as const,
      revoked_at: "2026-05-01T12:00:00Z",
    };
    expect(parseAuthSession(row)).toEqual(row);
  });

  it("accepts an expired session row", () => {
    const row = {
      ...SESSION_FIXTURE,
      current: false,
      status: "expired" as const,
    };
    expect(parseAuthSession(row)).toEqual(row);
  });

  it("returns null on missing required fields", () => {
    const { id: _id, ...rest } = SESSION_FIXTURE;
    expect(parseAuthSession(rest)).toBeNull();
  });

  it("returns null on wrong-typed revoked_at", () => {
    expect(
      parseAuthSession({ ...SESSION_FIXTURE, revoked_at: 12345 }),
    ).toBeNull();
  });

  it("returns null on unknown status discriminator", () => {
    expect(
      parseAuthSession({ ...SESSION_FIXTURE, status: "totally_new" }),
    ).toBeNull();
  });

  it("returns null on wrong-typed current", () => {
    expect(
      parseAuthSession({ ...SESSION_FIXTURE, current: "true" }),
    ).toBeNull();
  });

  it("ignores unknown extra fields silently", () => {
    const parsed = parseAuthSession({
      ...SESSION_FIXTURE,
      future_safe: "ignored",
    });
    expect(parsed).toEqual(SESSION_FIXTURE);
  });

  it("drops smuggled secret-shaped fields field-by-field", () => {
    const hostile = {
      ...SESSION_FIXTURE,
      token_hash: SENTINEL_TOKEN_HASH,
      session_token: SENTINEL_SESSION_TOKEN,
      password_hash: SENTINEL_PASSWORD_HASH,
      bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
      private_key: SENTINEL_PRIVATE_KEY,
      encrypted_private_key: SENTINEL_ENCRYPTED_PRIVATE_KEY,
      access_token: SENTINEL_ACCESS_TOKEN,
      session_output: SENTINEL_SESSION_OUTPUT,
      remote_addr: SENTINEL_REMOTE_ADDR,
      user_agent: SENTINEL_USER_AGENT,
    };
    const parsed = parseAuthSession(hostile);
    expect(parsed).not.toBeNull();
    const json = JSON.stringify(parsed);
    for (const sentinel of [
      SENTINEL_TOKEN_HASH,
      SENTINEL_SESSION_TOKEN,
      SENTINEL_PASSWORD_HASH,
      SENTINEL_BOOTSTRAP_TOKEN,
      SENTINEL_PRIVATE_KEY,
      SENTINEL_ENCRYPTED_PRIVATE_KEY,
      SENTINEL_ACCESS_TOKEN,
      SENTINEL_SESSION_OUTPUT,
      SENTINEL_REMOTE_ADDR,
      SENTINEL_USER_AGENT,
    ]) {
      expect(parsed as Record<string, unknown>).not.toHaveProperty(sentinel);
      expect(json).not.toContain(sentinel);
    }
  });
});

// ---------------------------------------------------------------------
// listAuthSessions
// ---------------------------------------------------------------------

describe("listAuthSessions", () => {
  it("targets /api/v1/auth/sessions with credentials: include and GET", async () => {
    const { fetchImpl, calls } = captureFetch(() =>
      jsonResponse(200, { sessions: [SESSION_FIXTURE] }),
    );
    const result = await listAuthSessions({ fetchImpl });
    expect(result).toEqual({ ok: true, sessions: [SESSION_FIXTURE] });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/v1/auth/sessions");
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.method).toBe("GET");
  });

  it("never returns a session with a token_hash field even if smuggled", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(200, {
        sessions: [
          {
            ...SESSION_FIXTURE,
            token_hash: SENTINEL_TOKEN_HASH,
            session_token: SENTINEL_SESSION_TOKEN,
            password_hash: SENTINEL_PASSWORD_HASH,
            private_key: SENTINEL_PRIVATE_KEY,
            encrypted_private_key: SENTINEL_ENCRYPTED_PRIVATE_KEY,
            access_token: SENTINEL_ACCESS_TOKEN,
            session_output: SENTINEL_SESSION_OUTPUT,
          },
        ],
      }),
    );
    const result = await listAuthSessions({ fetchImpl });
    expect(result.ok).toBe(true);
    if (result.ok) {
      const json = JSON.stringify(result.sessions);
      for (const sentinel of [
        SENTINEL_TOKEN_HASH,
        SENTINEL_SESSION_TOKEN,
        SENTINEL_PASSWORD_HASH,
        SENTINEL_PRIVATE_KEY,
        SENTINEL_ENCRYPTED_PRIVATE_KEY,
        SENTINEL_ACCESS_TOKEN,
        SENTINEL_SESSION_OUTPUT,
      ]) {
        expect(json).not.toContain(sentinel);
      }
      expect(result.sessions[0] as Record<string, unknown>).not.toHaveProperty(
        "token_hash",
      );
      expect(result.sessions[0] as Record<string, unknown>).not.toHaveProperty(
        "session_token",
      );
    }
  });

  it("returns http error on 401 envelope without echoing operator detail", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: SENTINEL_OPERATOR },
      }),
    );
    const result = await listAuthSessions({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("http");
      const summary = describeAuthSessionsError(result.error);
      expect(summary).not.toContain(SENTINEL_OPERATOR);
    }
  });

  it("returns transport error when fetch throws", async () => {
    const fetchImpl = (async () => {
      throw new Error(`net ${SENTINEL_TRANSPORT_DETAIL}`);
    }) as unknown as typeof fetch;
    const result = await listAuthSessions({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "transport" },
    });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_TRANSPORT_DETAIL);
  });

  it("returns malformed_response when a row has unknown status", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(200, {
        sessions: [
          {
            ...SESSION_FIXTURE,
            status: "totally_new",
          },
        ],
      }),
    );
    const result = await listAuthSessions({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("returns malformed_response when the body is not JSON", async () => {
    const { fetchImpl } = captureFetch(
      () => new Response("not json", { status: 200 }),
    );
    const result = await listAuthSessions({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("returns malformed_response when the body has no sessions array", async () => {
    const { fetchImpl } = captureFetch(() => jsonResponse(200, { foo: 1 }));
    const result = await listAuthSessions({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("does not log on success or transport failure", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const okFetch = (async () =>
      jsonResponse(200, { sessions: [SESSION_FIXTURE] })) as unknown as typeof fetch;
    await listAuthSessions({ fetchImpl: okFetch });
    const failFetch = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    await listAuthSessions({ fetchImpl: failFetch });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------
// revokeAuthSession
// ---------------------------------------------------------------------

describe("revokeAuthSession", () => {
  it("POSTs to the path-encoded revoke endpoint with credentials: include", async () => {
    const { fetchImpl, calls } = captureFetch(() => noContent());
    const result = await revokeAuthSession(SESSION_FIXTURE.id, { fetchImpl });
    expect(result).toEqual({ ok: true, current: false });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(
      `/api/v1/auth/sessions/${SESSION_FIXTURE.id}/revoke`,
    );
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.method).toBe("POST");
    expect(calls[0].init.body).toBeUndefined();
  });

  it("path-encodes a pathological session id so it cannot escape the route", async () => {
    const { fetchImpl, calls } = captureFetch(() => noContent());
    // A pathological id with slashes and percent characters MUST be
    // percent-encoded into the path so the URL parser cannot
    // interpret it as a different route.
    const id = "../../etc/passwd?x=1";
    const result = await revokeAuthSession(id, { fetchImpl });
    expect(result.ok).toBe(true);
    expect(calls[0].url).toBe(
      `/api/v1/auth/sessions/${encodeURIComponent(id)}/revoke`,
    );
    expect(calls[0].url).not.toContain("../");
    expect(calls[0].url).not.toContain("?x=1");
  });

  it("returns ok with current=true when the caller declared the row current", async () => {
    const { fetchImpl } = captureFetch(() => noContent());
    const result = await revokeAuthSession(SESSION_FIXTURE.id, {
      fetchImpl,
      current: true,
    });
    expect(result).toEqual({ ok: true, current: true });
  });

  it("returns http error for a 404 envelope", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(404, {
        error: { code: "not_found", message: SENTINEL_OPERATOR },
      }),
    );
    const result = await revokeAuthSession(SESSION_FIXTURE.id, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("http");
      const summary = describeAuthSessionsError(result.error);
      expect(summary).not.toContain(SENTINEL_OPERATOR);
    }
  });

  it("returns transport error when fetch throws", async () => {
    const fetchImpl = (async () => {
      throw new Error(`net ${SENTINEL_TRANSPORT_DETAIL}`);
    }) as unknown as typeof fetch;
    const result = await revokeAuthSession(SESSION_FIXTURE.id, { fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "transport" },
    });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_TRANSPORT_DETAIL);
  });

  it("does not log on success or HTTP failure", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const okFetch = (async () => noContent()) as unknown as typeof fetch;
    await revokeAuthSession(SESSION_FIXTURE.id, { fetchImpl: okFetch });
    const failFetch = (async () =>
      jsonResponse(404, {
        error: { code: "not_found", message: SENTINEL_OPERATOR },
      })) as unknown as typeof fetch;
    await revokeAuthSession(SESSION_FIXTURE.id, { fetchImpl: failFetch });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------
// revokeAllAuthSessionsExceptCurrent
// ---------------------------------------------------------------------

describe("revokeAllAuthSessionsExceptCurrent", () => {
  it("POSTs to /api/v1/auth/sessions/revoke-all-except-current with credentials: include", async () => {
    const { fetchImpl, calls } = captureFetch(() =>
      jsonResponse(200, { revoked_count: 3 }),
    );
    const result = await revokeAllAuthSessionsExceptCurrent({ fetchImpl });
    expect(result).toEqual({ ok: true, revoked_count: 3 });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(
      "/api/v1/auth/sessions/revoke-all-except-current",
    );
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.method).toBe("POST");
    expect(calls[0].init.body).toBeUndefined();
  });

  it("accepts revoked_count: 0 (no other sessions)", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(200, { revoked_count: 0 }),
    );
    const result = await revokeAllAuthSessionsExceptCurrent({ fetchImpl });
    expect(result).toEqual({ ok: true, revoked_count: 0 });
  });

  it("returns malformed_response when revoked_count is missing or wrong-typed", async () => {
    for (const body of [{}, { revoked_count: "3" }, { revoked_count: -1 }, { revoked_count: NaN }]) {
      const { fetchImpl } = captureFetch(() => jsonResponse(200, body));
      const result = await revokeAllAuthSessionsExceptCurrent({ fetchImpl });
      expect(result).toEqual({
        ok: false,
        error: { kind: "malformed_response" },
      });
    }
  });

  it("returns http error for a 403 csrf envelope", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(403, {
        error: { code: "csrf_origin_mismatch", message: SENTINEL_OPERATOR },
      }),
    );
    const result = await revokeAllAuthSessionsExceptCurrent({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const summary = describeAuthSessionsError(result.error);
      expect(summary).not.toContain(SENTINEL_OPERATOR);
      expect(summary.toLowerCase()).toContain("blocked by browser");
    }
  });

  it("returns transport error when fetch throws", async () => {
    const fetchImpl = (async () => {
      throw new Error(`net ${SENTINEL_TRANSPORT_DETAIL}`);
    }) as unknown as typeof fetch;
    const result = await revokeAllAuthSessionsExceptCurrent({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "transport" },
    });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_TRANSPORT_DETAIL);
  });
});

// ---------------------------------------------------------------------
// describeAuthSessionsError
// ---------------------------------------------------------------------

describe("describeAuthSessionsError", () => {
  it("formats 401 as a session-ended line without echoing operator detail", () => {
    const summary = describeAuthSessionsError({
      kind: "http",
      status: 401,
      code: "unauthorized",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("Your session has ended. Please sign in again.");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats 403 as a browser-policy line", () => {
    const summary = describeAuthSessionsError({
      kind: "http",
      status: 403,
      code: "csrf_origin_mismatch",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toContain("blocked by browser security policy");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats 404 as 'no longer available'", () => {
    const summary = describeAuthSessionsError({
      kind: "http",
      status: 404,
      code: "not_found",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("That session is no longer available.");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats other HTTP errors as a function of status only", () => {
    const summary = describeAuthSessionsError({
      kind: "http",
      status: 500,
      code: "internal_error",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("Cannot manage sessions: HTTP 500");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary).not.toContain("internal_error");
  });

  it("collapses transport detail to a static line", () => {
    expect(describeAuthSessionsError({ kind: "transport" })).toBe(
      "Cannot reach the backend.",
    );
  });

  it("collapses malformed_response to a static line", () => {
    expect(describeAuthSessionsError({ kind: "malformed_response" })).toBe(
      "Cannot manage sessions: malformed response.",
    );
  });
});

// ---------------------------------------------------------------------
// describeAuthSessionStatus
// ---------------------------------------------------------------------

describe("describeAuthSessionStatus", () => {
  it("renders each closed-enum value", () => {
    expect(describeAuthSessionStatus("active")).toBe("Active");
    expect(describeAuthSessionStatus("expired")).toBe("Expired");
    expect(describeAuthSessionStatus("revoked")).toBe("Revoked");
  });
});
