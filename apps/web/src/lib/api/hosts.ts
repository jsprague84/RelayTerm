/**
 * Frontend helpers for `/api/v1/hosts`.
 *
 * Surface today: list (read-only inventory) and create (POST). The DTOs
 * mirror `HostResponse` and `CreateHostRequest` on the backend
 * (`crates/relayterm-api/src/dto/host.rs`); list parsing ignores unknown
 * extra fields so a future safe addition does not break older clients.
 *
 * Create is a metadata-only write — it stores a reachable target
 * definition. It does NOT attempt an SSH connection, does NOT verify
 * the host key, and does NOT confirm the host is reachable. Host-key
 * trust and auth-check remain future work.
 *
 * Edit and delete UI are future work; the helpers below are intentionally
 * scoped to the create + list flows.
 */

import {
  fetchJsonList,
  postJsonItem,
  readErrorEnvelope,
  type LoadOptions,
  type LoadResult,
  type WireError,
} from "./apiErrors.js";

export interface Host {
  id: string;
  display_name: string;
  hostname: string;
  /** SSH port. Backend serializes this as `u16`. */
  port: number;
  default_username: string;
  /** RFC 3339 timestamp. */
  created_at: string;
  /** RFC 3339 timestamp. */
  updated_at: string;
}

export interface ListHostsOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/hosts`. */
  endpoint?: string;
}

export async function listHosts(
  options: ListHostsOptions = {},
): Promise<LoadResult<Host[]>> {
  const endpoint = options.endpoint ?? "/api/v1/hosts";
  return fetchJsonList<Host>(endpoint, parseHost, options);
}

export function parseHost(raw: unknown): Host | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.display_name !== "string" ||
    typeof r.hostname !== "string" ||
    typeof r.port !== "number" ||
    typeof r.default_username !== "string" ||
    typeof r.created_at !== "string" ||
    typeof r.updated_at !== "string"
  ) {
    return null;
  }
  if (!Number.isInteger(r.port) || r.port < 1 || r.port > 65535) {
    return null;
  }
  return {
    id: r.id,
    display_name: r.display_name,
    hostname: r.hostname,
    port: r.port,
    default_username: r.default_username,
    created_at: r.created_at,
    updated_at: r.updated_at,
  };
}

/**
 * Length and value bounds mirroring the backend validators in
 * `crates/relayterm-core/src/validation.rs`. Kept in sync by hand —
 * drift would still be caught server-side as `400 invalid_input`, but
 * the local checks let the UI refuse a too-long name without a wire
 * round-trip.
 */
export const MAX_HOST_DISPLAY_NAME_LEN = 128;
export const MAX_HOSTNAME_LEN = 253;
export const MAX_USERNAME_LEN = 64;
export const DEFAULT_SSH_PORT = 22;

/**
 * Request body for `POST /api/v1/hosts`. Mirrors the backend's
 * `CreateHostRequest`. Port is optional on the wire (defaults to 22
 * server-side); the validator below normalizes it before sending so
 * the request shape is explicit.
 */
export interface CreateHostRequest {
  display_name: string;
  hostname: string;
  /** Optional; defaults to {@link DEFAULT_SSH_PORT} when omitted. */
  port?: number;
  default_username: string;
}

export type CreateHostInvalidReason =
  | "missing_display_name"
  | "display_name_has_surrounding_whitespace"
  | "display_name_too_long"
  | "display_name_has_control_chars"
  | "missing_hostname"
  | "hostname_too_long"
  | "hostname_has_whitespace"
  | "hostname_has_control_chars"
  | "hostname_has_invalid_char"
  | "port_out_of_range"
  | "missing_username"
  | "username_too_long"
  | "username_bad_leading_char"
  | "username_has_invalid_char";

export type CreateHostValidation =
  | {
      ok: true;
      body: {
        display_name: string;
        hostname: string;
        port: number;
        default_username: string;
      };
    }
  | { ok: false; reason: CreateHostInvalidReason };

// ASCII-only host character allowlist mirroring `validate_hostname`:
// alphanumerics, `-`, `.`, `:`, `[`, `]`, `_`. Whitespace and control
// characters are checked separately so the formatter can produce
// distinct reasons.
const HOSTNAME_ALLOWED = /^[A-Za-z0-9\-.:\[\]_]+$/;

// SSH-username allowlist after the leading char: ASCII alphanumerics,
// `-`, `_`, `.`. Mirrors the backend's `validate_ssh_username` rules.
const USERNAME_TAIL_ALLOWED = /^[A-Za-z0-9_.\-]*$/;

// eslint-disable-next-line no-control-regex
const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F]/;

/**
 * Validate a create-host request on the client. Mirrors the backend's
 * field-level rules in `crates/relayterm-core/src/validation.rs`. The
 * backend remains authoritative; a local refusal lets the UI show a
 * precise reason without burning a wire round-trip.
 */
export function validateCreateHostRequest(
  raw: CreateHostRequest,
): CreateHostValidation {
  const display_name = raw.display_name ?? "";
  if (display_name.length === 0) {
    return { ok: false, reason: "missing_display_name" };
  }
  if (display_name.trim() !== display_name) {
    return { ok: false, reason: "display_name_has_surrounding_whitespace" };
  }
  if ([...display_name].length > MAX_HOST_DISPLAY_NAME_LEN) {
    return { ok: false, reason: "display_name_too_long" };
  }
  if (CONTROL_CHARS.test(display_name)) {
    return { ok: false, reason: "display_name_has_control_chars" };
  }

  const hostname = raw.hostname ?? "";
  if (hostname.length === 0) {
    return { ok: false, reason: "missing_hostname" };
  }
  if (hostname.length > MAX_HOSTNAME_LEN) {
    return { ok: false, reason: "hostname_too_long" };
  }
  if (/\s/.test(hostname)) {
    return { ok: false, reason: "hostname_has_whitespace" };
  }
  if (CONTROL_CHARS.test(hostname)) {
    return { ok: false, reason: "hostname_has_control_chars" };
  }
  if (!HOSTNAME_ALLOWED.test(hostname)) {
    return { ok: false, reason: "hostname_has_invalid_char" };
  }

  const port = raw.port ?? DEFAULT_SSH_PORT;
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    return { ok: false, reason: "port_out_of_range" };
  }

  const default_username = raw.default_username ?? "";
  if (default_username.length === 0) {
    return { ok: false, reason: "missing_username" };
  }
  if (default_username.length > MAX_USERNAME_LEN) {
    return { ok: false, reason: "username_too_long" };
  }
  const first = default_username.charCodeAt(0);
  const isLetter =
    (first >= 65 && first <= 90) || (first >= 97 && first <= 122);
  const isUnderscore = first === 95;
  if (!isLetter && !isUnderscore) {
    return { ok: false, reason: "username_bad_leading_char" };
  }
  if (!USERNAME_TAIL_ALLOWED.test(default_username.slice(1))) {
    return { ok: false, reason: "username_has_invalid_char" };
  }

  return {
    ok: true,
    body: { display_name, hostname, port, default_username },
  };
}

export type CreateHostError =
  | { kind: "validation"; reason: CreateHostInvalidReason }
  | WireError;

/**
 * Format a {@link CreateHostError} as a one-line UI summary.
 *
 * Stays a function of `kind` + `status` + `code` (and the validation
 * `reason` enum) only — never echoes the wire `message` of an HTTP
 * error or the thrown `Error.message` of a transport failure. The
 * typed error object preserves both for programmatic callers.
 */
export function describeCreateHostError(err: CreateHostError): string {
  switch (err.kind) {
    case "validation":
      return `Cannot create host: ${describeHostValidationReason(err.reason)}`;
    case "http":
      return `Failed to create host: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Failed to create host: transport error";
    case "malformed_response":
      return "Failed to create host: malformed response";
  }
}

