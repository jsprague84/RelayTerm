/**
 * Frontend helpers for `/api/v1/audit-events`.
 *
 * Surface today: read-only recent feed for the current user. Mirrors
 * `AuditEventResponse` (`crates/relayterm-api/src/dto/audit_event.rs`).
 *
 * Scope rules (load-bearing):
 *  - Current-user only. There is no admin / cross-user route.
 *  - No raw payload is accepted from the wire. Each known
 *    {@link AuditEventKind} maps onto a closed allow-list of safe public
 *    fields via {@link AuditPayloadSummary}; an unknown shape collapses
 *    the whole row to `null` and the loader treats that as a malformed
 *    response.
 *  - Helpers MUST NOT log raw response bodies. Operator detail belongs
 *    in server logs, not the browser console.
 *  - Sentinel-string tests in `tests/auditApi.test.ts` pin that
 *    `private_key`, `encrypted_private_key`, `client_info`,
 *    `remote_addr`, and `user_agent` cannot survive parsing even when
 *    the wire payload smuggles them.
 */

import {
  fetchJsonList,
  type LoadOptions,
  type LoadResult,
} from "./apiErrors.js";

/**
 * Wire kinds the backend may emit. The set mirrors
 * `relayterm_core::audit_event::AuditEventKind::as_str`. Unknown tags
 * are tolerated — they parse as `string` and surface as a generic UI
 * line — but only the listed ones receive structured copy from
 * {@link summarizeAuditEvent}.
 */
export type AuditEventKindTag = string;

/**
 * Server-profile lifecycle kinds the audit feed currently structures.
 */
export const SERVER_PROFILE_LIFECYCLE_KINDS = [
  "server_profile_created",
  "server_profile_disabled",
  "server_profile_enabled",
] as const;
export type ServerProfileLifecycleKind =
  (typeof SERVER_PROFILE_LIFECYCLE_KINDS)[number];

/**
 * Sanitised payload summary attached to each audit event. The variant
 * is chosen on the backend; this type mirrors the wire shape.
 *
 * `Generic` carries no payload data — the renderer falls back to a
 * "Audit event" line. Adding a new variant means landing a backend
 * sanitizer change first AND adding a renderer arm here.
 */
export type AuditPayloadSummary =
  | {
      kind: "server_profile_lifecycle";
      server_profile_id: string | null;
      name: string | null;
      host_id: string | null;
      ssh_identity_id: string | null;
      /** RFC 3339 timestamp; `null` means the row is currently enabled. */
      disabled_at: string | null;
    }
  | { kind: "generic" };

export interface AuditEvent {
  id: string;
  kind: AuditEventKindTag;
  /** RFC 3339 timestamp. */
  recorded_at: string;
  summary: AuditPayloadSummary;
}

