import { describe, expect, it, vi, beforeEach } from "vitest";
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
} from "@relayterm/terminal-core";

/**
 * @xterm/xterm and friends are mocked so the adapter can be exercised
 * outside a browser. The mocks track constructor args, write payloads
 * and dispose calls so tests can assert on the boundary without ever
 * inspecting input bytes themselves.
 */
/**
 * `ResizeObserver` is not part of the JSDOM-free vitest environment. The
 * hoisted fake captures observe/disconnect calls and lets each test
 * trigger the registered callback synchronously so the autofit wiring
 * can be exercised without a browser.
 */
interface FakeResizeObserverEntry {
  observed: Element | null;
  disconnected: boolean;
  trigger: () => void;
}

const observerHoisted = vi.hoisted(() => {
  const instances: FakeResizeObserverEntry[] = [];
  class FakeResizeObserver {
    #entry: FakeResizeObserverEntry;
    constructor(cb: () => void) {
      this.#entry = { observed: null, disconnected: false, trigger: cb };
      instances.push(this.#entry);
    }
    observe(el: Element) {
      this.#entry.observed = el;
    }
    disconnect() {
      this.#entry.disconnected = true;
    }
    unobserve() {}
  }
  return { FakeResizeObserver, instances };
});

const hoisted = vi.hoisted(() => {
  type FakeListener<T> = (arg: T) => void;
  type FakeOnResizeArg = { cols: number; rows: number };

  class FakeTerminal {
    static instances: FakeTerminal[] = [];
    cols = 0;
    rows = 0;
    options: unknown;
    writes: Array<string | Uint8Array> = [];
    focused = false;
    disposed = false;
    cleared = 0;
    loadedAddons: unknown[] = [];
    dataListeners = new Set<FakeListener<string>>();
    resizeListeners = new Set<FakeListener<FakeOnResizeArg>>();
    /**
     * Mirrors xterm.js's public `Terminal.textarea` — the hidden helper
     * `<textarea>` xterm wires keyboard input through. Real xterm sets
     * it during `open()`; the fake does the same so `focusTarget()` can
     * be exercised without a browser. A plain sentinel object is enough
     * — the adapter only ever returns it by reference.
     */
    textarea: HTMLTextAreaElement | undefined;

    constructor(options: unknown) {
      this.options = options;
      FakeTerminal.instances.push(this);
    }

    open(_el: HTMLElement) {
      this.textarea = {
        __fakeXtermTextarea: true,
      } as unknown as HTMLTextAreaElement;
    }
    loadAddon(addon: unknown) {
      this.loadedAddons.push(addon);
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
    clear() {
      this.cleared += 1;
    }
    dispose() {
      this.disposed = true;
    }

    emitData(data: string) {
      for (const l of [...this.dataListeners]) l(data);
    }
  }

  class FakeFitAddon {
    static instances: FakeFitAddon[] = [];
    fitCalls = 0;
    constructor() {
      FakeFitAddon.instances.push(this);
    }
    fit() {
      this.fitCalls++;
    }
    dispose() {}
  }

  class FakeWebLinksAddon {
    dispose() {}
  }

  return { FakeTerminal, FakeFitAddon, FakeWebLinksAddon };
});

vi.mock("@xterm/xterm", () => ({ Terminal: hoisted.FakeTerminal }));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: hoisted.FakeFitAddon }));
vi.mock("@xterm/addon-web-links", () => ({
  WebLinksAddon: hoisted.FakeWebLinksAddon,
}));

// Install a ResizeObserver shim on globalThis so the xterm autofit
// path can attach an observer when the option is enabled. The shim is
// scoped per-test via `beforeEach` resetting the captured instances list.
Object.defineProperty(globalThis, "ResizeObserver", {
  value: observerHoisted.FakeResizeObserver,
  configurable: true,
  writable: true,
});

// requestAnimationFrame may be undefined in vitest's Node env; the
// adapter falls back to a microtask, but providing a stable fake makes
// rAF-coalesced fit calls assertable.
const rafCallbacks: Array<() => void> = [];
Object.defineProperty(globalThis, "requestAnimationFrame", {
  value: (cb: () => void) => {
    rafCallbacks.push(cb);
    return rafCallbacks.length;
  },
  configurable: true,
  writable: true,
});
Object.defineProperty(globalThis, "cancelAnimationFrame", {
  value: (_id: number) => {},
  configurable: true,
  writable: true,
});

function flushRaf(): void {
  const queue = rafCallbacks.slice();
  rafCallbacks.length = 0;
  for (const cb of queue) cb();
}

