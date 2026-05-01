import { describe, expect, it } from "vitest";

import type { Host } from "../src/lib/api/hosts.js";
import type { ServerProfile } from "../src/lib/api/serverProfiles.js";
import {
  publicKeyPreview,
  type SshIdentity,
} from "../src/lib/api/sshIdentities.js";
import {
  describeReadinessFromKnownState,
  hostProfileCount,
  identityPublicDetail,
  identitySummary,
  publicKeyCopyValue,
  relatedProfilesForHost,
  resolveProfileDetail,
  safeDisplayValue,
  shortId,
} from "../src/lib/app/inventory/inventoryDetails.js";

/**
 * Sentinels that MUST NEVER appear in any user-visible string, parsed
 * detail object, or formatted summary returned by the helpers under
 * test. The redaction rule for the inventory detail surface mirrors
 * the rule pinned by `inventoryApi.test.ts`:
 *
 *  - `private_key` / `encrypted_private_key` fields never reach the
 *    detail-panel projection of an SSH identity, even if a hostile
 *    fixture smuggles them onto the input object.
 *  - The full OpenSSH public key reaches the UI only through the
 *    deliberate `publicKeyCopyValue` helper. The detail summary uses
 *    a truncated preview so the full key cannot leak through an
 *    incidental hover surface.
 */
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_DETAIL_8823";

const HOST_A: Host = {
  id: "11111111-1111-1111-1111-111111111111",
  display_name: "edge-1",
  hostname: "edge-1.example.internal",
  port: 22,
  default_username: "deploy",
  created_at: "2026-04-29T00:00:00Z",
  updated_at: "2026-04-29T00:01:00Z",
};

const HOST_B: Host = {
  id: "11111111-1111-1111-1111-222222222222",
  display_name: "edge-2",
  hostname: "edge-2.example.internal",
  port: 2222,
  default_username: "ops",
  created_at: "2026-04-29T01:00:00Z",
  updated_at: "2026-04-29T01:00:00Z",
};

const IDENTITY_A: SshIdentity = {
  id: "33333333-3333-3333-3333-333333333333",
  name: "primary",
  key_type: "ed25519",
  public_key:
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleSSHPublicKeyBase64Body relay@example",
  fingerprint_sha256: "SHA256:abcdefg",
  created_at: "2026-04-29T00:00:00Z",
  last_used_at: null,
};

function profile(
  id: string,
  hostId: string,
  identityId: string,
  override: string | null = null,
): ServerProfile {
  return {
    id,
    name: `profile-${id.slice(0, 4)}`,
    host_id: hostId,
    ssh_identity_id: identityId,
    username_override: override,
    tags: [],
    created_at: "2026-04-29T00:00:00Z",
    updated_at: "2026-04-29T00:00:00Z",
    last_connected_at: null,
  };
}

describe("shortId", () => {
  it("truncates a UUID with the ellipsis suffix", () => {
    expect(shortId("11111111-1111-1111-1111-111111111111")).toBe("11111111…");
  });

  it("returns the input unchanged when shorter than the prefix length", () => {
    expect(shortId("abc")).toBe("abc");
  });

  it("returns an empty string for non-string input", () => {
    expect(shortId(undefined as unknown as string)).toBe("");
  });
});

describe("safeDisplayValue", () => {
  it("returns the supplied placeholder for null / undefined / empty", () => {
    expect(safeDisplayValue(null)).toBe("—");
    expect(safeDisplayValue(undefined)).toBe("—");
    expect(safeDisplayValue("")).toBe("—");
  });

  it("returns the supplied value when non-empty", () => {
    expect(safeDisplayValue("ok")).toBe("ok");
  });

  it("honours a custom placeholder", () => {
    expect(safeDisplayValue(null, "(unknown)")).toBe("(unknown)");
  });
});

