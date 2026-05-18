/**
 * Pure helpers for the production Operational Status panel.
 *
 * Scope: shape the small, safe summaries the Settings → Operational
 * Status card renders. The Svelte component
 * (`views/OperationalStatusPanel.svelte`) owns the imperative load /
 * refresh wiring; everything that turns a typed wire / local result
 * into a one-line summary or a count breakdown sits here so vitest
 * can pin the rules without a Svelte runtime.
 *
 * Honesty rules (load-bearing, mirror the dashboard's
 * `dashboardSummary.ts` posture):
 *  - A failed load surfaces as `unavailable`, NOT as zero. Fake zeros
 *    would lie about the operator's state.
 *  - Backend reachability is reported as `ok` / `down` / `unknown`
 *    based on the `/healthz` probe alone. The panel does NOT call
 *    "everything is fine" just because the inventory cards loaded —
 *    a 401 on `/auth/me` and a healthy `/healthz` are different
 *    propositions and the operator should see both.
 *  - The panel never claims B2 (production smoke) or B3 (mobile
 *    portrait) are done. The v1 release cutline at
 *    `docs/v1-production-readiness.md` § 5 owns those — the panel's
 *    readiness section only points to the runbooks.
 *  - Redaction: no helper here declares, reads, or copies any
 *    `password_hash`, `session_token`, `token_hash`, `bootstrap_token`,
 *    `private_key`, `encrypted_private_key`, `client_info`,
 *    `remote_addr`, `user_agent`, or `data_b64` field. Wire DTOs that
 *    cross this surface (`AuthSession`, `TerminalSession`,
 *    `SessionPolicy`) have already been parsed field-by-field by the
 *    api helpers; smuggled extras cannot reach the rendered summary.
 *    Sentinel-string tests in `tests/operationalStatus.test.ts` pin
 *    the redaction posture against future drift.
 */

import type { LoadResult } from "../../api/apiErrors.js";
import {
  describeAuthSessionsError,
  type AuthError,
  type AuthSession,
} from "../../api/auth.js";
import type { HealthStatus } from "../../api/health.js";
import type { SessionPolicy } from "../../api/sessionPolicy.js";
import {
  formatDetachedTtl,
} from "../../api/sessionPolicy.js";
import type {
  TerminalSession,
  TerminalSessionStatus,
} from "../../api/terminalSessions.js";
import {
  summarizeSessionStatuses,
  type SessionStatusBreakdown,
} from "../dashboard/dashboardSummary.js";
import {
  DEFAULT_RENDERER_ID,
  effectiveRendererId,
  isExperimentalRenderer,
  rendererLabel,
  type RendererId,
  type TerminalSettings,
} from "./terminalSettings.js";

/**
 * Indicator state for a single status row. Mirrors the dashboard's
 * `CardState` shape so the Operational Status panel can render the
 * same neutral em-dash treatment for both loading and unavailable.
 *
 * `value` carries a precomputed one-line label suitable for direct
 * render. Keeping the label inside the state (rather than in the
 * component) means the redaction posture lives in one place — the
 * Svelte view never has to know how to format.
 */
export type IndicatorState =
  | { kind: "loading" }
  | { kind: "ready"; value: string; tone: IndicatorTone }
  | { kind: "unavailable"; summary: string };

/**
 * Visual tone for a ready indicator. Pure enum — no Tailwind class
 * mapping here (the component owns the class lookup so a future
 * design system swap is a one-file change). Tones are honest:
 *  - `ok`      — observed positive signal (e.g. `/healthz` returned 200).
 *  - `info`    — neutral fact (counts, configuration values).
 *  - `warn`    — operator should know (e.g. experimental gate ON).
 *  - `bad`     — observed failure (e.g. `/healthz` returned non-2xx).
 */
export type IndicatorTone = "ok" | "info" | "warn" | "bad";

// ---------------------------------------------------------------------
// Backend reachability
// ---------------------------------------------------------------------

/**
 * Map a {@link HealthStatus} (from `checkHealth()`) to an indicator
 * row. `unknown` is the pre-probe placeholder; `ok` and `down` are
 * the two terminal outcomes.
 *
 * Copy stays neutral: a healthy result claims liveness ("backend
 * reachable") only, NOT "everything is fine." A down result is
 * worded actionably ("backend did not respond") without surfacing
 * the underlying transport detail (which `checkHealth` already drops
 * by design).
 */
export function summarizeHealth(status: HealthStatus): IndicatorState {
  if (status === "unknown") return { kind: "loading" };
  if (status === "ok") {
    return {
      kind: "ready",
      value: "Backend reachable",
      tone: "ok",
    };
  }
  return {
    kind: "unavailable",
    summary: "Backend did not respond to a health probe.",
  };
}