import { XtermRenderer, type XtermRendererOptions } from "../src/index.js";
// The conversion helpers are adapter-internal — see comment in index.ts.
// Tests reach into the relative module so the public API stays neutral.
import { toXtermOptions, toXtermTheme } from "../src/options.js";

const { FakeTerminal, FakeFitAddon, FakeWebLinksAddon } = hoisted;

const stubElement = {} as unknown as HTMLElement;

beforeEach(() => {
  FakeTerminal.instances.length = 0;
  FakeFitAddon.instances.length = 0;
  observerHoisted.instances.length = 0;
  rafCallbacks.length = 0;
});

/**
 * Sentinel input string used by tests asserting the redaction rule.
 * If this string ever appears in a serialized event payload, error
 * message, or console call, the redaction pin in `XtermRenderer` has
 * regressed — see step 6 ("Input and logging safety") of the slice spec.
 */
const SECRET_INPUT = "RELAY_SECRET_KEYS_SHOULD_NEVER_LEAK";

describe("toXtermOptions / toXtermTheme", () => {
  it("forwards only fields the caller actually set", () => {
    expect(toXtermOptions({})).toEqual({});
  });

  it("maps neutral knobs onto xterm option names", () => {
    const opts: XtermRendererOptions = {
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      lineHeight: 1.2,
      cursorStyle: "bar",
      cursorBlink: true,
      scrollbackLines: 5000,
    };
    expect(toXtermOptions(opts)).toEqual({
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      lineHeight: 1.2,
      cursorStyle: "bar",
      cursorBlink: true,
      scrollback: 5000,
    });
  });

  it("renames scrollbackLines → scrollback", () => {
    expect(toXtermOptions({ scrollbackLines: 12 })).toEqual({ scrollback: 12 });
  });

  it("forwards a full ANSI palette via toXtermTheme", () => {
    const theme = toXtermTheme({
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

  it("`xtermOnly` escape hatch overrides the portable mapping", () => {
    const mapped = toXtermOptions({
      fontSize: 14,
      xtermOnly: { fontSize: 18, allowProposedApi: true },
    });
    expect(mapped).toEqual({ fontSize: 18, allowProposedApi: true });
  });
});

describe("XtermRenderer satisfies TerminalRenderer", () => {
  it("constructs without an element and defers DOM work to mount", () => {
    const renderer: TerminalRenderer = new XtermRenderer({ fontSize: 14 });
    expect(FakeTerminal.instances).toHaveLength(0);
    // typecheck-only: renderer is the renderer-neutral interface
    void renderer.write("noop before mount");
  });

  it("forwards mapped options into the xterm.js Terminal constructor", () => {
    const renderer = new XtermRenderer({
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      cursorStyle: "underline",
      cursorBlink: true,
      scrollbackLines: 1000,
    });
    renderer.mount(stubElement);
    expect(FakeTerminal.instances).toHaveLength(1);
    const term = FakeTerminal.instances[0]!;
    expect(term.options).toEqual({
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      cursorStyle: "underline",
      cursorBlink: true,
      scrollback: 1000,
    });
    expect(
      term.loadedAddons.some((a) => a instanceof FakeFitAddon),
    ).toBe(true);
    expect(
      term.loadedAddons.some((a) => a instanceof FakeWebLinksAddon),
    ).toBe(true);
  });

  it("queues writes issued before mount and flushes them in order", () => {
    const renderer = new XtermRenderer();
    renderer.write("first");
    renderer.write(new Uint8Array([0x32, 0x33]));
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(term.writes).toHaveLength(2);
    expect(term.writes[0]).toBe("first");
    expect(term.writes[1]).toBeInstanceOf(Uint8Array);
  });

  it("write accepts string and Uint8Array post-mount", () => {
    const renderer = new XtermRenderer();
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.write("hello");
    renderer.write(new Uint8Array([0x41, 0x42]));
    expect(term.writes).toEqual(["hello", new Uint8Array([0x41, 0x42])]);
  });

  it("forwards onData payloads to onInput subscribers", () => {
    const renderer = new XtermRenderer();
    const inputs: RendererInput[] = [];
    const unsub = renderer.onInput((d) => inputs.push(d));
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    term.emitData("a");
    term.emitData("b");
    unsub();
    term.emitData("c");
    expect(inputs).toEqual(["a", "b"]);
  });

  it("forwards xterm onResize to onResize subscribers", () => {
    const renderer = new XtermRenderer();
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.resize(80, 24);
    expect(term.cols).toBe(80);
    expect(term.rows).toBe(24);
    expect(sizes).toEqual([{ cols: 80, rows: 24 }]);
  });

  it("focus() delegates to the xterm Terminal", () => {
    const renderer = new XtermRenderer();
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(term.focused).toBe(false);
    renderer.focus();
    expect(term.focused).toBe(true);
  });

  it("focus() before mount and after dispose is a silent no-op", () => {
    const renderer = new XtermRenderer();
    expect(() => renderer.focus()).not.toThrow();
    renderer.mount(stubElement);
    renderer.dispose();
    expect(() => renderer.focus()).not.toThrow();
  });

  it("focusTarget() returns the xterm helper textarea after mount, null otherwise", () => {
    const renderer = new XtermRenderer();
    // Pre-mount: no Terminal exists yet, so there is no input element.
    expect(renderer.focusTarget()).toBeNull();
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    // After mount, focusTarget() is exactly xterm's `Terminal.textarea`
    // — the element `focus()` targets and the one a real keystroke
    // hits. Returned by reference; never read for content.
    expect(renderer.focusTarget()).toBe(term.textarea);
    renderer.dispose();
    // After dispose the renderer is dead and exposes no input element.
    expect(renderer.focusTarget()).toBeNull();
  });

  it("clear() invokes Terminal.clear and is safe before mount / after dispose", () => {
    const renderer = new XtermRenderer();
    // Pre-mount: no FakeTerminal exists yet, so clear() has nothing
    // to delegate to and must not throw or accidentally construct one.
    expect(() => renderer.clear()).not.toThrow();
    expect(FakeTerminal.instances).toHaveLength(0);
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.clear();
    renderer.clear();
    expect(term.cleared).toBe(2);
    renderer.dispose();
    expect(() => renderer.clear()).not.toThrow();
  });

  it("fit() returns null before mount and post-fit dims after mount", () => {
    const renderer = new XtermRenderer();
    expect(renderer.fit()).toBeNull();
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    term.cols = 120;
    term.rows = 40;
    expect(renderer.fit()).toEqual({ cols: 120, rows: 40 });
  });

  it("dispose is idempotent and tears down listeners", () => {
    const renderer = new XtermRenderer();
    const inputs: RendererInput[] = [];
    renderer.onInput((d) => inputs.push(d));
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;

    renderer.dispose();
    renderer.dispose(); // second call must be a no-op
    expect(term.disposed).toBe(true);
    // Listener set was cleared; new emissions land nowhere even if
    // something in xterm-land kept firing onData (it shouldn't).
    term.emitData("after-dispose");
    expect(inputs).toEqual([]);
  });

  it("write after dispose is a silent no-op", () => {
    const renderer = new XtermRenderer();
    renderer.mount(stubElement);
    renderer.dispose();
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("re-mount after dispose throws, not silently re-attaches", () => {
    const renderer = new XtermRenderer();
    renderer.mount(stubElement);
    renderer.dispose();
    expect(() => renderer.mount(stubElement)).toThrow(/after dispose/);
  });

  it("dispose before mount is a clean no-op and locks the renderer", () => {
    const renderer = new XtermRenderer();
    expect(() => renderer.dispose()).not.toThrow();
    // Idempotent on a never-mounted renderer too.
    expect(() => renderer.dispose()).not.toThrow();
    // No xterm Terminal was ever constructed.
    expect(FakeTerminal.instances).toHaveLength(0);
    // Subsequent mount is rejected — same dead-renderer policy as
    // dispose-after-mount.
    expect(() => renderer.mount(stubElement)).toThrow(/after dispose/);
    // Writes after dispose are silent no-ops, mounted or not.
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("double mount on a live renderer throws", () => {
    const renderer = new XtermRenderer();
    renderer.mount(stubElement);
    expect(() => renderer.mount(stubElement)).toThrow(/already mounted/);
  });

  it("a throwing input listener does not break siblings or leak input", () => {
    const renderer = new XtermRenderer();
    const seen: RendererInput[] = [];
    renderer.onInput(() => {
      // Intentionally throw with a tag that does NOT include input bytes.
      throw new Error("listener boom");
    });
    renderer.onInput((d) => seen.push(d));
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    term.emitData(SECRET_INPUT);
    expect(seen).toEqual([SECRET_INPUT]);
  });
});

describe("XtermRenderer redaction rule (step 6)", () => {
  /**
   * The whole adapter must never leak input bytes through:
   *   - errors thrown by mount/dispose/etc.
   *   - console.log/warn/error.
   *   - the xterm options blob (e.g. via a stray serialization).
   *
   * We feed a known sentinel through onData and assert it is ABSENT from
   * every observable side-channel except the user's own onInput callback.
   */
  it("input bytes never appear in console output", () => {
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const renderer = new XtermRenderer();
      renderer.onInput(() => {
        // user-side; allowed to see input. We don't echo it ourselves.
      });
      renderer.mount(stubElement);
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

  it("input bytes never appear inside thrown errors from the adapter", () => {
    const renderer = new XtermRenderer();
    renderer.onInput(() => {
      throw new Error("listener boom");
    });
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    let captured: unknown = null;
    try {
      term.emitData(SECRET_INPUT);
    } catch (e) {
      captured = e;
    }
    // The fanout catches listener errors so this should be unreachable,
    // but if a future change ever lets one escape we want the bytes
    // out of the error envelope.
    if (captured instanceof Error) {
      expect(captured.message).not.toContain(SECRET_INPUT);
    }
  });
});

describe("XtermRenderer autofit", () => {
  it("autofit defaults off: no ResizeObserver is constructed", () => {
    const renderer = new XtermRenderer();
    renderer.mount(stubElement);
    expect(observerHoisted.instances).toHaveLength(0);
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("autofit:false explicit also leaves no observer", () => {
    const renderer = new XtermRenderer({ autofit: false });
    renderer.mount(stubElement);
    expect(observerHoisted.instances).toHaveLength(0);
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("autofit:true installs a ResizeObserver on the mount element", () => {
    const renderer = new XtermRenderer({ autofit: true });
    renderer.mount(stubElement);
    expect(observerHoisted.instances).toHaveLength(1);
    expect(observerHoisted.instances[0]!.observed).toBe(stubElement);
    expect(renderer.autofitActive?.()).toBe(true);
  });

  it("autofitActive() is false before mount and after dispose", () => {
    const renderer = new XtermRenderer({ autofit: true });
    expect(renderer.autofitActive?.()).toBe(false);
    renderer.mount(stubElement);
    expect(renderer.autofitActive?.()).toBe(true);
    renderer.dispose();
    expect(renderer.autofitActive?.()).toBe(false);
  });

  it("observer callback fans through FitAddon.fit() (rAF-coalesced)", () => {
    const renderer = new XtermRenderer({ autofit: true });
    renderer.mount(stubElement);
    const fit = FakeFitAddon.instances[0]!;
    const before = fit.fitCalls;
    // Trigger the observer twice synchronously; rAF coalesces into one
    observerHoisted.instances[0]!.trigger();
    observerHoisted.instances[0]!.trigger();
    flushRaf();
    expect(fit.fitCalls - before).toBe(1);
  });

  it("observer fires propagate through the existing onResize seam", () => {
    const renderer = new XtermRenderer({ autofit: true });
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    // Simulate the FitAddon driving a new grid (real FitAddon calls
    // term.resize which fans onResize listeners synchronously).
    observerHoisted.instances[0]!.trigger();
    flushRaf();
    // Drive the fake terminal resize the same way real FitAddon does
    term.resize(120, 40);
    expect(sizes.some((s) => s.cols === 120 && s.rows === 40)).toBe(true);
  });

  it("dispose disconnects the observer", () => {
    const renderer = new XtermRenderer({ autofit: true });
    renderer.mount(stubElement);
    expect(observerHoisted.instances[0]!.disconnected).toBe(false);
    renderer.dispose();
    expect(observerHoisted.instances[0]!.disconnected).toBe(true);
  });

  it("late observer callbacks after dispose do not call FitAddon.fit", () => {
    const renderer = new XtermRenderer({ autofit: true });
    renderer.mount(stubElement);
    const fit = FakeFitAddon.instances[0]!;
    const before = fit.fitCalls;
    renderer.dispose();
    // Real ResizeObservers don't fire after disconnect; defence-in-depth
    // assertion against a future change that ignores #disposed in the
    // coalesced rAF callback.
    observerHoisted.instances[0]!.trigger();
    flushRaf();
    expect(fit.fitCalls).toBe(before);
  });

  it("autofit option is not echoed onto the xterm options blob", () => {
    const renderer = new XtermRenderer({ autofit: true, fontSize: 14 });
    renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(JSON.stringify(term.options)).not.toContain("autofit");
  });
});

/**
 * Reuse `RendererOutput` so vitest type-checks the test imports.
 * Without this, the unused-import lint would clip the symbol away.
 */
const _typeProbe: RendererOutput = "ok";
void _typeProbe;