function describeHostValidationReason(reason: CreateHostInvalidReason): string {
  switch (reason) {
    case "missing_display_name":
      return "display name is required";
    case "display_name_has_surrounding_whitespace":
      return "display name must not start or end with whitespace";
    case "display_name_too_long":
      return `display name must be at most ${MAX_HOST_DISPLAY_NAME_LEN} characters`;
    case "display_name_has_control_chars":
      return "display name must not contain control characters";
    case "missing_hostname":
      return "hostname is required";
    case "hostname_too_long":
      return `hostname must be at most ${MAX_HOSTNAME_LEN} characters`;
    case "hostname_has_whitespace":
      return "hostname must not contain whitespace";
    case "hostname_has_control_chars":
      return "hostname must not contain control characters";
    case "hostname_has_invalid_char":
      return "hostname may only contain letters, digits, '-', '.', ':', '[', ']', '_'";
    case "port_out_of_range":
      return "port must be an integer between 1 and 65535";
    case "missing_username":
      return "default username is required";
    case "username_too_long":
      return `default username must be at most ${MAX_USERNAME_LEN} characters`;
    case "username_bad_leading_char":
      return "default username must start with a letter or '_'";
    case "username_has_invalid_char":
      return "default username may only contain letters, digits, '-', '_', '.'";
  }
}

