import { describe, it, expect } from "vitest";
import {
  BACKEND_CONFIG_STORAGE_KEY,
  BACKEND_URL_MAX_LEN,
  validateBackendOrigin,
  deriveApiBaseUrl,
  deriveHealthUrl,
  deriveWebSocketBaseUrl,
  parseStoredBackendConfig,
  serializeBackendConfig,
  loadBackendConfig,
  saveBackendConfig,
  clearBackendConfig,
  type BackendConfig,
  type BackendConfigStorage,
} from "../src/lib/runtime/backendConfig.js";

/**
 * Sentinels used to pin the redaction posture of the backend-URL
 * primitive. None of these substrings may appear in any value the
 * helpers return, in any thrown `Error.message`, or in any serialised
 * `BackendConfig` payload. Mirrors the inventory / terminal-sessions
 * sentinel pattern in the rest of `apps/web/tests/`.
 */
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_BACKEND_URL_PRIVATE_KEY_BYTES_9101";
const SENTINEL_SESSION_TOKEN = "RELAY_SENTINEL_BACKEND_URL_SESSION_TOKEN_9102";
const SENTINEL_PASSWORD = "RELAY_SENTINEL_BACKEND_URL_HUNTER2_9103";
const SENTINEL_BOOTSTRAP_TOKEN =
  "RELAY_SENTINEL_BACKEND_URL_BOOTSTRAP_TOKEN_9104";
const SENTINEL_ENCRYPTED_PK = "RELAY_SENTINEL_BACKEND_URL_ENCRYPTED_PK_9105";

function memoryStorage(
  initial: Record<string, string> = {},
): BackendConfigStorage & { snapshot: () => Record<string, string> } {
  const data = new Map<string, string>(Object.entries(initial));
  return {
    getItem: (k) => (data.has(k) ? (data.get(k) as string) : null),
    setItem: (k, v) => {
      data.set(k, v);
    },
    removeItem: (k) => {
      data.delete(k);
    },
    snapshot: () => Object.fromEntries(data),
  };
}

const VALID_CFG: BackendConfig = {
  version: 1,
  backendOrigin: "https://relay.example.com",
  savedAt: "2026-05-08T12:00:00.000Z",
};

describe("BACKEND_CONFIG_STORAGE_KEY", () => {
  it("is the versioned key documented in the design (§ 8)", () => {
    expect(BACKEND_CONFIG_STORAGE_KEY).toBe("relayterm.backend-config.v1");
  });
});

describe("validateBackendOrigin — accept", () => {
  it("accepts a plain https origin", () => {
    expect(validateBackendOrigin("https://relayterm.example.com")).toEqual({
      ok: true,
      origin: "https://relayterm.example.com",
    });
  });

  it("normalizes a trailing slash off the origin", () => {
    expect(validateBackendOrigin("https://relayterm.example.com/")).toEqual({
      ok: true,
      origin: "https://relayterm.example.com",
    });
  });

  it("lowercases the host per RFC 3986", () => {
    expect(validateBackendOrigin("https://RELAY.EXAMPLE.COM")).toEqual({
      ok: true,
      origin: "https://relay.example.com",
    });
  });

  it("preserves a non-default port", () => {
    expect(validateBackendOrigin("https://relay.example.com:8443")).toEqual({
      ok: true,
      origin: "https://relay.example.com:8443",
    });
  });

  it("strips the default https port (443) on canonicalisation", () => {
    const result = validateBackendOrigin("https://relay.example.com:443");
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.origin).toBe("https://relay.example.com");
  });

  it("trims surrounding whitespace before validating", () => {
    expect(validateBackendOrigin("  https://relay.example.com  ")).toEqual({
      ok: true,
      origin: "https://relay.example.com",
    });
  });

  it("accepts http://localhost (loopback dev allowance, design § 10)", () => {
    expect(validateBackendOrigin("http://localhost")).toEqual({
      ok: true,
      origin: "http://localhost",
    });
  });

  it("accepts http://localhost:8080 with a non-default port", () => {
    expect(validateBackendOrigin("http://localhost:8080")).toEqual({
      ok: true,
      origin: "http://localhost:8080",
    });
  });

  it("accepts http://127.0.0.1 (IPv4 loopback)", () => {
    expect(validateBackendOrigin("http://127.0.0.1")).toEqual({
      ok: true,
      origin: "http://127.0.0.1",
    });
  });

  it("accepts http://[::1] (IPv6 loopback)", () => {
    expect(validateBackendOrigin("http://[::1]")).toEqual({
      ok: true,
      origin: "http://[::1]",
    });
  });

  it("accepts http://10.0.2.2 because design § 10 allows the Android emulator loopback to the host machine", () => {
    expect(validateBackendOrigin("http://10.0.2.2")).toEqual({
      ok: true,
      origin: "http://10.0.2.2",
    });
  });

  it("accepts http://10.0.2.2:8080 with the emulator-loopback port", () => {
    expect(validateBackendOrigin("http://10.0.2.2:8080")).toEqual({
      ok: true,
      origin: "http://10.0.2.2:8080",
    });
  });

  it("accepts http://0.0.0.0 (design § 10 'for completeness')", () => {
    expect(validateBackendOrigin("http://0.0.0.0")).toEqual({
      ok: true,
      origin: "http://0.0.0.0",
    });
  });
});