// ---------------------------------------------------------------------
// Browser auth sessions (current user)
// ---------------------------------------------------------------------

/**
 * Counts of the caller's own auth sessions, by status. Built off the
 * already-parsed {@link AuthSession} DTO, so smuggled `token_hash` /
 * `password_hash` / `bootstrap_token` keys cannot survive.
 */
export interface AuthSessionsCounts {
  total: number;
  active: number;
  expired: number;
  revoked: number;
  /** Whether the caller's current session row was observed in the list. */
  has_current: boolean;
}

export type AuthSessionsSummary =
  | { kind: "loading" }
  | { kind: "ready"; counts: AuthSessionsCounts }
  /**
   * The `summary` field MUST always be a pre-formatted safe string
   * built by a redaction-aware formatter (today: the canonical
   * source is {@link describeAuthSessionsError}). Callers downstream
   * (e.g. {@link describeAuthSessions}, the panel snippet) pass this
   * value straight to the rendered DOM. A future caller that builds
   * `AuthSessionsSummary` by hand and stashes a raw wire `message`
   * here would silently leak through every consumer; do not do
   * that. The sentinel sweep in `tests/operationalStatus.test.ts`
   * pins the safe path.
   */
  | { kind: "unavailable"; summary: string };

/**
 * Envelope returned by `listAuthSessions()` — a `{ ok: true, sessions }`
 * / `{ ok: false, error: AuthError }` union, NOT the inventory-style
 * {@link LoadResult}. The helper accepts the envelope directly so the
 * panel does not have to re-shape it before calling.
 */
export type AuthSessionsLoadResult =
  | { ok: true; sessions: AuthSession[] }
  | { ok: false; error: AuthError };

/**
 * Aggregate the caller's auth-session list into a small struct the
 * panel renders. `null` is the pre-fetch state. A failed load surfaces
 * as `unavailable` with a typed safe summary built through
 * {@link describeAuthSessionsError} — no wire `message` is echoed; the
 * formatter is the redaction backstop.
 *
 * The aggregation never echoes per-row fields. The `has_current`
 * boolean is the only per-row signal that crosses into the rendered
 * summary, and it is a synthesised flag — not a copy of any field.
 */
export function summarizeAuthSessions(
  result: AuthSessionsLoadResult | null,
): AuthSessionsSummary {
  if (result === null) return { kind: "loading" };
  if (!result.ok) {
    return {
      kind: "unavailable",
      summary: describeAuthSessionsError(result.error),
    };
  }
  const counts: AuthSessionsCounts = {
    total: result.sessions.length,
    active: 0,
    expired: 0,
    revoked: 0,
    has_current: false,
  };
  for (const s of result.sessions) {
    if (s.status === "active") counts.active += 1;
    else if (s.status === "expired") counts.expired += 1;
    else if (s.status === "revoked") counts.revoked += 1;
    if (s.current) counts.has_current = true;
  }
  return { kind: "ready", counts };
}

/**
 * One-line label rendered next to the "Browser sessions" indicator.
 * Pure function of the aggregate; never reads any per-row field.
 */
export function describeAuthSessions(summary: AuthSessionsSummary): IndicatorState {
  if (summary.kind === "loading") return { kind: "loading" };
  if (summary.kind === "unavailable") {
    return { kind: "unavailable", summary: summary.summary };
  }
  const { active, total } = summary.counts;
  const noun = active === 1 ? "session" : "sessions";
  const tail =
    total > active
      ? ` (${total} total including expired or revoked)`
      : "";
  const value = `${active} active ${noun}${tail}`;
  return { kind: "ready", value, tone: active === 0 ? "warn" : "info" };
}

// ---------------------------------------------------------------------
// Terminal sessions
// ---------------------------------------------------------------------

/**
 * Aggregate the terminal-session list into the same shape the
 * dashboard already renders — re-uses {@link summarizeSessionStatuses}
 * so a future change to status taxonomy lands in one helper.
 */
export type TerminalSessionsSummary = SessionStatusBreakdown;

export function summarizeTerminalSessions(
  result: LoadResult<TerminalSession[]> | null,
): TerminalSessionsSummary {
  return summarizeSessionStatuses(result);
}

/**
 * The four wire-stable {@link TerminalSessionStatus} variants in the
 * order the panel renders them ("currently usable" first, "history"
 * last). Exported as a frozen tuple so a test can pin the order.
 */
export const TERMINAL_SESSION_STATUS_DISPLAY_ORDER: readonly TerminalSessionStatus[] =
  ["active", "detached", "starting", "closed"] as const;

/**
 * Human-facing label for a terminal-session status row inside the
 * Operational Status panel. Kept here so the panel does not pull in
 * the per-row `terminal/sessionStatus.ts` formatter (it would drag
 * an entire dependency surface in for one string each).
 */
