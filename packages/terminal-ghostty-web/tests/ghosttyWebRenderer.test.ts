import { describe, expect, it, vi, beforeEach } from "vitest";
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
} from "@relayterm/terminal-core";
// Type-only reference to upstream `Ghostty`. `vi.mock("ghostty-web")`
// rewrites the runtime module, but the `type` import keeps tsc looking
// at the real declarations from `node_modules/ghostty-web` so the
// `__setGhosttyLoaderForTesting` parameter type lines up at the type
// level.
import type { Ghostty as RealGhostty } from "ghostty-web";

/**
 * `ghostty-web` is mocked so the adapter can be exercised outside a
 * browser. The mock tracks `Ghostty.load()` calls (one-shot WASM load),
 * the options the `Terminal` constructor saw, write payloads,
 * focus/resize dispositions, and the `onData`/`onResize` listener fans.
 *
 * The adapter is the only place in the repo that imports `ghostty-web`;
 * tests reach for `__resetGhosttyLoadPromiseForTesting` /
 * `__setGhosttyLoaderForTesting` (internal test seams) so each test
 * starts with a fresh module-scope load cache and can stall / replace
 * the loader without depending on the fragile ESM-binding semantics of
 * `vi.spyOn` against a static `Ghostty` import.
 */
interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
}

function defer<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const hoisted = vi.hoisted(() => {
  type FakeListener<T> = (arg: T) => void;
  type FakeOnResizeArg = { cols: number; rows: number };

  const loadCalls: {
    count: number;
    lastWasmUrl: string | undefined;
  } = {
    count: 0,
    lastWasmUrl: undefined,
  };

  /**
   * A throwaway stub the adapter and the FakeTerminal don't introspect.
   * The real `Ghostty` instance exposes WASM-bound methods (`exports`,
   * `createTerminal`, etc.); the adapter only ever passes the instance
   * through `Terminal({ ghostty })` and never calls anything on it
   * directly, so a sentinel object is sufficient.
   */
  const fakeGhosttyInstance = { __fakeGhostty: true } as const;

  class FakeGhostty {
    static load(wasmUrl?: string) {
      loadCalls.count += 1;
      loadCalls.lastWasmUrl = wasmUrl;
      return Promise.resolve(fakeGhosttyInstance);
    }
  }

  class FakeTerminal {
    static instances: FakeTerminal[] = [];
    options: unknown;
    cols = 0;
    rows = 0;
    writes: Array<string | Uint8Array> = [];
    focused = false;
    disposed = false;
    opened = false;
    dataListeners = new Set<FakeListener<string>>();
    resizeListeners = new Set<FakeListener<FakeOnResizeArg>>();
    /**
     * Mirrors ghostty-web's public `Terminal.element` — the
     * contenteditable host element ghostty-web wires its keydown
     * listener to (NOT the hidden helper textarea). Real ghostty-web
     * sets it to the element passed to `open()`; the fake does the same
     * so `focusTarget()` can be exercised without a browser.
     */
    element: HTMLElement | undefined;

    constructor(options: unknown) {
      this.options = options;
      FakeTerminal.instances.push(this);
    }

    open(el: HTMLElement) {
      this.opened = true;
      this.element = el;
    }
    onData(cb: FakeListener<string>) {
      this.dataListeners.add(cb);
      return {
        dispose: () => {
          this.dataListeners.delete(cb);
        },
      };
    }
    onResize(cb: FakeListener<FakeOnResizeArg>) {
      this.resizeListeners.add(cb);
      return {
        dispose: () => {
          this.resizeListeners.delete(cb);
        },
      };
    }
    write(data: string | Uint8Array, cb?: () => void) {
      this.writes.push(data);
      cb?.();
    }
    focus() {
      this.focused = true;
    }
    resize(cols: number, rows: number) {
      this.cols = cols;
      this.rows = rows;
      for (const l of [...this.resizeListeners]) l({ cols, rows });
    }
    dispose() {
      this.disposed = true;
    }
    emitData(data: string) {
      for (const l of [...this.dataListeners]) l(data);
    }
  }

  return { FakeGhostty, FakeTerminal, fakeGhosttyInstance, loadCalls };
});

