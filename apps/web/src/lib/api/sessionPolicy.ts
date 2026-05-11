/**
 * Frontend helper for `GET /api/v1/config/session-policy`.
 *
 * Surfaces the deployment's effective detached-live-PTY TTL so the
 * production UI can stop hardcoding the legacy `~30s` literal. The wire
 * shape is intentionally one numeric field; the persistence disclaimer
 * (in-memory replay, no backend-restart survival) lives in the UI copy
 * that consumes the formatter, not on the wire.
 *
 * Scope contracts re-asserted here:
 *  - {@link parseSessionPolicy} builds the DTO field-by-field. A stray
 *    secret-shaped sibling on the wire (cookie material, vault keys,
 *    database urls, env names) cannot reach the parsed object because
 *    no path here copies it. The companion sentinel test in
 *    `tests/sessionPolicy.test.ts` pins this against future
 *    "be helpful and pass through extras" regressions.
 *  - {@link describeDetachedTtl} stays a pure function of the configured
 *    seconds. It never echoes wire / transport detail.
 *  - {@link loadSessionPolicy} falls back to
 *    {@link DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS} on every failure
 *    path so a view that depends on it cannot be blocked by transport
 *    blips, malformed responses, or a not-yet-deployed backend.
 */

import type { LoadOptions, WireError } from "./apiErrors.js";
import { readErrorEnvelope } from "./apiErrors.js";

/**
 * Fallback TTL (seconds) used when the policy fetch has not yet resolved
 * or failed. Matches the backend default (`relayterm_terminal::
 * DETACHED_LIVE_PTY_TTL = 30 s`). When the wire-observed value lands,
 * the consuming view overwrites this with the real one — but the UI
 * never sits blocked on the fetch.
 */
export const DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS = 30;

/**
 * Fallback per-user live-PTY ceiling (Phase 1B.1 quota) used when the
 * policy fetch has not yet resolved or failed. Matches the backend
 * default (`relayterm_terminal::DEFAULT_MAX_LIVE_PTY_PER_USER = 8`).
 * Surfaced through {@link describeMaxLivePtyPerUser} so quota-refusal
 * copy can render with a defensible parameterised number even when
 * the SPA boots before the wire round-trip resolves.
 */
export const DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER = 8;

/**
 * Wire-side range bounds for the parsed TTL. Mirrors the backend's
 * `5..=86_400` config-validator bound exactly — a value outside this
 * range cannot have been emitted by a current backend, so we treat it
 * as a malformed wire body and fall back to the default rather than
 * trusting a hostile payload.
 */
const POLICY_MIN_TTL_SECONDS = 5;
const POLICY_MAX_TTL_SECONDS = 24 * 60 * 60;

/**
 * Wire-side range bounds for the parsed per-user live-PTY ceiling.
 * Mirrors the backend's `1..=256` config-validator bound (Phase 1B.1).
 * Same rationale as the TTL bounds above: a value outside this range
 * cannot have been emitted by a current backend, so we collapse it to
 * `malformed_response` rather than trusting a hostile payload.
 */
const POLICY_MIN_MAX_LIVE_PTY_PER_USER = 1;
const POLICY_MAX_MAX_LIVE_PTY_PER_USER = 256;

/**
 * Parsed, typed session policy. Carries only the fields the SPA renders;
 * future fields require an explicit migration here AND on the backend.
 */
export interface SessionPolicy {
  /** Effective detached-live-PTY TTL window in seconds. */
  detached_live_pty_ttl_seconds: number;
  /**
   * Effective per-user live-PTY ceiling (Phase 1B.1 quota). The SPA
   * uses this to render parameterised copy on a `429 too_many_sessions`
   * refusal ("you're at the limit of N sessions"). NOT a probe for the
   * caller's current count — the count never crosses the wire.
   */
  max_live_pty_sessions_per_user: number;
}

