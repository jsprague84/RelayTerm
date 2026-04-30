/**
 * Pure helpers for the dev-only xterm live-terminal lab's state surface.
 *
 * The lab itself is a Svelte component and awkward to unit-test, but the
 * *rules* it enforces — what to show in each phase, which buttons are
 * legal, how to phrase the detached-TTL window, how to format replay
 * events without leaking payload bytes — are pure functions and worth
 * pinning. Anything in this module is callable from the component
 * `<script>` block AND from `vitest` against the same code.
 *
 * Critical contracts:
 *  - {@link DETACHED_TTL_MS} mirrors the backend's pinned
 *    `relayterm_terminal::DETACHED_LIVE_PTY_TTL` (30s). Drift is a bug:
 *    the lab text claims a number the operator should be able to verify
 *    against the server. The constant is intentionally local — the
 *    backend value isn't on the wire and we don't poll for it.
 *  - {@link describeTtlWindow} NEVER claims exact backend state. The
 *    countdown is derived from a local detach timestamp and labelled
 *    `approximate` so an operator doesn't read it as a server signal.
 *  - {@link formatReplayWindowLost} carries seq metadata only (per SPEC
 *    §"Output sequence + in-memory replay buffer contract" → "Logging
 *    and reflection prohibitions"). The missed bytes are unrecoverable
 *    from this surface by design and MUST NOT appear in any log line.
 */

import type { TerminalSessionState } from "@relayterm/terminal-core";

/**
 * Lab-level phase, derived from `TerminalSessionState` plus a few extras
 * that the core client doesn't model:
 *
 *  - `replaying` — strictly a sub-state of `attached`; entered on
 *    `replay_start`, exited on `replay_end` or `replay_window_lost`.
 *  - `reconnecting` — the brief window after `teardown()` and before
 *    the next `attach` resolves. Local-only signal.
 *  - `expired` — the local TTL countdown elapsed without a reconnect.
 *    The server may or may not have closed yet; the label is
 *    deliberately the same as a server-confirmed close because the
 *    PTY is unrecoverable in either case.
 *
 * The lab does NOT introduce a state machine library. `phase` is a
 * derivation of three reactive inputs (clientState, replayActive,
 * detachedAt+now) computed inline in the component via `$derived`.
 */
export type LabPhase =
  | "idle"
  | "connecting"
  | "attached"
  | "replaying"
  | "detached"
  | "reconnecting"
  | "closed"
  | "expired"
  | "error";

export type LabPhaseTone = "neutral" | "info" | "ok" | "warn" | "error";

/**
 * Inputs the {@link derivePhase} function needs from the lab. Mapping
 * `TerminalSessionState` → `LabPhase` requires three side signals:
 *
 *  - `replayActive` — between `replay_start` and `replay_end`.
 *  - `detachedAt` — when the lab observed a detach (server frame OR
 *    local disconnect-without-close). Triggers the TTL countdown.
 *  - `nowMs` + `detachedAt` together decide `detached` vs `expired`.
 *  - `reconnectInFlight` — the lab tore down and hasn't received the
 *    next `state_change` yet.
 */
export interface DerivePhaseInput {
  clientState: TerminalSessionState;
  replayActive: boolean;
  detachedAtMs: number | null;
  nowMs: number;
  reconnectInFlight: boolean;
}

/**
 * Map the lab's reactive inputs to a single phase. Pure — no time
 * source, no DOM, no terminal-core calls. The lab passes `nowMs` so
 * tests can inject a deterministic clock.
 */
