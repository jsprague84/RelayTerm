/**
 * Frontend helpers for `/api/v1/auth/*` (bootstrap / login / logout / me).
 *
 * Surface: {@link getCurrentUser}, {@link login}, {@link logout},
 * {@link bootstrap}, plus a small typed-error union and a single
 * {@link describeAuthError} formatter that reaches the UI.
 *
 * **Security-critical.** This module is the only place in the SPA that
 * touches the auth wire surface. The rules below are load-bearing:
 *
 *  - Every request uses `credentials: "include"` so the browser ships
 *    the `relayterm_session` cookie. The cookie itself is `HttpOnly` —
 *    nothing in this module reads, writes, or echoes the cookie value.
 *  - The {@link CurrentUserResponse} parser is field-by-field and
 *    declares the public-user fields only (`id`, `email`, `display_name`,
 *    `created_at`, `last_login_at`). A stray `password_hash`,
 *    `session_token`, `token_hash`, or `bootstrap_token` on a backend
 *    response cannot smuggle onto the parsed object — sentinel-string
 *    tests in `tests/authApi.test.ts` pin this.
 *  - {@link describeAuthError} stays a function of `kind` + `status` +
 *    `code` only — it never echoes the wire `message` of an HTTP error,
 *    the thrown `Error.message` of a transport failure, the offered
 *    plaintext password / bootstrap token, or any other request input.
 *    The session-management formatter {@link describeAuthSessionsError}
 *    is strictly tighter (function of `kind` + `status` only — `code`
 *    is dropped entirely) because the session surface has no
 *    per-`code` UI branching today; both formatters preserve the
 *    same redaction posture.
 *  - No path here logs, throws, or formats raw response bodies.
 *  - Login validation copy never reveals whether the offered email
 *    belongs to a known account ("invalid credentials" only).
 */

import { readErrorEnvelope, type LoadOptions } from "./apiErrors.js";

// ---------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------

/**
 * Public-safe user profile DTO. Mirrors the backend's `UserResponse`
 * (`crates/relayterm-api/src/dto/auth.rs`); secret-shaped fields are
 * intentionally NOT declared on this interface, so a backend bug or
 * hostile fixture that smuggled `password_hash` / `session_token` /
 * `token_hash` / `bootstrap_token` onto the response cannot reach the
 * parsed object.
 */
export interface CurrentUser {
  id: string;
  email: string;
  display_name: string;
  /** RFC 3339 timestamp. */
  created_at: string;
  /** RFC 3339 timestamp; absent for a freshly-bootstrapped user that
   * has not logged in yet. */
  last_login_at: string | null;
}

/**
 * Login response shape. Backend returns the same `UserResponse` shape on
 * `POST /auth/login` 200; the cookie is set via `Set-Cookie`. The body
 * carries no token — the cookie is the single legitimate sink for the
 * session-token plaintext.
 */
export type LoginResponse = CurrentUser;

/**
 * Bootstrap response shape. Backend returns the same `UserResponse` shape
 * on `POST /auth/bootstrap` 201; bootstrap does NOT mint a session, so
 * no cookie is set. The SPA must call {@link login} next to obtain the
 * session cookie.
 */
export type BootstrapResponse = CurrentUser;

/**
 * Build a {@link CurrentUser} from an unknown JSON body. Field-by-field
 * construction is the redaction backstop: secret-shaped properties on
 * the input cannot reach the returned object because no path here
 * copies them. Returns `null` on any missing or wrong-typed required
 * field; unknown extra fields are silently dropped (mirroring the
 * inventory parsers).
 */
export function parseCurrentUser(raw: unknown): CurrentUser | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.email !== "string" ||
    typeof r.display_name !== "string" ||
    typeof r.created_at !== "string"
  ) {
    return null;
  }
  if (r.last_login_at !== null && typeof r.last_login_at !== "string") {
    return null;
  }
  return {
    id: r.id,
    email: r.email,
    display_name: r.display_name,
    created_at: r.created_at,
    last_login_at: (r.last_login_at as string | null) ?? null,
  };
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/**
 * Typed auth error union. The HTTP variant carries `status` + `code` +
 * `message` for programmatic callers, but the formatter
 * ({@link describeAuthError}) is the single point that reaches the UI
 * and stays a function of `kind` + `status` + `code` only.
 */
export type AuthError =
  | { kind: "http"; status: number; code: string; message: string }
  | { kind: "transport" }
  | { kind: "malformed_response" };

export type AuthAction = "sign in" | "sign out" | "first-time setup" | "load session";

