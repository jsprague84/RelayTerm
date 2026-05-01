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
}
