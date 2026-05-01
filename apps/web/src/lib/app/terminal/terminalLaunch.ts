/**
 * Pure helpers for the production terminal launch flow.
 *
 * The production terminal workspace component (`ProductionTerminal.svelte`)
 * keeps the imperative client/renderer wiring; everything with a stable
 * contract — phase labels, button enablement, error formatting — sits
 * here so vitest can pin the rules without a Svelte runtime.
 *
 * Scope contracts re-asserted:
 *  - {@link describeLaunchError} is a function of `kind` + `status` +
 *    `code` ONLY. It NEVER echoes the wire `message` of an HTTP error,
 *    the thrown `Error.message` of a transport failure, or any field
 *    of the request body. The redaction sentinel test
 *    (`tests/terminalLaunch.test.ts`) pins this against future
 *    "be helpful and include the message" regressions.
 *  - {@link describeWorkspaceError} maps `TerminalClientError` to a
 *    short, public string. Server `message` text is intentionally NOT
 *    surfaced — the backend collapses operator detail to a static
 *    field, but a future revision that broadened it must not leak
 *    through this surface.
 *  - {@link phaseLabel} / {@link phaseTone} / {@link computeWorkspaceEnablement}
 *    drive the production UI. The rules mirror the dev lab's helpers but
 *    are scoped to the smaller production button set (no diagnostics
 *    panel, no renderer switcher, no manual resize).
 */

import type {
  TerminalClientError,
  TerminalSessionState,
} from "@relayterm/terminal-core";
import type { CreateTerminalSessionError } from "../../api/terminalSessions.js";

/**
 * Pinned detached-PTY TTL on the backend (30s). The same constant lives
 * in `lib/dev/liveTerminalState.ts`; both copies must move in lockstep
 * with `relayterm_terminal::DETACHED_LIVE_PTY_TTL`. It is duplicated
 * (rather than imported from the dev module) because the production
 * shell is forbidden from importing `lib/dev/`.
 */
export const DETACHED_TTL_MS = 30_000;

/**
 * Top-level workspace phase. Mirrors the dev lab's `LabPhase` but with
 * the experimental sub-states (`reconnecting`) collapsed: production
 * does not auto-poll the local detach clock or expose a renderer
 * switcher, so a dedicated `reconnecting` phase would be UI noise.
 */
export type WorkspacePhase =
  | "idle"
  | "creating"
  | "connecting"
  | "attached"
  | "replaying"
  | "detached"
  | "closed"
  | "error";

export type WorkspacePhaseTone = "neutral" | "info" | "ok" | "warn" | "error";

const PHASE_LABELS: Record<WorkspacePhase, string> = {
  idle: "idle",
  creating: "creating session…",
  connecting: "connecting…",
  attached: "live",
  replaying: "replaying",
  detached: "detached (TTL window)",
  closed: "closed",
  error: "error",
};

const PHASE_TONES: Record<WorkspacePhase, WorkspacePhaseTone> = {
  idle: "neutral",
  creating: "info",
  connecting: "info",
  attached: "ok",
  replaying: "info",
  detached: "warn",
  closed: "neutral",
  error: "error",
};

export function phaseLabel(phase: WorkspacePhase): string {
  return PHASE_LABELS[phase];
}

export function phaseTone(phase: WorkspacePhase): WorkspacePhaseTone {
  return PHASE_TONES[phase];
}

export interface DerivePhaseInput {
  /** `null` before the create call resolves. */
  clientState: TerminalSessionState | null;
  replayActive: boolean;
  creating: boolean;
}

export function derivePhase(input: DerivePhaseInput): WorkspacePhase {
  if (input.creating) return "creating";
  if (input.clientState === null) return "idle";
  switch (input.clientState) {
    case "idle":
      return "idle";
    case "connecting":
      return "connecting";
    case "attached":
      return input.replayActive ? "replaying" : "attached";
    case "detached":
      return "detached";
    case "closed":
      return "closed";
    case "error":
      return "error";
  }
}

export interface WorkspaceEnablementInput {
  phase: WorkspacePhase;
  /** Highest output `seq` the client has observed (replayed or live). */
  lastSeenSeq: number;
}