/**
 * Parse a wire-side session-policy body field-by-field.
 *
 * Returns `null` when the wire shape is wrong (missing / non-integer /
 * out-of-range), which the caller collapses to `malformed_response`.
 *
 * Strict field-by-field copy: a hostile or accidentally-widened wire
 * body that smuggles secret-shaped fields (cookie material, vault
 * keys, environment names, database paths) cannot reach the returned
 * object because the function never spreads, assigns, or reflects.
 * Range-clamping does NOT happen here — an out-of-range value is a
 * wire bug worth surfacing as `malformed_response`, not silently
 * masking.
 */
export function parseSessionPolicy(raw: unknown): SessionPolicy | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const ttl = r.detached_live_pty_ttl_seconds;
  if (typeof ttl !== "number" || !Number.isInteger(ttl)) return null;
  if (ttl < POLICY_MIN_TTL_SECONDS || ttl > POLICY_MAX_TTL_SECONDS) return null;
  const cap = r.max_live_pty_sessions_per_user;
  if (typeof cap !== "number" || !Number.isInteger(cap)) return null;
  if (
    cap < POLICY_MIN_MAX_LIVE_PTY_PER_USER ||
    cap > POLICY_MAX_MAX_LIVE_PTY_PER_USER
  ) {
    return null;
  }
  return {
    detached_live_pty_ttl_seconds: ttl,
    max_live_pty_sessions_per_user: cap,
  };
}

export type SessionPolicyError = WireError;

export type SessionPolicyResponse =
  | { ok: true; policy: SessionPolicy }
  | { ok: false; error: SessionPolicyError };

export interface FetchSessionPolicyOptions extends LoadOptions {
  /** Replaceable for tests. Defaults to `/api/v1/config/session-policy`. */
  endpoint?: string;
}

/**
 * GET the configured session policy.
 *
 * Returns `{ ok: true, policy }` on a 2xx with a parseable body. Anything
 * else surfaces as a typed {@link SessionPolicyError}. The function does
 * not throw, does not log raw bodies, and does not echo wire / transport
 * detail through any user-facing string.
 */
export async function fetchSessionPolicy(
  options: FetchSessionPolicyOptions = {},
): Promise<SessionPolicyResponse> {
  const endpoint = options.endpoint ?? "/api/v1/config/session-policy";
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
  const parsed = parseSessionPolicy(body);
  if (parsed === null) {
    return { ok: false, error: { kind: "malformed_response" } };
  }
  return { ok: true, policy: parsed };
}

/**
 * Module-level cache of a successful fetch. Multiple views asking for
 * the policy in the same browser tab share the result. Failures are
 * NOT cached — a transient network blip should not pin the fallback
 * forever; the next consumer gets a fresh attempt.
 *
 * The cache holds the resolved success value (not the in-flight
 * promise) so the policy is exposed synchronously after the first
 * resolve. The pending-state guard prevents 3 concurrent views from
 * firing 3 wire calls at mount time.
 */
let cachedPolicy: SessionPolicy | null = null;
let inflight: Promise<SessionPolicy> | null = null;

/**
 * Resolve the configured session policy with a safe fallback.
 *
 * Never throws, never blocks. Failure paths (transport, HTTP, malformed
 * body, range violation) all collapse to the
 * {@link DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS} fallback so the calling
 * view can render honest copy even before the network round-trip
 * resolves OR if it fails entirely. The successful value is cached at
 * module scope; failures are not.
 */
export async function loadSessionPolicy(
  options: FetchSessionPolicyOptions = {},
): Promise<SessionPolicy> {
  if (cachedPolicy !== null) return cachedPolicy;
  if (inflight !== null) return inflight;
  inflight = (async () => {
    const result = await fetchSessionPolicy(options);
    if (result.ok) {
      cachedPolicy = result.policy;
      return result.policy;
    }
    // Drop the inflight so the next caller can try again — failures are
    // not cached. The DEFAULT fallback is what the immediate caller
    // sees so the UI can render honest copy without blocking.
    return {
      detached_live_pty_ttl_seconds: DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
      max_live_pty_sessions_per_user: DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER,
    };
  })().finally(() => {
    // CRITICAL ordering: clear `inflight` BEFORE any subsequent caller
    // sees `inflight !== null`. On the success path `cachedPolicy` is
    // already set, so the next call hits the synchronous return at the
    // top of `loadSessionPolicy`. On the failure path NEITHER cache
    // slot is set, so the next call starts a fresh attempt — that's
    // the "failures are not cached" contract pinned by
    // `tests/sessionPolicy.test.ts::loadSessionPolicy "does NOT cache
    // failures"`. Reversing this assignment would either pin the
    // fallback forever (cache failures) or block concurrent callers
    // on a stale inflight (cache stale promises). Don't touch.
    inflight = null;
  });
  return inflight;
}

