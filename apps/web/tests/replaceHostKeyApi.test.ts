import { describe, expect, it, vi } from "vitest";
import {
  describeReplaceHostKeyError,
  isHostKeyReplacementReasonCode,
  parseReplaceHostKeyResponse,
  replaceHostKey,
  type HostKeyPreflightResponse,
  type ReplaceHostKeyError,
  type ReplaceHostKeyResponse,
} from "../src/lib/api/serverProfiles.js";
import {
  decideReplaceSubmit,
  reasonCodeIsValid,
  replaceConfirmationMatches,
  replaceGateForPreflight,
  replacementReasonOptions,
  synthesizePostReplacePreflight,
} from "../src/lib/app/hostKeyTrustState.js";

/**
 * Sentinels MUST NEVER appear in formatted UI strings, parsed DTOs, or
 * helper output. Mirrors the redaction posture in `hostKeyApi.test.ts`:
 *  - Operator detail in 4xx envelopes does not reach the formatted
 *    summary string.
 *  - Transport `Error.message` does not reach the formatted summary.
 *  - `private_key` / `encrypted_private_key` / `password` / `cookie` /
 *    `session_token` / `token_hash` cannot smuggle into the parsed
 *    response (defense in depth — the wire shape doesn't declare those
 *    fields, but the parser builds field-by-field, so a hostile fixture
 *    can't sneak them through).
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_REPLACE_OPERATOR_DETAIL_9301";
const FORBIDDEN_SUBSTRINGS = [
  "RELAY_SENTINEL_REPLACE_PRIVATE_KEY_9302",
  "RELAY_SENTINEL_REPLACE_ENCRYPTED_PRIVATE_KEY_9303",
  "RELAY_SENTINEL_REPLACE_PASSWORD_9304",
  "RELAY_SENTINEL_REPLACE_COOKIE_9305",
  "RELAY_SENTINEL_REPLACE_SESSION_TOKEN_9306",
  "RELAY_SENTINEL_REPLACE_TOKEN_HASH_9307",
] as const;

const PROFILE_ID = "11111111-1111-1111-1111-111111111111";
const HOST_ID = "22222222-2222-2222-2222-222222222222";
const REVOKED_ID = "33333333-3333-3333-3333-333333333333";
const TRUSTED_ID = "44444444-4444-4444-4444-444444444444";
// `SHA256:` (7) + 43 base64 chars = standard SHA-256 fingerprint length.
const OLD_FP = "SHA256:abcdefGHIJKLmnopqrstuvwxyz0123456789ABCDEFGHJ";
const NEW_FP = "SHA256:zyxwvuTSRQPOnmlkjihgfedcba9876543210ZYXWVUT12";

const RESPONSE_FIXTURE: ReplaceHostKeyResponse = {
  profile_id: PROFILE_ID,
  host_id: HOST_ID,
  revoked_known_host_entry_id: REVOKED_ID,
  revoked_fingerprint: OLD_FP,
  trusted_known_host_entry_id: TRUSTED_ID,
  trusted_fingerprint: NEW_FP,
  host_key_type: "ed25519",
  trusted_at: "2026-05-10T00:00:00Z",
};

const PREFLIGHT_CHANGED: HostKeyPreflightResponse = {
  profile_id: PROFILE_ID,
  host_id: HOST_ID,
  hostname: "edge-1.example.internal",
  port: 22,
  host_key_status: "changed",
  host_key_type: "ed25519",
  host_key_fingerprint: NEW_FP,
  active_pin_fingerprint: OLD_FP,
  message: "host key changed; trust route refuses",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

// ---------------------------------------------------------------------------
// reasonCodeIsValid
// ---------------------------------------------------------------------------

describe("reasonCodeIsValid", () => {
  it("accepts each canonical reason code", () => {
    for (const code of [
      "server_reinstalled",
      "host_key_rotated",
      "lab_target_recreated",
      "operator_other",
    ]) {
      expect(reasonCodeIsValid(code)).toBe(true);
      // Also confirm the API-layer alias is the same predicate.
      expect(isHostKeyReplacementReasonCode(code)).toBe(true);
    }
  });

  it("rejects values outside the closed accept-list", () => {
    for (const v of [
      "",
      "operator_freeform",
      "SERVER_REINSTALLED",
      "server_reinstalled ",
      " server_reinstalled",
      "server-reinstalled",
    ]) {
      expect(reasonCodeIsValid(v)).toBe(false);
    }
  });

  it("rejects non-string values", () => {
    expect(reasonCodeIsValid(undefined as unknown as string)).toBe(false);
    expect(reasonCodeIsValid(null as unknown as string)).toBe(false);
    expect(reasonCodeIsValid(0 as unknown as string)).toBe(false);
    expect(reasonCodeIsValid({} as unknown as string)).toBe(false);
  });
});

describe("replacementReasonOptions", () => {
  it("returns each canonical reason code with a non-empty label", () => {
    const opts = replacementReasonOptions();
    const codes = opts.map((o) => o.code);
    expect(codes).toEqual([
      "server_reinstalled",
      "host_key_rotated",
      "lab_target_recreated",
      "operator_other",
    ]);
    for (const o of opts) {
      expect(reasonCodeIsValid(o.code)).toBe(true);
      expect(o.label.length).toBeGreaterThan(0);
    }
  });

  it("returns a fresh array on every call (defensive against caller mutation)", () => {
    const a = replacementReasonOptions();
    const b = replacementReasonOptions();
    expect(a).not.toBe(b);
    a[0].label = "mutated";
    expect(replacementReasonOptions()[0].label).not.toBe("mutated");
  });
});

// ---------------------------------------------------------------------------
// replaceConfirmationMatches
// ---------------------------------------------------------------------------

describe("replaceConfirmationMatches", () => {
  it("accepts the byte-exact confirmation token", () => {
    expect(replaceConfirmationMatches("REPLACE")).toBe(true);
  });

  it("rejects any case other than uppercase", () => {
    expect(replaceConfirmationMatches("replace")).toBe(false);
    expect(replaceConfirmationMatches("Replace")).toBe(false);
    expect(replaceConfirmationMatches("RePlAcE")).toBe(false);
  });

  it("rejects whitespace-padded input", () => {
    expect(replaceConfirmationMatches(" REPLACE")).toBe(false);
    expect(replaceConfirmationMatches("REPLACE ")).toBe(false);
    expect(replaceConfirmationMatches("REPLACE\n")).toBe(false);
    expect(replaceConfirmationMatches("\tREPLACE")).toBe(false);
  });

  it("rejects partial matches and empty strings", () => {
    expect(replaceConfirmationMatches("REPLAC")).toBe(false);
    expect(replaceConfirmationMatches("REPLACEE")).toBe(false);
    expect(replaceConfirmationMatches("")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// replaceGateForPreflight
// ---------------------------------------------------------------------------

describe("replaceGateForPreflight", () => {
  it("returns ok with both fingerprints for a well-formed changed preflight", () => {
    expect(replaceGateForPreflight(PREFLIGHT_CHANGED, OLD_FP)).toEqual({
      kind: "ok",
      old_fingerprint: OLD_FP,
      new_fingerprint: NEW_FP,
    });
  });

  it("blocks when the preflight status is unknown", () => {
    expect(
      replaceGateForPreflight(
        { ...PREFLIGHT_CHANGED, host_key_status: "unknown" },
        OLD_FP,
      ),
    ).toEqual({ kind: "not_changed_status" });
  });

  it("blocks when the preflight status is trusted", () => {
    expect(
      replaceGateForPreflight(
        { ...PREFLIGHT_CHANGED, host_key_status: "trusted" },
        OLD_FP,
      ),
    ).toEqual({ kind: "not_changed_status" });
  });

  it("blocks when the active pin fingerprint is missing", () => {
    expect(replaceGateForPreflight(PREFLIGHT_CHANGED, null)).toEqual({
      kind: "missing_active_pin",
    });
    expect(replaceGateForPreflight(PREFLIGHT_CHANGED, "")).toEqual({
      kind: "missing_active_pin",
    });
  });

  it("blocks when the active pin shape is malformed", () => {
    expect(
      replaceGateForPreflight(PREFLIGHT_CHANGED, "not-a-fingerprint"),
    ).toEqual({ kind: "invalid_old_fingerprint_shape" });
    expect(replaceGateForPreflight(PREFLIGHT_CHANGED, "MD5:abcd")).toEqual({
      kind: "invalid_old_fingerprint_shape",
    });
  });

  it("blocks when the captured (new) fingerprint is missing or malformed", () => {
    expect(
      replaceGateForPreflight(
        { ...PREFLIGHT_CHANGED, host_key_fingerprint: "" },
        OLD_FP,
      ),
    ).toEqual({ kind: "invalid_new_fingerprint_shape" });
    expect(
      replaceGateForPreflight(
        { ...PREFLIGHT_CHANGED, host_key_fingerprint: "garbage" },
        OLD_FP,
      ),
    ).toEqual({ kind: "invalid_new_fingerprint_shape" });
  });
});

// ---------------------------------------------------------------------------
// decideReplaceSubmit
// ---------------------------------------------------------------------------

describe("decideReplaceSubmit", () => {
  it("returns ready with the wire request when every gate passes", () => {
    const decision = decideReplaceSubmit(
      PREFLIGHT_CHANGED,
      "server_reinstalled",
      "REPLACE",
    );
    expect(decision).toEqual({
      kind: "ready",
      request: {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "server_reinstalled",
      },
    });
  });

  it("blocks when the preflight is not in changed status", () => {
    expect(
      decideReplaceSubmit(
        { ...PREFLIGHT_CHANGED, host_key_status: "unknown" },
        "server_reinstalled",
        "REPLACE",
      ),
    ).toEqual({ kind: "blocked", reason: "not_changed_status" });
    expect(
      decideReplaceSubmit(
        { ...PREFLIGHT_CHANGED, host_key_status: "trusted" },
        "server_reinstalled",
        "REPLACE",
      ),
    ).toEqual({ kind: "blocked", reason: "not_changed_status" });
  });

  it("blocks when active_pin_fingerprint is missing", () => {
    expect(
      decideReplaceSubmit(
        { ...PREFLIGHT_CHANGED, active_pin_fingerprint: null },
        "server_reinstalled",
        "REPLACE",
      ),
    ).toEqual({ kind: "blocked", reason: "missing_active_pin" });
  });

  it("blocks when the reason code is null or invalid", () => {
    expect(
      decideReplaceSubmit(PREFLIGHT_CHANGED, null, "REPLACE"),
    ).toEqual({ kind: "blocked", reason: "invalid_reason_code" });
    expect(
      decideReplaceSubmit(
        PREFLIGHT_CHANGED,
        "server-reinstalled" as unknown as null,
        "REPLACE",
      ),
    ).toEqual({ kind: "blocked", reason: "invalid_reason_code" });
  });

  it("blocks when the typed confirmation is not exact REPLACE", () => {
    for (const input of ["", "replace", " REPLACE ", "REPLACE\n", "REPLACEx"]) {
      expect(
        decideReplaceSubmit(
          PREFLIGHT_CHANGED,
          "server_reinstalled",
          input,
        ),
      ).toEqual({ kind: "blocked", reason: "confirmation_mismatch" });
    }
  });

  it("checks gates in a stable, helpful order", () => {
    // Status gate fires before reason / confirmation, so a unknown-status
    // preflight + bad inputs surfaces the most useful blocker.
    expect(
      decideReplaceSubmit(
        { ...PREFLIGHT_CHANGED, host_key_status: "unknown" },
        null,
        "",
      ),
    ).toEqual({ kind: "blocked", reason: "not_changed_status" });
  });
});

// ---------------------------------------------------------------------------
// synthesizePostReplacePreflight
// ---------------------------------------------------------------------------

describe("synthesizePostReplacePreflight", () => {
  it("derives a trusted preflight from the original preflight + replacement response", () => {
    const synthetic = synthesizePostReplacePreflight(
      PREFLIGHT_CHANGED,
      RESPONSE_FIXTURE,
    );
    expect(synthetic.profile_id).toBe(PREFLIGHT_CHANGED.profile_id);
    expect(synthetic.host_id).toBe(PREFLIGHT_CHANGED.host_id);
    expect(synthetic.hostname).toBe(PREFLIGHT_CHANGED.hostname);
    expect(synthetic.port).toBe(PREFLIGHT_CHANGED.port);
    // The captured-key fields advance to the newly-trusted pin.
    expect(synthetic.host_key_status).toBe("trusted");
    expect(synthetic.host_key_type).toBe(RESPONSE_FIXTURE.host_key_type);
    expect(synthetic.host_key_fingerprint).toBe(
      RESPONSE_FIXTURE.trusted_fingerprint,
    );
    // The replace flow is no longer applicable on the synthesized state —
    // there is now nothing to replace.
    expect(synthetic.active_pin_fingerprint).toBeNull();
  });

  it("does not echo private_key / encrypted_private_key / password / cookie / token fields", () => {
    const polluted = {
      ...RESPONSE_FIXTURE,
      private_key: "RELAY_SENTINEL_REPLACE_PRIVATE_KEY_9302",
      encrypted_private_key: "RELAY_SENTINEL_REPLACE_ENCRYPTED_PRIVATE_KEY_9303",
      password: "RELAY_SENTINEL_REPLACE_PASSWORD_9304",
      cookie: "RELAY_SENTINEL_REPLACE_COOKIE_9305",
      session_token: "RELAY_SENTINEL_REPLACE_SESSION_TOKEN_9306",
      token_hash: "RELAY_SENTINEL_REPLACE_TOKEN_HASH_9307",
    } as ReplaceHostKeyResponse;
    const synthetic = synthesizePostReplacePreflight(
      PREFLIGHT_CHANGED,
      polluted,
    );
    const serialised = JSON.stringify(synthetic);
    for (const sentinel of FORBIDDEN_SUBSTRINGS) {
      expect(serialised).not.toContain(sentinel);
    }
  });
});

// ---------------------------------------------------------------------------
// parseReplaceHostKeyResponse
// ---------------------------------------------------------------------------

describe("parseReplaceHostKeyResponse", () => {
  it("accepts a well-formed response", () => {
    expect(parseReplaceHostKeyResponse(RESPONSE_FIXTURE)).toEqual(
      RESPONSE_FIXTURE,
    );
  });

  it("rejects responses missing required string fields", () => {
    for (const key of [
      "profile_id",
      "host_id",
      "revoked_known_host_entry_id",
      "revoked_fingerprint",
      "trusted_known_host_entry_id",
      "trusted_fingerprint",
      "host_key_type",
      "trusted_at",
    ] as const) {
      const bad: Record<string, unknown> = { ...RESPONSE_FIXTURE };
      delete bad[key];
      expect(parseReplaceHostKeyResponse(bad)).toBeNull();
    }
  });

  it("rejects non-string IDs and fingerprints", () => {
    expect(
      parseReplaceHostKeyResponse({
        ...RESPONSE_FIXTURE,
        profile_id: 7 as unknown as string,
      }),
    ).toBeNull();
    expect(
      parseReplaceHostKeyResponse({
        ...RESPONSE_FIXTURE,
        revoked_fingerprint: { oops: 1 } as unknown as string,
      }),
    ).toBeNull();
    expect(
      parseReplaceHostKeyResponse({
        ...RESPONSE_FIXTURE,
        trusted_known_host_entry_id: null as unknown as string,
      }),
    ).toBeNull();
  });

  it("rejects non-object input", () => {
    expect(parseReplaceHostKeyResponse(null)).toBeNull();
    expect(parseReplaceHostKeyResponse("nope")).toBeNull();
    expect(parseReplaceHostKeyResponse(42)).toBeNull();
    expect(parseReplaceHostKeyResponse(undefined)).toBeNull();
  });

  it("does not expose smuggled secret-shaped fields on the parsed object", () => {
    const smuggled: Record<string, unknown> = { ...RESPONSE_FIXTURE };
    for (const s of FORBIDDEN_SUBSTRINGS) {
      smuggled.private_key = s;
      smuggled.encrypted_private_key = s;
      smuggled.password = s;
      smuggled.cookie = s;
      smuggled.session_token = s;
      smuggled.token_hash = s;
    }
    const parsed = parseReplaceHostKeyResponse(smuggled);
    expect(parsed).not.toBeNull();
    if (parsed) {
      const json = JSON.stringify(parsed);
      for (const s of FORBIDDEN_SUBSTRINGS) {
        expect(json).not.toContain(s);
      }
      expect("private_key" in parsed).toBe(false);
      expect("encrypted_private_key" in parsed).toBe(false);
      expect("password" in parsed).toBe(false);
      expect("cookie" in parsed).toBe(false);
      expect("session_token" in parsed).toBe(false);
      expect("token_hash" in parsed).toBe(false);
    }
  });
});

// ---------------------------------------------------------------------------
// replaceHostKey API helper
// ---------------------------------------------------------------------------

describe("replaceHostKey", () => {
  it("posts to the per-profile endpoint with the expected JSON body", async () => {
    const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
    const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init });
      return jsonResponse(200, RESPONSE_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result).toEqual({ ok: true, replacement: RESPONSE_FIXTURE });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(
      `/api/v1/server-profiles/${PROFILE_ID}/replace-host-key`,
    );
    expect(calls[0].init?.method).toBe("POST");
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers["content-type"]).toBe("application/json");
    expect(headers.accept).toBe("application/json");
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({
      expected_old_fingerprint: OLD_FP,
      expected_new_fingerprint: NEW_FP,
      reason_code: "host_key_rotated",
    });
  });

  it("URL-encodes the profile id in the request path", async () => {
    const calls: string[] = [];
    const fetchImpl = (async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse(200, RESPONSE_FIXTURE);
    }) as unknown as typeof fetch;
    await replaceHostKey(
      "weird/id with space",
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "server_reinstalled",
      },
      { fetchImpl },
    );
    expect(calls[0]).toBe(
      "/api/v1/server-profiles/weird%2Fid%20with%20space/replace-host-key",
    );
  });

  it("refuses to dispatch a malformed old fingerprint", async () => {
    let called = 0;
    const fetchImpl = (async () => {
      called += 1;
      return jsonResponse(200, RESPONSE_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: "MD5:nope",
        expected_new_fingerprint: NEW_FP,
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result).toEqual({
      ok: false,
      error: { kind: "validation", reason: "invalid_old_fingerprint_shape" },
    });
    expect(called).toBe(0);
  });

  it("refuses to dispatch a malformed new fingerprint", async () => {
    let called = 0;
    const fetchImpl = (async () => {
      called += 1;
      return jsonResponse(200, RESPONSE_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: "garbage",
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result).toEqual({
      ok: false,
      error: { kind: "validation", reason: "invalid_new_fingerprint_shape" },
    });
    expect(called).toBe(0);
  });

  it("refuses to dispatch an unknown reason code", async () => {
    let called = 0;
    const fetchImpl = (async () => {
      called += 1;
      return jsonResponse(200, RESPONSE_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "operator_freeform" as unknown as "operator_other",
      },
      { fetchImpl },
    );
    expect(result).toEqual({
      ok: false,
      error: { kind: "validation", reason: "invalid_reason_code" },
    });
    expect(called).toBe(0);
  });

  it("classifies known 409 conflict reasons from the wire message", async () => {
    const wireReasons: Array<{
      message: string;
      expected:
        | "active_pin_mismatch"
        | "captured_unchanged"
        | "captured_mismatch"
        | "captured_revoked"
        | "new_fingerprint_already_active"
        | "profile_disabled";
    }> = [
      { message: "host_key active_pin_mismatch", expected: "active_pin_mismatch" },
      { message: "host_key captured_unchanged", expected: "captured_unchanged" },
      { message: "host_key captured_mismatch", expected: "captured_mismatch" },
      { message: "host_key captured_revoked", expected: "captured_revoked" },
      {
        message: "host_key new_fingerprint_already_active",
        expected: "new_fingerprint_already_active",
      },
      { message: "server_profile disabled", expected: "profile_disabled" },
    ];
    for (const { message, expected } of wireReasons) {
      const fetchImpl = (async () =>
        jsonResponse(409, {
          error: { code: "conflict", message },
        })) as unknown as typeof fetch;
      const result = await replaceHostKey(
        PROFILE_ID,
        {
          expected_old_fingerprint: OLD_FP,
          expected_new_fingerprint: NEW_FP,
          reason_code: "host_key_rotated",
        },
        { fetchImpl },
      );
      expect(result.ok).toBe(false);
      if (!result.ok && result.error.kind === "http") {
        expect(result.error.status).toBe(409);
        expect(result.error.reason).toBe(expected);
      } else {
        expect.fail(`expected http error for ${message}`);
      }
    }
  });

  it("collapses an unrecognised 409 wire message to reason=null", async () => {
    const fetchImpl = (async () =>
      jsonResponse(409, {
        error: {
          code: "conflict",
          message: `something_new ${SENTINEL_OPERATOR}`,
        },
      })) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBeNull();
      expect(describeReplaceHostKeyError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
  });

  it("maps a 400 envelope to a typed http error with reason=null", async () => {
    const fetchImpl = (async () =>
      jsonResponse(400, {
        error: {
          code: "invalid_input",
          message: `expected_old_fingerprint ${SENTINEL_OPERATOR}`,
        },
      })) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(400);
      expect(result.error.code).toBe("invalid_input");
      expect(result.error.reason).toBeNull();
      expect(describeReplaceHostKeyError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
  });

  it("maps 401 / 403 / 404 / 502 / 503 envelopes without echoing operator detail", async () => {
    const cases: Array<{ status: number; code: string }> = [
      { status: 401, code: "unauthorized" },
      { status: 403, code: "csrf_origin_mismatch" },
      { status: 404, code: "not_found" },
      { status: 502, code: "bad_gateway" },
      { status: 503, code: "service_unavailable" },
    ];
    for (const { status, code } of cases) {
      const fetchImpl = (async () =>
        jsonResponse(status, {
          error: { code, message: SENTINEL_OPERATOR },
        })) as unknown as typeof fetch;
      const result = await replaceHostKey(
        PROFILE_ID,
        {
          expected_old_fingerprint: OLD_FP,
          expected_new_fingerprint: NEW_FP,
          reason_code: "host_key_rotated",
        },
        { fetchImpl },
      );
      expect(result.ok).toBe(false);
      if (!result.ok && result.error.kind === "http") {
        expect(result.error.status).toBe(status);
        expect(result.error.code).toBe(code);
        expect(describeReplaceHostKeyError(result.error)).not.toContain(
          SENTINEL_OPERATOR,
        );
      } else {
        expect.fail(`expected http error for ${status}`);
      }
    }
  });

  it("collapses an unparseable success body to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, { not_a_replace: true })) as unknown as typeof fetch;
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("maps a transport rejection to a typed transport error and stays silent", async () => {
    const fetchImpl = (async () => {
      throw new Error(`network ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const result = await replaceHostKey(
      PROFILE_ID,
      {
        expected_old_fingerprint: OLD_FP,
        expected_new_fingerprint: NEW_FP,
        reason_code: "host_key_rotated",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      expect(describeReplaceHostKeyError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected transport error");
    }
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------------
// describeReplaceHostKeyError
// ---------------------------------------------------------------------------

describe("describeReplaceHostKeyError", () => {
  it("renders distinct copy per 409 reason and never echoes the wire message", () => {
    const reasons: Array<{
      reason:
        | "active_pin_mismatch"
        | "captured_unchanged"
        | "captured_mismatch"
        | "captured_revoked"
        | "new_fingerprint_already_active"
        | "profile_disabled";
      mustContain: string;
    }> = [
      { reason: "active_pin_mismatch", mustContain: "no longer matches" },
      { reason: "captured_unchanged", mustContain: "did not actually change" },
      { reason: "captured_mismatch", mustContain: "differs from what you confirmed" },
      { reason: "captured_revoked", mustContain: "revoked previously" },
      {
        reason: "new_fingerprint_already_active",
        mustContain: "another operator",
      },
      { reason: "profile_disabled", mustContain: "disabled" },
    ];
    const seen = new Set<string>();
    for (const { reason, mustContain } of reasons) {
      const out = describeReplaceHostKeyError({
        kind: "http",
        status: 409,
        code: "conflict",
        message: `host_key conflict ${SENTINEL_OPERATOR}`,
        reason,
      });
      expect(out).not.toContain(SENTINEL_OPERATOR);
      expect(out.toLowerCase()).toContain(mustContain.toLowerCase());
      seen.add(out);
    }
    // All six branches produce DISTINCT strings — the SPA gets a real
    // discriminator out of the formatter.
    expect(seen.size).toBe(reasons.length);
  });

  it("falls back to a generic 409 when the wire reason is unrecognised", () => {
    const out = describeReplaceHostKeyError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `host_key something_new ${SENTINEL_OPERATOR}`,
      reason: null,
    });
    expect(out).not.toContain(SENTINEL_OPERATOR);
    expect(out).toBe("Replace refused: HTTP 409 conflict");
  });

  it("formats validation refusals with stable copy", () => {
    expect(
      describeReplaceHostKeyError({
        kind: "validation",
        reason: "invalid_old_fingerprint_shape",
      }),
    ).toBe("Cannot replace host key: old fingerprint shape is invalid");
    expect(
      describeReplaceHostKeyError({
        kind: "validation",
        reason: "invalid_new_fingerprint_shape",
      }),
    ).toBe("Cannot replace host key: new fingerprint shape is invalid");
    expect(
      describeReplaceHostKeyError({
        kind: "validation",
        reason: "invalid_reason_code",
      }),
    ).toBe("Cannot replace host key: reason code is not recognised");
  });

  it("formats 400 / 401 / 403 / 404 / 502 / 503 with safe per-status copy", () => {
    expect(
      describeReplaceHostKeyError({
        kind: "http",
        status: 400,
        code: "invalid_input",
        message: SENTINEL_OPERATOR,
        reason: null,
      }),
    ).toBe("Replace refused: backend rejected the request shape");

    expect(
      describeReplaceHostKeyError({
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: SENTINEL_OPERATOR,
        reason: null,
      }),
    ).toBe("Replace refused: not authenticated");

    expect(
      describeReplaceHostKeyError({
        kind: "http",
        status: 403,
        code: "csrf_origin_mismatch",
        message: SENTINEL_OPERATOR,
        reason: null,
      }),
    ).toBe("Replace refused: request blocked by browser security policy");

    expect(
      describeReplaceHostKeyError({
        kind: "http",
        status: 404,
        code: "not_found",
        message: SENTINEL_OPERATOR,
        reason: null,
      }),
    ).toBe("Replace refused: server profile not found");

    expect(
      describeReplaceHostKeyError({
        kind: "http",
        status: 502,
        code: "bad_gateway",
        message: SENTINEL_OPERATOR,
        reason: null,
      }).toLowerCase(),
    ).toContain("could not re-probe");

    expect(
      describeReplaceHostKeyError({
        kind: "http",
        status: 503,
        code: "service_unavailable",
        message: SENTINEL_OPERATOR,
        reason: null,
      }).toLowerCase(),
    ).toContain("vault");
  });

  it("never echoes operator detail for any HTTP status", () => {
    const statuses: Array<ReplaceHostKeyError> = [
      { kind: "http", status: 400, code: "invalid_input", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 401, code: "unauthorized", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 403, code: "csrf_origin_mismatch", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 404, code: "not_found", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 418, code: "teapot", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 500, code: "internal_error", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 502, code: "bad_gateway", message: SENTINEL_OPERATOR, reason: null },
      { kind: "http", status: 503, code: "service_unavailable", message: SENTINEL_OPERATOR, reason: null },
    ];
    for (const err of statuses) {
      expect(describeReplaceHostKeyError(err)).not.toContain(SENTINEL_OPERATOR);
    }
  });

  it("never echoes a transport error's thrown message", () => {
    expect(
      describeReplaceHostKeyError({
        kind: "transport",
        message: `boom ${SENTINEL_OPERATOR}`,
      }),
    ).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats malformed_response without exposing data", () => {
    expect(
      describeReplaceHostKeyError({ kind: "malformed_response" }),
    ).toBe("Replace refused: malformed response");
  });
});

// ---------------------------------------------------------------------------
// Sentinel: no formatter or helper emits forbidden secret-shaped substrings
// even when present in inputs / wire envelopes.
// ---------------------------------------------------------------------------

describe("redaction sentinel — replace-host-key surface", () => {
  it("the formatter never echoes private_key / password / cookie / token sentinels even if smuggled into the wire message", () => {
    for (const s of FORBIDDEN_SUBSTRINGS) {
      const out = describeReplaceHostKeyError({
        kind: "http",
        status: 409,
        code: "conflict",
        message: `host_key active_pin_mismatch ${s}`,
        reason: "active_pin_mismatch",
      });
      expect(out).not.toContain(s);
    }
  });

  it("the API helper does not surface secret-shaped strings on a transport rejection", async () => {
    for (const s of FORBIDDEN_SUBSTRINGS) {
      const fetchImpl = (async () => {
        throw new Error(`some ${s} surface`);
      }) as unknown as typeof fetch;
      const result = await replaceHostKey(
        PROFILE_ID,
        {
          expected_old_fingerprint: OLD_FP,
          expected_new_fingerprint: NEW_FP,
          reason_code: "host_key_rotated",
        },
        { fetchImpl },
      );
      expect(result.ok).toBe(false);
      if (!result.ok && result.error.kind === "transport") {
        // The typed error still preserves message for programmatic
        // callers; the formatter is responsible for redaction.
        expect(describeReplaceHostKeyError(result.error)).not.toContain(s);
      } else {
        expect.fail("expected transport error");
      }
    }
  });
});