describe("hostProfileCount", () => {
  it("counts only profiles whose host_id matches the host", () => {
    const profiles: ServerProfile[] = [
      profile("a", HOST_A.id, IDENTITY_A.id),
      profile("b", HOST_A.id, IDENTITY_A.id),
      profile("c", HOST_B.id, IDENTITY_A.id),
    ];
    expect(hostProfileCount(HOST_A, profiles)).toBe(2);
    expect(hostProfileCount(HOST_B, profiles)).toBe(1);
  });

  it("returns 0 when no profile is linked", () => {
    expect(hostProfileCount(HOST_A, [])).toBe(0);
  });
});

describe("relatedProfilesForHost", () => {
  it("preserves input order so the panel matches the main list", () => {
    const profiles: ServerProfile[] = [
      profile("a", HOST_A.id, IDENTITY_A.id),
      profile("b", HOST_B.id, IDENTITY_A.id),
      profile("c", HOST_A.id, IDENTITY_A.id),
    ];
    const result = relatedProfilesForHost(HOST_A, profiles);
    expect(result.map((p) => p.id)).toEqual(["a", "c"]);
  });

  it("returns an empty array when nothing matches", () => {
    expect(
      relatedProfilesForHost(
        HOST_A,
        [profile("c", HOST_B.id, IDENTITY_A.id)],
      ),
    ).toEqual([]);
  });
});

describe("identitySummary", () => {
  it("returns only public metadata fields", () => {
    const summary = identitySummary(IDENTITY_A);
    expect(summary).toEqual({
      id: IDENTITY_A.id,
      name: IDENTITY_A.name,
      key_type: IDENTITY_A.key_type,
      fingerprint_sha256: IDENTITY_A.fingerprint_sha256,
    });
  });

  it("redaction sentinel: private-key fields on the input do not reach the summary", () => {
    const hostile = {
      ...IDENTITY_A,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
    } as unknown as SshIdentity;
    const summary = identitySummary(hostile);
    expect(JSON.stringify(summary)).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(
      Object.prototype.hasOwnProperty.call(summary, "encrypted_private_key"),
    ).toBe(false);
    expect(
      Object.prototype.hasOwnProperty.call(summary, "private_key"),
    ).toBe(false);
  });
});

describe("resolveProfileDetail", () => {
  const profileFixture = profile("p1", HOST_A.id, IDENTITY_A.id);

  it("resolves both host and identity when present in the supplied lists", () => {
    const detail = resolveProfileDetail(
      profileFixture,
      [HOST_A, HOST_B],
      [IDENTITY_A],
    );
    expect(detail.profile).toBe(profileFixture);
    expect(detail.links.host).toBe(HOST_A);
    expect(detail.links.effectiveUsername).toBe(HOST_A.default_username);
    expect(detail.links.inheritedFromHost).toBe(true);
    expect(detail.identity).toEqual({
      id: IDENTITY_A.id,
      name: IDENTITY_A.name,
      key_type: IDENTITY_A.key_type,
      fingerprint_sha256: IDENTITY_A.fingerprint_sha256,
    });
  });

  it("renders an unresolved host honestly without synthesising a placeholder", () => {
    const detail = resolveProfileDetail(profileFixture, [], [IDENTITY_A]);
    expect(detail.links.host).toBeNull();
    expect(detail.links.effectiveUsername).toBeNull();
    expect(detail.identity).not.toBeNull();
  });

  it("renders an unresolved identity honestly without synthesising a placeholder", () => {
    const detail = resolveProfileDetail(profileFixture, [HOST_A], []);
    expect(detail.identity).toBeNull();
    expect(detail.links.host).toBe(HOST_A);
  });

  it("propagates a username override over the host default", () => {
    const overridden = profile("p1", HOST_A.id, IDENTITY_A.id, "alice");
    const detail = resolveProfileDetail(overridden, [HOST_A], [IDENTITY_A]);
    expect(detail.links.effectiveUsername).toBe("alice");
    expect(detail.links.inheritedFromHost).toBe(false);
  });

  it("redaction sentinel: private-key fields on identity input do not reach the detail", () => {
    const hostile = {
      ...IDENTITY_A,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
    } as unknown as SshIdentity;
    const detail = resolveProfileDetail(profileFixture, [HOST_A], [hostile]);
    expect(JSON.stringify(detail)).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(detail.identity).not.toBeNull();
    if (detail.identity) {
      expect(
        Object.prototype.hasOwnProperty.call(
          detail.identity,
          "encrypted_private_key",
        ),
      ).toBe(false);
      expect(
        Object.prototype.hasOwnProperty.call(detail.identity, "private_key"),
      ).toBe(false);
    }
  });
});

