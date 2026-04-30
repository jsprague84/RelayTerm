/**
 * Frontend helper for `GET /api/v1/server-profiles`.
 *
 * Read-only inventory surface. Mirrors `ServerProfileResponse`
 * (`crates/relayterm-api/src/dto/server_profile.rs`). The DTO carries
 * id references to the linked `host` and `ssh_identity` rather than an
 * embedded sub-object — see {@link resolveProfileLinks} for the
 * client-side join helper.
 */

import type { Host } from "./hosts.js";
import {
  fetchJsonList,
  type LoadOptions,
  type LoadResult,
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
