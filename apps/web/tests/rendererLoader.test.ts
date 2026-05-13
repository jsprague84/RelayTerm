/**
 * Renderer-loader unit tests. The loader is the single place the
 * production app shell decides which renderer to mount; these tests
 * pin the gate semantics, the fallback taxonomy, and the dynamic-
 * import-table invariant ("xterm never goes through an importer").
 *
 * The tests inject a stub importer table so the real
 * `@relayterm/terminal-{ghostty-web,restty,wterm}` modules are NEVER
 * imported under jsdom — the heavy WASM payloads would never resolve
 * here, and the tests would either hang or false-fail.
 */
import { describe, expect, it, vi } from "vitest";
import type {
  BaseTerminalRendererOptions,
  RendererInput,
  RendererOutput,
  TerminalRenderer,
} from "@relayterm/terminal-core";

// xterm's transitive `@xterm/addon-fit` import touches `self`, which is
// not defined in Vitest's node environment. The loader statically
// imports `XtermRenderer` for the production-bundle's default path; we
// stub it here so the loader can be unit-tested without spinning up a
// DOM. The stub is constructor-compatible with `XtermRenderer` — every
// loader path that returns the xterm fallback ends up handing back an
// instance of this class.
vi.mock("@relayterm/terminal-xterm", () => ({
  XtermRenderer: class StubXtermRenderer implements TerminalRenderer {
    constructor(public readonly options: BaseTerminalRendererOptions) {}
    mount(): void {}
    write(): void {}
    resize(): void {}
    focus(): void {}
    dispose(): void {}
    onInput(): () => void {
      return () => {};
    }
    onResize(): () => void {
      return () => {};
    }
  },
}));

import {
  type RendererImporters,
  loadRendererWithImporters,
} from "../src/lib/app/terminal/rendererLoader.js";

class FakeRenderer implements TerminalRenderer {
  public disposed = false;
  // Tag so a test can prove which adapter was instantiated.
  constructor(public readonly tag: string) {}
  mount(_target: HTMLElement): void | Promise<void> {
    return;
  }
  write(_chunk: RendererOutput): void {
    return;
  }
  resize(_cols: number, _rows: number): void {
    return;
  }
  focus(): void {
    return;
  }
  dispose(): void {
    this.disposed = true;
  }
  onInput(_handler: (data: RendererInput) => void): () => void {
    return () => {};
  }
  onResize(
    _handler: (size: { cols: number; rows: number }) => void,
  ): () => void {
    return () => {};
  }
}

function baseOptions(): BaseTerminalRendererOptions {
  return {
    fontFamily: "ui-monospace",
    fontSize: 13,
    lineHeight: 1.0,
    cursorStyle: "block",
    cursorBlink: true,
    scrollbackLines: 2_000,
    theme: { background: "#000", foreground: "#fff", cursor: "#fff" },
  };
}

function stubImporters(): {
  importers: RendererImporters;
  spies: {
    ghosttyWeb: ReturnType<typeof vi.fn>;
    restty: ReturnType<typeof vi.fn>;
    wterm: ReturnType<typeof vi.fn>;
  };
} {
  const ghosttyWeb = vi.fn().mockResolvedValue({
    GhosttyWebRenderer: class extends FakeRenderer {
      constructor(_options: BaseTerminalRendererOptions) {
        super("ghostty-web");
      }
    },
  });
  const restty = vi.fn().mockResolvedValue({
    ResttyRenderer: class extends FakeRenderer {
      constructor(
        _options: BaseTerminalRendererOptions & { cols: number; rows: number },
      ) {
        super("restty");
      }
    },
  });
  const wterm = vi.fn().mockResolvedValue({
    WtermRenderer: class extends FakeRenderer {
      constructor(
        _options: BaseTerminalRendererOptions & { cols: number; rows: number },
      ) {
        super("wterm");
      }
    },
  });
  return {
    importers: { ghosttyWeb, restty, wterm },
    spies: { ghosttyWeb, restty, wterm },
  };
}