/**
 * Format an {@link AuthError} as a one-line UI summary. Stays a
 * function of `kind` + `status` + `code` only — never echoes the wire
 * `message`, the transport detail, or any request input. The login
 * 401 collapses to a generic "invalid credentials" string so a probe
 * cannot learn whether the offered email belongs to an account.
 */
export function describeAuthError(action: AuthAction, err: AuthError): string {
  switch (err.kind) {
    case "http":
      if (err.status === 401) {
        if (action === "sign in") {
          return "Sign in failed: invalid credentials";
        }
        if (action === "first-time setup") {
          return "First-time setup failed: bootstrap token rejected";
        }
        return "Your session has ended. Please sign in again.";
      }
      if (err.status === 403 && err.code === "csrf_origin_mismatch") {
        return `Cannot ${action}: request blocked by browser security policy`;
      }
      if (err.status === 409) {
        if (action === "first-time setup") {
          return "First-time setup is no longer available: an account already exists";
        }
        return `Cannot ${action}: HTTP 409 ${err.code}`;
      }
      if (err.status === 503) {
        if (action === "first-time setup") {
          return "First-time setup is disabled on this server";
        }
        return `Cannot ${action}: backend is not available`;
      }
      return `Cannot ${action}: HTTP ${err.status} ${err.code}`;
    case "transport":
      return `Cannot ${action}: network error`;
    case "malformed_response":
      return `Cannot ${action}: malformed response`;
  }
}

// ---------------------------------------------------------------------
// Internal request helpers
// ---------------------------------------------------------------------

interface AuthFetchOptions extends LoadOptions {
  endpoint: string;
}

interface AuthPostOptions<T> extends AuthFetchOptions {
  body?: unknown;
  parse: (raw: unknown) => T | null;
}

/**
 * Build the standard fetch init for an auth request. Always
 * `credentials: "include"` so the browser ships and accepts the
 * session cookie. The browser controls the `Origin` header on POSTs;
 * we deliberately do NOT set it from JS (the backend's `Origin` /
 * CSRF guard is appeased by the browser-attached value, never by a
 * JS-supplied one).
 */
function authFetchInit(
  method: "GET" | "POST",
  body: unknown | undefined,
): RequestInit {
  const init: RequestInit = {
    method,
    credentials: "include",
    headers: { accept: "application/json" },
  };
  if (body !== undefined) {
    (init.headers as Record<string, string>)["content-type"] =
      "application/json";
    init.body = JSON.stringify(body);
  }
  return init;
}

async function authPost<T>(
  opts: AuthPostOptions<T>,
): Promise<{ ok: true; data: T } | { ok: false; error: AuthError }> {
  const fetchImpl = opts.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return { ok: false, error: { kind: "transport" } };
  }

  let response: Response;
  try {
    response = await fetchImpl(opts.endpoint, authFetchInit("POST", opts.body));
  } catch {
    return { ok: false, error: { kind: "transport" } };
  }

  if (!response.ok) {
    const { code, message } = await readErrorEnvelope(response);
    return {
      ok: false,
      error: { kind: "http", status: response.status, code, message },
    };
  }

  // 204 (logout) carries no body; collapse to a minimal-success shape.
  if (response.status === 204) {
    return { ok: true, data: null as unknown as T };
  }

  let parsed: unknown;
  try {
    parsed = await response.json();
  } catch {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  const item = opts.parse(parsed);
  if (item === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, data: item };
}

// ---------------------------------------------------------------------
// getCurrentUser
// ---------------------------------------------------------------------

export type GetCurrentUserResult =
  | { ok: true; user: CurrentUser }
  | { ok: false; error: AuthError };

export interface GetCurrentUserOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/auth/me`. */
  endpoint?: string;
}

/**
 * `GET /api/v1/auth/me`. Resolves to `{ ok: true, user }` for a valid
 * session cookie; an `{ ok: false, error: { kind: "http", status: 401 } }`
 * means "no valid session" and is the canonical signal for the SPA to
 * render {@link import("../app/views/LoginView.svelte").default}.
 *
 * The helper does NOT throw, does NOT log, and does NOT echo response
 * detail through the formatter. Callers that need a one-line UI summary
 * format via {@link describeAuthError}.
 */
export async function getCurrentUser(
  options: GetCurrentUserOptions = {},
): Promise<GetCurrentUserResult> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return { ok: false, error: { kind: "transport" } };
  }
  const endpoint = options.endpoint ?? "/api/v1/auth/me";

  let response: Response;
  try {
    response = await fetchImpl(endpoint, authFetchInit("GET", undefined));
  } catch {
    return { ok: false, error: { kind: "transport" } };
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
  const user = parseCurrentUser(parsed);
  if (user === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, user };
}

// ---------------------------------------------------------------------
// login
// ---------------------------------------------------------------------

export interface LoginRequest {
  email: string;
  password: string;
}

export type LoginResult =
  | { ok: true; user: LoginResponse }
  | { ok: false; error: AuthError };

export interface LoginOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/auth/login`. */
  endpoint?: string;
}

