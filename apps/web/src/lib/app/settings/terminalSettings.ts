/**
 * Production terminal preferences model.
 *
 * Scope: local-only browser preferences for the production terminal
 * workspace. NOT a backend settings API, NOT a per-user / per-account
 * settings store, NOT a per-server-profile override surface, NOT a
 * theme-import/export pipeline. Each of those is explicit future work.
 *
 * Architecture rules (load-bearing):
 *  - Settings are renderer-neutral by construction. The persisted
 *    snapshot maps cleanly onto {@link BaseTerminalRendererOptions} from
 *    `@relayterm/terminal-core` plus a preset id; nothing xterm-specific
 *    leaks into this surface.
 *  - The persisted snapshot stores ONLY cosmetic preferences. It MUST
 *    NOT carry secrets, server profiles, identities, public/private
 *    keys, session ids, or terminal output. The redaction tests in
 *    `tests/terminalSettings.test.ts` pin sentinel strings absent from
 *    `serializeSettings`.
 *  - localStorage parse failures (missing key, malformed JSON, wrong
 *    schema, hostile fixture) MUST collapse to {@link defaultTerminalSettings}.
 *    Unknown / extra fields are dropped silently. Out-of-range values
 *    are clamped to the documented bounds rather than rejected — the
 *    UI still validates inputs before save, but the loader is the last
 *    line of defence against a corrupted entry locking the operator
 *    out of the terminal.
 */
import type {
  BaseTerminalRendererOptions,
  RendererCursorStyle,
  RendererTheme,
} from "@relayterm/terminal-core";
import {
  DEFAULT_THEME_PRESET_ID,
  findThemePreset,
  TERMINAL_THEME_PRESETS,
} from "./themePresets.js";

/**
 * Default font stack — same string `ProductionTerminal` shipped before
 * this slice. Ordering matters: ui-monospace first so the OS picks the
 * native programmer font; the named fonts cover users who installed
 * them; the generic fallbacks cover everything else.
 */
export const DEFAULT_FONT_FAMILY =
  'ui-monospace, "JetBrains Mono", "Fira Code", "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace';

export const FONT_SIZE_MIN = 8;
export const FONT_SIZE_MAX = 32;
export const DEFAULT_FONT_SIZE = 13;

export const LINE_HEIGHT_MIN = 0.8;
export const LINE_HEIGHT_MAX = 2.5;
export const DEFAULT_LINE_HEIGHT = 1.0;

export const SCROLLBACK_MIN = 0;
export const SCROLLBACK_MAX = 100_000;
export const DEFAULT_SCROLLBACK_LINES = 2_000;

/** Maximum length of a fontFamily string. Keeps the persisted entry small. */
export const FONT_FAMILY_MAX_LEN = 256;

export const CURSOR_STYLES: readonly RendererCursorStyle[] = [
  "block",
  "underline",
  "bar",
] as const;

export const DEFAULT_CURSOR_STYLE: RendererCursorStyle = "block";
export const DEFAULT_CURSOR_BLINK = true;

/**
 * Persisted shape (v1). Adding a field is a breaking change relative to
 * existing localStorage entries — bump the storage key (e.g.
 * `relayterm.terminal-settings.v2`) and migrate from the v1 read path.
 *
 * The struct intentionally stores the theme PRESET ID rather than the
 * full {@link RendererTheme}. This keeps presets editable in code (a
 * preset tweak is picked up by every saved entry) and makes the saved
 * footprint small. Custom palettes (per-color overrides) are explicit
 * future work; landing them safely will need a new schema version.
 */
export interface TerminalSettings {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  cursorStyle: RendererCursorStyle;
  cursorBlink: boolean;
  scrollbackLines: number;
  themePresetId: string;
}

export const TERMINAL_SETTINGS_STORAGE_KEY = "relayterm.terminal-settings.v1";

/**
 * Returns a fresh defaults object. The function is preferred over a
 * frozen constant so callers can mutate the returned struct (the
 * settings UI keeps a mutable draft) without `Object.assign({}, ...)`
 * boilerplate at every call site.
 */
export function defaultTerminalSettings(): TerminalSettings {
  return {
    fontFamily: DEFAULT_FONT_FAMILY,
    fontSize: DEFAULT_FONT_SIZE,
    lineHeight: DEFAULT_LINE_HEIGHT,
    cursorStyle: DEFAULT_CURSOR_STYLE,
    cursorBlink: DEFAULT_CURSOR_BLINK,
    scrollbackLines: DEFAULT_SCROLLBACK_LINES,
    themePresetId: DEFAULT_THEME_PRESET_ID,
  };
}

