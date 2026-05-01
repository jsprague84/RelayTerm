/**
 * Frontend helpers for `/api/v1/server-profiles`.
 *
 * Surface today: list (read-only inventory) and create (POST). Mirrors
 * `ServerProfileResponse` and `CreateServerProfileRequest`
 * (`crates/relayterm-api/src/dto/server_profile.rs`). The DTO carries
 * id references to the linked `host` and `ssh_identity` rather than an
 * embedded sub-object — see {@link resolveProfileLinks} for the
 * client-side join helper.
 *
 * Create is a metadata-only write — it links a host, an SSH identity,
 * and an optional username override. It does NOT trust the host key,
 * does NOT verify SSH authentication, and does NOT confirm the public
 * key is installed on the target server. Host-key trust, auth-check,
 * and terminal launch remain future work.
 */

import { MAX_USERNAME_LEN, type Host } from "./hosts.js";
import {
  fetchJsonList,
  postJsonItem,
  readErrorEnvelope,
  type LoadOptions,
  type LoadResult,
  type WireError,
} from "./apiErrors.js";
import type { SshKeyType } from "./sshIdentities.js";

export interface ServerProfile {
  id: string;
  name: string;
  host_id: string;
  ssh_identity_id: string;
  /** When `null`, the host's `default_username` is used. */
  username_override: string | null;
  tags: string[];
  /** RFC 3339 timestamp. */
  created_at: string;
  /** RFC 3339 timestamp. */
  updated_at: string;
  /** RFC 3339 timestamp; absent when the profile has never connected. */
  last_connected_at: string | null;
}

