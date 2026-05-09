import { describe, expect, it, vi } from "vitest";
import {
  buildHandoffUrl,
  decideHandoff,
  performHandoff,
  type NavigationTarget,
} from "../src/lib/runtime/backendHandoff.js";
import {
  BACKEND_CONFIG_STORAGE_KEY,
  clearBackendConfig,
  loadBackendConfig,
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

/**
 * Default `currentOrigin` for tests that exercise the bundled-shell
 * code path (i.e. WebView is at `tauri://localhost`, NOT yet at the
 * backend). Pinned as a constant so the same-origin short-circuit is
 * provably never accidentally tripped by the existing-coverage tests
 * — equality with `VALID_CFG.backendOrigin` would change the
 * expected decision shape.
 */
const BUNDLED_TAURI_ORIGIN = "tauri://localhost";

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
  it("returns passthrough / not_tauri_runtime when the predicate is false (browser deployment)", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => false,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
    ).toEqual({ kind: "passthrough", reason: "not_tauri_runtime" });
  });

  it("returns show_picker / no_config in a Tauri shell with empty storage", () => {
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage: memoryStorage(),
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
  });

  it("returns show_picker / no_config when stored config is malformed JSON", () => {
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: "not json",
    });
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
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
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
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
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
  });

  it("returns navigate with the configured origin's root URL when valid config is present", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      currentOrigin: BUNDLED_TAURI_ORIGIN,
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
      currentOrigin: BUNDLED_TAURI_ORIGIN,
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
      currentOrigin: BUNDLED_TAURI_ORIGIN,
    });
    expect(decision.kind).toBe("passthrough");
    expect(navigation.calls).toEqual([]);
  });

  it("does not navigate when no config is stored (Tauri shell, first launch)", () => {
    const navigation = recordingNavigation();
    const decision = performHandoff({
      isTauriBootstrapEnabled: () => true,
      storage: memoryStorage(),
      navigation,
      currentOrigin: BUNDLED_TAURI_ORIGIN,
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
      currentOrigin: BUNDLED_TAURI_ORIGIN,
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
      currentOrigin: BUNDLED_TAURI_ORIGIN,
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
      currentOrigin: BUNDLED_TAURI_ORIGIN,
    });
    expect(decision).toEqual({ kind: "show_picker", reason: "no_config" });
    expect(navigation.calls).toEqual([]);
    expect(JSON.stringify(decision)).not.toContain(SENTINEL_PASSWORD);
    expect(JSON.stringify(decision)).not.toContain("alice");
  });
});

/**
 * Reset-flow tests covering the primitives the Change Server affordance
 * (ConfiguredBackendGate.svelte → handleChangeServer) relies on.
 *
 * The component glues together two well-tested helpers — `clearTimeout`
 * for the pending navigation handle and `clearBackendConfig` for the
 * storage slot — so these tests pin the contract at the primitive
 * level rather than instantiating the Svelte component (vitest +
 * jsdom + @testing-library/svelte are deliberately not wired into
 * this app's test stack; design § 14 calls these out as still
 * deferred). The component remains thin enough that these primitive
 * tests cover the behaviour that matters: storage gets cleared, the
 * scheduled navigation never fires, only the documented storage key
 * is touched, the next save still routes through navigate, and
 * sentinel-shaped data never leaks through the reset path.
 */
