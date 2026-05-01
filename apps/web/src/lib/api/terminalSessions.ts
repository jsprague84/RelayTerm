/**
 * Frontend helper for `POST /api/v1/terminal-sessions`.
 *
 * Scope: shared by the production terminal launch UI
 * (`apps/web/src/lib/app/terminal/`, driven from `ServersView.svelte`)
 * AND the dev launcher in `lib/dev/DevTerminalWorkbench.svelte`. Both
 * callers issue the same typed create request through this module —
 * production reaches the result via the AppShell-level active launch
 * state; the dev workbench hands it to `XtermLiveTerminalLab` for
 * auto-attach. SPEC.md "Production terminal launch UI" pins the
 * production contract.
 *
 * Critical contracts re-asserted here:
 *  - Inputs are validated client-side BEFORE the wire round-trip
 *    ({@link validateCreateRequest}). The backend's `invalid_input` is
 *    defense-in-depth; we refuse a typo locally so the UI can show a
 *    clear message without burning a request.
 *  - The backend is authoritative on every field of the response. The
 *    helper does not synthesize, default, or rename fields. Unknown
 *    fields are ignored; missing required fields collapse to a typed
 *    parse error so the launcher can surface "session created but
 *    response was malformed" without panicking.
 *  - Error envelopes are mapped to a small, safe public summary
 *    ({@link CreateTerminalSessionError}). Operator-facing detail (raw
 *    SQL fragments, peer banners, vault internals) is NEVER produced by
 *    the backend in 4xx/5xx bodies, but we still strip everything
 *    except the short `code` and the static `message`. The two
 *    formatters that reach the UI ({@link describeCreateError} here
 *    and `describeLaunchError` in the production workspace) are
 *    functions of `kind`/`status`/`code` only — never the wire body.
 *
 * What this helper does NOT do:
 *  - It does NOT log raw response bodies. A future rev that adds
 *    tracing must keep the same rule — body fields can later carry
 *    data we don't want in the console.
 *  - It does NOT authenticate. Authentication is the AppShell's
 *    concern (and, today, the dev-auth shim's). Same-origin requests
 *    pick up cookies; no header threading happens here.
 */

import { CELL_GRID_MAX, CELL_GRID_MIN } from "../terminal/cellGrid.js";

/** Cell-grid bounds — the wire-side `relayterm_protocol::ResizeMsg` clamp. */
export { CELL_GRID_MAX, CELL_GRID_MIN } from "../terminal/cellGrid.js";

const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;

/** Wire-stable status of a `terminal_session` row, mirroring
 * `relayterm_core::terminal_session::TerminalSessionStatus`. */
export type TerminalSessionStatus =
  | "starting"
  | "active"
  | "detached"
  | "closed";

export interface CreateTerminalSessionRequest {
  server_profile_id: string;
  cols?: number;
  rows?: number;
}

/**
 * Backend's `CreateTerminalSessionResponse` flattened wire shape. The
 * fields here are the ones the frontend actually consumes; unknown fields
 * are ignored on parse so a future backend addition doesn't break older
 * clients.
 */
export interface CreateTerminalSessionResponse {
  id: string;
  server_profile_id: string;
  status: TerminalSessionStatus;
  cols: number;
  rows: number;
  created_at: string;
  last_seen_at: string;
  closed_at: string | null;
  message: string;
  pty_live: boolean;
}

export type CreateRequestValidation =
  | { ok: true; body: { server_profile_id: string; cols: number; rows: number } }
  | { ok: false; reason: CreateRequestInvalidReason };

export type CreateRequestInvalidReason =
  | "missing_server_profile_id"
  | "non-integer-cols"
  | "non-integer-rows"
  | "below-min-cols"
  | "below-min-rows"
  | "above-max-cols"
  | "above-max-rows";

/**
 * Validate a create-session request on the client. The backend rejects
 * invalid dims with `400 invalid_input`; we refuse them locally so the
 * dev UI can show a precise reason without a wire round-trip.
 *
 * `server_profile_id` is treated as opaque — the backend deserializes it
 * into a UUID and 404s on a miss. We only enforce non-empty here so the
 * UI can disable the create button when the input is empty.
 */
export function validateCreateRequest(
  raw: CreateTerminalSessionRequest,
): CreateRequestValidation {
  const id = raw.server_profile_id?.trim() ?? "";
  if (id.length === 0) {
    return { ok: false, reason: "missing_server_profile_id" };
  }
  const cols = raw.cols ?? DEFAULT_COLS;
  const rows = raw.rows ?? DEFAULT_ROWS;
  if (!Number.isInteger(cols)) {
    return { ok: false, reason: "non-integer-cols" };
  }
  if (!Number.isInteger(rows)) {
    return { ok: false, reason: "non-integer-rows" };
  }
  if (cols < CELL_GRID_MIN) return { ok: false, reason: "below-min-cols" };
  if (rows < CELL_GRID_MIN) return { ok: false, reason: "below-min-rows" };
  if (cols > CELL_GRID_MAX) return { ok: false, reason: "above-max-cols" };
  if (rows > CELL_GRID_MAX) return { ok: false, reason: "above-max-rows" };
  return { ok: true, body: { server_profile_id: id, cols, rows } };
}

