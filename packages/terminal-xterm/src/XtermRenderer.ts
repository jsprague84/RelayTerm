/**
 * `XtermRenderer` — the xterm.js-backed `TerminalRenderer` adapter.
 *
 * The class is the only place in the repo that imports `@xterm/xterm`
 * directly. Every other package (the terminal-core protocol/client, the
 * web app, future Tauri shells) talks to xterm through this adapter and
 * the `TerminalRenderer` interface in `@relayterm/terminal-core`.
 *
 * Lifecycle:
 *  - `new XtermRenderer(options)` — captures options, defers all DOM
 *    work to `mount`. Calling `write` before `mount` is allowed and
 *    queues; the queue is flushed on mount.
 *  - `mount(element)` — constructs the xterm `Terminal`, wires fit /
 *    web-links addons, opens it inside `element`, and bridges xterm's
 *    `onData`/`onResize` events into our renderer-neutral listeners.
 *  - `dispose()` — idempotent; tears down xterm + addons + listeners.
 *
 * Input-secrecy rule: `onData` payloads (real keystrokes once a PTY
 * lands) must NEVER be logged, thrown, or stashed beyond the user's
 * own `onInput` callback. This file deliberately carries no
 * `console.log` of input data, no inclusion of input bytes in error
 * messages, and no debug buffer beyond the parser queue xterm itself
 * keeps internally.
 *
 * Styles are NOT imported here on purpose: a Node-only consumer (vitest,
 * future SSR) can't resolve a `.css` import. Browser consumers should
 * `import "@relayterm/terminal-xterm/styles"` once at app boot.
 */
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
  Unsubscribe,
} from "@relayterm/terminal-core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";

import {
  toXtermOptions,
  type XtermRendererOptions,
} from "./options.js";

/**
 * Shape of a renderer-neutral resize event. Mirrors what `TerminalRenderer.onResize`
 * in `terminal-core` already accepts; redefined locally to avoid pulling
 * a private type alias across the package boundary.
 */
interface RendererResize {
  cols: number;
  rows: number;
}

type InputListener = (data: RendererInput) => void;
type ResizeListener = (size: RendererResize) => void;

export class XtermRenderer implements TerminalRenderer {
  readonly #options: XtermRendererOptions;
  readonly #inputListeners = new Set<InputListener>();
  readonly #resizeListeners = new Set<ResizeListener>();
  #terminal: Terminal | null = null;
  #fitAddon: FitAddon | null = null;
  #linksAddon: WebLinksAddon | null = null;
  #onDataDispose: { dispose(): void } | null = null;
  #onResizeDispose: { dispose(): void } | null = null;
  #pendingWrites: RendererOutput[] = [];
  #disposed = false;

  constructor(options: XtermRendererOptions = {}) {
    this.#options = options;
  }

  mount(element: HTMLElement): void {
    if (this.#disposed) {
      throw new Error("XtermRenderer: cannot mount after dispose");
    }
    if (this.#terminal) {
      // Re-mount is not allowed by the renderer contract; signal loudly
      // rather than silently re-attaching to a different element.
      throw new Error("XtermRenderer: already mounted");
    }

    const term = new Terminal(toXtermOptions(this.#options));
    const fit = new FitAddon();
    const links = new WebLinksAddon();
    term.loadAddon(fit);
    term.loadAddon(links);
    term.open(element);

    this.#onDataDispose = term.onData((data: string) => {
      this.#fanoutInput(data);
    });
    this.#onResizeDispose = term.onResize((size: RendererResize) => {
      this.#fanoutResize(size);
    });

    this.#terminal = term;
    this.#fitAddon = fit;
    this.#linksAddon = links;

    if (this.#pendingWrites.length > 0) {
      const queued = this.#pendingWrites;
      this.#pendingWrites = [];
      for (const chunk of queued) {
        term.write(chunk);
      }
    }
  }

  write(data: RendererOutput): void {
    if (this.#disposed) return;
    if (!this.#terminal) {
      this.#pendingWrites.push(data);
      return;
    }
    this.#terminal.write(data);
  }

  focus(): void {
    this.#terminal?.focus();
  }

  /**
   * The element `focus()` targets — xterm's hidden helper `<textarea>`
   * (`Terminal.textarea`), a child of the mount element. xterm attaches
   * its keydown listener to this textarea, so it is the element a real
   * keystroke hits. `null` before mount and after dispose.
   *
   * Per the `TerminalRenderer` contract this is used only for focus +
   * a stable test selector — the textarea is never read for content
   * and input bytes still flow exclusively through `onInput`.
   */
  focusTarget(): HTMLElement | null {
    return this.#terminal?.textarea ?? null;
  }

  resize(cols: number, rows: number): void {
    this.#terminal?.resize(cols, rows);
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#onDataDispose?.dispose();
    this.#onResizeDispose?.dispose();
    this.#onDataDispose = null;
    this.#onResizeDispose = null;
    // `term.dispose()` cleans up addons attached via `loadAddon`, so
    // we don't need to dispose `fit`/`links` manually.
    this.#terminal?.dispose();
    this.#terminal = null;
    this.#fitAddon = null;
    this.#linksAddon = null;
    this.#pendingWrites.length = 0;
    this.#inputListeners.clear();
    this.#resizeListeners.clear();
  }

  onInput(cb: InputListener): Unsubscribe {
    this.#inputListeners.add(cb);
    return () => {
      this.#inputListeners.delete(cb);
    };
  }

  onResize(cb: ResizeListener): Unsubscribe {
    this.#resizeListeners.add(cb);
    return () => {
      this.#resizeListeners.delete(cb);
    };
  }

  /**
   * Re-fit the terminal to its container. Callers should also issue a
   * `resize` frame to the backend so russh resizes the PTY — the
   * renderer cannot do that itself. Returns the post-fit cell-grid
   * dimensions for that follow-up call (or `null` if the addon
   * declined to fit, e.g. before `mount`).
   */
  fit(): RendererResize | null {
    if (!this.#terminal || !this.#fitAddon) return null;
    this.#fitAddon.fit();
    return { cols: this.#terminal.cols, rows: this.#terminal.rows };
  }

  /**
   * Clear the LOCAL viewport and scrollback. This affects the renderer
   * surface only — no wire frame is sent, the backend's replay buffer
   * is untouched, and the remote shell is not asked to run `clear`.
   * Safe before mount and after dispose (no-op).
   */
  clear(): void {
    this.#terminal?.clear();
  }

  #fanoutInput(data: RendererInput): void {
    for (const listener of [...this.#inputListeners]) {
      try {
        listener(data);
      } catch {
        // A misbehaving listener must not interrupt sibling listeners
        // and — critically — must not surface input bytes through any
        // thrown error. Swallow without logging.
      }
    }
  }

  #fanoutResize(size: RendererResize): void {
    for (const listener of [...this.#resizeListeners]) {
      try {
        listener(size);
      } catch {
        // Same rationale as #fanoutInput.
      }
    }
  }
}
