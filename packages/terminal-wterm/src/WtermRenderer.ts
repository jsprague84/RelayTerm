/**
 * `WtermRenderer` — the wterm-backed `TerminalRenderer` adapter.
 *
 * wterm (`https://wterm.dev`, npm `@wterm/dom@0.2.x` + `@wterm/core@0.2.x`)
 * is a DOM-rendered terminal emulator with a Zig+WASM core. The DOM
 * package (`@wterm/dom`) ships the `WTerm` orchestrator which wires the
 * `WasmBridge` (vt parser/state) to a CSS-themed grid renderer and a
 * textarea-backed `InputHandler`. The DOM rendering style makes wterm
 * the natural mobile/accessibility-oriented experiment in the renderer
 * lineup — text selection, IME composition, paste, and mobile soft
 * keyboards flow through the platform's native textarea input model
 * rather than a canvas/WebGPU surface.
 *
 * Lifecycle and DOM ownership (the part of wterm's API that's
 * different from xterm/ghostty-web/restty):
 *  - `WTerm`'s constructor takes the host element and **synchronously
 *    mutates it** — it appends a child `<div class="term-grid">`,
 *    adds the `wterm` class to the host, and attaches a `click`
 *    listener. `init()` is the async step that loads WASM and starts
 *    rendering. The adapter therefore defers BOTH construction and
 *    `init()` to `mount(element)`, so the host element is not touched
 *    before the caller hands it over.
 *  - `WTerm.write(data)` accepts `string | Uint8Array` directly; no
 *    UTF-8 decode step is needed (unlike `restty/xterm`).
 *  - `WTerm.resize(cols, rows)` synchronously calls back into the
 *    constructor-supplied `onResize(cols, rows)`. The adapter wires
 *    that callback to fan out to its own `onResize` listeners; the
 *    same single-source-of-truth rule the xterm/restty adapters
 *    document applies — manual resize controls must call
 *    `renderer.resize(...)` only and let the subscriber drive the
 *    wire frame. Calling `client.sendResize(...)` directly alongside
 *    `renderer.resize(...)` would double-fire.
 *  - `WTerm.destroy()` clears the host element's `innerHTML`,
 *    detaches the click listener, disconnects the (optional) internal
 *    `ResizeObserver`, and tears down the `InputHandler`. Idempotent:
 *    repeated `destroy()` is a no-op (the WTerm marks `_destroyed`
 *    early in the first call). `dispose()` on the adapter mirrors
 *    that idempotency and is safe to call before, during, or after
 *    `mount`.
 *  - `init()` may throw — the underlying `WasmBridge.load` error path
 *    rethrows with a `[wterm] Failed to load WASM from ${url}: …`
 *    message that includes the WASM URL but never any terminal input
 *    or output (init runs before any data flows). The adapter still
 *    rethrows with a static message because that's the redaction
 *    rule the sibling adapters follow uniformly.
 *
 * Async-mount-vs-dispose race: the awaited `init()` may resolve after
 * a synchronous `dispose()`. The mount path re-checks the disposed
 * flag after the await; if disposal happened first, `WTerm.destroy()`
 * is called immediately on the just-constructed instance and no
 * pending writes are flushed. Same shape as the ghostty-web adapter.
 *
 * Pre-mount queueing:
 *  - `write(data)` before mount queues the chunk; the queue is
 *    drained in order once `mount` completes.
 *  - `resize(cols, rows)` before mount caches the latest pair (only
 *    the latest matters; an interim resize during boot is dead).
 *    The cached pair is applied once `mount` completes.
 *  - `focus()` before mount is a no-op (no DOM element to focus).
 *
 * Input-secrecy rule: identical to the xterm, ghostty-web, and restty
 * adapters' pin. Listener payloads (real keystrokes once a PTY lands)
 * are NEVER logged, thrown, or otherwise surfaced beyond the
 * consumer's `onInput` callback. The file deliberately carries no
 * `console.*` calls and never embeds input bytes in errors. Init
 * failures are rethrown with a static message even though the
 * underlying error does not embed input bytes — defence in depth.
 *
 * Browser/runtime caveats (documented adapter behaviour, not
 * regressions):
 *  - wterm needs a real DOM (`document`, `requestAnimationFrame`,
 *    `ResizeObserver` if `autoResize` is on, `getComputedStyle`,
 *    `getBoundingClientRect`). Tests run against a mocked `WTerm`
 *    rather than a real DOM; real-browser smoke goes through the
 *    dev lab.
 *  - DOM rendering means selection, copy, paste, and mobile soft
 *    keyboards interact with the browser's native text-handling
 *    primitives. This is intentional and is the entire reason
 *    wterm is the mobile/accessibility-oriented experiment. The
 *    flip side: a font / theme / cursor-style change goes through
 *    the `.wterm` CSS host (custom properties + theme-named modifier
 *    classes from `@wterm/dom/src/terminal.css`), not the
 *    `WTermOptions` bag. The neutral cosmetic options the adapter
 *    accepts are silently dropped — see `options.ts`.
 *  - `@wterm/core` inlines the WASM as a base64 module
 *    (`WASM_BASE64` in `wasm-inline.js`, ~17 KB), so no separate
 *    asset wiring is needed under Vite. The `wtermOnly.wasmUrl`
 *    knob exists only for callers who want to serve the WASM as a
 *    separate fetch'd asset (none today).
 */
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
  Unsubscribe,
} from "@relayterm/terminal-core";
import { WTerm, type WTermOptions } from "@wterm/dom";

