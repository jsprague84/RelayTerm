/**
 * Client-side terminal launch timing diagnostics.
 *
 * Purpose: give a future mobile / workspace investigation a first-class
 * frontend signal for "how long did each launch stage take", without
 * relying on the staging nginx access log to infer WebSocket open time
 * (the `GET …/ws → 101` line records the upgrade *close* timestamp, not
 * the open timestamp — see the 2026-05-16b Playwright-first investigation
 * in `docs/deployment/vps-staging-smoke.md`).
 *
 * The recorder owns nothing transport-side. The launch flow calls
 * {@link LaunchTimingRecorder.mark} at named lifecycle points; the
 * production workspace renders a compact diagnostic strip and mirrors
 * a small set of `data-launch-timing-*` attributes for smoke selectors.
 *
 * Redaction posture (load-bearing):
 *  - Records only event NAMES and a relative millisecond offset from
 *    `launch_started`. NEVER captures terminal payload bytes, server
 *    `message` strings, URLs, cookies, headers, tokens, or
 *    `Error.message` text. Errors collapse to a closed-vocabulary
 *    {@link LaunchTimingErrorKind} value.
 *  - Lives entirely in memory. Nothing here writes to localStorage,
 *    sessionStorage, IndexedDB, or any other persistence. A page
 *    reload drops the data; that is intentional.
 *  - Uses `performance.now()` for monotonic timestamps (or an injected
 *    `now()` for tests). The recorder NEVER emits wall-clock
 *    timestamps; a smoke that needs to compare an event to a server
 *    log can read `performance.timeOrigin` separately at the call
 *    site if absolute time is required.
 *  - All events are one-shot. If a caller marks the same event twice
 *    the recorder keeps the FIRST observation — the lifetime-style
 *    measurements we care about (`ws_open`, `ws_close`) are
 *    open-vs-close, not repeat-vs-repeat.
 *
 * Verification support (lifetime_X_then_close): the recorder makes it
 * trivial for a future smoke to:
 *   1. open a terminal,
 *   2. hold it open for known X seconds,
 *   3. close from the client,
 *   4. compare client `ws_open` ms, client `ws_close` ms, and the
 *      nginx WS access-log timestamp (now correctly read as the close
 *      timestamp).
 * See `apps/web/e2e/SMOKE.md` § "Launch timing diagnostics" for the
 * step-by-step procedure.
 */

import type { TerminalClientError } from "@relayterm/terminal-core";

/**
 * Closed taxonomy of launch lifecycle events. The set is intentionally
 * small — each name maps to one decision point in the production launch
 * flow (`ServersView.launchProfile` → `AppShell.handleLaunch` →
 * `ProductionTerminal.attach`).
 *
 *  - `launch_started`             — the "Launch terminal" / "Reconnect"
 *                                   click handler entered. The recorder
 *                                   anchors every subsequent event to
 *                                   this point (its relative ms is `0`).
 *  - `create_session_post_started` — `fetch()` issued for
 *                                   `POST /api/v1/terminal-sessions`.
 *  - `create_session_post_resolved` — `fetch()` resolved (success OR
 *                                   typed error; the snapshot's
 *                                   `createPostOutcome` discriminates).
 *  - `ws_connect_started`         — `client.attach(...)` invoked.
 *  - `ws_open`                    — WebSocket `open` event fired (the
 *                                   transport emitted its first server
 *                                   frame OR the synthetic open hook
 *                                   ran).
 *  - `first_server_message`       — the very first server frame
 *                                   landed (JSON or binary). The
 *                                   recorder does NOT capture which
 *                                   frame kind — only that one arrived.
 *  - `first_output`               — the first `Output` frame landed.
 *                                   Distinct from
 *                                   `first_server_message` because the
 *                                   first server frame is normally
 *                                   `session_attached`, with `output`
 *                                   following.
 *  - `attached`                   — the client transitioned to
 *                                   `attached` state.
 *  - `detach_requested`           — the operator clicked Detach.
 *  - `close_requested`            — the operator clicked End session.
 *  - `ws_close`                   — the transport `close` event fired
 *                                   (any cause).
 *  - `error`                      — a typed client / transport error
 *                                   surfaced. The recorder stores the
 *                                   closed-vocabulary kind, never the
 *                                   underlying message.
 */
export const LAUNCH_TIMING_EVENT_NAMES = [
  "launch_started",
  "create_session_post_started",
  "create_session_post_resolved",
  "ws_connect_started",
  "ws_open",
  "first_server_message",
  "first_output",
  "attached",
  "detach_requested",
  "close_requested",
  "ws_close",
  "error",
] as const;

export type LaunchTimingEventName = (typeof LAUNCH_TIMING_EVENT_NAMES)[number];

/**
 * Closed-vocabulary outcome of the create-session POST. Captured
 * alongside `create_session_post_resolved` so the snapshot can tell a
 * 201 from a 4xx / 5xx / transport failure without echoing the wire
 * `message` field or any HTTP body fragment.
 */
export type LaunchTimingCreatePostOutcome = "ok" | "error";

