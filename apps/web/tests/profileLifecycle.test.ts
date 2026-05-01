import { describe, expect, it, vi } from "vitest";

import {
  describeLifecycleError,
  disableServerProfile,
  enableServerProfile,
  type LifecycleError,
  type ServerProfile,
} from "../src/lib/api/serverProfiles.js";
import {
  canLaunchProfile,
  canRunProfileSetupActions,
  describeDisabledProfile,
  DISABLE_CONFIRMATION_COPY,
  ENABLE_CONFIRMATION_COPY,
  disableConfirmationMatches,
  isServerProfileDisabled,
  profileLifecycleLabel,
  profileLifecycleTone,
} from "../src/lib/app/inventory/profileLifecycle.js";

/**
 * Sentinels that MUST NEVER appear in any wire-helper output, parsed
 * DTO, formatted summary, or static copy string.
 *
 * The redaction rule mirrors the rest of the inventory surface:
 *  - Operator detail in 4xx envelopes does not reach the formatter.
 *  - Transport `Error.message` does not reach the formatter.
 *  - `private_key` / `encrypted_private_key` fields cannot smuggle
 *    onto a parsed `ServerProfile` even if a hostile fixture puts them
 *    on the wire body.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_LIFECYCLE_OPERATOR_DETAIL_5301";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_LIFECYCLE_PRIVATE_KEY_5302";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const ENABLED_FIXTURE: ServerProfile = {
  id: "22222222-2222-2222-2222-222222222222",
  name: "edge-1 prod",
  host_id: "11111111-1111-1111-1111-111111111111",
  ssh_identity_id: "33333333-3333-3333-3333-333333333333",
  username_override: null,
  tags: ["prod"],
  created_at: "2026-05-01T00:00:00Z",
  updated_at: "2026-05-01T00:00:00Z",
  last_connected_at: null,
  disabled_at: null,
};

const DISABLED_FIXTURE: ServerProfile = {
  ...ENABLED_FIXTURE,
  id: "22222222-2222-2222-2222-333333333333",
  name: "edge-1 staging",
  disabled_at: "2026-05-01T01:23:45Z",
};

describe("isServerProfileDisabled", () => {
  it("returns false for a profile with disabled_at = null", () => {
    expect(isServerProfileDisabled(ENABLED_FIXTURE)).toBe(false);
  });

  it("returns true for a profile with a non-empty disabled_at timestamp", () => {
    expect(isServerProfileDisabled(DISABLED_FIXTURE)).toBe(true);
  });

  it("treats an empty disabled_at string as enabled", () => {
    expect(
      isServerProfileDisabled({ ...ENABLED_FIXTURE, disabled_at: "" }),
    ).toBe(false);
  });
});

describe("profileLifecycleLabel / profileLifecycleTone", () => {
  it("labels enabled profiles with neutral tone", () => {
    expect(profileLifecycleLabel(ENABLED_FIXTURE)).toBe("enabled");
    expect(profileLifecycleTone(ENABLED_FIXTURE)).toBe("neutral");
  });

  it("labels disabled profiles with muted tone", () => {
    expect(profileLifecycleLabel(DISABLED_FIXTURE)).toBe("disabled");
    expect(profileLifecycleTone(DISABLED_FIXTURE)).toBe("muted");
  });
});

describe("canRunProfileSetupActions / canLaunchProfile", () => {
  it("permits setup and launch on enabled profiles", () => {
    expect(canRunProfileSetupActions(ENABLED_FIXTURE)).toBe(true);
    expect(canLaunchProfile(ENABLED_FIXTURE)).toBe(true);
  });

  it("blocks setup and launch on disabled profiles", () => {
    expect(canRunProfileSetupActions(DISABLED_FIXTURE)).toBe(false);
    expect(canLaunchProfile(DISABLED_FIXTURE)).toBe(false);
  });
});

describe("describeDisabledProfile", () => {
  it("returns an empty string for an enabled profile", () => {
    expect(describeDisabledProfile(ENABLED_FIXTURE)).toBe("");
  });

  it("returns honest copy for a disabled profile", () => {
    const copy = describeDisabledProfile(DISABLED_FIXTURE);
    expect(copy).toContain("disabled");
    expect(copy).toContain("Existing live sessions");
    // Avoid destructive / archival language.
    expect(copy.toLowerCase()).not.toContain("delete");
    expect(copy.toLowerCase()).not.toContain("removed");
    expect(copy.toLowerCase()).not.toContain("archived");
  });

  it("never embeds profile-specific data that could carry a sentinel", () => {
    const hostile = {
      ...DISABLED_FIXTURE,
      name: SENTINEL_OPERATOR,
    };
    expect(describeDisabledProfile(hostile)).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("disable / enable confirmation copy", () => {
  it("describes the launch / setup gate without destructive language", () => {
    expect(DISABLE_CONFIRMATION_COPY).toContain("blocks new terminal launches");
    expect(DISABLE_CONFIRMATION_COPY).toContain("Existing live sessions");
    expect(DISABLE_CONFIRMATION_COPY.toLowerCase()).not.toContain("delete");
    expect(DISABLE_CONFIRMATION_COPY.toLowerCase()).not.toContain("kill");
  });

  it("explains that enable does NOT prove trust or auth readiness", () => {
    expect(ENABLE_CONFIRMATION_COPY).toContain("does NOT prove");
    expect(ENABLE_CONFIRMATION_COPY).toContain("auth-check");
  });
});

describe("disableConfirmationMatches", () => {
  it("requires an exact echo of the profile name", () => {
    expect(disableConfirmationMatches(ENABLED_FIXTURE, "edge-1 prod")).toBe(true);
  });

  it("rejects an empty or mismatched value", () => {
    expect(disableConfirmationMatches(ENABLED_FIXTURE, "")).toBe(false);
    expect(disableConfirmationMatches(ENABLED_FIXTURE, "edge-1")).toBe(false);
    expect(disableConfirmationMatches(ENABLED_FIXTURE, "EDGE-1 PROD")).toBe(false);
    expect(disableConfirmationMatches(ENABLED_FIXTURE, " edge-1 prod ")).toBe(
      false,
    );
  });
});

describe("disableServerProfile", () => {
  it("targets POST /api/v1/server-profiles/:id/disable and returns the parsed row", async () => {
    const captured: Array<{ url: string; method: string; body: unknown }> = [];
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured.push({
        url: String(input),
        method: String(init?.method ?? ""),
        body: init?.body ? JSON.parse(String(init.body)) : null,
      });
      return jsonResponse(200, DISABLED_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await disableServerProfile(ENABLED_FIXTURE.id, { fetchImpl });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.profile.disabled_at).toBe(DISABLED_FIXTURE.disabled_at);
    }
    expect(captured).toHaveLength(1);
    expect(captured[0].url).toBe(
      `/api/v1/server-profiles/${ENABLED_FIXTURE.id}/disable`,
    );
    expect(captured[0].method).toBe("POST");
    expect(captured[0].body).toEqual({});
  });

  it("path-encodes the profile id", async () => {
    let observedUrl = "";
    const fetchImpl = (async (input: string | URL | Request) => {
      observedUrl = String(input);
      return jsonResponse(200, DISABLED_FIXTURE);
    }) as unknown as typeof fetch;
    await disableServerProfile("hostile id with /slash", { fetchImpl });
    expect(observedUrl).toBe(
      "/api/v1/server-profiles/hostile%20id%20with%20%2Fslash/disable",
    );
  });

  it("maps a 4xx envelope to a typed http error (operator detail dropped)", async () => {
    const fetchImpl = (async () =>
      jsonResponse(404, {
        error: {
          code: "not_found",
          message: "server profile not found",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await disableServerProfile(ENABLED_FIXTURE.id, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(404);
      expect(result.error.code).toBe("not_found");
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
      expect(describeLifecycleError("disable", result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
  });

  it("collapses an unparseable 2xx body to malformed_response", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, { id: "broken" })) as unknown as typeof fetch;
    const result = await disableServerProfile(ENABLED_FIXTURE.id, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({ kind: "malformed_response" });
    }
  });

  it("redaction: a 200 response carrying private-key sentinels parses without leaking", async () => {
    const fetchImpl = (async () =>
      jsonResponse(200, {
        ...DISABLED_FIXTURE,
        encrypted_private_key: SENTINEL_PRIVATE_KEY,
        private_key: SENTINEL_PRIVATE_KEY,
      })) as unknown as typeof fetch;
    const result = await disableServerProfile(ENABLED_FIXTURE.id, { fetchImpl });
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

  it("does not log raw response bodies on success or failure", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const successFetch = (async () =>
      jsonResponse(200, DISABLED_FIXTURE)) as unknown as typeof fetch;
    await disableServerProfile(ENABLED_FIXTURE.id, { fetchImpl: successFetch });
    const httpFetch = (async () =>
      jsonResponse(404, {
        error: {
          code: "not_found",
          message: "server profile not found",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    await disableServerProfile(ENABLED_FIXTURE.id, { fetchImpl: httpFetch });
    const transportFetch = (async () => {
      throw new Error(`network ${SENTINEL_OPERATOR}`);
    }) as unknown as typeof fetch;
    await disableServerProfile(ENABLED_FIXTURE.id, {
      fetchImpl: transportFetch,
    });
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    expect(logSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
    logSpy.mockRestore();
  });
});

describe("enableServerProfile", () => {
  it("targets POST /api/v1/server-profiles/:id/enable and returns the parsed row", async () => {
    const captured: Array<{ url: string; method: string; body: unknown }> = [];
    const fetchImpl = (async (
      input: string | URL | Request,
      init?: RequestInit,
    ) => {
      captured.push({
        url: String(input),
        method: String(init?.method ?? ""),
        body: init?.body ? JSON.parse(String(init.body)) : null,
      });
      return jsonResponse(200, ENABLED_FIXTURE);
    }) as unknown as typeof fetch;
    const result = await enableServerProfile(DISABLED_FIXTURE.id, { fetchImpl });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.profile.disabled_at).toBeNull();
    }
    expect(captured).toHaveLength(1);
    expect(captured[0].url).toBe(
      `/api/v1/server-profiles/${DISABLED_FIXTURE.id}/enable`,
    );
    expect(captured[0].method).toBe("POST");
    expect(captured[0].body).toEqual({});
  });

  it("maps a 4xx envelope to a typed http error (operator detail dropped)", async () => {
    const fetchImpl = (async () =>
      jsonResponse(401, {
        error: {
          code: "unauthorized",
          message: "dev-auth disabled",
          operator_detail: SENTINEL_OPERATOR,
        },
      })) as unknown as typeof fetch;
    const result = await enableServerProfile(DISABLED_FIXTURE.id, { fetchImpl });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.status).toBe(401);
      expect(JSON.stringify(result.error)).not.toContain(SENTINEL_OPERATOR);
      expect(describeLifecycleError("enable", result.error)).not.toContain(
        SENTINEL_OPERATOR,
      );
    } else {
      expect.fail("expected http error");
    }
  });
});

describe("describeLifecycleError", () => {
  it("produces a stable function-of-kind+status+code summary", () => {
    expect(
      describeLifecycleError("disable", {
        kind: "http",
        status: 404,
        code: "not_found",
        message: SENTINEL_OPERATOR,
      } satisfies LifecycleError),
    ).toBe("Failed to disable server profile: server profile not found");
    expect(
      describeLifecycleError("enable", {
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: SENTINEL_OPERATOR,
      } satisfies LifecycleError),
    ).toBe("Failed to enable server profile: not authenticated");
    expect(
      describeLifecycleError("disable", {
        kind: "transport",
        message: `boom ${SENTINEL_OPERATOR}`,
      }),
    ).toBe("Failed to disable server profile: transport error");
    expect(
      describeLifecycleError("enable", { kind: "malformed_response" }),
    ).toBe("Failed to enable server profile: malformed response");
  });

  it("never echoes operator detail in any output", () => {
    const cases: LifecycleError[] = [
      { kind: "http", status: 500, code: "internal_error", message: SENTINEL_OPERATOR },
      { kind: "transport", message: `request to https://example.com/${SENTINEL_OPERATOR}` },
    ];
    for (const action of ["disable", "enable"] as const) {
      for (const c of cases) {
        const summary = describeLifecycleError(action, c);
        expect(summary).not.toContain(SENTINEL_OPERATOR);
        expect(summary).not.toContain("https://");
      }
    }
  });
});
