/**
 * Cross-view "active terminal launch" model.
 *
 * Lives at the shell level so that pressing "Launch terminal" inside
 * `ServersView` can switch to `TerminalView` AND hand off the session
 * id without a routing library. The shape is intentionally minimal —
 * everything else (last_seen_seq, replay state, attached/detached
 * phase) lives on `ProductionTerminal.svelte` for the duration of one
 * attachment.
 */
import type { LaunchTimingRecorder } from "./terminalLaunchTiming.js";

export interface ActiveLaunch {
  /** Backend `terminal_session.id` returned by `POST /api/v1/terminal-sessions`. */
  sessionId: string;
  /** Cell-grid columns the row was created with. */
  cols: number;
  /** Cell-grid rows the row was created with. */
  rows: number;
  /**
   * Operator-facing label. Derived at launch time from the originating
   * server profile (its `name`); just a hint for the workspace header.
   * The status line still falls back to the session id when omitted.
   */
  profileLabel?: string;
  /**
   * Replay bookmark to seed the next attach with. Set ONLY when the
   * launch came from the local active-session store
   * (`activeSessionStore.ts`) and the saved record carries a positive
   * `last_seen_seq`. A fresh launch from a profile row leaves this
   * unset; a reconnect from the Sessions list leaves it unset too —
   * the local store is the single producer of this hint.
   *
   * The production terminal seeds its `lastSeenSeq` state from this
   * value when present and passes it to the wire `attach` so the
   * backend's replay handshake covers the gap. The wire request itself
   * still gates on `lastSeenSeq > 0`, so a malformed `0` here collapses
   * to "no resume" rather than a wire-side error.
   */
  lastSeenSeq?: number;
  /**
   * Client-side launch-timing recorder for this launch attempt. In
   * memory only — never persisted, never serialised. The producer
   * (`ServersView.launchProfile` or the saved-session reconnect path)
   * has already marked `launch_started`, `create_session_post_started`,
   * and `create_session_post_resolved`; the production workspace
   * subscribes for the WebSocket / client events
   * (`ws_connect_started`, `ws_open`, `first_server_message`,
   * `first_output`, `attached`, `detach_requested`, `close_requested`,
   * `ws_close`, `error`).
   *
   * Optional because the saved-session-restore path in
   * `activeSessionStore.buildReconnectAttempt` cannot synthesize one
   * (the recorder is anchored on the click handler, not on the saved
   * record). The workspace tolerates the missing recorder and skips
   * the diagnostic strip.
   */
  timing?: LaunchTimingRecorder;
}
