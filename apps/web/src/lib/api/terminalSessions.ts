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
import {
  fetchJsonList,
  postJsonItem,
  type LoadError,
  type LoadOptions,
  type LoadResult,
  type WireError,
} from "./apiErrors.js";

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
      // Phase 1B.1: per-user live-PTY ceiling refusal. The dev lab
      // surfaces the typed code+status without the parameterised
      // production copy (the dev lab stays self-contained — see
      // `docs/session-quotas.md` § 7.6).
      if (err.status === 429 && err.code === "too_many_sessions") {
        return "create failed: per-user live session limit reached";
      }
      // Phase 1B.2a: per-user starting-burst refusal. Same dev-lab
      // posture as above — typed code+status only, no production copy.
      if (err.status === 429 && err.code === "too_many_starting_sessions") {
        return "create failed: per-user starting session limit reached";
      }
      // Phase 1B.2b: deployment-wide live-PTY refusal. Same dev-lab
      // posture — typed code+status only, no production copy
      // (`docs/session-quotas.md` § 7.6).
      if (err.status === 429 && err.code === "too_many_sessions_deployment") {
        return "create failed: deployment live session limit reached";
      }
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

// ---------------------------------------------------------------------------
// List + close helpers (Sessions list view)
// ---------------------------------------------------------------------------
//
// `GET /api/v1/terminal-sessions` and `POST /:id/close` mirror the backend
// `TerminalSessionResponse` / `CloseTerminalSessionResponse` DTOs in
// `crates/relayterm-api/src/dto/terminal_session.rs`. The list response
// does NOT carry the `message` / `pty_live` fields — those are only on the
// create envelope. The close response carries an `already_closed` boolean.
//
// Redaction posture (load-bearing):
//  - Parsers build the DTO field-by-field. A stray `private_key` /
//    `encrypted_private_key` / `peer_banner` smuggled onto the wire body
//    cannot reach the parsed object because no path here copies it. The
//    backend never emits those fields on this surface, but the rule lives
//    in the helper so a future regression cannot smuggle them through.
//  - {@link describeSessionLoadError} / {@link describeCloseSessionError}
//    stay functions of `kind` + `status` + `code` ONLY. The wire `message`
//    field of an HTTP error and the thrown `Error.message` of a transport
//    failure are NEVER echoed in any user-facing string. The typed errors
//    keep them for programmatic callers.

/**
 * Read-only snapshot of a `terminal_session` row as returned by
 * `GET /api/v1/terminal-sessions`.
 *
 * Carries the safe public fields the SessionsView renders. The DTO is a
 * subset of the create-response shape — the list endpoint does not emit
 * `message` or `pty_live`, and we don't synthesize them here. Callers
 * that need those fields should hit the create response instead.
 */
export interface TerminalSession {
  id: string;
  server_profile_id: string;
  status: TerminalSessionStatus;
  cols: number;
  rows: number;
  /** RFC 3339 timestamp. */
  created_at: string;
  /** RFC 3339 timestamp; the last point the orchestrator observed activity. */
  last_seen_at: string;
  /** RFC 3339 timestamp; `null` while the row is still alive. */
  closed_at: string | null;
}

/**
 * Parse one session row from the list / close-response wire bodies.
 *
 * Constructs the DTO field-by-field so unknown extra fields are dropped
 * silently. A stray `private_key` / `encrypted_private_key` cannot smuggle
 * onto the parsed object because no path here copies it. Returns `null`
 * if any required field is missing or has the wrong shape — the caller
 * collapses that to `malformed_response`.
 */
export function parseTerminalSession(raw: unknown): TerminalSession | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.server_profile_id !== "string" ||
    typeof r.status !== "string" ||
    typeof r.cols !== "number" ||
    typeof r.rows !== "number" ||
    typeof r.created_at !== "string" ||
    typeof r.last_seen_at !== "string"
  ) {
    return null;
  }
  if (
    r.status !== "starting" &&
    r.status !== "active" &&
    r.status !== "detached" &&
    r.status !== "closed"
  ) {
    return null;
  }
  const closedAt = r.closed_at;
  if (closedAt !== undefined && closedAt !== null && typeof closedAt !== "string") {
    return null;
  }
  return {
    id: r.id,
    server_profile_id: r.server_profile_id,
    status: r.status,
    cols: r.cols,
    rows: r.rows,
    created_at: r.created_at,
    last_seen_at: r.last_seen_at,
    closed_at: closedAt === undefined ? null : closedAt,
  };
}

