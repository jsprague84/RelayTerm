/**
 * Pure helpers for the production Terminal Sessions list UI.
 *
 * The view component (`SessionsView.svelte`) keeps the imperative load /
 * reconnect / close wiring; everything with a stable contract — status
 * label, status tone, action enablement, copy strings — sits here so
 * vitest can pin the rules without a Svelte runtime.
 *
 * Honesty rules re-asserted (mirrors SPEC.md "Production terminal
 * sessions list/status UI"):
 *  - {@link canReconnect} is `false` for `closed` sessions. Closed rows
 *    cannot be re-attached; the orchestrator dropped the runtime.
 *  - {@link canReconnect} is `true` for `active` and `detached` rows but
 *    the UI MUST present the detached path with a TTL disclaimer — the
 *    PTY only survives ~30s after the last attachment dropped.
 *  - {@link canReconnect} for `starting` is `false`: the create call has
 *    not yet returned, the runtime is not bound, and a wire attach would
 *    race the create.
 *  - {@link canClose} is `true` only when the row is still alive
 *    (`starting`, `active`, `detached`). Closing an already-closed row
 *    surfaces `already_closed = true` from the backend, but the UI keeps
 *    the close button disabled to reduce footgun cycles.
 *  - {@link describeSessionStatus} returns short copy that does NOT
 *    overpromise backend-restart recovery, durable replay, or auto-
 *    reconnect outside the TTL window.
 */

import type { TerminalSessionStatus } from "../../api/terminalSessions.js";

export type SessionStatusTone =
  | "neutral"
  | "info"
  | "ok"
  | "warn"
  | "error";

const STATUS_LABELS: Record<TerminalSessionStatus, string> = {
  starting: "starting",
  active: "active",
  detached: "detached",
  closed: "closed",
};

const STATUS_TONES: Record<TerminalSessionStatus, SessionStatusTone> = {
  starting: "info",
  active: "ok",
  detached: "warn",
  closed: "neutral",
};

const STATUS_DESCRIPTIONS: Record<TerminalSessionStatus, string> = {
  starting:
    "Session is starting; the SSH PTY is not yet bound. Reconnect becomes available once the orchestrator promotes it to active.",
  active:
    "Session is live on the backend. Open it to attach, or close it to end the PTY immediately.",
  detached:
    "No client is attached. The remote PTY only survives briefly (~30s) after the last detach — reconnect within that window or the session is reaped. Replay is in-memory and not durable across a backend restart.",
  closed:
    "Session ended. The runtime is gone and cannot be reconnected. Launch a new session from the originating server profile.",
};

export function statusLabel(status: TerminalSessionStatus): string {
  return STATUS_LABELS[status];
}

export function statusTone(status: TerminalSessionStatus): SessionStatusTone {
  return STATUS_TONES[status];
}

/**
 * Human-readable description of a status. Honest about TTL, in-memory
 * replay, and post-restart recovery (none of which exist yet on the
 * backend).
 */
export function describeSessionStatus(status: TerminalSessionStatus): string {
  return STATUS_DESCRIPTIONS[status];
}

/**
 * Whether the row supports the Reconnect/Open action.
 *
 * Only `active` and `detached` rows are reconnectable. `starting` is
 * not — the create call has not yet bound a runtime; attaching would
 * race. `closed` is not — the runtime is gone.
 */
export function canReconnect(status: TerminalSessionStatus): boolean {
  return status === "active" || status === "detached";
}

/**
 * Whether the row supports the Close action.
 *
 * The backend's close is idempotent (closing a closed row returns
 * `already_closed = true`), but the UI keeps the button disabled on
 * `closed` rows so the operator doesn't click a no-op. `starting`
 * rows are closable — the orchestrator hands the close back as a
 * normal lifecycle transition.
 */
export function canClose(status: TerminalSessionStatus): boolean {
  return status !== "closed";
}

/**
 * Whether the row is in a state where the TTL disclaimer copy is
 * meaningful. Only `detached` rows are bounded by the 30s TTL; the
 * disclaimer shown for any other status would be misleading.
 */
export function showsTtlHint(status: TerminalSessionStatus): boolean {
  return status === "detached";
}
