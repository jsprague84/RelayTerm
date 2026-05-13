/**
 * Tests for the inventory PATCH / DELETE helpers added in the
 * `feat/inventory-management-basics` slice. Mirrors the redaction
 * posture of the create-api tests:
 *  - `describeXError` is a function of `kind` + `status` + `code` (and
 *    the derived `reason` discriminator) only — it MUST NEVER echo
 *    the wire `message` of an HTTP error or the thrown `Error.message`
 *    of a transport failure.
 *  - The parsed-row DTO MUST NEVER carry private-key material — the
 *    field-by-field constructor in `parseSshIdentity` is the
 *    redaction backstop.
 */

import { describe, expect, it, vi } from "vitest";
import {
  deleteHost,
  describeDeleteHostError,
  describeUpdateHostError,
  updateHost,
  validateUpdateHostRequest,
} from "../src/lib/api/hosts.js";
import {
  deleteServerProfile,
  describeDeleteServerProfileError,
  describeUpdateServerProfileError,
  updateServerProfile,
  validateUpdateServerProfileRequest,
} from "../src/lib/api/serverProfiles.js";
import {
  deleteSshIdentity,
  describeDeleteSshIdentityError,
  describeImportSshIdentityError,
  describeUpdateSshIdentityError,
  importSshIdentity,
  MAX_PRIVATE_KEY_OPENSSH_BYTES,
  updateSshIdentity,
  validateImportSshIdentityRequest,
  validateUpdateSshIdentityRequest,
} from "../src/lib/api/sshIdentities.js";

/**
 * Sentinels that MUST NEVER appear in user-visible UI strings or
 * parsed DTOs.
 */
const SENTINEL_OPERATOR = "RELAY_SENTINEL_MUTATIONS_OPERATOR_9201";
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_MUTATIONS_PRIVATE_KEY_9202";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function noContentResponse(): Response {
  return new Response(null, { status: 204 });
}

// ---------------------------------------------------------------------------
// Host PATCH / DELETE
// ---------------------------------------------------------------------------

describe("validateUpdateHostRequest", () => {
  it("rejects an empty body", () => {
    const v = validateUpdateHostRequest({});
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("empty_update");
  });

  it("accepts a single supplied field", () => {
    const v = validateUpdateHostRequest({ display_name: "Renamed" });
    expect(v.ok).toBe(true);
    if (v.ok) expect(v.body).toEqual({ display_name: "Renamed" });
  });

  it("flags an out-of-range port", () => {
    const v = validateUpdateHostRequest({ port: 70000 });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("port_out_of_range");
  });

  it("flags a hostname with whitespace", () => {
    const v = validateUpdateHostRequest({ hostname: "bad host" });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("hostname_has_whitespace");
  });
});

describe("updateHost", () => {
  it("PATCHes the right endpoint and parses the response", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(200, {
        id: "abc",
        display_name: "Renamed",
        hostname: "h.example.com",
        port: 2222,
        default_username: "ops",
        created_at: "2026-05-01T00:00:00Z",
        updated_at: "2026-05-12T00:00:00Z",
      }),
    );
    const result = await updateHost(
      "abc",
      { display_name: "Renamed", port: 2222 },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(fetchImpl).toHaveBeenCalledOnce();
    const [url, init] = fetchImpl.mock.calls[0];
    expect(url).toBe("/api/v1/hosts/abc");
    expect((init as RequestInit).method).toBe("PATCH");
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.host.display_name).toBe("Renamed");
  });

  it("maps 404 to typed http error", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(404, {
        error: {
          code: "not_found",
          message: `host not found ${SENTINEL_OPERATOR}`,
        },
      }),
    );
    const result = await updateHost(
      "bogus",
      { display_name: "X" },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.kind).toBe("http");
  });
});

