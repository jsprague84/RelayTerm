import { describe, expect, it, vi } from "vitest";
import {
  createHost,
  describeCreateHostError,
  validateCreateHostRequest,
  MAX_HOST_DISPLAY_NAME_LEN,
  MAX_HOSTNAME_LEN,
  MAX_USERNAME_LEN,
  DEFAULT_SSH_PORT,
  type CreateHostError,
  type Host,
} from "../src/lib/api/hosts.js";
import {
  canSubmitServerProfile,
  createServerProfile,
  describeCreateServerProfileError,
  parseTagsInput,
  validateCreateServerProfileRequest,
  MAX_PROFILE_NAME_LEN,
  MAX_TAG_LEN,
  MAX_TAGS,
  type CreateServerProfileError,
  type ServerProfile,
} from "../src/lib/api/serverProfiles.js";

/**
 * Sentinels that MUST NEVER appear in user-visible UI strings or
 * parsed DTOs. Mirrors the redaction rule in `inventoryApi.test.ts`:
 * the formatter is a function of `kind` + `status` + `code` (and the
 * validation `reason`) only — it never echoes the wire `message` or
 * the thrown `Error.message` of a transport failure.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_CREATE_OPERATOR_DETAIL_9001";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_CREATE_PRIVATE_KEY_9002";

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
  created_at: "2026-04-30T00:00:00Z",
  updated_at: "2026-04-30T00:01:00Z",
};

const PROFILE_FIXTURE: ServerProfile = {
  id: "22222222-2222-2222-2222-222222222222",
  name: "edge-1 prod",
  host_id: HOST_FIXTURE.id,
  ssh_identity_id: "33333333-3333-3333-3333-333333333333",
  username_override: null,
  tags: ["prod"],
  created_at: "2026-04-30T00:00:00Z",
  updated_at: "2026-04-30T00:00:00Z",
  last_connected_at: null,
  disabled_at: null,
};

describe("validateCreateHostRequest", () => {
  it("accepts a well-formed request", () => {
    const result = validateCreateHostRequest({
      display_name: "Bastion (us-east-1)",
      hostname: "bastion.example.internal",
      port: 2222,
      default_username: "deploy",
    });
    expect(result).toEqual({
      ok: true,
      body: {
        display_name: "Bastion (us-east-1)",
        hostname: "bastion.example.internal",
        port: 2222,
        default_username: "deploy",
      },
    });
  });

  it("defaults the port to 22 when omitted", () => {
    const result = validateCreateHostRequest({
      display_name: "Default-port host",
      hostname: "h.example.com",
      default_username: "deploy",
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.body.port).toBe(DEFAULT_SSH_PORT);
    }
  });

  it("rejects an empty or whitespace-only display name", () => {
    expect(
      validateCreateHostRequest({
        display_name: "",
        hostname: "h.example.com",
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "missing_display_name" });
  });

  it("rejects surrounding whitespace on display name", () => {
    expect(
      validateCreateHostRequest({
        display_name: " Bastion",
        hostname: "h.example.com",
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "display_name_has_surrounding_whitespace" });
  });

  it("rejects display names longer than the bound", () => {
    expect(
      validateCreateHostRequest({
        display_name: "a".repeat(MAX_HOST_DISPLAY_NAME_LEN + 1),
        hostname: "h.example.com",
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "display_name_too_long" });
  });

  it("rejects control characters in display name", () => {
    expect(
      validateCreateHostRequest({
        display_name: "bad\nname",
        hostname: "h.example.com",
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "display_name_has_control_chars" });
  });

  it("rejects whitespace inside hostname", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "bad host",
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "hostname_has_whitespace" });
  });

  it("rejects invalid characters in hostname", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "host;rm-rf",
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "hostname_has_invalid_char" });
  });

  it("accepts dotted DNS, IPv4, and bracketed IPv6", () => {
    for (const hostname of [
      "db-1.internal.example.com",
      "10.0.0.5",
      "[2001:db8::1]",
    ]) {
      const result = validateCreateHostRequest({
        display_name: "ok",
        hostname,
        default_username: "deploy",
      });
      expect(result.ok).toBe(true);
    }
  });

  it("rejects hostname over the length bound", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "a".repeat(MAX_HOSTNAME_LEN + 1),
        default_username: "deploy",
      }),
    ).toEqual({ ok: false, reason: "hostname_too_long" });
  });

  it("rejects ports outside 1..=65535 and non-integers", () => {
    for (const port of [0, -1, 65536, 22.5, Number.NaN]) {
      expect(
        validateCreateHostRequest({
          display_name: "ok",
          hostname: "h.example.com",
          port,
          default_username: "deploy",
        }),
      ).toEqual({ ok: false, reason: "port_out_of_range" });
    }
  });

  it("rejects empty username", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "",
      }),
    ).toEqual({ ok: false, reason: "missing_username" });
  });

  it("rejects username with leading digit (mirrors backend BadLeadingChar)", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "1abc",
      }),
    ).toEqual({ ok: false, reason: "username_bad_leading_char" });
  });

  it("rejects username with invalid character (mirrors backend InvalidChar)", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "user@host",
      }),
    ).toEqual({ ok: false, reason: "username_has_invalid_char" });
  });

  it("rejects username over MAX_USERNAME_LEN", () => {
    expect(
      validateCreateHostRequest({
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "a".repeat(MAX_USERNAME_LEN + 1),
      }),
    ).toEqual({ ok: false, reason: "username_too_long" });
  });

  it("accepts the leading-underscore username form", () => {
    const result = validateCreateHostRequest({
      display_name: "ok",
      hostname: "h.example.com",
      default_username: "_systemd",
    });
    expect(result.ok).toBe(true);
  });
});

describe("createHost", () => {
  it("targets POST /api/v1/hosts with the validated body", async () => {
    const captured: Array<{ url: string; init: RequestInit | undefined }> = [];
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured.push({ url: String(input), init });
      return jsonResponse(201, HOST_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await createHost(
      {
        display_name: "edge-1",
        hostname: "edge-1.example.internal",
        port: 22,
        default_username: "deploy",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    expect(captured).toHaveLength(1);
    expect(captured[0].url).toBe("/api/v1/hosts");
    expect(captured[0].init?.method).toBe("POST");
    expect(JSON.parse(String(captured[0].init?.body))).toEqual({
      display_name: "edge-1",
      hostname: "edge-1.example.internal",
      port: 22,
      default_username: "deploy",
    });
  });

  it("refuses an invalid request before issuing a wire round-trip", async () => {
    let calls = 0;
    const fetchImpl = (async () => {
      calls += 1;
      return jsonResponse(201, HOST_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await createHost(
      {
        display_name: "",
        hostname: "h.example.com",
        default_username: "deploy",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    expect(calls).toBe(0);
    if (!result.ok && result.error.kind === "validation") {
      expect(result.error.reason).toBe("missing_display_name");
    } else {
      expect.fail("expected validation error");
    }
  });

  it("maps a 4xx envelope to a typed http error (operator detail dropped)", async () => {
    const fetchImpl = (async () =>
      jsonResponse(400, {
        error: {
          code: "invalid_input",
          message: "hostname has whitespace",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await createHost(
      {
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "deploy",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(400);
      expect(result.error.code).toBe("invalid_input");
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
      expect(describeCreateHostError(result.error)).not.toContain(
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
    const result = await createHost(
      {
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "deploy",
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "transport") {
      expect(describeCreateHostError(result.error)).not.toContain(
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
    const result = await createHost(
      {
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "deploy",
      },
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
      jsonResponse(201, HOST_FIXTURE)) as unknown as typeof fetch;
    await createHost(
      {
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "deploy",
      },
      { fetchImpl: successFetch },
    );
    const httpFetch = (async () =>
      jsonResponse(503, {
        error: { code: "service_unavailable", message: "service unavailable" },
      })) as unknown as typeof fetch;
    await createHost(
      {
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "deploy",
      },
      { fetchImpl: httpFetch },
    );
    const transportFetch = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    await createHost(
      {
        display_name: "ok",
        hostname: "h.example.com",
        default_username: "deploy",
      },
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

describe("describeCreateHostError", () => {
  it("produces a stable function-of-kind+status+code summary", () => {
    expect(
      describeCreateHostError({
        kind: "validation",
        reason: "missing_display_name",
      }),
    ).toBe("Cannot create host: display name is required");
    expect(
      describeCreateHostError({
        kind: "http",
        status: 503,
        code: "service_unavailable",
        message: SENTINEL_OPERATOR,
      } satisfies CreateHostError),
    ).toBe("Failed to create host: HTTP 503 service_unavailable");
    expect(
      describeCreateHostError({
        kind: "transport",
        message: `boom ${SENTINEL_OPERATOR}`,
      }),
    ).toBe("Failed to create host: transport error");
    expect(
      describeCreateHostError({ kind: "malformed_response" }),
    ).toBe("Failed to create host: malformed response");
  });

  it("never echoes operator detail in any output", () => {
    const cases: CreateHostError[] = [
      { kind: "http", status: 400, code: "invalid_input", message: SENTINEL_OPERATOR },
      { kind: "transport", message: `request to https://example.com/${SENTINEL_OPERATOR}` },
    ];
    for (const c of cases) {
      const summary = describeCreateHostError(c);
      expect(summary).not.toContain(SENTINEL_OPERATOR);
      expect(summary).not.toContain("https://");
    }
  });
});

describe("parseTagsInput", () => {
  it("returns an empty array for an empty input", () => {
    expect(parseTagsInput("")).toEqual([]);
    expect(parseTagsInput("   ")).toEqual([]);
  });

  it("splits on commas and trims whitespace", () => {
    expect(parseTagsInput("prod, us-east-1 ,k8s_node")).toEqual([
      "prod",
      "us-east-1",
      "k8s_node",
    ]);
  });

  it("drops empty tokens (trailing comma, double comma)", () => {
    expect(parseTagsInput(", prod,,us-east-1, ")).toEqual([
      "prod",
      "us-east-1",
    ]);
  });
});

describe("validateCreateServerProfileRequest", () => {
  it("accepts a well-formed request without an override", () => {
    const result = validateCreateServerProfileRequest({
      name: "Prod / us-east-1",
      host_id: HOST_FIXTURE.id,
      ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      tags: ["prod", "us-east-1"],
    });
    expect(result).toEqual({
      ok: true,
      body: {
        name: "Prod / us-east-1",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        username_override: null,
        tags: ["prod", "us-east-1"],
      },
    });
  });

  it("normalizes an empty-string override to null", () => {
    const result = validateCreateServerProfileRequest({
      name: "ok",
      host_id: HOST_FIXTURE.id,
      ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      username_override: "",
      tags: [],
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.body.username_override).toBeNull();
    }
  });

  it("preserves a valid username override", () => {
    const result = validateCreateServerProfileRequest({
      name: "ok",
      host_id: HOST_FIXTURE.id,
      ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      username_override: "root",
      tags: [],
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.body.username_override).toBe("root");
    }
  });

  it("rejects missing host_id / ssh_identity_id", () => {
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: "",
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      }),
    ).toEqual({ ok: false, reason: "missing_host_id" });
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: "",
      }),
    ).toEqual({ ok: false, reason: "missing_ssh_identity_id" });
  });

  it("rejects an invalid override (bad leading char)", () => {
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        username_override: "1abc",
      }),
    ).toEqual({ ok: false, reason: "username_override_bad_leading_char" });
  });

  it("rejects names with surrounding whitespace, length, or controls", () => {
    expect(
      validateCreateServerProfileRequest({
        name: " trimme",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      }),
    ).toEqual({ ok: false, reason: "name_has_surrounding_whitespace" });
    expect(
      validateCreateServerProfileRequest({
        name: "a".repeat(MAX_PROFILE_NAME_LEN + 1),
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      }),
    ).toEqual({ ok: false, reason: "name_too_long" });
    expect(
      validateCreateServerProfileRequest({
        name: "ok\nname",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      }),
    ).toEqual({ ok: false, reason: "name_has_control_chars" });
  });

  it("rejects invalid tag characters (mirrors backend tag rules)", () => {
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: ["with space"],
      }),
    ).toEqual({ ok: false, reason: "tag_has_invalid_char" });
  });

  it("rejects duplicate tags", () => {
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: ["prod", "prod"],
      }),
    ).toEqual({ ok: false, reason: "tag_duplicate" });
  });

  it("rejects too many tags", () => {
    const many = Array.from({ length: MAX_TAGS + 1 }, (_, i) => `t${i}`);
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: many,
      }),
    ).toEqual({ ok: false, reason: "too_many_tags" });
  });

  it("rejects a tag over MAX_TAG_LEN", () => {
    expect(
      validateCreateServerProfileRequest({
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: ["a".repeat(MAX_TAG_LEN + 1)],
      }),
    ).toEqual({ ok: false, reason: "tag_too_long" });
  });
});

describe("canSubmitServerProfile", () => {
  it("blocks creation when both inventories are empty", () => {
    expect(canSubmitServerProfile(0, 0)).toEqual({
      kind: "no_hosts_or_identities",
    });
  });

  it("blocks when only hosts are missing", () => {
    expect(canSubmitServerProfile(0, 1)).toEqual({ kind: "no_hosts" });
  });

  it("blocks when only identities are missing", () => {
    expect(canSubmitServerProfile(1, 0)).toEqual({ kind: "no_identities" });
  });

  it("allows creation when at least one of each exists", () => {
    expect(canSubmitServerProfile(1, 1)).toEqual({ kind: "ok" });
  });
});

describe("createServerProfile", () => {
  it("targets POST /api/v1/server-profiles and omits username_override when absent", async () => {
    const captured: Array<{ url: string; body: unknown }> = [];
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured.push({
        url: String(input),
        body: JSON.parse(String(init?.body)),
      });
      return jsonResponse(201, PROFILE_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await createServerProfile(
      {
        name: "edge-1 prod",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: ["prod"],
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    expect(captured).toHaveLength(1);
    expect(captured[0].url).toBe("/api/v1/server-profiles");
    expect(captured[0].body).toEqual({
      name: "edge-1 prod",
      host_id: HOST_FIXTURE.id,
      ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      tags: ["prod"],
    });
    // The body must NOT contain username_override at all when the
    // override is null — the backend treats omitted and null the same,
    // but matching the integration tests' shape keeps the wire stable.
    expect(
      Object.prototype.hasOwnProperty.call(captured[0].body, "username_override"),
    ).toBe(false);
  });

  it("includes username_override when provided", async () => {
    let captured: Record<string, unknown> = {};
    const fetchImpl = (async (
      _input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return jsonResponse(201, {
        ...PROFILE_FIXTURE,
        username_override: "root",
      });
    }) as unknown as typeof fetch;
    const result = await createServerProfile(
      {
        name: "edge-1 prod",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        username_override: "root",
        tags: [],
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    expect(captured.username_override).toBe("root");
  });

  it("refuses an invalid request before issuing a wire round-trip", async () => {
    let calls = 0;
    const fetchImpl = (async () => {
      calls += 1;
      return jsonResponse(201, PROFILE_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await createServerProfile(
      {
        name: "",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    expect(calls).toBe(0);
    if (!result.ok && result.error.kind === "validation") {
      expect(result.error.reason).toBe("missing_name");
    } else {
      expect.fail("expected validation error");
    }
  });

  it("redaction: a 201 response carrying private-key sentinels parses without leaking", async () => {
    const fetchImpl = (async () =>
      jsonResponse(201, {
        ...PROFILE_FIXTURE,
        encrypted_private_key: SENTINEL_PRIVATE_KEY,
        private_key: SENTINEL_PRIVATE_KEY,
      })) as unknown as typeof fetch;
    const result = await createServerProfile(
      {
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: [],
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(JSON.stringify(result.profile)).not.toContain(SENTINEL_PRIVATE_KEY);
      expect(
        Object.prototype.hasOwnProperty.call(result.profile, "private_key"),
      ).toBe(false);
      expect(
        Object.prototype.hasOwnProperty.call(
          result.profile,
          "encrypted_private_key",
        ),
      ).toBe(false);
    }
  });

  it("maps a 4xx envelope to a typed http error (operator detail dropped)", async () => {
    const fetchImpl = (async () =>
      jsonResponse(404, {
        error: {
          code: "not_found",
          message: "host not found",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await createServerProfile(
      {
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: [],
      },
      { fetchImpl },
    );
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
      expect(
        describeCreateServerProfileError(result.error),
      ).not.toContain(SENTINEL_OPERATOR);
    } else {
      expect.fail("expected http error");
    }
  });

  it("collapses an unparseable 2xx body to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(201, { id: "broken" })) as unknown as typeof fetch;
    const result = await createServerProfile(
      {
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: [],
      },
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
      jsonResponse(201, PROFILE_FIXTURE)) as unknown as typeof fetch;
    await createServerProfile(
      {
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: [],
      },
      { fetchImpl: successFetch },
    );
    const httpFetch = (async () =>
      jsonResponse(404, {
        error: { code: "not_found", message: "host not found" },
      })) as unknown as typeof fetch;
    await createServerProfile(
      {
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: [],
      },
      { fetchImpl: httpFetch },
    );
    const transportFetch = (async () => {
      throw new Error("boom");
    }) as unknown as typeof fetch;
    await createServerProfile(
      {
        name: "ok",
        host_id: HOST_FIXTURE.id,
        ssh_identity_id: PROFILE_FIXTURE.ssh_identity_id,
        tags: [],
      },
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

describe("describeCreateServerProfileError", () => {
  it("collapses 404 not_found to a stale-reference hint", () => {
    expect(
      describeCreateServerProfileError({
        kind: "http",
        status: 404,
        code: "not_found",
        message: SENTINEL_OPERATOR,
      } satisfies CreateServerProfileError),
    ).toBe(
      "Failed to create server profile: linked host or SSH identity not found",
    );
  });

  it("never echoes wire / transport detail", () => {
    const cases: CreateServerProfileError[] = [
      { kind: "http", status: 400, code: "invalid_input", message: SENTINEL_OPERATOR },
      { kind: "http", status: 404, code: "not_found", message: SENTINEL_OPERATOR },
      { kind: "transport", message: `request to https://x/${SENTINEL_OPERATOR}` },
    ];
    for (const c of cases) {
      const s = describeCreateServerProfileError(c);
      expect(s).not.toContain(SENTINEL_OPERATOR);
      expect(s).not.toContain("https://");
    }
  });

  it("describes each validation reason without leaking inputs", () => {
    expect(
      describeCreateServerProfileError({
        kind: "validation",
        reason: "tag_has_invalid_char",
      }),
    ).toBe(
      "Cannot create server profile: tags may only contain letters, digits, '-', '_'",
    );
  });
});