describe("Change Server reset flow (gate primitives)", () => {
  it("clearBackendConfig + decideHandoff returns to show_picker / no_config", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }).kind,
    ).toBe("navigate");

    clearBackendConfig(storage);

    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
    ).toEqual({ kind: "show_picker", reason: "no_config" });
    expect(loadBackendConfig(storage)).toBeNull();
  });

  it("reset only touches the documented storage key (sibling slots untouched)", () => {
    const UNRELATED_KEY = "relayterm.unrelated.example";
    const UNRELATED_VALUE = "preserved-unrelated-value";
    const storage = memoryStorage({ [UNRELATED_KEY]: UNRELATED_VALUE });
    saveBackendConfig(storage, VALID_CFG);

    clearBackendConfig(storage);

    expect(storage.snapshot()).toEqual({ [UNRELATED_KEY]: UNRELATED_VALUE });
  });

  it("reset followed by saving a new origin transitions back to navigate (re-pick path)", () => {
    const storage = memoryStorage();
    saveBackendConfig(storage, VALID_CFG);
    clearBackendConfig(storage);

    const NEW_ORIGIN_CFG: BackendConfig = {
      version: 1,
      backendOrigin: "https://relay-2.example.com",
      savedAt: "2026-05-08T12:30:00.000Z",
    };
    saveBackendConfig(storage, NEW_ORIGIN_CFG);

    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      }),
    ).toEqual({
      kind: "navigate",
      targetUrl: "https://relay-2.example.com/",
      config: NEW_ORIGIN_CFG,
    });
  });

  it("cancelling a scheduled handoff via clearTimeout prevents navigation.assign", () => {
    // Pins the contract the gate's `handleChangeServer` relies on:
    // a setTimeout-scheduled navigation, when cancelled before its
    // delay elapses, never reaches `navigation.assign`. The gate uses
    // the exact same setTimeout/clearTimeout pair, gated on the
    // injectable `navigationDelayMs` prop, to honour the Change
    // Server affordance.
    vi.useFakeTimers();
    try {
      const storage = memoryStorage();
      saveBackendConfig(storage, VALID_CFG);
      const navigation = recordingNavigation();
      const decision = decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: BUNDLED_TAURI_ORIGIN,
      });
      expect(decision.kind).toBe("navigate");

      const target =
        decision.kind === "navigate" ? decision.targetUrl : "<unreachable>";
      const handle = setTimeout(() => {
        navigation.assign(target);
      }, 100);

      // Operator clicks "Change server" before the timer fires.
      clearTimeout(handle);
      clearBackendConfig(storage);

      // Advance well past the original delay; the navigation MUST NOT fire.
      vi.advanceTimersByTime(1_000);

      expect(navigation.calls).toEqual([]);
      expect(loadBackendConfig(storage)).toBeNull();
      expect(
        decideHandoff({
          isTauriBootstrapEnabled: () => true,
          storage,
          currentOrigin: BUNDLED_TAURI_ORIGIN,
        }),
      ).toEqual({ kind: "show_picker", reason: "no_config" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("reset path does not echo sentinel-shaped strings even when storage held suspicious data", () => {
    // A storage that managed to retain a credential-bearing payload
    // (e.g. by surviving an old slice's looser drift policy) MUST NOT
    // leak any of it through the reset path. The reset is a blind
    // `removeItem` + a fresh `decideHandoff` — neither reads the
    // suspicious value back into a returned envelope.
    const storage = memoryStorage({
      [BACKEND_CONFIG_STORAGE_KEY]: JSON.stringify({
        version: 1,
        backendOrigin: `https://alice:${SENTINEL_PASSWORD}@example.com`,
        savedAt: SENTINEL_SESSION_TOKEN,
        smuggled: SENTINEL_PRIVATE_KEY,
      }),
    });

    clearBackendConfig(storage);
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      currentOrigin: BUNDLED_TAURI_ORIGIN,
    });

    expect(decision).toEqual({ kind: "show_picker", reason: "no_config" });
    const serialised = JSON.stringify(decision);
    expect(serialised).not.toContain(SENTINEL_PRIVATE_KEY);
    expect(serialised).not.toContain(SENTINEL_SESSION_TOKEN);
    expect(serialised).not.toContain(SENTINEL_PASSWORD);
    expect(serialised).not.toContain("alice");
    // And the storage slot is empty after reset.
    expect(storage.snapshot()).toEqual({});
  });
});

