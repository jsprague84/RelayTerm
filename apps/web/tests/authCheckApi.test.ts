import { describe, expect, it, vi } from "vitest";
import {
  authCheckServerProfile,
  describeAuthCheckError,
  parseAuthCheckResponse,
  type AuthCheckError,
  type AuthCheckResponse,
} from "../src/lib/api/serverProfiles.js";
import {
  AUTH_CHECK_DISCLAIMER,
  AUTH_CHECK_SUCCESS_FOOTNOTE,
  authCheckStatusDescription,
  authCheckStatusLabel,
  authCheckStatusTone,
  terminalLaunchWouldBeAllowed,
} from "../src/lib/app/authCheckState.js";

/**
 * Sentinels MUST NEVER appear in formatted UI strings, parsed DTOs, or
 * helper output. Mirrors the redaction rule in `hostKeyApi.test.ts`:
 *  - Operator detail in 4xx/5xx envelopes does not reach the formatted
 *    summary string.
 *  - Transport `Error.message` does not reach the formatted summary.
 *  - `private_key` / `encrypted_private_key` fields cannot smuggle into
 *    the parsed auth-check response (defense in depth — the wire shape
 *    does not declare them, but the parser builds field-by-field, so a
 *    hostile fixture cannot sneak them through).
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_AUTHCHECK_OPERATOR_DETAIL_9201";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_AUTHCHECK_PRIVATE_KEY_9202";

const PROFILE_ID = "77777777-7777-7777-7777-777777777777";
const HOST_ID = "88888888-8888-8888-8888-888888888888";
const IDENTITY_ID = "99999999-9999-9999-9999-999999999999";

const SUCCEEDED_FIXTURE: AuthCheckResponse = {
  profile_id: PROFILE_ID,
  host_id: HOST_ID,
  ssh_identity_id: IDENTITY_ID,
  status: "authentication_succeeded",
  message:
    "ssh public-key authentication succeeded; no PTY was allocated and no command was executed",
  checked_at: "2026-04-30T12:00:00Z",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("parseAuthCheckResponse", () => {
  it("accepts a well-formed response for every wire status", () => {
    const statuses = [
      "authentication_succeeded",
      "authentication_failed",
      "host_key_unknown",
      "host_key_changed",
      "connection_failed",
    ] as const;
    for (const status of statuses) {
      const fixture = { ...SUCCEEDED_FIXTURE, status };
      const parsed = parseAuthCheckResponse(fixture);
      expect(parsed).not.toBeNull();
      expect(parsed?.status).toBe(status);
    }
  });

  it("rejects unknown status values", () => {
    expect(
      parseAuthCheckResponse({ ...SUCCEEDED_FIXTURE, status: "totally_made_up" }),
    ).toBeNull();
    // `revoked` is NOT a wire status here — the backend uses
    // `host_key_unknown` for revoked-and-reappearing.
    expect(
      parseAuthCheckResponse({ ...SUCCEEDED_FIXTURE, status: "revoked" }),
    ).toBeNull();
  });

  it("rejects responses missing required fields", () => {
    const { ssh_identity_id: _id, ...missingIdentity } = SUCCEEDED_FIXTURE;
    expect(parseAuthCheckResponse(missingIdentity)).toBeNull();
    const { checked_at: _ts, ...missingCheckedAt } = SUCCEEDED_FIXTURE;
    expect(parseAuthCheckResponse(missingCheckedAt)).toBeNull();
    const { message: _msg, ...missingMessage } = SUCCEEDED_FIXTURE;
    expect(parseAuthCheckResponse(missingMessage)).toBeNull();
  });

  it("does not expose private_key / encrypted_private_key on the parsed object", () => {
    const parsed = parseAuthCheckResponse({
      ...SUCCEEDED_FIXTURE,
      private_key: SENTINEL_PRIVATE_KEY,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
    });
    expect(parsed).not.toBeNull();
    if (parsed) {
      expect(JSON.stringify(parsed)).not.toContain(SENTINEL_PRIVATE_KEY);
      expect("private_key" in parsed).toBe(false);
      expect("encrypted_private_key" in parsed).toBe(false);
    }
  });

  it("rejects non-object input", () => {
    expect(parseAuthCheckResponse(null)).toBeNull();
    expect(parseAuthCheckResponse("nope")).toBeNull();
    expect(parseAuthCheckResponse(42)).toBeNull();
  });
});

describe("authCheckServerProfile", () => {
  it("posts to the per-profile endpoint with an empty JSON body", async () => {
    const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
    const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init });
      return jsonResponse(200, SUCCEEDED_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await authCheckServerProfile(PROFILE_ID, { fetchImpl });
    expect(result).toEqual({ ok: true, check: SUCCEEDED_FIXTURE });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(
      `/api/v1/server-profiles/${PROFILE_ID}/auth-check`,
    );
    expect(calls[0].init?.method).toBe("POST");
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({});
  });

  it("URL-encodes the profile id", async () => {
    const calls: string[] = [];
    const fetchImpl = (async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse(200, {
        ...SUCCEEDED_FIXTURE,
        profile_id: "weird/id with space",
      });
    }) as unknown as typeof fetch;
    await authCheckServerProfile("weird/id with space", { fetchImpl });
    expect(calls[0]).toBe(
      "/api/v1/server-profiles/weird%2Fid%20with%20space/auth-check",
    );
  });

  it("returns typed status outcomes on a 200 (host_key_unknown is NOT an HTTP error)", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, {
        ...SUCCEEDED_FIXTURE,
        status: "host_key_unknown",
        message: "host key is not pinned and trusted",
      })) as unknown as typeof fetch;
    const result = await authCheckServerProfile(PROFILE_ID, { fetchImpl });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.check.status).toBe("host_key_unknown");
    }
  });

  it("maps a 503 envelope to a typed http error WITHOUT logging", async () => {
    const fetchImpl = (async () =>
      jsonResponse(503, {
        error: {
          code: "service_unavailable",
          message: `vault disabled ${SENTINEL_OPERATOR}`,
        },
      })) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const result = await authCheckServerProfile(PROFILE_ID, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(503);
      expect(result.error.code).toBe("service_unavailable");
      // The typed error preserves the wire message for programmatic
      // callers; the formatter is responsible for redacting it.
      expect(describeAuthCheckError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });

  it("collapses an unparseable success body to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, { not_an_auth_check: true })) as unknown as typeof fetch;
    const result = await authCheckServerProfile(PROFILE_ID, { fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("maps a transport rejection to a typed transport error", async () => {
    const fetchImpl = (async () => {
      throw new Error(`network ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    const result = await authCheckServerProfile(PROFILE_ID, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      expect(describeAuthCheckError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected transport error");
    }
  });

  it("does not log on transport failure", async () => {
    const fetchImpl = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    await authCheckServerProfile(PROFILE_ID, { fetchImpl });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

describe("describeAuthCheckError", () => {
  it("never echoes wire `message` for any HTTP status", () => {
    const statuses: Array<AuthCheckError> = [
      {
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: SENTINEL_OPERATOR,
      },
      {
        kind: "http",
        status: 404,
        code: "not_found",
        message: SENTINEL_OPERATOR,
      },
      {
        kind: "http",
        status: 500,
        code: "internal_error",
        message: SENTINEL_OPERATOR,
      },
      {
        kind: "http",
        status: 503,
        code: "service_unavailable",
        message: SENTINEL_OPERATOR,
      },
      {
        kind: "http",
        status: 599,
        code: "weird",
        message: SENTINEL_OPERATOR,
      },
    ];
    for (const err of statuses) {
      expect(describeAuthCheckError(err)).not.toContain(SENTINEL_OPERATOR);
    }
  });

  it("never echoes a transport error's thrown message", () => {
    expect(
      describeAuthCheckError({
        kind: "transport",
        message: `request failed ${SENTINEL_OPERATOR}`,
      }),
    ).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats vault/saturation as a precise hint", () => {
    const summary = describeAuthCheckError({
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    const lower = summary.toLowerCase();
    expect(lower).toContain("vault");
    expect(lower).toContain("saturat");
  });

  it("formats internal_error as a vault-data-integrity hint", () => {
    const summary = describeAuthCheckError({
      kind: "http",
      status: 500,
      code: "internal_error",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("decrypt");
  });

  it("formats not-found as a precise hint", () => {
    const summary = describeAuthCheckError({
      kind: "http",
      status: 404,
      code: "not_found",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("server profile not found");
  });

  it("formats unknown HTTP errors generically without the wire message", () => {
    expect(
      describeAuthCheckError({
        kind: "http",
        status: 418,
        code: "teapot",
        message: SENTINEL_OPERATOR,
      }),
    ).toBe("Auth-check failed: HTTP 418 teapot");
  });

  it("formats malformed_response without exposing data", () => {
    expect(describeAuthCheckError({ kind: "malformed_response" })).toBe(
      "Auth-check failed: malformed response",
    );
  });
});

describe("authCheckStatusLabel", () => {
  it("labels each wire status with conservative copy", () => {
    expect(authCheckStatusLabel("authentication_succeeded")).toBe(
      "Authenticated",
    );
    expect(authCheckStatusLabel("authentication_failed")).toBe(
      "Auth rejected",
    );
    expect(authCheckStatusLabel("host_key_unknown")).toBe(
      "Host key not trusted",
    );
    expect(authCheckStatusLabel("host_key_changed")).toBe(
      "Host key changed",
    );
    expect(authCheckStatusLabel("connection_failed")).toBe(
      "Connection failed",
    );
  });

  it("never implies a terminal session, shell, or command was opened", () => {
    for (const status of [
      "authentication_succeeded",
      "authentication_failed",
      "host_key_unknown",
      "host_key_changed",
      "connection_failed",
    ] as const) {
      const label = authCheckStatusLabel(status).toLowerCase();
      expect(label).not.toContain("terminal");
      expect(label).not.toContain("shell");
      expect(label).not.toContain("session opened");
      expect(label).not.toContain("pty");
    }
  });
});

describe("authCheckStatusDescription", () => {
  it("success copy disclaims PTY/command/terminal scope", () => {
    const desc = authCheckStatusDescription(
      "authentication_succeeded",
    ).toLowerCase();
    expect(desc).toContain("no pty");
    expect(desc).toContain("no command was executed");
    expect(desc).toContain("separate");
    // Must not imply a terminal session was opened.
    expect(desc).not.toContain("session opened");
    expect(desc).not.toContain("shell ready");
    expect(desc).not.toContain("connected to the shell");
  });

  it("authentication_failed names the wrong-key/wrong-user diagnostic", () => {
    const desc = authCheckStatusDescription(
      "authentication_failed",
    ).toLowerCase();
    expect(desc).toContain("authorized_keys");
  });

  it("host_key_unknown surfaces the trust-host-key precondition", () => {
    const desc = authCheckStatusDescription("host_key_unknown").toLowerCase();
    expect(desc).toContain("trust");
    expect(desc).toContain("preflight");
    // Must not imply auth was attempted.
    expect(desc).not.toContain("authenticated");
  });

  it("host_key_changed warns about MITM and refuses to continue", () => {
    const desc = authCheckStatusDescription("host_key_changed").toLowerCase();
    expect(desc).toContain("differ");
    expect(desc).toContain("man-in-the-middle");
    expect(desc).toContain("not attempted");
  });

  it("connection_failed names the transport-layer cause", () => {
    const desc = authCheckStatusDescription("connection_failed").toLowerCase();
    expect(desc).toContain("transport");
    // Should hint at common causes without leaking peer detail.
    expect(desc).toMatch(/refused|timed out|reach/);
  });

  it("never references private_key / encrypted_private_key", () => {
    for (const status of [
      "authentication_succeeded",
      "authentication_failed",
      "host_key_unknown",
      "host_key_changed",
      "connection_failed",
    ] as const) {
      const desc = authCheckStatusDescription(status).toLowerCase();
      expect(desc).not.toContain("private_key");
      expect(desc).not.toContain("encrypted_private_key");
    }
  });
});

describe("authCheckStatusTone", () => {
  it("maps each status to a deterministic tone", () => {
    expect(authCheckStatusTone("authentication_succeeded")).toBe("ok");
    expect(authCheckStatusTone("authentication_failed")).toBe("error");
    expect(authCheckStatusTone("host_key_unknown")).toBe("warn");
    expect(authCheckStatusTone("host_key_changed")).toBe("blocked");
    expect(authCheckStatusTone("connection_failed")).toBe("error");
  });
});

describe("terminalLaunchWouldBeAllowed", () => {
  it("only allows launch on authentication_succeeded", () => {
    expect(terminalLaunchWouldBeAllowed("authentication_succeeded")).toBe(
      true,
    );
    for (const status of [
      "authentication_failed",
      "host_key_unknown",
      "host_key_changed",
      "connection_failed",
    ] as const) {
      expect(terminalLaunchWouldBeAllowed(status)).toBe(false);
    }
  });
});

describe("static disclaimers", () => {
  it("auth-check disclaimer names the trusted-host-key precondition and disclaims terminal/command scope", () => {
    const t = AUTH_CHECK_DISCLAIMER.toLowerCase();
    expect(t).toContain("trusted host key");
    expect(t).toContain("does not open a terminal");
    expect(t).toContain("does not run commands");
    expect(t).toContain("does not install");
  });

  it("success footnote disclaims terminal launch", () => {
    const t = AUTH_CHECK_SUCCESS_FOOTNOTE.toLowerCase();
    expect(t).toContain("separate action");
    // Must not imply a terminal/shell/PTY exists.
    expect(t).not.toContain("session opened");
    expect(t).not.toContain("shell ready");
    expect(t).not.toContain("pty");
  });
});