/**
 * `POST /api/v1/auth/login`. On 200 the backend has set the session
 * cookie via `Set-Cookie`; the SPA does not read or store it. The
 * response body is parsed via {@link parseCurrentUser} so a stray
 * secret-shaped field cannot smuggle onto the returned object.
 *
 * The helper does NOT echo the offered email or password into any
 * error or log path. The 401 collapse rule lives in
 * {@link describeAuthError}.
 */
export async function login(
  request: LoginRequest,
  options: LoginOptions = {},
): Promise<LoginResult> {
  const result = await authPost<LoginResponse>({
    endpoint: options.endpoint ?? "/api/v1/auth/login",
    fetchImpl: options.fetchImpl,
    body: { email: request.email, password: request.password },
    parse: parseCurrentUser,
  });
  if (!result.ok) return { ok: false, error: result.error };
  return { ok: true, user: result.data };
}

// ---------------------------------------------------------------------
// logout
// ---------------------------------------------------------------------

export type LogoutResult =
  | { ok: true }
  | { ok: false; error: AuthError };

export interface LogoutOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/auth/logout`. */
  endpoint?: string;
}

/**
 * `POST /api/v1/auth/logout`. The backend returns 204 with a clear-
 * cookie `Set-Cookie` header for both real revocation and no-op
 * (missing/unknown/already-revoked cookie) cases.
 *
 * The SPA always clears local auth state on call, regardless of the
 * wire outcome — see `AppShell.svelte` for the local-cleanup contract
 * (`clearActiveSession` + currentUser reset). A failed network call
 * therefore does NOT trap the user in a logged-in UI state.
 */
export async function logout(
  options: LogoutOptions = {},
): Promise<LogoutResult> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return { ok: false, error: { kind: "transport" } };
  }
  const endpoint = options.endpoint ?? "/api/v1/auth/logout";

  let response: Response;
  try {
    response = await fetchImpl(endpoint, authFetchInit("POST", undefined));
  } catch {
    return { ok: false, error: { kind: "transport" } };
  }
  if (!response.ok) {
    const { code, message } = await readErrorEnvelope(response);
    return {
      ok: false,
      error: { kind: "http", status: response.status, code, message },
    };
  }
  return { ok: true };
}

// ---------------------------------------------------------------------
// bootstrap
// ---------------------------------------------------------------------

export interface BootstrapRequest {
  bootstrap_token: string;
  email: string;
  display_name: string;
  password: string;
}

export type BootstrapResult =
  | { ok: true; user: BootstrapResponse }
  | { ok: false; error: AuthError };

export interface BootstrapOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/auth/bootstrap`. */
  endpoint?: string;
}

/**
 * `POST /api/v1/auth/bootstrap`. Creates the first user; does NOT mint
 * a session and does NOT set a cookie. The SPA's flow on success is
 * "show 'Account created. Please sign in.'" — never auto-login from
 * this surface, since that would silently turn bootstrap into a second
 * unauthenticated session-issuing route.
 *
 * The bootstrap token and password are passed straight to the wire
 * body and never echoed into any error / log path. The validation
 * 400 collapses to a generic message via {@link describeAuthError}.
 */
export async function bootstrap(
  request: BootstrapRequest,
  options: BootstrapOptions = {},
): Promise<BootstrapResult> {
  const result = await authPost<BootstrapResponse>({
    endpoint: options.endpoint ?? "/api/v1/auth/bootstrap",
    fetchImpl: options.fetchImpl,
    body: {
      bootstrap_token: request.bootstrap_token,
      email: request.email,
      display_name: request.display_name,
      password: request.password,
    },
    parse: parseCurrentUser,
  });
  if (!result.ok) return { ok: false, error: result.error };
  return { ok: true, user: result.data };
}

// ---------------------------------------------------------------------
// Validation helpers (client-side mirror of backend rules)
// ---------------------------------------------------------------------

