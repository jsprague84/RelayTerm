/**
 * Frontend primitives for the Tauri runtime backend-URL slice.
 *
 * Scope: pure helpers + an injectable-storage round-trip surface used
 * by the Tauri shells' bootstrap picker (phase C). The browser
 * deployment never reaches this module — its consumer is gated on
 * `isTauri()`. See `docs/spec/tauri-runtime-backend-url.md` (the
 * normative design doc, especially §§ 8, 10, 14) for the rules
 * implemented here.
 *
 * Redaction posture (load-bearing):
 *  - The validator returns a typed `reason` enum on rejection. The
 *    rejection envelope MUST NOT echo the input URL or any substring
 *    of it. Sentinel-string smuggling tests in
 *    `tests/backendConfig.test.ts` pin this.
 *  - `serializeBackendConfig` projects ONLY the documented fields
 *    (`version`, `backendOrigin`, `savedAt`); any sneaky additional
 *    field on the input is silently dropped before stringify.
 *  - Helpers do NOT log. The picker UI is the single point that
 *    surfaces a rejection reason to the user.
 *  - Storage is injected through `BackendConfigStorage`; the helpers
 *    do not reach for `window.localStorage` directly. Tests use an
 *    in-memory shim. The browser deployment never invokes the storage
 *    helpers; the Tauri shell binds them to `window.localStorage`.
 *
 * Out of scope for this module (phase C and beyond): runtime Tauri
 * detection (`isTauri()`), the picker view, WebView navigation, Tauri
 * capabilities, backend CORS, cookie / CSRF / auth changes, and any
 * native secure-storage choice.
 */

/** Versioned `localStorage` key for the persisted config. */
export const BACKEND_CONFIG_STORAGE_KEY = "relayterm.backend-config.v1";

/** Hard upper bound on accepted URL length (design § 10). */
export const BACKEND_URL_MAX_LEN = 2048;

/**
 * Reasons a candidate backend URL was rejected. Stays a closed
 * string-literal union so the picker UI can map each reason to a
 * static, redacted message.
 */
export type BackendUrlError =
  | "url_empty"
  | "url_too_long"
  | "url_parse_failed"
  | "url_credentials_forbidden"
  | "url_scheme_forbidden"
  | "url_http_non_localhost"
  | "url_path_forbidden"
  | "url_search_forbidden"
  | "url_hash_forbidden";

export type BackendUrlValidation =
  | { ok: true; origin: string }
  | { ok: false; reason: BackendUrlError };

/**
 * Persisted form of a saved backend URL. The version suffix on
 * {@link BACKEND_CONFIG_STORAGE_KEY} plus the explicit `version: 1`
 * field together let a future shape change drop legacy data instead of
 * auto-migrating (design § 8).
 */
export interface BackendConfig {
  version: 1;
  backendOrigin: string;
  savedAt: string;
}

/**
 * Minimal shape of `Storage` the helpers need. Injected so tests do
 * not depend on `window.localStorage` and the production callsite can
 * pick the storage explicitly.
 */
export interface BackendConfigStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

// IPv6 hostnames round-trip through `URL.hostname` with their square
// brackets preserved (`new URL("http://[::1]").hostname === "[::1]"`).
// The bracketed form is what we need to match for the loopback gate.
const HTTP_LOOPBACK_HOSTS = new Set<string>([
  "localhost",
  "127.0.0.1",
  "[::1]",
  "10.0.2.2",
  "0.0.0.0",
]);

/**
 * Validate and canonicalise a candidate backend origin per design § 10.
 *
 * Accepts: `https://<host>[:port]` (any host); `http://<host>[:port]`
 * only when `<host>` is `localhost`, `127.0.0.1`, `::1`, `10.0.2.2`,
 * or `0.0.0.0`. Rejects: empty, oversized, unparseable, embedded
 * userinfo, non-http(s) schemes, any non-`/` path, any query string,
 * any hash fragment.
 *
 * On success, returns the canonical origin (lower-cased host, default
 * port stripped, no trailing slash, no path, no query, no fragment).
 * The returned `origin` is the sole sanitised form callers may rely
 * on; downstream `derive*` helpers assume an already-validated origin.
 */