describe("describeUpdateHostError", () => {
  it("never echoes the wire message", () => {
    const summary = describeUpdateHostError({
      kind: "http",
      status: 404,
      code: "not_found",
      message: `host not found ${SENTINEL_OPERATOR}`,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("uses a friendly message for empty_update", () => {
    expect(
      describeUpdateHostError({ kind: "validation", reason: "empty_update" }),
    ).toBe("Cannot save host: change at least one field");
  });
});

describe("deleteHost", () => {
  it("returns ok on 204", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(noContentResponse());
    const result = await deleteHost("abc", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(true);
    const [url, init] = fetchImpl.mock.calls[0];
    expect(url).toBe("/api/v1/hosts/abc");
    expect((init as RequestInit).method).toBe("DELETE");
  });

  it("maps a 409 host-referenced conflict to typed reason", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(409, {
        error: { code: "conflict", message: "host referenced" },
      }),
    );
    const result = await deleteHost("abc", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("referenced");
    }
  });

  it("collapses an unknown 409 message to reason=null (no echo)", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(409, {
        error: {
          code: "conflict",
          message: `host weird-reason ${SENTINEL_OPERATOR}`,
        },
      }),
    );
    const result = await deleteHost("abc", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(false);
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBeNull();
    }
    const summary = result.ok
      ? ""
      : describeDeleteHostError(result.error);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("describeDeleteHostError", () => {
  it("produces helpful copy on a referenced 409", () => {
    const summary = describeDeleteHostError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: "host referenced",
      reason: "referenced",
    });
    expect(summary).toMatch(/still used by/i);
  });

  it("never echoes wire message", () => {
    const summary = describeDeleteHostError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `host referenced ${SENTINEL_OPERATOR}`,
      reason: "referenced",
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });
});

// ---------------------------------------------------------------------------
// SSH identity PATCH (rename) / DELETE
// ---------------------------------------------------------------------------

describe("validateUpdateSshIdentityRequest", () => {
  it("accepts a valid rename", () => {
    const v = validateUpdateSshIdentityRequest({ name: "new-name" });
    expect(v.ok).toBe(true);
    if (v.ok) expect(v.body).toEqual({ name: "new-name" });
  });

  it("rejects a blank name", () => {
    const v = validateUpdateSshIdentityRequest({ name: "   " });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("missing_name");
  });
});

describe("updateSshIdentity", () => {
  it("does not echo private-key material in the parsed DTO", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(200, {
        id: "iid",
        name: "new-name",
        key_type: "ed25519",
        public_key: "ssh-ed25519 AAAA",
        fingerprint_sha256: "SHA256:abc",
        created_at: "2026-05-01T00:00:00Z",
        last_used_at: null,
        // Hostile fields should not survive the parser.
        encrypted_private_key: SENTINEL_PRIVATE_KEY,
        private_key: SENTINEL_PRIVATE_KEY,
      }),
    );
    const result = await updateSshIdentity(
      "iid",
      { name: "new-name" },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(result.ok).toBe(true);
    const raw = JSON.stringify(result);
    expect(raw).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(raw).not.toContain("encrypted_private_key");
    expect(raw).not.toContain("private_key");
  });
});

describe("deleteSshIdentity", () => {
  it("returns ok on 204", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(noContentResponse());
    const result = await deleteSshIdentity("iid", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(true);
  });

  it("maps 409 ssh_identity-referenced to typed reason", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(409, {
        error: { code: "conflict", message: "ssh_identity referenced" },
      }),
    );
    const result = await deleteSshIdentity("iid", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("referenced");
    } else {
      throw new Error("expected http error with referenced reason");
    }
  });
});

