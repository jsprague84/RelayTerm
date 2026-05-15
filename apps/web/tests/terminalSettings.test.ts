import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  CURSOR_STYLES,
  DEFAULT_CURSOR_STYLE,
  DEFAULT_FONT_FAMILY,
  DEFAULT_FONT_SIZE,
  DEFAULT_LINE_HEIGHT,
  DEFAULT_RENDERER_ID,
  DEFAULT_SCROLLBACK_LINES,
  FONT_FAMILY_MAX_LEN,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS,
  LINE_HEIGHT_MAX,
  LINE_HEIGHT_MIN,
  RENDERER_IDS,
  SCROLLBACK_MAX,
  SCROLLBACK_MIN,
  TERMINAL_SETTINGS_STORAGE_KEY,
  clampFontSize,
  clampLineHeight,
  clampScrollbackLines,
  clearTerminalSettings,
  defaultTerminalSettings,
  effectiveRendererId,
  isCursorStyle,
  isExperimentalRenderer,
  isRendererId,
  loadTerminalSettings,
  normalizeTerminalSettings,
  parseTerminalSettings,
  rendererLabel,
  resolveTheme,
  saveTerminalSettings,
  sanitizeFontFamily,
  serializeSettings,
  settingsToRendererOptions,
  type TerminalSettings,
} from "../src/lib/app/settings/terminalSettings.js";
import {
  DEFAULT_THEME_PRESET_ID,
  TERMINAL_THEME_PRESETS,
  findThemePreset,
} from "../src/lib/app/settings/themePresets.js";

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, value);
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  clear(): void {
    this.map.clear();
  }
}

const STORAGE_HOST = globalThis as { localStorage?: unknown };
let storage: MemoryStorage;
let originalStorage: unknown;

beforeEach(() => {
  storage = new MemoryStorage();
  originalStorage = STORAGE_HOST.localStorage;
  Object.defineProperty(globalThis, "localStorage", {
    value: storage,
    configurable: true,
    writable: true,
  });
});

afterEach(() => {
  if (originalStorage === undefined) {
    delete (globalThis as { localStorage?: unknown }).localStorage;
  } else {
    Object.defineProperty(globalThis, "localStorage", {
      value: originalStorage,
      configurable: true,
      writable: true,
    });
  }
  vi.restoreAllMocks();
});

describe("defaults", () => {
  it("has the documented shape", () => {
    const d = defaultTerminalSettings();
    expect(d.fontFamily).toBe(DEFAULT_FONT_FAMILY);
    expect(d.fontSize).toBe(DEFAULT_FONT_SIZE);
    expect(d.lineHeight).toBe(DEFAULT_LINE_HEIGHT);
    expect(d.cursorStyle).toBe(DEFAULT_CURSOR_STYLE);
    expect(d.cursorBlink).toBe(true);
    expect(d.scrollbackLines).toBe(DEFAULT_SCROLLBACK_LINES);
    expect(d.themePresetId).toBe(DEFAULT_THEME_PRESET_ID);
    expect(d.rendererId).toBe(DEFAULT_RENDERER_ID);
    expect(d.rendererId).toBe("xterm");
    expect(d.experimentalRendererEvaluationEnabled).toBe(false);
    // Renderer-neutral autofit defaults OFF. Fresh users see zero
    // behaviour change until they opt in via Settings.
    expect(d.autofitEnabled).toBe(false);
  });

  it("returns a fresh object so callers can mutate without aliasing", () => {
    const a = defaultTerminalSettings();
    const b = defaultTerminalSettings();
    expect(a).not.toBe(b);
    a.fontSize = 20;
    expect(b.fontSize).toBe(DEFAULT_FONT_SIZE);
  });
});

describe("clampFontSize", () => {
  it("rejects non-finite values", () => {
    expect(clampFontSize(Number.NaN)).toBe(DEFAULT_FONT_SIZE);
    expect(clampFontSize(Number.POSITIVE_INFINITY)).toBe(DEFAULT_FONT_SIZE);
  });
  it("clamps to bounds", () => {
    expect(clampFontSize(0)).toBe(FONT_SIZE_MIN);
    expect(clampFontSize(1_000)).toBe(FONT_SIZE_MAX);
  });
  it("rounds non-integers", () => {
    expect(clampFontSize(13.4)).toBe(13);
    expect(clampFontSize(13.6)).toBe(14);
  });
});