/**
 * Closed-vocabulary error kinds the recorder accepts. Mirrors the
 * {@link import("@relayterm/terminal-core").TerminalClientError} `kind`
 * union (transport / decode / protocol violations / send-state errors)
 * plus a `create_session_post` bucket for the launcher's own POST
 * failure and an `unknown` escape hatch. NEVER carries a free-form
 * message — the kind alone is the signal.
 *
 * The {@link _assertClientErrorKindIsRecorderKind} type-only export
 * below pins the invariant that every `TerminalClientError["kind"]`
 * value is a valid {@link LaunchTimingErrorKind}. If a future revision
 * of `@relayterm/terminal-core` adds a `kind` value that is NOT in
 * this union, that pin fires at compile time so the recorder's
 * closed-vocabulary tests are not silently bypassed by the workspace
 * call-site's `recordTimingError(err.kind)`.
 */
export type LaunchTimingErrorKind =
  | "create_session_post"
  | "transport"
  | "decode"
  | "unexpected_first_frame"
  | "send_before_attached"
  | "send_after_terminal"
  | "server_error"
  | "unknown";

/**
 * Compile-time pin: every `TerminalClientError["kind"]` value MUST be
 * assignable to {@link LaunchTimingErrorKind}. The TypeScript assignment
 * is the assertion — if the upstream union ever gains a value not
 * present here, this line stops compiling. The type alias is
 * intentionally unused at runtime; the leading underscore marks it as
 * a compile-time-only export.
 */
export type _assertClientErrorKindIsRecorderKind =
  TerminalClientError["kind"] extends LaunchTimingErrorKind ? true : never;

/**
 * One recorded event. `relativeMs` is the offset from `launch_started`
 * (0 for the anchor itself; never negative — the recorder rejects
 * negative deltas by clamping to 0 to defend against a misbehaving
 * `now()` override in tests).
 */
export interface LaunchTimingEvent {
  name: LaunchTimingEventName;
  relativeMs: number;
}

/**
 * Stable snapshot of the recorder state. Events are returned in the
 * order they were marked (deterministic across renderers and platforms);
 * `createPostOutcome` and `errorKind` are convenience fields the UI uses
 * without scanning the events list.
 */
export interface LaunchTimingSnapshot {
  events: LaunchTimingEvent[];
  createPostOutcome: LaunchTimingCreatePostOutcome | null;
  errorKind: LaunchTimingErrorKind | null;
}

/**
 * Construction options. `now()` is injectable for tests; production
 * callers omit it and the recorder falls back to `performance.now()`.
 */
export interface LaunchTimingRecorderOptions {
  now?: () => number;
}

/**
 * Time-source resolver. Avoids capturing `performance` at module load
 * so a non-browser test environment (vitest jsdom is fine, but a plain
 * node env would not be) can still construct the module.
 */
function defaultNow(): number {
  if (typeof performance !== "undefined" && typeof performance.now === "function") {
    return performance.now();
  }
  // Last-resort fallback. Wall-clock through Date.now is intentionally
  // converted to ms; the recorder treats every value as relative
  // anyway and we clamp negative deltas downstream.
  return Date.now();
}

/**
 * In-memory recorder for one launch attempt. Constructed by the launch
 * call site (typically `ServersView.launchProfile`); passed through
 * {@link import("./activeLaunch.js").ActiveLaunch} to the production
 * terminal workspace, which feeds it WebSocket / client events.
 *
 * Lifetime: one recorder per launch attempt. A reconnect from
 * `ProductionTerminal`'s Reconnect button does NOT spawn a fresh
 * recorder — the existing one keeps recording so the snapshot covers
 * "launch click → live → detach → reconnect → live" if that path is
 * exercised. A fresh navigation away + relaunch creates a new recorder
 * because `ActiveLaunch` itself is rebuilt.
 *
 * Construction marks `launch_started` immediately so every other event
 * has a well-defined anchor.
 */
export class LaunchTimingRecorder {
  readonly #now: () => number;
  readonly #anchorMs: number;
  readonly #events: LaunchTimingEvent[] = [];
  readonly #seen = new Set<LaunchTimingEventName>();
  readonly #listeners = new Set<(snapshot: LaunchTimingSnapshot) => void>();
  #createPostOutcome: LaunchTimingCreatePostOutcome | null = null;
  #errorKind: LaunchTimingErrorKind | null = null;

  constructor(options: LaunchTimingRecorderOptions = {}) {
    this.#now = options.now ?? defaultNow;
    this.#anchorMs = this.#now();
    // The anchor IS the first event so the snapshot always has at
    // least one row. Bypasses the dedupe set explicitly; the recorder
    // contract is "launch_started is always present".
    this.#events.push({ name: "launch_started", relativeMs: 0 });
    this.#seen.add("launch_started");
  }

