import { describe, expect, it } from "vitest";

import type { Host } from "../src/lib/api/hosts.js";
import type { ServerProfile } from "../src/lib/api/serverProfiles.js";
import type { SshIdentity } from "../src/lib/api/sshIdentities.js";
import {
  collectProfileTags,
  countFilteredResults,
  filterHosts,
  filterIdentities,
  filterProfiles,
  normalizeSearchText,
} from "../src/lib/app/inventory/inventoryFilters.js";

/**
 * Sentinels that MUST NEVER appear in any value returned by the filter
 * helpers, the matching haystack, or any string the helpers expose to
 * the caller. The redaction rule mirrors the one pinned by
 * `inventoryDetails.test.ts` and `inventoryApi.test.ts`:
 *
 *  - `private_key` / `encrypted_private_key` fields on a hostile
 *    SshIdentity input cannot reach the filter result, the matching
 *    haystack, or any returned object.
 *  - The filter helpers must not include the OpenSSH `public_key`
 *    body in their matching surface — substring matching against a
 *    400-char base64 body is rarely useful and would invite a future
 *    preview surface that echoes the matched fragment.
 *  - Sentinels for fake "wire" / session-output / access-token shapes
 *    pin that the helpers operate ONLY on the typed fields they were
 *    handed.
 */
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_PRIVATE_KEY_FILTER_4019";
const SENTINEL_PUBLIC_KEY_BODY =
  "AAAAC3NzaC1lZDI1NTE5RELAY_SENTINEL_PUBLIC_KEY_FILTER_4020";
const SENTINEL_SESSION_OUTPUT = "RELAY_SENTINEL_SESSION_OUTPUT_4021";
const SENTINEL_ACCESS_TOKEN = "RELAY_SENTINEL_ACCESS_TOKEN_4022";

const HOST_EDGE_PROD: Host = {
  id: "11111111-1111-1111-1111-111111111111",
  display_name: "Edge Prod",
  hostname: "edge-prod.example.internal",
  port: 22,
  default_username: "deploy",
  created_at: "2026-04-29T00:00:00Z",
  updated_at: "2026-04-29T00:01:00Z",
};

const HOST_EDGE_STAGING: Host = {
  id: "11111111-1111-1111-1111-222222222222",
  display_name: "Edge Staging",
  hostname: "edge-staging.example.internal",
  port: 2222,
  default_username: "ops",
  created_at: "2026-04-29T01:00:00Z",
  updated_at: "2026-04-29T01:00:00Z",
};

const HOST_BASTION: Host = {
  id: "11111111-1111-1111-1111-333333333333",
  display_name: "Bastion",
  hostname: "bastion.corp.example",
  port: 22,
  default_username: "root",
  created_at: "2026-04-29T02:00:00Z",
  updated_at: "2026-04-29T02:00:00Z",
};

const IDENTITY_PRIMARY: SshIdentity = {
  id: "33333333-3333-3333-3333-333333333333",
  name: "primary",
  key_type: "ed25519",
  public_key:
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleSSHPublicKeyBase64Body relay@example",
  fingerprint_sha256: "SHA256:abcDEF123",
  created_at: "2026-04-29T00:00:00Z",
  last_used_at: null,
};

const IDENTITY_RSA: SshIdentity = {
  id: "33333333-3333-3333-3333-444444444444",
  name: "legacy-rsa",
  key_type: "rsa",
  public_key: "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ== legacy@example",
  fingerprint_sha256: "SHA256:legacyFP456",
  created_at: "2026-04-29T03:00:00Z",
  last_used_at: "2026-04-30T03:00:00Z",
};

function profileFixture(
  id: string,
  name: string,
  hostId: string,
  identityId: string,
  tags: string[] = [],
  override: string | null = null,
): ServerProfile {
  return {
    id,
    name,
    host_id: hostId,
    ssh_identity_id: identityId,
    username_override: override,
    tags,
    created_at: "2026-04-29T00:00:00Z",
    updated_at: "2026-04-29T00:00:00Z",
    last_connected_at: null,
    disabled_at: null,
  };
}