export interface ListServerProfilesOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/server-profiles`. */
  endpoint?: string;
}

export async function listServerProfiles(
  options: ListServerProfilesOptions = {},
): Promise<LoadResult<ServerProfile[]>> {
  const endpoint = options.endpoint ?? "/api/v1/server-profiles";
  return fetchJsonList<ServerProfile>(endpoint, parseServerProfile, options);
}

export function parseServerProfile(raw: unknown): ServerProfile | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.name !== "string" ||
    typeof r.host_id !== "string" ||
    typeof r.ssh_identity_id !== "string" ||
    typeof r.created_at !== "string" ||
    typeof r.updated_at !== "string"
  ) {
    return null;
  }
  if (
    r.username_override !== null &&
    typeof r.username_override !== "string"
  ) {
    return null;
  }
  if (
    r.last_connected_at !== null &&
    typeof r.last_connected_at !== "string"
  ) {
    return null;
  }
  if (!Array.isArray(r.tags) || !r.tags.every((t) => typeof t === "string")) {
    return null;
  }
  return {
    id: r.id,
    name: r.name,
    host_id: r.host_id,
    ssh_identity_id: r.ssh_identity_id,
    username_override: r.username_override,
    tags: r.tags as string[],
    created_at: r.created_at,
    updated_at: r.updated_at,
    last_connected_at: r.last_connected_at,
  };
}

export interface ProfileLinks {
  /** Resolved host, or `null` if the linked host_id is not in the
   * supplied list (deleted, foreign-owned, or out of scope). */
  host: Host | null;
  /** The username actually used when connecting: the override if
   * present, otherwise the host's `default_username`. `null` only when
   * the host link could not be resolved AND there is no override. */
  effectiveUsername: string | null;
  /** True when {@link effectiveUsername} came from the host's default
   * (i.e. the profile has no `username_override`). False when the
   * override was used; null when neither was resolvable. */
  inheritedFromHost: boolean | null;
}

/**
 * Length and value bounds mirroring the backend validators in
 * `crates/relayterm-core/src/validation.rs`. Kept in sync by hand —
 * drift would still be caught server-side as `400 invalid_input`.
 */
export const MAX_PROFILE_NAME_LEN = 64;
export const MAX_TAG_LEN = 32;
export const MAX_TAGS = 32;

/**
 * Request body for `POST /api/v1/server-profiles`. Mirrors the backend's
 * `CreateServerProfileRequest`.
 */
export interface CreateServerProfileRequest {
  name: string;
  host_id: string;
  ssh_identity_id: string;
  /** Optional. When omitted the host's `default_username` is used. */
  username_override?: string | null;
  /** Optional. Empty array is normal; the backend defaults to `[]`. */
  tags?: string[];
}

export type CreateServerProfileInvalidReason =
  | "missing_name"
  | "name_has_surrounding_whitespace"
  | "name_too_long"
  | "name_has_control_chars"
  | "missing_host_id"
  | "missing_ssh_identity_id"
  | "username_override_too_long"
  | "username_override_bad_leading_char"
  | "username_override_has_invalid_char"
  | "tag_empty"
  | "tag_too_long"
  | "tag_has_invalid_char"
  | "tag_duplicate"
  | "too_many_tags";

export type CreateServerProfileValidation =
  | {
      ok: true;
      body: {
        name: string;
        host_id: string;
        ssh_identity_id: string;
        username_override: string | null;
        tags: string[];
      };
    }
  | { ok: false; reason: CreateServerProfileInvalidReason };

const TAG_ALLOWED = /^[A-Za-z0-9_\-]+$/;
const USERNAME_TAIL_ALLOWED = /^[A-Za-z0-9_.\-]*$/;
// eslint-disable-next-line no-control-regex
const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F]/;

/**
 * Parse a comma-separated tag input string into a normalized array.
 *
 * Whitespace around each token is trimmed. Empty tokens (e.g. trailing
 * commas, double commas) are dropped silently — this is a UX convenience,
 * not a validation step. The returned array is then handed to
 * {@link validateCreateServerProfileRequest}, which is authoritative
 * for shape and uniqueness rules.
 */
export function parseTagsInput(input: string): string[] {
  if (input.length === 0) return [];
  return input
    .split(",")
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

/**
 * Validate a create-server-profile request on the client. Mirrors the
 * backend's field-level rules in `crates/relayterm-core/src/validation.rs`.
 * The backend remains authoritative.
 *
 * Note: this validator does NOT check that `host_id` / `ssh_identity_id`
 * actually exist in the user's inventory — that is enforced server-side
 * (404 on a missing reference). The UI's "no host" / "no identity"
 * checks live alongside the form rendering, see
 * {@link canSubmitServerProfile}.
 */
export function validateCreateServerProfileRequest(
  raw: CreateServerProfileRequest,
): CreateServerProfileValidation {
  const name = raw.name ?? "";
  if (name.length === 0) {
    return { ok: false, reason: "missing_name" };
  }
  if (name.trim() !== name) {
    return { ok: false, reason: "name_has_surrounding_whitespace" };
  }
  if ([...name].length > MAX_PROFILE_NAME_LEN) {
    return { ok: false, reason: "name_too_long" };
  }
  if (CONTROL_CHARS.test(name)) {
    return { ok: false, reason: "name_has_control_chars" };
  }

  const host_id = raw.host_id ?? "";
  if (host_id.length === 0) {
    return { ok: false, reason: "missing_host_id" };
  }
  const ssh_identity_id = raw.ssh_identity_id ?? "";
  if (ssh_identity_id.length === 0) {
    return { ok: false, reason: "missing_ssh_identity_id" };
  }

  const overrideRaw = raw.username_override;
  let username_override: string | null = null;
  if (overrideRaw !== undefined && overrideRaw !== null && overrideRaw !== "") {
    if (overrideRaw.length > MAX_USERNAME_LEN) {
      return { ok: false, reason: "username_override_too_long" };
    }
    const first = overrideRaw.charCodeAt(0);
    const isLetter =
      (first >= 65 && first <= 90) || (first >= 97 && first <= 122);
    const isUnderscore = first === 95;
    if (!isLetter && !isUnderscore) {
      return { ok: false, reason: "username_override_bad_leading_char" };
    }
    if (!USERNAME_TAIL_ALLOWED.test(overrideRaw.slice(1))) {
      return { ok: false, reason: "username_override_has_invalid_char" };
    }
    username_override = overrideRaw;
  }

  const rawTags = raw.tags ?? [];
  if (rawTags.length > MAX_TAGS) {
    return { ok: false, reason: "too_many_tags" };
  }
  const tags: string[] = [];
  for (const tag of rawTags) {
    if (tag.length === 0) {
      return { ok: false, reason: "tag_empty" };
    }
    if (tag.length > MAX_TAG_LEN) {
      return { ok: false, reason: "tag_too_long" };
    }
    if (!TAG_ALLOWED.test(tag)) {
      return { ok: false, reason: "tag_has_invalid_char" };
    }
    if (tags.includes(tag)) {
      return { ok: false, reason: "tag_duplicate" };
    }
    tags.push(tag);
  }

  return {
    ok: true,
    body: { name, host_id, ssh_identity_id, username_override, tags },
  };
}

/**
 * Whether a server-profile create form should be enabled. The UI must
 * not ship a request that the backend would 404 — both a host AND an
 * SSH identity must already exist in the caller's inventory before a
 * profile can be created. Returning a typed reason lets the UI render
 * a precise, honest empty state.
 */
export type ServerProfileCreatability =
  | { kind: "ok" }
  | { kind: "no_hosts" }
  | { kind: "no_identities" }
  | { kind: "no_hosts_or_identities" };

export function canSubmitServerProfile(
  hostCount: number,
  identityCount: number,
): ServerProfileCreatability {
  if (hostCount === 0 && identityCount === 0) {
    return { kind: "no_hosts_or_identities" };
  }
  if (hostCount === 0) return { kind: "no_hosts" };
  if (identityCount === 0) return { kind: "no_identities" };
  return { kind: "ok" };
}

export type CreateServerProfileError =
  | { kind: "validation"; reason: CreateServerProfileInvalidReason }
  | WireError;

/**
 * Format a {@link CreateServerProfileError} as a one-line UI summary.
 *
 * Stays a function of `kind` + `status` + `code` (and the validation
 * `reason` enum) only — never echoes the wire `message` of an HTTP
 * error or the thrown `Error.message` of a transport failure.
 */
export function describeCreateServerProfileError(
  err: CreateServerProfileError,
): string {
  switch (err.kind) {
    case "validation":
      return `Cannot create server profile: ${describeProfileValidationReason(err.reason)}`;
    case "http":
      // 404 here means the host or ssh_identity referenced in the
      // request body could not be resolved for the caller — the UI
      // surfaces this as a stale-reference hint.
      if (err.status === 404 && err.code === "not_found") {
        return "Failed to create server profile: linked host or SSH identity not found";
      }
      return `Failed to create server profile: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Failed to create server profile: transport error";
    case "malformed_response":
      return "Failed to create server profile: malformed response";
  }
}