export type CreateTerminalSessionError =
  | { kind: "validation"; reason: CreateRequestInvalidReason }
  | { kind: "http"; status: number; code: string; message: string }
  | { kind: "transport"; message: string }
  | { kind: "malformed_response" };

/**
 * Map a public-facing error to a short, safe one-line summary suitable
 * for the dev launcher's status text. Operator detail (e.g. router-level
 * 500 messages) never reaches this function — `ApiError::Internal` already
 * collapses to the static `internal_error` envelope server-side. The
 * `http` and `transport` kinds carry a `message` field for programmatic
 * callers, but this formatter deliberately does NOT echo it: a future
 * `fetchImpl` wrapper that surfaces request URLs / headers / retry detail
 * inside the thrown `Error.message` would otherwise reach the launcher's
 * status line. Status text stays code+status only.
 */
export function describeCreateError(err: CreateTerminalSessionError): string {
  switch (err.kind) {
    case "validation":
      return `invalid request: ${err.reason}`;
    case "http":
      return `create failed: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "create failed: transport error";
    case "malformed_response":
      return "create failed: malformed response";
  }
}

const MIN_FETCH = (typeof fetch === "function" ? fetch : undefined) as
  | typeof fetch
  | undefined;

export interface CreateTerminalSessionOptions {
  /** Replaceable for tests. Defaults to `globalThis.fetch`. */
  fetchImpl?: typeof fetch;
  /** Replaceable for tests. Defaults to `/api/v1/terminal-sessions`. */
  endpoint?: string;
}

/**
 * POST a create-session request and parse the typed response.
 *
 * Returns `{ ok: true, session }` on a 2xx with a parseable body. Anything
 * else surfaces as a typed `CreateTerminalSessionError`. The function does
 * not throw for HTTP failures — the caller decides UX.
 */
export async function createTerminalSession(
  raw: CreateTerminalSessionRequest,
  options: CreateTerminalSessionOptions = {},
): Promise<
  { ok: true; session: CreateTerminalSessionResponse }
  | { ok: false; error: CreateTerminalSessionError }
> {
  const validation = validateCreateRequest(raw);
  if (!validation.ok) {
    return { ok: false, error: { kind: "validation", reason: validation.reason } };
  }

  const fetchImpl = options.fetchImpl ?? MIN_FETCH;
  if (!fetchImpl) {
    return {
      ok: false,
      error: { kind: "transport", message: "fetch unavailable" },
    };
  }
  const endpoint = options.endpoint ?? "/api/v1/terminal-sessions";

  let response: Response;
  try {
    response = await fetchImpl(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(validation.body),
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
  const parsed = parseCreateResponse(body);
  if (!parsed) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, session: parsed };
}

async function readErrorEnvelope(
  response: Response,
): Promise<{ code: string; message: string }> {
  // The backend's `ApiError::IntoResponse` always emits
  // `{ error: { code, message } }`. We extract those two fields and drop
  // anything else; if the body is malformed we fall back to the status
  // text. Operator detail is logged server-side, never surfaced here.
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
        typeof inner.message === "string" ? inner.message : response.statusText;
      return { code, message };
    }
  } catch {
    // fall through to status-text fallback
  }
  return { code: "unknown_error", message: response.statusText || "error" };
}

function parseCreateResponse(
  body: unknown,
): CreateTerminalSessionResponse | null {
  if (!body || typeof body !== "object") return null;
  const b = body as Record<string, unknown>;
  if (
    typeof b.id !== "string" ||
    typeof b.server_profile_id !== "string" ||
    typeof b.status !== "string" ||
    typeof b.cols !== "number" ||
    typeof b.rows !== "number" ||
    typeof b.created_at !== "string" ||
    typeof b.last_seen_at !== "string" ||
    typeof b.message !== "string" ||
    typeof b.pty_live !== "boolean"
  ) {
    return null;
  }
  if (
    b.status !== "starting" &&
    b.status !== "active" &&
    b.status !== "detached" &&
    b.status !== "closed"
  ) {
    return null;
  }
  const closedAt = b.closed_at;
  if (closedAt !== null && typeof closedAt !== "string") return null;
  return {
    id: b.id,
    server_profile_id: b.server_profile_id,
    status: b.status,
    cols: b.cols,
    rows: b.rows,
    created_at: b.created_at,
    last_seen_at: b.last_seen_at,
    closed_at: closedAt,
    message: b.message,
    pty_live: b.pty_live,
  };
}
