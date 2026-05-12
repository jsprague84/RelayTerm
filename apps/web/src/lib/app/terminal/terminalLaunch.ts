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
import {
  DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
  DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER,
  formatDetachedTtl,
} from "../../api/sessionPolicy.js";

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
  /**
   * Move browser focus into the renderer surface. Meaningful whenever
   * a renderer is mounted — the live-replay phases — and a no-op
   * otherwise (the safe-focus helper still tolerates a missing
   * renderer; the enablement just hides the affordance).
   */
  focus: boolean;
  /**
   * Refit the renderer to the container and emit a fresh `resize`
   * frame to the backend. Same scope as `focus`: only meaningful while
   * a renderer is live.
   */
  fit: boolean;
  /**
   * Clear the LOCAL viewport + scrollback. Safe whenever the renderer
   * is mounted; never sends a wire frame and never mutates the backend
   * replay buffer.
   */
  clear: boolean;
}

const ATTACHED_PHASES = new Set<WorkspacePhase>(["attached", "replaying"]);
/**
 * Phases from which the workspace's `Reconnect` button MAY re-attach.
 * `closed` is deliberately excluded: the orchestrator dropped the
 * runtime, the wire `attach` is guaranteed to fail, and the operator
 * just sees a generic "connection error" with no recovery path. The
 * staging-smoke "End session → Reconnect → connection error" UX bug
 * came from `closed` previously satisfying this predicate.
 *
 * `error` stays in: a transport blip / decode glitch may resolve on a
 * second try, and the wire-side failure surfaces cleanly through
 * {@link describeWorkspaceError} otherwise.
 */
const RECONNECTABLE_PHASES = new Set<WorkspacePhase>(["detached", "error"]);

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
    focus: attached,
    fit: attached,
    clear: attached,
  };
}

/**
 * Operator-facing message rendered when the workspace refuses a stale
 * reconnect click against a closed session. Centralised so the helper
 * test can pin the copy and a future regression that swaps it for the
 * generic "connection error" string trips the test.
 */
export const RECONNECT_CLOSED_MESSAGE =
  "This session is closed and cannot be reconnected. Launch a new session from the originating server profile.";

/**
 * Operator-facing fallback message rendered when the workspace refuses
 * a reconnect click from a phase that is neither closed nor in the
 * reconnectable set (idle / creating / connecting / attached /
 * replaying). Exported so the helper test pins the copy and a future
 * "be helpful and merge it into the closed-message" regression trips
 * the test.
 */
export const RECONNECT_INELIGIBLE_MESSAGE =
  "Reconnect is not available from the current state.";

export interface ReconnectAttemptInput {
  phase: WorkspacePhase;
}

export type ReconnectAttemptDecision =
  | { kind: "permit" }
  | { kind: "blocked"; summary: string };

/**
 * Classify a `Reconnect` click against the current workspace phase.
 *
 * Defence in depth alongside {@link computeWorkspaceEnablement}: the
 * button is disabled for `closed` already, but if a state-change race
 * or future regression re-enables it, the imperative click handler
 * delegates here and refuses to open the WebSocket. Returning a
 * structured decision (instead of a boolean) lets the workspace
 * surface honest copy — "this session is closed" — instead of the
 * generic "connection error" the staging-smoke bug produced.
 */
