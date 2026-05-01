/**
 * Theme presets for the production terminal preferences UI.
 *
 * Presets are renderer-neutral: each entry is a plain
 * {@link RendererTheme} from `@relayterm/terminal-core`. Adapter packages
 * (today only `@relayterm/terminal-xterm` in production) map the neutral
 * shape onto their renderer's own theme type during option translation,
 * so a preset works for any current or future adapter without per-
 * renderer branching here.
 *
 * Scope is deliberately small. This is NOT an Alacritty-port, NOT a
 * theme marketplace, NOT user-import/export. Adding a preset is cheap;
 * the curated set stays small until the production terminal grows real
 * theming surfaces (per-profile overrides, custom palettes, etc.).
 */
import type { RendererTheme } from "@relayterm/terminal-core";

export interface TerminalThemePreset {
  /** Stable id; persisted in localStorage. Never user-facing. */
  readonly id: string;
  /** Operator-facing label. Tweak freely; not load-bearing. */
  readonly label: string;
  /** One-line "feel" hint shown next to the select. */
  readonly description: string;
  readonly theme: RendererTheme;
}

/**
 * The "house" theme — black-ish background, soft foreground, no ANSI
 * overrides. Mirrors the inline defaults that `ProductionTerminal`
 * shipped before the settings slice landed, so an operator who upgrades
 * sees no surprise visual change until they explicitly pick a different
 * preset.
 */
const RELAYTERM_DARK: TerminalThemePreset = {
  id: "relayterm-dark",
  label: "RelayTerm Dark",
  description: "House default — neutral dark background, soft foreground.",
  theme: {
    background: "#0a0a0a",
    foreground: "#e4e4e7",
    cursor: "#e4e4e7",
    selectionBackground: "#3f3f46",
  },
};

/**
 * Alacritty-ish dark — a warmer near-black background and a saturated
 * 16-slot palette in the spirit of the `alacritty` reference theme.
 * This is NOT a port and we make NO claim of byte-for-byte parity; the
 * label is "Alacritty-ish" deliberately. The preset exists because the
 * Alacritty palette is what most operators expect when they ask for
 * "the terminal look."
 */
const ALACRITTY_ISH_DARK: TerminalThemePreset = {
  id: "alacritty-ish-dark",
  label: "Alacritty-ish Dark",
  description: "Warmer dark background with a saturated 16-slot palette.",
  theme: {
    background: "#1d1f21",
    foreground: "#c5c8c6",
    cursor: "#c5c8c6",
    selectionBackground: "#373b41",
    black: "#1d1f21",
    red: "#cc6666",
    green: "#b5bd68",
    yellow: "#f0c674",
    blue: "#81a2be",
    magenta: "#b294bb",
    cyan: "#8abeb7",
    white: "#c5c8c6",
    brightBlack: "#666666",
    brightRed: "#d54e53",
    brightGreen: "#b9ca4a",
    brightYellow: "#e7c547",
    brightBlue: "#7aa6da",
    brightMagenta: "#c397d8",
    brightCyan: "#70c0b1",
    brightWhite: "#eaeaea",
  },
};

const HIGH_CONTRAST: TerminalThemePreset = {
  id: "high-contrast",
  label: "High Contrast",
  description: "Pure black on white-bright foreground for maximum legibility.",
  theme: {
    background: "#000000",
    foreground: "#ffffff",
    cursor: "#ffff00",
    selectionBackground: "#555555",
    black: "#000000",
    red: "#ff5555",
    green: "#55ff55",
    yellow: "#ffff55",
    blue: "#5599ff",
    magenta: "#ff55ff",
    cyan: "#55ffff",
    white: "#ffffff",
    brightBlack: "#888888",
    brightRed: "#ff8888",
    brightGreen: "#88ff88",
    brightYellow: "#ffff88",
    brightBlue: "#88aaff",
    brightMagenta: "#ff88ff",
    brightCyan: "#88ffff",
    brightWhite: "#ffffff",
  },
};

const SOLARIZED_DARK: TerminalThemePreset = {
  id: "solarized-dark",
  label: "Solarized Dark",
  description: "Classic blue-tinted muted palette.",
  theme: {
    background: "#002b36",
    foreground: "#839496",
    cursor: "#93a1a1",
    selectionBackground: "#073642",
    black: "#073642",
    red: "#dc322f",
    green: "#859900",
    yellow: "#b58900",
    blue: "#268bd2",
    magenta: "#d33682",
    cyan: "#2aa198",
    white: "#eee8d5",
    brightBlack: "#586e75",
    brightRed: "#cb4b16",
    brightGreen: "#586e75",
    brightYellow: "#657b83",
    brightBlue: "#839496",
    brightMagenta: "#6c71c4",
    brightCyan: "#93a1a1",
    brightWhite: "#fdf6e3",
  },
};

export const TERMINAL_THEME_PRESETS: readonly TerminalThemePreset[] = [
  RELAYTERM_DARK,
  ALACRITTY_ISH_DARK,
  HIGH_CONTRAST,
  SOLARIZED_DARK,
] as const;

export const DEFAULT_THEME_PRESET_ID = RELAYTERM_DARK.id;

/**
 * Look up a preset by id. Returns `null` for unknown ids — callers that
 * need a fallback should compose with {@link DEFAULT_THEME_PRESET_ID}
 * explicitly so the substitution is visible at the call site.
 */
export function findThemePreset(id: string): TerminalThemePreset | null {
  for (const preset of TERMINAL_THEME_PRESETS) {
    if (preset.id === id) return preset;
  }
  return null;
}
