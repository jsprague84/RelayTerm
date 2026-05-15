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
  /**
   * Live `ResizeObserver` when the neutral `autofit` option is true and
   * the renderer is mounted. `null` otherwise (pre-mount, autofit-off,
   * post-dispose, or when the environment has no `ResizeObserver`).
   * Owned exclusively by this adapter — `terminal-core` and the
   * production shell stay renderer-neutral and never reach for it.
   */
  #autofitObserver: ResizeObserver | null = null;
  /**
   * Active rAF token used to coalesce a burst of `ResizeObserver`
   * callbacks into a single `FitAddon.fit()` call. `null` when no rAF
   * is queued. Cleared on dispose so a late callback that survives
   * `disconnect()` does not call into a torn-down `FitAddon`.
   */
  #pendingFitFrame: number | null = null;

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

    // Renderer-neutral autofit (`BaseTerminalRendererOptions.autofit`).
    // When enabled, install a `ResizeObserver` on the mount element and
    // re-run `FitAddon.fit()` on each container change. The fit fans
    // out through xterm's `onResize` synchronously, which the existing
    // single subscriber translates into the wire `resize` frame — no
    // new event channel. Coalesce with `requestAnimationFrame` so a
    // burst of observer callbacks during a drag does not thrash the
    // atlas. When the browser/runtime has no `ResizeObserver`
    // (test harness without the shim, ancient browser), the option
    // silently no-ops; `autofitActive()` reports the truth.
    if (this.#options.autofit === true) {
      this.#installAutofitObserver(element);
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
    // Tear down the autofit observer FIRST. A late observer callback
    // that survives `disconnect()` would otherwise race the FitAddon
    // teardown a few lines below; clearing both the observer and the
    // pending rAF token here guarantees no fit fires post-dispose.
    if (this.#autofitObserver !== null) {
      this.#autofitObserver.disconnect();
      this.#autofitObserver = null;
    }
    if (this.#pendingFitFrame !== null) {
      const cancel = (globalThis as { cancelAnimationFrame?: (id: number) => void })
        .cancelAnimationFrame;
      if (typeof cancel === "function") cancel(this.#pendingFitFrame);
      this.#pendingFitFrame = null;
    }
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

  /**
   * Report whether the renderer-neutral
   * `BaseTerminalRendererOptions.autofit` capability is genuinely wired
   * right now. `true` only while a live `ResizeObserver` + `FitAddon`
   * pair is observing the mount container. `false` pre-mount, after
   * dispose, when autofit was not requested, or in an environment
   * without `ResizeObserver`. Diagnostic-only — never reads or carries
   * payload bytes; fitting changes still flow through `onResize`.
   */
  autofitActive(): boolean {
    return this.#autofitObserver !== null;
  }

  #installAutofitObserver(element: HTMLElement): void {
    const Ctor = (
      globalThis as { ResizeObserver?: typeof ResizeObserver }
    ).ResizeObserver;
    if (typeof Ctor !== "function") {
      // No-op silently: the option is renderer-neutral intent; an
      // environment that cannot honour it is the same shape as a
      // renderer that no-ops autofit (ghostty-web / restty today).
      return;
    }
    const observer = new Ctor(() => {
      // The renderer was disposed between an observer queue-up and the
      // callback firing — bail without touching the addon.
      if (this.#disposed) return;
      this.#scheduleFit();
    });
    observer.observe(element);
    this.#autofitObserver = observer;
  }

  #scheduleFit(): void {
    if (this.#pendingFitFrame !== null) return;
    const raf = (globalThis as { requestAnimationFrame?: (cb: () => void) => number })
      .requestAnimationFrame;
    const runFit = () => {
      this.#pendingFitFrame = null;
      if (this.#disposed) return;
      const fit = this.#fitAddon;
      if (fit === null) return;
      try {
        fit.fit();
      } catch {
        // FitAddon.fit() can throw if the host element was detached
        // mid-resize — swallow to keep the redaction posture (no
        // payload bytes can surface) and to avoid escalating a
        // best-effort fit into a workspace error.
      }
    };
    if (typeof raf === "function") {
      this.#pendingFitFrame = raf(runFit);
    } else {
      // Fallback for environments without rAF: defer one microtask so
      // a burst of observer callbacks still coalesces.
      this.#pendingFitFrame = 0;
      queueMicrotask(runFit);
    }
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
