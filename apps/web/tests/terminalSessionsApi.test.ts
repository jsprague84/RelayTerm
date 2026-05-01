import { describe, expect, it } from "vitest";
import {
  CELL_GRID_MAX,
  CELL_GRID_MIN,
  closeTerminalSession,
  describeCloseSessionError,
  describeSessionLoadError,
  type CreateTerminalSessionResponse,
  createTerminalSession,
  describeCreateError,
  listTerminalSessions,
  parseTerminalSession,
  type TerminalSession,
  validateCreateRequest,
} from "../src/lib/api/terminalSessions.js";

/**
 * Sentinel that should NEVER appear in any user-visible error summary.
 * The redaction rule (SPEC §"Live SSH PTY bridge contract → Diagnostic
 * UI") forbids the dev launcher from surfacing operator-facing detail or
 * raw request/response bodies through the status channel; this canary
 * pins the rule against any future regression.
 */
const SENTINEL = "RELAY_SENTINEL_API_OPERATOR_DETAIL_5510";

const VALID_BODY: CreateTerminalSessionResponse = {
  id: "11111111-1111-1111-1111-111111111111",
  server_profile_id: "22222222-2222-2222-2222-222222222222",
  status: "active",
  cols: 80,
  rows: 24,
  created_at: "2026-04-29T00:00:00Z",
  last_seen_at: "2026-04-29T00:00:01Z",
  closed_at: null,
  message: "ssh pty started; replay across reconnects is not yet implemented",
  pty_live: true,
};

describe("validateCreateRequest", () => {
  it("accepts a request with default cols/rows when omitted", () => {
    const result = validateCreateRequest({ server_profile_id: "abc" });
    expect(result).toEqual({
      ok: true,
      body: { server_profile_id: "abc", cols: 80, rows: 24 },
    });
  });

  it("trims server_profile_id", () => {
    const result = validateCreateRequest({ server_profile_id: "  abc  " });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.body.server_profile_id).toBe("abc");
  });

  it("rejects empty / whitespace-only server_profile_id", () => {
    expect(validateCreateRequest({ server_profile_id: "" })).toEqual({
      ok: false,
      reason: "missing_server_profile_id",
    });
    expect(validateCreateRequest({ server_profile_id: "   " })).toEqual({
      ok: false,
      reason: "missing_server_profile_id",
    });
  });

  it("rejects non-integer cols/rows", () => {
    expect(
      validateCreateRequest({ server_profile_id: "abc", cols: 80.5, rows: 24 }),
    ).toEqual({ ok: false, reason: "non-integer-cols" });
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: 80,
        rows: Number.NaN,
      }),
    ).toEqual({ ok: false, reason: "non-integer-rows" });
  });

  it("rejects below-min and above-max", () => {
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: CELL_GRID_MIN - 1,
        rows: 24,
      }),
    ).toEqual({ ok: false, reason: "below-min-cols" });
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: 80,
        rows: CELL_GRID_MIN - 1,
      }),
    ).toEqual({ ok: false, reason: "below-min-rows" });
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: CELL_GRID_MAX + 1,
        rows: 24,
      }),
    ).toEqual({ ok: false, reason: "above-max-cols" });
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: 80,
        rows: CELL_GRID_MAX + 1,
      }),
    ).toEqual({ ok: false, reason: "above-max-rows" });
  });

  it("accepts the inclusive bounds", () => {
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: CELL_GRID_MIN,
        rows: CELL_GRID_MIN,
      }).ok,
    ).toBe(true);
    expect(
      validateCreateRequest({
        server_profile_id: "abc",
        cols: CELL_GRID_MAX,
        rows: CELL_GRID_MAX,
      }).ok,
    ).toBe(true);
  });
});