export interface WorkspaceEnablement {
  /** Send wire `Detach` frame. Only meaningful while attached. */
  detach: boolean;
  /** Send wire `Close` frame. Only meaningful while attached. */
  close: boolean;
  /**
   * Re-attach with `last_seen_seq` after a detach. Disabled while live,
   * while creating, or when the bookmark is `0` (fresh attach has no
   * resume info).
   */
  reconnect: boolean;
  /** Tear down the local client/renderer and return to idle. */
  dispose: boolean;
}

const ATTACHED_PHASES = new Set<WorkspacePhase>(["attached", "replaying"]);
const RECONNECTABLE_PHASES = new Set<WorkspacePhase>([
  "detached",
  "closed",
  "error",
]);

export function computeWorkspaceEnablement(
  input: WorkspaceEnablementInput,
): WorkspaceEnablement {
  const attached = ATTACHED_PHASES.has(input.phase);
  const reconnectable = RECONNECTABLE_PHASES.has(input.phase);
  return {
    detach: attached,
    close: attached,
    reconnect: reconnectable && input.lastSeenSeq > 0,
    dispose: input.phase !== "idle" && input.phase !== "creating",
  };
}

/**
 * Format a {@link CreateTerminalSessionError} as a one-line UI summary.
 *
 * Stays a function of `kind` + `status` + `code` ONLY — never echoes
 * the wire `message` field of an HTTP error or the thrown
 * `Error.message` of a transport failure. The backend's `ApiError`
 * already collapses internal detail to static strings, but the
 * launcher's UI surface MUST stay independent of the wire body so a
 * future fetch wrapper that smuggles request URLs / headers through
 * thrown messages cannot leak through this surface.
 *
 * The validation branch carries the structured `reason` instead of the
 * wire body, which is fine — `reason` is a closed string-literal union
 * defined in the resource module and is operator-safe by construction.
 */
export function describeLaunchError(err: CreateTerminalSessionError): string {
  switch (err.kind) {
    case "validation":
      return `Could not start terminal: ${err.reason}`;
    case "http":
      return `Could not start terminal: HTTP ${err.status} ${err.code}`;
    case "transport":
      return "Could not start terminal: transport error";
    case "malformed_response":
      return "Could not start terminal: malformed response";
  }
}

/**
 * Format a {@link TerminalClientError} as a one-line UI summary for the
 * workspace status line.
 *
 * Server `message` text is intentionally NOT surfaced. The backend's
 * `ServerMsg::Error.message` field is currently a small set of static
 * strings, but the contract does not pin that — a future widening
 * would otherwise leak through the workspace's status surface. The
 * client error envelope's `code` (when present) is wire-stable and
 * safe to render.
 *
 * The redaction rule matches `describeLaunchError`: status is a
 * function of `kind` (and `code` for server errors) only.
 */
export function describeWorkspaceError(err: TerminalClientError): string {
  switch (err.kind) {
    case "transport":
      return "Connection error";
    case "decode":
      return "Protocol decode error";
    case "unexpected_first_frame":
      return "Unexpected protocol handshake";
    case "send_before_attached":
      return "Send attempted before attach";
    case "send_after_terminal":
      return "Send attempted after session ended";
    case "server_error":
      return err.code === undefined
        ? "Server error"
        : `Server error: ${err.code}`;
  }
}

/**
 * Build the WebSocket URL for a session-id attach. The path is the
 * canonical `/api/v1/terminal-sessions/:id/ws` route; the protocol is
 * `wss:` when the page itself was loaded over HTTPS, `ws:` otherwise.
 *
 * Encapsulated as a pure helper so a vitest can confirm the wire URL
 * shape — including encoding of session ids that look almost-like a
 * UUID but contain unsafe path characters — without spinning up a
 * Svelte component.
 */
export function buildAttachWsUrl(input: {
  sessionId: string;
  protocol: string;
  host: string;
}): string {
  const proto = input.protocol === "https:" ? "wss:" : "ws:";
  const path = `/api/v1/terminal-sessions/${encodeURIComponent(input.sessionId)}/ws`;
  return `${proto}//${input.host}${path}`;
}