describe("describeReadinessFromKnownState", () => {
  const profileFixture = profile("p1", HOST_A.id, IDENTITY_A.id);

  it("does not imply trust or auth success when both links resolve", () => {
    const detail = resolveProfileDetail(
      profileFixture,
      [HOST_A],
      [IDENTITY_A],
    );
    const hint = describeReadinessFromKnownState(detail);
    expect(hint.hostLinkResolved).toBe(true);
    expect(hint.identityLinkResolved).toBe(true);
    // Must not say "ready", "trusted", "verified", or "passed" — the
    // detail panel cannot prove any of those from list data alone.
    expect(hint.advisory).not.toMatch(/ready|trusted|verified|passed/i);
    expect(hint.advisory).toMatch(/host-key trust/i);
    expect(hint.advisory).toMatch(/auth-check/i);
  });

  it("flags an unresolved host link without echoing wire data", () => {
    const detail = resolveProfileDetail(profileFixture, [], [IDENTITY_A]);
    const hint = describeReadinessFromKnownState(detail);
    expect(hint.hostLinkResolved).toBe(false);
    expect(hint.advisory.toLowerCase()).toContain("host link");
  });

  it("flags an unresolved identity link", () => {
    const detail = resolveProfileDetail(profileFixture, [HOST_A], []);
    const hint = describeReadinessFromKnownState(detail);
    expect(hint.identityLinkResolved).toBe(false);
    expect(hint.advisory.toLowerCase()).toContain("ssh identity");
  });

  it("flags both links unresolved when neither is in the supplied lists", () => {
    const detail = resolveProfileDetail(profileFixture, [], []);
    const hint = describeReadinessFromKnownState(detail);
    expect(hint.hostLinkResolved).toBe(false);
    expect(hint.identityLinkResolved).toBe(false);
  });
});

describe("identityPublicDetail", () => {
  it("uses the supplied preview function — full key is not embedded", () => {
    const detail = identityPublicDetail(IDENTITY_A, publicKeyPreview);
    expect(detail.publicKeyPreview).not.toBe(IDENTITY_A.public_key);
    expect(detail.publicKeyPreview.length).toBeLessThan(
      IDENTITY_A.public_key.length,
    );
    // The full key SHOULD NOT appear in the detail summary surface.
    expect(JSON.stringify(detail)).not.toContain(
      "AAAAC3NzaC1lZDI1NTE5AAAAIExampleSSHPublicKeyBase64Body",
    );
  });

  it("redaction sentinel: private-key fields on input do not reach the detail", () => {
    const hostile = {
      ...IDENTITY_A,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
    } as unknown as SshIdentity;
    const detail = identityPublicDetail(hostile, publicKeyPreview);
    expect(JSON.stringify(detail)).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(
      Object.prototype.hasOwnProperty.call(detail, "encrypted_private_key"),
    ).toBe(false);
    expect(Object.prototype.hasOwnProperty.call(detail, "private_key")).toBe(
      false,
    );
  });

  it("preserves last_used_at as null when the identity has never authenticated", () => {
    const detail = identityPublicDetail(IDENTITY_A, publicKeyPreview);
    expect(detail.last_used_at).toBeNull();
  });
});

describe("publicKeyCopyValue", () => {
  it("yields the full public key — the deliberate copy action's value", () => {
    expect(publicKeyCopyValue(IDENTITY_A)).toBe(IDENTITY_A.public_key);
  });

  it("redaction sentinel: never includes a private-key field even when the input has one", () => {
    const hostile = {
      ...IDENTITY_A,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
    } as unknown as SshIdentity;
    const value = publicKeyCopyValue(hostile);
    expect(value).toBe(IDENTITY_A.public_key);
    expect(value).not.toContain(SENTINEL_PRIVATE_KEY);
  });
});
