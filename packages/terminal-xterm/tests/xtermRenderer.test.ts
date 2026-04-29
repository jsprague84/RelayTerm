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
    loadedAddons: unknown[] = [];
    dataListeners = new Set<FakeListener<string>>();
    resizeListeners = new Set<FakeListener<FakeOnResizeArg>>();

    constructor(options: unknown) {
      this.options = options;
      FakeTerminal.instances.push(this);
    }

    open(_el: HTMLElement) {}
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
    dispose() {
      this.disposed = true;
    }

    emitData(data: string) {
      for (const l of [...this.dataListeners]) l(data);
    }
  }

  class FakeFitAddon {
    fitCalls = 0;
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

import { XtermRenderer, type XtermRendererOptions } from "../src/index.js";
// The conversion helpers are adapter-internal — see comment in index.ts.
// Tests reach into the relative module so the public API stays neutral.
import { toXtermOptions, toXtermTheme } from "../src/options.js";

const { FakeTerminal, FakeFitAddon, FakeWebLinksAddon } = hoisted;

const stubElement = {} as unknown as HTMLElement;

beforeEach(() => {
  FakeTerminal.instances.length = 0;
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

/**
 * Reuse `RendererOutput` so vitest type-checks the test imports.
 * Without this, the unused-import lint would clip the symbol away.
 */
const _typeProbe: RendererOutput = "ok";
void _typeProbe;