export function terminalSessionStatusLabel(
  status: TerminalSessionStatus,
): string {
  switch (status) {
    case "active":
      return "Active";
    case "detached":
      return "Detached";
    case "starting":
      return "Starting";
    case "closed":
      return "Closed (history)";
  }
}

/**
 * One-line label for the terminal-sessions indicator: a small summary
 * sentence the panel renders above the per-status breakdown. Pure
 * function of the aggregate.
 */
export function describeTerminalSessions(
  summary: TerminalSessionsSummary,
): IndicatorState {
  if (summary.kind === "loading") return { kind: "loading" };
  if (summary.kind === "unavailable") {
    return {
      kind: "unavailable",
      summary: "Could not load terminal sessions.",
    };
  }
  const live = summary.counts.active + summary.counts.detached;
  const starting = summary.counts.starting;
  if (live === 0 && starting === 0) {
    return {
      kind: "ready",
      value: "No live terminal sessions.",
      tone: "info",
    };
  }
  const liveNoun = live === 1 ? "session" : "sessions";
  const liveText = `${live} live ${liveNoun}`;
  const startingText =
    starting > 0
      ? `, ${starting} starting`
      : "";
  return {
    kind: "ready",
    value: `${liveText}${startingText}.`,
    tone: "info",
  };
}

// ---------------------------------------------------------------------
// Terminal defaults (local browser preferences)
// ---------------------------------------------------------------------

/**
 * Snapshot of the local browser's terminal defaults — the public-safe
 * subset of {@link TerminalSettings} the operator needs to verify at a
 * glance. Cosmetic fields (font family/size, theme preset, scrollback,
 * cursor) are NOT surfaced here: they live in the "Terminal appearance"
 * card above this panel and would just duplicate it.
 */
export interface TerminalDefaultsSummary {
  /** Renderer the next session would actually mount — collapses an
   * experimental selection back to `xterm` when the gate is off. */
  effective_renderer: RendererId;
  /** Renderer the operator picked (may differ from `effective_renderer`
   * when the gate is off). */
  selected_renderer: RendererId;
  /** Operator-facing label for {@link effective_renderer}. */
  effective_renderer_label: string;
  /** Operator-facing label for {@link selected_renderer}. */
  selected_renderer_label: string;
  /** Whether the experimental-renderer evaluation gate is ON. */
  experimental_gate_enabled: boolean;
  /** Whether the renderer-neutral autofit capability is ON. */
  autofit_enabled: boolean;
  /** True when {@link selected_renderer} is an experimental adapter
   * (i.e. anything other than xterm). */
  selection_is_experimental: boolean;
  /** True when the selection would be silently downgraded to xterm on
   * the next mount (gate off but selection is experimental). */
  selection_currently_downgraded: boolean;
}

export function summarizeTerminalDefaults(
  settings: TerminalSettings,
): TerminalDefaultsSummary {
  const selected = settings.rendererId;
  const effective = effectiveRendererId(settings);
  return {
    effective_renderer: effective,
    selected_renderer: selected,
    effective_renderer_label: rendererLabel(effective),
    selected_renderer_label: rendererLabel(selected),
    experimental_gate_enabled: settings.experimentalRendererEvaluationEnabled,
    autofit_enabled: settings.autofitEnabled,
    selection_is_experimental: isExperimentalRenderer(selected),
    selection_currently_downgraded:
      isExperimentalRenderer(selected) && effective === DEFAULT_RENDERER_ID,
  };
}

/**
 * One-line "next session will mount X" copy. Always honest about the
 * v1 default ("xterm is the v1 default") so an operator who flipped
 * the gate but never picked a renderer does not assume the experimental
 * one is active.
 */
export function describeEffectiveRenderer(
  summary: TerminalDefaultsSummary,
): string {
  const base = `Next terminal session will mount ${summary.effective_renderer_label}.`;
  if (summary.selection_currently_downgraded) {
    return `${base} Your selection (${summary.selected_renderer_label}) requires the experimental gate; the workspace falls back to xterm.`;
  }
  if (summary.effective_renderer === DEFAULT_RENDERER_ID) {
    return `${base} xterm is the v1 production default.`;
  }
  return `${base} Experimental renderers are for evaluation only and are not promoted into production at v1.`;
}

export function describeExperimentalGate(
  summary: TerminalDefaultsSummary,
): IndicatorState {
  if (summary.experimental_gate_enabled) {
    return {
      kind: "ready",
      value: "Experimental renderer gate: ON",
      tone: "warn",
    };
  }
  return {
    kind: "ready",
    value: "Experimental renderer gate: off",
    tone: "ok",
  };
}