vi.mock("ghostty-web", () => ({
  Ghostty: hoisted.FakeGhostty,
  Terminal: hoisted.FakeTerminal,
}));

/**
 * The adapter imports its WASM asset URL via Vite's `?url` suffix from
 * `ghostty-web/ghostty-vt.wasm?url`. Vitest's Vite layer can resolve
 * that in this repo without a mock (the real file exists in
 * `node_modules`), but mocking the wrapper module keeps the unit test
 * hermetic and lets the assertion below pin that the asset-URL string
 * actually reaches the loader (vs. an `undefined` regression that would
 * fall back to upstream's inlined data URL).
 *
 * The literal is duplicated between the mock factory and the
 * `FAKE_WASM_URL` constant below because `vi.mock` is hoisted above
 * top-level `const` declarations — the factory can't close over a
 * symbol that hasn't been initialized at hoist time.
 */
vi.mock("../src/wasmUrl.js", () => ({
  ghosttyWasmUrl: "/test-assets/ghostty-vt.wasm",
}));
const FAKE_WASM_URL = "/test-assets/ghostty-vt.wasm";

import {
  GhosttyWebRenderer,
  type GhosttyWebRendererOptions,
} from "../src/index.js";
// Adapter-internal helpers reached via the relative module so the
// public API surface stays renderer-neutral. The two `__...ForTesting`
// seams below are deliberately NOT re-exported from the barrel; they
// must remain unreachable from package consumers.
import {
  __resetGhosttyLoadPromiseForTesting,
  __setGhosttyLoaderForTesting,
} from "../src/GhosttyWebRenderer.js";
import { toGhosttyOptions, toGhosttyTheme } from "../src/options.js";

const { FakeTerminal, fakeGhosttyInstance, loadCalls } = hoisted;
const stubElement = {} as unknown as HTMLElement;

beforeEach(() => {
  FakeTerminal.instances.length = 0;
  loadCalls.count = 0;
  loadCalls.lastWasmUrl = undefined;
  // Restore both the cached load promise and the loader function in
  // case a test left the stalled-load seam in place.
  __setGhosttyLoaderForTesting(null);
  __resetGhosttyLoadPromiseForTesting();
});

/**
 * Sentinel input used by tests asserting the redaction rule. If it
 * ever appears in a thrown error, console call, or stored options
 * blob, the redaction pin in `GhosttyWebRenderer` has regressed.
 */
const SECRET_INPUT = "RELAY_SECRET_KEYS_SHOULD_NEVER_LEAK";