describe("describeDeleteSshIdentityError", () => {
  it("guides user to remove dependent profile on referenced 409", () => {
    const summary = describeDeleteSshIdentityError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: "ssh_identity referenced",
      reason: "referenced",
    });
    expect(summary).toMatch(/saved server profile/i);
  });

  it("never echoes wire message", () => {
    const summary = describeDeleteSshIdentityError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `ssh_identity referenced ${SENTINEL_OPERATOR}`,
      reason: "referenced",
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("describeUpdateSshIdentityError", () => {
  it("never echoes wire message", () => {
    const summary = describeUpdateSshIdentityError({
      kind: "http",
      status: 404,
      code: "not_found",
      message: `ssh_identity not found ${SENTINEL_OPERATOR}`,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });
});

// ---------------------------------------------------------------------------
// Server profile PATCH / DELETE
// ---------------------------------------------------------------------------

describe("validateUpdateServerProfileRequest", () => {
  it("rejects an empty body", () => {
    const v = validateUpdateServerProfileRequest({});
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("empty_update");
  });

  it("preserves an explicit null username_override (clear intent)", () => {
    const v = validateUpdateServerProfileRequest({ username_override: null });
    expect(v.ok).toBe(true);
    if (v.ok) expect(v.body.username_override).toBeNull();
  });

  it("accepts a partial rename", () => {
    const v = validateUpdateServerProfileRequest({ name: "after" });
    expect(v.ok).toBe(true);
    if (v.ok) expect(v.body).toEqual({ name: "after" });
  });

  it("rejects an invalid username override", () => {
    const v = validateUpdateServerProfileRequest({
      username_override: "0badleadingchar",
    });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("username_override_bad_leading_char");
  });
});

describe("updateServerProfile", () => {
  it("sends username_override=null on clear and parses the response", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(200, {
        id: "pid",
        name: "p",
        host_id: "hid",
        ssh_identity_id: "iid",
        username_override: null,
        tags: [],
        created_at: "2026-05-01T00:00:00Z",
        updated_at: "2026-05-12T00:00:00Z",
        last_connected_at: null,
        disabled_at: null,
      }),
    );
    const result = await updateServerProfile(
      "pid",
      { username_override: null },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    const [, init] = fetchImpl.mock.calls[0];
    const body = JSON.parse((init as RequestInit).body as string);
    expect(body).toEqual({ username_override: null });
    expect(result.ok).toBe(true);
  });
});

describe("deleteServerProfile", () => {
  it("returns ok on 204", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(noContentResponse());
    const result = await deleteServerProfile("pid", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    expect(result.ok).toBe(true);
  });

  it("maps 409 server_profile-referenced to typed reason", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(409, {
        error: { code: "conflict", message: "server_profile referenced" },
      }),
    );
    const result = await deleteServerProfile("pid", {
      fetchImpl: fetchImpl as unknown as typeof fetch,
    });
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("referenced");
    } else {
      throw new Error("expected http error with referenced reason");
    }
  });
});

describe("describeDeleteServerProfileError", () => {
  it("suggests disable on a referenced 409", () => {
    const summary = describeDeleteServerProfileError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: "server_profile referenced",
      reason: "referenced",
    });
    expect(summary).toMatch(/disable it instead/i);
  });

  it("never echoes wire message", () => {
    const summary = describeDeleteServerProfileError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `server_profile referenced ${SENTINEL_OPERATOR}`,
      reason: "referenced",
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });
});

describe("describeUpdateServerProfileError", () => {
  it("never echoes wire message", () => {
    const summary = describeUpdateServerProfileError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `server_profile some-unique-conflict ${SENTINEL_OPERATOR}`,
    });
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });
});

// ---------------------------------------------------------------------------
// SSH identity import (POST /api/v1/ssh-identities/import)
//
// Throwaway test fixtures only — the PEM strings here are minimal
// shape-valid placeholders. The redaction sentinels (`SENTINEL_PEM`,
// `SENTINEL_OPERATOR`) prove that PEM bytes and wire `message` text
// never reach the typed result envelope or the formatter output.
// ---------------------------------------------------------------------------

const SENTINEL_PEM = "RT-SENTINEL-IMPORT-PEM-MARKER";

/** Shape-valid PEM body the local validator accepts. The backend
 * parser would reject this — the helper test never runs the real
 * parser; it only asserts the wire / formatter contracts. */
function pemFixture(extra = ""): string {
  return `-----BEGIN OPENSSH PRIVATE KEY-----\n${SENTINEL_PEM}${extra}\n-----END OPENSSH PRIVATE KEY-----\n`;
}