export interface ListTerminalSessionsOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/terminal-sessions`. */
  endpoint?: string;
}

/**
 * GET the caller's terminal sessions.
 *
 * Returns a typed {@link LoadResult}; transport, HTTP, and parse failures
 * collapse to a single envelope so the UI can render loading/empty/error
 * states without try/catch noise. The helper does NOT throw and does NOT
 * log raw response bodies.
 */
export async function listTerminalSessions(
  options: ListTerminalSessionsOptions = {},
): Promise<LoadResult<TerminalSession[]>> {
  const endpoint = options.endpoint ?? "/api/v1/terminal-sessions";
  return fetchJsonList<TerminalSession>(endpoint, parseTerminalSession, options);
}

/**
 * Format a {@link LoadError} as a one-line UI summary. Stays a function of
 * `kind` + `status` + `code` ONLY — never echoes the wire `message` of
 * an HTTP error or the thrown `Error.message` of a transport failure.
 *
 * Narrows on the imported `LoadError` union so a future variant added to
 * the shared envelope forces this formatter to be updated in lockstep.
 */
export function describeSessionLoadError(err: LoadError): string {
  switch (err.kind) {
    case "http":
      return `Failed to load terminal sessions: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Failed to load terminal sessions: transport error";
    case "malformed_response":
      return "Failed to load terminal sessions: malformed response";
  }
}

/**
 * Parsed shape of `POST /api/v1/terminal-sessions/:id/close`. The backend
 * flattens the session row alongside `already_closed` so a single object
 * carries both the post-close row state AND whether close was a no-op.
 */
export interface CloseTerminalSessionResult {
  session: TerminalSession;
  already_closed: boolean;
}

function parseCloseResponse(raw: unknown): CloseTerminalSessionResult | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const session = parseTerminalSession(r);
  if (session === null) return null;
  if (typeof r.already_closed !== "boolean") return null;
  return { session, already_closed: r.already_closed };
}

export interface CloseTerminalSessionOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to
   * `/api/v1/terminal-sessions/:id/close`. */
  endpoint?: string;
}

export type CloseTerminalSessionError = WireError;

export type CloseTerminalSessionResponse =
  | { ok: true; result: CloseTerminalSessionResult }
  | { ok: false; error: CloseTerminalSessionError };

/**
 * POST a close request for the given session id.
 *
 * Backend semantics (mirrored, not re-implemented here):
 *  - Idempotent: closing an already-closed session returns `200 OK` with
 *    `already_closed = true`.
 *  - Foreign-owned ids surface as `404 not_found`.
 *
 * The helper does NOT throw, does NOT log raw response bodies, and does
 * NOT echo wire / transport detail through the formatter.
 */
export async function closeTerminalSession(
  sessionId: string,
  options: CloseTerminalSessionOptions = {},
): Promise<CloseTerminalSessionResponse> {
  const endpoint =
    options.endpoint ??
    `/api/v1/terminal-sessions/${encodeURIComponent(sessionId)}/close`;
  const result = await postJsonItem<CloseTerminalSessionResult>(
    endpoint,
    {},
    parseCloseResponse,
    options,
  );
  if (!result.ok) return { ok: false, error: result.error };
  return { ok: true, result: result.data };
}

/**
 * Format a close error as a one-line UI summary. Same redaction posture
 * as {@link describeSessionLoadError}: a function of `kind` + `status` +
 * `code` ONLY.
 */