/**
 * Test-only cache reset. Vitest tests that exercise the loader twice
 * in the same module instance must call this between cases so the
 * second test does not see the first's cached value.
 */
export function __resetSessionPolicyCache(): void {
  cachedPolicy = null;
  inflight = null;
}

const SECONDS_PER_MINUTE = 60;
const SECONDS_PER_HOUR = 60 * 60;
const SECONDS_PER_DAY = 24 * 60 * 60;

/**
 * Format an integer-seconds TTL as a short, approximate English copy
 * suitable for inline UX strings ("about 30 seconds", "about 5 minutes",
 * "about 1 hour", "about 24 hours").
 *
 * The function preserves the "about" hedge because the backend's reaper
 * fires at the configured TTL within a margin (the 2026-05-10 long-TTL
 * smoke observed ±30 ms at 1800 s, but a future load profile may widen
 * that margin); pretending the literal value is precise would be a UX
 * overclaim of the same family as "always available".
 *
 * Pluralisation is honest: `1 second`, `2 seconds`. Hour formatting
 * stays in hours up to 24 then falls back to days; "1 day" is reserved
 * for the 86_400 case (the validator hard-cap) so an operator-set
 * value reads naturally.
 */
export function formatDetachedTtl(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) {
    return formatDetachedTtl(DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS);
  }
  const s = Math.round(seconds);
  if (s < SECONDS_PER_MINUTE) {
    return `about ${s} second${s === 1 ? "" : "s"}`;
  }
  if (s < SECONDS_PER_HOUR) {
    const minutes = Math.round(s / SECONDS_PER_MINUTE);
    return `about ${minutes} minute${minutes === 1 ? "" : "s"}`;
  }
  if (s < SECONDS_PER_DAY) {
    const hours = Math.round(s / SECONDS_PER_HOUR);
    return `about ${hours} hour${hours === 1 ? "" : "s"}`;
  }
  // 24h validator cap — render as days for >= 1 day.
  const days = Math.round(s / SECONDS_PER_DAY);
  return `about ${days} day${days === 1 ? "" : "s"}`;
}

/**
 * Full-sentence detached-session disclaimer including the configured
 * TTL window AND the persistence caveat.
 *
 * The two-sentence shape is load-bearing: the first sentence parametises
 * the time window on the wire-observed value; the second is the honest
 * persistence claim (in-memory replay, no backend-restart survival) the
 * SPEC pinned before this slice. A future revision MUST keep both —
 * dropping the disclaimer is the overclaim anti-pattern named in
 * `docs/persistent-sessions.md` § 11.7.
 */
export function describeDetachedTtl(seconds: number): string {
  return `Detached sessions stay reconnectable for ${formatDetachedTtl(
    seconds,
  )} after the last client drop. Replay is in-memory and not durable across a backend restart.`;
}

/**
 * Short, parameterised copy describing the per-user live-PTY ceiling
 * (Phase 1B.1 quota). Used by terminal-launch error formatters to
 * render honest text on a `429 too_many_sessions` refusal.
 *
 * Anti-overclaim register (`docs/session-quotas.md` § 7.5):
 *   - Never says "your session quota" — the cap is per-deployment.
 *   - Never says "rate-limiting" / "slow down" / "queue" / "wait N
 *     seconds" — this is a concurrent ceiling, not a rate limit, and
 *     the refusal carries no `Retry-After` contract.
 *   - Never says "always available" — sessions are bounded.
 */
export function describeMaxLivePtyPerUser(cap: number): string {
  const safe = Number.isInteger(cap) && cap > 0
    ? cap
    : DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER;
  return `This deployment allows up to ${safe} live terminal session${safe === 1 ? "" : "s"} per user.`;
}