describe("validateBackendOrigin — reject", () => {
  it("rejects an empty string", () => {
    expect(validateBackendOrigin("")).toEqual({
      ok: false,
      reason: "url_empty",
    });
  });

  it("rejects whitespace-only input as empty", () => {
    expect(validateBackendOrigin("   ")).toEqual({
      ok: false,
      reason: "url_empty",
    });
  });

  it("rejects an unparseable URL", () => {
    expect(validateBackendOrigin("not-a-url")).toEqual({
      ok: false,
      reason: "url_parse_failed",
    });
  });

  it("rejects http://example.com (cleartext to non-loopback host)", () => {
    expect(validateBackendOrigin("http://example.com")).toEqual({
      ok: false,
      reason: "url_http_non_localhost",
    });
  });

  it("rejects http://192.168.1.10 (LAN host over cleartext)", () => {
    expect(validateBackendOrigin("http://192.168.1.10")).toEqual({
      ok: false,
      reason: "url_http_non_localhost",
    });
  });

  it("rejects URLs with a bare embedded username", () => {
    expect(validateBackendOrigin("https://alice@example.com")).toEqual({
      ok: false,
      reason: "url_credentials_forbidden",
    });
  });

  it("rejects URLs with embedded username:password credentials and does not echo the password", () => {
    const result = validateBackendOrigin(
      `https://alice:${SENTINEL_PASSWORD}@example.com`,
    );
    expect(result).toEqual({ ok: false, reason: "url_credentials_forbidden" });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_PASSWORD);
    expect(JSON.stringify(result)).not.toContain("alice");
  });

  it("rejects URLs with a bare password (no username)", () => {
    expect(validateBackendOrigin("https://:hunter2@example.com")).toEqual({
      ok: false,
      reason: "url_credentials_forbidden",
    });
  });

  it.each([
    ["javascript:alert(1)"],
    ["data:text/plain,foo"],
    ["file:///etc/passwd"],
    ["blob:https://example.com/abc"],
    ["about:blank"],
    ["tauri://localhost"],
    ["ftp://example.com"],
    ["ws://example.com"],
    ["wss://example.com"],
  ])("rejects non-http(s) scheme: %s", (input) => {
    const result = validateBackendOrigin(input);
    expect(result).toEqual({ ok: false, reason: "url_scheme_forbidden" });
  });

  it("rejects a non-/ path", () => {
    expect(validateBackendOrigin("https://relay.example.com/api")).toEqual({
      ok: false,
      reason: "url_path_forbidden",
    });
  });

  it("rejects nested path segments", () => {
    expect(validateBackendOrigin("https://relay.example.com/foo/bar")).toEqual({
      ok: false,
      reason: "url_path_forbidden",
    });
  });

  it("rejects a query string", () => {
    expect(
      validateBackendOrigin("https://relay.example.com/?token=abc"),
    ).toEqual({
      ok: false,
      reason: "url_search_forbidden",
    });
  });

  it("rejects a hash fragment", () => {
    expect(validateBackendOrigin("https://relay.example.com/#frag")).toEqual({
      ok: false,
      reason: "url_hash_forbidden",
    });
  });

  it(`rejects URLs longer than ${BACKEND_URL_MAX_LEN} chars`, () => {
    const long = "https://" + "a".repeat(BACKEND_URL_MAX_LEN) + ".example.com";
    expect(validateBackendOrigin(long)).toEqual({
      ok: false,
      reason: "url_too_long",
    });
  });

  it("BACKEND_URL_MAX_LEN matches the design § 10 cap of 2048", () => {
    expect(BACKEND_URL_MAX_LEN).toBe(2048);
  });
});

