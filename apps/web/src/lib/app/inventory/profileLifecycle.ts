/**
 * Pure helpers for the production server-profile disable / enable UI.
 *
 * The Servers view owns the imperative `$state`, the API call wiring,
 * and the launch / preflight / trust / auth-check render conditions.
 * The boolean / label / copy logic that gates those affordances lives
 * here so vitest can pin the contract without a Svelte runtime.
 *
 * Architectural rules (mirror SPEC.md "Server profile disable / enable
 * UI"):
 *  - Helpers are PURE — no DOM, no localStorage, no fetch, no logging.
 *  - Helpers operate on the typed `ServerProfile` DTO. They never touch
 *    `private_key` / `encrypted_private_key` (the DTO doesn't declare
 *    them); the redaction-sentinel tests pin that the helpers never
 *    surface those even if a hostile fixture smuggles them in.
 *  - Disable is a launch-time gate, NOT a runtime kill switch. The
 *    helpers' copy must NOT imply that disabling a profile kills its
 *    existing live terminal sessions.
 *  - Disabled profiles are NOT deleted, archived, or hidden by default.
 *    The copy must avoid destructive language.
 */

import type { ServerProfile } from "../../api/serverProfiles.js";

/**
 * Whether the supplied profile is currently disabled. Mirrors the
 * backend's `ServerProfile::is_disabled()` predicate — a non-null
 * `disabled_at` timestamp is the single source of truth.
 *
 * Pure; never falls back through a side channel; tolerates a stray
 * `undefined` for forward compatibility with future DTO additions
 * (matching the parser's `disabled_at` policy).
 */
export function isServerProfileDisabled(profile: ServerProfile): boolean {
  return typeof profile.disabled_at === "string" && profile.disabled_at.length > 0;
}

/**
 * One-word label for the profile's lifecycle state. Used in inventory
 * badges next to the profile name.
 */
export function profileLifecycleLabel(profile: ServerProfile): string {
  return isServerProfileDisabled(profile) ? "disabled" : "enabled";
}

/**
 * Visual tone for the lifecycle badge. The Servers view maps this to
 * a colour palette; keeping the mapping discrete (string-literal
 * union) instead of a colour string keeps the styling decision in the
 * component, not the helper.
 */
export type LifecycleTone = "neutral" | "muted";

export function profileLifecycleTone(profile: ServerProfile): LifecycleTone {
  return isServerProfileDisabled(profile) ? "muted" : "neutral";
}

/**
 * Whether host-key preflight, host-key trust, and SSH auth-check are
 * allowed against this profile. Disabled profiles refuse setup-side
 * actions on the backend with `409 conflict` — the UI mirrors that
 * gate so the operator sees a clear "enable first" affordance instead
 * of having to read a wire error.
 */
export function canRunProfileSetupActions(profile: ServerProfile): boolean {
  return !isServerProfileDisabled(profile);
}

/**
 * Whether a new terminal session may be launched against this profile.
 * Mirrors the backend's launch-time guard — disabled profiles are
 * refused with `409 conflict { entity: "server_profile", reason:
 * "disabled" }`.
 *
 * Existing live sessions are NOT covered by this gate; they continue
 * until they close on their own (operator close, remote shell exit,
 * PTY teardown, TTL expiry).
 */
export function canLaunchProfile(profile: ServerProfile): boolean {
  return !isServerProfileDisabled(profile);
}

/**
 * Honest one-line copy describing what disabled state means for THIS
 * profile. Returns an empty string for an enabled profile — the caller
 * does the conditional render.
 *
 * The copy avoids destructive / archival language: the profile is not
 * deleted, the operator can re-enable, and existing live sessions are
 * unaffected.
 */
export function describeDisabledProfile(profile: ServerProfile): string {
  if (!isServerProfileDisabled(profile)) return "";
  return "This profile is disabled. New terminal launches, host-key preflight / trust, and auth-check are blocked. Existing live sessions are unaffected.";
}

/**
 * Describes the meaning of the disable confirmation copy shown next to
 * the "Disable profile" button. Static; no profile-specific data is
 * interpolated, so a hostile profile name cannot reach the rendered
 * paragraph by accident.
 */
export const DISABLE_CONFIRMATION_COPY =
  "Disabling this profile blocks new terminal launches, host-key preflight, host-key trust, and auth-check. Existing live sessions keep running until they close on their own.";

/**
 * Describes what re-enabling the profile actually proves. Static; same
 * redaction posture as {@link DISABLE_CONFIRMATION_COPY}.
 */
export const ENABLE_CONFIRMATION_COPY =
  "Enabling permits setup and launch attempts again. It does NOT prove host-key trust or auth readiness — re-run preflight, trust the host key, and re-run auth-check before launching.";

/**
 * Disable confirmation gate. The Servers view requires the operator to
 * type the profile's name verbatim before the disable request fires —
 * mirrors the deliberate-confirmation pattern the host-key trust panel
 * already uses. Returns `true` when the typed value matches the
 * profile's `name` exactly (including case and whitespace).
 *
 * The comparison is intentionally strict: a disable affects every
 * future launch / setup action against the profile, so requiring an
 * exact echo of the displayed name is a low-friction-but-deliberate
 * confirmation that the operator is acting on the row they think they
 * are. Pure; never falls back through a side channel.
 */
export function disableConfirmationMatches(
  profile: ServerProfile,
  typed: string,
): boolean {
  if (typeof typed !== "string") return false;
  if (typed.length === 0) return false;
  return typed === profile.name;
}