/** Mirrors the backend's `PASSWORD_MIN_LEN` in `dto/auth.rs`. */
export const PASSWORD_MIN_LEN = 12;
/** Mirrors the backend's `PASSWORD_MAX_LEN` in `dto/auth.rs`. */
export const PASSWORD_MAX_LEN = 1024;
/** Mirrors the backend's `EMAIL_MAX_LEN`. */
export const EMAIL_MAX_LEN = 320;
/** Mirrors the backend's `DISPLAY_NAME_MAX_LEN`. */
export const DISPLAY_NAME_MAX_LEN = 200;
/** Mirrors the backend's `BOOTSTRAP_TOKEN_MAX_LEN`. */
export const BOOTSTRAP_TOKEN_MAX_LEN = 4096;

export type LoginFormError =
  | "missing_email"
  | "email_invalid"
  | "missing_password"
  | "password_too_short";

/**
 * Validate a login form on the client. Rules mirror the backend's
 * `LoginRequest::validated`. Failure produces a typed enum that the
 * UI maps to a static string via {@link describeLoginFormError}.
 *
 * Importantly: the strings returned here NEVER reveal whether the
 * offered email belongs to a known account. The "user not found" /
 * "wrong password" distinction lives only in the operator-side audit
 * log; the wire and the UI both collapse to "invalid credentials".
 */
export function validateLoginForm(
  raw: LoginRequest,
): { ok: true } | { ok: false; reason: LoginFormError } {
  if (!raw.email || raw.email.length === 0) {
    return { ok: false, reason: "missing_email" };
  }
  if (!looksLikeEmail(raw.email) || raw.email.length > EMAIL_MAX_LEN) {
    return { ok: false, reason: "email_invalid" };
  }
  if (!raw.password || raw.password.length === 0) {
    return { ok: false, reason: "missing_password" };
  }
  if (raw.password.length < PASSWORD_MIN_LEN) {
    return { ok: false, reason: "password_too_short" };
  }
  return { ok: true };
}

export function describeLoginFormError(reason: LoginFormError): string {
  switch (reason) {
    case "missing_email":
      return "Enter your email.";
    case "email_invalid":
      return "Enter a valid email.";
    case "missing_password":
      return "Enter your password.";
    case "password_too_short":
      return `Password must be at least ${PASSWORD_MIN_LEN} characters.`;
  }
}

export type BootstrapFormError =
  | "missing_bootstrap_token"
  | "bootstrap_token_too_long"
  | "missing_email"
  | "email_invalid"
  | "missing_display_name"
  | "display_name_too_long"
  | "missing_password"
  | "password_too_short"
  | "password_too_long"
  | "password_confirmation_mismatch";

export interface BootstrapFormDraft extends BootstrapRequest {
  password_confirmation: string;
}

/**
 * Validate a bootstrap form on the client. Rules mirror the backend's
 * `BootstrapRequest::validated` plus a frontend-only password-confirm
 * check (the backend does not see the confirmation field — it is a
 * UX-only safety against a typo on first-account creation).
 */
export function validateBootstrapForm(
  raw: BootstrapFormDraft,
): { ok: true } | { ok: false; reason: BootstrapFormError } {
  if (!raw.bootstrap_token || raw.bootstrap_token.length === 0) {
    return { ok: false, reason: "missing_bootstrap_token" };
  }
  if (raw.bootstrap_token.length > BOOTSTRAP_TOKEN_MAX_LEN) {
    return { ok: false, reason: "bootstrap_token_too_long" };
  }
  if (!raw.email || raw.email.length === 0) {
    return { ok: false, reason: "missing_email" };
  }
  if (!looksLikeEmail(raw.email) || raw.email.length > EMAIL_MAX_LEN) {
    return { ok: false, reason: "email_invalid" };
  }
  if (!raw.display_name || raw.display_name.length === 0) {
    return { ok: false, reason: "missing_display_name" };
  }
  if (raw.display_name.length > DISPLAY_NAME_MAX_LEN) {
    return { ok: false, reason: "display_name_too_long" };
  }
  if (!raw.password || raw.password.length === 0) {
    return { ok: false, reason: "missing_password" };
  }
  if (raw.password.length < PASSWORD_MIN_LEN) {
    return { ok: false, reason: "password_too_short" };
  }
  if (raw.password.length > PASSWORD_MAX_LEN) {
    return { ok: false, reason: "password_too_long" };
  }
  if (raw.password !== raw.password_confirmation) {
    return { ok: false, reason: "password_confirmation_mismatch" };
  }
  return { ok: true };
}

