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

import {
  isHostKeyReplacementReasonCode,
  type HostKeyPreflightResponse,
  type HostKeyReplacementReasonCode,
  type HostKeyStatus,
  type ReplaceHostKeyRequest,
  type ReplaceHostKeyResponse,
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
      return "Host key matches an active pinned entry. Run auth-check below to confirm the configured SSH identity authenticates before launching a terminal session.";
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

// ---------------------------------------------------------------------------
// Host-key REPLACE pure helpers
// ---------------------------------------------------------------------------
//
// Phase 3 of the host-key replace flow (see
// `docs/spec/host-key-replace.md` § R6). UI-state helpers — no Svelte
// state, no I/O, no side effects. The HostKeyPanel component will
// compose these in Phase 4; Phase 3 lands them so they can be unit-
// tested in isolation.
//
// Architectural rule (load-bearing): the replace affordance is offered
// ONLY when the most-recent preflight returned `host_key_status ===
// "changed"` AND the fingerprint shape is valid. It is invisible
// (NOT just disabled) for `unknown` and `trusted` outcomes. The replace
// route IS the only operator-sanctioned recovery path from a `changed`
// outcome — there is no "force trust", no "overwrite", no global
// bypass.

/**
 * Wire-stable reason code on the request body. Re-exported so the
 * HostKeyPanel does not need to reach into `lib/api/` for the type.
 */
export type { HostKeyReplacementReasonCode };

/**
 * Type-guard for {@link HostKeyReplacementReasonCode}. Re-exported so
 * a UI component can refuse to submit a request whose `reason_code`
 * fell out of the closed accept-list (e.g. through a forced enum cast)
 * BEFORE any wire round-trip.
 */
export const reasonCodeIsValid = isHostKeyReplacementReasonCode;

/**
 * Operator-facing label + wire enum value for each reason code.
 *
 * Source of truth for the reason picker. The wire enum value stays in
 * sync with the backend's `KnownHostRevocationReason::from_str_tag`
 * accept-list and the DB CHECK in
 * `20260510000022_known_host_entries_revoke_metadata.sql`. The label
 * column is operator copy and may evolve without breaking the wire.
 */
export interface HostKeyReplacementReasonOption {
  /** Wire-stable enum tag — submitted to the backend verbatim. */
  code: HostKeyReplacementReasonCode;
  /** Operator-facing label rendered in the reason picker. */
  label: string;
}

/**
 * Ordered list of reason-code options for the modal's reason picker.
 *
 * Order is operator copy: most common cause first, "operator other"
 * last so it isn't picked by accident. The function returns a fresh
 * array on every call so callers cannot accidentally mutate a shared
 * module-level singleton — Svelte renders the picker once per modal
 * open and a fresh array carries no state-leak risk.
 */
export function replacementReasonOptions(): HostKeyReplacementReasonOption[] {
  return [
    { code: "server_reinstalled", label: "Server reinstalled or rebuilt" },
    {
      code: "host_key_rotated",
      label: "Host key rotated by the server operator",
    },
    {
      code: "lab_target_recreated",
      label: "Lab or staging target recreated",
    },
    { code: "operator_other", label: "Other (acknowledged)" },
  ];
}

/**
 * Whether the operator's typed-confirmation matches the destructive
 * action gate. Byte-exact, case-sensitive — matches the same posture
 * as {@link fingerprintConfirmationMatches}: a `"replace"` /
 * `" REPLACE "` / `"REPLACE\n"` MUST be refused so a destructive write
 * cannot be dispatched by accidental whitespace.
 */
export function replaceConfirmationMatches(input: string): boolean {
  return input === "REPLACE";
}

/**
 * Whether the replace action may be offered from a preflight result,
 * and — if not — why. Pure function of the parsed preflight: no I/O,
 * no Svelte state, no side effects. The Phase 4 component will
 * translate each variant into the appropriate
 * disabled/visible/copy state.
 *
 * On the `ok` path, the helper exposes both fingerprints (old = the
 * active pin the operator is consenting to revoke; new = the freshly-
 * captured fingerprint the operator just confirmed in preflight).
 * The HostKeyPanel forwards them into the modal AND into the request
 * body — keeping the helper as the single derivation point closes a
 * shape-mismatch race where the modal could show the old fingerprint
 * but submit a different `expected_old_fingerprint`.
 *
 * `activePinFingerprint` is supplied by the caller from the inventory
 * panel's known-host-entries fetch. The helper does not run any
 * fingerprint-shape check on `activePinFingerprint` — the panel is
 * responsible for sourcing it from a trusted server response, where
 * the shape was already validated.
 */
export type ReplaceGate =
  | {
      kind: "ok";
      old_fingerprint: string;
      new_fingerprint: string;
    }
  | { kind: "not_changed_status" }
  | { kind: "missing_active_pin" }
  | { kind: "invalid_old_fingerprint_shape" }
  | { kind: "invalid_new_fingerprint_shape" };

export function replaceGateForPreflight(
  preflight: HostKeyPreflightResponse,
  activePinFingerprint: string | null,
): ReplaceGate {
  // Only a `changed` preflight outcome enables the affordance. `unknown`
  // and `trusted` MUST be invisible (not just disabled) — the spec
  // forbids offering replace as a path of least resistance.
  if (preflight.host_key_status !== "changed") {
    return { kind: "not_changed_status" };
  }
  if (activePinFingerprint === null || activePinFingerprint.length === 0) {
    return { kind: "missing_active_pin" };
  }
  if (!isFingerprintShapeValid(activePinFingerprint)) {
    return { kind: "invalid_old_fingerprint_shape" };
  }
  if (
    preflight.host_key_fingerprint.length === 0 ||
    !isFingerprintShapeValid(preflight.host_key_fingerprint)
  ) {
    return { kind: "invalid_new_fingerprint_shape" };
  }
  return {
    kind: "ok",
    old_fingerprint: activePinFingerprint,
    new_fingerprint: preflight.host_key_fingerprint,
  };
}

/**
 * Submit-time decision for the host-key replace modal. Combines every
 * gate the operator must pass through ({@link replaceGateForPreflight},
 * the closed reason-code accept-list, and the typed-`REPLACE`
 * confirmation) into one dispatch site so the component never builds a
 * partially-validated request.
 *
 * On `ready`, the embedded {@link ReplaceHostKeyRequest} is wire-shape
 * complete and may be passed straight to `replaceHostKey(...)` — the
 * `expected_old_fingerprint` is sourced from the preflight's active-pin
 * field, the `expected_new_fingerprint` from the captured fingerprint,
 * and `reason_code` from the validated picker selection. Refusal
 * variants tell the UI which gate failed so the existing modal copy /
 * helper text can light up the correct field.
 */
export type ReplaceSubmitDecision =
  | {
      kind: "blocked";
      reason:
        | "not_changed_status"
        | "missing_active_pin"
        | "invalid_old_fingerprint_shape"
        | "invalid_new_fingerprint_shape"
        | "invalid_reason_code"
        | "confirmation_mismatch";
    }
  | { kind: "ready"; request: ReplaceHostKeyRequest };

export function decideReplaceSubmit(
  preflight: HostKeyPreflightResponse,
  reasonCode: string | null,
  confirmInput: string,
): ReplaceSubmitDecision {
  const gate = replaceGateForPreflight(
    preflight,
    preflight.active_pin_fingerprint,
  );
  if (gate.kind !== "ok") {
    return { kind: "blocked", reason: gate.kind };
  }
  if (!reasonCodeIsValid(reasonCode ?? "")) {
    return { kind: "blocked", reason: "invalid_reason_code" };
  }
  if (!replaceConfirmationMatches(confirmInput)) {
    return { kind: "blocked", reason: "confirmation_mismatch" };
  }
  return {
    kind: "ready",
    request: {
      expected_old_fingerprint: gate.old_fingerprint,
      expected_new_fingerprint: gate.new_fingerprint,
      reason_code: reasonCode as HostKeyReplacementReasonCode,
    },
  };
}

/**
 * Synthesize a `host_key_status: "trusted"` preflight response from the
 * original `changed` preflight + the successful replace response. The
 * panel uses this to advance the badge / fingerprint area to the new
 * pin without an extra round-trip.
 *
 * Builds the result field-by-field — a stray `private_key` /
 * `encrypted_private_key` / `cookie` / `session_token` / `token_hash`
 * smuggled onto the {@link ReplaceHostKeyResponse} fixture cannot reach
 * the synthesized object because no path here copies it. The
 * `active_pin_fingerprint` is reset to `null` because the replace flow
 * is no longer applicable from the synthetic state — there is now
 * nothing to replace.
 */
export function synthesizePostReplacePreflight(
  preflight: HostKeyPreflightResponse,
  replacement: ReplaceHostKeyResponse,
): HostKeyPreflightResponse {
  return {
    profile_id: preflight.profile_id,
    host_id: preflight.host_id,
    hostname: preflight.hostname,
    port: preflight.port,
    host_key_status: "trusted",
    host_key_type: replacement.host_key_type,
    host_key_fingerprint: replacement.trusted_fingerprint,
    active_pin_fingerprint: null,
    message: preflight.message,
  };
}
