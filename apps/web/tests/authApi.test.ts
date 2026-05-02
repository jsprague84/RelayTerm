import { describe, expect, it, vi } from "vitest";
import {
  bootstrap,
  describeAuthError,
  describeAuthGateError,
  describeBootstrapFormError,
  describeLoginFormError,
  getCurrentUser,
  login,
  logout,
  parseCurrentUser,
  validateBootstrapForm,
  validateLoginForm,
  type CurrentUser,
} from "../src/lib/api/auth.js";

/**
 * Sentinels that MUST NEVER appear in user-visible UI strings, parsed
 * DTOs, or formatted summaries. The redaction rule for the auth surface
 * is the same as the inventory surface — operator-facing detail and
 * sensitive request inputs do not reach any string the SPA renders.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_AUTH_OPERATOR_DETAIL_9001";
const SENTINEL_PASSWORD = "RELAY_SENTINEL_PASSWORD_PLAINTEXT_9002";
const SENTINEL_BOOTSTRAP_TOKEN = "RELAY_SENTINEL_BOOTSTRAP_TOKEN_9003";
const SENTINEL_SESSION_TOKEN = "RELAY_SENTINEL_SESSION_TOKEN_9004";
const SENTINEL_TOKEN_HASH = "RELAY_SENTINEL_TOKEN_HASH_9005";
const SENTINEL_PASSWORD_HASH = "RELAY_SENTINEL_PASSWORD_HASH_9006";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_9007";
const SENTINEL_ENCRYPTED_PRIVATE_KEY =
  "RELAY_SENTINEL_ENCRYPTED_PRIVATE_KEY_9008";
const SENTINEL_ACCESS_TOKEN = "RELAY_SENTINEL_ACCESS_TOKEN_9009";
const SENTINEL_SESSION_OUTPUT = "RELAY_SENTINEL_SESSION_OUTPUT_9010";
const SENTINEL_TRANSPORT_DETAIL = "RELAY_SENTINEL_TRANSPORT_DETAIL_9011";

const USER_FIXTURE: CurrentUser = {
  id: "11111111-1111-1111-1111-111111111111",
  email: "operator@example.com",
  display_name: "Operator",
  created_at: "2026-04-30T00:00:00Z",
  last_login_at: "2026-05-01T01:23:45Z",
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

/** Build a fetch mock that records every call's url + init. */
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
// parseCurrentUser
// ---------------------------------------------------------------------

describe("parseCurrentUser", () => {
  it("accepts a well-formed user", () => {
    expect(parseCurrentUser(USER_FIXTURE)).toEqual(USER_FIXTURE);
  });

  it("treats `last_login_at: null` as null", () => {
    expect(
      parseCurrentUser({ ...USER_FIXTURE, last_login_at: null }),
    ).toEqual({ ...USER_FIXTURE, last_login_at: null });
  });

  it("returns null on missing required fields", () => {
    const { id: _id, ...rest } = USER_FIXTURE;
    expect(parseCurrentUser(rest)).toBeNull();
  });

  it("returns null on wrong-typed last_login_at", () => {
    expect(
      parseCurrentUser({ ...USER_FIXTURE, last_login_at: 12345 }),
    ).toBeNull();
  });

  it("ignores unknown extra fields silently", () => {
    const parsed = parseCurrentUser({
      ...USER_FIXTURE,
      future_safe: "ignored",
    });
    expect(parsed).toEqual(USER_FIXTURE);
  });

  it("drops smuggled secret-shaped fields field-by-field", () => {
    const hostile = {
      ...USER_FIXTURE,
      password_hash: SENTINEL_PASSWORD_HASH,
      session_token: SENTINEL_SESSION_TOKEN,
      token_hash: SENTINEL_TOKEN_HASH,
      bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
      private_key: SENTINEL_PRIVATE_KEY,
      encrypted_private_key: SENTINEL_ENCRYPTED_PRIVATE_KEY,
      access_token: SENTINEL_ACCESS_TOKEN,
      session_output: SENTINEL_SESSION_OUTPUT,
    };
    const parsed = parseCurrentUser(hostile);
    expect(parsed).not.toBeNull();
    // The DTO is built field-by-field; secret-shaped fields cannot
    // reach the parsed object even when present on the input.
    const json = JSON.stringify(parsed);
    for (const sentinel of [
      SENTINEL_PASSWORD_HASH,
      SENTINEL_SESSION_TOKEN,
      SENTINEL_TOKEN_HASH,
      SENTINEL_BOOTSTRAP_TOKEN,
      SENTINEL_PRIVATE_KEY,
      SENTINEL_ENCRYPTED_PRIVATE_KEY,
      SENTINEL_ACCESS_TOKEN,
      SENTINEL_SESSION_OUTPUT,
    ]) {
      expect(parsed as Record<string, unknown>).not.toHaveProperty(
        sentinel,
      );
      expect(json).not.toContain(sentinel);
    }
  });
});