/**
 * Same-origin short-circuit (`already_at_backend`).
 *
 * Why this exists: the Tauri v2 WebView injects `__TAURI_INTERNALS__`
 * on remote-origin pages too, so after path A's bundled-shell handoff
 * the gate runs again at the post-handoff origin. Without a
 * same-origin short-circuit, the gate would read its own (now-saved)
 * config and schedule another `window.location.assign(${origin}/)` —
 * a navigate loop manifesting as the "Connecting…" splash flashing
 * indefinitely. Discovered 2026-05-09 against a local hosted Compose
 * stack via the bundled desktop shell; pinned here so a future
 * refactor cannot silently re-introduce the loop.
 *
 * Origin equality is RFC-6454-byte (mirrors `relayterm-api`'s
 * `CsrfGuard` semantics in `crates/relayterm-api/src/auth/csrf.rs`):
 * `localhost` ≠ `127.0.0.1`, different ports differ, schemes differ.
 * The same gotcha was previously documented under
 * "Encountered Lessons" 2026-05-09 against `RELAYTERM_AUTH__ALLOWED_ORIGINS`.
 */
describe("decideHandoff — same-origin short-circuit (already_at_backend)", () => {
  /** A representative remote origin used as a saved backend across
   * these cases. Equal-by-bytes matches must select the passthrough
   * branch; any byte difference must select navigate. */
  const REMOTE_ORIGIN = "https://relay.example.com";
  const REMOTE_CFG: BackendConfig = {
    version: 1,
    backendOrigin: REMOTE_ORIGIN,
    savedAt: "2026-05-08T12:00:00.000Z",
  };

  it("Case 1 — built Tauri at bundled origin with a remote backend config still navigates once", () => {
    // Bundled Tauri shell origin (tauri://localhost) ≠ saved
    // backendOrigin (https://relay.example.com) ⇒ navigate fires
    // exactly once. This is the path A handoff's first leg.
    const storage = memoryStorage();
    saveBackendConfig(storage, REMOTE_CFG);
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      currentOrigin: BUNDLED_TAURI_ORIGIN,
    });
    expect(decision).toEqual({
      kind: "navigate",
      targetUrl: "https://relay.example.com/",
      config: REMOTE_CFG,
    });
  });

  it("Case 2 — built Tauri at the remote origin equal to saved backendOrigin passes through (no navigation)", () => {
    // The bug this case pins: after the bundled shell hands off and
    // the WebView is at REMOTE_ORIGIN, the gate must NOT schedule
    // another window.location.assign(REMOTE_ORIGIN + "/") — that's
    // the navigate loop. Equality between `currentOrigin` and the
    // saved `backendOrigin` selects the passthrough branch so the
    // gate's caller renders `children` (AuthGate / AppShell) instead.
    const storage = memoryStorage();
    saveBackendConfig(storage, REMOTE_CFG);
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      currentOrigin: REMOTE_ORIGIN,
    });
    expect(decision).toEqual({
      kind: "passthrough",
      reason: "already_at_backend",
    });
    // performHandoff must therefore not invoke navigation.assign.
    const navigation = recordingNavigation();
    performHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      navigation,
      currentOrigin: REMOTE_ORIGIN,
    });
    expect(navigation.calls).toEqual([]);
  });

  it("Case 3 — same host different port still navigates (port is part of the origin tuple)", () => {
    // RFC 6454 origins are (scheme, host, port). A WebView at
    // https://relay.example.com:8443 with a saved backend at
    // https://relay.example.com (default port 443 implicit) is at a
    // different origin and MUST navigate.
    const storage = memoryStorage();
    saveBackendConfig(storage, REMOTE_CFG);
    const decision = decideHandoff({
      isTauriBootstrapEnabled: () => true,
      storage,
      currentOrigin: "https://relay.example.com:8443",
    });
    expect(decision).toEqual({
      kind: "navigate",
      targetUrl: "https://relay.example.com/",
      config: REMOTE_CFG,
    });
  });

  it("Case 4 — localhost vs 127.0.0.1 are NOT equal (mirrors CSRF byte-equality lesson)", () => {
    // Pins the same gotcha already documented for
    // RELAYTERM_AUTH__ALLOWED_ORIGINS in AGENTS.md "Encountered
    // Lessons" 2026-05-09: the byte-equality check in
    // crates/relayterm-api/src/auth/csrf.rs does NOT collapse
    // `localhost` and `127.0.0.1`. Apply the same posture client
    // side: a WebView at http://127.0.0.1:8081 with a saved backend
    // of http://localhost:8081 is at a DIFFERENT origin per RFC
    // 6454 and must still navigate.
    const LOOPBACK_NAME = "http://localhost:8081";
    const LOOPBACK_IP = "http://127.0.0.1:8081";
    const cfg: BackendConfig = {
      version: 1,
      backendOrigin: LOOPBACK_NAME,
      savedAt: "2026-05-09T02:25:05.401Z",
    };
    const storage = memoryStorage();
    saveBackendConfig(storage, cfg);

    // currentOrigin = 127.0.0.1, saved = localhost → navigate.
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: LOOPBACK_IP,
      }),
    ).toEqual({
      kind: "navigate",
      targetUrl: `${LOOPBACK_NAME}/`,
      config: cfg,
    });

    // And the symmetrical case — currentOrigin = localhost,
    // saved = 127.0.0.1 → also navigate. Pinning both directions so
    // a future "normalize loopback" refactor cannot silently
    // collapse the two without an explicit decision.
    const storage2 = memoryStorage();
    const cfgIp: BackendConfig = {
      ...cfg,
      backendOrigin: LOOPBACK_IP,
    };
    saveBackendConfig(storage2, cfgIp);
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage: storage2,
        currentOrigin: LOOPBACK_NAME,
      }),
    ).toEqual({
      kind: "navigate",
      targetUrl: `${LOOPBACK_IP}/`,
      config: cfgIp,
    });
  });

  it("Case 5 — saved origin's canonical (no trailing slash) form matches a currentOrigin without trailing slash", () => {
    // `validateBackendOrigin` strips trailing slashes when canonicalising
    // (Phase B docs § 10) and `window.location.origin` never has one.
    // So a byte-equality compare is sufficient — we don't need a
    // separate trim step in the short-circuit. This case pins that
    // contract by writing the canonical form to storage and
    // confirming the same canonical form passed as currentOrigin
    // selects passthrough. The "trailing slash" wording in the task
    // refers to the false-mismatch risk; canonicalisation makes that
    // risk inapplicable, and this test documents the why.
    const ORIGIN_NO_SLASH = "https://relay.example.com";
    const cfg: BackendConfig = {
      version: 1,
      backendOrigin: ORIGIN_NO_SLASH, // already canonical
      savedAt: "2026-05-08T12:00:00.000Z",
    };
    const storage = memoryStorage();
    saveBackendConfig(storage, cfg);
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => true,
        storage,
        currentOrigin: ORIGIN_NO_SLASH,
      }),
    ).toEqual({
      kind: "passthrough",
      reason: "already_at_backend",
    });
  });

  it("Case 6 — browser deployment / Tauri dev short-circuit takes precedence over already_at_backend", () => {
    // The browser deployment must NEVER reach the same-origin
    // comparison: passthrough/not_tauri_runtime wins. This is the
    // load-bearing guarantee from design § 13 — the production
    // browser never sees this gate's runtime branches. Same for
    // tauri:dev / tauri:android:dev where `import.meta.env.DEV`
    // forces `isTauriBootstrapEnabled` to false; the dev URL
    // (http://localhost:5173) never coincides with a saved backend
    // origin in practice, but the gate's correctness must not depend
    // on that.
    const storage = memoryStorage();
    saveBackendConfig(storage, REMOTE_CFG);
    const navigation = recordingNavigation();

    // Browser deployment: predicate = false. Even if we passed
    // `currentOrigin` matching the saved backend, the not_tauri_runtime
    // branch wins, which is also passthrough but for a different reason.
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => false,
        storage,
        currentOrigin: REMOTE_ORIGIN,
      }),
    ).toEqual({
      kind: "passthrough",
      reason: "not_tauri_runtime",
    });
    performHandoff({
      isTauriBootstrapEnabled: () => false,
      storage,
      navigation,
      currentOrigin: REMOTE_ORIGIN,
    });
    expect(navigation.calls).toEqual([]);

    // Tauri dev: predicate also returns false (because
    // !import.meta.env.DEV ⇒ false). Same outcome.
    expect(
      decideHandoff({
        isTauriBootstrapEnabled: () => false,
        storage,
        currentOrigin: "http://localhost:5173",
      }),
    ).toEqual({
      kind: "passthrough",
      reason: "not_tauri_runtime",
    });
  });
});