describe("createTerminalSession - request shaping", () => {
  it("posts validated body as JSON to the canonical endpoint", async () => {
    let captured: { url: string; init: RequestInit | undefined } | null = null;
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured = { url: String(input), init };
      return new Response(JSON.stringify(VALID_BODY), {
        status: 201,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;

    const result = await createTerminalSession(
      { server_profile_id: "  abc  ", cols: 100, rows: 30 },
      { fetchImpl },
    );

    expect(result.ok).toBe(true);
    expect(captured).not.toBeNull();
    expect(captured!.url).toBe("/api/v1/terminal-sessions");
    expect(captured!.init?.method).toBe("POST");
    expect(captured!.init?.body).toBe(
      JSON.stringify({ server_profile_id: "abc", cols: 100, rows: 30 }),
    );
  });

  it("returns parsed session on a 2xx body", async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify(VALID_BODY), {
        status: 201,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "abc" },
      { fetchImpl },
    );
    expect(result).toEqual({ ok: true, session: VALID_BODY });
  });

  it("ignores unknown extra fields in a 2xx body", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({ ...VALID_BODY, future_field: "ignore me" }),
        {
          status: 201,
          headers: { "content-type": "application/json" },
        },
      )) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "abc" },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
  });

  it("collapses validation failure to a typed error before fetching", async () => {
    let calls = 0;
    const fetchImpl = (async () => {
      calls++;
      return new Response("", { status: 200 });
    }) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({
        kind: "validation",
        reason: "missing_server_profile_id",
      });
    }
    expect(calls).toBe(0);
  });
});

describe("createTerminalSession - error mapping", () => {
  it("maps a 4xx error envelope to a safe http error summary", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          error: {
            code: "conflict",
            message: "host_key conflict",
            // Sentinel field — must NOT appear in the typed error.
            operator_detail: SENTINEL,
          },
        }),
        { status: 409, headers: { "content-type": "application/json" } },
      )) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "abc" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({
        kind: "http",
        status: 409,
        code: "conflict",
        message: "host_key conflict",
      });
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL);
    }
  });

  it("falls back to status text when the error body is malformed", async () => {
    const fetchImpl = (async () =>
      new Response("not json", {
        status: 500,
        statusText: "Internal Server Error",
      })) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "abc" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(500);
      expect(result.error.code).toBe("unknown_error");
      // Status text is a static server string; no sentinel could leak here
      // because we never read the body for malformed envelopes.
      expect(result.error.message).toBe("Internal Server Error");
    } else {
      expect.fail("expected http error");
    }
  });

  it("preserves the thrown message in the typed error for programmatic callers", async () => {
    const fetchImpl = (async () => {
      throw new Error(`boom ${SENTINEL}`);
    }) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "abc" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      // The helper passes through the thrown `Error.message` so a
      // programmatic caller can branch on it. The redaction rule is
      // enforced at the formatter (see `describeCreateError` test
      // below) — the launcher's status line never echoes this string.
      expect(result.error.message).toContain("boom");
    } else {
      expect.fail("expected transport error");
    }
  });

  it("flags malformed success bodies", async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify({ id: 42 /* wrong type */ }), {
        status: 201,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;
    const result = await createTerminalSession(
      { server_profile_id: "abc" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({ kind: "malformed_response" });
    }
  });
});

describe("describeCreateError", () => {
  it("formats the four error kinds without echoing operator detail", () => {
    expect(
      describeCreateError({ kind: "validation", reason: "below-min-cols" }),
    ).toBe("invalid request: below-min-cols");
    expect(
      describeCreateError({
        kind: "http",
        status: 409,
        code: "conflict",
        message: `peer banner ${SENTINEL}`,
      }),
    ).toBe("create failed: HTTP 409 conflict");
    expect(
      describeCreateError({ kind: "transport", message: "Failed to fetch" }),
    ).toBe("create failed: transport error");
    expect(describeCreateError({ kind: "malformed_response" })).toBe(
      "create failed: malformed response",
    );
  });

  it("never echoes the wire `message` field of an http error", () => {
    // Defense-in-depth pin: if a future revision tries to "be helpful" and
    // include the `message` in the formatted summary, this test is the
    // tripwire. The backend's `ApiError` already collapses internal detail
    // to static strings, but the launcher's own status text MUST stay
    // dependent only on status+code, not on the wire body.
    const summary = describeCreateError({
      kind: "http",
      status: 502,
      code: "bad_gateway",
      message: SENTINEL,
    });
    expect(summary).not.toContain(SENTINEL);
  });

  it("never echoes the thrown message of a transport error", () => {
    // The transport `message` field is allowed inside the typed error
    // (programmatic callers may branch on it), but the launcher's
    // status line MUST stay free of fetch-wrapper detail. A future
    // wrapper that included the request URL / headers / retry log in
    // `Error.message` would otherwise leak through this surface.
    const summary = describeCreateError({
      kind: "transport",
      message: `request to https://example.com/path with headers ${SENTINEL}`,
    });
    expect(summary).not.toContain(SENTINEL);
    expect(summary).not.toContain("https://");
  });
});

