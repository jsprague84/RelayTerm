import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
  DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER,
  DEFAULT_MAX_STARTING_SESSIONS_PER_USER,
  __resetSessionPolicyCache,
  describeDetachedTtl,
  describeMaxLivePtyPerUser,
  describeMaxStartingPerUser,
  fetchSessionPolicy,
  formatDetachedTtl,
  loadSessionPolicy,
  parseSessionPolicy,
  type SessionPolicy,
} from "../src/lib/api/sessionPolicy.js";

/** Well-formed wire body for Phase 1B.2a+ session-policy responses. */
const VALID_BODY = {
  detached_live_pty_ttl_seconds: 30,
  max_live_pty_sessions_per_user: 8,
  max_starting_sessions_per_user: 4,
};

/**
 * Secret-shaped sentinels that MUST NEVER reach the parsed DTO no
 * matter how a hostile (or accidentally widened) backend body widens
 * its shape. Mirrors the `AUDIT_FORBIDDEN_SUBSTRINGS` backstop in the
 * backend's `crates/relayterm-api/tests/api.rs::session_policy_*`
 * tests; a future regression on either side fails the matching test.
 */
const HOSTILE_SECRETS: Record<string, string> = {
  private_key: "BEGIN OPENSSH PRIVATE KEY RELAY-SECRET-DO-NOT-LEAK",
  encrypted_private_key: "ENC-RELAY-SECRET-DO-NOT-LEAK",
  session_token: "session-token-DO-NOT-LEAK",
  token_hash: "token-hash-DO-NOT-LEAK",
  password_hash: "argon2id-DO-NOT-LEAK",
  bootstrap_token: "bootstrap-DO-NOT-LEAK",
  database_url: "postgres://relay:DO-NOT-LEAK@db/relayterm",
  vault_master_key: "vault-master-DO-NOT-LEAK",
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  __resetSessionPolicyCache();
});

afterEach(() => {
  __resetSessionPolicyCache();
});

describe("DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS", () => {
  it("mirrors the backend's SPEC-pinned DETACHED_LIVE_PTY_TTL of 30 s", () => {
    // The SPA falls back to this constant while the policy fetch is in
    // flight OR has failed. It MUST track the backend's
    // `relayterm_terminal::DETACHED_LIVE_PTY_TTL` default so a
    // not-yet-deployed policy endpoint still renders honest copy.
    expect(DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS).toBe(30);
  });
});

