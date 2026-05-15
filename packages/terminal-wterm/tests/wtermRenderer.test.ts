import { describe, expect, it, vi, beforeEach } from "vitest";
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
} from "@relayterm/terminal-core";

/**
 * `@wterm/dom` is mocked so the adapter can be exercised outside a
 * browser. The mock tracks the constructor arguments (the host element
 * and the options bag), the awaited `init()`, write payloads,
 * focus/resize/destroy dispositions, and exposes hooks the test can
 * use to drive the synthesised `onData`/`onResize` callbacks. The
 * adapter is the only place in the repo that imports `@wterm/dom`,
 * so the standard `vi.mock(...)` hoist is sufficient — wterm has no
 * module-scope WASM init that would need a `__forTesting` seam (every
 * `WTerm` constructs its own `WasmBridge` inside `init()`).
 *
 * The mock supports the pieces of the wterm 0.2.x surface the adapter
 * actually touches:
 *  - `new WTerm(element, options)` — synchronous
 *  - `await wterm.init()` — async; the mock's `__nextInitFails` flag
 *    flips one upcoming `init()` to reject
 *  - `wterm.write(string | Uint8Array)`
 *  - `wterm.resize(cols, rows)` — synchronously fires the
 *    constructor-supplied `onResize(cols, rows)` callback, mirroring
 *    the production behaviour the adapter relies on
 *  - `wterm.focus()`
 *  - `wterm.destroy()` — idempotent; the mock guards against
 *    double-destroy by setting `destroyed = true`
 *  - `__deferInit` lets the test interleave a synchronous `dispose()`
 *    between `mount()` and the resolution of `init()`
 */
const hoisted = vi.hoisted(() => {
  type DataListener = (data: string) => void;
  type ResizeListener = (cols: number, rows: number) => void;

  interface FakeOptions {
    cols?: number;
    rows?: number;
    cursorBlink?: boolean;
    autoResize?: boolean;
    wasmUrl?: string;
    debug?: boolean;
    onData?: DataListener;
    onResize?: ResizeListener;
    onTitle?: (title: string) => void;
  }

  class FakeWTerm {
    static instances: FakeWTerm[] = [];
    static __nextInitFails = false;
    static __initResolvers: Array<() => void> = [];
    static __deferInit = false;

    element: HTMLElement;
    options: FakeOptions;
    cols: number;
    rows: number;
    writes: Array<string | Uint8Array> = [];
    focused = false;
    /**
     * The element the most recent `focus()` call targeted. Real wterm's
     * `WTerm.focus()` delegates to `InputHandler.focus()`, which focuses
     * the hidden keyboard `<textarea>`; before `init()` builds the
     * `InputHandler` it falls back to the host element. The fake mirrors
     * that branch so a test can pin that `focus()` and `focusTarget()`
     * agree on one input surface.
     */
    focusedElement: HTMLElement | null = null;
    /**
     * Mirrors `WTerm.input` — the (type-level-private) `InputHandler`
     * wterm constructs at the end of `init()`. The adapter's
     * `focusTarget()` reaches `input.textarea`, the hidden keyboard
     * `<textarea>` wterm appends to the host and wires its `keydown`
     * listener to. A plain sentinel object is enough — the adapter only
     * ever returns it by reference and never reads it for content.
     */
    input: { textarea: HTMLTextAreaElement } | null = null;
    destroyed = false;
    initStarted = false;
    initSettled = false;
    initRejected = false;

    constructor(element: HTMLElement, options: FakeOptions = {}) {
      this.element = element;
      this.options = options;
      this.cols = options.cols ?? 80;
      this.rows = options.rows ?? 24;
      FakeWTerm.instances.push(this);
    }

    async init(): Promise<this> {
      this.initStarted = true;
      if (FakeWTerm.__deferInit) {
        await new Promise<void>((resolve) => {
          FakeWTerm.__initResolvers.push(resolve);
        });
      }
      if (FakeWTerm.__nextInitFails) {
        FakeWTerm.__nextInitFails = false;
        this.initSettled = true;
        this.initRejected = true;
        throw new Error("wterm: failed to initialize: simulated WASM failure");
      }
      // Real wterm builds the `InputHandler` (and its hidden keyboard
      // `<textarea>`) at the end of a successful `init()`. Mirror that
      // so `focusTarget()` has something to return post-mount.
      this.input = {
        textarea: {
          __fakeWtermTextarea: true,
        } as unknown as HTMLTextAreaElement,
      };
      this.initSettled = true;
      return this;
    }

    write(data: string | Uint8Array): void {
      if (this.destroyed) return;
      this.writes.push(data);
    }

    resize(cols: number, rows: number): void {
      if (this.destroyed) return;
      this.cols = cols;
      this.rows = rows;
      this.options.onResize?.(cols, rows);
    }

    focus(): void {
      this.focused = true;
      // `WTerm.focus()` focuses the InputHandler's textarea once `init()`
      // built it, and falls back to the host element otherwise.
      this.focusedElement = this.input ? this.input.textarea : this.element;
    }

    destroy(): void {
      this.destroyed = true;
    }

    /** Drive the wterm `onData` callback the adapter wired in `mount`. */
    emitData(data: string): void {
      this.options.onData?.(data);
    }

    static __resolveInits(): void {
      const queued = FakeWTerm.__initResolvers.slice();
      FakeWTerm.__initResolvers.length = 0;
      for (const resolve of queued) resolve();
    }
  }

  return { FakeWTerm };
});