function clampNumber(value: number, min: number, max: number): number {
  if (value < min) return min;
  if (value > max) return max;
  return value;
}

export function clampFontSize(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_FONT_SIZE;
  return clampNumber(Math.round(value), FONT_SIZE_MIN, FONT_SIZE_MAX);
}

export function clampLineHeight(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_LINE_HEIGHT;
  // Round to two decimals so the persisted form does not drift through
  // float quirks (`1.4 - 0.1 === 1.2999999...`).
  const rounded = Math.round(value * 100) / 100;
  return clampNumber(rounded, LINE_HEIGHT_MIN, LINE_HEIGHT_MAX);
}

export function clampScrollbackLines(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_SCROLLBACK_LINES;
  return clampNumber(Math.trunc(value), SCROLLBACK_MIN, SCROLLBACK_MAX);
}

export function isCursorStyle(value: unknown): value is RendererCursorStyle {
  return (
    typeof value === "string" &&
    (CURSOR_STYLES as readonly string[]).includes(value)
  );
}

// Match ASCII control characters (U+0000-U+001F + U+007F). Built via
// String.fromCharCode so the source file itself stays free of literal
// control bytes that linters / pre-commit scanners trip on.
const CONTROL_CHAR_RE = new RegExp(
  `[${String.fromCharCode(0)}-${String.fromCharCode(0x1f)}${String.fromCharCode(0x7f)}]`,
  "g",
);

export function sanitizeFontFamily(value: string): string {
  // Strip control characters (including newlines / tabs) — a font-family
  // value should be a CSS string. We do not strip quotes; CSS allows
  // quoted family names like `"JetBrains Mono"`.
  const cleaned = value.replace(CONTROL_CHAR_RE, "").trim();
  if (cleaned.length === 0) return DEFAULT_FONT_FAMILY;
  if (cleaned.length > FONT_FAMILY_MAX_LEN) {
    return cleaned.slice(0, FONT_FAMILY_MAX_LEN);
  }
  return cleaned;
}

function pickString(raw: Record<string, unknown>, key: string): string | null {
  const value = raw[key];
  return typeof value === "string" ? value : null;
}

function pickNumber(raw: Record<string, unknown>, key: string): number | null {
  const value = raw[key];
  return typeof value === "number" ? value : null;
}

function pickBoolean(raw: Record<string, unknown>, key: string): boolean | null {
  const value = raw[key];
  return typeof value === "boolean" ? value : null;
}

/**
 * Coerce an arbitrary parsed value (typically `JSON.parse(localStorage)`
 * output) into a complete {@link TerminalSettings}. Unknown / wrongly-
 * typed fields fall back to defaults; out-of-range numerics are clamped;
 * unknown theme preset ids fall back to the default preset id.
 *
 * The function NEVER throws and NEVER reads keys other than the seven
 * documented fields — a hostile entry that injects extra keys cannot
 * smuggle anything onto the parsed object.
 */
export function parseTerminalSettings(input: unknown): TerminalSettings {
  if (input === null || typeof input !== "object" || Array.isArray(input)) {
    return defaultTerminalSettings();
  }
  const raw = input as Record<string, unknown>;

  const fontFamilyRaw = pickString(raw, "fontFamily");
  const fontFamily =
    fontFamilyRaw === null
      ? DEFAULT_FONT_FAMILY
      : sanitizeFontFamily(fontFamilyRaw);

  const fontSizeRaw = pickNumber(raw, "fontSize");
  const fontSize =
    fontSizeRaw === null ? DEFAULT_FONT_SIZE : clampFontSize(fontSizeRaw);

  const lineHeightRaw = pickNumber(raw, "lineHeight");
  const lineHeight =
    lineHeightRaw === null ? DEFAULT_LINE_HEIGHT : clampLineHeight(lineHeightRaw);

  const cursorStyleRaw = raw["cursorStyle"];
  const cursorStyle: RendererCursorStyle = isCursorStyle(cursorStyleRaw)
    ? cursorStyleRaw
    : DEFAULT_CURSOR_STYLE;

  const cursorBlinkRaw = pickBoolean(raw, "cursorBlink");
  const cursorBlink =
    cursorBlinkRaw === null ? DEFAULT_CURSOR_BLINK : cursorBlinkRaw;

  const scrollbackLinesRaw = pickNumber(raw, "scrollbackLines");
  const scrollbackLines =
    scrollbackLinesRaw === null
      ? DEFAULT_SCROLLBACK_LINES
      : clampScrollbackLines(scrollbackLinesRaw);

  const themePresetIdRaw = pickString(raw, "themePresetId");
  const themePresetId =
    themePresetIdRaw !== null && findThemePreset(themePresetIdRaw) !== null
      ? themePresetIdRaw
      : DEFAULT_THEME_PRESET_ID;

  return {
    fontFamily,
    fontSize,
    lineHeight,
    cursorStyle,
    cursorBlink,
    scrollbackLines,
    themePresetId,
  };
}

