import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  isTauriBootstrapEnabled,
  isTauriRuntime,
} from "../src/lib/runtime/tauriRuntime.js";

/**
 * Unit tests for the runtime detection helpers.
 *
 * Vitest defaults to a Node environment for this app (see
 * vitest.config.ts). `globalThis.window` is `undefined` by default,
 * which exercises the SSR / Node short-circuit. To simulate the Tauri
 * WebView and the browser deployment, we attach a stub `window` for
 * the duration of a single test and tear it down afterwards.
 */

type GlobalWithWindow = typeof globalThis & { window?: Record<string, unknown> };

function withWindow(stub: Record<string, unknown>, fn: () => void) {
  const g = globalThis as GlobalWithWindow;
  const had = "window" in g;
  const previous = g.window;
  g.window = stub;
  try {
    fn();
  } finally {
    if (had) {
      g.window = previous;
    } else {
      delete g.window;
    }
  }
}

describe("isTauriRuntime", () => {
  it("returns false when window is undefined (SSR / Node / vitest default)", () => {
    expect(isTauriRuntime()).toBe(false);
  });

  it("returns false in a browser-like environment with no Tauri globals", () => {
    withWindow({}, () => {
      expect(isTauriRuntime()).toBe(false);
    });
  });

  it("returns true when window.__TAURI_INTERNALS__ is present (Tauri v2)", () => {
    withWindow({ __TAURI_INTERNALS__: {} }, () => {
      expect(isTauriRuntime()).toBe(true);
    });
  });

  it("returns true when window.__TAURI__ is present (legacy fallback)", () => {
    withWindow({ __TAURI__: {} }, () => {
      expect(isTauriRuntime()).toBe(true);
    });
  });

  it("returns true when window.isTauri === true (v2.1+ shorthand)", () => {
    withWindow({ isTauri: true }, () => {
      expect(isTauriRuntime()).toBe(true);
    });
  });

  it("does not treat an `isTauri` truthy non-boolean as Tauri (defence in depth)", () => {
    withWindow({ isTauri: "yes" }, () => {
      // The check is strict equality `=== true`; a stray string on
      // some other framework's window MUST NOT flip the predicate.
      expect(isTauriRuntime()).toBe(false);
    });
  });
});

describe("isTauriBootstrapEnabled", () => {
  // The helper consults `import.meta.env.DEV`, which Vite stubs in
  // vitest. It is `true` under `vitest run` (since vitest runs in dev
  // mode by default), so without intervention `isTauriBootstrapEnabled`
  // returns `false` even when `isTauriRuntime()` is `true`.
  // We use `vi.stubEnv` to flip DEV to false for the "built shell"
  // assertions, mirroring the Tauri build's static replacement.

  beforeEach(() => {
    vi.unstubAllEnvs();
  });

  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("returns false in browser-like environments regardless of DEV", () => {
    vi.stubEnv("DEV", false);
    withWindow({}, () => {
      expect(isTauriBootstrapEnabled()).toBe(false);
    });
  });

  it("returns false in a Tauri WebView under tauri:dev (DEV=true)", () => {
    vi.stubEnv("DEV", true);
    withWindow({ __TAURI_INTERNALS__: {} }, () => {
      // This is the critical "tauri:dev / tauri:android:dev never
      // sees the picker" guarantee from design § 13.
      expect(isTauriBootstrapEnabled()).toBe(false);
    });
  });

  it("returns true in a built Tauri WebView (DEV=false, Tauri global present)", () => {
    vi.stubEnv("DEV", false);
    withWindow({ __TAURI_INTERNALS__: {} }, () => {
      expect(isTauriBootstrapEnabled()).toBe(true);
    });
  });

  it("returns false when DEV is false but no Tauri global is present (browser deployment)", () => {
    vi.stubEnv("DEV", false);
    withWindow({}, () => {
      expect(isTauriBootstrapEnabled()).toBe(false);
    });
  });
});