export function describeBootstrapFormError(reason: BootstrapFormError): string {
  switch (reason) {
    case "missing_bootstrap_token":
      return "Enter the bootstrap token.";
    case "bootstrap_token_too_long":
      return `Bootstrap token must be at most ${BOOTSTRAP_TOKEN_MAX_LEN} characters.`;
    case "missing_email":
      return "Enter the account email.";
    case "email_invalid":
      return "Enter a valid email.";
    case "missing_display_name":
      return "Enter the account display name.";
    case "display_name_too_long":
      return `Display name must be at most ${DISPLAY_NAME_MAX_LEN} characters.`;
    case "missing_password":
      return "Enter a password.";
    case "password_too_short":
      return `Password must be at least ${PASSWORD_MIN_LEN} characters.`;
    case "password_too_long":
      return `Password must be at most ${PASSWORD_MAX_LEN} characters.`;
    case "password_confirmation_mismatch":
      return "Passwords do not match.";
  }
}

/**
 * One-line UI summary for a failed `getCurrentUser()` call that is NOT
 * a 401 (a 401 means "no valid session" and is the canonical signal to
 * render the login screen — that case is not formatted as an error).
 *
 * Stays a function of `kind` + `status` only — never echoes the wire
 * `message`, the wire `code`, or transport detail. Used by `AuthGate`'s
 * loading-error surface so the inline message construction stays
 * sentinel-tested instead of free-floating in component code.
 */
export function describeAuthGateError(err: AuthError): string {
  if (err.kind === "http") {
    return `Cannot reach the backend: HTTP ${err.status}`;
  }
  if (err.kind === "transport") {
    return "Cannot reach the backend.";
  }
  return "Cannot reach the backend: malformed response.";
}

// ---------------------------------------------------------------------
// Current-user session management (`/api/v1/auth/sessions`)
// ---------------------------------------------------------------------

/**
 * Wire-side status discriminator for one session row. Mirrors the
 * backend's `SessionStatus` enum (`crates/relayterm-api/src/dto/auth.rs`).
 *
 * The backend collapses "revoked AND expired" to `revoked` because
 * revocation is the deliberate-action signal and expiry is a passive
 * timestamp; the SPA just renders whatever the backend says.
 */
export type AuthSessionStatus = "active" | "expired" | "revoked";

/**
 * Public-safe session DTO. Mirrors the backend's `SessionListItem`.
 *
 * **Token-redacted by construction.** This interface deliberately omits
 * `token_hash` and has no plaintext-token field — the only public
 * reference to a session is its `id`. {@link parseAuthSession} builds
 * the object field-by-field, so a stray `token_hash` / `session_token`
 * / `password_hash` / `bootstrap_token` / `private_key` /
 * `encrypted_private_key` / `access_token` / `session_output` on the
 * wire CANNOT smuggle onto the parsed object — sentinel-string tests in
 * `tests/authSessionsApi.test.ts` pin this.
 */
export interface AuthSession {
  id: string;
  /** RFC 3339 timestamp. */
  created_at: string;
  /** RFC 3339 timestamp. */
  last_seen_at: string;
  /** RFC 3339 timestamp. */
  expires_at: string;
  /** RFC 3339 timestamp; null when the session has not been revoked. */
  revoked_at: string | null;
  /** True for the row that authenticated THIS request. */
  current: boolean;
  status: AuthSessionStatus;
}

/**
 * Build an {@link AuthSession} from an unknown wire object. Returns
 * `null` for any missing or wrong-typed required field. Field-by-field
 * construction is the redaction backstop: secret-shaped properties on
 * the input cannot reach the returned object because no path here
 * copies them.
 *
 * The `status` discriminator is validated against the closed
 * {@link AuthSessionStatus} union; an unknown status value collapses
 * the row to `null` so the loader treats it as a malformed response
 * rather than displaying an unmapped state.
 */
export function parseAuthSession(raw: unknown): AuthSession | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.created_at !== "string" ||
    typeof r.last_seen_at !== "string" ||
    typeof r.expires_at !== "string" ||
    typeof r.current !== "boolean" ||
    typeof r.status !== "string"
  ) {
    return null;
  }
  if (r.revoked_at !== null && typeof r.revoked_at !== "string") {
    return null;
  }
  if (
    r.status !== "active" &&
    r.status !== "expired" &&
    r.status !== "revoked"
  ) {
    return null;
  }
  return {
    id: r.id,
    created_at: r.created_at,
    last_seen_at: r.last_seen_at,
    expires_at: r.expires_at,
    revoked_at: (r.revoked_at as string | null) ?? null,
    current: r.current,
    status: r.status,
  };
}

