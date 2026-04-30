import { describe, expect, it, vi, beforeEach } from "vitest";
import type {
  RendererInput,
  RendererOutput,
  TerminalRenderer,
} from "@relayterm/terminal-core";

/**
 * `restty/xterm` is mocked so the adapter can be exercised outside a
 * browser. The mock tracks the options the `Terminal` constructor saw,
 * `open` invocation, write payloads, focus/resize dispositions, and the
 * `onData`/`onResize` listener fans. The adapter is the only place in
 * the repo that imports `restty/xterm`; tests reach for the `Terminal`
 * mock through the standard `vi.mock(...)` hoist (no `__forTesting`
 * seam is needed because restty's xterm shim has no module-scope WASM
 * init — every `Terminal` constructs its own internal `Restty`).
 */
const hoisted = vi.hoisted(() => {
  type FakeListener<T> = (arg: T) => void;
  type FakeOnResizeArg = { cols: number; rows: number };

  class FakeTerminal {
    static instances: FakeTerminal[] = [];
    options: unknown;
    cols = 0;
    rows = 0;
    writes: Array<string> = [];
    focused = false;
    disposed = false;
    opened = false;
    dataListeners = new Set<FakeListener<string>>();
    resizeListeners = new Set<FakeListener<FakeOnResizeArg>>();

    constructor(options: unknown) {
      this.options = options;
      FakeTerminal.instances.push(this);
    }

    open(_el: HTMLElement) {
      this.opened = true;
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
    write(data: string, cb?: () => void) {
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

  return { FakeTerminal };
});

vi.mock("restty/xterm", () => ({
  Terminal: hoisted.FakeTerminal,
}));

import {
  ResttyRenderer,
  type ResttyRendererCtorOptions,
} from "../src/index.js";
import { toResttyOptions } from "../src/options.js";

const { FakeTerminal } = hoisted;
const stubElement = {} as unknown as HTMLElement;

beforeEach(() => {
  FakeTerminal.instances.length = 0;
});

/**
 * Sentinel input used by tests asserting the redaction rule. If it ever
 * appears in a thrown error, console call, or stored options blob, the
 * redaction pin in `ResttyRenderer` has regressed. Same pattern as
 * `terminal-xterm` and `terminal-ghostty-web`.
 */
const SECRET_INPUT = "RELAY_RESTTY_SECRET_KEYS_SHOULD_NEVER_LEAK";

describe("toResttyOptions", () => {
  it("returns an empty bag for empty options and no initial grid", () => {
    expect(toResttyOptions({})).toEqual({});
  });

  it("forwards the initial cell grid as cols/rows", () => {
    expect(toResttyOptions({}, { cols: 80, rows: 24 })).toEqual({
      cols: 80,
      rows: 24,
    });
  });

  it("silently drops neutral knobs that have no restty analogue", () => {
    expect(
      toResttyOptions({
        fontFamily: "JetBrains Mono",
        fontSize: 14,
        lineHeight: 1.4,
        cursorStyle: "block",
        cursorBlink: true,
        scrollbackLines: 5000,
        theme: { background: "#000", foreground: "#fff" },
      }),
    ).toEqual({});
  });

  it("`resttyOnly` escape hatch overrides initial grid keys", () => {
    expect(
      toResttyOptions(
        { resttyOnly: { cols: 132, fontSize: 18, customFlag: true } },
        { cols: 80, rows: 24 },
      ),
    ).toEqual({
      cols: 132,
      rows: 24,
      fontSize: 18,
      customFlag: true,
    });
  });
});

describe("ResttyRenderer satisfies TerminalRenderer", () => {
  it("constructs without an element and defers DOM work to mount", () => {
    const renderer: TerminalRenderer = new ResttyRenderer({ fontSize: 14 });
    expect(FakeTerminal.instances).toHaveLength(0);
    void renderer.write("noop before mount");
  });

  it("forwards mapped options into the restty Terminal constructor", async () => {
    const opts: ResttyRendererCtorOptions = {
      cols: 100,
      rows: 30,
      resttyOnly: { customKey: "value" },
    };
    const renderer = new ResttyRenderer(opts);
    await renderer.mount(stubElement);
    expect(FakeTerminal.instances).toHaveLength(1);
    const term = FakeTerminal.instances[0]!;
    expect(term.options).toEqual({
      cols: 100,
      rows: 30,
      customKey: "value",
    });
    expect(term.opened).toBe(true);
  });

  it("queues writes issued before mount and flushes them in order", async () => {
    const renderer = new ResttyRenderer();
    renderer.write("first");
    renderer.write(new Uint8Array([0x32, 0x33])); // "23"
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(term.writes).toEqual(["first", "23"]);
  });

  it("decodes Uint8Array writes to UTF-8 strings post-mount", async () => {
    const renderer = new ResttyRenderer();
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.write("hello");
    renderer.write(new Uint8Array([0x41, 0x42, 0x43])); // "ABC"
    expect(term.writes).toEqual(["hello", "ABC"]);
  });

  it("forwards onData payloads to onInput subscribers", async () => {
    const renderer = new ResttyRenderer();
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

  it("forwards restty onResize to onResize subscribers", async () => {
    const renderer = new ResttyRenderer();
    const sizes: Array<{ cols: number; rows: number }> = [];
    renderer.onResize?.((s) => sizes.push(s));
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    renderer.resize(80, 24);
    expect(term.cols).toBe(80);
    expect(term.rows).toBe(24);
    expect(sizes).toEqual([{ cols: 80, rows: 24 }]);
  });

  it("focus() delegates to the restty Terminal", async () => {
    const renderer = new ResttyRenderer();
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(term.focused).toBe(false);
    renderer.focus();
    expect(term.focused).toBe(true);
  });

  it("dispose is idempotent and tears down listeners", async () => {
    const renderer = new ResttyRenderer();
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
    const renderer = new ResttyRenderer();
    await renderer.mount(stubElement);
    renderer.dispose();
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("re-mount after dispose throws, not silently re-attaches", async () => {
    const renderer = new ResttyRenderer();
    await renderer.mount(stubElement);
    renderer.dispose();
    await expect(renderer.mount(stubElement)).rejects.toThrow(/after dispose/);
  });

  it("dispose before mount is a clean no-op and locks the renderer", async () => {
    const renderer = new ResttyRenderer();
    expect(() => renderer.dispose()).not.toThrow();
    expect(() => renderer.dispose()).not.toThrow();
    expect(FakeTerminal.instances).toHaveLength(0);
    await expect(renderer.mount(stubElement)).rejects.toThrow(/after dispose/);
    expect(() => renderer.write("ignored")).not.toThrow();
  });

  it("double mount on a live renderer throws", async () => {
    const renderer = new ResttyRenderer();
    await renderer.mount(stubElement);
    await expect(renderer.mount(stubElement)).rejects.toThrow(/already mounted/);
  });

  it("dispose during pending mount cancels the open and never constructs a Terminal", async () => {
    const renderer = new ResttyRenderer();
    const mountPromise = renderer.mount(stubElement);
    // dispose() runs before the awaited microtask resolves
    renderer.dispose();
    await mountPromise;
    expect(FakeTerminal.instances).toHaveLength(0);
  });

  it("a throwing input listener does not break siblings or leak input", async () => {
    const renderer = new ResttyRenderer();
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

describe("ResttyRenderer redaction rule", () => {
  /**
   * Identical pin to `XtermRenderer` and `GhosttyWebRenderer`'s
   * redaction tests. The adapter must NEVER leak input bytes through:
   *   - errors thrown by mount/dispose/etc.
   *   - console.log/warn/error.
   *   - any stored options blob.
   */
  it("input bytes never appear in console output", async () => {
    const logSpy = vi.spyOn(console, "log").mockImplementation(() => {});
    const errSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const renderer = new ResttyRenderer();
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
    const renderer = new ResttyRenderer();
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

  it("constructor options are NOT echoed onto the Terminal options blob beyond the documented mapping", async () => {
    // Defence-in-depth: a future maintainer who naïvely spreads
    // `this.#options` into the restty options bag would surface
    // neutral knobs (or worse, a `theme` containing string literals
    // that look like keystrokes) into the constructor options. The
    // contract: only `cols`/`rows`/`resttyOnly` reach the restty
    // Terminal — never the cosmetic neutral knobs.
    const renderer = new ResttyRenderer({
      cols: 80,
      rows: 24,
      fontFamily: SECRET_INPUT,
      theme: { background: SECRET_INPUT, foreground: SECRET_INPUT },
    });
    await renderer.mount(stubElement);
    const term = FakeTerminal.instances[0]!;
    expect(JSON.stringify(term.options)).not.toContain(SECRET_INPUT);
  });
});

/**
 * Reuse `RendererOutput` so vitest type-checks the test imports.
 * Without this, the unused-import lint would clip the symbol away.
 */
const _typeProbe: RendererOutput = "ok";
void _typeProbe;