describe("toGhosttyOptions / toGhosttyTheme", () => {
  it("forwards only fields the caller actually set", () => {
    expect(toGhosttyOptions({})).toEqual({});
  });

  it("maps neutral knobs onto ghostty-web option names", () => {
    const opts: GhosttyWebRendererOptions = {
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      cursorStyle: "bar",
      cursorBlink: true,
      scrollbackLines: 5000,
    };
    expect(toGhosttyOptions(opts)).toEqual({
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      cursorStyle: "bar",
      cursorBlink: true,
      scrollback: 5000,
    });
  });

  it("renames scrollbackLines → scrollback", () => {
    expect(toGhosttyOptions({ scrollbackLines: 12 })).toEqual({
      scrollback: 12,
    });
  });

  it("silently drops lineHeight (no ghostty-web analogue)", () => {
    expect(toGhosttyOptions({ lineHeight: 1.4 })).toEqual({});
  });

  it("silently drops the renderer-neutral autofit option (no analogue today)", () => {
    // ghostty-web has no container-fit / ResizeObserver path; the
    // renderer-neutral `autofit` option is accepted on the public
    // surface for cross-renderer parity and dropped here so it does
    // not leak into the underlying `ITerminalOptions` blob.
    expect(toGhosttyOptions({ autofit: true })).toEqual({});
    expect(toGhosttyOptions({ autofit: false })).toEqual({});
  });

  it("forwards a full ANSI palette via toGhosttyTheme", () => {
    const theme = toGhosttyTheme({
      background: "#000",
      foreground: "#fff",
      cursor: "#f0f",
      selectionBackground: "#0ff",
      black: "#000",
      red: "#f00",
      green: "#0f0",
      yellow: "#ff0",
      blue: "#00f",
      magenta: "#f0f",
      cyan: "#0ff",
      white: "#ddd",
      brightBlack: "#444",
      brightRed: "#f44",
      brightGreen: "#4f4",
      brightYellow: "#ff4",
      brightBlue: "#44f",
      brightMagenta: "#f4f",
      brightCyan: "#4ff",
      brightWhite: "#fff",
    });
    expect(theme).toEqual({
      background: "#000",
      foreground: "#fff",
      cursor: "#f0f",
      selectionBackground: "#0ff",
      black: "#000",
      red: "#f00",
      green: "#0f0",
      yellow: "#ff0",
      blue: "#00f",
      magenta: "#f0f",
      cyan: "#0ff",
      white: "#ddd",
      brightBlack: "#444",
      brightRed: "#f44",
      brightGreen: "#4f4",
      brightYellow: "#ff4",
      brightBlue: "#44f",
      brightMagenta: "#f4f",
      brightCyan: "#4ff",
      brightWhite: "#fff",
    });
  });

  it("`ghosttyOnly` escape hatch overrides the portable mapping", () => {
    const mapped = toGhosttyOptions({
      fontSize: 14,
      ghosttyOnly: { fontSize: 18, convertEol: true },
    });
    expect(mapped).toEqual({ fontSize: 18, convertEol: true });
  });
});