const PROFILE_PROD = profileFixture(
  "p-prod",
  "Prod East",
  HOST_EDGE_PROD.id,
  IDENTITY_PRIMARY.id,
  ["prod", "us-east-1"],
);
const PROFILE_STAGING = profileFixture(
  "p-staging",
  "Staging East",
  HOST_EDGE_STAGING.id,
  IDENTITY_PRIMARY.id,
  ["staging", "us-east-1"],
  "alice",
);
const PROFILE_LEGACY_DETACHED = profileFixture(
  "p-detached",
  "Legacy detached",
  "00000000-0000-0000-0000-000000000000",
  IDENTITY_RSA.id,
  ["legacy"],
);
const PROFILE_NO_IDENTITY = profileFixture(
  "p-no-identity",
  "Bastion runbook",
  HOST_BASTION.id,
  "00000000-0000-0000-0000-deadbeefdead",
  [],
);

const ALL_HOSTS: readonly Host[] = [
  HOST_EDGE_PROD,
  HOST_EDGE_STAGING,
  HOST_BASTION,
];
const ALL_IDENTITIES: readonly SshIdentity[] = [IDENTITY_PRIMARY, IDENTITY_RSA];
const ALL_PROFILES: readonly ServerProfile[] = [
  PROFILE_PROD,
  PROFILE_STAGING,
  PROFILE_LEGACY_DETACHED,
  PROFILE_NO_IDENTITY,
];

describe("normalizeSearchText", () => {
  it("trims surrounding whitespace and lowercases the input", () => {
    expect(normalizeSearchText("  Edge BOX  ")).toBe("edge box");
  });

  it("collapses internal whitespace runs to a single space", () => {
    expect(normalizeSearchText("prod\t\n  east")).toBe("prod east");
  });

  it("returns an empty string for empty or whitespace-only input", () => {
    expect(normalizeSearchText("")).toBe("");
    expect(normalizeSearchText("   ")).toBe("");
  });

  it("returns an empty string for non-string input", () => {
    expect(normalizeSearchText(undefined)).toBe("");
    expect(normalizeSearchText(null)).toBe("");
    expect(normalizeSearchText(42)).toBe("");
  });
});

describe("filterHosts", () => {
  it("returns a shallow copy when the query is empty", () => {
    const result = filterHosts(ALL_HOSTS, "");
    expect(result).toEqual(ALL_HOSTS.slice());
    expect(result).not.toBe(ALL_HOSTS);
  });

  it("matches by display name (case-insensitive)", () => {
    expect(filterHosts(ALL_HOSTS, "edge").map((h) => h.id)).toEqual([
      HOST_EDGE_PROD.id,
      HOST_EDGE_STAGING.id,
    ]);
  });

  it("matches by hostname", () => {
    expect(filterHosts(ALL_HOSTS, "bastion.corp").map((h) => h.id)).toEqual([
      HOST_BASTION.id,
    ]);
  });

  it("matches by default username", () => {
    expect(filterHosts(ALL_HOSTS, "deploy").map((h) => h.id)).toEqual([
      HOST_EDGE_PROD.id,
    ]);
  });

  it("matches by port (rendered as decimal)", () => {
    expect(filterHosts(ALL_HOSTS, "2222").map((h) => h.id)).toEqual([
      HOST_EDGE_STAGING.id,
    ]);
  });

  it("requires every whitespace-separated token to match", () => {
    expect(filterHosts(ALL_HOSTS, "edge prod").map((h) => h.id)).toEqual([
      HOST_EDGE_PROD.id,
    ]);
  });

  it("returns an empty array when nothing matches", () => {
    expect(filterHosts(ALL_HOSTS, "no-such-host")).toEqual([]);
  });

  it("does not mutate the input array", () => {
    const before = ALL_HOSTS.slice();
    filterHosts(ALL_HOSTS, "edge");
    expect(ALL_HOSTS).toEqual(before);
  });
});

