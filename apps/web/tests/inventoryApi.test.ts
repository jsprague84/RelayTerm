import { describe, expect, it, vi } from "vitest";
import {
  describeLoadError,
  fetchJsonList,
  readErrorEnvelope,
  type LoadError,
} from "../src/lib/api/apiErrors.js";
import { listHosts, parseHost, type Host } from "../src/lib/api/hosts.js";
import {
  listServerProfiles,
  parseServerProfile,
  resolveProfileLinks,
  type ServerProfile,
} from "../src/lib/api/serverProfiles.js";
import {
  createSshIdentity,
  describeCreateSshIdentityError,
  listSshIdentities,
  MAX_IDENTITY_NAME_LEN,
  parseSshIdentity,
  publicKeyPreview,
  SUPPORTED_GENERATION_KEY_TYPES,
  validateCreateSshIdentityRequest,
  type SshIdentity,
} from "../src/lib/api/sshIdentities.js";

/**
 * Sentinels that MUST NEVER appear in user-visible UI strings, parsed
 * DTOs, or formatted summaries. The redaction rule for the inventory
 * surface (mirrors the existing `terminalSessionsApi.test.ts` rule):
 *  - Operator detail in 4xx envelopes does not reach the formatted
 *    summary string.
 *  - Transport `Error.message` does not reach the formatted summary.
 *  - `private_key` / `encrypted_private_key` fields do not reach the
 *    parsed SSH identity DTO or any formatted preview.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_INVENTORY_OPERATOR_DETAIL_8821";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_BYTES_8822";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

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
  tags: ["prod", "edge"],
  created_at: "2026-04-29T00:00:00Z",
  updated_at: "2026-04-29T00:00:00Z",
  last_connected_at: null,
  disabled_at: null,
};

const IDENTITY_FIXTURE: SshIdentity = {
  id: "33333333-3333-3333-3333-333333333333",
  name: "primary",
  key_type: "ed25519",
  public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleSSHPublicKey relay@example",
  fingerprint_sha256: "SHA256:abcdefg",
  created_at: "2026-04-29T00:00:00Z",
  last_used_at: null,
};

describe("readErrorEnvelope", () => {
  it("extracts only `code` and `message`, drops sibling fields", async () => {
    const response = jsonResponse(409, {
      error: {
        code: "conflict",
        message: "host_key conflict",
        operator_detail: SENTINEL_OPERATOR,
      },
    });
    const env = await readErrorEnvelope(response);
    expect(env).toEqual({ code: "conflict", message: "host_key conflict" });
    expect(JSON.stringify(env)).not.toContain(SENTINEL_OPERATOR);
  });

  it("falls back to status text when the body is not JSON", async () => {
    const response = new Response("not json", {
      status: 500,
      statusText: "Internal Server Error",
    });
    const env = await readErrorEnvelope(response);
    expect(env).toEqual({
      code: "unknown_error",
      message: "Internal Server Error",
    });
  });

  it("falls back when the body is JSON but does not match the envelope shape", async () => {
    const response = jsonResponse(500, {
      operator_detail: SENTINEL_OPERATOR,
      not_an_error_envelope: true,
    });
    const env = await readErrorEnvelope(response);
    expect(env.code).toBe("unknown_error");
    expect(env.message).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("describeLoadError", () => {
  it("formats each error kind as a function of kind+status+code", () => {
    expect(
      describeLoadError("hosts", {
        kind: "http",
        status: 503,
        code: "service_unavailable",
        message: SENTINEL_OPERATOR,
      } satisfies LoadError),
    ).toBe("Failed to load hosts: HTTP 503 service_unavailable");
    expect(
      describeLoadError("server profiles", {
        kind: "transport",
        message: `boom ${SENTINEL_OPERATOR}`,
      }),
    ).toBe("Failed to load server profiles: transport error");
    expect(
      describeLoadError("SSH identities", {
        kind: "malformed_response",
      }),
    ).toBe("Failed to load SSH identities: malformed response");
  });

  it("never echoes the wire `message` of an http error", () => {
    const summary = describeLoadError("hosts", {
      kind: "http",
      status: 502,
      code: "bad_gateway",
      message: SENTINEL_OPERATOR,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("never echoes a transport error's thrown message", () => {
    const summary = describeLoadError("hosts", {
      kind: "transport",
      message: `request to https://example.com/${SENTINEL_OPERATOR}`,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
    expect(summary).not.toContain("https://");
  });
});

describe("fetchJsonList", () => {
  it("returns parsed items on a 2xx body", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [HOST_FIXTURE])) as unknown as typeof fetch;
    const result = await fetchJsonList<Host>("/api/v1/hosts", parseHost, {
      fetchImpl,
    });
    expect(result).toEqual({ ok: true, data: [HOST_FIXTURE] });
  });

  it("collapses any unparseable item to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [
        HOST_FIXTURE,
        { id: "broken" /* missing fields */ },
      ])) as unknown as typeof fetch;
    const result = await fetchJsonList<Host>("/api/v1/hosts", parseHost, {
      fetchImpl,
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({ kind: "malformed_response" });
    }
  });

  it("flags a non-array success body", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, { not: "an array" })) as unknown as typeof fetch;
    const result = await fetchJsonList<Host>("/api/v1/hosts", parseHost, {
      fetchImpl,
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({ kind: "malformed_response" });
    }
  });

  it("maps a 4xx error envelope to a typed http error", async () => {
    const fetchImpl = (async () =>
      jsonResponse(403, {
        error: {
          code: "forbidden",
          message: "owner mismatch",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await fetchJsonList<Host>("/api/v1/hosts", parseHost, {
      fetchImpl,
    });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(403);
      expect(result.error.code).toBe("forbidden");
      expect(result.error.message).toBe("owner mismatch");
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
    } else {
      expect.fail("expected http error");
    }
  });

  it("maps a transport rejection to a typed transport error", async () => {
    const fetchImpl = (async () => {
      throw new Error(`network ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    const result = await fetchJsonList<Host>("/api/v1/hosts", parseHost, {
      fetchImpl,
    });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      // The typed error preserves Error.message for programmatic
      // callers, BUT describeLoadError must redact it. The redaction
      // posture is enforced at the formatter, not the typed error.
      expect(describeLoadError("hosts", result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected transport error");
    }
  });

  it("does not log raw response bodies on success", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [HOST_FIXTURE])) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    await fetchJsonList<Host>("/api/v1/hosts", parseHost, { fetchImpl });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });

  it("does not log on transport failure", async () => {
    const fetchImpl = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    await fetchJsonList<Host>("/api/v1/hosts", parseHost, { fetchImpl });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
  });
});

describe("parseHost", () => {
  it("accepts a well-formed host", () => {
    expect(parseHost(HOST_FIXTURE)).toEqual(HOST_FIXTURE);
  });

  it("ignores unknown extra fields", () => {
    const parsed = parseHost({
      ...HOST_FIXTURE,
      future_safe_field: "ignored",
    });
    expect(parsed).toEqual(HOST_FIXTURE);
  });

  it("rejects out-of-range ports", () => {
    expect(parseHost({ ...HOST_FIXTURE, port: 0 })).toBeNull();
    expect(parseHost({ ...HOST_FIXTURE, port: 65536 })).toBeNull();
    expect(parseHost({ ...HOST_FIXTURE, port: 22.5 })).toBeNull();
  });

  it("rejects missing required fields", () => {
    const { hostname: _hostname, ...rest } = HOST_FIXTURE;
    expect(parseHost(rest)).toBeNull();
  });
});

describe("listHosts", () => {
  it("targets /api/v1/hosts", async () => {
    let captured = "";
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = String(input);
      return jsonResponse(200, [HOST_FIXTURE]);
    }) as unknown as typeof fetch;
    await listHosts({ fetchImpl });
    expect(captured).toBe("/api/v1/hosts");
  });
});

describe("parseServerProfile", () => {
  it("accepts a profile with a username override and last_connected_at", () => {
    const raw = {
      ...PROFILE_FIXTURE,
      username_override: "ops",
      last_connected_at: "2026-04-30T00:00:00Z",
    };
    const parsed = parseServerProfile(raw);
    expect(parsed?.username_override).toBe("ops");
    expect(parsed?.last_connected_at).toBe("2026-04-30T00:00:00Z");
  });

  it("rejects non-string tag entries", () => {
    expect(
      parseServerProfile({ ...PROFILE_FIXTURE, tags: ["ok", 42] }),
    ).toBeNull();
  });

  it("ignores unknown extra fields", () => {
    expect(
      parseServerProfile({
        ...PROFILE_FIXTURE,
        future_safe_field: SENTINEL_OPERATOR,
      }),
    ).toEqual(PROFILE_FIXTURE);
  });

  it("accepts a disabled_at timestamp and surfaces it on the parsed object", () => {
    const raw = { ...PROFILE_FIXTURE, disabled_at: "2026-05-01T12:34:56Z" };
    const parsed = parseServerProfile(raw);
    expect(parsed?.disabled_at).toBe("2026-05-01T12:34:56Z");
  });

  it("collapses a missing disabled_at to null for forward compatibility", () => {
    // The API always emits the field today, but a future server build that
    // momentarily omits it (e.g. an older deploy during a rolling upgrade)
    // must not crash the parser. Older builds also predate the field
    // entirely; both shapes collapse to "enabled" at the parser level.
    const raw: Record<string, unknown> = { ...PROFILE_FIXTURE };
    delete raw.disabled_at;
    expect(parseServerProfile(raw)?.disabled_at).toBeNull();
  });

  it("rejects a wrong-shape disabled_at (e.g. boolean) to prevent silent drift", () => {
    expect(
      parseServerProfile({ ...PROFILE_FIXTURE, disabled_at: true }),
    ).toBeNull();
  });

  it("never copies private_key / encrypted_private_key onto the parsed profile", () => {
    // Defence-in-depth sentinel: the server must never put key material on
    // a server_profile response. If a future server bug sneaks one in, the
    // field-by-field parser pattern silently drops it. Asserting absence
    // here pins the contract so a future refactor that copies `r` whole
    // would surface as a clear test failure, not a silent leak.
    const REDACT = "REDACT-MARKER-FE-DISABLED-AT-9F2B";
    const raw = {
      ...PROFILE_FIXTURE,
      private_key: REDACT,
      encrypted_private_key: REDACT,
    };
    const parsed = parseServerProfile(raw);
    expect(parsed).not.toBeNull();
    expect(parsed as unknown as Record<string, unknown>).not.toHaveProperty(
      "private_key",
    );
    expect(parsed as unknown as Record<string, unknown>).not.toHaveProperty(
      "encrypted_private_key",
    );
    expect(JSON.stringify(parsed)).not.toContain(REDACT);
  });
});

describe("listServerProfiles", () => {
  it("targets /api/v1/server-profiles", async () => {
    let captured = "";
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = String(input);
      return jsonResponse(200, [PROFILE_FIXTURE]);
    }) as unknown as typeof fetch;
    await listServerProfiles({ fetchImpl });
    expect(captured).toBe("/api/v1/server-profiles");
  });
});

describe("resolveProfileLinks", () => {
  it("inherits the host's default_username when no override is set", () => {
    const links = resolveProfileLinks(PROFILE_FIXTURE, [HOST_FIXTURE]);
    expect(links.host).toEqual(HOST_FIXTURE);
    expect(links.effectiveUsername).toBe(HOST_FIXTURE.default_username);
    expect(links.inheritedFromHost).toBe(true);
  });

  it("uses the override and reports inherited=false", () => {
    const links = resolveProfileLinks(
      { ...PROFILE_FIXTURE, username_override: "ops" },
      [HOST_FIXTURE],
    );
    expect(links.effectiveUsername).toBe("ops");
    expect(links.inheritedFromHost).toBe(false);
  });

  it("returns nulls when the host link cannot be resolved AND there is no override", () => {
    const links = resolveProfileLinks(PROFILE_FIXTURE, []);
    expect(links.host).toBeNull();
    expect(links.effectiveUsername).toBeNull();
    expect(links.inheritedFromHost).toBeNull();
  });

  it("still surfaces the override even when the host is unresolvable", () => {
    const links = resolveProfileLinks(
      { ...PROFILE_FIXTURE, username_override: "ops" },
      [],
    );
    expect(links.host).toBeNull();
    expect(links.effectiveUsername).toBe("ops");
    expect(links.inheritedFromHost).toBe(false);
  });
});

describe("parseSshIdentity", () => {
  it("accepts a well-formed identity", () => {
    expect(parseSshIdentity(IDENTITY_FIXTURE)).toEqual(IDENTITY_FIXTURE);
  });

  it("rejects unknown key types", () => {
    expect(
      parseSshIdentity({ ...IDENTITY_FIXTURE, key_type: "garbage" }),
    ).toBeNull();
  });

  it("redaction sentinel: drops `encrypted_private_key` from the parsed object", () => {
    // A backend bug (or a future test fixture pulled from disk) might
    // accidentally include the raw encrypted bytes. The parser must
    // construct the DTO field-by-field so the byte string cannot
    // smuggle through onto the parsed object — even if it is present
    // on the input record.
    const raw = {
      ...IDENTITY_FIXTURE,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
    };
    const parsed = parseSshIdentity(raw);
    expect(parsed).not.toBeNull();
    expect(JSON.stringify(parsed)).not.toContain(SENTINEL_PRIVATE_KEY);
    // Defense-in-depth: the parsed object must not carry these keys
    // at runtime, even as `undefined`.
    expect(
      Object.prototype.hasOwnProperty.call(parsed, "encrypted_private_key"),
    ).toBe(false);
    expect(
      Object.prototype.hasOwnProperty.call(parsed, "private_key"),
    ).toBe(false);
  });

  it("ignores unknown extra fields", () => {
    expect(
      parseSshIdentity({
        ...IDENTITY_FIXTURE,
        future_safe_field: "ok",
      }),
    ).toEqual(IDENTITY_FIXTURE);
  });
});

describe("listSshIdentities", () => {
  it("targets /api/v1/ssh-identities", async () => {
    let captured = "";
    const fetchImpl = (async (input: string | URL | Request) => {
      captured = String(input);
      return jsonResponse(200, [IDENTITY_FIXTURE]);
    }) as unknown as typeof fetch;
    await listSshIdentities({ fetchImpl });
    expect(captured).toBe("/api/v1/ssh-identities");
  });

  it("redaction sentinel: a wire body containing private-key fields parses without leaking", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, [
        {
          ...IDENTITY_FIXTURE,
          encrypted_private_key: SENTINEL_PRIVATE_KEY,
          private_key: SENTINEL_PRIVATE_KEY,
        },
      ])) as unknown as typeof fetch;
    const result = await listSshIdentities({ fetchImpl });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(JSON.stringify(result.data)).not.toContain(SENTINEL_PRIVATE_KEY);
    }
  });
});

describe("validateCreateSshIdentityRequest", () => {
  it("accepts a well-formed request with a default key type", () => {
    const result = validateCreateSshIdentityRequest({ name: "primary" });
    expect(result).toEqual({
      ok: true,
      body: { name: "primary", key_type: "ed25519" },
    });
  });

  it("accepts an explicit ed25519 key type", () => {
    const result = validateCreateSshIdentityRequest({
      name: "primary",
      key_type: "ed25519",
    });
    expect(result).toEqual({
      ok: true,
      body: { name: "primary", key_type: "ed25519" },
    });
  });

  it("rejects an empty or whitespace-only name", () => {
    expect(validateCreateSshIdentityRequest({ name: "" })).toEqual({
      ok: false,
      reason: "missing_name",
    });
    expect(validateCreateSshIdentityRequest({ name: "    " })).toEqual({
      ok: false,
      reason: "missing_name",
    });
  });

  it("rejects surrounding whitespace (mirrors the backend's rule)", () => {
    expect(validateCreateSshIdentityRequest({ name: " primary" })).toEqual({
      ok: false,
      reason: "name_has_surrounding_whitespace",
    });
    expect(validateCreateSshIdentityRequest({ name: "primary " })).toEqual({
      ok: false,
      reason: "name_has_surrounding_whitespace",
    });
  });

  it("rejects names longer than MAX_IDENTITY_NAME_LEN", () => {
    expect(
      validateCreateSshIdentityRequest({
        name: "a".repeat(MAX_IDENTITY_NAME_LEN + 1),
      }),
    ).toEqual({ ok: false, reason: "name_too_long" });
  });

  it("rejects control characters", () => {
    // C0 (U+0000, U+001F) and C1 (U+007F DEL, U+0085 NEL).
    for (const ch of ["\u0000", "\u001F", "\u007F", "\u0085"]) {
      expect(
        validateCreateSshIdentityRequest({ name: `bad${ch}name` }),
      ).toEqual({ ok: false, reason: "name_has_control_chars" });
    }
  });

  it("rejects key types outside SUPPORTED_GENERATION_KEY_TYPES", () => {
    // Wire-stable types that the vault cannot generate today must not
    // smuggle through the client validator. The backend would 400 with
    // `unsupported key_type` — we refuse locally so the UI can show a
    // clean message without burning a request.
    for (const tag of ["rsa", "ecdsa_p256", "ecdsa_p384", "ecdsa_p521"] as const) {
      expect(
        validateCreateSshIdentityRequest({ name: "p", key_type: tag }),
      ).toEqual({ ok: false, reason: "unsupported_key_type" });
    }
  });

  it("SUPPORTED_GENERATION_KEY_TYPES contains exactly ed25519 today", () => {
    // Pinning the intersection: surface a compile/test break the moment
    // a UI option drifts ahead of (or behind) the vault's generators.
    expect(SUPPORTED_GENERATION_KEY_TYPES).toEqual(["ed25519"]);
  });
});

describe("describeCreateSshIdentityError", () => {
  it("describes each kind without echoing wire / transport detail", () => {
    expect(
      describeCreateSshIdentityError({
        kind: "validation",
        reason: "missing_name",
      }),
    ).toBe("Cannot generate SSH identity: name is required");
    expect(
      describeCreateSshIdentityError({
        kind: "validation",
        reason: "name_too_long",
      }),
    ).toContain(`${MAX_IDENTITY_NAME_LEN} characters`);
    expect(
      describeCreateSshIdentityError({
        kind: "http",
        status: 400,
        code: "invalid_input",
        message: SENTINEL_OPERATOR,
      }),
    ).toBe("Failed to generate SSH identity: HTTP 400 invalid_input");
    expect(
      describeCreateSshIdentityError({
        kind: "transport",
        message: `boom ${SENTINEL_OPERATOR}`,
      }),
    ).toBe("Failed to generate SSH identity: transport error");
    expect(
      describeCreateSshIdentityError({ kind: "malformed_response" }),
    ).toBe("Failed to generate SSH identity: malformed response");
  });

  it("collapses 503 service_unavailable to a vault-not-configured hint", () => {
    expect(
      describeCreateSshIdentityError({
        kind: "http",
        status: 503,
        code: "service_unavailable",
        message: SENTINEL_OPERATOR,
      }),
    ).toBe("Cannot generate SSH identity: backend vault is not configured");
  });

  it("never echoes operator detail in any output", () => {
    const cases = [
      describeCreateSshIdentityError({
        kind: "http",
        status: 502,
        code: "bad_gateway",
        message: SENTINEL_OPERATOR,
      }),
      describeCreateSshIdentityError({
        kind: "transport",
        message: `request to https://example.com/${SENTINEL_OPERATOR}`,
      }),
      describeCreateSshIdentityError({
        kind: "http",
        status: 503,
        code: "service_unavailable",
        message: SENTINEL_OPERATOR,
      }),
    ];
    for (const summary of cases) {
      expect(summary).not.toContain(SENTINEL_OPERATOR);
      expect(summary).not.toContain("https://");
    }
  });
});

describe("createSshIdentity", () => {
  it("targets POST /api/v1/ssh-identities with the validated body", async () => {
    const captured: Array<{ url: string; init: RequestInit | undefined }> = [];
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured.push({ url: String(input), init });
      return jsonResponse(201, IDENTITY_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await createSshIdentity(
      { name: "primary", key_type: "ed25519" },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    expect(captured).toHaveLength(1);
    expect(captured[0].url).toBe("/api/v1/ssh-identities");
    expect(captured[0].init?.method).toBe("POST");
    expect(JSON.parse(String(captured[0].init?.body))).toEqual({
      name: "primary",
      key_type: "ed25519",
    });
  });

  it("refuses an invalid request before issuing a wire round-trip", async () => {
    let calls = 0;
    const fetchImpl = (async () => {
      calls += 1;
      return jsonResponse(201, IDENTITY_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await createSshIdentity({ name: "  " }, { fetchImpl });
    expect(result.ok).toBe(false);
    expect(calls).toBe(0);
    if (!result.ok && result.error.kind === "validation") {
      expect(result.error.reason).toBe("missing_name");
    } else {
      expect.fail("expected validation error");
    }
  });

  it("redaction sentinel: a 201 response with private-key fields parses without leaking", async () => {
    const fetchImpl = (async () =>
      jsonResponse(201, {
        ...IDENTITY_FIXTURE,
        encrypted_private_key: SENTINEL_PRIVATE_KEY,
        private_key: SENTINEL_PRIVATE_KEY,
      })) as unknown as typeof fetch;
    const result = await createSshIdentity(
      { name: "primary" },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(JSON.stringify(result.identity)).not.toContain(
        SENTINEL_PRIVATE_KEY,
      );
      expect(
        Object.prototype.hasOwnProperty.call(
          result.identity,
          "encrypted_private_key",
        ),
      ).toBe(false);
      expect(
        Object.prototype.hasOwnProperty.call(result.identity, "private_key"),
      ).toBe(false);
    }
  });

  it("maps a 4xx envelope to a typed http error (operator detail dropped)", async () => {
    const fetchImpl = (async () =>
      jsonResponse(400, {
        error: {
          code: "invalid_input",
          message: "name must not be empty",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await createSshIdentity(
      { name: "primary" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(400);
      expect(result.error.code).toBe("invalid_input");
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
      // The formatter never echoes the wire `message`.
      expect(describeCreateSshIdentityError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
  });

  it("maps a transport rejection to a typed transport error (formatter redacts)", async () => {
    const fetchImpl = (async () => {
      throw new Error(`network ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    const result = await createSshIdentity(
      { name: "primary" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      expect(describeCreateSshIdentityError(result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected transport error");
    }
  });

  it("collapses an unparseable 2xx body to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(201, {
        id: "broken" /* missing required fields */,
      })) as unknown as typeof fetch;
    const result = await createSshIdentity(
      { name: "primary" },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({ kind: "malformed_response" });
    }
  });

  it("does not log raw response bodies on success or failure", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const successFetch = (async () =>
      jsonResponse(201, IDENTITY_FIXTURE)) as unknown as typeof fetch;
    await createSshIdentity({ name: "primary" }, { fetchImpl: successFetch });
    const httpFetch = (async () =>
      jsonResponse(503, {
        error: { code: "service_unavailable", message: "service unavailable" },
      })) as unknown as typeof fetch;
    await createSshIdentity({ name: "primary" }, { fetchImpl: httpFetch });
    const transportFetch = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    await createSshIdentity(
      { name: "primary" },
      { fetchImpl: transportFetch },
    );
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

describe("publicKeyPreview", () => {
  it("returns algorithm + truncated body with an ellipsis", () => {
    const preview = publicKeyPreview(IDENTITY_FIXTURE.public_key, 8);
    expect(preview.startsWith("ssh-ed25519 ")).toBe(true);
    expect(preview.endsWith("…")).toBe(true);
  });

  it("is a pure function of its argument — does not read other identity fields by side channel", () => {
    // The helper takes a single string. There is no path here that
    // looks up an identity object's fingerprint, encrypted_private_key,
    // or any other field — we pin that with a length budget that fits
    // a fingerprint string so a future "be helpful" change that
    // concatenates the fingerprint into the preview would force the
    // length to grow past the budget. Defense-in-depth on top of the
    // type system (which already disallows the call site).
    const longBody = "A".repeat(64);
    const preview = publicKeyPreview(`ssh-ed25519 ${longBody}`, 24);
    expect(preview.length).toBeLessThan(`ssh-ed25519 `.length + 24 + 2);
    // No SHA256: marker should appear unless it was in the input.
    expect(preview).not.toContain("SHA256:");
  });

  it("returns the full pair when the body is shorter than max", () => {
    expect(publicKeyPreview("ssh-ed25519 AAA", 24)).toBe("ssh-ed25519 AAA");
  });

  it("handles empty input safely", () => {
    expect(publicKeyPreview("")).toBe("");
    expect(publicKeyPreview("   ")).toBe("");
  });

  it("handles a single-token input by truncating", () => {
    expect(publicKeyPreview("alongstringwithoutspace", 8)).toBe(
      "alongstr…",
    );
  });
});