describe("clampLineHeight", () => {
  it("rejects non-finite values", () => {
    expect(clampLineHeight(Number.NaN)).toBe(DEFAULT_LINE_HEIGHT);
  });
  it("clamps to bounds", () => {
    expect(clampLineHeight(0)).toBe(LINE_HEIGHT_MIN);
    expect(clampLineHeight(99)).toBe(LINE_HEIGHT_MAX);
  });
  it("rounds to two decimals", () => {
    expect(clampLineHeight(1.234)).toBe(1.23);
    expect(clampLineHeight(1.4 - 0.1)).toBe(1.3);
  });
});

describe("clampScrollbackLines", () => {
  it("rejects non-finite values", () => {
    expect(clampScrollbackLines(Number.NaN)).toBe(DEFAULT_SCROLLBACK_LINES);
  });
  it("clamps to bounds", () => {
    expect(clampScrollbackLines(-100)).toBe(SCROLLBACK_MIN);
    expect(clampScrollbackLines(SCROLLBACK_MAX + 5)).toBe(SCROLLBACK_MAX);
  });
  it("truncates fractional values", () => {
    expect(clampScrollbackLines(2_000.9)).toBe(2_000);
  });
});

describe("isCursorStyle", () => {
  it("accepts the closed set", () => {
    for (const style of CURSOR_STYLES) {
      expect(isCursorStyle(style)).toBe(true);
    }
  });
  it("rejects everything else", () => {
    expect(isCursorStyle("ibeam")).toBe(false);
    expect(isCursorStyle(undefined)).toBe(false);
    expect(isCursorStyle(42)).toBe(false);
    expect(isCursorStyle(null)).toBe(false);
  });
});

describe("sanitizeFontFamily", () => {
  it("strips control characters", () => {
    const dirty = `JetBrains${String.fromCharCode(0x00)}\nMono${String.fromCharCode(0x7f)}`;
    expect(sanitizeFontFamily(dirty)).toBe("JetBrainsMono");
  });
  it("trims surrounding whitespace", () => {
    expect(sanitizeFontFamily("   Menlo   ")).toBe("Menlo");
  });
  it("falls back to the default when empty after stripping", () => {
    expect(sanitizeFontFamily("")).toBe(DEFAULT_FONT_FAMILY);
    expect(sanitizeFontFamily("   ")).toBe(DEFAULT_FONT_FAMILY);
  });
  it("clips overlong values", () => {
    const big = "x".repeat(FONT_FAMILY_MAX_LEN + 50);
    expect(sanitizeFontFamily(big).length).toBe(FONT_FAMILY_MAX_LEN);
  });
});

describe("findThemePreset / resolveTheme", () => {
  it("looks up presets by id", () => {
    for (const preset of TERMINAL_THEME_PRESETS) {
      expect(findThemePreset(preset.id)).toBe(preset);
    }
  });
  it("returns null for unknown ids", () => {
    expect(findThemePreset("not-a-preset")).toBeNull();
  });
  it("falls back to the first preset when the saved id is unknown", () => {
    const settings: TerminalSettings = {
      ...defaultTerminalSettings(),
      themePresetId: "ghost-preset",
    };
    expect(resolveTheme(settings)).toBe(TERMINAL_THEME_PRESETS[0]?.theme);
  });
});

