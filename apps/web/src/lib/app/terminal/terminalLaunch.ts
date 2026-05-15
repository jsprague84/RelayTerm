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
  TerminalRenderer,
  TerminalSessionState,
} from "@relayterm/terminal-core";
import type { CreateTerminalSessionError } from "../../api/terminalSessions.js";
import type { RendererLoadFallback } from "./rendererLoader.js";
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
export function safeFit(renderer: unknown): { cols: number; rows: number } | null {
  // `fit` is an xterm-specific capability — it is not on the neutral
  // {@link TerminalRenderer} surface, so an experimental renderer
  // (ghostty-web, restty, wterm) may not expose it. Probe at runtime
  // so a Fit click on a non-fittable renderer is a clean no-op rather
  // than a TypeError. The `unknown` parameter type lets the production
  // workspace pass its neutrally-typed renderer variable without an
  // explicit cast at every call site.
  if (renderer === null || typeof renderer !== "object") return null;
  const fit = (renderer as { fit?: unknown }).fit;
  if (typeof fit !== "function") return null;
  try {
    const result = (fit as () => { cols: number; rows: number } | null).call(
      renderer,
    );
    return result ?? null;
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
export function safeClearViewport(renderer: unknown): boolean {
  // Same xterm-specific-capability rule as {@link safeFit} — probe at
  // runtime so a Clear click on a renderer without a `clear()`
  // affordance no-ops cleanly.
  if (renderer === null || typeof renderer !== "object") return false;
  const clear = (renderer as { clear?: unknown }).clear;
  if (typeof clear !== "function") return false;
  try {
    (clear as () => void).call(renderer);
    return true;
  } catch {
    return false;
  }
}

/**
 * Marker attribute stamped by {@link markRendererInputTarget} onto the
 * DOM element that actually receives keyboard input for the mounted
 * renderer. A renderer-evaluation smoke (or an operator) focuses and
 * verifies `[data-relayterm-terminal-input]` instead of guessing
 * between the viewport DIV and a per-renderer helper textarea — the
 * xterm and ghostty-web adapters disagree on which element that is
 * (xterm: a hidden child `<textarea>`; ghostty-web: the contenteditable
 * host element). The marker is renderer-neutral: one selector targets
 * the correct element regardless of which renderer mounted.
 *
 * Deliberately NOT `data-testid`: ghostty-web's focus target IS the
 * viewport element, which already carries
 * `data-testid="production-terminal-viewport"`; a second `data-testid`
 * would clobber it. A dedicated attribute coexists with the existing
 * testid and never collides.
 */
export const TERMINAL_INPUT_MARKER_ATTR = "data-relayterm-terminal-input";

/**
 * Resolve a renderer's keyboard-input element via the renderer-neutral
 * optional `focusTarget()` method on {@link TerminalRenderer}.
 *
 * Returns the element typed as {@link Element} (the type that actually
 * carries `setAttribute` / `removeAttribute` — the
 * `TerminalRenderer.focusTarget` contract narrows it to `HTMLElement`
 * for typed callers, but this helper takes `unknown` and duck-types).
 * Returns `null` when the renderer does not implement `focusTarget()`
 * (restty / wterm today), when it has no input element yet (pre-mount /
 * post-dispose), when the call throws (a mid-dispose race), or when the
 * returned value is not a settable DOM element. The `unknown` parameter
 * type lets the production workspace pass its neutrally-typed renderer
 * variable without a cast.
 */
function resolveRendererInputTarget(renderer: unknown): Element | null {
  if (renderer === null || typeof renderer !== "object") return null;
  const focusTarget = (renderer as { focusTarget?: unknown }).focusTarget;
  if (typeof focusTarget !== "function") return null;
  let element: unknown;
  try {
    element = (focusTarget as () => unknown).call(renderer);
  } catch {
    // A renderer mid-dispose could throw; treat exactly like "no input
    // element exposed". Swallow without logging — an error message
    // could surface renderer-internal state.
    return null;
  }
  if (element === null || typeof element !== "object") return null;
  const candidate = element as {
    setAttribute?: unknown;
    removeAttribute?: unknown;
  };
  if (
    typeof candidate.setAttribute !== "function" ||
    typeof candidate.removeAttribute !== "function"
  ) {
    return null;
  }
  return element as Element;
}

/**
 * Stamp {@link TERMINAL_INPUT_MARKER_ATTR} on the renderer's keyboard-
 * input element. Returns the marked element, or `null` when the
 * renderer exposes no settable input element (see
 * {@link resolveRendererInputTarget}).
 *
 * Pair every successful call with {@link unmarkRendererInputTarget}
 * before the renderer is disposed — the attribute lives on the
 * renderer-owned DOM node, and `focusTarget()` returns `null` once the
 * renderer is torn down, so the marker can only be removed while the
 * renderer is still live.
 *
 * Redaction posture: this helper only ever calls `setAttribute` with a
 * fixed attribute name and the literal string `"true"`. It NEVER reads
 * the element's value / textContent, never logs, never touches
 * `localStorage` / `sessionStorage`, and never carries payload bytes —
 * user input still flows exclusively through `renderer.onInput`.
 */
export function markRendererInputTarget(renderer: unknown): Element | null {
  const element = resolveRendererInputTarget(renderer);
  if (element === null) return null;
  try {
    element.setAttribute(TERMINAL_INPUT_MARKER_ATTR, "true");
  } catch {
    return null;
  }
  return element;
}

/**
 * Remove the {@link TERMINAL_INPUT_MARKER_ATTR} marker from the
 * renderer's keyboard-input element. Call this while the renderer is
 * still live (before `dispose()`), so a future adapter whose
 * `dispose()` nulls its internal terminal but leaves the host element
 * in the DOM cannot strand a stale marker on a reusable node. A no-op
 * when the renderer exposes no input element. Same redaction posture as
 * {@link markRendererInputTarget} — only `removeAttribute` is called.
 */
export function unmarkRendererInputTarget(renderer: unknown): void {
  const element = resolveRendererInputTarget(renderer);
  if (element === null) return;
  try {
    element.removeAttribute(TERMINAL_INPUT_MARKER_ATTR);
  } catch {
    // Best-effort cleanup; a throw here means the element is already
    // gone, which is the desired end state anyway.
  }
}

/**
 * Closed taxonomy mirrored onto `data-renderer-autofit` on the
 * production terminal section. Reflects the operator's preference AND
 * the mounted renderer's honest capability — see § 9 of
 * `docs/renderer-neutral-autofit.md`.
 *
 *  - `off`         — operator did not enable autofit; the renderer is
 *                    not being container-fit. The default for fresh
 *                    users.
 *  - `active`      — autofit enabled AND the mounted renderer wired it
 *                    (`autofitActive() === true`).
 *  - `unsupported` — autofit enabled BUT the mounted renderer no-ops
 *                    it (`autofitActive()` is `false`, throws, the
 *                    method is omitted, or no renderer is mounted).
 *
 * Diagnostic taxonomy only — never carries payload bytes. The workspace
 * exposes the value so a renderer-evaluation smoke can prove autofit is
 * actually wired for a given renderer without visual guessing.
 */
export type RendererAutofitStatus = "off" | "active" | "unsupported";

export interface RendererAutofitStatusInput {
  /** Operator preference from `TerminalSettings.autofitEnabled`. */
  autofitEnabled: boolean;
  /**
   * Mounted renderer (or `null` pre-mount / post-mount-failure). Typed
   * as `unknown` so the helper duck-types the optional `autofitActive()`
   * method on the renderer-neutral interface — a renderer that omits
   * the method maps to `unsupported`.
   */
  renderer: unknown;
}

/**
 * Compute the closed `data-renderer-autofit` value for a workspace.
 *
 * Redaction posture: this helper inspects exactly one boolean
 * (`autofitEnabled`) and one optional method (`autofitActive()`) on the
 * renderer. It NEVER reads renderer DOM, NEVER touches input/output
 * bytes, and NEVER logs. A throwing `autofitActive()` (a renderer mid-
 * dispose) is swallowed to `unsupported` — the right diagnostic shape
 * for a renderer the workspace cannot honestly call "active".
 */
export function computeRendererAutofitStatus(
  input: RendererAutofitStatusInput,
): RendererAutofitStatus {
  if (!input.autofitEnabled) return "off";
  const renderer = input.renderer;
  if (renderer === null || typeof renderer !== "object") return "unsupported";
  const probe = (renderer as { autofitActive?: unknown }).autofitActive;
  if (typeof probe !== "function") return "unsupported";
  try {
    return (probe as () => boolean).call(renderer) === true
      ? "active"
      : "unsupported";
  } catch {
    return "unsupported";
  }
}

/**
 * Operator-facing copy rendered as the title attribute on the
 * disabled-because-autofit-is-active Fit button. The string is closed
 * vocabulary — no session ids, no error messages, no renderer-internal
 * state. § 9 of `docs/renderer-neutral-autofit.md` pins the wording.
 */
export const FIT_BUTTON_AUTOFIT_ACTIVE_TOOLTIP =
  "Autofit is keeping the terminal sized to its container.";

/**
 * Operator-facing copy rendered as the title attribute on the
 * disabled-because-renderer-does-not-support-fit Fit button. Closed
 * vocabulary; pins the "honest about what the button does" rule from
 * § 9 of `docs/renderer-neutral-autofit.md`.
 */
export const FIT_BUTTON_RENDERER_UNSUPPORTED_TOOLTIP =
  "Fit is not supported by the current renderer.";

export interface FitButtonStateInput {
  /**
   * `true` when the workspace is in a live phase (attached / replaying)
   * AND the renderer is mounted. The production component derives this
   * from {@link computeWorkspaceEnablement}'s `fit` predicate.
   */
  liveRenderer: boolean;
  /**
   * Mounted renderer (or `null`). Typed `unknown` so the helper
   * duck-types the optional `fit()` method (xterm exposes it; the
   * experimental adapters do not). Used only to probe the method's
   * presence — never invoked here.
   */
  renderer: unknown;
  /**
   * `true` when the renderer has reported live autofit
   * (`autofitActive() === true`).
   */
  autofitActive: boolean;
}

export interface FitButtonState {
  /** `true` exactly when the operator can usefully click Fit. */
  enabled: boolean;
  /**
   * Closed-vocabulary tooltip explaining a disabled state. `undefined`
   * for the enabled path AND for the "not-live" path — the disabled-
   * because-not-live case relies on the same generic disable affordance
   * the other workspace buttons use; only the autofit-active and
   * renderer-unsupported cases get a dedicated explanation string.
   */
  tooltip: string | undefined;
}

/**
 * Compute Fit-button enablement and copy. The button stays a best-
 * effort one-shot refit via the existing `safeFit()` (the xterm-only
 * `fit()`); this helper makes it HONEST about what it does — informed
 * by `autofitActive()` and the renderer's capability surface.
 *
 * Precedence (deliberate):
 *  1. Workspace not live → disabled, no special tooltip (the generic
 *     "fit is only meaningful while attached" affordance covers this).
 *  2. Autofit is active → disabled with the autofit-active tooltip. A
 *     renderer that is continuously fitting its container makes the
 *     one-shot button redundant; surfacing the autofit reason is more
 *     helpful than the missing-`fit()` one even when both predicates
 *     fire (some future renderer might be in this exact shape).
 *  3. Renderer has no `fit()` → disabled with the renderer-unsupported
 *     tooltip. The dev lab already exposes adapter-specific affordances
 *     for the experimental renderers; the production button is honest
 *     about its xterm-shaped one-shot nature.
 *  4. Otherwise → enabled, no tooltip.
 */
export function computeFitButtonState(
  input: FitButtonStateInput,
): FitButtonState {
  if (!input.liveRenderer) {
    return { enabled: false, tooltip: undefined };
  }
  if (input.autofitActive) {
    return { enabled: false, tooltip: FIT_BUTTON_AUTOFIT_ACTIVE_TOOLTIP };
  }
  const renderer = input.renderer;
  const hasFit =
    renderer !== null &&
    typeof renderer === "object" &&
    typeof (renderer as { fit?: unknown }).fit === "function";
  if (!hasFit) {
    return {
      enabled: false,
      tooltip: FIT_BUTTON_RENDERER_UNSUPPORTED_TOOLTIP,
    };
  }
  return { enabled: true, tooltip: undefined };
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
 * Operator-facing copy rendered when {@link mountRendererSafely}
 * reports a failed asynchronous mount. The string is intentionally a
 * fixed constant: it carries the failure taxonomy (`adapter_mount_failed`)
 * and the remediation (Settings → xterm → reopen) but NEVER echoes
 * the underlying `Error.message`, the renderer id, the WASM init URL,
 * or any other detail the renderer adapter raised. The SMOKE redaction
 * test and `terminalLaunch.test.ts` both pin this rule.
 */
export const RENDERER_MOUNT_FAILED_MESSAGE =
  "Renderer failed to mount. Switch back to xterm in Settings and reopen the terminal.";

/**
 * Outcome of {@link mountRendererSafely}. The success arm carries
 * nothing; the failure arm carries the typed fallback taxonomy value
 * the workspace mirrors onto its `data-renderer-fallback` attribute.
 *
 * The shape is a discriminated union so the caller's `switch` is
 * exhaustive at compile time — a future taxonomy expansion would break
 * the workspace's `attach()` typecheck rather than silently fall
 * through.
 */
export type MountRendererOutcome =
  | { kind: "mounted" }
  | { kind: "failed"; fallback: Extract<RendererLoadFallback, "adapter_mount_failed"> };

/**
 * Await `renderer.mount(target)` and translate a rejection into a
 * typed fallback diagnostic.
 *
 * The 2026-05-13 ghostty-web production-shell smoke surfaced a real
 * gap: the renderer loader's synchronous fallback paths
 * (`experimental_gate_off` / `unknown_renderer_id` / `adapter_load_failed`)
 * cover gate + dynamic-import + constructor failures, but the
 * adapter's WASM init runs inside `mount()` and rejects ASYNCHRONOUSLY
 * after the loader resolved cleanly. The workspace was left wedged at
 * `data-renderer="unmounted"` with no operator-visible explanation.
 *
 * Redaction posture (mirrors `rendererLoader.ts`'s `catch` block):
 *  - The thrown `Error.message` is swallowed deliberately. It can
 *    include the WASM init URL, a CSP directive verbatim, or stack
 *    frames into the renderer adapter — operator noise that does not
 *    help the recovery action. The fallback string is the safe signal.
 *  - The renderer reference is NOT disposed inside this helper; the
 *    caller owns the mount target and the lifecycle. Disposing here
 *    would compose badly with the workspace's generation-guard reentry
 *    (a teardown that races with the rejection would double-dispose).
 *  - This function NEVER receives or surfaces payload bytes; its only
 *    side effect is the renderer's own `mount()` reaching the DOM.
 *
 * The caller is the production workspace's `attach()` — see
 * `ProductionTerminal.svelte`. The dev lab does not use this helper
 * because the dev workspace exposes a renderer switcher that re-mounts
 * on every selection change with its own diagnostics panel.
 */
export async function mountRendererSafely(
  renderer: TerminalRenderer,
  target: HTMLElement,
): Promise<MountRendererOutcome> {
  try {
    await renderer.mount(target);
    return { kind: "mounted" };
  } catch {
    return { kind: "failed", fallback: "adapter_mount_failed" };
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
