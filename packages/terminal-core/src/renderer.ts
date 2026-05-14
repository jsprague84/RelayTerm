/**
 * Renderer-neutral terminal abstraction.
 *
 * The renderer interface is the integration seam between
 * `TerminalSessionClient` and a concrete drawing backend
 * (xterm.js, libghostty-vt, restty, wterm, canvas, WebGPU, native Tauri).
 * No method here may assume any of those exist. Anything renderer-shaped
 * that has to leak into the protocol/client surface gets pushed back
 * into the renderer package, never up.
 *
 * A renderer is allowed to:
 *  - own a DOM element and draw into it
 *  - capture user input and emit it via `onInput`
 *  - report cell-grid resize via the `onResize` hook (optional; the
 *    client can also drive resize from above)
 *
 * A renderer is NOT allowed to:
 *  - persist any state across a `dispose`/`mount` cycle
 *  - decide auth, replay policy, or sequence numbering
 *  - assume a single live socket or a stable connection
 */

import type { Unsubscribe } from "./events.js";

/** Output bytes the orchestrator hands to a renderer. */
export type RendererOutput = string | Uint8Array;

/** User-driven input the renderer captures and forwards to the client. */
export type RendererInput = string | Uint8Array;

export interface TerminalRenderer {
  /**
   * Mount into the given element. Returning a Promise is allowed for
   * renderers that load asynchronously (WASM, addon initialization).
   */
  mount(element: HTMLElement): void | Promise<void>;
  /** Write raw output bytes/text. Must be safe to call before mount. */
  write(data: RendererOutput): void;
  /** Move browser focus into the renderer surface. */
  focus(): void;
  /**
   * Optional: the DOM element that `focus()` moves browser focus to —
   * the element that actually receives keyboard input. For the
   * xterm.js adapter this is xterm's hidden helper `<textarea>`; for
   * the ghostty-web adapter it is the contenteditable host element
   * (ghostty-web attaches its keydown listener to the host, not to a
   * helper textarea). Returns `null` before mount, after dispose, or
   * for a renderer that does not expose a single stable input element.
   *
   * This exists so a consumer can stamp a stable, renderer-neutral
   * test-selector on the element a real keystroke hits — the four
   * adapters disagree on whether that element is a child textarea or
   * the viewport host itself, which made the production-shell
   * renderer-evaluation smoke unable to target input fairly across
   * renderers. The element is used ONLY for focus + selector purposes;
   * it is NEVER read for content and never carries payload bytes —
   * user input still flows exclusively through `onInput`.
   */
  focusTarget?(): HTMLElement | null;
  /** Update the visible cell grid. Caller still drives wire `resize`. */
  resize(cols: number, rows: number): void;
  /** Tear down. Must release all listeners and DOM/WebGL resources. */
  dispose(): void;
  /** Subscribe to user-driven input events from the renderer. */
  onInput(cb: (data: RendererInput) => void): Unsubscribe;
  /**
   * Optional: subscribe to renderer-driven cell-grid resize. Renderers
   * that don't track their own size can omit this; the client treats a
   * missing implementation as "the caller drives resize."
   */
  onResize?(cb: (size: { cols: number; rows: number }) => void): Unsubscribe;
}