export interface ListRecentAuditEventsOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/audit-events/recent`. */
  endpoint?: string;
  /**
   * Optional client-supplied page size. Backend clamps to `1..=100` and
   * defaults to `20` when omitted. The helper only forwards the value
   * — it does NOT clamp client-side.
   */
  limit?: number;
}

/**
 * GET the current-user audit feed.
 *
 * Returns at most `limit` events (default `20`, hard cap `100` on the
 * backend). Foreign-actor and `actor_id IS NULL` rows are filtered at
 * the SQL layer.
 */
export async function listRecentAuditEvents(
  options: ListRecentAuditEventsOptions = {},
): Promise<LoadResult<AuditEvent[]>> {
  const base = options.endpoint ?? "/api/v1/audit-events/recent";
  const endpoint =
    typeof options.limit === "number"
      ? `${base}?limit=${encodeURIComponent(String(options.limit))}`
      : base;
  return fetchJsonList<AuditEvent>(endpoint, parseAuditEvent, options);
}

/**
 * Parse one audit event off the wire. The function does NOT pass-through
 * unknown payload fields — it constructs each field by name so smuggled
 * `private_key` / `encrypted_private_key` / `client_info` / `remote_addr`
 * / `user_agent` keys cannot survive.
 *
 * Returns `null` if the wire shape is broken (missing required fields,
 * wrong types, malformed summary). The caller treats `null` as
 * `malformed_response`.
 */
export function parseAuditEvent(raw: unknown): AuditEvent | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (
    typeof r.id !== "string" ||
    typeof r.kind !== "string" ||
    typeof r.recorded_at !== "string"
  ) {
    return null;
  }
  const summary = parseAuditPayloadSummary(r.summary);
  if (summary === null) {
    return null;
  }
  return {
    id: r.id,
    kind: r.kind,
    recorded_at: r.recorded_at,
    summary,
  };
}

function parseAuditPayloadSummary(raw: unknown): AuditPayloadSummary | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.kind !== "string") return null;

  if (r.kind === "server_profile_lifecycle") {
    const fields = ["server_profile_id", "name", "host_id", "ssh_identity_id"];
    for (const f of fields) {
      const v = r[f];
      if (v !== null && typeof v !== "string") return null;
    }
    if (r.disabled_at !== null && typeof r.disabled_at !== "string") {
      return null;
    }
    return {
      kind: "server_profile_lifecycle",
      server_profile_id: (r.server_profile_id as string | null) ?? null,
      name: (r.name as string | null) ?? null,
      host_id: (r.host_id as string | null) ?? null,
      ssh_identity_id: (r.ssh_identity_id as string | null) ?? null,
      disabled_at: (r.disabled_at as string | null) ?? null,
    };
  }
  // Generic AND any-future-summary-variant: fall through to a generic
  // shape so a backend that ships a new sanitizer arm before the
  // frontend updates does NOT collapse the whole feed to
  // `malformed_response`. The forward-compatibility cost of accepting
  // an unknown `kind` here is one rendered "Audit event" line; the
  // alternative (rejecting the whole list) is strictly worse for the
  // user. Forbidden field names are already dropped by construction —
  // the only thing we keep off `r` is `kind: "generic"`.
  return { kind: "generic" };
}

/**
 * Human-facing label for an audit event kind. Known kinds get
 * structured copy; unknown ones fall through to a generic label.
 *
 * Stays a pure function of the wire `kind` tag — never echoes raw
 * payload content.
 */
export function describeAuditEventKind(kind: AuditEventKindTag): string {
  switch (kind) {
    case "server_profile_created":
      return "Server profile created";
    case "server_profile_disabled":
      return "Server profile disabled";
    case "server_profile_enabled":
      return "Server profile enabled";
    case "login_succeeded":
      return "Sign-in succeeded";
    case "login_failed":
      return "Sign-in failed";
    case "logout_succeeded":
      return "Signed out";
    case "host_key_accepted":
      return "Host key trusted";
    case "host_key_mismatch":
      return "Host key mismatch";
    case "host_key_revoked":
      return "Host key revoked";
    case "ssh_identity_created":
      return "SSH identity created";
    case "ssh_identity_deleted":
      return "SSH identity deleted";
    case "session_opened":
      return "Terminal session opened";
    case "session_closed":
      return "Terminal session closed";
    default:
      return "Audit event";
  }
}

/**
 * One-line UI summary for an audit event. The output is a function of
 * the structured DTO only — it never pulls anything off the original
 * wire payload.
 *
 * For server-profile lifecycle events the line includes the profile
 * name when present (`name` is also a public, non-sensitive field).
 * Unknown kinds collapse to the generic kind label so the feed keeps
 * rendering when the backend grows a new event type before the
 * frontend catches up.
 */
export function summarizeAuditEvent(event: AuditEvent): string {
  const label = describeAuditEventKind(event.kind);
  if (event.summary.kind === "server_profile_lifecycle") {
    const name = event.summary.name?.trim();
    if (name && name.length > 0) {
      return `${label}: ${name}`;
    }
  }
  return label;
}
