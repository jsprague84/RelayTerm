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
 * Stable identifiers for the swappable renderer adapters as seen by the
 * production shell. Mirrors the dev-lab's {@link RendererId} type from
 * `lib/dev/rendererDiagnostics.ts` so the SMOKE selectors
 * (`renderer-option-<id>`) match across surfaces; both lists must stay
 * in sync, but they cannot share a module (the production shell is
 * forbidden from importing `lib/dev/` per the isolation rule).
 *
 * xterm is the production compatibility baseline and the default; the
 * other three are experimental and only mount when the operator flips
 * the experimental-renderer-evaluation gate AND picks them. See
 * `docs/terminal-renderer-evaluation.md` § "Promotion criteria".
 */
export type RendererId = "xterm" | "ghostty-web" | "restty" | "wterm";

export const RENDERER_IDS: readonly RendererId[] = [
  "xterm",
  "ghostty-web",
  "restty",
  "wterm",
] as const;

/**
 * The production compatibility baseline. Used as the fallback for every
 * "unknown / experimental-but-gate-off / load-failed" path so the
 * production shell never lands on a renderer the operator did not
 * explicitly opt into.
 */
export const DEFAULT_RENDERER_ID: RendererId = "xterm";

export function isRendererId(value: unknown): value is RendererId {
  return (
    typeof value === "string" &&
    (RENDERER_IDS as readonly string[]).includes(value)
  );
}

/**
 * Operator-facing label for a renderer. The exact wording is the same
 * shape as the dev lab's labels (`xterm baseline`, `ghostty-web
 * experimental`, …) so the SMOKE runbook can read it directly off the
 * production shell. A future promotion would flip the wording in lockstep
 * with the gate posture.
 */
export function rendererLabel(id: RendererId): string {
  switch (id) {
    case "xterm":
      return "xterm baseline";
    case "ghostty-web":
      return "ghostty-web experimental";
    case "restty":
      return "restty experimental";
    case "wterm":
      return "wterm experimental";
  }
}

export function isExperimentalRenderer(id: RendererId): boolean {
  return id !== DEFAULT_RENDERER_ID;
}

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
 * Persisted shape (v2). Adding a field is a breaking change relative to
 * existing localStorage entries — the storage key is bumped on schema
 * additions and the legacy key is migrated through
 * {@link LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS}.
 *
 * The struct intentionally stores the theme PRESET ID rather than the
 * full {@link RendererTheme}. This keeps presets editable in code (a
 * preset tweak is picked up by every saved entry) and makes the saved
 * footprint small. Custom palettes (per-color overrides) are explicit
 * future work; landing them safely will need a new schema version.
 *
 * Schema history:
 *  - v1 (`relayterm.terminal-settings.v1`) — initial nine fields.
 *  - v2 (`relayterm.terminal-settings.v2`) — adds {@link autofitEnabled}
 *    so the renderer-neutral `BaseTerminalRendererOptions.autofit`
 *    capability can be toggled per-browser. Migration is non-destructive:
 *    a v1 entry is read on first load when no v2 entry exists, the
 *    missing `autofitEnabled` defaults to `false` (renderer-neutral
 *    autofit ships OFF by default), and the next save writes v2.
 */
export interface TerminalSettings {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  cursorStyle: RendererCursorStyle;
  cursorBlink: boolean;
  scrollbackLines: number;
  themePresetId: string;
  /**
   * Selected renderer. Persisted but only honored at attach time when
   * {@link experimentalRendererEvaluationEnabled} is also true (or the
   * id is `xterm`). The loader collapses every "selected but blocked"
   * path back to {@link DEFAULT_RENDERER_ID}, so a stale persisted
   * `ghostty-web` after the gate was turned back off does not mount
   * the experimental adapter.
   */
  rendererId: RendererId;
  /**
   * Operator opt-in for the experimental renderer evaluation. Off by
   * default. Carries the SAME contract whether read from a fresh entry,
   * a stale entry, or an unrecognised value: only an explicit `true`
   * unlocks the experimental adapters.
   */
  experimentalRendererEvaluationEnabled: boolean;
  /**
   * Operator opt-in for the renderer-neutral
   * {@link BaseTerminalRendererOptions.autofit} capability — "keep the
   * cell grid fitted to the workspace container". Off by default so
   * fresh users see zero behaviour change. xterm and wterm honour it
   * with their own container-observation paths; ghostty-web and restty
   * accept the option and report `autofitActive()` as `false` honestly.
   * Local-only browser preference; never sent to or stored by the
   * backend.
   */
  autofitEnabled: boolean;
}

export const TERMINAL_SETTINGS_STORAGE_KEY = "relayterm.terminal-settings.v2";