describe("GhosttyWebRenderer satisfies TerminalRenderer", () => {
  it("constructs without an element and defers DOM work to mount", () => {
    const renderer: TerminalRenderer = new GhosttyWebRenderer({ fontSize: 14 });
    expect(FakeTerminal.instances).toHaveLength(0);
    expect(loadCalls.count).toBe(0);
    void renderer.write("noop before mount");
  });

  it("calls Ghostty.load exactly once across multiple mounts", async () => {
    const a = new GhosttyWebRenderer();
    const b = new GhosttyWebRenderer();
    await a.mount(stubElement);
    await b.mount(stubElement);
    expect(loadCalls.count).toBe(1);
    expect(FakeTerminal.instances).toHaveLength(2);
  });

  it("loads the WASM via the Vite-emitted same-origin asset URL", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    expect(loadCalls.count).toBe(1);
    expect(loadCalls.lastWasmUrl).toBe(FAKE_WASM_URL);
  });

  it("passes the loaded Ghostty instance into Terminal via options.ghostty", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    expect(FakeTerminal.instances).toHaveLength(1);
    const term = FakeTerminal.instances[0]!;
    expect(term.options).toMatchObject({ ghostty: fakeGhosttyInstance });
  });

  it("forwards mapped options into the ghostty-web Terminal constructor", async () => {
    const renderer = new GhosttyWebRenderer({
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      cursorStyle: "underline",
      cursorBlink: true,
      scrollbackLines: 1000,
    });
    await renderer.mount(stubElement);
    expect(FakeTerminal.instances).toHaveLength(1);
    const term = FakeTerminal.instances[0]!;
    // `ghostty` is asserted in a sibling test; here we pin the neutral
    // option mapping using `objectContaining` so the assertion is
    // forward-compatible with future options.* additions.
    expect(term.options).toEqual(
      expect.objectContaining({
        fontFamily: "JetBrains Mono",
        fontSize: 14,
        cursorStyle: "underline",
        cursorBlink: true,
        scrollback: 1000,
        ghostty: fakeGhosttyInstance,
      }),
    );
    expect(term.opened).toBe(true);
  });

  it("queues writes issued before mount and flushes them in order", async () => {
    const renderer = new GhosttyWebRenderer();
    renderer.write("first");
    renderer.write(new Uint8Array([0x32, 0x33]));
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(term.writes).toHaveLength(2);
    expect(term.writes[0]).toBe("first");
    expect(term.writes[1]).toBeInstanceOf(Uint8Array);
  });

  it("write accepts string and Uint8Array post-mount", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.write("hello");
    renderer.write(new Uint8Array([0x41, 0x42]));
    expect(term.writes).toEqual(["hello", new Uint8Array([0x41, 0x42])]);
  });

  it("forwards onData payloads to onInput subscribers", async () => {
    const renderer = new GhosttyWebRenderer();
    const inputs: RendererInput[] = [];
    const unsub = renderer.onInput((d) => inputs.push(d));
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    term.emitData("a");
    term.emitData("b");
    unsub();
    term.emitData("c");
    expect(inputs).toEqual(["a", "b"]);
  });

  it("forwards ghostty onResize to onResize subscribers", async () => {
    const renderer = new GhosttyWebRenderer();
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.resize(80, 24);
    expect(term.cols).toBe(80);
    expect(term.rows).toBe(24);
    expect(sizes).toEqual([{ cols: 80, rows: 24 }]);
  });

  it("focus() delegates to the ghostty-web Terminal", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(term.focused).toBe(false);
    renderer.focus();
    expect(term.focused).toBe(true);
  });

  it("focusTarget() returns the ghostty-web host element after mount, null otherwise", async () => {
    const renderer = new GhosttyWebRenderer();
    // Pre-mount: no Terminal exists yet, so there is no input element.
    expect(renderer.focusTarget()).toBeNull();
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    // After mount, focusTarget() is exactly ghostty-web's
    // `Terminal.element` — the contenteditable host the keydown
    // listener is attached to, and the element `focus()` targets.
    // ghostty-web sets `.element` to the element passed to `open()`,
    // which is the element handed to `mount()`.
    expect(renderer.focusTarget()).toBe(term.element);
    expect(renderer.focusTarget()).toBe(stubElement);
    renderer.dispose();
    // After dispose the renderer is dead and exposes no input element.
    expect(renderer.focusTarget()).toBeNull();
  });

  it("dispose is idempotent and tears down listeners", async () => {
    const renderer = new GhosttyWebRenderer();
    const inputs: RendererInput[] = [];
    renderer.onInput((d) => inputs.push(d));
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;

    renderer.dispose();
    renderer.dispose(); // second call must be a no-op
    expect(term.disposed).toBe(true);
    term.emitData("after-dispose");
    expect(inputs).toEqual([]);
  });

  it("write after dispose is a silent no-op", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    renderer.dispose();
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("re-mount after dispose throws, not silently re-attaches", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    renderer.dispose();
    await expect(renderer.mount(stubElement)).rejects.toThrow(/after dispose/);
  });

  it("dispose before mount is a clean no-op and locks the renderer", async () => {
    const renderer = new GhosttyWebRenderer();
    expect(() => renderer.dispose()).not.toThrow();
    expect(() => renderer.dispose()).not.toThrow();
    expect(FakeTerminal.instances).toHaveLength(0);
    await expect(renderer.mount(stubElement)).rejects.toThrow(/after dispose/);
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("double mount on a live renderer throws", async () => {
    const renderer = new GhosttyWebRenderer();
    await renderer.mount(stubElement);
    await expect(renderer.mount(stubElement)).rejects.toThrow(/already mounted/);
  });

  it("dispose during pending mount cancels the open and never constructs a Terminal", async () => {
    // Stall the WASM load so we can fire dispose() in between. The
    // adapter exposes `__setGhosttyLoaderForTesting` precisely so we
    // don't have to reach for `vi.spyOn` against an ESM live binding —
    // that approach is fragile in strict-ESM transforms because the
    // renderer module captured `Ghostty` by reference at import time.
    // Swapping `loaderFn` instead is the documented seam.
    const deferred = defer<RealGhostty>();
    let loadsSeen = 0;
    let urlSeen: string | undefined;
    __setGhosttyLoaderForTesting((url) => {
      loadsSeen++;
      urlSeen = url;
      return deferred.promise;
    });
    const renderer = new GhosttyWebRenderer();
    const mountPromise = renderer.mount(stubElement);
    // dispose() runs before load resolves
    renderer.dispose();
    deferred.resolve(fakeGhosttyInstance as unknown as RealGhostty);
    await mountPromise;
    expect(loadsSeen).toBe(1);
    expect(urlSeen).toBe(FAKE_WASM_URL);
    expect(FakeTerminal.instances).toHaveLength(0);
  });

  it("a throwing input listener does not break siblings or leak input", async () => {
    const renderer = new GhosttyWebRenderer();
    const seen: RendererInput[] = [];
    renderer.onInput(() => {
      throw new Error("listener boom");
    });
    renderer.onInput((d) => seen.push(d));
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    term.emitData(SECRET_INPUT);
    expect(seen).toEqual([SECRET_INPUT]);
  });
});