describe("loadRenderer — xterm default path", () => {
  it("returns xterm and never invokes an importer", async () => {
    const { importers, spies } = stubImporters();
    const result = await loadRendererWithImporters(
      {
        id: "xterm",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    expect(result.rendererId).toBe("xterm");
    expect(result.requestedRendererId).toBe("xterm");
    expect(result.fallback).toBeUndefined();
    expect(spies.ghosttyWeb).not.toHaveBeenCalled();
    expect(spies.restty).not.toHaveBeenCalled();
    expect(spies.wterm).not.toHaveBeenCalled();
  });
});

describe("loadRenderer — experimental gate", () => {
  it("falls back to xterm when an experimental id is requested but the gate is off", async () => {
    const { importers, spies } = stubImporters();
    const result = await loadRendererWithImporters(
      {
        id: "ghostty-web",
        experimentalEnabled: false,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    expect(result.rendererId).toBe("xterm");
    expect(result.requestedRendererId).toBe("ghostty-web");
    expect(result.fallback).toBe("experimental_gate_off");
    // CRITICAL: the gate must short-circuit BEFORE any importer is called.
    // Otherwise a stale gate-off setting would still ship the WASM payload.
    expect(spies.ghosttyWeb).not.toHaveBeenCalled();
    expect(spies.restty).not.toHaveBeenCalled();
    expect(spies.wterm).not.toHaveBeenCalled();
  });

  it("mounts ghostty-web when the gate is on", async () => {
    const { importers, spies } = stubImporters();
    const result = await loadRendererWithImporters(
      {
        id: "ghostty-web",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    expect(result.rendererId).toBe("ghostty-web");
    expect(result.requestedRendererId).toBe("ghostty-web");
    expect(result.fallback).toBeUndefined();
    expect(spies.ghosttyWeb).toHaveBeenCalledTimes(1);
    expect(spies.restty).not.toHaveBeenCalled();
    expect(spies.wterm).not.toHaveBeenCalled();
  });

  it("mounts restty with cols/rows when the gate is on", async () => {
    const { importers, spies } = stubImporters();
    const result = await loadRendererWithImporters(
      {
        id: "restty",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 120,
        rows: 40,
      },
      importers,
    );
    expect(result.rendererId).toBe("restty");
    expect(spies.restty).toHaveBeenCalledTimes(1);
  });

  it("mounts wterm with cols/rows when the gate is on", async () => {
    const { importers, spies } = stubImporters();
    const result = await loadRendererWithImporters(
      {
        id: "wterm",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 100,
        rows: 30,
      },
      importers,
    );
    expect(result.rendererId).toBe("wterm");
    expect(spies.wterm).toHaveBeenCalledTimes(1);
  });
});

describe("loadRenderer — fallback safety", () => {
  it("falls back to xterm when the adapter dynamic import rejects", async () => {
    const { importers } = stubImporters();
    importers.ghosttyWeb = vi
      .fn()
      .mockRejectedValue(new Error("wasm-init-failed in /assets/abc.js"));
    const result = await loadRendererWithImporters(
      {
        id: "ghostty-web",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    expect(result.rendererId).toBe("xterm");
    expect(result.requestedRendererId).toBe("ghostty-web");
    expect(result.fallback).toBe("adapter_load_failed");
  });

  it("falls back to xterm when the adapter constructor throws", async () => {
    const { importers } = stubImporters();
    importers.wterm = vi.fn().mockResolvedValue({
      WtermRenderer: class {
        constructor() {
          throw new Error("constructor blew up");
        }
      },
    });
    const result = await loadRendererWithImporters(
      {
        id: "wterm",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    expect(result.rendererId).toBe("xterm");
    expect(result.requestedRendererId).toBe("wterm");
    expect(result.fallback).toBe("adapter_load_failed");
  });

  it("collapses an unknown renderer id to xterm without an importer call", async () => {
    const { importers, spies } = stubImporters();
    const result = await loadRendererWithImporters(
      {
        // intentional cast — simulating a corrupted persisted setting.
        id: "native" as unknown as "xterm",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    expect(result.rendererId).toBe("xterm");
    expect(result.requestedRendererId).toBe("xterm");
    expect(result.fallback).toBe("unknown_renderer_id");
    expect(spies.ghosttyWeb).not.toHaveBeenCalled();
    expect(spies.restty).not.toHaveBeenCalled();
    expect(spies.wterm).not.toHaveBeenCalled();
  });

  it("does not surface the underlying adapter error message", async () => {
    const { importers } = stubImporters();
    importers.restty = vi
      .fn()
      .mockRejectedValue(
        new Error("PEM body leak: BEGIN OPENSSH PRIVATE KEY: ..."),
      );
    const result = await loadRendererWithImporters(
      {
        id: "restty",
        experimentalEnabled: true,
        options: baseOptions(),
        cols: 80,
        rows: 24,
      },
      importers,
    );
    // The fallback is a closed-vocabulary string — never the raw Error
    // message. A regression that surfaces `err.message` would break this.
    expect(result.fallback).toBe("adapter_load_failed");
    expect(JSON.stringify(result)).not.toContain("BEGIN OPENSSH");
    expect(JSON.stringify(result)).not.toContain("PEM body leak");
  });
});