function describeProfileValidationReason(
  reason: CreateServerProfileInvalidReason,
): string {
  switch (reason) {
    case "missing_name":
      return "name is required";
    case "name_has_surrounding_whitespace":
      return "name must not start or end with whitespace";
    case "name_too_long":
      return `name must be at most ${MAX_PROFILE_NAME_LEN} characters`;
    case "name_has_control_chars":
      return "name must not contain control characters";
    case "missing_host_id":
      return "a host must be selected";
    case "missing_ssh_identity_id":
      return "an SSH identity must be selected";
    case "username_override_too_long":
      return `username override must be at most ${MAX_USERNAME_LEN} characters`;
    case "username_override_bad_leading_char":
      return "username override must start with a letter or '_'";
    case "username_override_has_invalid_char":
      return "username override may only contain letters, digits, '-', '_', '.'";
    case "tag_empty":
      return "tags must not be empty";
    case "tag_too_long":
      return `tags must be at most ${MAX_TAG_LEN} characters`;
    case "tag_has_invalid_char":
      return "tags may only contain letters, digits, '-', '_'";
    case "tag_duplicate":
      return "tags must be unique";
    case "too_many_tags":
      return `at most ${MAX_TAGS} tags are allowed`;
  }
}

export interface CreateServerProfileOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/server-profiles`. */
  endpoint?: string;
}

export type CreateServerProfileResult =
  | { ok: true; profile: ServerProfile }
  | { ok: false; error: CreateServerProfileError };

/**
 * POST a create-server-profile request and parse the response.
 *
 * On a 2xx, the response is parsed by {@link parseServerProfile}. The
 * function does not throw, does not log raw response bodies, and does
 * not echo wire / transport detail through the formatter.
 *
 * Backend semantics this helper does NOT change:
 *  - Profile creation does not trust the host key, does not verify SSH
 *    authentication, and does not confirm the public key is installed.
 */
export async function createServerProfile(
  raw: CreateServerProfileRequest,
  options: CreateServerProfileOptions = {},
): Promise<CreateServerProfileResult> {
  const validation = validateCreateServerProfileRequest(raw);
  if (!validation.ok) {
    return {
      ok: false,
      error: { kind: "validation", reason: validation.reason },
    };
  }
  // The backend accepts `null` only for an absent override but treats
  // omitted as the same. We send the validated normalized form so the
  // wire body is explicit.
  const wireBody: Record<string, unknown> = {
    name: validation.body.name,
    host_id: validation.body.host_id,
    ssh_identity_id: validation.body.ssh_identity_id,
    tags: validation.body.tags,
  };
  if (validation.body.username_override !== null) {
    wireBody.username_override = validation.body.username_override;
  }
  const endpoint = options.endpoint ?? "/api/v1/server-profiles";
  const result = await postJsonItem<ServerProfile>(
    endpoint,
    wireBody,
    parseServerProfile,
    options,
  );
  if (!result.ok) {
    return { ok: false, error: result.error };
  }
  return { ok: true, profile: result.data };
}

/**
 * Resolve a profile's linked host and effective username from the
 * already-fetched hosts list.
 *
 * The backend's read endpoints return ids only — the frontend joins
 * them client-side. A missing host (`null`) is rendered honestly in
 * the UI; the helper does NOT synthesize a placeholder host or pretend
 * the link is valid.
 */
export function resolveProfileLinks(
  profile: ServerProfile,
  hosts: readonly Host[],
): ProfileLinks {
  const host = hosts.find((h) => h.id === profile.host_id) ?? null;
  if (profile.username_override !== null) {
    return {
      host,
      effectiveUsername: profile.username_override,
      inheritedFromHost: false,
    };
  }
  if (host !== null) {
    return {
      host,
      effectiveUsername: host.default_username,
      inheritedFromHost: true,
    };
  }
  return { host: null, effectiveUsername: null, inheritedFromHost: null };
}

// ---------------------------------------------------------------------------
// Host-key preflight + trust helpers
// ---------------------------------------------------------------------------
//
// Wire shapes mirror `crates/relayterm-api/src/dto/preflight.rs`. The wire
// `host_key_status` is one of `unknown | trusted | changed`; `revoked` is
// NOT a wire status today — a previously-revoked fingerprint that the
// server presents fresh surfaces as `unknown`, and the trust route
// rejects it with a `409 conflict { entity: "host_key" }`. The UI layer
// therefore models `revoked` ONLY as a derived trust-rejection reason,
// never as a parsed-status value.
//
// Redaction posture: the responses parsed here carry only public host-side
// data (fingerprint, key type, hostname/port). No private-key field is
// declared on either DTO. The error formatter is a function of `kind` +
// `status` + `code` only — operator detail and transport `Error.message`
// never reach the UI.

/**
 * Wire-stable host-key status returned by the preflight route.
 *
 * - `unknown` — no active pinned entry matches the captured key. First
 *   time seen, or the prior pin was revoked.
 * - `trusted` — an active, non-revoked pinned entry matches.
 * - `changed` — an active pinned entry exists but the captured key
 *   differs. The trust route refuses to silently overwrite.
 */
export type HostKeyStatus = "unknown" | "trusted" | "changed";

const HOST_KEY_STATUSES: ReadonlySet<HostKeyStatus> = new Set([
  "unknown",
  "trusted",
  "changed",
]);

/**
 * Parsed shape of `POST /api/v1/server-profiles/:id/host-key-preflight`.
 *
 * Carries ONLY public host-side data — no private-key field is declared
 * here. The parser builds the DTO field-by-field, so any stray
 * `private_key` / `encrypted_private_key` smuggled onto the wire body
 * cannot reach the returned object. See the redaction-sentinel tests in
 * `tests/hostKeyApi.test.ts`.
 */
export interface HostKeyPreflightResponse {
  profile_id: string;
  host_id: string;
  hostname: string;
  port: number;
  host_key_status: HostKeyStatus;
  host_key_type: SshKeyType;
  /** `SHA256:<base64>` form. Public-ish security metadata; safe to
   * display deliberately. */
  host_key_fingerprint: string;
  /** Static, server-supplied human-facing message keyed to the status.
   * The backend wires this from a fixed string per status — no operator
   * detail is interpolated. The UI is free to use it but does not
   * depend on its exact wording. */
  message: string;
}

export function parseHostKeyPreflightResponse(
  raw: unknown,
): HostKeyPreflightResponse | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.profile_id !== "string" ||
    typeof r.host_id !== "string" ||
    typeof r.hostname !== "string" ||
    typeof r.port !== "number" ||
    typeof r.host_key_status !== "string" ||
    typeof r.host_key_type !== "string" ||
    typeof r.host_key_fingerprint !== "string" ||
    typeof r.message !== "string"
  ) {
    return null;
  }
  if (!HOST_KEY_STATUSES.has(r.host_key_status as HostKeyStatus)) {
    return null;
  }
  // Construct field-by-field. A stray `encrypted_private_key` /
  // `private_key` on `r` cannot reach the returned object because no
  // path here copies it.
  return {
    profile_id: r.profile_id,
    host_id: r.host_id,
    hostname: r.hostname,
    port: r.port,
    host_key_status: r.host_key_status as HostKeyStatus,
    host_key_type: r.host_key_type as SshKeyType,
    host_key_fingerprint: r.host_key_fingerprint,
    message: r.message,
  };
}

/**
 * Parsed shape of `POST /api/v1/server-profiles/:id/trust-host-key`.
 *
 * Same redaction posture as the preflight response — only public
 * host-side data is declared. `trusted_at` is an RFC 3339 timestamp.
 */
export interface TrustHostKeyResponse {
  known_host_entry_id: string;
  host_id: string;
  host_key_type: SshKeyType;
  host_key_fingerprint: string;
  trusted_at: string;
}

export function parseTrustHostKeyResponse(
  raw: unknown,
): TrustHostKeyResponse | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.known_host_entry_id !== "string" ||
    typeof r.host_id !== "string" ||
    typeof r.host_key_type !== "string" ||
    typeof r.host_key_fingerprint !== "string" ||
    typeof r.trusted_at !== "string"
  ) {
    return null;
  }
  return {
    known_host_entry_id: r.known_host_entry_id,
    host_id: r.host_id,
    host_key_type: r.host_key_type as SshKeyType,
    host_key_fingerprint: r.host_key_fingerprint,
    trusted_at: r.trusted_at,
  };
}

/**
 * Sanity-check the fingerprint shape on the client before posting it
 * back to the trust endpoint. Mirrors the backend's
 * `validated_expected_fingerprint` (`crates/relayterm-api/src/dto/preflight.rs`):
 * must start with `SHA256:`, length 8..=128, no whitespace or control
 * characters. Backend remains authoritative.
 */
export function isValidFingerprintShape(fp: string): boolean {
  if (!fp.startsWith("SHA256:")) return false;
  if (fp.length < 8 || fp.length > 128) return false;
  for (let i = 0; i < fp.length; i++) {
    const code = fp.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) return false;
    // Whitespace: space, tab, newline, carriage return, form feed,
    // vertical tab.
    if (
      code === 0x20 ||
      code === 0x09 ||
      code === 0x0a ||
      code === 0x0d ||
      code === 0x0b ||
      code === 0x0c
    ) {
      return false;
    }
  }
  return true;
}

export interface HostKeyPreflightOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to
   * `/api/v1/server-profiles/:id/host-key-preflight`. */
  endpoint?: string;
}

export type PreflightError = WireError;

export type HostKeyPreflightResult =
  | { ok: true; preflight: HostKeyPreflightResponse }
  | { ok: false; error: PreflightError };

/**
 * POST a host-key preflight request and parse the typed response.
 *
 * The route runs an SSH KEX-only probe and disconnects WITHOUT
 * authenticating. This helper is a thin transport — it does not throw,
 * does not log raw response bodies, and does not echo wire / transport
 * detail through any user-facing string.
 */
export async function hostKeyPreflight(
  profileId: string,
  options: HostKeyPreflightOptions = {},
): Promise<HostKeyPreflightResult> {
  const endpoint =
    options.endpoint ??
    `/api/v1/server-profiles/${encodeURIComponent(profileId)}/host-key-preflight`;
  const result = await postJsonItem<HostKeyPreflightResponse>(
    endpoint,
    {},
    parseHostKeyPreflightResponse,
    options,
  );
  if (!result.ok) return { ok: false, error: result.error };
  return { ok: true, preflight: result.data };
}

/**
 * Format a host-key preflight error as a one-line UI summary. Stays a
 * function of `kind` + `status` + `code` ONLY — never echoes the wire
 * `message` of an HTTP error or the thrown `Error.message` of a
 * transport failure.
 *
 * Per-status copy mirrors the backend's failure shapes:
 *  - `502 bad_gateway` — the SSH probe itself failed (unreachable,
 *    timeout, transport, unsupported algorithm). Wire body is the static
 *    `"bad gateway"` string per the ApiError contract.
 *  - `503 service_unavailable` — the vault is disabled.
 *  - `404 not_found` — the profile is missing or foreign-owned.
 *  - `401 unauthorized` — dev-auth disabled.
 */
export function describePreflightError(err: PreflightError): string {
  switch (err.kind) {
    case "http":
      if (err.status === 502 && err.code === "bad_gateway") {
        return "Host-key preflight failed: could not reach the server (network, timeout, or unsupported host-key algorithm)";
      }
      if (err.status === 503 && err.code === "service_unavailable") {
        return "Host-key preflight failed: backend vault is not configured";
      }
      if (err.status === 404 && err.code === "not_found") {
        return "Host-key preflight failed: server profile not found";
      }
      if (err.status === 401) {
        return "Host-key preflight failed: not authenticated";
      }
      return `Host-key preflight failed: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Host-key preflight failed: transport error";
    case "malformed_response":
      return "Host-key preflight failed: malformed response";
  }
}