  /**
   * Record an event. Returns the relative ms that was stored, or
   * `null` if the event was already recorded (one-shot rule).
   *
   * Refuses `"launch_started"` after construction (the anchor is set
   * exactly once). All other names are accepted in any order; the
   * snapshot preserves the call order.
   *
   * Emits the snapshot to listeners exactly once per call that
   * actually stored a new event.
   */
  mark(name: LaunchTimingEventName): number | null {
    const ms = this.#record(name);
    if (ms !== null) this.#emit();
    return ms;
  }

  /**
   * Record the outcome of `create_session_post_resolved`. Idempotent:
   * the FIRST outcome sticks. Marks the event automatically if it
   * has not been marked yet (so a caller can mark+outcome in one
   * helper call). Returns the relative ms that was stored, or `null`
   * if the event was already marked.
   *
   * Emits the snapshot to listeners EXACTLY ONCE per public call,
   * even when both the event and the outcome land in the same call.
   */
  markCreateSessionPostResolved(
    outcome: LaunchTimingCreatePostOutcome,
  ): number | null {
    const ms = this.#record("create_session_post_resolved");
    let changed = ms !== null;
    if (this.#createPostOutcome === null) {
      this.#createPostOutcome = outcome;
      changed = true;
    }
    if (changed) this.#emit();
    return ms;
  }

  /**
   * Record an error event with a closed-vocabulary kind. Idempotent:
   * the FIRST error kind sticks. Marks the `error` event automatically
   * if it has not been marked yet. Returns the relative ms that was
   * stored, or `null` if the event was already marked.
   *
   * Emits the snapshot to listeners EXACTLY ONCE per public call,
   * even when both the event and the kind land in the same call.
   */
  markError(kind: LaunchTimingErrorKind): number | null {
    const ms = this.#record("error");
    let changed = ms !== null;
    if (this.#errorKind === null) {
      this.#errorKind = kind;
      changed = true;
    }
    if (changed) this.#emit();
    return ms;
  }

  /**
   * Inner event-append that does NOT emit. Used by the three public
   * `mark*` methods so each public call emits exactly once even when
   * it changes multiple snapshot fields.
   */
  #record(name: LaunchTimingEventName): number | null {
    if (this.#seen.has(name)) return null;
    const delta = this.#now() - this.#anchorMs;
    // Clamp negative deltas to 0 — defends against a misbehaving
    // `now()` override in tests AND against `performance.now()` clock
    // skew across browser tabs. The signal we care about is monotonic
    // order, not sub-ms precision.
    const relativeMs = delta < 0 ? 0 : delta;
    this.#events.push({ name, relativeMs });
    this.#seen.add(name);
    return relativeMs;
  }

  /** Return the relative ms of a previously marked event, or null. */
  relativeMsFor(name: LaunchTimingEventName): number | null {
    const event = this.#events.find((e) => e.name === name);
    return event === undefined ? null : event.relativeMs;
  }

  /** Stable, defensively-copied snapshot of the current state. */
  snapshot(): LaunchTimingSnapshot {
    return {
      events: this.#events.map((e) => ({ name: e.name, relativeMs: e.relativeMs })),
      createPostOutcome: this.#createPostOutcome,
      errorKind: this.#errorKind,
    };
  }

  /**
   * Subscribe to snapshot updates. The listener is called once per
   * `mark` / `markCreateSessionPostResolved` / `markError` that
   * actually changed state. Returns an unsubscribe function.
   *
   * Listeners are invoked synchronously after the state change. A
   * listener that throws does NOT bring down other listeners; the
   * exception is swallowed because the recorder must not become a
   * vector for surfacing renderer-internal stack traces.
   */
  subscribe(
    listener: (snapshot: LaunchTimingSnapshot) => void,
  ): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  #emit(): void {
    const snapshot = this.snapshot();
    for (const listener of this.#listeners) {
      try {
        listener(snapshot);
      } catch {
        // Swallow — see subscribe() doc comment.
      }
    }
  }
}

/**
 * Stable label rendered next to each event in the production workspace
 * diagnostic strip. Closed vocabulary; pinned by
 * `tests/terminalLaunchTiming.test.ts` so a regression that broadens it
 * (e.g. "added the create-session URL for clarity") trips the suite.
 */
export const LAUNCH_TIMING_EVENT_LABELS: Record<LaunchTimingEventName, string> =
  {
    launch_started: "launch click",
    create_session_post_started: "POST /api/v1/terminal-sessions sent",
    create_session_post_resolved: "POST resolved",
    ws_connect_started: "WebSocket connect started",
    ws_open: "WebSocket open",
    first_server_message: "first server frame",
    first_output: "first Output frame",
    attached: "client attached",
    detach_requested: "Detach clicked",
    close_requested: "End session clicked",
    ws_close: "WebSocket close",
    error: "error",
  };

/**
 * Format a non-negative monotonic delta as an operator-readable cell.
 * Sub-second values get one decimal place ("123.4 ms"); ≥ 1 s values
 * collapse to integer seconds ("4.2 s") so a 90 s wait reads as "90.0 s"
 * instead of "90000.5 ms".
 */
export function formatRelativeMs(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  if (ms < 1000) return `${ms.toFixed(1)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}