describe("parseSessionPolicy", () => {
  it("accepts a well-shaped body with all three fields", () => {
    expect(parseSessionPolicy(VALID_BODY)).toEqual({
      detached_live_pty_ttl_seconds: 30,
      max_live_pty_sessions_per_user: 8,
      max_starting_sessions_per_user: 4,
    });
    expect(
      parseSessionPolicy({
        detached_live_pty_ttl_seconds: 1800,
        max_live_pty_sessions_per_user: 16,
        max_starting_sessions_per_user: 8,
      }),
    ).toEqual({
      detached_live_pty_ttl_seconds: 1800,
      max_live_pty_sessions_per_user: 16,
      max_starting_sessions_per_user: 8,
    });
    expect(
      parseSessionPolicy({
        detached_live_pty_ttl_seconds: 86_400,
        max_live_pty_sessions_per_user: 256,
        max_starting_sessions_per_user: 32,
      }),
    ).toEqual({
      detached_live_pty_ttl_seconds: 86_400,
      max_live_pty_sessions_per_user: 256,
      max_starting_sessions_per_user: 32,
    });
  });

  it("rejects out-of-range / non-integer / non-numeric TTL values", () => {
    // The parser mirrors the backend validator's `5..=86_400` bound
    // exactly: a value outside that range cannot have been emitted by
    // a current backend, so we treat it as a malformed wire body and
    // fall back to the default rather than trust a hostile payload.
    expect(
      parseSessionPolicy({ ...VALID_BODY, detached_live_pty_ttl_seconds: 0 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, detached_live_pty_ttl_seconds: 4 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, detached_live_pty_ttl_seconds: -1 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({
        ...VALID_BODY,
        detached_live_pty_ttl_seconds: 86_401,
      }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, detached_live_pty_ttl_seconds: 30.5 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, detached_live_pty_ttl_seconds: "30" }),
    ).toBeNull();
    expect(parseSessionPolicy({})).toBeNull();
    expect(parseSessionPolicy(null)).toBeNull();
    expect(parseSessionPolicy(undefined)).toBeNull();
    expect(parseSessionPolicy("30")).toBeNull();
  });

  it("rejects out-of-range / non-integer max_live_pty_sessions_per_user values", () => {
    // Mirrors backend validator's `1..=256` bound.
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_live_pty_sessions_per_user: 0 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_live_pty_sessions_per_user: -1 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_live_pty_sessions_per_user: 257 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_live_pty_sessions_per_user: 4.5 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_live_pty_sessions_per_user: "8" }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ detached_live_pty_ttl_seconds: 30 }),
    ).toBeNull();
  });

  it("rejects out-of-range / non-integer max_starting_sessions_per_user values", () => {
    // Mirrors backend validator's `1..=32` bound.
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_starting_sessions_per_user: 0 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_starting_sessions_per_user: -1 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_starting_sessions_per_user: 33 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_starting_sessions_per_user: 4.5 }),
    ).toBeNull();
    expect(
      parseSessionPolicy({ ...VALID_BODY, max_starting_sessions_per_user: "4" }),
    ).toBeNull();
    expect(
      parseSessionPolicy({
        detached_live_pty_ttl_seconds: 30,
        max_live_pty_sessions_per_user: 8,
      }),
    ).toBeNull();
  });

  it("ignores secret-shaped sibling fields — they NEVER reach the parsed DTO", () => {
    // The parser builds the DTO field-by-field; a hostile body with
    // every secret-shaped sibling under the sun must yield exactly
    // the three valid fields. This is the load-bearing redaction
    // backstop on the frontend side.
    const hostile: Record<string, unknown> = {
      ...VALID_BODY,
      detached_live_pty_ttl_seconds: 600,
      max_live_pty_sessions_per_user: 4,
      max_starting_sessions_per_user: 2,
      ...HOSTILE_SECRETS,
    };
    const parsed = parseSessionPolicy(hostile);
    expect(parsed).toEqual({
      detached_live_pty_ttl_seconds: 600,
      max_live_pty_sessions_per_user: 4,
      max_starting_sessions_per_user: 2,
    });
    const keys = Object.keys(parsed as object).sort();
    expect(keys).toEqual([
      "detached_live_pty_ttl_seconds",
      "max_live_pty_sessions_per_user",
      "max_starting_sessions_per_user",
    ]);
    const raw = JSON.stringify(parsed);
    for (const secret of Object.values(HOSTILE_SECRETS)) {
      expect(raw).not.toContain(secret);
    }
  });
});

describe("DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER", () => {
  it("mirrors the backend's Phase 1B.1 default of 8", () => {
    // The SPA falls back to this constant while the policy fetch is
    // in flight OR has failed. It MUST track the backend's
    // `relayterm_terminal::DEFAULT_MAX_LIVE_PTY_PER_USER` default so
    // an at-cap launch refusal renders honest copy.
    expect(DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER).toBe(8);
  });
});

describe("DEFAULT_MAX_STARTING_SESSIONS_PER_USER", () => {
  it("mirrors the backend's Phase 1B.2a default of 4", () => {
    // The SPA falls back to this constant while the policy fetch is
    // in flight OR has failed. It MUST track the backend's
    // `relayterm_terminal::DEFAULT_MAX_STARTING_PER_USER` default so
    // an at-cap launch refusal renders honest copy.
    expect(DEFAULT_MAX_STARTING_SESSIONS_PER_USER).toBe(4);
  });
});

