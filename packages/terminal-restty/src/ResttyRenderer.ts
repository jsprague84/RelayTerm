/**
 * `ResttyRenderer` — the restty-backed `TerminalRenderer` adapter.
 *
 * restty (`https://github.com/wiedymi/restty`, npm `restty@0.1.x`) is a
 * browser terminal rendering library powered by libghostty-vt (WASM),
 * WebGPU/WebGL2, and TypeScript text shaping. The package exposes a
 * native `Restty` surface (panes / plugins / shader stages) and a focused
 * xterm.js compatibility shim under `restty/xterm`. We bind to the latter
 * because it is shape-compatible with the existing `XtermRenderer` /
 * `GhosttyWebRenderer` adapters: `open(parent)`, `write(data, cb?)`,
 * `resize(cols, rows)`, `focus()`, `onData(cb)`, `onResize(cb)`,
 * `dispose()`. Binding through the shim keeps the adapter a thin glue
 * layer and avoids leaking restty's pane/plugin surface into the
 * renderer-neutral interface.
 *
 * Lifecycle:
 *  - `new ResttyRenderer(options)` — captures options. `write` before
 *    mount queues; the queue is flushed once `mount` resolves.
 *  - `await renderer.mount(element)` — synchronously constructs the
 *    restty xterm-compat `Terminal`, opens it into the DOM, bridges
 *    `onData`/`onResize`. `mount` is `async` for parity with the
 *    `GhosttyWebRenderer` adapter (and to give restty room to grow into
 *    a future async init step without changing the adapter contract);
 *    today the `await` resolves on the same microtask. A synchronous
 *    `dispose()` issued during the awaited mount is checked again after
 *    the await — if disposal happened first the open is cancelled and
 *    no `Terminal` is constructed.
 *  - `dispose()` — synchronous and idempotent. Safe to call before,
 *    during, or after `mount`. After dispose the renderer is dead;
 *    re-mount throws.
 *
 * Browser/runtime caveats (documented adapter behavior, not regressions):
 *  - restty needs a real DOM (canvas, `document`) and either WebGPU or
 *    WebGL2. The xterm-compat shim does not expose a `setRenderer` knob
 *    of its own; the restty runtime auto-falls-back when WebGPU is
 *    unavailable. The adapter does NOT attempt to detect or report
 *    fallback — that's a restty concern.
 *  - The shipped restty payload is large (~3 MB JS plus inlined WASM).
 *    The package declares `sideEffects: false` so a production build
 *    that never reaches this adapter (today: any prod build, because
 *    the dev lab is gated on `import.meta.env.DEV`) tree-shakes it out.
 *  - `restty/xterm`'s `Terminal.write(data: string)` accepts strings
 *    only — the underlying call is `Restty.sendInput(data, "pty")`.
 *    `RendererOutput` carries `string | Uint8Array`, so `Uint8Array`
 *    payloads are UTF-8-decoded inside `write` before forwarding. UTF-8
 *    is the correct decoding for SSH PTY output. A future binary frame
 *    format is out of scope here; if non-UTF-8 bytes arrive (e.g. raw
 *    ESC sequences with C1 controls in 0x80..0xFF), `TextDecoder`
 *    replaces them with U+FFFD by default — that's the same behavior the
 *    other adapters' `write(Uint8Array)` ends up with under most
 *    consumers. The decode step is the single point of difference vs
 *    `XtermRenderer` / `GhosttyWebRenderer`, which forward the
 *    `Uint8Array` to xterm.js and ghostty-web respectively.
 *  - restty's xterm shim uses `console.error` to surface throwing
 *    listener callbacks (`emitWithGuard` in `restty/xterm.js`). The
 *    adapter's own `#fanoutInput` catch-all eats any exception thrown
 *    by a consumer-supplied input listener BEFORE restty sees it, so
 *    the restty shim never logs renderer input itself.
 *
 * Input-secrecy rule: identical to the xterm and ghostty-web adapters'
 * pin. Listener payloads (real keystrokes once a PTY lands) are NEVER
 * logged, thrown, or otherwise surfaced beyond the consumer's
 * `onInput` callback. The file deliberately carries no `console.*`
 * calls and never embeds input bytes in errors. The redaction rule is
 * pinned by `tests/resttyRenderer.test.ts` with the same sentinel-string
 * approach used by the sibling adapters.
 */
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
  Unsubscribe,
} from "@relayterm/terminal-core";
import { Terminal } from "restty/xterm";