export interface TrustHostKeyOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to
   * `/api/v1/server-profiles/:id/trust-host-key`. */
  endpoint?: string;
}

export type TrustHostKeyError =
  | { kind: "validation"; reason: "invalid_fingerprint_shape" }
  | { kind: "http"; status: number; code: string; message: string }
  | { kind: "transport"; message: string }
  | { kind: "malformed_response" };

export type TrustHostKeyResult =
  | { ok: true; trust: TrustHostKeyResponse }
  | { ok: false; error: TrustHostKeyError };

/**
 * POST a trust-host-key request with the caller's expected fingerprint.
 *
 * The backend re-probes, refuses to trust if the captured key changed
 * or differs from `expected_fingerprint`, and refuses if a revoked row
 * exists for the captured `(key_type, fingerprint)`. This helper does
 * NOT auto-trust, does NOT throw, does NOT log raw response bodies, and
 * does NOT echo wire / transport detail through the formatter.
 *
 * The local validator rejects an obviously malformed `expected_fingerprint`
 * before any wire round-trip; the backend remains authoritative.
 */
export async function trustHostKey(
  profileId: string,
  expectedFingerprint: string,
  options: TrustHostKeyOptions = {},
): Promise<TrustHostKeyResult> {
  if (!isValidFingerprintShape(expectedFingerprint)) {
    return {
      ok: false,
      error: { kind: "validation", reason: "invalid_fingerprint_shape" },
    };
  }
  const endpoint =
    options.endpoint ??
    `/api/v1/server-profiles/${encodeURIComponent(profileId)}/trust-host-key`;
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
      body: JSON.stringify({ expected_fingerprint: expectedFingerprint }),
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
  const parsed = parseTrustHostKeyResponse(body);
  if (parsed === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, trust: parsed };
}