// ---------------------------------------------------------------------
// describeAuthError
// ---------------------------------------------------------------------

describe("describeAuthError", () => {
  it("collapses 401 on sign-in to a generic invalid-credentials line", () => {
    const summary = describeAuthError("sign in", {
      kind: "http",
      status: 401,
      code: "unauthorized",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("Sign in failed: invalid credentials");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    // The copy must NOT reveal whether the offered email is known.
    expect(summary.toLowerCase()).not.toContain("not found");
    expect(summary.toLowerCase()).not.toContain("no such");
    expect(summary.toLowerCase()).not.toContain("unknown email");
    expect(summary.toLowerCase()).not.toContain("does not exist");
  });

  it("collapses 401 on first-time setup to a bootstrap-rejected line", () => {
    const summary = describeAuthError("first-time setup", {
      kind: "http",
      status: 401,
      code: "unauthorized",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe(
      "First-time setup failed: bootstrap token rejected",
    );
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats other actions' 401 as a session-ended line", () => {
    const summary = describeAuthError("load session", {
      kind: "http",
      status: 401,
      code: "unauthorized",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("Your session has ended. Please sign in again.");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats 403 csrf_origin_mismatch as a browser-policy line", () => {
    const summary = describeAuthError("sign in", {
      kind: "http",
      status: 403,
      code: "csrf_origin_mismatch",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toContain("blocked by browser security policy");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats 409 on first-time setup as already-bootstrapped", () => {
    const summary = describeAuthError("first-time setup", {
      kind: "http",
      status: 409,
      code: "conflict",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe(
      "First-time setup is no longer available: an account already exists",
    );
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats 503 on first-time setup as 'disabled on this server'", () => {
    const summary = describeAuthError("first-time setup", {
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("First-time setup is disabled on this server");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("never echoes a wire `message` for an arbitrary HTTP error", () => {
    const summary = describeAuthError("sign in", {
      kind: "http",
      status: 500,
      code: "internal_error",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("Cannot sign in: HTTP 500 internal_error");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("collapses transport detail to a static network-error line", () => {
    const summary = describeAuthError("sign in", { kind: "transport" });
    expect(summary).toBe("Cannot sign in: network error");
  });

  it("collapses malformed_response to a static line", () => {
    const summary = describeAuthError("sign in", {
      kind: "malformed_response",
    });
    expect(summary).toBe("Cannot sign in: malformed response");
  });
});

// ---------------------------------------------------------------------
// describeAuthGateError (used by AuthGate's loading-error surface)
// ---------------------------------------------------------------------

describe("describeAuthGateError", () => {
  it("formats an HTTP failure as a function of status only", () => {
    const summary = describeAuthGateError({
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).toBe("Cannot reach the backend: HTTP 503");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary).not.toContain("service_unavailable");
  });

  it("collapses transport detail to a static line", () => {
    const summary = describeAuthGateError({ kind: "transport" });
    expect(summary).toBe("Cannot reach the backend.");
  });

  it("collapses malformed_response to a static line", () => {
    const summary = describeAuthGateError({ kind: "malformed_response" });
    expect(summary).toBe("Cannot reach the backend: malformed response.");
  });
});

// ---------------------------------------------------------------------
// getCurrentUser
// ---------------------------------------------------------------------

describe("getCurrentUser", () => {
  it("targets /api/v1/auth/me with credentials: include", async () => {
    const { fetchImpl, calls } = captureFetch(() =>
      jsonResponse(200, USER_FIXTURE),
    );
    const result = await getCurrentUser({ fetchImpl });
    expect(result).toEqual({ ok: true, user: USER_FIXTURE });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/v1/auth/me");
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.method).toBe("GET");
  });

  it("returns http error for a 401 envelope", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: "unauthorized" },
      }),
    );
    const result = await getCurrentUser({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(401);
      expect(result.error.code).toBe("unauthorized");
    } else {
      expect.fail("expected http error");
    }
  });

  it("returns transport error when fetch throws", async () => {
    const fetchImpl = (async () => {
      throw new Error(`net ${SENTINEL_TRANSPORT_DETAIL}`);
    }) as unknown as typeof fetch;
    const result = await getCurrentUser({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "transport" },
    });
    // The transport-error variant carries no message — there is
    // nothing to leak through the formatter.
    expect(JSON.stringify(result)).not.toContain(SENTINEL_TRANSPORT_DETAIL);
  });

  it("returns malformed_response when the body is not JSON", async () => {
    const { fetchImpl } = captureFetch(
      () => new Response("not json", { status: 200 }),
    );
    const result = await getCurrentUser({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("returns malformed_response when the body fails parseCurrentUser", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(200, { id: "broken" }),
    );
    const result = await getCurrentUser({ fetchImpl });
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
      jsonResponse(200, USER_FIXTURE)) as unknown as typeof fetch;
    await getCurrentUser({ fetchImpl: okFetch });
    const failFetch = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    await getCurrentUser({ fetchImpl: failFetch });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------
// login
// ---------------------------------------------------------------------

describe("login", () => {
  it("POSTs the credentials to /api/v1/auth/login with credentials: include", async () => {
    const { fetchImpl, calls } = captureFetch(() =>
      jsonResponse(200, USER_FIXTURE),
    );
    const result = await login(
      { email: "operator@example.com", password: SENTINEL_PASSWORD },
      { fetchImpl },
    );
    expect(result).toEqual({ ok: true, user: USER_FIXTURE });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/v1/auth/login");
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.method).toBe("POST");
    const body = JSON.parse(String(calls[0].init.body));
    expect(body).toEqual({
      email: "operator@example.com",
      password: SENTINEL_PASSWORD,
    });
    // The headers carry only accept + content-type. The browser
    // attaches Origin for state-changing requests; we deliberately
    // do NOT supply it from JS.
    const headers = (calls[0].init.headers ?? {}) as Record<string, string>;
    expect(Object.keys(headers).map((k) => k.toLowerCase())).not.toContain(
      "origin",
    );
  });

  it("does not return any session-token field on success", async () => {
    // The backend's wire response is the safe UserResponse. Even if
    // a future regression smuggled a token field onto the response,
    // parseCurrentUser drops it field-by-field. This test pins the
    // contract.
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(200, {
        ...USER_FIXTURE,
        session_token: SENTINEL_SESSION_TOKEN,
        token_hash: SENTINEL_TOKEN_HASH,
        password_hash: SENTINEL_PASSWORD_HASH,
      }),
    );
    const result = await login(
      { email: "operator@example.com", password: SENTINEL_PASSWORD },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      const json = JSON.stringify(result.user);
      expect(json).not.toContain(SENTINEL_SESSION_TOKEN);
      expect(json).not.toContain(SENTINEL_TOKEN_HASH);
      expect(json).not.toContain(SENTINEL_PASSWORD_HASH);
      expect(result.user as Record<string, unknown>).not.toHaveProperty(
        "session_token",
      );
      expect(result.user as Record<string, unknown>).not.toHaveProperty(
        "token_hash",
      );
    }
  });

  it("maps a 401 to an http error without echoing the offered password", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: "invalid credentials" },
      }),
    );
    const result = await login(
      { email: "operator@example.com", password: SENTINEL_PASSWORD },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("http");
      const summary = describeAuthError("sign in", result.error);
      expect(summary).toBe("Sign in failed: invalid credentials");
      expect(summary).not.toContain(SENTINEL_PASSWORD);
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_PASSWORD);
    }
  });

  it("does not log on success or HTTP failure", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const okFetch = (async () =>
      jsonResponse(200, USER_FIXTURE)) as unknown as typeof fetch;
    await login({ email: "u@example.com", password: SENTINEL_PASSWORD }, {
      fetchImpl: okFetch,
    });
    const failFetch = (async () =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: SENTINEL_OPERATOR },
      })) as unknown as typeof fetch;
    await login({ email: "u@example.com", password: SENTINEL_PASSWORD }, {
      fetchImpl: failFetch,
    });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------
// logout
// ---------------------------------------------------------------------

describe("logout", () => {
  it("POSTs to /api/v1/auth/logout with credentials: include and no body", async () => {
    const { fetchImpl, calls } = captureFetch(() => noContent());
    const result = await logout({ fetchImpl });
    expect(result).toEqual({ ok: true });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/v1/auth/logout");
    expect(calls[0].init.method).toBe("POST");
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.body).toBeUndefined();
  });

  it("returns http error if the backend somehow rejects logout", async () => {
    // The backend is idempotent in practice, but the helper still
    // surfaces non-2xx structurally so callers can decide.
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(403, {
        error: { code: "csrf_origin_mismatch", message: SENTINEL_OPERATOR },
      }),
    );
    const result = await logout({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("http");
      if (result.error.kind === "http") {
        expect(result.error.status).toBe(403);
        expect(result.error.code).toBe("csrf_origin_mismatch");
        const summary = describeAuthError("sign out", result.error);
        expect(summary).not.toContain(SENTINEL_OPERATOR);
      }
    }
  });

  it("returns transport error when fetch throws", async () => {
    // Mirrors the equivalent test in `getCurrentUser` / `login` so the
    // logout helper has parity coverage for the three wire-failure
    // shapes (success / HTTP error / transport).
    const fetchImpl = (async () => {
      throw new Error(`net ${SENTINEL_TRANSPORT_DETAIL}`);
    }) as unknown as typeof fetch;
    const result = await logout({ fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "transport" },
    });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_TRANSPORT_DETAIL);
  });

  it("safely formats an unexpected non-2xx logout HTTP response", async () => {
    // The backend is idempotent in practice, but if a future
    // misconfiguration ever produced a 5xx on logout the formatter
    // must still drop the wire `message` rather than echoing it.
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(503, {
        error: { code: "service_unavailable", message: SENTINEL_OPERATOR },
      }),
    );
    const result = await logout({ fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const summary = describeAuthError("sign out", result.error);
      expect(summary).not.toContain(SENTINEL_OPERATOR);
      expect(summary.toLowerCase()).toContain("sign out");
    }
  });
});

// ---------------------------------------------------------------------
// bootstrap
// ---------------------------------------------------------------------

describe("bootstrap", () => {
  it("POSTs the bootstrap body to /api/v1/auth/bootstrap with credentials: include", async () => {
    const { fetchImpl, calls } = captureFetch(() =>
      jsonResponse(201, USER_FIXTURE),
    );
    const result = await bootstrap(
      {
        bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
        email: "first@example.com",
        display_name: "First Operator",
        password: SENTINEL_PASSWORD,
      },
      { fetchImpl },
    );
    expect(result).toEqual({ ok: true, user: USER_FIXTURE });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/v1/auth/bootstrap");
    expect(calls[0].init.credentials).toBe("include");
    expect(calls[0].init.method).toBe("POST");
    const body = JSON.parse(String(calls[0].init.body));
    expect(body).toEqual({
      bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
      email: "first@example.com",
      display_name: "First Operator",
      password: SENTINEL_PASSWORD,
    });
  });

  it("never echoes the bootstrap token or password through any error string", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: "bad bootstrap token" },
      }),
    );
    const result = await bootstrap(
      {
        bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
        email: "first@example.com",
        display_name: "First Operator",
        password: SENTINEL_PASSWORD,
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      const summary = describeAuthError("first-time setup", result.error);
      expect(summary).toBe(
        "First-time setup failed: bootstrap token rejected",
      );
      expect(summary).not.toContain(SENTINEL_BOOTSTRAP_TOKEN);
      expect(summary).not.toContain(SENTINEL_PASSWORD);
      // The typed error preserves the wire `message` for programmatic
      // callers, but the request inputs MUST NOT have leaked into it.
      expect(JSON.stringify(result.error)).not.toContain(
        SENTINEL_BOOTSTRAP_TOKEN,
      );
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_PASSWORD);
    }
  });

  it("does not log on success or HTTP failure", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const okFetch = (async () =>
      jsonResponse(201, USER_FIXTURE)) as unknown as typeof fetch;
    await bootstrap(
      {
        bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
        email: "first@example.com",
        display_name: "First",
        password: SENTINEL_PASSWORD,
      },
      { fetchImpl: okFetch },
    );
    const failFetch = (async () =>
      jsonResponse(409, {
        error: { code: "conflict", message: SENTINEL_OPERATOR },
      })) as unknown as typeof fetch;
    await bootstrap(
      {
        bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
        email: "first@example.com",
        display_name: "First",
        password: SENTINEL_PASSWORD,
      },
      { fetchImpl: failFetch },
    );
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------
// validateLoginForm / describeLoginFormError
// ---------------------------------------------------------------------

describe("validateLoginForm", () => {
  it("accepts a valid email + sufficiently long password", () => {
    expect(
      validateLoginForm({
        email: "user@example.com",
        password: "long-enough-pw",
      }),
    ).toEqual({ ok: true });
  });

  it("rejects empty email", () => {
    expect(
      validateLoginForm({ email: "", password: "long-enough-pw" }),
    ).toEqual({ ok: false, reason: "missing_email" });
  });

  it("rejects malformed emails", () => {
    for (const bad of ["nope", "@nope", "nope@", "two@@signs.com"]) {
      const result = validateLoginForm({
        email: bad,
        password: "long-enough-pw",
      });
      expect(result).toEqual({ ok: false, reason: "email_invalid" });
    }
  });

  it("rejects empty / too-short passwords", () => {
    expect(
      validateLoginForm({ email: "u@example.com", password: "" }),
    ).toEqual({ ok: false, reason: "missing_password" });
    expect(
      validateLoginForm({ email: "u@example.com", password: "short" }),
    ).toEqual({ ok: false, reason: "password_too_short" });
  });

  it("describeLoginFormError stays a function of the reason enum only", () => {
    expect(describeLoginFormError("missing_email")).toBe("Enter your email.");
    expect(describeLoginFormError("email_invalid")).toBe(
      "Enter a valid email.",
    );
    expect(describeLoginFormError("missing_password")).toBe(
      "Enter your password.",
    );
    const tooShort = describeLoginFormError("password_too_short");
    expect(tooShort).toContain("12");
    expect(tooShort.toLowerCase()).not.toContain("not found");
    expect(tooShort.toLowerCase()).not.toContain("unknown");
  });
});

// ---------------------------------------------------------------------
// validateBootstrapForm / describeBootstrapFormError
// ---------------------------------------------------------------------

describe("validateBootstrapForm", () => {
  const VALID = {
    bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
    email: "first@example.com",
    display_name: "First Operator",
    password: "long-enough-pw",
    password_confirmation: "long-enough-pw",
  };

  it("accepts a valid form", () => {
    expect(validateBootstrapForm(VALID)).toEqual({ ok: true });
  });

  it("rejects when password and confirmation differ", () => {
    expect(
      validateBootstrapForm({
        ...VALID,
        password_confirmation: "long-enough-pw-typo",
      }),
    ).toEqual({ ok: false, reason: "password_confirmation_mismatch" });
  });

  it("rejects empty bootstrap token", () => {
    expect(validateBootstrapForm({ ...VALID, bootstrap_token: "" })).toEqual({
      ok: false,
      reason: "missing_bootstrap_token",
    });
  });

  it("rejects empty email and malformed email", () => {
    expect(validateBootstrapForm({ ...VALID, email: "" })).toEqual({
      ok: false,
      reason: "missing_email",
    });
    expect(validateBootstrapForm({ ...VALID, email: "nope" })).toEqual({
      ok: false,
      reason: "email_invalid",
    });
  });

  it("rejects empty display_name", () => {
    expect(validateBootstrapForm({ ...VALID, display_name: "" })).toEqual({
      ok: false,
      reason: "missing_display_name",
    });
  });

  it("rejects too-short / too-long password", () => {
    expect(validateBootstrapForm({ ...VALID, password: "short", password_confirmation: "short" })).toEqual({
      ok: false,
      reason: "password_too_short",
    });
    const huge = "x".repeat(2000);
    expect(
      validateBootstrapForm({
        ...VALID,
        password: huge,
        password_confirmation: huge,
      }),
    ).toEqual({ ok: false, reason: "password_too_long" });
  });

  it("describeBootstrapFormError stays a pure function of the reason enum", () => {
    expect(describeBootstrapFormError("missing_bootstrap_token")).toBe(
      "Enter the bootstrap token.",
    );
    expect(describeBootstrapFormError("password_confirmation_mismatch")).toBe(
      "Passwords do not match.",
    );
  });
});