describe("describeMaxStartingPerUser", () => {
  it("renders parameterised copy on a sane integer cap", () => {
    expect(describeMaxStartingPerUser(1)).toBe(
      "This deployment allows up to 1 terminal session starting at once per user.",
    );
    expect(describeMaxStartingPerUser(4)).toBe(
      "This deployment allows up to 4 terminal sessions starting at once per user.",
    );
    expect(describeMaxStartingPerUser(8)).toBe(
      "This deployment allows up to 8 terminal sessions starting at once per user.",
    );
  });

  it("falls back to the default for non-positive / non-integer inputs", () => {
    expect(describeMaxStartingPerUser(0)).toContain(
      "4 terminal sessions starting",
    );
    expect(describeMaxStartingPerUser(-1)).toContain(
      "4 terminal sessions starting",
    );
    expect(describeMaxStartingPerUser(4.5)).toContain(
      "4 terminal sessions starting",
    );
    expect(describeMaxStartingPerUser(Number.NaN)).toContain(
      "4 terminal sessions starting",
    );
  });

  it("never uses the anti-overclaim register", () => {
    // `docs/session-quotas.md` § 7.5 / § 12.
    const forbidden = [
      "your session quota",
      "we're rate-limiting you",
      "please slow down",
      "queue",
      "always available",
      "persistent across restart",
    ];
    for (const cap of [1, 4, 8, 16, 32]) {
      const copy = describeMaxStartingPerUser(cap).toLowerCase();
      for (const phrase of forbidden) {
        expect(copy).not.toContain(phrase);
      }
      expect(copy).not.toMatch(/wait \d+ seconds/);
    }
  });
});

describe("describeMaxLivePtyPerUser", () => {
  it("renders parameterised copy on a sane integer cap", () => {
    expect(describeMaxLivePtyPerUser(1)).toBe(
      "This deployment allows up to 1 live terminal session per user.",
    );
    expect(describeMaxLivePtyPerUser(8)).toBe(
      "This deployment allows up to 8 live terminal sessions per user.",
    );
    expect(describeMaxLivePtyPerUser(16)).toBe(
      "This deployment allows up to 16 live terminal sessions per user.",
    );
  });

  it("falls back to the default for non-positive / non-integer inputs", () => {
    expect(describeMaxLivePtyPerUser(0)).toContain("8 live terminal sessions");
    expect(describeMaxLivePtyPerUser(-1)).toContain("8 live terminal sessions");
    expect(describeMaxLivePtyPerUser(4.5)).toContain("8 live terminal sessions");
    expect(describeMaxLivePtyPerUser(Number.NaN)).toContain(
      "8 live terminal sessions",
    );
  });

  it("never uses the anti-overclaim register", () => {
    // `docs/session-quotas.md` § 7.5 / § 12. Full set of substrings
    // the quota-refusal copy MUST avoid.
    const forbidden = [
      "your session quota",
      "we're rate-limiting you",
      "please slow down",
      "queue",
      "always available",
      "persistent across restart",
    ];
    for (const cap of [1, 4, 8, 16, 64, 256]) {
      const copy = describeMaxLivePtyPerUser(cap).toLowerCase();
      for (const phrase of forbidden) {
        expect(copy).not.toContain(phrase);
      }
      expect(copy).not.toMatch(/wait \d+ seconds/);
    }
  });
});