describe("validateImportSshIdentityRequest", () => {
  it("accepts a shape-valid pasted PEM", () => {
    const v = validateImportSshIdentityRequest({
      name: "imported",
      private_key_openssh: pemFixture(),
    });
    expect(v.ok).toBe(true);
    if (v.ok) {
      expect(v.body.name).toBe("imported");
      expect(v.body.private_key_openssh).toContain(SENTINEL_PEM);
    }
  });

  it("rejects a blank name", () => {
    const v = validateImportSshIdentityRequest({
      name: "  ",
      private_key_openssh: pemFixture(),
    });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("missing_name");
  });

  it("rejects a missing PEM", () => {
    const v = validateImportSshIdentityRequest({
      name: "imported",
      private_key_openssh: "",
    });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("missing_private_key");
  });

  it("rejects an oversized PEM", () => {
    const v = validateImportSshIdentityRequest({
      name: "imported",
      private_key_openssh: "A".repeat(MAX_PRIVATE_KEY_OPENSSH_BYTES + 1),
    });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("private_key_too_long");
  });

  it("rejects a non-ASCII PEM", () => {
    const v = validateImportSshIdentityRequest({
      name: "imported",
      private_key_openssh: `${pemFixture()}ÿ`,
    });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("private_key_not_ascii");
  });

  it("rejects a PEM with no OpenSSH header (e.g. a public-key paste)", () => {
    const v = validateImportSshIdentityRequest({
      name: "imported",
      private_key_openssh: "ssh-ed25519 AAAA-not-a-private-key",
    });
    expect(v.ok).toBe(false);
    if (!v.ok) expect(v.reason).toBe("private_key_missing_pem_header");
  });
});

describe("importSshIdentity", () => {
  it("POSTs the right endpoint with the validated body", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(201, {
        id: "iid",
        name: "imported",
        key_type: "ed25519",
        public_key: "ssh-ed25519 AAAA imported",
        fingerprint_sha256: "SHA256:abc",
        created_at: "2026-05-12T00:00:00Z",
        last_used_at: null,
      }),
    );
    const result = await importSshIdentity(
      { name: "imported", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(result.ok).toBe(true);
    const [endpoint, init] = fetchImpl.mock.calls[0];
    expect(endpoint).toBe("/api/v1/ssh-identities/import");
    expect((init as RequestInit).method).toBe("POST");
    const body = JSON.parse((init as RequestInit).body as string);
    expect(body.name).toBe("imported");
    expect(body.private_key_openssh).toContain(SENTINEL_PEM);
  });

  it("does not echo private-key material in the parsed DTO", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(201, {
        id: "iid",
        name: "imported",
        key_type: "ed25519",
        public_key: "ssh-ed25519 AAAA imported",
        fingerprint_sha256: "SHA256:abc",
        created_at: "2026-05-12T00:00:00Z",
        last_used_at: null,
        // Hostile fields the parser must drop.
        encrypted_private_key: SENTINEL_PEM,
        private_key: SENTINEL_PEM,
        private_key_openssh: SENTINEL_PEM,
      }),
    );
    const result = await importSshIdentity(
      { name: "imported", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(result.ok).toBe(true);
    const raw = JSON.stringify(result);
    expect(raw).not.toContain(SENTINEL_PEM);
    expect(raw).not.toContain("encrypted_private_key");
    expect(raw).not.toContain("private_key");
  });

  it("classifies a 409 ssh_identity-duplicate-fingerprint", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(409, {
        error: { code: "conflict", message: "ssh_identity duplicate_fingerprint" },
      }),
    );
    const result = await importSshIdentity(
      { name: "dup", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("duplicate_fingerprint");
    } else {
      throw new Error("expected http duplicate_fingerprint reason");
    }
  });

  it("classifies a 400 unsupported_key_format encrypted", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(400, {
        error: { code: "invalid_input", message: "unsupported_key_format encrypted" },
      }),
    );
    const result = await importSshIdentity(
      { name: "enc", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("unsupported_format_encrypted");
    } else {
      throw new Error("expected unsupported_format_encrypted reason");
    }
  });

  it("classifies a 400 unsupported_key_format malformed", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(400, {
        error: { code: "invalid_input", message: "unsupported_key_format malformed" },
      }),
    );
    const result = await importSshIdentity(
      { name: "bad", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("unsupported_format_malformed");
    } else {
      throw new Error("expected unsupported_format_malformed reason");
    }
  });

  it('classifies a 400 unsupported key_type "rsa"', async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      jsonResponse(400, {
        error: { code: "invalid_input", message: 'unsupported key_type "rsa"' },
      }),
    );
    const result = await importSshIdentity(
      { name: "rsa", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    if (!result.ok && result.error.kind === "http") {
      expect(result.error.reason).toBe("unsupported_key_type");
    } else {
      throw new Error("expected unsupported_key_type reason");
    }
  });

  it("does not echo PEM bytes on a transport failure", async () => {
    const fetchImpl = vi.fn().mockRejectedValueOnce(
      new Error(`transport oops ${SENTINEL_OPERATOR}`),
    );
    const result = await importSshIdentity(
      { name: "x", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(result.ok).toBe(false);
    const raw = JSON.stringify(result);
    expect(raw).not.toContain(SENTINEL_PEM);
  });

  it("collapses a non-JSON response to malformed_response without echoing PEM", async () => {
    const fetchImpl = vi.fn().mockResolvedValueOnce(
      new Response("not json", {
        status: 201,
        headers: { "content-type": "application/json" },
      }),
    );
    const result = await importSshIdentity(
      { name: "x", private_key_openssh: pemFixture() },
      { fetchImpl: fetchImpl as unknown as typeof fetch },
    );
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.kind).toBe("malformed_response");
    }
    expect(JSON.stringify(result)).not.toContain(SENTINEL_PEM);
  });
});