import {
  toWtermOptions,
  type WtermInitialGrid,
  type WtermRendererOptions,
} from "./options.js";

interface RendererResize {
  cols: number;
  rows: number;
}

/**
 * Structural view of the wterm internals `focusTarget()` reaches for.
 *
 * `WTerm` keeps its `InputHandler` on a `private input` field, and the
 * `InputHandler` keeps the hidden keyboard `<textarea>` on a `private
 * textarea` field — neither is on `@wterm/dom`'s public `.d.ts`
 * surface, but both exist at runtime. That textarea is the element
 * wterm appends to the host, attaches its `keydown`/`paste`/IME
 * listeners to, and the one `WTerm.focus()` ultimately focuses (via
 * `InputHandler.focus()`). This adapter is the single place in the
 * repo that knows wterm internals (see the file header), so the narrow
 * structural cast is contained here rather than leaking upward.
 */
interface WtermInputInternals {
  input: { textarea: HTMLTextAreaElement } | null;
}

type InputListener = (data: RendererInput) => void;
type ResizeListener = (size: RendererResize) => void;

/**
 * Construction-time options for `WtermRenderer`. The neutral renderer
 * options are merged with an optional initial cell grid because the
 * dev lab passes one on attach but the future production caller may
 * not know the grid until after layout — the field is optional.
 */
export type WtermRendererCtorOptions = WtermRendererOptions & WtermInitialGrid;

export class WtermRenderer implements TerminalRenderer {
  readonly #options: WtermRendererOptions;
  readonly #initialGrid: WtermInitialGrid;
  readonly #inputListeners = new Set<InputListener>();
  readonly #resizeListeners = new Set<ResizeListener>();
  #wterm: WTerm | null = null;
  #pendingWrites: RendererOutput[] = [];
  /**
   * Latest `(cols, rows)` requested before `mount` resolved. Only the
   * most recent value is applied: an interim resize during boot is
   * dead by the time the post-mount apply runs.
   */
  #pendingResize: RendererResize | null = null;
  #disposed = false;
  #mountStarted = false;

  constructor(options: WtermRendererCtorOptions = {}) {
    const { cols, rows, ...rest } = options;
    this.#options = rest;
    this.#initialGrid = {};
    if (cols !== undefined) this.#initialGrid.cols = cols;
    if (rows !== undefined) this.#initialGrid.rows = rows;
  }