export function derivePhase(input: DerivePhaseInput): LabPhase {
  const { clientState, replayActive, detachedAtMs, nowMs, reconnectInFlight } =
    input;
  if (clientState === "error") return "error";
  if (clientState === "closed") return "closed";
  if (clientState === "attached") {
    return replayActive ? "replaying" : "attached";
  }
  if (clientState === "connecting") return "connecting";
  if (clientState === "detached") {
    return ttlElapsed(detachedAtMs, nowMs) ? "expired" : "detached";
  }
  // clientState === "idle" — three sub-cases:
  //   - in-flight reconnect (the lab just tore down)
  //   - we just disconnected without sending Close (TTL is ticking)
  //   - genuine idle (no session ever connected, or a clean close cleared it)
  if (reconnectInFlight) return "reconnecting";
  if (detachedAtMs !== null) {
    return ttlElapsed(detachedAtMs, nowMs) ? "expired" : "detached";
  }
  return "idle";
}

function ttlElapsed(detachedAtMs: number | null, nowMs: number): boolean {
  if (detachedAtMs === null) return false;
  return nowMs - detachedAtMs >= DETACHED_TTL_MS;
}

const PHASE_LABELS: Record<LabPhase, string> = {
  idle: "idle",
  connecting: "connecting",
  attached: "attached / live",
  replaying: "replaying",
  detached: "detached (TTL window)",
  reconnecting: "reconnecting",
  closed: "closed",
  expired: "expired (TTL elapsed)",
  error: "error",
};

const PHASE_TONES: Record<LabPhase, LabPhaseTone> = {
  idle: "neutral",
  connecting: "info",
  attached: "ok",
  replaying: "info",
  detached: "warn",
  reconnecting: "info",
  closed: "neutral",
  expired: "warn",
  error: "error",
};

export function labelForPhase(phase: LabPhase): string {
  return PHASE_LABELS[phase];
}

export function toneForPhase(phase: LabPhase): LabPhaseTone {
  return PHASE_TONES[phase];
}

/**
 * Backend's pinned detached-PTY TTL (30s). The constant is duplicated
 * here intentionally — the backend value isn't on the wire and we don't
 * want to poll for it; if the backend changes the pin this constant
 * must change in lockstep. SPEC §"Detached-session TTL contract" names
 * the same number.
 */
export const DETACHED_TTL_MS = 30_000;

export interface ButtonEnablementInput {
  phase: LabPhase;
  hasSessionId: boolean;
  lastSeenSeq: number;
}

/**
 * Which lab buttons should be enabled given the current phase. The
 * Svelte template wires each button's `disabled` attribute to one of
 * these — the rules live in this pure function so the component file
 * has no branching logic of its own to test.
 *
 * Rules:
 *  - `connect` — only valid from a fully clean state (`idle`, `closed`,
 *    `expired`, `error`). The button is greyed when a session id is
 *    missing because the URL builder would 400 the upgrade.
 *  - `ping` / `applyResize` / `detach` / `close` — wire-frame buttons
 *    require an attached client. `replaying` is a sub-state of attached
 *    so they're enabled there too; the wire frames go through the
 *    same socket.
 *  - `dispose` — anything BUT `idle` is fair game. Disposing during a
 *    socket drop or replay handshake just tears down the local
 *    listeners; the server already moved on.
 *  - `disconnectNoClose` — only from a live attachment. This is the
 *    button that initiates the detached-TTL window from the lab side.
 *  - `reconnectWithBookmark` — needs a positive `lastSeenSeq` AND a
 *    non-attached state. Forcing the operator to dispose first would
 *    surface as a stuck button; we just require they not click it
 *    while the wire is already live.
 *  - `reconnectWithoutBookmark` — same disabling rules as the bookmark
 *    variant, but doesn't require any seq history. Useful for "I want
 *    to reconnect after a `replay_window_lost` and accept the grid
 *    reset" flows.
 */
export interface ButtonEnablement {
  connect: boolean;
  ping: boolean;
  applyResize: boolean;
  detach: boolean;
  close: boolean;
  dispose: boolean;
  disconnectNoClose: boolean;
  reconnectWithBookmark: boolean;
  reconnectWithoutBookmark: boolean;
}

const FRESH_PHASES = new Set<LabPhase>(["idle", "closed", "expired", "error"]);
const ATTACHED_PHASES = new Set<LabPhase>(["attached", "replaying"]);