export interface CreateHostOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/hosts`. */
  endpoint?: string;
}

export type CreateHostResult =
  | { ok: true; host: Host }
  | { ok: false; error: CreateHostError };

/**
 * POST a create-host request and parse the response.
 *
 * On a 2xx, the response is parsed by {@link parseHost}; an unparseable
 * body collapses to `malformed_response`. The function does not throw,
 * does not log raw response bodies, and does not echo wire / transport
 * detail through the formatter.
 *
 * Backend semantics this helper does NOT change:
 *  - The host is a metadata-only target definition.
 *  - No SSH connection, host-key probe, or auth-check is performed.
 */
export async function createHost(
  raw: CreateHostRequest,
  options: CreateHostOptions = {},
): Promise<CreateHostResult> {
  const validation = validateCreateHostRequest(raw);
  if (!validation.ok) {
    return {
      ok: false,
      error: { kind: "validation", reason: validation.reason },
    };
  }
  const endpoint = options.endpoint ?? "/api/v1/hosts";
  const result = await postJsonItem<Host>(
    endpoint,
    validation.body,
    parseHost,
    options,
  );
  if (!result.ok) {
    return { ok: false, error: result.error };
  }
  return { ok: true, host: result.data };
}

// ---------------------------------------------------------------------------
// PATCH / DELETE helpers
// ---------------------------------------------------------------------------
//
// Wire shapes mirror `crates/relayterm-api/src/dto/host.rs` (PATCH) and the
// 204-No-Content response from `DELETE /api/v1/hosts/:id`. The helpers do
// NOT throw, do NOT log raw response bodies, and do NOT echo wire / transport
// detail through any user-facing string.

/**
 * Request body for `PATCH /api/v1/hosts/:id`.
 *
 * Every field is optional — only the fields the caller supplies are sent.
 * An entirely-empty body is refused at the client validator AND at the
 * backend (`400 invalid_input`).
 */
export interface UpdateHostRequest {
  display_name?: string;
  hostname?: string;
  port?: number;
  default_username?: string;
}

export type UpdateHostInvalidReason =
  | "empty_update"
  | CreateHostInvalidReason;

export type UpdateHostValidation =
  | {
      ok: true;
      body: {
        display_name?: string;
        hostname?: string;
        port?: number;
        default_username?: string;
      };
    }
  | { ok: false; reason: UpdateHostInvalidReason };

/**
 * Validate a partial-update host request on the client. Mirrors the
 * per-field validators in {@link validateCreateHostRequest} and adds
 * the empty-body refusal so the UI does not ship a no-op PATCH.
 */
export function validateUpdateHostRequest(
  raw: UpdateHostRequest,
): UpdateHostValidation {
  const out: {
    display_name?: string;
    hostname?: string;
    port?: number;
    default_username?: string;
  } = {};
  let touched = false;

  if (raw.display_name !== undefined) {
    touched = true;
    // Delegate to the create validator with placeholder values for the
    // other fields, so a single source of truth runs the per-field
    // rules. The delegated call cannot accept partial input — it would
    // fail with a missing-field reason for the absent ones — hence the
    // placeholders. They are valid for the OTHER validators, so the
    // ONLY reasons that can surface here are display-name-related.
    const v = validateCreateHostRequest({
      display_name: raw.display_name,
      hostname: "placeholder.example.com",
      port: DEFAULT_SSH_PORT,
      default_username: "placeholder",
    });
    if (!v.ok) {
      // Catch-all: any future display-name reason added to the validator
      // surfaces here even if the prefix changes. The placeholders mean
      // we cannot see any non-display-name reason — but if that ever
      // changes (e.g. cross-field validation), failing closed is safer
      // than silently passing.
      return { ok: false, reason: v.reason };
    }
    out.display_name = raw.display_name;
  }
  if (raw.hostname !== undefined) {
    touched = true;
    const v = validateCreateHostRequest({
      display_name: "placeholder",
      hostname: raw.hostname,
      port: DEFAULT_SSH_PORT,
      default_username: "placeholder",
    });
    if (!v.ok) {
      return { ok: false, reason: v.reason };
    }
    out.hostname = raw.hostname;
  }
  if (raw.port !== undefined) {
    touched = true;
    if (!Number.isInteger(raw.port) || raw.port < 1 || raw.port > 65535) {
      return { ok: false, reason: "port_out_of_range" };
    }
    out.port = raw.port;
  }
  if (raw.default_username !== undefined) {
    touched = true;
    const v = validateCreateHostRequest({
      display_name: "placeholder",
      hostname: "placeholder.example.com",
      port: DEFAULT_SSH_PORT,
      default_username: raw.default_username,
    });
    if (!v.ok) {
      return { ok: false, reason: v.reason };
    }
    out.default_username = raw.default_username;
  }
  if (!touched) {
    return { ok: false, reason: "empty_update" };
  }
  return { ok: true, body: out };
}

export type UpdateHostError =
  | { kind: "validation"; reason: UpdateHostInvalidReason }
  | WireError;

export type UpdateHostResult =
  | { ok: true; host: Host }
  | { ok: false; error: UpdateHostError };

export interface UpdateHostOptions extends LoadOptions {
  endpoint?: string;
}

/**
 * PATCH a host and parse the updated row. The helper does not throw
 * and does not echo wire / transport detail through `describeUpdateHostError`.
 */
export async function updateHost(
  id: string,
  raw: UpdateHostRequest,
  options: UpdateHostOptions = {},
): Promise<UpdateHostResult> {
  const validation = validateUpdateHostRequest(raw);
  if (!validation.ok) {
    return {
      ok: false,
      error: { kind: "validation", reason: validation.reason },
    };
  }
  const endpoint =
    options.endpoint ?? `/api/v1/hosts/${encodeURIComponent(id)}`;
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
      method: "PATCH",
      headers: {
        accept: "application/json",
        "content-type": "application/json",
      },
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
  const parsed = parseHost(body);
  if (!parsed) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, host: parsed };
}

/**
 * Format an {@link UpdateHostError} as a one-line UI summary. Stays a
 * function of `kind` + `status` + `code` (and the validation `reason`
 * enum) only — never echoes the wire `message` of an HTTP error or
 * the thrown `Error.message` of a transport failure.
 */
export function describeUpdateHostError(err: UpdateHostError): string {
  switch (err.kind) {
    case "validation":
      if (err.reason === "empty_update") {
        return "Cannot save host: change at least one field";
      }
      return `Cannot save host: ${describeHostValidationReason(err.reason)}`;
    case "http":
      if (err.status === 404 && err.code === "not_found") {
        return "Failed to save host: host not found";
      }
      if (err.status === 401) {
        return "Failed to save host: not authenticated";
      }
      if (err.status === 403 && err.code === "csrf_origin_mismatch") {
        return "Failed to save host: request blocked by browser security policy";
      }
      return `Failed to save host: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Failed to save host: transport error";
    case "malformed_response":
      return "Failed to save host: malformed response";
  }
}