/**
 * Project an arbitrary settings draft into the canonical shape — the
 * same path {@link parseTerminalSettings} runs but on an already-typed
 * input. Useful when the UI has built a draft via two-way bindings and
 * needs to apply the same clamp/sanitize rules before saving. Returns a
 * fresh object so the caller's draft is not mutated.
 */
export function normalizeTerminalSettings(
  draft: TerminalSettings,
): TerminalSettings {
  return parseTerminalSettings(draft);
}

/**
 * Serialize for localStorage. Returns the canonical JSON string of the
 * normalized settings — never the draft as-supplied. Centralising the
 * normalization here means the redaction sentinel tests can pin the
 * single string the storage key sees.
 */
export function serializeSettings(settings: TerminalSettings): string {
  return JSON.stringify(normalizeTerminalSettings(settings));
}

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function storage(): StorageLike | null {
  // Wrapped so SSR / test environments without `localStorage` don't
  // break import. The Settings view runs in the browser; the helpers
  // are imported at module-load time but are guarded.
  try {
    if (typeof globalThis === "undefined") return null;
    const store = (globalThis as { localStorage?: StorageLike }).localStorage;
    return store ?? null;
  } catch {
    return null;
  }
}

/**
 * Load settings from localStorage. Any failure path — missing key,
 * unavailable storage, JSON parse error, schema mismatch — collapses to
 * {@link defaultTerminalSettings} silently. Errors are NEVER logged or
 * surfaced; the only signal a caller gets is "you got the defaults",
 * which is exactly the right thing for a cosmetic preference.
 */
export function loadTerminalSettings(): TerminalSettings {
  const store = storage();
  if (!store) return defaultTerminalSettings();
  let raw: string | null;
  try {
    raw = store.getItem(TERMINAL_SETTINGS_STORAGE_KEY);
  } catch {
    return defaultTerminalSettings();
  }
  if (raw === null) return defaultTerminalSettings();
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return defaultTerminalSettings();
  }
  return parseTerminalSettings(parsed);
}

/**
 * Save settings to localStorage. Returns `true` on success, `false` on
 * any failure (storage unavailable, quota exceeded, etc.). The settings
 * view uses the boolean to render a non-fatal "couldn't save locally"
 * note without surfacing the underlying error string (which can include
 * origin/quota detail).
 */
export function saveTerminalSettings(settings: TerminalSettings): boolean {
  const store = storage();
  if (!store) return false;
  try {
    store.setItem(TERMINAL_SETTINGS_STORAGE_KEY, serializeSettings(settings));
    return true;
  } catch {
    return false;
  }
}

/** Remove the persisted entry. Used by the "Reset to defaults" action. */
export function clearTerminalSettings(): void {
  const store = storage();
  if (!store) return;
  try {
    store.removeItem(TERMINAL_SETTINGS_STORAGE_KEY);
  } catch {
    // Same swallow rationale as `saveTerminalSettings`.
  }
}

/**
 * Resolve the {@link RendererTheme} for a settings snapshot. Used by the
 * settings preview and the renderer-options mapper.
 */
export function resolveTheme(settings: TerminalSettings): RendererTheme {
  const preset =
    findThemePreset(settings.themePresetId) ?? TERMINAL_THEME_PRESETS[0];
  // Defensive: TERMINAL_THEME_PRESETS is non-empty by construction; the
  // bang-style fallback would also work but the explicit check keeps
  // the assertion local.
  return preset.theme;
}

/**
 * Map a settings snapshot onto the renderer-neutral options shape every
 * `TerminalRenderer` adapter accepts. The production terminal workspace
 * passes the result straight to `new XtermRenderer(...)`; a future
 * production renderer would consume the same object unchanged.
 */
export function settingsToRendererOptions(
  settings: TerminalSettings,
): Required<BaseTerminalRendererOptions> {
  return {
    fontFamily: settings.fontFamily,
    fontSize: settings.fontSize,
    lineHeight: settings.lineHeight,
    cursorStyle: settings.cursorStyle,
    cursorBlink: settings.cursorBlink,
    scrollbackLines: settings.scrollbackLines,
    theme: resolveTheme(settings),
  };
}