describe("validateBackendOrigin — sentinel smuggling", () => {
  it("does not echo a 'private_key'-shaped substring smuggled in the path", () => {
    const malicious = `https://relay.example.com/${SENTINEL_PRIVATE_KEY}`;
    const result = validateBackendOrigin(malicious);
    expect(result).toEqual({ ok: false, reason: "url_path_forbidden" });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_PRIVATE_KEY);
  });

  it("does not echo a 'session_token'-shaped substring smuggled in the query", () => {
    const malicious = `https://relay.example.com/?session_token=${SENTINEL_SESSION_TOKEN}`;
    const result = validateBackendOrigin(malicious);
    expect(result).toEqual({ ok: false, reason: "url_search_forbidden" });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_SESSION_TOKEN);
  });

  it("does not echo a 'bootstrap_token'-shaped substring smuggled in the hash (design § 14)", () => {
    const malicious = `https://relay.example.com/#bootstrap_token=${SENTINEL_BOOTSTRAP_TOKEN}`;
    const result = validateBackendOrigin(malicious);
    expect(result).toEqual({ ok: false, reason: "url_hash_forbidden" });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_BOOTSTRAP_TOKEN);
  });

  it("does not echo an 'encrypted_private_key'-shaped substring smuggled in the path (design § 14)", () => {
    const malicious = `https://relay.example.com/encrypted_private_key/${SENTINEL_ENCRYPTED_PK}`;
    const result = validateBackendOrigin(malicious);
    expect(result).toEqual({ ok: false, reason: "url_path_forbidden" });
    expect(JSON.stringify(result)).not.toContain(SENTINEL_ENCRYPTED_PK);
  });
});

describe("deriveApiBaseUrl", () => {
  it("returns ${origin}/api for an https origin", () => {
    expect(deriveApiBaseUrl("https://relay.example.com")).toBe(
      "https://relay.example.com/api",
    );
  });

  it("returns ${origin}/api for a loopback http origin", () => {
    expect(deriveApiBaseUrl("http://localhost:8080")).toBe(
      "http://localhost:8080/api",
    );
  });
});

describe("deriveHealthUrl", () => {
  it("returns ${origin}/healthz for an https origin", () => {
    expect(deriveHealthUrl("https://relay.example.com")).toBe(
      "https://relay.example.com/healthz",
    );
  });

  it("returns ${origin}/healthz for a loopback http origin", () => {
    expect(deriveHealthUrl("http://127.0.0.1:8080")).toBe(
      "http://127.0.0.1:8080/healthz",
    );
  });
});

describe("deriveWebSocketBaseUrl", () => {
  it("upgrades https to wss", () => {
    expect(deriveWebSocketBaseUrl("https://relay.example.com")).toBe(
      "wss://relay.example.com",
    );
  });

  it("downgrades http (loopback) to ws", () => {
    expect(deriveWebSocketBaseUrl("http://localhost:8080")).toBe(
      "ws://localhost:8080",
    );
  });

  it("upgrades https with a non-default port to wss with the same port", () => {
    expect(deriveWebSocketBaseUrl("https://relay.example.com:8443")).toBe(
      "wss://relay.example.com:8443",
    );
  });
});