export function describeCloseSessionError(
  err: CloseTerminalSessionError,
): string {
  switch (err.kind) {
    case "http":
      if (err.status === 404 && err.code === "not_found") {
        return "Could not close session: session not found";
      }
      if (err.status === 401) {
        return "Could not close session: not authenticated";
      }
      return `Could not close session: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Could not close session: transport error";
    case "malformed_response":
      return "Could not close session: malformed response";
  }
}

// ---------------------------------------------------------------------------
// Get-by-id + saved-session validation (Terminal view recovery affordance)
// ---------------------------------------------------------------------------
//
// Mirrors backend `GET /api/v1/terminal-sessions/:id`. The handler returns
// the same `TerminalSessionResponse` shape the list endpoint emits, so
// {@link parseTerminalSession} is reused — no second parser, no second
// redaction pin to maintain.
//
// Used by the empty-state Terminal view to validate the local active-session
// pointer BEFORE offering the "Reconnect last session" affordance. The
// validator returns a typed decision so the view can:
//   - reconnectable → keep + offer
//   - stale         → clear the local pointer
//   - uncertain     → keep + show a cautious message (e.g. transport
//     unavailable; we don't punish a user with a dropped pointer over a
//     network blip)

export interface GetTerminalSessionOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to
   * `/api/v1/terminal-sessions/:id`. */
  endpoint?: string;
}

export type GetTerminalSessionError = WireError;

export type GetTerminalSessionResponse =
  | { ok: true; session: TerminalSession }
  | { ok: false; error: GetTerminalSessionError };

/**
 * GET a single terminal session by id.
 *
 * Returns `{ ok: true, session }` on a 2xx with a parseable body. 404 is
 * the canonical "row is gone or owned by someone else" surface — the
 * caller treats it as a stale pointer (the backend never differentiates
 * "not yours" from "doesn't exist", which is the right redaction).
 *
 * Same posture as the rest of this module: does NOT throw, does NOT log
 * raw bodies, does NOT echo wire / transport detail through the
 * formatter.
 */
export async function getTerminalSession(
  sessionId: string,
  options: GetTerminalSessionOptions = {},
): Promise<GetTerminalSessionResponse> {
  const endpoint =
    options.endpoint ??
    `/api/v1/terminal-sessions/${encodeURIComponent(sessionId)}`;
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
  const parsed = parseTerminalSession(body);
  if (parsed === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, session: parsed };
}

/**
 * Whether a session row's status admits a reconnect attempt.
 *
 * Mirrors {@link sessionStatus.canReconnect} — `active` and `detached` are
 * reconnectable; `starting` and `closed` are not. Lives here too so an
 * API-layer caller can branch on the wire DTO without importing the
 * status-helpers module.
 */
export function isSessionReconnectable(
  session: Pick<TerminalSession, "status">,
): boolean {
  return session.status === "active" || session.status === "detached";
}

/**
 * Outcome of a saved-session validation pass.
 *
 *  - `reconnectable` → the row is alive and reconnect is allowed; offer
 *    the affordance and pass `session` along for any local UI hint.
 *  - `stale` → the row is gone or terminal; clear the local pointer.
 *  - `uncertain` → we couldn't decide (transport blip, surprising HTTP
 *    error, or the row is `starting` and not yet reconnectable). The
 *    caller MUST NOT clear the pointer; show a cautious message instead.
 *
 * `stale` and `uncertain` carry a pre-formatted `summary` string so the
 * UI does not need a second formatter. Strings are functions of `kind`
 * + `code` + `status` ONLY (same redaction posture as the rest of this
 * module). The `reconnectable` variant intentionally omits `summary` —
 * the affordance renders the same copy whether a verify ran or not.
 */
export type SavedSessionValidation =
  | { kind: "reconnectable"; session: TerminalSession }
  | {
      kind: "stale";
      reason: "closed" | "not_found";
      summary: string;
    }
  | {
      kind: "uncertain";
      reason: "starting" | "transport" | "http" | "malformed";
      summary: string;
    };

/**
 * Validate a saved active-session pointer against the backend.
 *
 * Wraps {@link getTerminalSession} and {@link isSessionReconnectable} into
 * a typed decision the empty-state Terminal view consumes directly. The
 * function never throws and never logs.
 *
 * Decision table:
 *  - 200 + `active`/`detached` → `reconnectable`
 *  - 200 + `closed`            → `stale (closed)`
 *  - 200 + `starting`          → `uncertain (starting)`
 *  - 404                       → `stale (not_found)`
 *  - other HTTP                → `uncertain (http)`
 *  - transport failure         → `uncertain (transport)`
 *  - malformed response        → `uncertain (malformed)`
 */
export async function validateSavedSession(
  sessionId: string,
  options: GetTerminalSessionOptions = {},
): Promise<SavedSessionValidation> {
  const result = await getTerminalSession(sessionId, options);
  if (result.ok) {
    if (isSessionReconnectable(result.session)) {
      return { kind: "reconnectable", session: result.session };
    }
    if (result.session.status === "closed") {
      return {
        kind: "stale",
        reason: "closed",
        summary: "Saved session is no longer available.",
      };
    }
    return {
      kind: "uncertain",
      reason: "starting",
      summary: "Saved session is still starting; reconnect is not yet ready.",
    };
  }
  return classifyValidationError(result.error);
}

function classifyValidationError(
  err: GetTerminalSessionError,
): SavedSessionValidation {
  switch (err.kind) {
    case "http":
      if (err.status === 404) {
        return {
          kind: "stale",
          reason: "not_found",
          summary: "Saved session is no longer available.",
        };
      }
      return {
        kind: "uncertain",
        reason: "http",
        summary: `Could not check saved session: HTTP ${err.status} ${err.code}`,
      };
    case "transport":
      return {
        kind: "uncertain",
        reason: "transport",
        summary: "Could not check saved session: backend unavailable.",
      };
    case "malformed_response":
      return {
        kind: "uncertain",
        reason: "malformed",
        summary: "Could not check saved session: malformed response.",
      };
  }
}

/**
 * Format a get-session error for surfaces that don't want the full
 * {@link SavedSessionValidation} structure (e.g. a manual "check saved
 * session" button outside the validate flow). Same redaction posture as
 * {@link describeSessionLoadError}.
 */
export function describeSessionGetError(
  err: GetTerminalSessionError,
): string {
  switch (err.kind) {
    case "http":
      // Match `classifyValidationError`'s 404 handling: any 404 — regardless
      // of the wire `code` — is the canonical "row is gone or owned by
      // someone else" surface. The backend collapses both into the same
      // status (the right redaction); we keep the formatter consistent
      // with the validator so a non-canonical code on a 404 doesn't
      // surface as a confusingly technical "HTTP 404 <code>" string.
      if (err.status === 404) {
        return "Saved session is no longer available.";
      }
      if (err.status === 401) {
        return "Could not check saved session: not authenticated.";
      }
      return `Could not check saved session: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Could not check saved session: backend unavailable.";
    case "malformed_response":
      return "Could not check saved session: malformed response.";
  }
}