describe("formatDetachedTtl", () => {
  it("renders seconds for sub-minute values", () => {
    expect(formatDetachedTtl(1)).toBe("about 1 second");
    expect(formatDetachedTtl(5)).toBe("about 5 seconds");
    expect(formatDetachedTtl(30)).toBe("about 30 seconds");
    expect(formatDetachedTtl(59)).toBe("about 59 seconds");
  });

  it("renders minutes for sub-hour values", () => {
    expect(formatDetachedTtl(60)).toBe("about 1 minute");
    expect(formatDetachedTtl(300)).toBe("about 5 minutes");
    expect(formatDetachedTtl(1800)).toBe("about 30 minutes");
    expect(formatDetachedTtl(3599)).toBe("about 60 minutes");
  });

  it("renders hours for sub-day values", () => {
    expect(formatDetachedTtl(3600)).toBe("about 1 hour");
    expect(formatDetachedTtl(4 * 3600)).toBe("about 4 hours");
    expect(formatDetachedTtl(23 * 3600)).toBe("about 23 hours");
  });

  it("renders days at the 24 h validator cap", () => {
    expect(formatDetachedTtl(86_400)).toBe("about 1 day");
  });

  it("falls back to the SPEC-pinned default for non-positive / non-finite inputs", () => {
    // The fallback keeps a malformed wire value from silently producing
    // a "0 seconds" or NaN string in the UI. The function is pure so
    // the fallback is testable directly.
    expect(formatDetachedTtl(0)).toBe("about 30 seconds");
    expect(formatDetachedTtl(-1)).toBe("about 30 seconds");
    expect(formatDetachedTtl(Number.NaN)).toBe("about 30 seconds");
    expect(formatDetachedTtl(Number.POSITIVE_INFINITY)).toBe(
      "about 30 seconds",
    );
  });
});

describe("describeDetachedTtl", () => {
  it("includes the formatted window AND the persistence disclaimer", () => {
    const copy = describeDetachedTtl(300).toLowerCase();
    expect(copy).toContain("5 minutes");
    expect(copy).toContain("in-memory");
    expect(copy).toContain("backend restart");
  });

  it("never claims durable persistence across restart (anti-overclaim)", () => {
    // Mirrors the forbidden-substring register from
    // `docs/persistent-sessions.md` § 11.7. A future revision that
    // weakens the disclaimer must update the design doc first.
    const forbidden = [
      "your session is saved",
      "always available",
      "your shell will resume automatically",
      "persistent across restart",
      "session recovery",
      "your work is preserved",
    ];
    for (const window of [30, 300, 1800, 86_400]) {
      const copy = describeDetachedTtl(window).toLowerCase();
      for (const phrase of forbidden) {
        expect(copy).not.toContain(phrase);
      }
    }
  });
});

describe("fetchSessionPolicy", () => {
  it("returns the parsed policy on a 200 with a valid body", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse(200, {
        detached_live_pty_ttl_seconds: 600,
        max_live_pty_sessions_per_user: 4,
        max_starting_sessions_per_user: 2,
      }),
    );
    const result = await fetchSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.policy).toEqual({
        detached_live_pty_ttl_seconds: 600,
        max_live_pty_sessions_per_user: 4,
        max_starting_sessions_per_user: 2,
      });
    }
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl).toHaveBeenCalledWith(
      "/api/v1/config/session-policy",
      expect.objectContaining({ headers: { accept: "application/json" } }),
    );
  });

  it("collapses 401 to a typed http error without echoing wire detail", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse(401, {
        error: { code: "unauthorized", message: "session invalid" },
      }),
    );
    const result = await fetchSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error).toEqual({
        kind: "http",
        status: 401,
        code: "unauthorized",
        message: "session invalid",
      });
    }
  });

  it("collapses a malformed body to malformed_response", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse(200, {
        detached_live_pty_ttl_seconds: "not-a-number",
        max_live_pty_sessions_per_user: 8,
        max_starting_sessions_per_user: 4,
      }),
    );
    const result = await fetchSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("malformed_response");
    }
  });

  it("collapses a transport failure to a typed transport error", async () => {
    const fetchImpl = vi.fn(async () => {
      throw new Error("network down");
    });
    const result = await fetchSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("transport");
    }
  });
});