/**
 * Typed error envelope returned by {@link deleteHost}. The 409 case
 * carries the typed `reason` so the formatter can render precise copy
 * without echoing the wire `message`.
 */
export type DeleteHostError =
  | {
      kind: "http";
      status: number;
      code: string;
      message: string;
      /** Derived 409 discriminator; `null` for non-409 statuses or
       * unrecognised wire reasons. */
      reason: "referenced" | null;
    }
  | { kind: "transport"; message: string };

export type DeleteHostResult =
  | { ok: true }
  | { ok: false; error: DeleteHostError };

export interface DeleteHostOptions extends LoadOptions {
  endpoint?: string;
}

/**
 * DELETE a host. Returns `{ ok: true }` on `204 No Content`; maps a
 * `409 conflict { entity: "host", reason: "referenced" }` to a typed
 * `referenced` discriminator. Does not throw, does not log.
 */
export async function deleteHost(
  id: string,
  options: DeleteHostOptions = {},
): Promise<DeleteHostResult> {
  const endpoint =
    options.endpoint ?? `/api/v1/hosts/${encodeURIComponent(id)}`;
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return {
      ok: false,
      error: { kind: "transport", message: "fetch unavailable" },
    };
  }
  let response: Response;
  try {
    response = await fetchImpl(endpoint, { method: "DELETE" });
  } catch (err) {
    return {
      ok: false,
      error: {
        kind: "transport",
        message: err instanceof Error ? err.message : "unknown",
      },
    };
  }
  if (response.status === 204) return { ok: true };
  const { code, message } = await readErrorEnvelope(response);
  // Map the 409 wire body to the typed reason. The on-wire form is
  // `host referenced` per `ApiError::Conflict { entity: "host",
  // reason: Some("referenced") }`. Anything else collapses to `null`
  // so the formatter falls back to a generic 409 string and the wire
  // message is never re-emitted to the UI.
  const reason: "referenced" | null =
    response.status === 409 && message === "host referenced" ? "referenced" : null;
  return {
    ok: false,
    error: { kind: "http", status: response.status, code, message, reason },
  };
}

/**
 * Format a {@link DeleteHostError} as a one-line UI summary. Stays a
 * function of `kind` + `status` + `code` + the derived `reason`
 * discriminator only.
 */
export function describeDeleteHostError(err: DeleteHostError): string {
  switch (err.kind) {
    case "http":
      if (err.status === 409 && err.reason === "referenced") {
        return "Cannot delete host: it is still used by a saved server profile or has trusted host keys — remove the dependent items first";
      }
      if (err.status === 404 && err.code === "not_found") {
        return "Cannot delete host: host not found";
      }
      if (err.status === 401) {
        return "Cannot delete host: not authenticated";
      }
      if (err.status === 403 && err.code === "csrf_origin_mismatch") {
        return "Cannot delete host: request blocked by browser security policy";
      }
      return `Cannot delete host: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Cannot delete host: transport error";
  }
}
