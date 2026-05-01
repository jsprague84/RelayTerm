import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  CURSOR_STYLES,
  DEFAULT_CURSOR_STYLE,
  DEFAULT_FONT_FAMILY,
  DEFAULT_FONT_SIZE,
  DEFAULT_LINE_HEIGHT,
  DEFAULT_SCROLLBACK_LINES,
  FONT_FAMILY_MAX_LEN,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  LINE_HEIGHT_MAX,
  LINE_HEIGHT_MIN,
  SCROLLBACK_MAX,
  SCROLLBACK_MIN,
  TERMINAL_SETTINGS_STORAGE_KEY,
  clampFontSize,
  clampLineHeight,
  clampScrollbackLines,
  clearTerminalSettings,
  defaultTerminalSettings,
  isCursorStyle,
  loadTerminalSettings,
  normalizeTerminalSettings,
  parseTerminalSettings,
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
    };
    expect(parseTerminalSettings(input)).toEqual(input);
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
      ].sort(),
    );
    expect((options as Record<string, unknown>).xtermOnly).toBeUndefined();
  });
});
