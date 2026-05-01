/**
 * Production app shell URL routing helper.
 *
 * Pure functions that map between {@link AppViewId} and a stable
 * pathname surface. No side effects, no `window` access — the shell
 * is the only place that reads/writes `window.location` and history.
 *
 * Scope (load-bearing — this slice):
 * - One stable pathname per production view.
 * - Trailing slashes and casing normalize predictably.
 * - Unknown / malformed paths collapse to the dashboard. The helper
 *   never throws on user-supplied input.
 * - No query / hash / session-id parsing. Terminal-session ids and
 *   any other secrets stay out of the URL — see SPEC.md.
 *
 * Out of scope: route params, nested routes, auth routes, deep-link
 * launch, route-based data preloading. See SPEC.md "URL routing".
 */

import { DEFAULT_VIEW, type AppViewId } from "./navigation.js";

/**
 * Stable production app paths. The dashboard is reachable at both
 * `/` and `/dashboard` — `/` is the canonical landing, `/dashboard`
 * is the canonical pushState target so refreshes round-trip cleanly.
 */
export type AppRoutePath =
  | "/"
  | "/dashboard"
  | "/terminal"
  | "/sessions"
  | "/servers"
  | "/identities"
  | "/settings";

interface RouteEntry {
  readonly view: AppViewId;
  /** Canonical path emitted by `pathForView`. */
  readonly canonical: AppRoutePath;
  /** Every path that resolves to this view (canonical first). */
  readonly aliases: readonly AppRoutePath[];
}

const ROUTES: readonly RouteEntry[] = [
  { view: "dashboard", canonical: "/dashboard", aliases: ["/dashboard", "/"] },
  { view: "terminal", canonical: "/terminal", aliases: ["/terminal"] },
  { view: "sessions", canonical: "/sessions", aliases: ["/sessions"] },
  { view: "servers", canonical: "/servers", aliases: ["/servers"] },
  { view: "identities", canonical: "/identities", aliases: ["/identities"] },
  { view: "settings", canonical: "/settings", aliases: ["/settings"] },
] as const;

const PATH_TO_VIEW = new Map<AppRoutePath, AppViewId>(
  ROUTES.flatMap((r) => r.aliases.map((a) => [a, r.view] as const)),
);

const VIEW_TO_PATH = new Map<AppViewId, AppRoutePath>(
  ROUTES.map((r) => [r.view, r.canonical] as const),
);

/**
 * Trim trailing slashes (other than the lone root `/`) and lower-case
 * the path. Returns the input unchanged when it does not look like a
 * string we can normalize. Never throws.
 */
function canonicalize(pathname: string): string {
  if (typeof pathname !== "string" || pathname.length === 0) return "/";
  // Strip a query string or hash if a caller hands us a full URL piece.
  // Conservative: only chop at the first `?` or `#`.
  const queryIdx = pathname.search(/[?#]/);
  let p = queryIdx === -1 ? pathname : pathname.slice(0, queryIdx);
  if (p.length === 0) return "/";
  // Collapse repeated trailing slashes; preserve a lone `/`.
  while (p.length > 1 && p.endsWith("/")) {
    p = p.slice(0, -1);
  }
  return p.toLowerCase();
}

/**
 * Map a pathname to its canonical app path, if any. Returns `null`
 * for unknown paths so callers can decide between fallback and
 * `replaceState` behavior.
 */
export function normalizeAppPath(pathname: string): AppRoutePath | null {
  const c = canonicalize(pathname);
  // Direct lookup against canonical aliases (which include `/`).
  if ((PATH_TO_VIEW as Map<string, AppViewId>).has(c)) {
    return c as AppRoutePath;
  }
  return null;
}

export function isKnownAppPath(pathname: string): boolean {
  return normalizeAppPath(pathname) !== null;
}

/**
 * Resolve a pathname to its view. Unknown paths collapse to the
 * default view (`dashboard`) — the shell is responsible for any
 * `replaceState` / canonicalization side effect.
 */
export function viewForPath(pathname: string): AppViewId {
  const normalized = normalizeAppPath(pathname);
  if (normalized === null) return DEFAULT_VIEW;
  return PATH_TO_VIEW.get(normalized) ?? DEFAULT_VIEW;
}

/** Canonical path for a known view. Round-trips with `viewForPath`. */
export function pathForView(view: AppViewId): AppRoutePath {
  const path = VIEW_TO_PATH.get(view);
  if (!path) {
    // The view set is closed by the AppViewId union; this branch is
    // a defensive fallback against a stale runtime caller, not an
    // expected state. Never throw — the shell prefers a safe default.
    return "/dashboard";
  }
  return path;
}
