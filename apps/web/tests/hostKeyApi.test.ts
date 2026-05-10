import { describe, expect, it, vi } from "vitest";
import {
  describePreflightError,
  describeTrustHostKeyError,
  hostKeyPreflight,
  isValidFingerprintShape,
  parseHostKeyPreflightResponse,
  parseTrustHostKeyResponse,
  trustHostKey,
  type HostKeyPreflightResponse,
  type PreflightError,
  type TrustHostKeyError,
  type TrustHostKeyResponse,
} from "../src/lib/api/serverProfiles.js";
import {
  fingerprintConfirmationMatches,
  hostKeyStatusDescription,
  hostKeyStatusLabel,
  PREFLIGHT_DISCLAIMER,
  TRUST_DISCLAIMER,
  trustGateForPreflight,
} from "../src/lib/app/hostKeyTrustState.js";

/**
 * Sentinels MUST NEVER appear in formatted UI strings, parsed DTOs, or
 * helper output. Mirrors the redaction rule in `inventoryApi.test.ts`
 * and `createApi.test.ts`:
 *  - Operator detail in 4xx envelopes does not reach the formatted
 *    summary string.
 *  - Transport `Error.message` does not reach the formatted summary.
 *  - `private_key` / `encrypted_private_key` fields cannot smuggle into
 *    the parsed preflight or trust responses (defense in depth — the
 *    wire shapes don't declare those fields, but the parser builds
 *    field-by-field, so a hostile fixture can't sneak them through).
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_HOSTKEY_OPERATOR_DETAIL_9101";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_HOSTKEY_PRIVATE_KEY_9102";

const PROFILE_ID = "44444444-4444-4444-4444-444444444444";
const HOST_ID = "55555555-5555-5555-5555-555555555555";
const ENTRY_ID = "66666666-6666-6666-6666-666666666666";
// `SHA256:` (7) + 43 base64 chars = standard SHA-256 fingerprint length.
const VALID_FP = "SHA256:abcdefGHIJKLmnopqrstuvwxyz0123456789ABCDEFGHJ";

const PREFLIGHT_FIXTURE: HostKeyPreflightResponse = {
  profile_id: PROFILE_ID,
  host_id: HOST_ID,
  hostname: "edge-1.example.internal",
  port: 22,
  host_key_status: "unknown",
  host_key_type: "ed25519",
  host_key_fingerprint: VALID_FP,
  active_pin_fingerprint: null,
  message: "host key not yet pinned; KEX-stage probe only",
};

// Distinct fingerprint for the active pin in changed-status fixtures —
// the captured fingerprint stays at VALID_FP while the active pin's
// fingerprint differs, mirroring the backend's classification rule.
const VALID_FP_OLD = "SHA256:OLDfpxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

const TRUST_FIXTURE: TrustHostKeyResponse = {
  known_host_entry_id: ENTRY_ID,
  host_id: HOST_ID,
  host_key_type: "ed25519",
  host_key_fingerprint: VALID_FP,
  trusted_at: "2026-04-30T00:00:00Z",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("isValidFingerprintShape", () => {
  it("accepts a well-formed SHA256 fingerprint", () => {
    expect(isValidFingerprintShape(VALID_FP)).toBe(true);
  });

  it("rejects fingerprints without the SHA256: prefix", () => {
    expect(isValidFingerprintShape("MD5:abcd")).toBe(false);
    expect(isValidFingerprintShape("abcdef")).toBe(false);
  });

  it("rejects too-short or too-long fingerprints", () => {
    expect(isValidFingerprintShape("SHA256:")).toBe(false);
    expect(isValidFingerprintShape(`SHA256:${"x".repeat(200)}`)).toBe(false);
  });

  it("rejects whitespace and control characters", () => {
    expect(isValidFingerprintShape("SHA256:ab cd")).toBe(false);
    expect(isValidFingerprintShape("SHA256:ab\ncd")).toBe(false);
    expect(isValidFingerprintShape("SHA256:ab\tcd")).toBe(false);
  });
});

describe("parseHostKeyPreflightResponse", () => {
  it("accepts a well-formed response", () => {
    expect(parseHostKeyPreflightResponse(PREFLIGHT_FIXTURE)).toEqual(
      PREFLIGHT_FIXTURE,
    );
  });

  it("rejects unknown host_key_status values", () => {
    expect(
      parseHostKeyPreflightResponse({
        ...PREFLIGHT_FIXTURE,
        host_key_status: "revoked",
      }),
    ).toBeNull();
    expect(
      parseHostKeyPreflightResponse({
        ...PREFLIGHT_FIXTURE,
        host_key_status: "totally_made_up",
      }),
    ).toBeNull();
  });

  it("rejects responses missing required fields", () => {
    const { hostname: _hostname, ...missingHostname } = PREFLIGHT_FIXTURE;
    expect(parseHostKeyPreflightResponse(missingHostname)).toBeNull();
    const { host_key_fingerprint: _fp, ...missingFp } = PREFLIGHT_FIXTURE;
    expect(parseHostKeyPreflightResponse(missingFp)).toBeNull();
  });

  it("does not expose private_key / encrypted_private_key on the parsed object", () => {
    const parsed = parseHostKeyPreflightResponse({
      ...PREFLIGHT_FIXTURE,
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
    expect(parseHostKeyPreflightResponse(null)).toBeNull();
    expect(parseHostKeyPreflightResponse("nope")).toBeNull();
    expect(parseHostKeyPreflightResponse(42)).toBeNull();
  });

  it("captures active_pin_fingerprint when status is changed", () => {
    const parsed = parseHostKeyPreflightResponse({
      ...PREFLIGHT_FIXTURE,
      host_key_status: "changed",
      active_pin_fingerprint: VALID_FP_OLD,
    });
    expect(parsed).not.toBeNull();
    expect(parsed?.active_pin_fingerprint).toBe(VALID_FP_OLD);
  });

  it("collapses missing active_pin_fingerprint to null (back-compat)", () => {
    // Older server builds will not ship the field. The parser MUST accept
    // such responses and surface the missing field as `null` so the
    // replace flow's gate falls back to `missing_active_pin` cleanly.
    const { active_pin_fingerprint: _omit, ...withoutField } =
      PREFLIGHT_FIXTURE;
    const parsed = parseHostKeyPreflightResponse(withoutField);
    expect(parsed).not.toBeNull();
    expect(parsed?.active_pin_fingerprint).toBeNull();
  });

  it("rejects active_pin_fingerprint values that are neither string nor null", () => {
    expect(
      parseHostKeyPreflightResponse({
        ...PREFLIGHT_FIXTURE,
        active_pin_fingerprint: 42,
      }),
    ).toBeNull();
    expect(
      parseHostKeyPreflightResponse({
        ...PREFLIGHT_FIXTURE,
        active_pin_fingerprint: { fp: VALID_FP_OLD },
      }),
    ).toBeNull();
  });
});

describe("parseTrustHostKeyResponse", () => {
  it("accepts a well-formed response", () => {
    expect(parseTrustHostKeyResponse(TRUST_FIXTURE)).toEqual(TRUST_FIXTURE);
  });

  it("rejects responses missing required fields", () => {
    const { trusted_at: _ts, ...missing } = TRUST_FIXTURE;
    expect(parseTrustHostKeyResponse(missing)).toBeNull();
  });

  it("does not expose private_key / encrypted_private_key on the parsed object", () => {
    const parsed = parseTrustHostKeyResponse({
      ...TRUST_FIXTURE,
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
});

describe("describePreflightError", () => {
  it("never echoes wire `message` of an http error", () => {
    const err: PreflightError = {
      kind: "http",
      status: 502,
      code: "bad_gateway",
      message: SENTINEL_OPERATOR,
    };
    const summary = describePreflightError(err);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("could not reach");
  });

  it("never echoes a transport error's thrown message", () => {
    const err: PreflightError = {
      kind: "transport",
      message: `request failed ${SENTINEL_OPERATOR}`,
    };
    expect(describePreflightError(err)).not.toContain(SENTINEL_OPERATOR);
  });

  it("formats vault-disabled as a precise hint", () => {
    const summary = describePreflightError({
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("vault");
  });

  it("formats not-found as a precise hint", () => {
    const summary = describePreflightError({
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
      describePreflightError({
        kind: "http",
        status: 418,
        code: "teapot",
        message: SENTINEL_OPERATOR,
      }),
    ).toBe("Host-key preflight failed: HTTP 418 teapot");
  });

  it("formats malformed_response without exposing data", () => {
    expect(
      describePreflightError({ kind: "malformed_response" }),
    ).toBe("Host-key preflight failed: malformed response");
  });
});

describe("describeTrustHostKeyError", () => {
  it("collapses 409 conflict to a deliberately conservative message", () => {
    const summary = describeTrustHostKeyError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `host_key conflict ${SENTINEL_OPERATOR}`,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary.toLowerCase()).toContain("re-run preflight");
  });

  it("formats client-side validation refusal", () => {
    const summary = describeTrustHostKeyError({
      kind: "validation",
      reason: "invalid_fingerprint_shape",
    });
    expect(summary).toBe("Cannot trust host key: fingerprint shape is invalid");
  });

  it("never echoes wire `message` for any HTTP status", () => {
    const statuses: Array<TrustHostKeyError> = [
      {
        kind: "http",
        status: 400,
        code: "invalid_input",
        message: SENTINEL_OPERATOR,
      },
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
        status: 502,
        code: "bad_gateway",
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
      expect(describeTrustHostKeyError(err)).not.toContain(SENTINEL_OPERATOR);
    }
  });

  it("never echoes transport detail", () => {
    expect(
      describeTrustHostKeyError({
        kind: "transport",
        message: `boom ${SENTINEL_OPERATOR}`,
      }),
    ).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("hostKeyPreflight", () => {
  it("posts to the per-profile endpoint and parses the response", async () => {
    const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
    const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init });
      return jsonResponse(200, PREFLIGHT_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await hostKeyPreflight(PROFILE_ID, { fetchImpl });
    expect(result).toEqual({ ok: true, preflight: PREFLIGHT_FIXTURE });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe(
      `/api/v1/server-profiles/${PROFILE_ID}/host-key-preflight`,
    );
    expect(calls[0].init?.method).toBe("POST");
  });

  it("URL-encodes the profile id", async () => {
    const calls: string[] = [];
    const fetchImpl = (async (input: RequestInfo | URL) => {
      calls.push(String(input));
      return jsonResponse(200, {
        ...PREFLIGHT_FIXTURE,
        profile_id: "weird/id with space",
      });
    }) as unknown as typeof fetch;
    await hostKeyPreflight("weird/id with space", { fetchImpl });
    expect(calls[0]).toBe(
      "/api/v1/server-profiles/weird%2Fid%20with%20space/host-key-preflight",
    );
  });

  it("maps a 502 envelope to a typed http error WITHOUT logging", async () => {
    const fetchImpl = (async () =>
      jsonResponse(502, {
        error: {
          code: "bad_gateway",
          message: `peer banner ${SENTINEL_OPERATOR}`,
        },
      })) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const result = await hostKeyPreflight(PROFILE_ID, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(502);
      expect(result.error.code).toBe("bad_gateway");
      // The typed error preserves the wire message for programmatic
      // callers; the formatter is responsible for redacting it. Verify
      // the formatter does so.
      expect(describePreflightError(result.error)).not.toContain(
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
      jsonResponse(200, { not_a_preflight: true })) as unknown as typeof fetch;
    const result = await hostKeyPreflight(PROFILE_ID, { fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("maps a transport rejection to a typed transport error", async () => {
    const fetchImpl = (async () => {
      throw new Error(`network ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    const result = await hostKeyPreflight(PROFILE_ID, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      expect(describePreflightError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected transport error");
    }
  });
});

describe("trustHostKey", () => {
  it("sends the expected_fingerprint in the POST body", async () => {
    const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
    const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(input), init });
      return jsonResponse(200, TRUST_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await trustHostKey(PROFILE_ID, VALID_FP, { fetchImpl });
    expect(result).toEqual({ ok: true, trust: TRUST_FIXTURE });
    expect(calls[0].url).toBe(
      `/api/v1/server-profiles/${PROFILE_ID}/trust-host-key`,
    );
    expect(calls[0].init?.method).toBe("POST");
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({
      expected_fingerprint: VALID_FP,
    });
  });

  it("refuses to send a malformed fingerprint without a wire round-trip", async () => {
    let called = 0;
    const fetchImpl = (async () => {
      called += 1;
      return jsonResponse(200, TRUST_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await trustHostKey(PROFILE_ID, "NOT_A_FINGERPRINT", {
      fetchImpl,
    });
    expect(result).toEqual({
      ok: false,
      error: { kind: "validation", reason: "invalid_fingerprint_shape" },
    });
    expect(called).toBe(0);
  });

  it("maps a 409 conflict to a typed http error and the formatter collapses it", async () => {
    const fetchImpl = (async () =>
      jsonResponse(409, {
        error: {
          code: "conflict",
          message: `host_key conflict ${SENTINEL_OPERATOR}`,
        },
      })) as unknown as typeof fetch;
    const result = await trustHostKey(PROFILE_ID, VALID_FP, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(409);
      expect(describeTrustHostKeyError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
  });

  it("collapses an unparseable success body to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, { not_a_trust: true })) as unknown as typeof fetch;
    const result = await trustHostKey(PROFILE_ID, VALID_FP, { fetchImpl });
    expect(result).toEqual({
      ok: false,
      error: { kind: "malformed_response" },
    });
  });

  it("does not log on transport failure", async () => {
    const fetchImpl = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    await trustHostKey(PROFILE_ID, VALID_FP, { fetchImpl });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

describe("hostKeyStatusLabel + description copy", () => {
  it("labels each wire status with conservative copy", () => {
    expect(hostKeyStatusLabel("unknown")).toBe("Not trusted");
    expect(hostKeyStatusLabel("trusted")).toBe("Trusted");
    expect(hostKeyStatusLabel("changed")).toBe("Changed");
  });

  it("describes the unknown status as KEX-only", () => {
    const desc = hostKeyStatusDescription("unknown");
    expect(desc.toLowerCase()).toContain("key exchange");
    expect(desc.toLowerCase()).toContain("verify");
    // Must not imply auth was checked.
    expect(desc.toLowerCase()).not.toContain("authenticated");
    expect(desc.toLowerCase()).not.toContain("logged in");
  });

  it("describes the trusted status without claiming auth has happened yet", () => {
    const desc = hostKeyStatusDescription("trusted");
    expect(desc.toLowerCase()).toContain("matches");
    expect(desc.toLowerCase()).toContain("auth-check");
    // Trust is KEX-only — must not imply public-key auth has been verified.
    expect(desc.toLowerCase()).not.toContain("authenticated successfully");
  });

  it("describes the changed status as a refusal that needs investigation", () => {
    const desc = hostKeyStatusDescription("changed");
    expect(desc.toLowerCase()).toContain("differ");
    expect(desc.toLowerCase()).toContain("man-in-the-middle");
  });
});

describe("trustGateForPreflight", () => {
  it("allows trust for a well-formed unknown result", () => {
    expect(trustGateForPreflight(PREFLIGHT_FIXTURE)).toEqual({ kind: "ok" });
  });

  it("blocks trust when the status is already trusted", () => {
    expect(
      trustGateForPreflight({
        ...PREFLIGHT_FIXTURE,
        host_key_status: "trusted",
      }),
    ).toEqual({ kind: "already_trusted" });
  });

  it("blocks trust when the status is changed", () => {
    expect(
      trustGateForPreflight({
        ...PREFLIGHT_FIXTURE,
        host_key_status: "changed",
      }),
    ).toEqual({ kind: "changed_refused" });
  });

  it("blocks trust when the captured fingerprint is missing", () => {
    expect(
      trustGateForPreflight({
        ...PREFLIGHT_FIXTURE,
        host_key_fingerprint: "",
      }),
    ).toEqual({ kind: "missing_fingerprint" });
  });

  it("blocks trust when the fingerprint shape is invalid", () => {
    expect(
      trustGateForPreflight({
        ...PREFLIGHT_FIXTURE,
        host_key_fingerprint: "not-a-fingerprint",
      }),
    ).toEqual({ kind: "invalid_fingerprint_shape" });
  });
});

describe("fingerprintConfirmationMatches", () => {
  it("requires byte-exact equality", () => {
    expect(fingerprintConfirmationMatches(VALID_FP, VALID_FP)).toBe(true);
  });

  it("treats casing as significant (base64 is case-significant)", () => {
    expect(
      fingerprintConfirmationMatches(VALID_FP, VALID_FP.toLowerCase()),
    ).toBe(false);
  });

  it("rejects an empty captured fingerprint even if confirmation is empty", () => {
    expect(fingerprintConfirmationMatches("", "")).toBe(false);
  });

  it("rejects whitespace-padded confirmation", () => {
    expect(fingerprintConfirmationMatches(VALID_FP, ` ${VALID_FP}`)).toBe(
      false,
    );
    expect(fingerprintConfirmationMatches(VALID_FP, `${VALID_FP} `)).toBe(
      false,
    );
  });

  it("rejects substring matches", () => {
    expect(
      fingerprintConfirmationMatches(VALID_FP, VALID_FP.slice(0, -1)),
    ).toBe(false);
  });
});

describe("static disclaimers", () => {
  it("preflight disclaimer names KEX-only scope and disclaims auth/terminal", () => {
    const t = PREFLIGHT_DISCLAIMER.toLowerCase();
    expect(t).toContain("key exchange");
    expect(t).toContain("does not authenticate");
    expect(t).toContain("does not open a terminal");
  });

  it("trust disclaimer warns about not overwriting changed/revoked keys", () => {
    const t = TRUST_DISCLAIMER.toLowerCase();
    expect(t).toContain("changed");
    expect(t).toContain("revoked");
    expect(t).toContain("automatically");
  });
});
