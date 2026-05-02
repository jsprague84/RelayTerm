import { describe, expect, it, vi } from "vitest";
import {
  changePassword,
  describeChangePasswordError,
  describeChangePasswordFormError,
  describeChangePasswordSuccess,
  parseChangePasswordResponse,
  validateChangePasswordForm,
  type AuthError,
  type ChangePasswordFormDraft,
  type ChangePasswordResponse,
} from "../src/lib/api/auth.js";

/**
 * Sentinels that MUST NEVER appear in user-visible UI strings, parsed
 * DTOs, or formatted summaries. The redaction rule for the change-
 * password surface mirrors the auth / sessions surfaces: operator-
 * facing detail and sensitive request inputs do not reach any string
 * the SPA renders.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_PWCHANGE_OPERATOR_DETAIL_9201";
const SENTINEL_CURRENT_PASSWORD =
  "RELAY_SENTINEL_PWCHANGE_CURRENT_PASSWORD_9202";
const SENTINEL_NEW_PASSWORD = "RELAY_SENTINEL_PWCHANGE_NEW_PASSWORD_9203";
const SENTINEL_PASSWORD_HASH = "RELAY_SENTINEL_PWCHANGE_PASSWORD_HASH_9204";
const SENTINEL_SESSION_TOKEN = "RELAY_SENTINEL_PWCHANGE_SESSION_TOKEN_9205";
const SENTINEL_TOKEN_HASH = "RELAY_SENTINEL_PWCHANGE_TOKEN_HASH_9206";
const SENTINEL_BOOTSTRAP_TOKEN = "RELAY_SENTINEL_PWCHANGE_BOOTSTRAP_TOKEN_9207";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PWCHANGE_PRIVATE_KEY_9208";
const SENTINEL_ENCRYPTED_PRIVATE_KEY =
  "RELAY_SENTINEL_PWCHANGE_ENCRYPTED_PRIVATE_KEY_9209";
const SENTINEL_TRANSPORT_DETAIL =
  "RELAY_SENTINEL_PWCHANGE_TRANSPORT_DETAIL_9210";

const SUCCESS_FIXTURE: ChangePasswordResponse = {
  revoked_other_sessions: 2,
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

interface CapturedCall {
  url: string;
  init: RequestInit;
}

function captureFetch(
  responder: (call: CapturedCall) => Response | Promise<Response>,
): { fetchImpl: typeof fetch; calls: CapturedCall[] } {
  const calls: CapturedCall[] = [];
  const fetchImpl = (async (
    input: string | URL | Request,
    init: RequestInit = {},
  ) => {
    const url = String(input);
    const captured: CapturedCall = { url, init };
    calls.push(captured);
    return await responder(captured);
  }) as unknown as typeof fetch;
  return { fetchImpl, calls };
}

// ---------------------------------------------------------------------
// parseChangePasswordResponse
// ---------------------------------------------------------------------

describe("parseChangePasswordResponse", () => {
  it("accepts a well-formed response", () => {
    expect(parseChangePasswordResponse(SUCCESS_FIXTURE)).toEqual(
      SUCCESS_FIXTURE,
    );
  });

  it("accepts a zero count", () => {
    expect(parseChangePasswordResponse({ revoked_other_sessions: 0 })).toEqual({
      revoked_other_sessions: 0,
    });
  });

  it("returns null on a missing count", () => {
    expect(parseChangePasswordResponse({})).toBeNull();
  });

  it("returns null on a non-numeric count", () => {
    expect(
      parseChangePasswordResponse({ revoked_other_sessions: "2" }),
    ).toBeNull();
  });

  it("returns null on a non-finite count", () => {
    expect(
      parseChangePasswordResponse({ revoked_other_sessions: Infinity }),
    ).toBeNull();
    expect(
      parseChangePasswordResponse({ revoked_other_sessions: NaN }),
    ).toBeNull();
  });

  it("returns null on a negative count", () => {
    expect(
      parseChangePasswordResponse({ revoked_other_sessions: -1 }),
    ).toBeNull();
  });

  it("ignores unknown extra fields silently", () => {
    const parsed = parseChangePasswordResponse({
      revoked_other_sessions: 3,
      // Smuggled secret-shaped fields MUST NOT make it onto the parsed
      // object — field-by-field construction is the redaction backstop.
      [SENTINEL_PASSWORD_HASH]: "x",
      [SENTINEL_SESSION_TOKEN]: "x",
      [SENTINEL_TOKEN_HASH]: "x",
      [SENTINEL_PRIVATE_KEY]: "x",
      [SENTINEL_ENCRYPTED_PRIVATE_KEY]: "x",
      [SENTINEL_BOOTSTRAP_TOKEN]: "x",
    });
    expect(parsed).toEqual({ revoked_other_sessions: 3 });
    const serialized = JSON.stringify(parsed);
    for (const sentinel of [
      SENTINEL_PASSWORD_HASH,
      SENTINEL_SESSION_TOKEN,
      SENTINEL_TOKEN_HASH,
      SENTINEL_PRIVATE_KEY,
      SENTINEL_ENCRYPTED_PRIVATE_KEY,
      SENTINEL_BOOTSTRAP_TOKEN,
    ]) {
      expect(serialized).not.toContain(sentinel);
    }
  });
});

// ---------------------------------------------------------------------
// changePassword
// ---------------------------------------------------------------------

describe("changePassword", () => {
  it("POSTs to /api/v1/auth/change-password with credentials: include", async () => {
    const { fetchImpl, calls } = captureFetch(() =>
      jsonResponse(200, { revoked_other_sessions: 0 }),
    );
    const result = await changePassword(
      {
        current_password: SENTINEL_CURRENT_PASSWORD,
        new_password: SENTINEL_NEW_PASSWORD,
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.response.revoked_other_sessions).toBe(0);
    }
    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe("/api/v1/auth/change-password");
    expect(calls[0]!.init.method).toBe("POST");
    expect(calls[0]!.init.credentials).toBe("include");
    const headers = calls[0]!.init.headers as Record<string, string>;
    expect(headers["content-type"]).toBe("application/json");
    // The body is allowed to carry the offered passwords on the wire —
    // they're being sent to the backend on purpose. The redaction rules
    // apply to UI strings, error formatting, and parsed objects.
    const body = JSON.parse(calls[0]!.init.body as string);
    expect(body).toEqual({
      current_password: SENTINEL_CURRENT_PASSWORD,
      new_password: SENTINEL_NEW_PASSWORD,
    });
  });

  it("returns malformed_response when the backend ships a missing count", async () => {
    const { fetchImpl } = captureFetch(() => jsonResponse(200, {}));
    const result = await changePassword(
      {
        current_password: "x".repeat(20),
        new_password: "y".repeat(20),
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("malformed_response");
    }
  });

  it("returns malformed_response when the backend ships a negative count", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(200, { revoked_other_sessions: -1 }),
    );
    const result = await changePassword(
      {
        current_password: "x".repeat(20),
        new_password: "y".repeat(20),
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("malformed_response");
    }
  });

  it("surfaces a 401 as a typed http error and does not throw", async () => {
    const { fetchImpl } = captureFetch(() =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: SENTINEL_OPERATOR },
      }),
    );
    const result = await changePassword(
      {
        current_password: "x".repeat(20),
        new_password: "y".repeat(20),
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("http");
      if (result.error.kind === "http") {
        expect(result.error.status).toBe(401);
        expect(result.error.code).toBe("unauthorized");
      }
    }
  });

  it("does not log to the console on transport failure", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const consoleWarnSpy = vi
      .spyOn(console, "warn")
      .mockImplementation(() => {});
    const consoleLogSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    try {
      const fetchImpl = (async () => {
        throw new Error(SENTINEL_TRANSPORT_DETAIL);
      }) as unknown as typeof fetch;
      const result = await changePassword(
        {
          current_password: SENTINEL_CURRENT_PASSWORD,
          new_password: SENTINEL_NEW_PASSWORD,
        },
        { fetchImpl },
      );
      expect(result.ok).toBe(false);
      if (!result.ok) {
        expect(result.error.kind).toBe("transport");
      }
      expect(consoleSpy).not.toHaveBeenCalled();
      expect(consoleWarnSpy).not.toHaveBeenCalled();
      expect(consoleLogSpy).not.toHaveBeenCalled();
    } finally {
      consoleSpy.mockRestore();
      consoleWarnSpy.mockRestore();
      consoleLogSpy.mockRestore();
    }
  });
});

// ---------------------------------------------------------------------
// describeChangePasswordError
// ---------------------------------------------------------------------

describe("describeChangePasswordError", () => {
  it("collapses 401 to a generic current-password / session message", () => {
    const err: AuthError = {
      kind: "http",
      status: 401,
      code: "unauthorized",
      message: SENTINEL_OPERATOR,
    };
    const summary = describeChangePasswordError(err);
    expect(summary.toLowerCase()).toContain("current password");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats 400 as a generic policy message", () => {
    const err: AuthError = {
      kind: "http",
      status: 400,
      code: "invalid_input",
      message: SENTINEL_OPERATOR,
    };
    const summary = describeChangePasswordError(err);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("password policy");
  });

  it("formats 403 as a CSRF-shaped message", () => {
    const err: AuthError = {
      kind: "http",
      status: 403,
      code: "csrf_origin_mismatch",
      message: SENTINEL_OPERATOR,
    };
    const summary = describeChangePasswordError(err);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("browser security policy");
  });

  it("never echoes the wire `message` for any HTTP status", () => {
    for (const status of [400, 401, 403, 409, 500, 502, 503]) {
      const err: AuthError = {
        kind: "http",
        status,
        code: "any_code",
        message: SENTINEL_OPERATOR,
      };
      expect(describeChangePasswordError(err)).not.toContain(SENTINEL_OPERATOR);
    }
  });

  it("returns a generic transport string on transport errors", () => {
    expect(describeChangePasswordError({ kind: "transport" })).toBe(
      "Cannot reach the backend.",
    );
  });

  it("returns a generic malformed-response string on parse errors", () => {
    expect(describeChangePasswordError({ kind: "malformed_response" })).toBe(
      "Cannot change password: malformed response.",
    );
  });
});

// ---------------------------------------------------------------------
// describeChangePasswordSuccess
// ---------------------------------------------------------------------

describe("describeChangePasswordSuccess", () => {
  it("renders a singular form for one revoked session", () => {
    expect(
      describeChangePasswordSuccess({ revoked_other_sessions: 1 }),
    ).toContain("1 other session was signed out");
  });

  it("renders a plural form for multiple revoked sessions", () => {
    expect(
      describeChangePasswordSuccess({ revoked_other_sessions: 5 }),
    ).toContain("5 other sessions were signed out");
  });

  it("omits a session count when none were revoked", () => {
    const summary = describeChangePasswordSuccess({
      revoked_other_sessions: 0,
    });
    expect(summary).toBe("Password updated.");
    expect(summary).not.toContain("0");
    expect(summary).not.toContain("session");
  });
});

// ---------------------------------------------------------------------
// validateChangePasswordForm
// ---------------------------------------------------------------------

describe("validateChangePasswordForm", () => {
  function valid(): ChangePasswordFormDraft {
    return {
      current_password: "current-password-meets-min",
      new_password: "new-password-meets-min",
      new_password_confirmation: "new-password-meets-min",
    };
  }

  it("accepts a well-formed draft", () => {
    expect(validateChangePasswordForm(valid())).toEqual({ ok: true });
  });

  it("rejects a missing current password", () => {
    const draft = valid();
    draft.current_password = "";
    expect(validateChangePasswordForm(draft)).toEqual({
      ok: false,
      reason: "missing_current_password",
    });
  });

  it("rejects a missing new password", () => {
    const draft = valid();
    draft.new_password = "";
    draft.new_password_confirmation = "";
    expect(validateChangePasswordForm(draft)).toEqual({
      ok: false,
      reason: "missing_new_password",
    });
  });

  it("rejects a too-short new password", () => {
    const draft = valid();
    draft.new_password = "short";
    draft.new_password_confirmation = "short";
    expect(validateChangePasswordForm(draft)).toEqual({
      ok: false,
      reason: "new_password_too_short",
    });
  });

  it("rejects a too-long new password", () => {
    const draft = valid();
    const huge = "x".repeat(2000);
    draft.new_password = huge;
    draft.new_password_confirmation = huge;
    expect(validateChangePasswordForm(draft)).toEqual({
      ok: false,
      reason: "new_password_too_long",
    });
  });

  it("rejects a new password equal to the current one", () => {
    const draft = valid();
    draft.new_password = draft.current_password;
    draft.new_password_confirmation = draft.current_password;
    expect(validateChangePasswordForm(draft)).toEqual({
      ok: false,
      reason: "new_password_same_as_current",
    });
  });

  it("rejects a confirmation mismatch", () => {
    const draft = valid();
    draft.new_password_confirmation = "different-value-at-min-length";
    expect(validateChangePasswordForm(draft)).toEqual({
      ok: false,
      reason: "confirmation_mismatch",
    });
  });

  it("describes every form-error reason without echoing inputs", () => {
    const reasons = [
      "missing_current_password",
      "missing_new_password",
      "new_password_too_short",
      "new_password_too_long",
      "new_password_same_as_current",
      "confirmation_mismatch",
    ] as const;
    for (const reason of reasons) {
      const summary = describeChangePasswordFormError(reason);
      expect(summary.length).toBeGreaterThan(0);
      // Sentinel passwords MUST NEVER reach a form-error string.
      expect(summary).not.toContain(SENTINEL_CURRENT_PASSWORD);
      expect(summary).not.toContain(SENTINEL_NEW_PASSWORD);
    }
  });
});