describe("parseTerminalSettings", () => {
  it("returns defaults for non-objects", () => {
    expect(parseTerminalSettings(null)).toEqual(defaultTerminalSettings());
    expect(parseTerminalSettings("nope")).toEqual(defaultTerminalSettings());
    expect(parseTerminalSettings(42)).toEqual(defaultTerminalSettings());
    expect(parseTerminalSettings([])).toEqual(defaultTerminalSettings());
  });

  it("ignores unknown / extra fields", () => {
    const parsed = parseTerminalSettings({
      fontSize: 16,
      themePresetId: DEFAULT_THEME_PRESET_ID,
      // a hostile fixture cannot smuggle these onto the parsed object
      private_key: "PRIVATE",
      encrypted_private_key: "ENC",
      session_output: "should never persist",
      __proto__: { stolen: true },
    });
    expect(parsed.fontSize).toBe(16);
    expect((parsed as Record<string, unknown>).private_key).toBeUndefined();
    expect(
      (parsed as Record<string, unknown>).encrypted_private_key,
    ).toBeUndefined();
    expect((parsed as Record<string, unknown>).session_output).toBeUndefined();
  });

  it("clamps invalid numerics rather than rejecting the whole entry", () => {
    const parsed = parseTerminalSettings({
      fontSize: 1_000,
      lineHeight: -5,
      scrollbackLines: SCROLLBACK_MAX + 1,
    });
    expect(parsed.fontSize).toBe(FONT_SIZE_MAX);
    expect(parsed.lineHeight).toBe(LINE_HEIGHT_MIN);
    expect(parsed.scrollbackLines).toBe(SCROLLBACK_MAX);
    // unrelated fields fall back to defaults rather than spreading
    expect(parsed.fontFamily).toBe(DEFAULT_FONT_FAMILY);
    expect(parsed.cursorStyle).toBe(DEFAULT_CURSOR_STYLE);
  });

  it("rejects bogus cursor styles", () => {
    const parsed = parseTerminalSettings({ cursorStyle: "ibeam" });
    expect(parsed.cursorStyle).toBe(DEFAULT_CURSOR_STYLE);
  });

  it("rejects unknown theme preset ids", () => {
    const parsed = parseTerminalSettings({ themePresetId: "foo-bar" });
    expect(parsed.themePresetId).toBe(DEFAULT_THEME_PRESET_ID);
  });

  it("accepts a fully-valid object verbatim", () => {
    const input: TerminalSettings = {
      fontFamily: '"Iosevka", monospace',
      fontSize: 14,
      lineHeight: 1.2,
      cursorStyle: "underline",
      cursorBlink: false,
      scrollbackLines: 5_000,
      themePresetId: TERMINAL_THEME_PRESETS[1]?.id ?? DEFAULT_THEME_PRESET_ID,
      rendererId: "ghostty-web",
      experimentalRendererEvaluationEnabled: true,
      autofitEnabled: true,
    };
    expect(parseTerminalSettings(input)).toEqual(input);
  });
});

describe("autofitEnabled parsing", () => {
  it("defaults to false when the field is missing", () => {
    expect(parseTerminalSettings({}).autofitEnabled).toBe(false);
    expect(parseTerminalSettings({ fontSize: 14 }).autofitEnabled).toBe(false);
  });

  it("only accepts literal booleans true/false", () => {
    // Truthy strings, numbers, objects, arrays must NOT coerce. The
    // renderer-neutral autofit option is binary by contract; any
    // non-boolean lands as the default `false`.
    expect(parseTerminalSettings({ autofitEnabled: "true" }).autofitEnabled).toBe(false);
    expect(parseTerminalSettings({ autofitEnabled: 1 }).autofitEnabled).toBe(false);
    expect(parseTerminalSettings({ autofitEnabled: {} }).autofitEnabled).toBe(false);
    expect(parseTerminalSettings({ autofitEnabled: null }).autofitEnabled).toBe(false);
    expect(parseTerminalSettings({ autofitEnabled: true }).autofitEnabled).toBe(true);
    expect(parseTerminalSettings({ autofitEnabled: false }).autofitEnabled).toBe(false);
  });

  it("round-trips through save/load", () => {
    saveTerminalSettings({
      ...defaultTerminalSettings(),
      autofitEnabled: true,
    });
    expect(loadTerminalSettings().autofitEnabled).toBe(true);
  });

  it("hostile non-boolean stored entry collapses to false (no crash)", () => {
    storage.setItem(
      TERMINAL_SETTINGS_STORAGE_KEY,
      JSON.stringify({
        autofitEnabled: "yes",
      }),
    );
    const loaded = loadTerminalSettings();
    expect(loaded.autofitEnabled).toBe(false);
  });
});

