import { describe, expect, it, vi } from "vitest";
import {
  buildHandoffUrl,
  decideHandoff,
  performHandoff,
  type NavigationTarget,
} from "../src/lib/runtime/backendHandoff.js";
import {
  BACKEND_CONFIG_STORAGE_KEY,
  saveBackendConfig,
  type BackendConfig,
  type BackendConfigStorage,
} from "../src/lib/runtime/backendConfig.js";

/**
 * Sentinel substrings that MUST NOT appear in any value the handoff
 * helpers return. Mirrors the redaction posture of the Phase B tests.
 */
const SENTINEL_PRIVATE_KEY = "RELAY_SENTINEL_HANDOFF_PRIVATE_KEY_BYTES_9201";
const SENTINEL_SESSION_TOKEN = "RELAY_SENTINEL_HANDOFF_SESSION_TOKEN_9202";
const SENTINEL_PASSWORD = "RELAY_SENTINEL_HANDOFF_HUNTER2_9203";

const VALID_CFG: BackendConfig = {
  version: 1,
  backendOrigin: "https://relay.example.com",
  savedAt: "2026-05-08T12:00:00.000Z",
};

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

function recordingNavigation(): NavigationTarget & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    assign: (url: string) => {
      calls.push(url);
    },
  };
}

describe("buildHandoffUrl", () => {
  it("appends a trailing slash to the origin", () => {
    expect(buildHandoffUrl("https://relay.example.com")).toBe(
      "https://relay.example.com/",
    );
  });

  it("preserves a non-default port", () => {
    expect(buildHandoffUrl("https://relay.example.com:8443")).toBe(
      "https://relay.example.com:8443/",
    );
  });

  it("preserves the loopback http origin shape", () => {
    expect(buildHandoffUrl("http://localhost:8080")).toBe(
      "http://localhost:8080/",
    );
  });
});

describe("decideHandoff", () => {
  it("returns show_picker / not_tauri_runtime when the predicate is false (browser deployment)", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    expect(
      decideHandoff({ isTauriBootstrapEnabled: () => false, storage }),
    ).toEqual({ kind: "show_picker", reason: "not_tauri_runtime" });
  });

  it("returns show_picker / no_config in a Tauri shell with empty storage", () => {
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage: memoryStorage(),
      }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
  });

  it("returns show_picker / no_config when stored config is malformed JSON", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: "not json",
    });
    expect(
      decideHandoff({ isTauriBootstrapEnabled: () => true, storage }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
  });

  it("returns show_picker / no_config when stored origin is non-canonical (drift)", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: JSON.stringify({
        version: 1,
        backendOrigin: "https://RELAY.example.com/",
        savedAt: "2026-05-08T12:00:00.000Z",
      }),
    });
    expect(
      decideHandoff({ isTauriBootstrapEnabled: () => true, storage }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
  });

  it("returns show_picker / no_config when stored config has the wrong version", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: JSON.stringify({
        ...VALID_CFG,
        version: 0,
      }),
    });
    expect(
      decideHandoff({ isTauriBootstrapEnabled: () => true, storage }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
  });

  it("returns navigate with the configured origin's root URL when valid config is present", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
    });
    expect(decision).toEqual({
      kind: "navigate",
      targetUrl: "https://relay.example.com/",
      config: VALID_CFG,
    });
  });

  it("does not echo a sentinel-shaped string smuggled through a malformed stored config", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: JSON.stringify({
        version: 1,
        backendOrigin: `https://relay.example.com/${SENTINEL_PRIVATE_KEY}`,
        savedAt: SENTINEL_SESSION_TOKEN,
      }),
    });
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
    });
    expect(decision).toEqual({ kind: "show_picker", reason: "no_config" });
    const serialised = JSON.stringify(decision);
    expect(serialised).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(serialised).not.toContain(SENTINEL_SESSION_TOKEN);
  });
});

describe("performHandoff", () => {
  it("does not navigate in browser mode (predicate false)", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    const navigation = recordingNavigation();
    const decision = performHandoff({
      isTauriBootstrapEnabled: () => false,
      storage,
      navigation,
    });
    expect(decision.kind).toBe("show_picker");
    expect(navigation.calls).toEqual([]);
  });

  it("does not navigate when no config is stored (Tauri shell, first launch)", () => {
    const navigation = recordingNavigation();
    const decision = performHandoff({
      isTauriBootstrapEnabled: () => true,
      storage: memoryStorage(),
      navigation,
    });
    expect(decision).toEqual({ kind: "show_picker", reason: "no_config" });
    expect(navigation.calls).toEqual([]);
  });

  it("navigates to the configured origin's root when valid config is present", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    const navigation = recordingNavigation();
    const decision = performHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      navigation,
    });
    expect(decision.kind).toBe("navigate");
    expect(navigation.calls).toEqual(["https://relay.example.com/"]);
  });

  it("never navigates if the predicate is reachable but returns false (defence in depth)", () => {
    // Ensure we don't navigate even if the storage holds a valid config.
    // Mirrors the design § 7 "browser deployment never sees the picker
    // and never gets a navigation kick" guarantee.
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    const navigation = recordingNavigation();
    const predicate = vi.fn(() => false);
    performHandoff({
      isTauriBootstrapEnabled: predicate,
      storage,
      navigation,
    });
    expect(predicate).toHaveBeenCalledOnce();
    expect(navigation.calls).toEqual([]);
  });

  it("does not log or echo password-shaped sentinels through navigation", () => {
    // A malicious storage that managed to persist a credential-bearing
    // origin would be filtered by `loadBackendConfig`'s drift policy
    // (the validator rejects userinfo). This test pins that nothing
    // sensitive reaches the navigation surface even via that path.
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: JSON.stringify({
        version: 1,
        backendOrigin: `https://alice:${SENTINEL_PASSWORD}@relay.example.com`,
        savedAt: "2026-05-08T12:00:00.000Z",
      }),
    });
    const navigation = recordingNavigation();
    const decision = performHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      navigation,
    });
    expect(decision).toEqual({ kind: "show_picker", reason: "no_config" });
    expect(navigation.calls).toEqual([]);
    expect(JSON.stringify(decision)).not.toContain(SENTINEL_PASSWORD);
    expect(JSON.stringify(decision)).not.toContain("alice");
  });
});