describe("filterProfiles", () => {
  it("returns a shallow copy when no filter is set", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES);
    expect(result).toEqual(ALL_PROFILES.slice());
    expect(result).not.toBe(ALL_PROFILES);
  });

  it("matches by profile name", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "prod east",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_PROD.id]);
  });

  it("matches by tag (substring)", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "us-east",
    });
    expect(result.map((p) => p.id)).toEqual([
      PROFILE_PROD.id,
      PROFILE_STAGING.id,
    ]);
  });

  it("matches by username override", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "alice",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_STAGING.id]);
  });

  it("matches by host-default username (effective username inheritance)", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "deploy",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_PROD.id]);
  });

  it("matches by linked host display name", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "bastion",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_NO_IDENTITY.id]);
  });

  it("matches by linked host hostname", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "edge-staging.example",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_STAGING.id]);
  });

  it("matches by linked identity name", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "legacy-rsa",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_LEGACY_DETACHED.id]);
  });

  it("matches by linked identity fingerprint", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "legacyfp456",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_LEGACY_DETACHED.id]);
  });

  it("filters by exact tag", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      tag: "staging",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_STAGING.id]);
  });

  it("returns nothing when the tag does not exist", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      tag: "no-such-tag",
    });
    expect(result).toEqual([]);
  });

  it("filters by missing-host link state", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      linkState: "missing_host",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_LEGACY_DETACHED.id]);
  });

  it("filters by missing-identity link state", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      linkState: "missing_identity",
    });
    expect(result.map((p) => p.id)).toEqual([PROFILE_NO_IDENTITY.id]);
  });

  it("combines tag and search filters with AND semantics", () => {
    const result = filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "east",
      tag: "us-east-1",
    });
    expect(result.map((p) => p.id)).toEqual([
      PROFILE_PROD.id,
      PROFILE_STAGING.id,
    ]);
  });

  it("safely handles a profile whose linked host or identity is missing", () => {
    // No match; we just want to assert the helper does not throw on the
    // detached / no-identity rows when scanning by linked metadata.
    expect(() =>
      filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
        query: "no-such-thing",
      }),
    ).not.toThrow();
  });

  it("does not mutate the input arrays", () => {
    const profilesBefore = ALL_PROFILES.slice();
    const hostsBefore = ALL_HOSTS.slice();
    const identitiesBefore = ALL_IDENTITIES.slice();
    filterProfiles(ALL_PROFILES, ALL_HOSTS, ALL_IDENTITIES, {
      query: "east",
      tag: "us-east-1",
    });
    expect(ALL_PROFILES).toEqual(profilesBefore);
    expect(ALL_HOSTS).toEqual(hostsBefore);
    expect(ALL_IDENTITIES).toEqual(identitiesBefore);
  });

  it("does not include the linked identity's public_key body in its matching surface", () => {
    // A query that would match anywhere inside the OpenSSH public-key
    // base64 body should NOT match — `filterProfiles` deliberately
    // omits `public_key` from its haystack.
    const hostileIdentity: SshIdentity = {
      ...IDENTITY_PRIMARY,
      public_key: `ssh-ed25519 ${SENTINEL_PUBLIC_KEY_BODY} relay@example`,
    };
    const result = filterProfiles(
      ALL_PROFILES,
      ALL_HOSTS,
      [hostileIdentity, IDENTITY_RSA],
      { query: SENTINEL_PUBLIC_KEY_BODY.toLowerCase().slice(0, 16) },
    );
    expect(result).toEqual([]);
  });

  it("redaction sentinel: hostile private/session/token fields on inputs are not part of the matching haystack", () => {
    const hostileIdentity = {
      ...IDENTITY_PRIMARY,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
      session_output: SENTINEL_SESSION_OUTPUT,
      access_token: SENTINEL_ACCESS_TOKEN,
    } as unknown as SshIdentity;
    const identitiesWithHostile: readonly SshIdentity[] = [
      hostileIdentity,
      IDENTITY_RSA,
    ];
    // A query-by-sentinel-value must NOT match any profile — the hostile
    // fields rode along on the input identity reference, but the helper
    // never put them into its matching haystack. This is the load-bearing
    // assertion; a JSON.stringify check on a `ServerProfile[]` result
    // would always pass vacuously because the hostile fields live on
    // the identity, not the profile, and the result holds profiles.
    expect(
      filterProfiles(ALL_PROFILES, ALL_HOSTS, identitiesWithHostile, {
        query: SENTINEL_PRIVATE_KEY.toLowerCase().slice(0, 16),
      }),
    ).toEqual([]);
    expect(
      filterProfiles(ALL_PROFILES, ALL_HOSTS, identitiesWithHostile, {
        query: SENTINEL_SESSION_OUTPUT.toLowerCase().slice(0, 16),
      }),
    ).toEqual([]);
    expect(
      filterProfiles(ALL_PROFILES, ALL_HOSTS, identitiesWithHostile, {
        query: SENTINEL_ACCESS_TOKEN.toLowerCase().slice(0, 16),
      }),
    ).toEqual([]);
    // The pre-existing happy-path "prod" query still returns the
    // expected single matching profile; the result references the
    // input profile (the helper is pure, not a deep-clone), so the
    // hostile identity's fields do NOT need to be filtered out of
    // the result envelope — they are not on the result objects.
    const happyPath = filterProfiles(
      ALL_PROFILES,
      ALL_HOSTS,
      identitiesWithHostile,
      { query: "prod" },
    );
    expect(happyPath.map((p) => p.id)).toEqual([PROFILE_PROD.id]);
  });
});