export function validateBackendOrigin(input: string): BackendUrlValidation {
  const trimmed = input.trim();
  if (trimmed.length === 0) return { ok: false, reason: "url_empty" };
  if (trimmed.length > BACKEND_URL_MAX_LEN)
    return { ok: false, reason: "url_too_long" };

  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return { ok: false, reason: "url_parse_failed" };
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { ok: false, reason: "url_scheme_forbidden" };
  }

  if (url.username !== "" || url.password !== "") {
    return { ok: false, reason: "url_credentials_forbidden" };
  }

  if (
    url.protocol === "http:" &&
    !HTTP_LOOPBACK_HOSTS.has(url.hostname.toLowerCase())
  ) {
    return { ok: false, reason: "url_http_non_localhost" };
  }

  // The WHATWG URL parser canonicalises an empty `http(s):` path to
  // `"/"`, so for any URL that survived the scheme check `pathname` is
  // at minimum `"/"` — anything else is an explicit path the operator
  // typed and we reject.
  if (url.pathname !== "/") {
    return { ok: false, reason: "url_path_forbidden" };
  }

  if (url.search !== "") {
    return { ok: false, reason: "url_search_forbidden" };
  }

  if (url.hash !== "") {
    return { ok: false, reason: "url_hash_forbidden" };
  }

  return { ok: true, origin: url.origin };
}

/**
 * Returns `${origin}/api` — the API mount prefix on a configured
 * backend. Production helpers in `apps/web/src/lib/api/*.ts` build
 * versioned routes by appending `/v1/<resource>` to this base
 * (e.g. `${deriveApiBaseUrl(origin)}/v1/hosts`); the version segment
 * is intentionally NOT baked in here so a future `/api/v2/...` cohort
 * can be derived from the same primitive without churn. Caller MUST
 * pass an already-validated origin (the `origin` field returned by
 * {@link validateBackendOrigin}).
 */
export function deriveApiBaseUrl(origin: string): string {
  return `${origin}/api`;
}

/** Returns `${origin}/healthz`. Caller MUST pass an already-validated origin. */
export function deriveHealthUrl(origin: string): string {
  return `${origin}/healthz`;
}

/**
 * Returns the WebSocket base URL for a given HTTP(S) origin: `https`
 * upgrades to `wss`; `http` (loopback only, by validator policy)
 * downgrades to `ws`. Caller MUST pass an already-validated origin.
 */
export function deriveWebSocketBaseUrl(origin: string): string {
  if (origin.startsWith("https://")) return "wss://" + origin.slice("https://".length);
  if (origin.startsWith("http://")) return "ws://" + origin.slice("http://".length);
  // Caller invariant violation; fall through to a clearly-broken URL
  // rather than silently rewriting to a different scheme.
  return origin;
}

/**
 * Project a `BackendConfig` to the canonical wire form. Only the three
 * documented fields are emitted; any extra property on the input
 * object is dropped before stringify. This is the single point that
 * touches `JSON.stringify` for the persisted shape.
 */
export function serializeBackendConfig(cfg: BackendConfig): string {
  return JSON.stringify({
    version: cfg.version,
    backendOrigin: cfg.backendOrigin,
    savedAt: cfg.savedAt,
  });
}

/**
 * Parse a stored backend config. Returns `null` for any deviation
 * from the canonical shape — malformed JSON, wrong version, missing
 * fields, an origin that fails {@link validateBackendOrigin}, or an
 * origin that re-canonicalises to a different value (drift on read;
 * design § 8 says drop, do not auto-migrate).
 *
 * Never throws; never logs.
 */
export function parseStoredBackendConfig(raw: string): BackendConfig | null {
  let body: unknown;
  try {
    body = JSON.parse(raw);
  } catch {
    return null;
  }
  if (body === null || typeof body !== "object" || Array.isArray(body)) {
    return null;
  }
  const obj = body as Record<string, unknown>;
  if (obj.version !== 1) return null;
  if (typeof obj.backendOrigin !== "string") return null;
  if (typeof obj.savedAt !== "string" || obj.savedAt.length === 0) return null;
  const validation = validateBackendOrigin(obj.backendOrigin);
  if (!validation.ok || validation.origin !== obj.backendOrigin) return null;
  return {
    version: 1,
    backendOrigin: validation.origin,
    savedAt: obj.savedAt,
  };
}

/**
 * Read the persisted config from the supplied storage. Returns `null`
 * when the slot is empty OR when the stored value fails
 * {@link parseStoredBackendConfig}. Drift (shape mismatch, version
 * mismatch, drifted origin) collapses to `null` so the caller falls
 * back to the picker rather than acting on stale data.
 */
export function loadBackendConfig(
  storage: BackendConfigStorage,
): BackendConfig | null {
  const raw = storage.getItem(BACKEND_CONFIG_STORAGE_KEY);
  if (raw === null) return null;
  return parseStoredBackendConfig(raw);
}

/** Write the canonical-shape JSON for `cfg` into the supplied storage. */
export function saveBackendConfig(
  storage: BackendConfigStorage,
  cfg: BackendConfig,
): void {
  storage.setItem(BACKEND_CONFIG_STORAGE_KEY, serializeBackendConfig(cfg));
}

/** Remove the persisted config (used by the "Change server" affordance). */
export function clearBackendConfig(storage: BackendConfigStorage): void {
  storage.removeItem(BACKEND_CONFIG_STORAGE_KEY);
}