describe("v1 → v2 storage migration", () => {
  it("LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS exposes the legacy v1 key explicitly", () => {
    // The migration path needs to be visible to test code AND to a
    // future operator-facing data-export feature. Pinning the legacy
    // key as a constant prevents a refactor from silently dropping the
    // migration source.
    expect(LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS).toContain(
      "relayterm.terminal-settings.v1",
    );
    // The current key MUST not be in the legacy list.
    expect(LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS).not.toContain(
      TERMINAL_SETTINGS_STORAGE_KEY,
    );
  });

  it("loads from a v1 entry when no v2 entry exists", () => {
    const v1 = {
      fontFamily: '"Iosevka", monospace',
      fontSize: 14,
      lineHeight: 1.2,
      cursorStyle: "underline" as const,
      cursorBlink: false,
      scrollbackLines: 5_000,
      themePresetId: DEFAULT_THEME_PRESET_ID,
      rendererId: "xterm" as const,
      experimentalRendererEvaluationEnabled: false,
    };
    storage.setItem("relayterm.terminal-settings.v1", JSON.stringify(v1));
    const loaded = loadTerminalSettings();
    // Every v1 field survives verbatim; autofitEnabled defaults false
    // because v1 entries pre-date the field.
    expect(loaded.fontSize).toBe(14);
    expect(loaded.fontFamily).toBe('"Iosevka", monospace');
    expect(loaded.cursorStyle).toBe("underline");
    expect(loaded.autofitEnabled).toBe(false);
  });

  it("prefers a v2 entry over a v1 entry when both exist", () => {
    storage.setItem(
      "relayterm.terminal-settings.v1",
      JSON.stringify({ fontSize: 22 }),
    );
    storage.setItem(
      TERMINAL_SETTINGS_STORAGE_KEY,
      JSON.stringify({ fontSize: 11 }),
    );
    expect(loadTerminalSettings().fontSize).toBe(11);
  });

  it("malformed v1 entry collapses to defaults (no v2 fallback chain back to v1)", () => {
    storage.setItem("relayterm.terminal-settings.v1", "{not-json");
    expect(loadTerminalSettings()).toEqual(defaultTerminalSettings());
  });
});

describe("normalizeTerminalSettings", () => {
  it("does not mutate the input draft", () => {
    const draft: TerminalSettings = {
      ...defaultTerminalSettings(),
      fontSize: 1_000,
    };
    const normalized = normalizeTerminalSettings(draft);
    expect(normalized.fontSize).toBe(FONT_SIZE_MAX);
    expect(draft.fontSize).toBe(1_000);
  });
});

describe("loadTerminalSettings (localStorage)", () => {
  it("returns defaults when the key is missing", () => {
    expect(loadTerminalSettings()).toEqual(defaultTerminalSettings());
  });

  it("returns defaults when the JSON is malformed", () => {
    storage.setItem(TERMINAL_SETTINGS_STORAGE_KEY, "{not-json");
    expect(loadTerminalSettings()).toEqual(defaultTerminalSettings());
  });

  it("returns defaults when the entry is the wrong shape", () => {
    storage.setItem(TERMINAL_SETTINGS_STORAGE_KEY, JSON.stringify(42));
    expect(loadTerminalSettings()).toEqual(defaultTerminalSettings());
  });

  it("ignores unknown fields on load (no smuggling)", () => {
    storage.setItem(
      TERMINAL_SETTINGS_STORAGE_KEY,
      JSON.stringify({
        fontSize: 18,
        themePresetId: DEFAULT_THEME_PRESET_ID,
        private_key: "PRIV",
        encrypted_private_key: "ENC",
        session_output: "should never persist",
        access_token: "BEARER ABC",
      }),
    );
    const loaded = loadTerminalSettings();
    expect(loaded.fontSize).toBe(18);
    const keys = Object.keys(loaded);
    expect(keys.sort()).toEqual(
      [
        "fontFamily",
        "fontSize",
        "lineHeight",
        "cursorStyle",
        "cursorBlink",
        "scrollbackLines",
        "themePresetId",
        "rendererId",
        "experimentalRendererEvaluationEnabled",
        "autofitEnabled",
      ].sort(),
    );
    expect(keys).not.toContain("private_key");
    expect(keys).not.toContain("encrypted_private_key");
    expect(keys).not.toContain("session_output");
    expect(keys).not.toContain("access_token");
  });

  it("does not log on parse failure", () => {
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    storage.setItem(TERMINAL_SETTINGS_STORAGE_KEY, "{not-json");
    loadTerminalSettings();
    expect(log).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
    expect(err).not.toHaveBeenCalled();
  });

  it("collapses to defaults when localStorage is unavailable", () => {
    Object.defineProperty(globalThis, "localStorage", {
      value: undefined,
      configurable: true,
      writable: true,
    });
    expect(loadTerminalSettings()).toEqual(defaultTerminalSettings());
  });
});