vi.mock("@wterm/dom", () => ({
  WTerm: hoisted.FakeWTerm,
}));

import { WtermRenderer, type WtermRendererCtorOptions } from "../src/index.js";
import { toWtermOptions } from "../src/options.js";

const { FakeWTerm } = hoisted;
const stubElement = {} as unknown as HTMLElement;

beforeEach(() => {
  FakeWTerm.instances.length = 0;
  FakeWTerm.__nextInitFails = false;
  FakeWTerm.__initResolvers.length = 0;
  FakeWTerm.__deferInit = false;
});

/**
 * Sentinel input used by tests asserting the redaction rule. If it
 * ever appears in a thrown error, console call, or stored options
 * blob, the redaction pin in `WtermRenderer` has regressed. Same
 * pattern as `terminal-xterm`, `terminal-ghostty-web`, and
 * `terminal-restty`.
 */
const SECRET_INPUT = "RELAY_WTERM_SECRET_KEYS_SHOULD_NEVER_LEAK";

describe("toWtermOptions", () => {
  it("defaults autoResize to false and otherwise emits an empty bag", () => {
    expect(toWtermOptions({})).toEqual({ autoResize: false });
  });

  it("base `autofit: true` maps to autoResize: true", () => {
    expect(toWtermOptions({ autofit: true })).toEqual({ autoResize: true });
  });

  it("base `autofit: false` keeps autoResize false (no surprise opt-in)", () => {
    expect(toWtermOptions({ autofit: false })).toEqual({ autoResize: false });
  });

  it("`wtermOnly.autoResize` wins over base autofit (explicit escape hatch)", () => {
    // The non-portable knob is the deliberate "I know what I'm doing"
    // override. It MUST take precedence whether it forces auto-resize
    // ON or OFF, regardless of the portable `autofit` value.
    expect(
      toWtermOptions({ autofit: true, wtermOnly: { autoResize: false } }),
    ).toEqual({ autoResize: false });
    expect(
      toWtermOptions({ autofit: false, wtermOnly: { autoResize: true } }),
    ).toEqual({ autoResize: true });
  });

  it("forwards the initial cell grid as cols/rows", () => {
    expect(toWtermOptions({}, { cols: 80, rows: 24 })).toEqual({
      autoResize: false,
      cols: 80,
      rows: 24,
    });
  });

  it("forwards cursorBlink from the neutral surface", () => {
    expect(toWtermOptions({ cursorBlink: true })).toEqual({
      autoResize: false,
      cursorBlink: true,
    });
  });

  it("silently drops cosmetic neutral knobs that have no wterm constructor analogue", () => {
    expect(
      toWtermOptions({
        fontFamily: "JetBrains Mono",
        fontSize: 14,
        lineHeight: 1.4,
        cursorStyle: "block",
        scrollbackLines: 5000,
        theme: { background: "#000", foreground: "#fff" },
      }),
    ).toEqual({ autoResize: false });
  });

  it("`wtermOnly.autoResize` overrides the adapter default", () => {
    expect(toWtermOptions({ wtermOnly: { autoResize: true } })).toEqual({
      autoResize: true,
    });
  });

  it("`wtermOnly` forwards wasmUrl and debug knobs", () => {
    expect(
      toWtermOptions({
        wtermOnly: { wasmUrl: "/wterm.wasm", debug: true },
      }),
    ).toEqual({
      autoResize: false,
      wasmUrl: "/wterm.wasm",
      debug: true,
    });
  });
});