describe("GhosttyWebRenderer redaction rule", () => {
  /**
   * Identical pin to `XtermRenderer`'s redaction tests. The adapter
   * must NEVER leak input bytes through:
   *   - errors thrown by mount/dispose/etc.
   *   - console.log/warn/error.
   *   - any stored options blob.
   */
  it("input bytes never appear in console output", async () => {
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const renderer = new GhosttyWebRenderer();
      renderer.onInput(() => {
        // user-side; allowed to see input. We don't echo it ourselves.
      });
      await renderer.mount(stubElement);
      const term = FakeTerminal.instances[0]!;
      term.emitData(SECRET_INPUT);
      renderer.dispose();
      const seenLog = logSpy.mock.calls.flat().map(String).join(" ");
      const seenErr = errSpy.mock.calls.flat().map(String).join(" ");
      const seenWarn = warnSpy.mock.calls.flat().map(String).join(" ");
      expect(seenLog).not.toContain(SECRET_INPUT);
      expect(seenErr).not.toContain(SECRET_INPUT);
      expect(seenWarn).not.toContain(SECRET_INPUT);
    } finally {
      logSpy.mockRestore();
      errSpy.mockRestore();
      warnSpy.mockRestore();
    }
  });

  it("input bytes never appear inside thrown errors from the adapter", async () => {
    const renderer = new GhosttyWebRenderer();
    renderer.onInput(() => {
      throw new Error("listener boom");
    });
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    let captured: unknown = null;
    try {
      term.emitData(SECRET_INPUT);
    } catch (e) {
      captured = e;
    }
    if (captured instanceof Error) {
      expect(captured.message).not.toContain(SECRET_INPUT);
    }
  });
});

describe("GhosttyWebRenderer autofit (accept-and-drop)", () => {
  it("accepts autofit:true without failure", async () => {
    const renderer = new GhosttyWebRenderer({ autofit: true });
    await renderer.mount(stubElement);
    // Pre-mount/post-mount construction succeeds; the option does NOT
    // throw and does NOT reach the underlying constructor blob.
    const term = FakeTerminal.instances[0]!;
    expect(JSON.stringify(term.options)).not.toContain("autofit");
  });

  it("autofitActive() reports false honestly (unsupported)", async () => {
    const renderer = new GhosttyWebRenderer({ autofit: true });
    await renderer.mount(stubElement);
    // ghostty-web has no real container-fit path; the workspace mirrors
    // this onto `data-renderer-autofit="unsupported"` so an operator sees
    // honest copy in Settings.
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("autofitActive() is false when autofit:false (consistent with unsupported)", async () => {
    const renderer = new GhosttyWebRenderer({ autofit: false });
    await renderer.mount(stubElement);
    expect(renderer.autofitActive?.()).toBe(false);
  });
});

/**
 * Reuse `RendererOutput` so vitest type-checks the test imports.
 * Without this, the unused-import lint would clip the symbol away.
 */
const _typeProbe: RendererOutput = "ok";
void _typeProbe;