// ---------------------------------------------------------------------------
// List + close helpers (Sessions list view)
// ---------------------------------------------------------------------------

const LIST_SESSION: TerminalSession = {
  id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
  server_profile_id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
  status: "active",
  cols: 80,
  rows: 24,
  created_at: "2026-04-30T00:00:00Z",
  last_seen_at: "2026-04-30T00:00:01Z",
  closed_at: null,
};

describe("parseTerminalSession", () => {
  it("parses a minimal list-row body", () => {
    const parsed = parseTerminalSession({
      id: LIST_SESSION.id,
      server_profile_id: LIST_SESSION.server_profile_id,
      status: "active",
      cols: 80,
      rows: 24,
      created_at: LIST_SESSION.created_at,
      last_seen_at: LIST_SESSION.last_seen_at,
      closed_at: null,
    });
    expect(parsed).toEqual(LIST_SESSION);
  });

  it("treats a missing `closed_at` as `null`", () => {
    const parsed = parseTerminalSession({
      id: LIST_SESSION.id,
      server_profile_id: LIST_SESSION.server_profile_id,
      status: "starting",
      cols: 80,
      rows: 24,
      created_at: LIST_SESSION.created_at,
      last_seen_at: LIST_SESSION.last_seen_at,
    });
    expect(parsed).not.toBeNull();
    expect(parsed?.closed_at).toBeNull();
  });

  it("rejects unknown status values", () => {
    expect(
      parseTerminalSession({ ...LIST_SESSION, status: "unknown" }),
    ).toBeNull();
  });

  it("returns null for non-objects", () => {
    expect(parseTerminalSession(null)).toBeNull();
    expect(parseTerminalSession(42)).toBeNull();
    expect(parseTerminalSession("nope")).toBeNull();
  });

  it("does NOT carry private_key or encrypted_private_key onto the parsed DTO", () => {
    // Redaction pin: even if the wire body smuggles either field, the
    // parser builds the DTO field-by-field so neither can reach the
    // returned object or any subsequent JSON.stringify of it.
    const parsed = parseTerminalSession({
      id: LIST_SESSION.id,
      server_profile_id: LIST_SESSION.server_profile_id,
      status: "active",
      cols: 80,
      rows: 24,
      created_at: LIST_SESSION.created_at,
      last_seen_at: LIST_SESSION.last_seen_at,
      closed_at: null,
      private_key: SENTINEL,
      encrypted_private_key: SENTINEL,
    });
    expect(parsed).not.toBeNull();
    const obj = parsed as Record<string, unknown>;
    expect(obj.private_key).toBeUndefined();
    expect(obj.encrypted_private_key).toBeUndefined();
    expect(JSON.stringify(parsed)).not.toContain(SENTINEL);
  });
});

describe("listTerminalSessions", () => {
  it("returns a parsed list on a 2xx body", async () => {
    let captured: { url: string } | null = null;
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = { url: String(input) };
      return new Response(JSON.stringify([LIST_SESSION]), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }) as unknown as typeof fetch;

    const result = await listTerminalSessions({ fetchImpl });
    expect(captured?.url).toBe("/api/v1/terminal-sessions");
    expect(result).toEqual({ ok: true, data: [LIST_SESSION] });
  });

  it("collapses an http envelope to a typed error and never leaks operator detail", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          error: {
            code: "internal_error",
            message: "internal error",
            operator_detail: SENTINEL,
          },
        }),
        { status: 500, headers: { "content-type": "application/json" } },
      )) as unknown as typeof fetch;
    const result = await listTerminalSessions({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL);
    }
  });

  it("flags a partially-valid response as malformed_response", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify([LIST_SESSION, { id: "missing-fields" }]),
        { status: 200, headers: { "content-type": "application/json" } },
      )) as unknown as typeof fetch;
    const result = await listTerminalSessions({ fetchImpl });
    expect(result).toEqual({ ok: false, error: { kind: "malformed_response" } });
  });
});