describe("serializeBackendConfig + parseStoredBackendConfig", () => {
  it("round-trips a valid config", () => {
    const raw = serializeBackendConfig(VALID_CFG);
    expect(parseStoredBackendConfig(raw)).toEqual(VALID_CFG);
  });

  it("serialises ONLY the documented fields — unknown sneaky fields are dropped (design § 14)", () => {
    const sneaky = {
      ...VALID_CFG,
      session_token: SENTINEL_SESSION_TOKEN,
      private_key: SENTINEL_PRIVATE_KEY,
      encrypted_private_key: SENTINEL_ENCRYPTED_PK,
      bootstrap_token: SENTINEL_BOOTSTRAP_TOKEN,
      password: SENTINEL_PASSWORD,
    } as unknown as BackendConfig;
    const raw = serializeBackendConfig(sneaky);
    expect(raw).not.toContain(SENTINEL_SESSION_TOKEN);
    expect(raw).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(raw).not.toContain(SENTINEL_ENCRYPTED_PK);
    expect(raw).not.toContain(SENTINEL_BOOTSTRAP_TOKEN);
    expect(raw).not.toContain(SENTINEL_PASSWORD);
    expect(raw).not.toContain("session_token");
    expect(raw).not.toContain("private_key");
    expect(raw).not.toContain("encrypted_private_key");
    expect(raw).not.toContain("bootstrap_token");
    expect(raw).not.toContain("password");
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    expect(Object.keys(parsed).sort()).toEqual([
      "backendOrigin",
      "savedAt",
      "version",
    ]);
  });

  it("returns null for malformed JSON", () => {
    expect(parseStoredBackendConfig("not json")).toBeNull();
  });

  it("returns null when the JSON root is not an object", () => {
    expect(parseStoredBackendConfig("[]")).toBeNull();
    expect(parseStoredBackendConfig("null")).toBeNull();
    expect(parseStoredBackendConfig('"a string"')).toBeNull();
    expect(parseStoredBackendConfig("42")).toBeNull();
  });

  it("returns null when version is not 1", () => {
    expect(
      parseStoredBackendConfig(JSON.stringify({ ...VALID_CFG, version: 999 })),
    ).toBeNull();
    expect(
      parseStoredBackendConfig(JSON.stringify({ ...VALID_CFG, version: 0 })),
    ).toBeNull();
    expect(
      parseStoredBackendConfig(JSON.stringify({ ...VALID_CFG, version: "1" })),
    ).toBeNull();
  });

  it("returns null when version is missing", () => {
    expect(
      parseStoredBackendConfig(
        JSON.stringify({
          backendOrigin: VALID_CFG.backendOrigin,
          savedAt: VALID_CFG.savedAt,
        }),
      ),
    ).toBeNull();
  });

  it("returns null when backendOrigin fails validation", () => {
    expect(
      parseStoredBackendConfig(
        JSON.stringify({ ...VALID_CFG, backendOrigin: "http://example.com" }),
      ),
    ).toBeNull();
  });

  it("returns null when backendOrigin contains embedded credentials", () => {
    const raw = JSON.stringify({
      ...VALID_CFG,
      backendOrigin: `https://alice:${SENTINEL_PASSWORD}@example.com`,
    });
    expect(parseStoredBackendConfig(raw)).toBeNull();
  });

  it("returns null when backendOrigin is non-canonical (drift on read; design § 8 — drop, do not auto-migrate)", () => {
    const raw = JSON.stringify({
      ...VALID_CFG,
      backendOrigin: "https://RELAY.example.com/",
    });
    expect(parseStoredBackendConfig(raw)).toBeNull();
  });

  it("returns null when savedAt is missing or not a string", () => {
    expect(
      parseStoredBackendConfig(
        JSON.stringify({ version: 1, backendOrigin: VALID_CFG.backendOrigin }),
      ),
    ).toBeNull();
    expect(
      parseStoredBackendConfig(
        JSON.stringify({ ...VALID_CFG, savedAt: 17 }),
      ),
    ).toBeNull();
    expect(
      parseStoredBackendConfig(
        JSON.stringify({ ...VALID_CFG, savedAt: "" }),
      ),
    ).toBeNull();
  });
});

describe("loadBackendConfig / saveBackendConfig / clearBackendConfig", () => {
  it("round-trips a config through an injectable storage", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    expect(loadBackendConfig(storage)).toEqual(VALID_CFG);
  });

  it("returns null when nothing is stored", () => {
    expect(loadBackendConfig(memoryStorage())).toBeNull();
  });

  it("returns null when stored payload is malformed JSON", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: "not json",
    });
    expect(loadBackendConfig(storage)).toBeNull();
  });

  it("returns null when stored config has the wrong version", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: JSON.stringify({
        version: 0,
        backendOrigin: "https://relay.example.com",
        savedAt: "2026-05-08T12:00:00.000Z",
      }),
    });
    expect(loadBackendConfig(storage)).toBeNull();
  });

  it("clears the stored config", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    clearBackendConfig(storage);
    expect(loadBackendConfig(storage)).toBeNull();
  });

  it("uses the documented storage key and writes nothing else", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    expect(Object.keys(storage.snapshot())).toEqual([
      BACKEND_CONFIG_STORAGE_KEY,
    ]);
  });

  it("save/load do not leak unknown sneaky fields through storage", () => {
    const storage = memoryStorage();
    const sneaky = {
      ...VALID_CFG,
      session_token: SENTINEL_SESSION_TOKEN,
    } as unknown as BackendConfig;
    saveBackendConfig(storage, sneaky);
    const stored = storage.snapshot()[BACKEND_CONFIG_STORAGE_KEY];
    expect(stored).toBeDefined();
    expect(stored).not.toContain(SENTINEL_SESSION_TOKEN);
    expect(stored).not.toContain("session_token");
    expect(loadBackendConfig(storage)).toEqual(VALID_CFG);
  });
});