describe("describeImportSshIdentityError", () => {
  it("guides operator past a duplicate-fingerprint 409", () => {
    const summary = describeImportSshIdentityError({
      kind: "http",
      status: 409,
      code: "conflict",
      message: `ssh_identity duplicate_fingerprint ${SENTINEL_OPERATOR}`,
      reason: "duplicate_fingerprint",
    });
    expect(summary).toMatch(/already imported/i);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("explains encrypted-key rejection without echoing wire message", () => {
    const summary = describeImportSshIdentityError({
      kind: "http",
      status: 400,
      code: "invalid_input",
      message: `unsupported_key_format encrypted ${SENTINEL_OPERATOR}`,
      reason: "unsupported_format_encrypted",
    });
    expect(summary).toMatch(/passphrase/i);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("explains malformed-PEM rejection without echoing wire message", () => {
    const summary = describeImportSshIdentityError({
      kind: "http",
      status: 400,
      code: "invalid_input",
      message: `unsupported_key_format malformed ${SENTINEL_OPERATOR}`,
      reason: "unsupported_format_malformed",
    });
    expect(summary).toMatch(/not a valid OpenSSH private key/i);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("explains RSA / non-Ed25519 rejection without echoing wire message", () => {
    const summary = describeImportSshIdentityError({
      kind: "http",
      status: 400,
      code: "invalid_input",
      message: `unsupported key_type "rsa" ${SENTINEL_OPERATOR}`,
      reason: "unsupported_key_type",
    });
    expect(summary).toMatch(/Ed25519/);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("flat-maps a CSRF 403 to safe browser-policy copy", () => {
    const summary = describeImportSshIdentityError({
      kind: "http",
      status: 403,
      code: "csrf_origin_mismatch",
      message: `forbidden ${SENTINEL_OPERATOR}`,
      reason: null,
    });
    expect(summary).toMatch(/browser security policy/i);
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("never echoes Error.message on a transport failure", () => {
    const summary = describeImportSshIdentityError({
      kind: "transport",
      message: `boom ${SENTINEL_OPERATOR}`,
    });
    expect(summary).toBe("Cannot import SSH identity: transport error");
    expect(summary).not.toContain(SENTINEL_OPERATOR);
  });

  it("renders a stable 503 vault-disabled summary", () => {
    const summary = describeImportSshIdentityError({
      kind: "http",
      status: 503,
      code: "service_unavailable",
      message: "service unavailable",
      reason: null,
    });
    expect(summary).toMatch(/vault is not configured/i);
  });
});