import {
  toResttyOptions,
  type ResttyInitialGrid,
  type ResttyRendererOptions,
} from "./options.js";

interface RendererResize {
  cols: number;
  rows: number;
}

type InputListener = (data: RendererInput) => void;
type ResizeListener = (size: RendererResize) => void;

/**
 * Construction-time options for `ResttyRenderer`. The neutral renderer
 * options are merged with an optional initial cell grid because the
 * dev lab passes one on attach but the future production caller may
 * not know the grid until after layout — the field is optional.
 */
export type ResttyRendererCtorOptions = ResttyRendererOptions &
  ResttyInitialGrid;

/**
 * UTF-8 decoder used to turn `Uint8Array` writes into the `string`
 * payload `restty/xterm`'s `Terminal.write` accepts. Allocated once at
 * module scope because `TextDecoder` is stateless when called via
 * `decode(bytes)` (no `{ stream: true }`) — every call returns a fresh
 * string and the decoder can be shared across renderers and writes.
 *
 * Replacement-on-error is the default `TextDecoder` behaviour and is
 * intentional here: a malformed byte run becomes U+FFFD rather than an
 * exception, so a corrupt PTY frame cannot surface input bytes through
 * a thrown error.
 */
const RESTTY_OUTPUT_DECODER = new TextDecoder("utf-8");

export class ResttyRenderer implements TerminalRenderer {
  readonly #options: ResttyRendererOptions;
  readonly #initialGrid: ResttyInitialGrid;
  readonly #inputListeners = new Set<InputListener>();
  readonly #resizeListeners = new Set<ResizeListener>();
  #terminal: Terminal | null = null;
  #onDataDispose: { dispose(): void } | null = null;
  #onResizeDispose: { dispose(): void } | null = null;
  #pendingWrites: RendererOutput[] = [];
  #disposed = false;
  #mountStarted = false;

  constructor(options: ResttyRendererCtorOptions = {}) {
    const { cols, rows, ...rest } = options;
    this.#options = rest;
    this.#initialGrid = {};
    if (cols !== undefined) this.#initialGrid.cols = cols;
    if (rows !== undefined) this.#initialGrid.rows = rows;
  }

  async mount(element: HTMLElement): Promise<void> {
    if (this.#disposed) {
      throw new Error("ResttyRenderer: cannot mount after dispose");
    }
    if (this.#mountStarted) {
      throw new Error("ResttyRenderer: already mounted");
    }
    this.#mountStarted = true;

    // No async init required by `restty/xterm` today: the WASM/runtime
    // load happens lazily inside the underlying `Restty` instance when
    // `open` is called. The `await` here is a placeholder so the
    // adapter contract matches the ghostty-web adapter (and so a
    // future restty version that introduces an explicit init step does
    // not require an adapter-shape change). Re-check `#disposed` after
    // the await for the same reason the ghostty-web adapter does — a
    // synchronous `dispose()` between mount entry and this point must
    // cancel the open silently.
    await Promise.resolve();
    if (this.#disposed) return;

    const opts = toResttyOptions(this.#options, this.#initialGrid);
    const term = new Terminal(opts);
    term.open(element);

    this.#onDataDispose = term.onData((data: string) => {
      this.#fanoutInput(data);
    });
    this.#onResizeDispose = term.onResize((size: RendererResize) => {
      this.#fanoutResize(size);
    });

    this.#terminal = term;

    if (this.#pendingWrites.length > 0) {
      const queued = this.#pendingWrites;
      this.#pendingWrites = [];
      for (const chunk of queued) {
        this.#writeToTerminal(term, chunk);
      }
    }
  }

  write(data: RendererOutput): void {
    if (this.#disposed) return;
    if (!this.#terminal) {
      this.#pendingWrites.push(data);
      return;
    }
    this.#writeToTerminal(this.#terminal, data);
  }

  focus(): void {
    this.#terminal?.focus();
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
    // restty's xterm-compat `Terminal.dispose()` tears down the
    // underlying `Restty` instance (canvas, IME input, render loop,
    // pane manager). The restty WASM module itself stays loaded for
    // the page; that's intentional — see file header.
    this.#terminal?.dispose();
    this.#terminal = null;
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

  #writeToTerminal(term: Terminal, data: RendererOutput): void {
    if (typeof data === "string") {
      term.write(data);
      return;
    }
    // restty/xterm accepts only `string`; UTF-8-decode bytes before
    // forwarding. See file header for why UTF-8 with replacement is
    // the right default for SSH PTY output.
    term.write(RESTTY_OUTPUT_DECODER.decode(data));
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