export interface AuthSessionsListResponse {
  sessions: AuthSession[];
}

function parseAuthSessionsListResponse(
  raw: unknown,
): AuthSessionsListResponse | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (!Array.isArray(r.sessions)) return null;
  const sessions: AuthSession[] = [];
  for (const item of r.sessions) {
    const parsed = parseAuthSession(item);
    if (parsed === null) return null;
    sessions.push(parsed);
  }
  return { sessions };
}

export interface RevokeAllAuthSessionsResponse {
  revoked_count: number;
}

function parseRevokeAllResponse(
  raw: unknown,
): RevokeAllAuthSessionsResponse | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.revoked_count !== "number") return null;
  if (!Number.isFinite(r.revoked_count) || r.revoked_count < 0) return null;
  return { revoked_count: r.revoked_count };
}

export type ListAuthSessionsResult =
  | { ok: true; sessions: AuthSession[] }
  | { ok: false; error: AuthError };

export interface ListAuthSessionsOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/auth/sessions`. */
  endpoint?: string;
}

/**
 * `GET /api/v1/auth/sessions`. Returns the caller's own browser
 * sessions; the wire shape is current-user-scoped in SQL on the
 * backend and the response carries `current: true` exactly once (the
 * row that authenticated this request).
 *
 * Cookie-bearing GET like {@link getCurrentUser} — `credentials:
 * "include"` so the browser ships the session cookie. The helper
 * does NOT throw, does NOT log, and does NOT echo response detail.
 */
export async function listAuthSessions(
  options: ListAuthSessionsOptions = {},
): Promise<ListAuthSessionsResult> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return { ok: false, error: { kind: "transport" } };
  }
  const endpoint = options.endpoint ?? "/api/v1/auth/sessions";

  let response: Response;
  try {
    response = await fetchImpl(endpoint, authFetchInit("GET", undefined));
  } catch {
    return { ok: false, error: { kind: "transport" } };
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
  const list = parseAuthSessionsListResponse(parsed);
  if (list === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, sessions: list.sessions };
}

export type RevokeAuthSessionResult =
  | { ok: true; current: boolean }
  | { ok: false; error: AuthError };

export interface RevokeAuthSessionOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to a builder that path-encodes
   * the session id into `/api/v1/auth/sessions/:id/revoke`. */
  endpoint?: (sessionId: string) => string;
  /**
   * Whether the targeted row is the caller's CURRENT session. The
   * backend always clears the cookie on a current-session revoke
   * regardless; this flag is just plumbed back to the result so the
   * UI can drive the appropriate sign-out flow.
   */
  current?: boolean;
}

/**
 * `POST /api/v1/auth/sessions/:id/revoke`. Idempotent on the backend:
 * a 204 means "revoked OR already-revoked", and a 404 means the row
 * either does not exist OR belongs to a different user (probe
 * resistance — both cases collapse to the same status). The helper
 * surfaces the typed error for the UI to format via
 * {@link describeAuthError}.
 *
 * The session id is path-encoded via {@link encodeURIComponent} so a
 * pathological id (slashes, percent characters) cannot escape the
 * route.
 */
export async function revokeAuthSession(
  sessionId: string,
  options: RevokeAuthSessionOptions = {},
): Promise<RevokeAuthSessionResult> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return { ok: false, error: { kind: "transport" } };
  }
  const buildEndpoint =
    options.endpoint ??
    ((id: string) =>
      `/api/v1/auth/sessions/${encodeURIComponent(id)}/revoke`);

  let response: Response;
  try {
    response = await fetchImpl(buildEndpoint(sessionId), authFetchInit("POST", undefined));
  } catch {
    return { ok: false, error: { kind: "transport" } };
  }
  if (!response.ok) {
    const { code, message } = await readErrorEnvelope(response);
    return {
      ok: false,
      error: { kind: "http", status: response.status, code, message },
    };
  }
  return { ok: true, current: options.current ?? false };
}

export type RevokeAllAuthSessionsResult =
  | { ok: true; revoked_count: number }
  | { ok: false; error: AuthError };

export interface RevokeAllAuthSessionsOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to
   * `/api/v1/auth/sessions/revoke-all-except-current`. */
  endpoint?: string;
}

/**
 * `POST /api/v1/auth/sessions/revoke-all-except-current`. Revokes every
 * non-revoked session owned by the caller EXCEPT the caller's current
 * session. Returns `revoked_count` — never per-row session ids — so the
 * audit row, the wire response, and the response shape stay aligned.
 *
 * The current cookie is intentionally NOT cleared on the backend: the
 * request itself proves the caller wants to keep the current session.
 */
export async function revokeAllAuthSessionsExceptCurrent(
  options: RevokeAllAuthSessionsOptions = {},
): Promise<RevokeAllAuthSessionsResult> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  if (typeof fetchImpl !== "function") {
    return { ok: false, error: { kind: "transport" } };
  }
  const endpoint =
    options.endpoint ?? "/api/v1/auth/sessions/revoke-all-except-current";

  let response: Response;
  try {
    response = await fetchImpl(endpoint, authFetchInit("POST", undefined));
  } catch {
    return { ok: false, error: { kind: "transport" } };
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
  const result = parseRevokeAllResponse(parsed);
  if (result === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, revoked_count: result.revoked_count };
}

// ---------------------------------------------------------------------
// Current-user password change (`/api/v1/auth/change-password`)
// ---------------------------------------------------------------------

export interface ChangePasswordRequest {
  current_password: string;
  new_password: string;
}

/**
 * Wire shape for a successful change-password response. Mirrors the
 * backend's `ChangePasswordResponse`.
 *
 * Carries the count of OTHER sessions revoked as part of the rotation.
 * Never carries per-row session ids, never carries any token-bearing
 * payload — `revoked_other_sessions` is the only field. The audit row
 * payload mirrors this shape so wire and audit stay byte-aligned.
 */
export interface ChangePasswordResponse {
  revoked_other_sessions: number;
}

/**
 * Build a {@link ChangePasswordResponse} from an unknown wire object.
 * Returns `null` for any missing or wrong-typed required field, or for
 * a non-finite / negative count.
 *
 * Field-by-field construction is the redaction backstop: a stray
 * secret-shaped property on the input cannot reach the returned object
 * because no path here copies it. The count is bounded to a non-
 * negative finite number so a hostile fixture cannot smuggle `Infinity`,
 * `NaN`, or a negative value into the UI.
 */
export function parseChangePasswordResponse(
  raw: unknown,
): ChangePasswordResponse | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.revoked_other_sessions !== "number") return null;
  if (
    !Number.isFinite(r.revoked_other_sessions) ||
    r.revoked_other_sessions < 0
  ) {
    return null;
  }
  return { revoked_other_sessions: r.revoked_other_sessions };
}

export type ChangePasswordResult =
  | { ok: true; response: ChangePasswordResponse }
  | { ok: false; error: AuthError };

export interface ChangePasswordOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/auth/change-password`. */
  endpoint?: string;
}