export function computeEnablement(input: ButtonEnablementInput): ButtonEnablement {
  const { phase, hasSessionId, lastSeenSeq } = input;
  const fresh = FRESH_PHASES.has(phase);
  const attached = ATTACHED_PHASES.has(phase);
  // Reconnect buttons are meaningful only when we're NOT live and not
  // already cycling. We also exclude `idle` — there is nothing to
  // reconnect TO from a never-attached state, and the `connect` button
  // is the correct affordance for that case. From `closed`, `expired`,
  // `error`, and `detached` the operator may still hold a useful
  // bookmark (or want a no-replay attach), so reconnect stays enabled.
  const reconnectable =
    !attached &&
    phase !== "connecting" &&
    phase !== "reconnecting" &&
    phase !== "idle";
  return {
    connect: fresh && hasSessionId,
    ping: attached,
    applyResize: attached,
    detach: attached,
    close: attached,
    dispose: phase !== "idle",
    disconnectNoClose: attached,
    reconnectWithBookmark: reconnectable && lastSeenSeq > 0 && hasSessionId,
    reconnectWithoutBookmark: reconnectable && hasSessionId,
  };
}

export interface TtlText {
  /** Operator-visible string. */
  label: string;
  /**
   * `true` whenever the timer was derived from a local detach
   * timestamp — the backend's exact remaining TTL is not on the wire.
   * UI MUST render the `~` prefix or another approximation marker.
   */
  approximate: boolean;
}

export interface TtlInput {
  detachedAtMs: number | null;
  nowMs: number;
}

/**
 * Format the detached-TTL window for the lab header.
 *
 * Returns `null` when there is nothing to show (no detach observed
 * yet). Once the lab has a detach timestamp, the function emits a
 * monotonic countdown that ALWAYS labels itself `approximate` because
 * the backend's true remaining TTL isn't surfaced on the wire.
 *
 * The wording deliberately:
 *  - never says "0s" — when the local clock crosses the deadline the
 *    label flips to "TTL elapsed locally; reattach may 409"
 *  - never claims accuracy beyond ~1s — the lab doesn't poll faster
 *  - never claims authority over server state — the operator is told
 *    the server may have already closed the PTY
 */
export function describeTtlWindow(input: TtlInput): TtlText | null {
  const { detachedAtMs, nowMs } = input;
  if (detachedAtMs === null) return null;
  const elapsedMs = nowMs - detachedAtMs;
  if (elapsedMs >= DETACHED_TTL_MS) {
    return {
      label: "TTL elapsed locally; reattach may 409 (server-truth, not local)",
      approximate: true,
    };
  }
  const remainingSec = Math.max(1, Math.ceil((DETACHED_TTL_MS - elapsedMs) / 1000));
  return {
    label: `TTL window active; ~${remainingSec}s remaining (approximate, local clock)`,
    approximate: true,
  };
}

/**
 * Format `replay_start` for the diagnostic event log. Seq range only —
 * the `output` frames themselves are logged separately by their seq +
 * byte length, never their bytes.
 */
export function formatReplayStart(msg: { from_seq: number; to_seq: number }): string {
  return `replay_start from_seq=${msg.from_seq} to_seq=${msg.to_seq}`;
}

export function formatReplayEnd(msg: { latest_seq: number }): string {
  return `replay_end latest_seq=${msg.latest_seq}`;
}

/**
 * Format `replay_window_lost` for the diagnostic event log. Carries
 * exactly the wire metadata fields — the missed payload bytes are not
 * available on the client (the server already evicted them) and MUST
 * NOT be implied by the formatting (no "..." or "[truncated]" hint).
 */
export function formatReplayWindowLost(msg: {
  requested_seq: number;
  oldest_available_seq: number | null;
  latest_seq: number;
}): string {
  return (
    `replay_window_lost requested_seq=${msg.requested_seq} ` +
    `oldest_available_seq=${msg.oldest_available_seq ?? "null"} ` +
    `latest_seq=${msg.latest_seq}`
  );
}