describe("describeSessionLoadError", () => {
  it("formats kinds without echoing operator detail", () => {
    expect(
      describeSessionLoadError({
        kind: "http",
        status: 500,
        code: "internal_error",
      }),
    ).toBe("Failed to load terminal sessions: HTTP 500 internal_error");
    expect(describeSessionLoadError({ kind: "transport" })).toBe(
      "Failed to load terminal sessions: transport error",
    );
    expect(describeSessionLoadError({ kind: "malformed_response" })).toBe(
      "Failed to load terminal sessions: malformed response",
    );
  });
});

describe("closeTerminalSession", () => {
  it("posts to the canonical endpoint and parses the close envelope", async () => {
    let captured: { url: string; init: RequestInit | undefined } | null = null;
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured = { url: String(input), init };
      return new Response(
        JSON.stringify({
          ...LIST_SESSION,
          status: "closed",
          closed_at: "2026-04-30T00:00:05Z",
          already_closed: false,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }) as unknown as typeof fetch;

    const result = await closeTerminalSession(LIST_SESSION.id, { fetchImpl });
    expect(captured?.url).toBe(
      `/api/v1/terminal-sessions/${LIST_SESSION.id}/close`,
    );
    expect(captured?.init?.method).toBe("POST");
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.result.session.status).toBe("closed");
      expect(result.result.session.closed_at).toBe("2026-04-30T00:00:05Z");
      expect(result.result.already_closed).toBe(false);
    }
  });

  it("flags an already-closed response (idempotent path)", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          ...LIST_SESSION,
          status: "closed",
          closed_at: "2026-04-30T00:00:05Z",
          already_closed: true,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      )) as unknown as typeof fetch;
    const result = await closeTerminalSession(LIST_SESSION.id, { fetchImpl });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.result.already_closed).toBe(true);
    }
  });

  it("encodes session ids that contain unsafe path characters", async () => {
    let captured: string | null = null;
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = String(input);
      return new Response("{}", { status: 200 });
    }) as unknown as typeof fetch;
    await closeTerminalSession("a/b c", { fetchImpl });
    expect(captured).toBe("/api/v1/terminal-sessions/a%2Fb%20c/close");
  });

  it("collapses 404 to a typed error without leaking the wire body", async () => {
    const fetchImpl = (async () =>
      new Response(
        JSON.stringify({
          error: {
            code: "not_found",
            message: "not found",
            operator_detail: SENTINEL,
          },
        }),
        { status: 404, headers: { "content-type": "application/json" } },
      )) as unknown as typeof fetch;
    const result = await closeTerminalSession(LIST_SESSION.id, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(404);
      expect(result.error.code).toBe("not_found");
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL);
    } else {
      expect.fail("expected http error");
    }
  });

  it("returns malformed_response when the body cannot be parsed as a close response", async () => {
    const fetchImpl = (async () =>
      new Response(JSON.stringify({ id: "x" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      })) as unknown as typeof fetch;
    const result = await closeTerminalSession(LIST_SESSION.id, { fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });
});

describe("describeCloseSessionError", () => {
  it("never echoes the wire `message` of an http error", () => {
    const summary = describeCloseSessionError({
      kind: "http",
      status: 500,
      code: "internal_error",
      message: SENTINEL,
    });
    expect(summary).not.toContain(SENTINEL);
  });

  it("never echoes the thrown message of a transport error", () => {
    const summary = describeCloseSessionError({
      kind: "transport",
      message: `request to https://example.com ${SENTINEL}`,
    });
    expect(summary).not.toContain(SENTINEL);
    expect(summary).not.toContain("https://");
  });

  it("formats 404 specifically", () => {
    expect(
      describeCloseSessionError({
        kind: "http",
        status: 404,
        code: "not_found",
        message: "not found",
      }),
    ).toBe("Could not close session: session not found");
  });

  it("formats 401 specifically", () => {
    expect(
      describeCloseSessionError({
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: "unauthorized",
      }),
    ).toBe("Could not close session: not authenticated");
  });
});