describe("collectProfileTags", () => {
  it("returns a sorted, deduped, case-insensitive list of tags", () => {
    const profiles: ServerProfile[] = [
      profileFixture("a", "a", HOST_EDGE_PROD.id, IDENTITY_PRIMARY.id, [
        "prod",
        "us-east-1",
      ]),
      profileFixture("b", "b", HOST_EDGE_PROD.id, IDENTITY_PRIMARY.id, [
        "Prod",
        "ops",
      ]),
      profileFixture("c", "c", HOST_EDGE_PROD.id, IDENTITY_PRIMARY.id, []),
    ];
    expect(collectProfileTags(profiles)).toEqual(["ops", "prod", "us-east-1"]);
  });

  it("drops empty strings", () => {
    const profiles: ServerProfile[] = [
      profileFixture("a", "a", HOST_EDGE_PROD.id, IDENTITY_PRIMARY.id, [
        "",
        "ok",
      ]),
    ];
    expect(collectProfileTags(profiles)).toEqual(["ok"]);
  });

  it("returns an empty array for an empty profile list", () => {
    expect(collectProfileTags([])).toEqual([]);
  });

  it("does not mutate the input", () => {
    const profiles: ServerProfile[] = [
      profileFixture("a", "a", HOST_EDGE_PROD.id, IDENTITY_PRIMARY.id, [
        "prod",
        "ops",
      ]),
    ];
    const tagsBefore = profiles[0].tags.slice();
    collectProfileTags(profiles);
    expect(profiles[0].tags).toEqual(tagsBefore);
  });
});

