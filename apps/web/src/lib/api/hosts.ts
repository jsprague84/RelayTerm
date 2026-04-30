/**
 * Frontend helper for `GET /api/v1/hosts`.
 *
 * Read-only inventory surface. The DTO mirrors `HostResponse` on the
 * backend (`crates/relayterm-api/src/dto/host.rs`); the parser ignores
 * unknown extra fields so a future safe addition does not break older
 * clients. No write helpers (create/update/delete) are exposed in this
 * slice — production CRUD UI is future work.
 */

import {
  fetchJsonList,
  type LoadOptions,
  type LoadResult,
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