/**
 * Format a trust-host-key error as a one-line UI summary. Same
 * redaction posture as {@link describePreflightError}: a function of
 * `kind` + `status` + `code` ONLY.
 *
 * `409 conflict { entity: "host_key" }` collapses to a single deliberately
 * conservative message — the backend uses the same status for "captured
 * fingerprint changed", "expected_fingerprint stale", and "revoked
 * fingerprint reappeared". The UI cannot distinguish them from the
 * wire body, so the formatter names what is true in all three cases:
 * trust was refused; do not retry without re-running preflight.
 */
export function describeTrustHostKeyError(err: TrustHostKeyError): string {
  switch (err.kind) {
    case "validation":
      return "Cannot trust host key: fingerprint shape is invalid";
    case "http":
      if (err.status === 409) {
        return "Trust refused: the host key changed, was revoked, or no longer matches the fingerprint shown — re-run preflight before trying again";
      }
      if (err.status === 400 && err.code === "invalid_input") {
        return "Trust refused: backend rejected the fingerprint shape";
      }
      if (err.status === 502 && err.code === "bad_gateway") {
        return "Trust refused: could not re-probe the server (network, timeout, or unsupported host-key algorithm)";
      }
      if (err.status === 503 && err.code === "service_unavailable") {
        return "Trust refused: backend vault is not configured";
      }
      if (err.status === 404 && err.code === "not_found") {
        return "Trust refused: server profile not found";
      }
      if (err.status === 401) {
        return "Trust refused: not authenticated";
      }
      return `Trust refused: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Trust refused: transport error";
    case "malformed_response":
      return "Trust refused: malformed response";
  }
}

// ---------------------------------------------------------------------------
// SSH auth-check helpers
// ---------------------------------------------------------------------------
//
// Wire shape mirrors `crates/relayterm-api/src/dto/auth_check.rs`. The route
// returns `200 OK` with a typed `status` enum for diagnostic outcomes
// (auth failure, host-key mismatch, connection failure are NOT HTTP errors
// — they are operator-facing diagnostic answers). HTTP errors are reserved
// for "the request couldn't be processed" cases (missing profile, vault
// disabled, internal data-integrity bug, concurrency cap saturated).
//
// Redaction posture: the wire response carries ONLY public diagnostic
// fields — no host key, fingerprint, peer banner, decrypted PEM, encrypted
// blob, or russh error text. The DTO declared here mirrors that surface
// 1:1; the parser builds it field-by-field so a stray `private_key` /
// `encrypted_private_key` smuggled onto the wire body cannot reach the
// returned object. The error formatter is a function of `kind` + `status`
// + `code` only — wire `message` and transport `Error.message` never
// reach the UI.

/**
 * Wire-stable auth-check status returned by `POST /:id/auth-check`.
 *
 * - `authentication_succeeded` — host key matched a trusted pin AND
 *   public-key authentication succeeded for the configured username.
 *   The auth-check route did NOT open a PTY, run a shell, or execute a
 *   command. Terminal launch remains a separate, deliberate action.
 * - `authentication_failed` — host key matched a trusted pin, but the
 *   server rejected the configured identity for the configured username
 *   (wrong key, wrong user, or `authorized_keys` not yet in place).
 * - `host_key_unknown` — no active, trusted, non-revoked pin matches
 *   the captured host key. Auth was NOT attempted. Trust the host key
 *   first via the host-key panel.
 * - `host_key_changed` — an active, non-revoked pin exists for the
 *   same key type with a DIFFERENT fingerprint. Auth was NOT attempted.
 *   Investigate before continuing — server reinstallation, key rotation,
 *   or man-in-the-middle are all possible.
 * - `connection_failed` — the SSH transport failed before authentication
 *   could complete (TCP refused, timeout, malformed peer, outer auth-
 *   check timeout). Auth was NOT attempted.
 */
export type AuthCheckStatus =
  | "authentication_succeeded"
  | "authentication_failed"
  | "host_key_unknown"
  | "host_key_changed"
  | "connection_failed";

const AUTH_CHECK_STATUSES: ReadonlySet<AuthCheckStatus> = new Set([
  "authentication_succeeded",
  "authentication_failed",
  "host_key_unknown",
  "host_key_changed",
  "connection_failed",
]);

/**
 * Parsed shape of `POST /api/v1/server-profiles/:id/auth-check`.
 *
 * Carries ONLY public diagnostic fields. No private-key field is declared
 * here — the parser builds the DTO field-by-field, so any stray
 * `private_key` / `encrypted_private_key` smuggled onto the wire body
 * cannot reach the returned object. See the redaction-sentinel tests in
 * `tests/authCheckApi.test.ts`.
 */
export interface AuthCheckResponse {
  profile_id: string;
  host_id: string;
  ssh_identity_id: string;
  status: AuthCheckStatus;
  /** Static, server-supplied human-facing message keyed off `status`.
   * The backend wires this from a fixed string per status — no operator
   * detail is interpolated. The UI is free to use it but does not depend
   * on its exact wording; the local `authCheckStatusDescription` helper
   * (in `lib/app/authCheckState.ts`) is the single source of truth for
   * rendered status copy. */
  message: string;
  /** RFC 3339 timestamp. */
  checked_at: string;
}

export function parseAuthCheckResponse(
  raw: unknown,
): AuthCheckResponse | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.profile_id !== "string" ||
    typeof r.host_id !== "string" ||
    typeof r.ssh_identity_id !== "string" ||
    typeof r.status !== "string" ||
    typeof r.message !== "string" ||
    typeof r.checked_at !== "string"
  ) {
    return null;
  }
  if (!AUTH_CHECK_STATUSES.has(r.status as AuthCheckStatus)) {
    return null;
  }
  // Construct field-by-field. A stray `encrypted_private_key` /
  // `private_key` on `r` cannot reach the returned object because no
  // path here copies it.
  return {
    profile_id: r.profile_id,
    host_id: r.host_id,
    ssh_identity_id: r.ssh_identity_id,
    status: r.status as AuthCheckStatus,
    message: r.message,
    checked_at: r.checked_at,
  };
}

export interface AuthCheckOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to
   * `/api/v1/server-profiles/:id/auth-check`. */
  endpoint?: string;
}

export type AuthCheckError = WireError;

export type AuthCheckResult =
  | { ok: true; check: AuthCheckResponse }
  | { ok: false; error: AuthCheckError };

/**
 * POST an auth-check request and parse the typed response.
 *
 * The route attempts SSH public-key authentication WITHOUT requesting a
 * PTY, opening a channel, or executing a command. Auth failure, host-key
 * mismatch, and connection failure are returned as 200-OK typed `status`
 * outcomes; only "request couldn't be processed" cases (missing profile,
 * vault disabled, vault-row corrupt, semaphore saturated, dev-auth
 * disabled) reach the {@link AuthCheckError} envelope.
 *
 * The helper does NOT throw, does NOT log raw response bodies, and does
 * NOT echo wire / transport detail through any user-facing string.
 */
export async function authCheckServerProfile(
  profileId: string,
  options: AuthCheckOptions = {},
): Promise<AuthCheckResult> {
  const endpoint =
    options.endpoint ??
    `/api/v1/server-profiles/${encodeURIComponent(profileId)}/auth-check`;
  const result = await postJsonItem<AuthCheckResponse>(
    endpoint,
    {},
    parseAuthCheckResponse,
    options,
  );
  if (!result.ok) return { ok: false, error: result.error };
  return { ok: true, check: result.data };
}

/**
 * Format an auth-check error as a one-line UI summary. Stays a function
 * of `kind` + `status` + `code` ONLY — never echoes the wire `message`
 * of an HTTP error or the thrown `Error.message` of a transport failure.
 *
 * Per-status copy mirrors the backend's failure shapes:
 *  - `503 service_unavailable` — vault disabled OR auth-check concurrency
 *    cap saturated. The wire body is the static `service unavailable`
 *    string in either case; the UI cannot distinguish them.
 *  - `500 internal_error` — vault row decrypted to a malformed PEM. Data-
 *    integrity bug; operator-facing copy is generic.
 *  - `404 not_found` — profile is missing or foreign-owned.
 *  - `401 unauthorized` — dev-auth disabled.
 */
export function describeAuthCheckError(err: AuthCheckError): string {
  switch (err.kind) {
    case "http":
      if (err.status === 503 && err.code === "service_unavailable") {
        return "Auth-check unavailable: backend vault is not configured or the auth-check concurrency cap is saturated — try again shortly";
      }
      if (err.status === 500 && err.code === "internal_error") {
        return "Auth-check failed: backend could not decrypt the SSH identity (vault data-integrity issue)";
      }
      if (err.status === 404 && err.code === "not_found") {
        return "Auth-check failed: server profile not found";
      }
      if (err.status === 401) {
        return "Auth-check failed: not authenticated";
      }
      return `Auth-check failed: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Auth-check failed: transport error";
    case "malformed_response":
      return "Auth-check failed: malformed response";
  }
}
