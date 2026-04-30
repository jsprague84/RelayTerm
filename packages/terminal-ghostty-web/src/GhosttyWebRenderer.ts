/**
 * `GhosttyWebRenderer` — the ghostty-web-backed `TerminalRenderer` adapter.
 *
 * ghostty-web wraps Ghostty's libghostty-vt parser via WebAssembly and
 * exposes an xterm.js-API-compatible `Terminal` class. The shape match
 * means the adapter mirrors `@relayterm/terminal-xterm`'s `XtermRenderer`
 * almost line-for-line; the meaningful differences are:
 *
 *   1. `mount` is async because `init()` (one-time WASM load) must
 *      resolve before constructing a `Terminal`. The adapter caches the
 *      `init()` promise at module scope so multiple renderer instances
 *      share one WASM module load — re-initializing would be wasteful.
 *   2. `dispose()` may run during the awaited `init()`. The mount path
 *      re-checks the disposed flag after the await and refuses to open
 *      a `Terminal` into the user's DOM if disposal already happened.
 *   3. ghostty-web has no analogue for xterm's `lineHeight` option;
 *      `GhosttyWebRendererOptions.lineHeight` is accepted at the neutral
 *      surface (so an app can swap renderers without renaming options)
 *      and silently dropped during the option mapping.
 *
 * Lifecycle:
 *  - `new GhosttyWebRenderer(options)` — captures options. `write`
 *    before mount queues; the queue is flushed once mount resolves.
 *  - `await renderer.mount(element)` — ensures WASM init, constructs
 *    the ghostty-web `Terminal`, opens it, bridges `onData`/`onResize`.
 *  - `dispose()` — synchronous and idempotent. Safe to call before,
 *    during, or after `mount`. After dispose the renderer is dead;
 *    re-mount throws.
 *
 * Input-secrecy rule: identical to the xterm adapter's pin. Listener
 * payloads (real keystrokes once a PTY lands) are NEVER logged, thrown,
 * or otherwise surfaced beyond the consumer's `onInput` callback. The
 * file deliberately carries no `console.*` calls and never embeds input
 * bytes in errors. `terminal-ghostty-web/tests/ghosttyWebRenderer.test.ts`
 * pins this with a sentinel string the same way the xterm adapter does.
 *
 * `init()` is global to ghostty-web's module: it loads a shared WASM
 * instance which every `Terminal` reuses. The shared promise lives at
 * module scope and is awaited on every mount so that subsequent
 * mounts pay only the cached resolve.
 */
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
  Unsubscribe,
} from "@relayterm/terminal-core";
import { init as ghosttyInit, Terminal } from "ghostty-web";

import {
  toGhosttyOptions,
  type GhosttyWebRendererOptions,
} from "./options.js";

interface RendererResize {
  cols: number;
  rows: number;
}

type InputListener = (data: RendererInput) => void;
type ResizeListener = (size: RendererResize) => void;

/**
 * Module-scope cache of the `init()` promise. ghostty-web's `init()`
 * loads a shared WASM module the underlying `Terminal` instances reuse,
 * so each consumer should hit it at most once per page.
 *
 * Two indirections live here:
 *
 *  - `initFn` defaults to ghostty-web's exported `init`, but tests can
 *    replace it via `__setGhosttyInitForTesting`. This is more robust
 *    than relying on `vi.spyOn` against an ESM named export — a
 *    captured local binding (the `ghosttyInit` import above) is not
 *    affected by post-hoc property mutation on the module namespace
 *    object in strict-ESM consumers.
 *  - `initPromise` memoizes the awaited result so the second mount on
 *    a page pays only the cached resolve.
 *
 * Both seams are intentionally NOT re-exported from the package barrel
 * (`index.ts`) — they're reachable only via the relative path tests
 * use, so package consumers cannot reset shared WASM state at runtime.
 */
type GhosttyInit = () => Promise<void>;
let initFn: GhosttyInit = ghosttyInit;
let initPromise: Promise<void> | null = null;

function ensureGhosttyInit(): Promise<void> {
  if (initPromise === null) {
    initPromise = initFn();
  }
  return initPromise;
}

/**
 * Reset the cached `init()` promise. Test-only seam — the production
 * surface intentionally has no way to force a re-init, because doing so
 * would tear the shared WASM module out from under any live Terminal.
 */
export function __resetGhosttyInitPromiseForTesting(): void {
  initPromise = null;
}

/**
 * Replace the function used to load ghostty-web's WASM. Test-only seam
 * for exercising init-failure or stall paths without depending on the
 * fragile ESM-binding semantics of `vi.spyOn` against a static import.
 * Pass `null` to restore the default (ghostty-web's exported `init`).
 */
export function __setGhosttyInitForTesting(fn: GhosttyInit | null): void {
  initFn = fn ?? ghosttyInit;
  initPromise = null;
}

export class GhosttyWebRenderer implements TerminalRenderer {
  readonly #options: GhosttyWebRendererOptions;
  readonly #inputListeners = new Set<InputListener>();
  readonly #resizeListeners = new Set<ResizeListener>();
  #terminal: Terminal | null = null;
  #onDataDispose: { dispose(): void } | null = null;
  #onResizeDispose: { dispose(): void } | null = null;
  #pendingWrites: RendererOutput[] = [];
  #disposed = false;
  #mountStarted = false;

  constructor(options: GhosttyWebRendererOptions = {}) {
    this.#options = options;
  }

  async mount(element: HTMLElement): Promise<void> {
    if (this.#disposed) {
      throw new Error("GhosttyWebRenderer: cannot mount after dispose");
    }
    if (this.#mountStarted) {
      throw new Error("GhosttyWebRenderer: already mounted");
    }
    this.#mountStarted = true;

    await ensureGhosttyInit();

    // Re-check after the await: a synchronous `dispose()` during the
    // WASM load must NOT result in an open Terminal. Bail silently —
    // the caller's `dispose()` already returned.
    if (this.#disposed) return;

    const term = new Terminal(toGhosttyOptions(this.#options));
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
    // ghostty-web's `Terminal.dispose()` releases the WASM-backed
    // viewport buffer and renderer. The shared `init()` module stays
    // loaded; that's intentional — see file header.
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