/**
 * Legacy storage keys the loader migrates from when the current
 * {@link TERMINAL_SETTINGS_STORAGE_KEY} is missing. Ordered most-recent
 * first so a future v3 bump that adds `v2` here keeps reading the most
 * recent migrate path first. Exposed so tests can pin the migration
 * source and a future operator-facing data-export feature can find it.
 *
 * Reading from a legacy key is non-destructive: the legacy entry stays
 * on the storage host until a subsequent {@link saveTerminalSettings}
 * call writes the current key. The legacy entry is then ignored on
 * future loads (the current-key branch wins). This is deliberate —
 * silently deleting a legacy entry would surprise an operator who
 * downgraded the app for an unrelated reason.
 */
export const LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS: readonly string[] = [
  "relayterm.terminal-settings.v1",
] as const;

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
    rendererId: DEFAULT_RENDERER_ID,
    experimentalRendererEvaluationEnabled: false,
    autofitEnabled: false,
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
 * unknown theme preset ids fall back to the default preset id; unknown
 * renderer ids fall back to {@link DEFAULT_RENDERER_ID}; and the
 * experimental gate boolean only accepts the literal `true`.
 *
 * The function NEVER throws and NEVER reads keys other than the nine
 * documented fields (the seven cosmetic ones plus `rendererId` and
 * `experimentalRendererEvaluationEnabled`) — a hostile entry that
 * injects extra keys cannot smuggle anything onto the parsed object.
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

  const rendererIdRaw = raw["rendererId"];
  const rendererId: RendererId = isRendererId(rendererIdRaw)
    ? rendererIdRaw
    : DEFAULT_RENDERER_ID;

  // Only the literal boolean `true` enables the gate. Any other value
  // (missing, malformed, truthy-string) collapses to `false` — the
  // experimental gate must be explicit, not coerced.
  const experimentalRaw = raw["experimentalRendererEvaluationEnabled"];
  const experimentalRendererEvaluationEnabled = experimentalRaw === true;

  // Same strict-boolean contract for renderer-neutral autofit: a
  // truthy string ("yes") or a number (1) must NOT coerce. Default
  // false matches the design ("ships zero behaviour change until an
  // operator opts in").
  const autofitRaw = raw["autofitEnabled"];
  const autofitEnabled = autofitRaw === true;

  return {
    fontFamily,
    fontSize,
    lineHeight,
    cursorStyle,
    cursorBlink,
    scrollbackLines,
    themePresetId,
    rendererId,
    experimentalRendererEvaluationEnabled,
    autofitEnabled,
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
  // Try the current storage key first. If it exists (even if malformed),
  // we treat its outcome as authoritative — we do NOT fall back to a
  // legacy key on a malformed current entry, because that would mask a
  // corrupted user setting with stale data.
  let currentRaw: string | null;
  try {
    currentRaw = store.getItem(TERMINAL_SETTINGS_STORAGE_KEY);
  } catch {
    return defaultTerminalSettings();
  }
  if (currentRaw !== null) {
    try {
      return parseTerminalSettings(JSON.parse(currentRaw));
    } catch {
      return defaultTerminalSettings();
    }
  }
  // Current key is missing. Walk legacy keys (most-recent first) for a
  // migration source. A malformed legacy entry also collapses to
  // defaults — we do NOT skip to the next legacy key because the keys
  // form a single migration chain, not a fallback chain.
  for (const legacyKey of LEGACY_TERMINAL_SETTINGS_STORAGE_KEYS) {
    let legacyRaw: string | null;
    try {
      legacyRaw = store.getItem(legacyKey);
    } catch {
      return defaultTerminalSettings();
    }
    if (legacyRaw === null) continue;
    try {
      return parseTerminalSettings(JSON.parse(legacyRaw));
    } catch {
      return defaultTerminalSettings();
    }
  }
  return defaultTerminalSettings();
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
 * passes the result through the renderer loader
 * (`apps/web/src/lib/app/terminal/rendererLoader.ts`) which constructs
 * the selected adapter — xterm on the default path, or one of the
 * experimental adapters when the operator gate is on. Every adapter
 * accepts the same renderer-neutral shape; nothing here is xterm-
 * specific.
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
    autofit: settings.autofitEnabled,
  };
}

/**
 * Pick the renderer to mount for a given settings snapshot. xterm is the
 * compatibility baseline and is always allowed; an experimental id is
 * honored only when {@link TerminalSettings.experimentalRendererEvaluationEnabled}
 * is `true`. This is the single place gate evaluation happens — the
 * production terminal workspace, the diagnostics surface, and the
 * Settings UI all flow through this helper so a future tweak (adding a
 * URL-parameter override, scoping the gate per-device) lands in one
 * place.
 */
export function effectiveRendererId(settings: TerminalSettings): RendererId {
  if (settings.rendererId === DEFAULT_RENDERER_ID) {
    return DEFAULT_RENDERER_ID;
  }
  return settings.experimentalRendererEvaluationEnabled
    ? settings.rendererId
    : DEFAULT_RENDERER_ID;
}
