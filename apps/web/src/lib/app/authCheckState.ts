/**
 * Pure helpers for the production SSH auth-check UI on
 * `ServersView.svelte`. Extracted so they can be unit-tested without
 * pulling in the Svelte component.
 *
 * Architectural rule (load-bearing):
 *  - Auth-check NEVER opens a PTY, runs a shell, executes a command,
 *    or persists a terminal session. The success copy must explicitly
 *    disclaim that scope so the operator does not mistake "credentials
 *    work" for "shell is ready".
 *  - Auth-check NEVER exposes private-key material. The wire response
 *    has no key fields, the parsed DTO has no key fields, and the UI
 *    formatters here are functions of `status` only.
 *  - Trusted host key is a precondition. `host_key_unknown` and
 *    `host_key_changed` are diagnostic outcomes the UI must surface
 *    as "trust the host key first" — never as an internal error.
 *  - The component layer that imports these helpers must hold its
 *    auth-check state per-profile-id (local Svelte state). No global
 *    stores, no router, no polling, no auto-retry on failure.
 */

import type { AuthCheckStatus } from "../api/serverProfiles.js";

/** Short, deliberately-conservative status label for the UI badge. */
export function authCheckStatusLabel(status: AuthCheckStatus): string {
  switch (status) {
    case "authentication_succeeded":
      return "Authenticated";
    case "authentication_failed":
      return "Auth rejected";
    case "host_key_unknown":
      return "Host key not trusted";
    case "host_key_changed":
      return "Host key changed";
    case "connection_failed":
      return "Connection failed";
  }
}

/**
 * One-line operator-facing description of what each auth-check outcome
 * means. Phrasing names the SSH-public-key-only scope and never implies
 * a PTY, shell, command execution, or session readiness.
 */
export function authCheckStatusDescription(status: AuthCheckStatus): string {
  switch (status) {
    case "authentication_succeeded":
      return "SSH public-key authentication succeeded for the configured username. No PTY was allocated and no command was executed. Terminal launch is a separate, deliberate action.";
    case "authentication_failed":
      return "Host key matched a trusted pin, but the server rejected the configured SSH identity for the configured username. Confirm the public key is installed in the user's authorized_keys on the target.";
    case "host_key_unknown":
      return "The captured host key is not pinned and trusted. RelayTerm refused to send a client signature to an unverified peer. Run host-key preflight and trust the captured fingerprint above before re-running auth-check.";
    case "host_key_changed":
      return "The host key differs from the pinned entry for this host. Auth was NOT attempted. Investigate before continuing — server reinstallation, key rotation, or man-in-the-middle are all possible explanations.";
    case "connection_failed":
      return "SSH transport failed before authentication could complete. The host, network, or sshd was unreachable, refused, timed out, or returned a malformed banner.";
  }
}

/**
 * Severity tone for the UI badge / panel. Pure function of `status`
 * with no I/O. The component layer maps each tone to a Tailwind colour
 * group.
 */
export type AuthCheckTone = "ok" | "warn" | "blocked" | "error";

export function authCheckStatusTone(status: AuthCheckStatus): AuthCheckTone {
  switch (status) {
    case "authentication_succeeded":
      return "ok";
    case "authentication_failed":
      return "error";
    case "host_key_unknown":
      return "warn";
    case "host_key_changed":
      return "blocked";
    case "connection_failed":
      return "error";
  }
}

/**
 * Whether terminal launch WOULD be allowed later for a given auth-check
 * outcome. Pure function of `status`; advisory only — terminal launch is
 * its own slice with its own server-side preconditions.
 *
 * Today every status returns `false` for the non-success cases. The
 * helper exists so the UI can render a precise, honest hint
 * ("credentials worked; terminal launch will be a separate action") and
 * so a future terminal-launch slice has a single place to update the
 * gating logic if the contract changes.
 */
export function terminalLaunchWouldBeAllowed(
  status: AuthCheckStatus,
): boolean {
  return status === "authentication_succeeded";
}

/**
 * Static disclaimer rendered next to the "Run auth-check" action.
 * Names the SSH-public-key-only scope so the operator cannot mistake
 * auth-check for a terminal launch or a command-execution surface.
 */
export const AUTH_CHECK_DISCLAIMER =
  "Auth-check verifies that the configured SSH identity authenticates against the server. It requires a trusted host key first. It does not open a terminal, does not run commands, and does not install your public key.";

/**
 * Static one-line confirmation rendered below a successful auth-check.
 * Pinned by tests against accidental "session opened" / "shell ready"
 * phrasing — auth-check must not imply terminal readiness.
 */
export const AUTH_CHECK_SUCCESS_FOOTNOTE =
  "Credentials worked for SSH authentication. Terminal launch is still a separate action and is not yet implemented in the production shell.";