  async mount(element: HTMLElement): Promise<void> {
    if (this.#disposed) {
      throw new Error("WtermRenderer: cannot mount after dispose");
    }
    if (this.#mountStarted) {
      throw new Error("WtermRenderer: already mounted");
    }
    this.#mountStarted = true;

    const mapped = toWtermOptions(this.#options, this.#initialGrid);
    const wtermOpts: WTermOptions = {
      // wterm's `WTermOptions` declares optional callbacks; assign via
      // distinct `onData`/`onResize` so the adapter remains the only
      // observer of input/resize fans. `onTitle` is intentionally not
      // wired — see file header.
      onData: (data: string) => {
        this.#fanoutInput(data);
      },
      onResize: (cols: number, rows: number) => {
        this.#fanoutResize({ cols, rows });
      },
      autoResize: mapped.autoResize,
    };
    if (mapped.cols !== undefined) wtermOpts.cols = mapped.cols;
    if (mapped.rows !== undefined) wtermOpts.rows = mapped.rows;
    if (mapped.cursorBlink !== undefined) {
      wtermOpts.cursorBlink = mapped.cursorBlink;
    }
    if (mapped.wasmUrl !== undefined) wtermOpts.wasmUrl = mapped.wasmUrl;
    if (mapped.debug !== undefined) wtermOpts.debug = mapped.debug;

    const wterm = new WTerm(element, wtermOpts);

    try {
      await wterm.init();
    } catch {
      // `init()` failure leaves the WTerm in a destroyed state already
      // (its own catch calls `destroy()` before rethrowing). Rethrow
      // with a static message regardless: the redaction rule says we
      // never propagate strings that the underlying library produced
      // unaudited, even though the failure path here predates any
      // terminal data flow.
      this.#wterm = null;
      throw new Error("WtermRenderer: failed to initialize wterm");
    }

    // Re-check after the async init: a synchronous `dispose()` during
    // the WASM load must NOT result in a live WTerm. Tear it down
    // immediately and bail. We deliberately do not flush the pending
    // queue afterwards — the renderer is dead.
    if (this.#disposed) {
      wterm.destroy();
      return;
    }

    this.#wterm = wterm;

    if (this.#pendingResize !== null) {
      const { cols, rows } = this.#pendingResize;
      this.#pendingResize = null;
      wterm.resize(cols, rows);
    }

    if (this.#pendingWrites.length > 0) {
      const queued = this.#pendingWrites;
      this.#pendingWrites = [];
      for (const chunk of queued) {
        wterm.write(chunk);
      }
    }
  }

  write(data: RendererOutput): void {
    if (this.#disposed) return;
    if (!this.#wterm) {
      this.#pendingWrites.push(data);
      return;
    }
    // `WTerm.write` accepts both `string` and `Uint8Array` directly;
    // no UTF-8 decode step is needed.
    this.#wterm.write(data);
  }

  focus(): void {
    // Pre-mount is a silent no-op: there is no DOM element to focus
    // until `mount` resolves and the WTerm is owned.
    this.#wterm?.focus();
  }

  /**
   * The element `focus()` ultimately targets — wterm's hidden keyboard
   * `<textarea>` (`InputHandler.textarea`), a child of the mount
   * element. wterm attaches its `keydown` listener to this textarea and
   * `WTerm.focus()` delegates to `InputHandler.focus()` which focuses
   * it, so it is the element a real keystroke hits. `null` before mount
   * (no WTerm yet), after dispose, and after a dispose that raced a
   * pending `init()` (the WTerm was destroyed and never adopted).
   *
   * Per the `TerminalRenderer` contract this is used only for focus +
   * a stable test selector — the textarea is never read for content
   * and input bytes still flow exclusively through `onInput`.
   */
  focusTarget(): HTMLElement | null {
    if (this.#wterm === null) return null;
    const input = (this.#wterm as unknown as WtermInputInternals).input;
    return input?.textarea ?? null;
  }

  resize(cols: number, rows: number): void {
    if (this.#disposed) return;
    if (!this.#wterm) {
      this.#pendingResize = { cols, rows };
      return;
    }
    // `WTerm.resize` synchronously fires the `onResize` callback the
    // adapter installed in `mount`, which fans out to subscribers.
    // The single-source-of-truth rule (manual resize controls must
    // call `renderer.resize(...)` only) holds the same way as in the
    // xterm/restty adapters.
    this.#wterm.resize(cols, rows);
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    // `WTerm.destroy()` is idempotent on the wterm side (it sets
    // `_destroyed = true` early). The adapter's `#disposed` guard
    // makes our own `dispose()` idempotent independently of wterm.
    this.#wterm?.destroy();
    this.#wterm = null;
    this.#pendingWrites.length = 0;
    this.#pendingResize = null;
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
