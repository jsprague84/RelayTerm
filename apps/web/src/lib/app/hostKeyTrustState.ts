/**
 * Pure helpers for the production host-key preflight + trust UI on
 * `ServersView.svelte`. Extracted so they can be unit-tested without
 * pulling in the Svelte component.
 *
 * Architectural rule (load-bearing):
 *  - Trust is NEVER auto-enabled. Even after a successful preflight,
 *    the action must require an explicit confirmation by the operator
 *    (checkbox or typed fingerprint).
 *  - Trust is ALWAYS refused for `changed` and `revoked` outcomes.
 *    `revoked` is not a wire status today — the backend collapses
 *    revoked-and-reappearing keys to `unknown`, then refuses the trust
 *    request with `409 conflict`. This module models `revoked` only as
 *    a derived UI hint, deferred to the trust-error formatter (see the
 *    `409` branch of `describeTrustHostKeyError`); the wire-status set
 *    stays `unknown | trusted | changed`.
 *  - Fingerprint confirmation requires an EXACT match against the
 *    captured fingerprint. {@link fingerprintConfirmationMatches} is
 *    the single source of truth for that comparison.
 */

import type {
  HostKeyPreflightResponse,
  HostKeyStatus,
} from "../api/serverProfiles.js";

/** Short, deliberately-conservative status label for the UI badge. */
export function hostKeyStatusLabel(status: HostKeyStatus): string {
  switch (status) {
    case "unknown":
      return "Not trusted";
    case "trusted":
      return "Trusted";
    case "changed":
      return "Changed";
  }
}

/**
 * One-line operator-facing description of what each preflight outcome
 * means. Phrasing names the KEX-only scope and never implies SSH
 * authentication or session readiness.
 */
export function hostKeyStatusDescription(status: HostKeyStatus): string {
  switch (status) {
    case "unknown":
      return "Host key was captured during SSH key exchange, but no pinned entry matches it. Verify the fingerprint matches what you expect for this server before trusting it.";
    case "trusted":
      return "Host key matches an active pinned entry. Run auth-check below to confirm the configured SSH identity authenticates. Terminal launch is still future work.";
    case "changed":
      return "Host key differs from the pinned entry for this host. RelayTerm will not overwrite a pinned key automatically. This may indicate server reinstallation, key rotation, or a possible man-in-the-middle.";
  }
}

/**
 * Whether the operator should be allowed to issue a trust request from
 * the current preflight result, and — if not — why. The backend will
 * refuse anyway (revoked rows are caught server-side, expected
 * fingerprint must match), but the UI gate is the first line of
 * defense and produces a precise, actionable hint.
 */
export type TrustGate =
  | { kind: "ok" }
  | { kind: "already_trusted" }
  | { kind: "changed_refused" }
  | { kind: "missing_fingerprint" }
  | { kind: "invalid_fingerprint_shape" };

/**
 * Decide whether trust may be offered from a preflight result. Pure
 * function of the parsed response — no I/O, no Svelte state, no
 * side effects. The component layer translates each variant into the
 * appropriate disabled/visible/copy state.
 */
export function trustGateForPreflight(
  preflight: HostKeyPreflightResponse,
): TrustGate {
  if (preflight.host_key_status === "trusted") {
    return { kind: "already_trusted" };
  }
  if (preflight.host_key_status === "changed") {
    return { kind: "changed_refused" };
  }
  // status === "unknown" — first-time-seen OR revoked-and-reappearing.
  // The trust route refuses revoked-and-reappearing with 409, so the UI
  // surfaces that as a deferred error after submit. Here we only check
  // shape pre-conditions.
  if (preflight.host_key_fingerprint.length === 0) {
    return { kind: "missing_fingerprint" };
  }
  if (!isFingerprintShapeValid(preflight.host_key_fingerprint)) {
    return { kind: "invalid_fingerprint_shape" };
  }
  return { kind: "ok" };
}

/**
 * Whether the operator's confirmation matches the captured fingerprint
 * EXACTLY. Used both when the UI requires typing the fingerprint to
 * confirm and when a checkbox + the captured fingerprint is paired with
 * the eventual POST body. The comparison is byte-exact and case-
 * sensitive — base64 in `SHA256:<base64>` is case-significant.
 */
export function fingerprintConfirmationMatches(
  captured: string,
  confirmation: string,
): boolean {
  if (captured.length === 0) return false;
  return captured === confirmation;
}

/** Local mirror of the wire shape check; kept private so callers don't
 * depend on the helper's exact regex. The trust API helper's
 * `isValidFingerprintShape` is the single source of truth for the
 * server-bound POST. */
function isFingerprintShapeValid(fp: string): boolean {
  if (!fp.startsWith("SHA256:")) return false;
  if (fp.length < 8 || fp.length > 128) return false;
  for (let i = 0; i < fp.length; i++) {
    const code = fp.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) return false;
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

/**
 * One-line static disclaimer rendered next to the "Trust" action.
 * Names the security model the operator is opting into.
 */
export const TRUST_DISCLAIMER =
  "Only trust if the fingerprint matches what you expect for the server. RelayTerm will not overwrite a changed or revoked host key automatically.";

/**
 * One-line static disclaimer for the preflight action itself. Names
 * the KEX-only scope so the operator is not misled into thinking
 * preflight authenticates or launches a terminal.
 */
export const PREFLIGHT_DISCLAIMER =
  "Preflight verifies the server's host key during SSH key exchange. It does not authenticate, does not open a terminal, and does not install your public key.";