/**
 * `POST /api/v1/auth/change-password`. The caller must already be
 * authenticated (`credentials: "include"` ships the session cookie).
 * The current cookie stays valid on success — a rotation is NOT a sign-
 * out — but every OTHER session for the caller is revoked server-side.
 *
 * Both passwords are passed straight to the wire body and never echoed
 * into any error / log path. `describeChangePasswordError` is the
 * single formatter that reaches the UI.
 */
export async function changePassword(
  request: ChangePasswordRequest,
  options: ChangePasswordOptions = {},
): Promise<ChangePasswordResult> {
  const result = await authPost<ChangePasswordResponse>({
    endpoint: options.endpoint ?? "/api/v1/auth/change-password",
    fetchImpl: options.fetchImpl,
    body: {
      current_password: request.current_password,
      new_password: request.new_password,
    },
    parse: parseChangePasswordResponse,
  });
  if (!result.ok) return { ok: false, error: result.error };
  return { ok: true, response: result.data };
}

/**
 * Client-side validation reasons for the change-password form. Mirrors
 * the backend's `ChangePasswordRequest::validated` plus a frontend-only
 * confirmation-match check (the backend does not see the confirmation
 * field).
 */
export type ChangePasswordFormError =
  | "missing_current_password"
  | "missing_new_password"
  | "new_password_too_short"
  | "new_password_too_long"
  | "new_password_same_as_current"
  | "confirmation_mismatch";

export interface ChangePasswordFormDraft {
  current_password: string;
  new_password: string;
  new_password_confirmation: string;
}