export function classifyReconnectAttempt(
  input: ReconnectAttemptInput,
): ReconnectAttemptDecision {
  if (input.phase === "closed") {
    return { kind: "blocked", summary: RECONNECT_CLOSED_MESSAGE };
  }
  if (RECONNECTABLE_PHASES.has(input.phase)) {
    return { kind: "permit" };
  }
  return { kind: "blocked", summary: RECONNECT_INELIGIBLE_MESSAGE };
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
export function describeLaunchError(
  err: CreateTerminalSessionError,
  options: { maxLivePtyPerUser?: number; detachedTtlSeconds?: number } = {},
): string {
  switch (err.kind) {
    case "validation":
      return `Could not start terminal: ${err.reason}`;
    case "http": {
      // Phase 1B.1: per-user live-PTY ceiling refusal (`429
      // too_many_sessions`). Mapped to the spec-pinned parameterised
      // copy in `docs/session-quotas.md` § 7.5. Both the cap and the
      // TTL-window fragment come from `/api/v1/config/session-policy`
      // (or the safe defaults) — never from the wire body of the
      // refusal, which intentionally carries no count, no cap, and
      // no TTL (§ 7.3). Branching on `code` only, never `message`.
      if (err.status === 429 && err.code === "too_many_sessions") {
        const cap = options.maxLivePtyPerUser
          ?? DEFAULT_MAX_LIVE_PTY_SESSIONS_PER_USER;
        const ttlFragment = formatDetachedTtl(
          options.detachedTtlSeconds ?? DEFAULT_DETACHED_LIVE_PTY_TTL_SECONDS,
        );
        return `You're at the limit of ${cap} concurrent terminal session${cap === 1 ? "" : "s"}. Close a session from the Sessions list before starting another. Detached sessions count toward this limit and free up automatically after their reconnect window (${ttlFragment} by default).`;
      }
      // Phase 1B.2a: per-user starting-burst refusal (`429
      // too_many_starting_sessions`). Distinct from the live-cap
      // refusal so the user knows the right action — wait for an
      // in-flight start to complete rather than close an existing
      // session. The wire body intentionally carries no count or
      // cap (§ 7.3); the cap is exposed separately via
      // `describeMaxStartingPerUser` for surfaces that want to
      // surface it.
      if (err.status === 429 && err.code === "too_many_starting_sessions") {
        return "You already have the maximum number of terminal sessions starting. Wait a moment for one to finish starting, then try again.";
      }
      // Phase 1B.2b: deployment-wide live-PTY refusal (`429
      // too_many_sessions_deployment`). The deployment cap is NOT
      // exposed via `/api/v1/config/session-policy` (operator-only,
      // fingerprinting risk — § 5.4), so this copy is STATIC, NOT
      // parameterised on a numeric cap. Honest about the multi-tenant
      // shape ("This RelayTerm deployment") without breaching the
      // owner-scope posture by naming other users; no `Retry-After`
      // wait-language; no "your session quota" overclaim. Branching
      // on `code` only, never `message` (`docs/session-quotas.md`
      // § 7.5).
      if (err.status === 429 && err.code === "too_many_sessions_deployment") {
        return "This RelayTerm deployment is at its live terminal session limit. Close an existing session or wait for a detached session to expire before starting another.";
      }
      return `Could not start terminal: HTTP ${err.status} ${err.code}`;
    }
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
 * Renderer surface a workspace control needs to drive. Kept minimal so
 * the safe-helper functions below can be exercised against a stub in
 * vitest without dragging in the real `XtermRenderer`.
 */
export interface FocusableRenderer {
  focus(): void;
}

export interface FittableRenderer {
  /**
   * Returns the post-fit cell-grid dimensions, or `null` if the
   * renderer declined (e.g. before mount). The shape mirrors
   * `XtermRenderer.fit`.
   */
  fit(): { cols: number; rows: number } | null;
}

export interface ClearableRenderer {
  clear(): void;
}

/**
 * Call `renderer.focus()` if the renderer exists and the call is safe.
 * Returns `true` when the call was made, `false` otherwise. The wrapper
 * absorbs synchronous throws so a torn-down or mid-dispose renderer
 * never escalates a focus request into an uncaught exception. Errors
 * are NOT logged — the renderer disposed-state branch is expected, and
 * silencing the rest matches the redaction posture (an error message
 * could surface the renderer's internal state).
 */
export function safeFocus(renderer: FocusableRenderer | null | undefined): boolean {
  if (!renderer) return false;
  try {
    renderer.focus();
    return true;
  } catch {
    return false;
  }
}

/**
 * Call `renderer.fit()` if the renderer exists. Returns the post-fit
 * dims when the renderer fitted, or `null` if it declined or threw.
 *
 * The wire `resize` frame is driven by the renderer's own `onResize`
 * fanout — xterm's fit addon fires the listener synchronously, and the
 * workspace subscribes to that signal in exactly one place. Do NOT
 * call `client.sendResize` from the call site of this helper, and do
 * NOT call it from inside this helper. See AGENTS.md "Encountered
 * Lessons" for the double-emit rule and the regression that prompted
 * it.
 */
export function safeFit(
  renderer: FittableRenderer | null | undefined,
): { cols: number; rows: number } | null {
  if (!renderer) return null;
  try {
    return renderer.fit();
  } catch {
    return null;
  }
}

/**
 * Call `renderer.clear()` if the renderer exists. Local viewport /
 * scrollback only — this helper NEVER sends a wire frame, NEVER
 * mutates the backend replay buffer, and NEVER asks the remote shell
 * to run `clear`. Returns `true` when the call was made.
 */
export function safeClearViewport(
  renderer: ClearableRenderer | null | undefined,
): boolean {
  if (!renderer) return false;
  try {
    renderer.clear();
    return true;
  } catch {
    return false;
  }
}

/**
 * Stable UX-copy strings rendered by the production terminal workspace.
 * Centralised so the redaction sentinel test can pin them as wire-noise
 * free, and so a SPEC drift trips a unit test rather than a manual
 * smoke. None of these strings depend on runtime state — they are
 * static copy that the workspace mounts inline.
 */
export const TERMINAL_UX_COPY = {
  settingsApplyNote:
    "Appearance settings apply to new terminal sessions. Save preferences in the Settings view, then launch (or reconnect) the session to see them.",
  copyPasteNote:
    "Use your browser's selection + clipboard shortcuts (Ctrl/Cmd+C / Ctrl/Cmd+V, or right-click Paste). Bracketed-paste confirmation, OSC 52, and a clipboard policy editor are future work.",
} as const;

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
