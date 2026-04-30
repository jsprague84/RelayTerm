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
  listSshIdentities,
  parseSshIdentity,
  publicKeyPreview,
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