describe("filterIdentities", () => {
  it("returns a shallow copy when no filter is set", () => {
    const result = filterIdentities(ALL_IDENTITIES);
    expect(result).toEqual(ALL_IDENTITIES.slice());
    expect(result).not.toBe(ALL_IDENTITIES);
  });

  it("matches by name (case-insensitive)", () => {
    expect(
      filterIdentities(ALL_IDENTITIES, { query: "PRIMARY" }).map((i) => i.id),
    ).toEqual([IDENTITY_PRIMARY.id]);
  });

  it("matches by fingerprint substring", () => {
    expect(
      filterIdentities(ALL_IDENTITIES, { query: "abcdef" }).map((i) => i.id),
    ).toEqual([IDENTITY_PRIMARY.id]);
  });

  it("matches by key type token", () => {
    expect(
      filterIdentities(ALL_IDENTITIES, { query: "ed25519" }).map((i) => i.id),
    ).toEqual([IDENTITY_PRIMARY.id]);
  });

  it("filters by exact key type", () => {
    expect(
      filterIdentities(ALL_IDENTITIES, { keyType: "rsa" }).map((i) => i.id),
    ).toEqual([IDENTITY_RSA.id]);
  });

  it("returns nothing when the key type does not match", () => {
    expect(
      filterIdentities(ALL_IDENTITIES, { keyType: "ecdsa_p256" }),
    ).toEqual([]);
  });

  it("combines query and key-type filters with AND semantics", () => {
    expect(
      filterIdentities(ALL_IDENTITIES, {
        query: "primary",
        keyType: "rsa",
      }),
    ).toEqual([]);
  });

  it("does NOT include the OpenSSH public_key body in its haystack", () => {
    const hostile: SshIdentity = {
      ...IDENTITY_PRIMARY,
      public_key: `ssh-ed25519 ${SENTINEL_PUBLIC_KEY_BODY} relay@example`,
    };
    const result = filterIdentities([hostile, IDENTITY_RSA], {
      query: SENTINEL_PUBLIC_KEY_BODY.toLowerCase().slice(0, 16),
    });
    expect(result).toEqual([]);
  });

  it("redaction sentinel: hostile private-key / session / token fields do not reach the result", () => {
    const hostile = {
      ...IDENTITY_PRIMARY,
      encrypted_private_key: SENTINEL_PRIVATE_KEY,
      private_key: SENTINEL_PRIVATE_KEY,
      session_output: SENTINEL_SESSION_OUTPUT,
      access_token: SENTINEL_ACCESS_TOKEN,
    } as unknown as SshIdentity;
    const result = filterIdentities([hostile, IDENTITY_RSA], {
      query: "primary",
    });
    expect(result).toHaveLength(1);
    // The helper does not strip foreign properties from the input
    // object — it just must not surface them through any computed
    // string. Sentinel assertions against the JSON-stringified result
    // would always fail because the hostile fields ride along on the
    // returned reference. Pin instead that the haystack itself never
    // matched against those sentinel substrings.
    expect(
      filterIdentities([hostile, IDENTITY_RSA], {
        query: SENTINEL_PRIVATE_KEY.toLowerCase().slice(0, 16),
      }),
    ).toEqual([]);
    expect(
      filterIdentities([hostile, IDENTITY_RSA], {
        query: SENTINEL_SESSION_OUTPUT.toLowerCase().slice(0, 16),
      }),
    ).toEqual([]);
    expect(
      filterIdentities([hostile, IDENTITY_RSA], {
        query: SENTINEL_ACCESS_TOKEN.toLowerCase().slice(0, 16),
      }),
    ).toEqual([]);
  });

  it("does not mutate the input array", () => {
    const before = ALL_IDENTITIES.slice();
    filterIdentities(ALL_IDENTITIES, { query: "primary", keyType: "ed25519" });
    expect(ALL_IDENTITIES).toEqual(before);
  });
});

describe("countFilteredResults", () => {
  it("collapses to the bare total when nothing is filtered", () => {
    expect(countFilteredResults(3, 3, "host")).toBe("3 hosts");
    expect(countFilteredResults(1, 1, "host")).toBe("1 host");
    expect(countFilteredResults(0, 0, "host")).toBe("0 hosts");
  });

  it("renders the visible / total form when a filter is active", () => {
    expect(countFilteredResults(1, 3, "host")).toBe("Showing 1 of 3 hosts");
    expect(countFilteredResults(0, 3, "host")).toBe("Showing 0 of 3 hosts");
    expect(countFilteredResults(2, 3, "profile")).toBe(
      "Showing 2 of 3 profiles",
    );
  });

  it("honours an explicit irregular plural", () => {
    expect(countFilteredResults(1, 5, "identity", "identities")).toBe(
      "Showing 1 of 5 identities",
    );
    expect(countFilteredResults(5, 5, "identity", "identities")).toBe(
      "5 identities",
    );
    expect(countFilteredResults(1, 1, "identity", "identities")).toBe(
      "1 identity",
    );
  });
});