export function describeAutofit(summary: TerminalDefaultsSummary): IndicatorState {
  return {
    kind: "ready",
    value: summary.autofit_enabled
      ? "Autofit: on"
      : "Autofit: off",
    tone: "info",
  };
}

// ---------------------------------------------------------------------
// Deployment session policy (effective TTL + quotas)
// ---------------------------------------------------------------------

export type SessionPolicySummary =
  | { kind: "loading" }
  | { kind: "ready"; policy: SessionPolicy }
  /**
   * Today's caller — {@link buildSessionPolicySummary} — never
   * produces this variant because `loadSessionPolicy()` itself
   * resolves to either the wire policy or a documented fallback
   * (see `api/sessionPolicy.ts::loadSessionPolicy`). The variant
   * stays in the union so a future caller that needs to surface a
   * hard policy failure can; the `summary` field carries the same
   * pre-formatted-by-a-safe-formatter requirement as
   * {@link AuthSessionsSummary.unavailable.summary}. Sentinel sweeps
   * in `tests/operationalStatus.test.ts` pin the safe path on the
   * indicator formatters.
   */
  | { kind: "unavailable"; summary: string };

/**
 * The session-policy loader (`loadSessionPolicy`) never throws and
 * always resolves to either a real wire value or a documented
 * fallback — there is no `null` failure path to render. The "loading"
 * branch here mirrors the dashboard's pre-fetch placeholder; the
 * "unavailable" branch is reserved for a future caller that wants to
 * surface a hard failure (it is intentionally NOT produced from a
 * cached fallback value, since the operator cannot tell the
 * difference).
 */
export function buildSessionPolicySummary(
  policy: SessionPolicy | null,
): SessionPolicySummary {
  if (policy === null) return { kind: "loading" };
  return { kind: "ready", policy };
}

export function describeDetachedTtlIndicator(
  summary: SessionPolicySummary,
): IndicatorState {
  if (summary.kind === "loading") return { kind: "loading" };
  if (summary.kind === "unavailable") {
    return { kind: "unavailable", summary: summary.summary };
  }
  const ttlText = formatDetachedTtl(
    summary.policy.detached_live_pty_ttl_seconds,
  );
  return {
    kind: "ready",
    value: `Detached PTY reconnect window: ${ttlText}.`,
    tone: "info",
  };
}

export function describeQuotaIndicator(
  summary: SessionPolicySummary,
): IndicatorState {
  if (summary.kind === "loading") return { kind: "loading" };
  if (summary.kind === "unavailable") {
    return { kind: "unavailable", summary: summary.summary };
  }
  const live = summary.policy.max_live_pty_sessions_per_user;
  const starting = summary.policy.max_starting_sessions_per_user;
  return {
    kind: "ready",
    value: `Per-user limits: up to ${live} live and ${starting} starting at once.`,
    tone: "info",
  };
}

// ---------------------------------------------------------------------
// Account summary
// ---------------------------------------------------------------------

/**
 * Operator-facing snippet built from the parsed {@link CurrentUser}
 * DTO. We intentionally do NOT surface `id` (a UUID is operator
 * noise) and do NOT format `last_login_at` aggressively — it can be
 * null for a freshly-bootstrapped user that has not logged in yet.
 *
 * Field set is deliberately small: email + display name + a hint of
 * when the account was created. No secret-shaped fields appear in
 * this struct because the {@link CurrentUser} DTO already excluded
 * them at parse time.
 */
export interface AccountSummary {
  email: string;
  display_name: string;
  account_created_at: string;
  last_login_at: string | null;
}

/**
 * The "Operational Status" panel is rendered only when the user is
 * authenticated (the auth gate would not have admitted us otherwise),
 * but we still tolerate a `null` user prop so unit tests can mount
 * the panel with a partial fixture.
 */
export function summarizeAccount(
  user: {
    email: string;
    display_name: string;
    created_at: string;
    last_login_at: string | null;
  } | null,
): AccountSummary | null {
  if (user === null) return null;
  return {
    email: user.email,
    display_name: user.display_name,
    account_created_at: user.created_at,
    last_login_at: user.last_login_at,
  };
}

// ---------------------------------------------------------------------
// Tailwind class table for tones
// ---------------------------------------------------------------------

/**
 * Tone → CSS class mapping used by the panel's indicator pills. Kept
 * in the helper so a test can pin the four-value taxonomy without
 * importing the Svelte component.
 */
export function toneClass(tone: IndicatorTone): string {
  switch (tone) {
    case "ok":
      return "border-emerald-900/60 bg-emerald-950/30 text-emerald-200";
    case "info":
      return "border-zinc-800 bg-zinc-950/60 text-zinc-200";
    case "warn":
      return "border-amber-900/60 bg-amber-950/20 text-amber-200";
    case "bad":
      return "border-rose-900/60 bg-rose-950/30 text-rose-200";
  }
}