describe("WtermRenderer satisfies TerminalRenderer", () => {
  it("constructs without an element and defers DOM work to mount", () => {
    const renderer: TerminalRenderer = new WtermRenderer({ fontSize: 14 });
    expect(FakeWTerm.instances).toHaveLength(0);
    void renderer.write("noop before mount");
  });

  it("forwards mapped options (and onData/onResize callbacks) into the wterm constructor", async () => {
    const opts: WtermRendererCtorOptions = {
      cols: 100,
      rows: 30,
      cursorBlink: true,
      wtermOnly: { autoResize: true, wasmUrl: "/wterm.wasm" },
    };
    const renderer = new WtermRenderer(opts);
    await renderer.mount(stubElement);
    expect(FakeWTerm.instances).toHaveLength(1);
    const wterm = FakeWTerm.instances[0]!;
    expect(wterm.element).toBe(stubElement);
    expect(wterm.options.cols).toBe(100);
    expect(wterm.options.rows).toBe(30);
    expect(wterm.options.cursorBlink).toBe(true);
    expect(wterm.options.autoResize).toBe(true);
    expect(wterm.options.wasmUrl).toBe("/wterm.wasm");
    expect(typeof wterm.options.onData).toBe("function");
    expect(typeof wterm.options.onResize).toBe("function");
    // `onTitle` is intentionally not surfaced on the renderer-neutral
    // interface; the adapter must not have wired it.
    expect(wterm.options.onTitle).toBeUndefined();
    expect(wterm.initSettled).toBe(true);
  });

  it("queues writes issued before mount and flushes them in order", async () => {
    const renderer = new WtermRenderer();
    renderer.write("first");
    renderer.write(new Uint8Array([0x32, 0x33])); // "23" in UTF-8
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    expect(wterm.writes).toHaveLength(2);
    expect(wterm.writes[0]).toBe("first");
    expect(wterm.writes[1]).toBeInstanceOf(Uint8Array);
    expect(Array.from(wterm.writes[1] as Uint8Array)).toEqual([0x32, 0x33]);
  });

  it("forwards string and Uint8Array writes post-mount without decoding", async () => {
    const renderer = new WtermRenderer();
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    renderer.write("hello");
    renderer.write(new Uint8Array([0x41, 0x42, 0x43]));
    expect(wterm.writes[0]).toBe("hello");
    expect(wterm.writes[1]).toBeInstanceOf(Uint8Array);
  });

  it("forwards wterm onData payloads to onInput subscribers and tears down on unsubscribe", async () => {
    const renderer = new WtermRenderer();
    const inputs: RendererInput[] = [];
    const unsub = renderer.onInput((d) => inputs.push(d));
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    wterm.emitData("a");
    wterm.emitData("b");
    unsub();
    wterm.emitData("c");
    expect(inputs).toEqual(["a", "b"]);
  });

  it("`renderer.resize(...)` calls `WTerm.resize` and fans out via onResize", async () => {
    const renderer = new WtermRenderer();
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    renderer.resize(132, 40);
    expect(wterm.cols).toBe(132);
    expect(wterm.rows).toBe(40);
    expect(sizes).toEqual([{ cols: 132, rows: 40 }]);
  });

  it("a pre-mount resize is applied once, after mount, with only the latest pair winning", async () => {
    const renderer = new WtermRenderer();
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    renderer.resize(80, 24);
    renderer.resize(132, 40);
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    // wterm's constructor took the initialGrid (none provided here ->
    // wterm defaults 80x24); the adapter then applies the queued
    // resize once init resolved.
    expect(wterm.cols).toBe(132);
    expect(wterm.rows).toBe(40);
    expect(sizes).toEqual([{ cols: 132, rows: 40 }]);
  });

  it("focus delegates to the wterm InputHandler post-mount and is a no-op pre-mount", async () => {
    const renderer = new WtermRenderer();
    expect(() => renderer.focus()).not.toThrow();
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    expect(wterm.focused).toBe(false);
    renderer.focus();
    expect(wterm.focused).toBe(true);
  });

  it("focusTarget() returns the wterm keyboard textarea after mount, null otherwise", async () => {
    const renderer = new WtermRenderer();
    // Pre-mount: no WTerm exists yet, so there is no input element.
    expect(renderer.focusTarget()).toBeNull();
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    // After mount, focusTarget() is exactly wterm's hidden helper
    // `<textarea>` (`WTerm.input.textarea`) — the element wterm appends
    // to the host, wires its `keydown` listener to, and `focus()`
    // targets. Returned by reference; never read for content.
    expect(renderer.focusTarget()).toBe(wterm.input!.textarea);
    renderer.dispose();
    // After dispose the renderer is dead and exposes no input element.
    expect(renderer.focusTarget()).toBeNull();
  });

  it("focus() and focusTarget() resolve to the same keyboard input surface", async () => {
    const renderer = new WtermRenderer();
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    renderer.focus();
    // `focus()` delegates to `WTerm.focus()` -> `InputHandler.focus()`,
    // which focuses the hidden `<textarea>`; `focusTarget()` reports
    // that same element. A renderer-fair smoke can therefore focus via
    // the workspace button and verify `document.activeElement` against
    // the renderer-neutral `[data-relayterm-terminal-input]` marker.
    expect(wterm.focusedElement).toBe(renderer.focusTarget());
  });

  it("focusTarget() returns null after a dispose during pending init", async () => {
    FakeWTerm.__deferInit = true;
    const renderer = new WtermRenderer();
    const mountPromise = renderer.mount(stubElement);
    renderer.dispose();
    FakeWTerm.__resolveInits();
    await mountPromise;
    // The just-constructed WTerm was destroyed and never adopted by the
    // adapter, so there is no input element to expose.
    expect(renderer.focusTarget()).toBeNull();
  });

  it("focusTarget() returns null after a failed init()", async () => {
    FakeWTerm.__nextInitFails = true;
    const renderer = new WtermRenderer();
    await expect(renderer.mount(stubElement)).rejects.toThrow(
      /failed to initialize/,
    );
    // `mount` nulls `#wterm` on the init-failure path before rethrowing,
    // so the renderer exposes no input element — it must not surface the
    // half-initialized InputHandler that wterm's own catch tore down.
    expect(renderer.focusTarget()).toBeNull();
  });

  it("dispose is idempotent and tears down listeners and the wterm", async () => {
    const renderer = new WtermRenderer();
    const inputs: RendererInput[] = [];
    renderer.onInput((d) => inputs.push(d));
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;

    renderer.dispose();
    renderer.dispose(); // idempotent
    expect(wterm.destroyed).toBe(true);
    wterm.emitData("after-dispose");
    expect(inputs).toEqual([]);
  });

  it("write after dispose is a silent no-op", async () => {
    const renderer = new WtermRenderer();
    await renderer.mount(stubElement);
    renderer.dispose();
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("resize after dispose is a silent no-op (no further onResize fans)", async () => {
    const renderer = new WtermRenderer();
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    await renderer.mount(stubElement);
    renderer.dispose();
    renderer.resize(132, 40);
    expect(sizes).toEqual([]);
  });

  it("re-mount after dispose throws, not silently re-attaches", async () => {
    const renderer = new WtermRenderer();
    await renderer.mount(stubElement);
    renderer.dispose();
    await expect(renderer.mount(stubElement)).rejects.toThrow(/after dispose/);
  });

  it("dispose before mount is a clean no-op and locks the renderer", async () => {
    const renderer = new WtermRenderer();
    expect(() => renderer.dispose()).not.toThrow();
    expect(() => renderer.dispose()).not.toThrow();
    expect(FakeWTerm.instances).toHaveLength(0);
    await expect(renderer.mount(stubElement)).rejects.toThrow(/after dispose/);
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("double mount on a live renderer throws", async () => {
    const renderer = new WtermRenderer();
    await renderer.mount(stubElement);
    await expect(renderer.mount(stubElement)).rejects.toThrow(/already mounted/);
  });

  it("dispose during pending init destroys the just-constructed wterm and skips the queue flush", async () => {
    FakeWTerm.__deferInit = true;
    const renderer = new WtermRenderer();
    renderer.write("queued-pre-init");
    const mountPromise = renderer.mount(stubElement);
    // The wterm has been constructed (synchronously by mount) but
    // its init is parked. Dispose now and then resolve init.
    expect(FakeWTerm.instances).toHaveLength(1);
    const wterm = FakeWTerm.instances[0]!;
    renderer.dispose();
    FakeWTerm.__resolveInits();
    await mountPromise;
    expect(wterm.destroyed).toBe(true);
    // The queued pre-mount write must NOT have flushed onto a
    // destroyed wterm.
    expect(wterm.writes).toHaveLength(0);
  });

  it("init failure throws a static error and does not leak the underlying message", async () => {
    FakeWTerm.__nextInitFails = true;
    const renderer = new WtermRenderer();
    let captured: unknown = null;
    try {
      await renderer.mount(stubElement);
    } catch (e) {
      captured = e;
    }
    expect(captured).toBeInstanceOf(Error);
    const err = captured as Error;
    expect(err.message).toBe("WtermRenderer: failed to initialize wterm");
    // Pinning that the underlying mock-supplied phrase does not leak
    // through. It's a defence-in-depth assertion: even though wterm's
    // own init error does not embed terminal data, the adapter
    // surface must not propagate library-supplied strings.
    expect(err.message).not.toContain("simulated WASM failure");
  });

  it("a throwing input listener does not break siblings or leak input", async () => {
    const renderer = new WtermRenderer();
    const seen: RendererInput[] = [];
    renderer.onInput(() => {
      throw new Error("listener boom");
    });
    renderer.onInput((d) => seen.push(d));
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    wterm.emitData(SECRET_INPUT);
    expect(seen).toEqual([SECRET_INPUT]);
  });
});

describe("WtermRenderer redaction rule", () => {
  /**
   * Identical pin to `XtermRenderer`, `GhosttyWebRenderer`, and
   * `ResttyRenderer`'s redaction tests. The adapter must NEVER leak
   * input bytes through:
   *   - errors thrown by mount/dispose/etc.
   *   - console.log/warn/error.
   *   - any stored options blob that would surface in diagnostics.
   */
  it("input bytes never appear in console output", async () => {
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const renderer = new WtermRenderer();
      renderer.onInput(() => {
        // user-side; allowed to see input. We don't echo it ourselves.
      });
      await renderer.mount(stubElement);
      const wterm = FakeWTerm.instances[0]!;
      wterm.emitData(SECRET_INPUT);
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
    const renderer = new WtermRenderer();
    renderer.onInput(() => {
      throw new Error("listener boom");
    });
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    let captured: unknown = null;
    try {
      wterm.emitData(SECRET_INPUT);
    } catch (e) {
      captured = e;
    }
    if (captured instanceof Error) {
      expect(captured.message).not.toContain(SECRET_INPUT);
    }
  });

  it("the adapter introduces no console output even when wtermOnly.debug is true", async () => {
    // wterm's own `DebugAdapter` may log render-path traces when
    // `debug: true` is passed through to the WTerm constructor. The
    // adapter is NOT responsible for what that adapter does — wterm
    // owns those traces and they live outside RelayTerm's redaction
    // surface. But the adapter itself MUST NOT contribute its own
    // console calls regardless of the `debug` flag value, so a
    // future change that wires a debug-conditional `console.*` into
    // the adapter would be caught here.
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const renderer = new WtermRenderer({
        wtermOnly: { debug: true },
      });
      await renderer.mount(stubElement);
      const wterm = FakeWTerm.instances[0]!;
      // The mock does not honour `debug`; we are pinning that the
      // adapter doesn't introduce its own console path.
      expect(wterm.options.debug).toBe(true);
      renderer.write("output");
      renderer.resize(132, 40);
      renderer.dispose();
      const seenLog = logSpy.mock.calls.flat().map(String).join(" ");
      const seenErr = errSpy.mock.calls.flat().map(String).join(" ");
      const seenWarn = warnSpy.mock.calls.flat().map(String).join(" ");
      expect(seenLog).toBe("");
      expect(seenErr).toBe("");
      expect(seenWarn).toBe("");
    } finally {
      logSpy.mockRestore();
      errSpy.mockRestore();
      warnSpy.mockRestore();
    }
  });

  it("constructor options are NOT echoed onto the wterm options blob beyond the documented mapping", async () => {
    // Defence-in-depth: a future maintainer who naïvely spread
    // `this.#options` into `WTermOptions` would surface neutral knobs
    // (or worse, a `theme` containing string literals that look like
    // keystrokes) into the constructor options. The contract: only
    // cols/rows/cursorBlink/autoResize/wasmUrl/debug reach the wterm
    // constructor — never the cosmetic neutral knobs.
    const renderer = new WtermRenderer({
      cols: 80,
      rows: 24,
      fontFamily: SECRET_INPUT,
      theme: { background: SECRET_INPUT, foreground: SECRET_INPUT },
    });
    await renderer.mount(stubElement);
    const wterm = FakeWTerm.instances[0]!;
    // The mock retains the entire options bag; serialise it and pin
    // that the sentinel never appears.
    expect(JSON.stringify(wterm.options)).not.toContain(SECRET_INPUT);
  });
});

describe("WtermRenderer autofit", () => {
  it("autofitActive() is false before mount", () => {
    const renderer = new WtermRenderer({ autofit: true });
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("autofit defaults off: autoResize is false post-mount and autofitActive false", async () => {
    const renderer = new WtermRenderer();
    await renderer.mount(stubElement);
    expect(FakeWTerm.instances[0]!.options.autoResize).toBe(false);
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("base autofit:true maps to WTermOptions.autoResize:true and autofitActive true", async () => {
    const renderer = new WtermRenderer({ autofit: true });
    await renderer.mount(stubElement);
    expect(FakeWTerm.instances[0]!.options.autoResize).toBe(true);
    expect(renderer.autofitActive?.()).toBe(true);
  });

  it("wtermOnly.autoResize precedence: wtermOnly:false wins over base autofit:true", async () => {
    const renderer = new WtermRenderer({
      autofit: true,
      wtermOnly: { autoResize: false },
    });
    await renderer.mount(stubElement);
    expect(FakeWTerm.instances[0]!.options.autoResize).toBe(false);
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("wtermOnly.autoResize precedence: wtermOnly:true wins over base autofit:false", async () => {
    const renderer = new WtermRenderer({
      autofit: false,
      wtermOnly: { autoResize: true },
    });
    await renderer.mount(stubElement);
    expect(FakeWTerm.instances[0]!.options.autoResize).toBe(true);
    expect(renderer.autofitActive?.()).toBe(true);
  });

  it("autofitActive() is false after dispose", async () => {
    const renderer = new WtermRenderer({ autofit: true });
    await renderer.mount(stubElement);
    expect(renderer.autofitActive?.()).toBe(true);
    renderer.dispose();
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("autofitActive() is false after a failed init()", async () => {
    FakeWTerm.__nextInitFails = true;
    const renderer = new WtermRenderer({ autofit: true });
    await expect(renderer.mount(stubElement)).rejects.toThrow(
      /failed to initialize/,
    );
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("autofit option is not echoed onto the WTerm options blob", async () => {
    const renderer = new WtermRenderer({ autofit: true });
    await renderer.mount(stubElement);
    // The mapped blob has `autoResize`, not `autofit`; the neutral
    // option name must not leak into the underlying constructor bag.
    const blob = FakeWTerm.instances[0]!.options;
    expect(Object.prototype.hasOwnProperty.call(blob, "autofit")).toBe(false);
  });
});

/**
 * Reuse `RendererOutput` so vitest type-checks the test imports.
 * Without this, the unused-import lint would clip the symbol away.
 */
const _typeProbe: RendererOutput = "ok";
void _typeProbe;
