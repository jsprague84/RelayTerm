/**
 * Shared types and helpers for the read-only inventory API surface
 * (`hosts.ts`, `serverProfiles.ts`, `sshIdentities.ts`).
 *
 * Scope: list endpoints only. The helpers do NOT throw; transport,
 * HTTP, and parse failures collapse to a typed {@link LoadError}
 * envelope so the caller can render loading/empty/error states without
 * try/catch noise.
 *
 * Redaction posture (load-bearing):
 *  - The HTTP envelope reader extracts ONLY `code` and `message` from
 *    the backend's `{ error: { code, message } }` response — every
 *    other field is dropped, including any future `operator_detail`
 *    siblings. Test sentinels in `tests/inventoryApi.test.ts` pin this.
 *  - {@link describeLoadError} formats a one-line UI summary that is
 *    a function of `kind`, `status`, and `code` ONLY — it never echoes
 *    the wire `message` or transport `Error.message`. A future fetch
 *    wrapper that smuggles request URLs / headers into thrown
 *    messages will not leak through this surface.
 *  - Helpers MUST NOT log raw response bodies. Operator detail belongs
 *    in server logs, not the browser console.
 */

/**
 * A short label used in formatted error summaries (e.g. "hosts").
 *
 * Intentionally a closed string-literal union: every caller of
 * {@link describeLoadError} is in this repo, and a typo in a label
 * should be a compile error rather than a silent UI string. When a new
 * resource lands (e.g. "terminal sessions"), extend the union AND the
 * caller in lockstep — never `as` the cast away.
 */
export type ResourceLabel =
  | "hosts"
  | "server profiles"
  | "SSH identities"
  | "audit events";

export type LoadError =
  | { kind: "http"; status: number; code: string; message: string }
  | { kind: "transport"; message: string }
  | { kind: "malformed_response" };

export type LoadResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: LoadError };

export interface LoadOptions {
  /** Replaceable for tests. Defaults to `globalThis.fetch`. */
  fetchImpl?: typeof fetch;
}

/**
 * Format a {@link LoadError} as a one-line UI string. Stays a function
 * of `kind` + `status` + `code` only — never echoes the wire `message`
 * or transport detail. The label is the human-facing resource name
 * (e.g. "hosts", "SSH identities") used in the rendered summary.
 */
export function describeLoadError(
  label: ResourceLabel,
  err: LoadError,
): string {
  switch (err.kind) {
    case "http":
      return `Failed to load ${label}: HTTP ${err.status} ${err.code}`;
    case "transport":
      return `Failed to load ${label}: transport error`;
    case "malformed_response":
      return `Failed to load ${label}: malformed response`;
  }
}

/**
 * Read the backend's `{ error: { code, message } }` response shape and
 * return only the `code` and `message`. Anything else in the envelope
 * — including `operator_detail` siblings or top-level fields — is
 * silently dropped. On a body that is not JSON or does not match the
 * envelope shape, falls back to status text.
 *
 * The body is read at most once via `response.json()`; the helper does
 * NOT log the parsed body. Callers that want the raw status text on a
 * malformed envelope receive the static `unknown_error` code.
 */
export async function readErrorEnvelope(
  response: Response,
): Promise<{ code: string; message: string }> {
  try {
    const body = (await response.json()) as unknown;
    if (
      body &&
      typeof body === "object" &&
      "error" in body &&
      typeof (body as { error: unknown }).error === "object" &&
      (body as { error: unknown }).error !== null
    ) {
      const inner = (body as { error: Record<string, unknown> }).error;
      const code =
        typeof inner.code === "string" ? inner.code : "unknown_error";
      const message =
        typeof inner.message === "string"
          ? inner.message
          : response.statusText;
      return { code, message };
    }
  } catch {
    // fall through to status-text fallback
  }
  return { code: "unknown_error", message: response.statusText || "error" };
}

/**
 * Subset of {@link LoadError} the shared POST helper can produce.
 * Validation lives at the resource layer; the wire helper only sees
 * transport / HTTP / parse outcomes. Shaped so a resource-level
 * `CreateXError` union can directly include this variant set.
 */
export type WireError =
  | { kind: "http"; status: number; code: string; message: string }
  | { kind: "transport"; message: string }
  | { kind: "malformed_response" };

export type WireResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: WireError };

/**
 * POST a JSON body to a typed-create endpoint and parse the response
 * with the supplied parser. The parser MUST return `null` if it cannot
 * validate the response; that collapses to `malformed_response`.
 *
 * The function does not throw. It does not log. It does not echo the
 * thrown message of a transport failure or the wire `message` of an
 * HTTP error in any user-facing field — the typed error preserves both
 * for programmatic callers, but the resource-level formatter is the
 * single point that reaches the UI.
 */
export async function postJsonItem<T>(
  endpoint: string,
  body: unknown,
  parseItem: (raw: unknown) => T | null,
  options: LoadOptions = {},
): Promise<WireResult<T>> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return {
      ok: false,
      error: { kind: "transport", message: "fetch unavailable" },
    };
  }

  let response: Response;
  try {
    response = await fetchImpl(endpoint, {
      method: "POST",
      headers: {
        accept: "application/json",
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
  } catch (err) {
    return {
      ok: false,
      error: {
        kind: "transport",
        message: err instanceof Error ? err.message : "unknown",
      },
    };
  }

  if (!response.ok) {
    const { code, message } = await readErrorEnvelope(response);
    return {
      ok: false,
      error: { kind: "http", status: response.status, code, message },
    };
  }

  let parsed: unknown;
  try {
    parsed = await response.json();
  } catch {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  const item = parseItem(parsed);
  if (item === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, data: item };
}

/**
 * GET a JSON list endpoint and parse each item with the supplied
 * parser. The parser MUST return `null` for any item it cannot
 * validate; a single `null` collapses the whole response to
 * `malformed_response` so the UI never renders partially-valid rows.
 *
 * The function does not throw. It does not log. It does not echo the
 * thrown message of a transport failure or the wire `message` of an
 * HTTP error in any field that reaches the UI's status formatter.
 */
export async function fetchJsonList<T>(
  endpoint: string,
  parseItem: (raw: unknown) => T | null,
  options: LoadOptions = {},
): Promise<LoadResult<T[]>> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return {
      ok: false,
      error: { kind: "transport", message: "fetch unavailable" },
    };
  }

  let response: Response;
  try {
    response = await fetchImpl(endpoint, {
      headers: { accept: "application/json" },
    });
  } catch (err) {
    return {
      ok: false,
      error: {
        kind: "transport",
        message: err instanceof Error ? err.message : "unknown",
      },
    };
  }

  if (!response.ok) {
    const { code, message } = await readErrorEnvelope(response);
    return {
      ok: false,
      error: { kind: "http", status: response.status, code, message },
    };
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  if (!Array.isArray(body)) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  const out: T[] = [];
  for (const raw of body) {
    const parsed = parseItem(raw);
    if (parsed === null) {
      return { ok: false, error: { kind: "malformed_response" } };
    }
    out.push(parsed);
  }
  return { ok: true, data: out };
}
