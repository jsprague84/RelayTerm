import { describe, it, expect } from "vitest";
import {
  isKnownAppPath,
  normalizeAppPath,
  pathForView,
  viewForPath,
  type AppRoutePath,
} from "../src/lib/app/routing.js";
import {
  DEFAULT_VIEW,
  NAV_ITEMS,
  type AppViewId,
} from "../src/lib/app/navigation.js";

describe("routing.viewForPath", () => {
  it("resolves the dashboard for the root path", () => {
    expect(viewForPath("/")).toBe<AppViewId>("dashboard");
  });

  it("resolves the canonical path for every nav item", () => {
    const cases: Array<[AppRoutePath, AppViewId]> = [
      ["/dashboard", "dashboard"],
      ["/terminal", "terminal"],
      ["/sessions", "sessions"],
      ["/servers", "servers"],
      ["/identities", "identities"],
      ["/settings", "settings"],
    ];
    for (const [path, expected] of cases) {
      expect(viewForPath(path)).toBe(expected);
    }
  });

  it("falls back to the default view for unknown paths", () => {
    for (const path of [
      "/nope",
      "/servers/123",
      "/dashboard/extra",
      "/api/v1/anything",
    ]) {
      expect(viewForPath(path)).toBe(DEFAULT_VIEW);
    }
  });

  it("treats the empty string and bare-double-slash as the root alias", () => {
    // These canonicalize to `/` (the dashboard alias), NOT the
    // unknown-path fallback. The end view is the dashboard either
    // way, but the route is matched, not defaulted.
    expect(normalizeAppPath("")).toBe("/");
    expect(normalizeAppPath("//")).toBe("/");
    expect(viewForPath("")).toBe<AppViewId>("dashboard");
    expect(viewForPath("//")).toBe<AppViewId>("dashboard");
  });

  it("does not throw on malformed input", () => {
    const malformed: unknown[] = [
      undefined,
      null,
      42,
      {},
      "/%E0%A4%A",
      "?query=only",
      "#hashonly",
    ];
    for (const value of malformed) {
      expect(() => viewForPath(value as string)).not.toThrow();
      expect(typeof viewForPath(value as string)).toBe("string");
    }
  });
});

describe("routing.pathForView", () => {
  it("maps every nav id to a stable canonical path", () => {
    const expected: Record<AppViewId, AppRoutePath> = {
      dashboard: "/dashboard",
      terminal: "/terminal",
      sessions: "/sessions",
      servers: "/servers",
      identities: "/identities",
      settings: "/settings",
    };
    for (const item of NAV_ITEMS) {
      expect(pathForView(item.id)).toBe(expected[item.id]);
    }
  });

  it("round-trips with viewForPath for every nav item", () => {
    for (const item of NAV_ITEMS) {
      expect(viewForPath(pathForView(item.id))).toBe(item.id);
    }
  });
});

describe("routing.normalizeAppPath", () => {
  it("collapses trailing slashes", () => {
    expect(normalizeAppPath("/servers/")).toBe("/servers");
    expect(normalizeAppPath("/identities//")).toBe("/identities");
    expect(normalizeAppPath("/")).toBe("/");
  });

  it("is case-insensitive", () => {
    expect(normalizeAppPath("/Dashboard")).toBe("/dashboard");
    expect(normalizeAppPath("/SETTINGS")).toBe("/settings");
  });

  it("strips a trailing query string or hash before matching", () => {
    expect(normalizeAppPath("/sessions?foo=bar")).toBe("/sessions");
    expect(normalizeAppPath("/terminal#anchor")).toBe("/terminal");
  });

  it("returns null for unknown paths", () => {
    expect(normalizeAppPath("/nope")).toBeNull();
    expect(normalizeAppPath("/dashboard/extra")).toBeNull();
    expect(normalizeAppPath("/servers/abc-123")).toBeNull();
  });

  it("returns root for empty input", () => {
    expect(normalizeAppPath("")).toBe("/");
  });
});

describe("routing.isKnownAppPath", () => {
  it("matches normalizeAppPath", () => {
    for (const item of NAV_ITEMS) {
      expect(isKnownAppPath(pathForView(item.id))).toBe(true);
    }
    expect(isKnownAppPath("/")).toBe(true);
    expect(isKnownAppPath("/nope")).toBe(false);
    expect(isKnownAppPath("/servers/abc")).toBe(false);
  });
});

describe("routing redaction posture", () => {
  it("does not parse session ids out of the URL", () => {
    // A path that *contains* a session-id-shaped segment must collapse
    // to the dashboard fallback — the helper has no concept of route
    // parameters and must never expose one. SPEC.md "URL routing"
    // forbids session ids in the URL.
    const sessionIdShaped =
      "/terminal/01HZK8VAY5W2W3X4Y5Z6A7B8C9?token=secret";
    expect(viewForPath(sessionIdShaped)).toBe(DEFAULT_VIEW);
    expect(normalizeAppPath(sessionIdShaped)).toBeNull();
  });

  it("ignores query strings without leaking their content", () => {
    const path = "/servers?api_key=verysecret";
    expect(normalizeAppPath(path)).toBe("/servers");
    // The helper must not retain or echo the query in any return.
    const out = JSON.stringify({
      view: viewForPath(path),
      path: normalizeAppPath(path),
    });
    expect(out.includes("verysecret")).toBe(false);
    expect(out.includes("api_key")).toBe(false);
  });
});

describe("routing alignment with navigation", () => {
  it("provides a canonical path for every NAV_ITEMS entry", () => {
    for (const item of NAV_ITEMS) {
      const path = pathForView(item.id);
      expect(path.startsWith("/")).toBe(true);
      expect(viewForPath(path)).toBe(item.id);
    }
  });

  it("uses a unique canonical path per view", () => {
    const paths = NAV_ITEMS.map((n) => pathForView(n.id));
    expect(new Set(paths).size).toBe(paths.length);
  });
});