describe("saveTerminalSettings + reset", () => {
  it("round-trips through normalize", () => {
    const out: TerminalSettings = {
      ...defaultTerminalSettings(),
      fontSize: 1_000, // out of range
      lineHeight: -5,
    };
    const ok = saveTerminalSettings(out);
    expect(ok).toBe(true);
    const back = loadTerminalSettings();
    expect(back.fontSize).toBe(FONT_SIZE_MAX);
    expect(back.lineHeight).toBe(LINE_HEIGHT_MIN);
  });

  it("returns false when storage throws", () => {
    const failing = new MemoryStorage();
    failing.setItem = () => {
      throw new Error("quota");
    };
    Object.defineProperty(globalThis, "localStorage", {
      value: failing,
      configurable: true,
      writable: true,
    });
    expect(saveTerminalSettings(defaultTerminalSettings())).toBe(false);
  });

  it("clearTerminalSettings removes the entry so the next load is defaults", () => {
    saveTerminalSettings({
      ...defaultTerminalSettings(),
      fontSize: 18,
    });
    expect(loadTerminalSettings().fontSize).toBe(18);
    clearTerminalSettings();
    expect(loadTerminalSettings()).toEqual(defaultTerminalSettings());
  });
});

describe("serializeSettings redaction", () => {
  it("never echoes a smuggled secret", () => {
    const draft = {
      ...defaultTerminalSettings(),
      // hostile cast: simulate an upstream layer assigning extra fields
      ...({
        private_key: "PRIVATE-DO-NOT-PERSIST",
        encrypted_private_key: "ENCRYPTED-DO-NOT-PERSIST",
        session_output: "RAW PTY OUTPUT",
        access_token: "BEARER ABC",
      } as object),
    } as TerminalSettings;
    const json = serializeSettings(draft);
    expect(json).not.toContain("PRIVATE-DO-NOT-PERSIST");
    expect(json).not.toContain("ENCRYPTED-DO-NOT-PERSIST");
    expect(json).not.toContain("RAW PTY OUTPUT");
    expect(json).not.toContain("BEARER ABC");
    expect(json).not.toMatch(/private_key/);
    expect(json).not.toMatch(/encrypted_private_key/);
    expect(json).not.toMatch(/session_output/);
    expect(json).not.toMatch(/access_token/);
  });
});

describe("renderer id helpers", () => {
  it("RENDERER_IDS covers exactly the four documented adapters", () => {
    expect([...RENDERER_IDS].sort()).toEqual(
      ["xterm", "ghostty-web", "restty", "wterm"].sort(),
    );
  });

  it("isRendererId rejects everything outside the closed set", () => {
    for (const id of RENDERER_IDS) {
      expect(isRendererId(id)).toBe(true);
    }
    expect(isRendererId("XTERM")).toBe(false);
    expect(isRendererId("native")).toBe(false);
    expect(isRendererId(undefined)).toBe(false);
    expect(isRendererId(null)).toBe(false);
    expect(isRendererId(0)).toBe(false);
  });

  it("rendererLabel matches the dev-lab wording (smoke contract)", () => {
    expect(rendererLabel("xterm")).toBe("xterm baseline");
    expect(rendererLabel("ghostty-web")).toBe("ghostty-web experimental");
    expect(rendererLabel("restty")).toBe("restty experimental");
    expect(rendererLabel("wterm")).toBe("wterm experimental");
  });

  it("isExperimentalRenderer is true for everything but xterm", () => {
    expect(isExperimentalRenderer("xterm")).toBe(false);
    expect(isExperimentalRenderer("ghostty-web")).toBe(true);
    expect(isExperimentalRenderer("restty")).toBe(true);
    expect(isExperimentalRenderer("wterm")).toBe(true);
  });
});

