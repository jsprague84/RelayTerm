import { describe, expect, it } from "vitest";
import { describeBackendUrlError } from "../src/lib/runtime/backendUrlError.js";
import type { BackendUrlError } from "../src/lib/runtime/backendConfig.js";

/**
 * Pin the picker UI's mapping from typed validation reasons (design
 * § 10) to user-facing strings. Each branch returns a static sentence
 * — the input URL is never echoed and no sentinel-shaped substring
 * leaks into the message.
 */

const ALL_REASONS: BackendUrlError[] = [
  "url_empty",
  "url_too_long",
  "url_parse_failed",
  "url_credentials_forbidden",
  "url_scheme_forbidden",
  "url_http_non_localhost",
  "url_path_forbidden",
  "url_search_forbidden",
  "url_hash_forbidden",
];

describe("describeBackendUrlError", () => {
  it.each(ALL_REASONS)("returns a non-empty static string for %s", (reason) => {
    const message = describeBackendUrlError(reason);
    expect(typeof message).toBe("string");
    expect(message.trim().length).toBeGreaterThan(0);
  });

  it("returns distinct strings for distinct reasons (no accidental fallthrough)", () => {
    const seen = new Set<string>();
    for (const reason of ALL_REASONS) {
      seen.add(describeBackendUrlError(reason));
    }
    expect(seen.size).toBe(ALL_REASONS.length);
  });

  it("the credentials-forbidden message warns about credentials in the URL", () => {
    expect(describeBackendUrlError("url_credentials_forbidden")).toMatch(
      /credentials|username|password/i,
    );
  });

  it("the http-non-localhost message points the operator at https", () => {
    expect(describeBackendUrlError("url_http_non_localhost")).toMatch(
      /https/i,
    );
  });

  it("none of the messages echo a sentinel-shaped substring (defence in depth)", () => {
    const SENTINEL = "RELAY_SENTINEL_URLERR_LEAK_9301";
    for (const reason of ALL_REASONS) {
      expect(describeBackendUrlError(reason)).not.toContain(SENTINEL);
    }
  });
});
