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
  type LoadOptions,
  type LoadResult,
  type WireError,
} from "./apiErrors.js";

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