describe("loadSessionPolicy", () => {
  it("returns the fetched policy on success", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse(200, {
        detached_live_pty_ttl_seconds: 1800,
        max_live_pty_sessions_per_user: 16,
        max_starting_sessions_per_user: 8,
      }),
    );
    const policy: SessionPolicy = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(policy).toEqual({
      detached_live_pty_ttl_seconds: 1800,
      max_live_pty_sessions_per_user: 16,
      max_starting_sessions_per_user: 8,
    });
  });

  it("falls back to the SPEC-pinned defaults on HTTP failure", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse(503, { error: { code: "service_unavailable", message: "" } }),
    );
    const policy = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(policy.detached_live_pty_ttl_seconds).toBe(
      DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
    );
    expect(policy.max_live_pty_sessions_per_user).toBe(
      DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER,
    );
    expect(policy.max_starting_sessions_per_user).toBe(
      DEFAULT_MAX_STARTING_SESSIONS_PER_USER,
    );
  });

  it("falls back to the SPEC-pinned defaults on transport failure", async () => {
    const fetchImpl = vi.fn(async () => {
      throw new Error("offline");
    });
    const policy = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(policy.detached_live_pty_ttl_seconds).toBe(
      DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
    );
    expect(policy.max_live_pty_sessions_per_user).toBe(
      DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER,
    );
    expect(policy.max_starting_sessions_per_user).toBe(
      DEFAULT_MAX_STARTING_SESSIONS_PER_USER,
    );
  });

  it("caches the successful result across calls (one wire round-trip)", async () => {
    const fetchImpl = vi.fn(async () =>
      jsonResponse(200, {
        detached_live_pty_ttl_seconds: 1800,
        max_live_pty_sessions_per_user: 16,
        max_starting_sessions_per_user: 8,
      }),
    );
    const a = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    const b = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(a).toEqual(b);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it("does NOT cache failures (next caller gets a fresh attempt)", async () => {
    const fetchImpl = vi
      .fn()
      .mockImplementationOnce(async () => {
        throw new Error("offline");
      })
      .mockImplementationOnce(async () =>
        jsonResponse(200, {
          detached_live_pty_ttl_seconds: 42,
          max_live_pty_sessions_per_user: 3,
          max_starting_sessions_per_user: 2,
        }),
      );
    const first = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(first.detached_live_pty_ttl_seconds).toBe(
      DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
    );
    expect(first.max_live_pty_sessions_per_user).toBe(
      DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER,
    );
    expect(first.max_starting_sessions_per_user).toBe(
      DEFAULT_MAX_STARTING_SESSIONS_PER_USER,
    );
    const second = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(second.detached_live_pty_ttl_seconds).toBe(42);
    expect(second.max_live_pty_sessions_per_user).toBe(3);
    expect(second.max_starting_sessions_per_user).toBe(2);
    expect(fetchImpl).toHaveBeenCalledTimes(2);
  });

  it("drops hostile secret-shaped sibling fields end-to-end", async () => {
    // End-to-end redaction sentinel: a hostile body that smuggles
    // every secret-shaped field MUST collapse to either the three
    // valid numeric fields (when shape parses) or the fallback (when
    // it doesn't). In either case, no hostile value reaches the
    // returned object.
    const fetchImpl = vi.fn(async () =>
      jsonResponse(200, {
        detached_live_pty_ttl_seconds: 90,
        max_live_pty_sessions_per_user: 2,
        max_starting_sessions_per_user: 1,
        ...HOSTILE_SECRETS,
      }),
    );
    const policy = await loadSessionPolicy({
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(policy.detached_live_pty_ttl_seconds).toBe(90);
    expect(policy.max_live_pty_sessions_per_user).toBe(2);
    expect(policy.max_starting_sessions_per_user).toBe(1);
    const keys = Object.keys(policy).sort();
    expect(keys).toEqual([
      "detached_live_pty_ttl_seconds",
      "max_live_pty_sessions_per_user",
      "max_starting_sessions_per_user",
    ]);
    const raw = JSON.stringify(policy);
    for (const secret of Object.values(HOSTILE_SECRETS)) {
      expect(raw).not.toContain(secret);
    }
  });
});