describe("rendererId / experimental gate parsing", () => {
  it("defaults to xterm when the persisted value is unknown", () => {
    const parsed = parseTerminalSettings({ rendererId: "native" });
    expect(parsed.rendererId).toBe("xterm");
  });

  it("keeps a valid persisted renderer id verbatim", () => {
    const parsed = parseTerminalSettings({ rendererId: "ghostty-web" });
    expect(parsed.rendererId).toBe("ghostty-web");
  });

  it("only accepts the literal boolean `true` for the experimental gate", () => {
    expect(
      parseTerminalSettings({}).experimentalRendererEvaluationEnabled,
    ).toBe(false);
    expect(
      parseTerminalSettings({ experimentalRendererEvaluationEnabled: "true" })
        .experimentalRendererEvaluationEnabled,
    ).toBe(false);
    expect(
      parseTerminalSettings({ experimentalRendererEvaluationEnabled: 1 })
        .experimentalRendererEvaluationEnabled,
    ).toBe(false);
    expect(
      parseTerminalSettings({ experimentalRendererEvaluationEnabled: true })
        .experimentalRendererEvaluationEnabled,
    ).toBe(true);
  });

  it("persisted renderer survives a save/load round-trip", () => {
    saveTerminalSettings({
      ...defaultTerminalSettings(),
      rendererId: "restty",
      experimentalRendererEvaluationEnabled: true,
    });
    const back = loadTerminalSettings();
    expect(back.rendererId).toBe("restty");
    expect(back.experimentalRendererEvaluationEnabled).toBe(true);
  });
});

describe("effectiveRendererId", () => {
  it("returns xterm when an experimental id is selected but the gate is off", () => {
    const s: TerminalSettings = {
      ...defaultTerminalSettings(),
      rendererId: "ghostty-web",
      experimentalRendererEvaluationEnabled: false,
    };
    expect(effectiveRendererId(s)).toBe("xterm");
  });

  it("returns the experimental id when both are set", () => {
    const s: TerminalSettings = {
      ...defaultTerminalSettings(),
      rendererId: "wterm",
      experimentalRendererEvaluationEnabled: true,
    };
    expect(effectiveRendererId(s)).toBe("wterm");
  });

  it("returns xterm verbatim even when the gate is on (baseline always allowed)", () => {
    const s: TerminalSettings = {
      ...defaultTerminalSettings(),
      rendererId: "xterm",
      experimentalRendererEvaluationEnabled: true,
    };
    expect(effectiveRendererId(s)).toBe("xterm");
  });

  it("a fresh defaults snapshot effectively resolves to xterm", () => {
    expect(effectiveRendererId(defaultTerminalSettings())).toBe("xterm");
  });
});

describe("settingsToRendererOptions", () => {
  it("maps every neutral field onto BaseTerminalRendererOptions", () => {
    const options = settingsToRendererOptions(defaultTerminalSettings());
    expect(options.fontFamily).toBe(DEFAULT_FONT_FAMILY);
    expect(options.fontSize).toBe(DEFAULT_FONT_SIZE);
    expect(options.lineHeight).toBe(DEFAULT_LINE_HEIGHT);
    expect(options.cursorStyle).toBe(DEFAULT_CURSOR_STYLE);
    expect(options.cursorBlink).toBe(true);
    expect(options.scrollbackLines).toBe(DEFAULT_SCROLLBACK_LINES);
    expect(options.theme).toBe(TERMINAL_THEME_PRESETS[0]?.theme);
    // Renderer-neutral autofit defaults OFF in the mapped options too.
    expect(options.autofit).toBe(false);
  });

  it("maps autofitEnabled:true onto autofit:true", () => {
    const options = settingsToRendererOptions({
      ...defaultTerminalSettings(),
      autofitEnabled: true,
    });
    expect(options.autofit).toBe(true);
  });

  it("does not leak any xterm-specific key", () => {
    const options = settingsToRendererOptions(defaultTerminalSettings());
    const keys = Object.keys(options).sort();
    expect(keys).toEqual(
      [
        "fontFamily",
        "fontSize",
        "lineHeight",
        "cursorStyle",
        "cursorBlink",
        "scrollbackLines",
        "theme",
        "autofit",
      ].sort(),
    );
    expect((options as Record<string, unknown>).xtermOnly).toBeUndefined();
  });
});
