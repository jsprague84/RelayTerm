/**
 * `GhosttyWebRenderer` — the ghostty-web-backed `TerminalRenderer` adapter.
 *
 * ghostty-web wraps Ghostty's libghostty-vt parser via WebAssembly and
 * exposes an xterm.js-API-compatible `Terminal` class. The shape match
 * means the adapter mirrors `@relayterm/terminal-xterm`'s `XtermRenderer`
 * almost line-for-line; the meaningful differences are:
 *
 *   1. `mount` is async because the WASM module must be compiled and
 *      instantiated before constructing a `Terminal`. The adapter caches
 *      the loaded `Ghostty` instance promise at module scope so multiple
 *      renderer instances share one WASM load — re-loading would tear
 *      shared state out from under any live `Terminal`.
 *   2. `dispose()` may run during the awaited load. The mount path
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
 *  - `await renderer.mount(element)` — ensures the shared `Ghostty`
 *    instance is loaded, constructs the ghostty-web `Terminal` (passing
 *    the loaded instance via `options.ghostty` so the no-arg `init()`
 *    sugar — and its inlined `data:application/wasm;base64,…` URL — is
 *    never reached), opens it, bridges `onData`/`onResize`.
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
 * CSP / WASM posture (load-bearing — slice 2026-05-13b):
 *  - ghostty-web@0.4.0's no-arg `init()` loads its WASM from an inlined
 *    `data:application/wasm;base64,…` URL, which is incompatible with
 *    RelayTerm's production CSP (`default-src 'self'`, no `connect-src`
 *    override). The adapter sidesteps that path entirely by:
 *      (a) importing `ghostty-web/ghostty-vt.wasm?url` so Vite emits a
 *          fingerprinted same-origin asset and substitutes its URL at
 *          build time (see `./wasmUrl.ts`); and
 *      (b) calling `Ghostty.load(wasmUrl)` directly, then handing the
 *          loaded `Ghostty` instance into the `Terminal` constructor's
 *          `options.ghostty`. Upstream's `getGhostty()` global cache is
 *          never consulted, so the inline data URL is unreachable from
 *          RelayTerm's production bundle.
 *  - `WebAssembly.compile()` inside `Ghostty.loadFromPath` still
 *    requires `'wasm-unsafe-eval'` in the deployment's CSP `script-src`.
 *    That is upstream-baked and out of scope for this adapter slice.
 */
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
  Unsubscribe,
} from "@relayterm/terminal-core";
import { Ghostty, Terminal } from "ghostty-web";

import {
  toGhosttyOptions,
  type GhosttyWebRendererOptions,
} from "./options.js";
import { ghosttyWasmUrl } from "./wasmUrl.js";

interface RendererResize {
  cols: number;
  rows: number;
}

type InputListener = (data: RendererInput) => void;
type ResizeListener = (size: RendererResize) => void;

/**
 * Module-scope cache of the `Ghostty.load()` promise. The loaded WASM
 * module backs every `Terminal` instance the page constructs, so each
 * consumer should hit it at most once per page.
 *
 * Two indirections live here:
 *
 *  - `loaderFn` defaults to a thin wrapper around ghostty-web's
 *    `Ghostty.load`, but tests can replace it via
 *    `__setGhosttyLoaderForTesting`. This is more robust than relying
 *    on `vi.spyOn` against an ESM named export — a captured local
 *    binding (the `Ghostty` import above) is not affected by post-hoc
 *    property mutation on the module namespace object in strict-ESM
 *    consumers.
 *  - `loadPromise` memoizes the awaited result so the second mount on
 *    a page pays only the cached resolve.
 *
 * Both seams are intentionally NOT re-exported from the package barrel
 * (`index.ts`) — they're reachable only via the relative path tests
 * use, so package consumers cannot reset shared WASM state at runtime.
 */
type GhosttyLoader = (wasmUrl: string) => Promise<Ghostty>;
const defaultLoader: GhosttyLoader = (url) => Ghostty.load(url);
let loaderFn: GhosttyLoader = defaultLoader;
let loadPromise: Promise<Ghostty> | null = null;

function ensureGhostty(): Promise<Ghostty> {
  if (loadPromise === null) {
    loadPromise = loaderFn(ghosttyWasmUrl);
  }
  return loadPromise;
}

/**
 * Reset the cached `Ghostty.load()` promise. Test-only seam — the
 * production surface intentionally has no way to force a re-load,
 * because doing so would tear the shared WASM module out from under
 * any live Terminal.
 */
export function __resetGhosttyLoadPromiseForTesting(): void {
  loadPromise = null;
}

/**
 * Replace the function used to load ghostty-web's WASM. Test-only seam
 * for exercising load-failure or stall paths without depending on the
 * fragile ESM-binding semantics of `vi.spyOn` against a static import.
 * Pass `null` to restore the default (a wrapper over `Ghostty.load`).
 */
export function __setGhosttyLoaderForTesting(
  fn: GhosttyLoader | null,
): void {
  loaderFn = fn ?? defaultLoader;
  loadPromise = null;
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

    const ghostty = await ensureGhostty();

    // Re-check after the await: a synchronous `dispose()` during the
    // WASM load must NOT result in an open Terminal. Bail silently —
    // the caller's `dispose()` already returned.
    if (this.#disposed) return;

    // Pass the pre-loaded `Ghostty` instance through `options.ghostty`
    // so the `Terminal` constructor bypasses upstream's `getGhostty()`
    // global cache (and the no-arg `init()` path that fills it from the
    // inlined data URL). See file header for CSP rationale.
    const term = new Terminal({
      ...toGhosttyOptions(this.#options),
      ghostty,
    });
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

  /**
   * The element `focus()` targets — ghostty-web's contenteditable host
   * element (`Terminal.element`, the element passed to `mount`).
   * ghostty-web attaches its keydown listener to the host, NOT to the
   * hidden helper `<textarea>` it also creates (that textarea is for
   * IME / composition / paste only), so the host is the element a real
   * keystroke hits. `null` before mount and after dispose.
   *
   * Per the `TerminalRenderer` contract this is used only for focus +
   * a stable test selector — the element is never read for content and
   * input bytes still flow exclusively through `onInput`.
   */
  focusTarget(): HTMLElement | null {
    return this.#terminal?.element ?? null;
  }

  resize(cols: number, rows: number): void {
    this.#terminal?.resize(cols, rows);
  }

  /**
   * Report whether the renderer-neutral
   * `BaseTerminalRendererOptions.autofit` capability is genuinely wired.
   * ghostty-web has no container-fit / `ResizeObserver` path on its
   * public surface today, so this adapter accepts the option (for
   * cross-renderer parity at the call site) and returns `false`
   * honestly. The workspace mirrors the value onto
   * `data-renderer-autofit="unsupported"` when the operator enabled
   * autofit and `"off"` otherwise — operator-facing taxonomy only;
   * never carries payload bytes.
   *
   * Revisited when / if ghostty-web grows a container-observation path
   * upstream; until then the unsupported-but-accepted shape matches the
   * cosmetic-knob drop pattern this adapter already follows.
   */
  autofitActive(): boolean {
    return false;
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#onDataDispose?.dispose();
    this.#onResizeDispose?.dispose();
    this.#onDataDispose = null;
    this.#onResizeDispose = null;
    // ghostty-web's `Terminal.dispose()` releases the WASM-backed
    // viewport buffer and renderer. The shared `Ghostty` module stays
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