export function validateChangePasswordForm(
  raw: ChangePasswordFormDraft,
): { ok: true } | { ok: false; reason: ChangePasswordFormError } {
  if (!raw.current_password || raw.current_password.length === 0) {
    return { ok: false, reason: "missing_current_password" };
  }
  if (!raw.new_password || raw.new_password.length === 0) {
    return { ok: false, reason: "missing_new_password" };
  }
  if (raw.new_password.length < PASSWORD_MIN_LEN) {
    return { ok: false, reason: "new_password_too_short" };
  }
  if (raw.new_password.length > PASSWORD_MAX_LEN) {
    return { ok: false, reason: "new_password_too_long" };
  }
  if (raw.new_password === raw.current_password) {
    return { ok: false, reason: "new_password_same_as_current" };
  }
  if (raw.new_password !== raw.new_password_confirmation) {
    return { ok: false, reason: "confirmation_mismatch" };
  }
  return { ok: true };
}

export function describeChangePasswordFormError(
  reason: ChangePasswordFormError,
): string {
  switch (reason) {
    case "missing_current_password":
      return "Enter your current password.";
    case "missing_new_password":
      return "Enter a new password.";
    case "new_password_too_short":
      return `New password must be at least ${PASSWORD_MIN_LEN} characters.`;
    case "new_password_too_long":
      return `New password must be at most ${PASSWORD_MAX_LEN} characters.`;
    case "new_password_same_as_current":
      return "New password must be different from your current password.";
    case "confirmation_mismatch":
      return "New passwords do not match.";
  }
}

/**
 * One-line UI summary for an {@link AuthError} produced by
 * {@link changePassword}. Stays a function of `kind` + `status` only —
 * never echoes the wire `message`, the wire `code`, the offered current
 * or new password, or transport detail.
 *
 * The 401 collapses to a generic "current password is incorrect" string
 * so a probe via this endpoint cannot distinguish "wrong password" from
 * "session expired" beyond the status code itself; the same probe-
 * resistance posture login uses applies here.
 */
export function describeChangePasswordError(err: AuthError): string {
  if (err.kind === "http") {
    if (err.status === 401) {
      return "Current password is incorrect, or your session has ended.";
    }
    if (err.status === 400) {
      return "New password did not meet the password policy.";
    }
    if (err.status === 403) {
      return "Cannot change password: request blocked by browser security policy.";
    }
    return `Cannot change password: HTTP ${err.status}`;
  }
  if (err.kind === "transport") {
    return "Cannot reach the backend.";
  }
  return "Cannot change password: malformed response.";
}

/**
 * One-line UI summary for a successful change-password call. Pure
 * function of the count — does NOT touch error formatting and does NOT
 * echo any request input.
 */
export function describeChangePasswordSuccess(
  response: ChangePasswordResponse,
): string {
  if (response.revoked_other_sessions === 0) {
    return "Password updated.";
  }
  if (response.revoked_other_sessions === 1) {
    return "Password updated. 1 other session was signed out.";
  }
  return `Password updated. ${response.revoked_other_sessions} other sessions were signed out.`;
}

/**
 * One-line UI summary for an {@link AuthError} produced by the session-
 * management helpers. Stays a function of `kind` + `status` only —
 * never echoes the wire `message`, the wire `code`, or transport
 * detail.
 */
export function describeAuthSessionsError(err: AuthError): string {
  if (err.kind === "http") {
    if (err.status === 401) {
      return "Your session has ended. Please sign in again.";
    }
    if (err.status === 403) {
      return "Cannot manage sessions: request blocked by browser security policy.";
    }
    if (err.status === 404) {
      return "That session is no longer available.";
    }
    return `Cannot manage sessions: HTTP ${err.status}`;
  }
  if (err.kind === "transport") {
    return "Cannot reach the backend.";
  }
  return "Cannot manage sessions: malformed response.";
}

/**
 * Human-facing label for an {@link AuthSessionStatus}. Pure function
 * of the closed enum; no wire detail leaks through.
 */
export function describeAuthSessionStatus(status: AuthSessionStatus): string {
  switch (status) {
    case "active":
      return "Active";
    case "expired":
      return "Expired";
    case "revoked":
      return "Revoked";
  }
}

/**
 * Cheap "looks like an email" gate. Mirrors the backend's bounds-only
 * check in `dto/auth.rs::validate_email`. Not a formal RFC-5322 parser
 * — the backend re-validates regardless; this exists so the UI can
 * refuse an obvious typo without a wire round-trip.
 */
function looksLikeEmail(value: string): boolean {
  let atCount = 0;
  for (let i = 0; i < value.length; i++) {
    if (value.charCodeAt(i) === 64) {
      atCount += 1;
      if (atCount > 1) return false;
    }
  }
  if (atCount !== 1) return false;
  if (value.startsWith("@") || value.endsWith("@")) return false;
  return true;
}
