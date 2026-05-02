/**
 * Renderer-neutral paste policy for the production terminal workspace.
 *
 * The policy is a pure function of paste-candidate text. It returns a
 * {@link PasteDecision} describing the risk class plus the metadata the
 * confirmation UI needs (line count, byte length, reason code, a static
 * operator-facing message). It intentionally does NOT carry the paste
 * content back through the decision object — the integration layer holds
 * the original text in a closure-scoped variable and clears it on send /
 * cancel / dispose. The decision object is safe to put in Svelte state,
 * stringify, or render to the DOM.
 *
 * Redaction posture (load-bearing):
 *  - The decision object NEVER includes the original paste text or any
 *    fragment of it.
 *  - {@link describePasteDecision} returns a STATIC operator-facing
 *    string keyed off the reason code only. It does not interpolate the
 *    paste content.
 *  - This module performs no I/O. It does not log, throw with payload
 *    bytes in the message, or persist anything.
 *  - The `safeUserMessage` field on the decision is the same static
 *    string the integration uses for the confirm/block panel.
 *
 * Scope:
 *  - Frontend-only. The backend never sees this policy. The wire
 *    {@link `client.sendInput`} surface remains unchanged; the policy
 *    just decides whether the integration layer forwards, holds for
 *    confirmation, or drops.
 *  - The neutral `TerminalRenderer` interface in `terminal-core` is
 *    untouched. The integration runs the policy on the bytes it would
 *    have forwarded to `client.sendInput` and decides per the decision.
 */

/**
 * Maximum length for a paste-candidate that should bypass the strict
 * policy and be treated as a keystroke / IME commit / special-key
 * sequence. Anything ≤ this length AND with no embedded newline is
 * forwarded immediately by the integration layer without running the
 * full risk analysis.
 *
 * The threshold is small on purpose: arrow keys (`\x1b[A`), function
 * keys (`\x1bOP`), and Alt-prefixed sequences fit comfortably; a true
 * paste of "ls" passes through silently as "safe" via the policy too.
 */
export const KEYSTROKE_MAX_LENGTH = 8;

/**
 * Paste byte size at or below which a single-line, control-char-free
 * paste is forwarded silently as "safe". Above this size (and below the
 * hard cap) the policy returns "confirm" so the operator gets a chance
 * to abort.
 */
export const PASTE_CONFIRM_BYTES = 4 * 1024;

/**
 * Paste byte size above which the policy returns "blocked". The hard
 * cap exists to prevent a runaway clipboard event from sending a multi-
 * megabyte payload to the remote shell — confirmation UI alone is
 * insufficient at that scale because the operator cannot meaningfully
 * review the content.
 */
export const PASTE_HARD_CAP_BYTES = 64 * 1024;

/**
 * Risk class for a paste-candidate.
 *  - `safe`: forward immediately, no UI.
 *  - `confirm`: hold for explicit operator confirmation; do NOT forward
 *    until the operator acks.
 *  - `blocked`: drop the paste; show a non-blocking note with the
 *    reason. The operator must paste again after fixing the source.
 */
export type PasteRisk = "safe" | "confirm" | "blocked";

/**
 * Closed enum of reason codes. Stable identifiers — UI strings keyed off
 * these codes live in {@link describePasteDecision}.
 */
export type PasteReasonCode =
  | "ok_empty"
  | "ok_keystroke"
  | "ok_single_line"
  | "multiline"
  | "large_payload"
  | "control_chars"
  | "bracketed_paste_markers"
  | "nul_byte"
  | "exceeds_hard_cap";

/**
 * Decision returned by {@link evaluatePaste} / {@link decidePaste}.
 *
 * The shape is intentionally flat and primitive-only so the entire
 * object can safely live in Svelte `$state`, be JSON-stringified for
 * test sentinels, or be rendered into the DOM. It carries METADATA only
 * — never the original paste text or any fragment of it.
 */
export interface PasteDecision {
  /** Risk class — see {@link PasteRisk}. */
  risk: PasteRisk;
  /** Stable identifier for the rule that produced this decision. */
  reasonCode: PasteReasonCode;
  /**
   * Number of lines in the paste-candidate. `1` for non-empty single-
   * line input; `0` for empty input. A trailing newline is counted as
   * an empty trailing line (so `"a\n"` has `lineCount = 2`), matching
   * the operator's mental model of "you pasted something with a newline
   * at the end".
   */
  lineCount: number;
  /** UTF-8 byte length of the paste-candidate. */
  byteLength: number;
  /** True when the input contains a non-tab, non-newline control char. */
  hasControlChars: boolean;
  /** True when the input contains the bracketed-paste markers. */
  hasBracketedPasteMarkers: boolean;
  /**
   * Operator-facing static string keyed off `reasonCode`. The
   * integration layer uses this directly for the confirm/block panel
   * heading. It NEVER contains the paste content.
   */
  safeUserMessage: string;
}

const ENCODER = new TextEncoder();

/**
 * UTF-8 byte length of `text`. Centralised so the integration and tests
 * agree on the unit reported in the decision object.
 */
export function pasteByteLength(text: string): number {
  return ENCODER.encode(text).length;
}

/**
 * Count lines as the operator perceives them: the number of `\n`-
 * separated segments after normalising `\r\n` and bare `\r` to `\n`.
 * Empty input returns `0`; a non-empty string with no newline returns
 * `1`; `"a\n"` returns `2` (the trailing empty line is counted so the
 * operator sees that the paste ends with a newline).
 */
export function pasteLineCount(text: string): number {
  if (text.length === 0) return 0;
  const normalised = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  return normalised.split("\n").length;
}

const NUL_BYTE = "\u0000";
const BRACKETED_PASTE_START = "\x1b[200~";
const BRACKETED_PASTE_END = "\x1b[201~";

function containsBracketedPasteMarkers(text: string): boolean {
  return (
    text.includes(BRACKETED_PASTE_START) || text.includes(BRACKETED_PASTE_END)
  );
}

/**
 * True if `text` contains an ASCII control character that is not tab,
 * line feed, or carriage return. Tab is whitelisted because pasted
 * indented snippets are normal. LF / CR are accounted for separately
 * via {@link pasteLineCount}.
 *
 * This check is paste-focused: the integration layer must NOT call this
 * on raw keystroke bytes (arrow keys legitimately contain ESC). The
 * {@link isLikelyKeystroke} short-circuit in {@link evaluatePaste}
 * keeps that contract in one place.
 */
function containsRiskyControlChars(text: string): boolean {
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (code < 0x20 && code !== 0x09 && code !== 0x0a && code !== 0x0d) {
      return true;
    }
    if (code === 0x7f) return true;
  }
  return false;
}

/**
 * Heuristic: does `text` look like a single keystroke / IME commit /
 * special-key sequence (so the integration can skip the paste policy)?
 *
 * Rules:
 *  - Empty string → keystroke (no-op).
 *  - Single character → keystroke (covers Enter `\r`, ESC, printable).
 *  - The exact two-character `\r\n` pair → keystroke. xterm sends bare
 *    `\r` for Enter today, but a future renderer adapter that emits
 *    CRLF for Enter would otherwise trip the multi-line confirm panel
 *    on every Enter press. The carve-out is exact: a longer string
 *    that *contains* `\r\n` (e.g. `\r\nls`) still classifies as a
 *    paste-candidate.
 *  - 2..{@link KEYSTROKE_MAX_LENGTH} chars with no `\r` or `\n` →
 *    keystroke (special keys, short IME commits).
 *  - Otherwise → not a keystroke; run the strict paste policy.
 *
 * This is exported so tests can pin the boundary.
 */
export function isLikelyKeystroke(text: string): boolean {
  if (text.length === 0) return true;
  if (text.length === 1) return true;
  if (text === "\r\n") return true;
  if (/[\r\n]/.test(text)) return false;
  if (text.length > KEYSTROKE_MAX_LENGTH) return false;
  return true;
}

/**
 * Static operator-facing string keyed off `reasonCode`. Centralised so
 * the confirm/block panel and the decision object agree on the wording.
 * NEVER interpolates paste content.
 */
export function describePasteDecision(reasonCode: PasteReasonCode): string {
  switch (reasonCode) {
    case "ok_empty":
      return "Empty paste — nothing to send.";
    case "ok_keystroke":
      return "Keystroke forwarded.";
    case "ok_single_line":
      return "Single-line paste forwarded.";
    case "multiline":
      return "Multiline paste detected.";
    case "large_payload":
      return "Large paste detected.";
    case "control_chars":
      return "Paste contains terminal control characters.";
    case "bracketed_paste_markers":
      return "Paste contains bracketed-paste markers.";
    case "nul_byte":
      return "Paste blocked: contains a NUL byte.";
    case "exceeds_hard_cap":
      return "Paste blocked: exceeds the size limit.";
  }
}

interface DecisionInputs {
  byteLength: number;
  lineCount: number;
  hasControlChars: boolean;
  hasBracketedPasteMarkers: boolean;
}

function buildDecision(
  risk: PasteRisk,
  reasonCode: PasteReasonCode,
  inputs: DecisionInputs,
): PasteDecision {
  return {
    risk,
    reasonCode,
    lineCount: inputs.lineCount,
    byteLength: inputs.byteLength,
    hasControlChars: inputs.hasControlChars,
    hasBracketedPasteMarkers: inputs.hasBracketedPasteMarkers,
    safeUserMessage: describePasteDecision(reasonCode),
  };
}

/**
 * Strict paste policy. Assumes `text` has already been classified as a
 * paste-candidate by the integration layer (i.e. {@link isLikelyKeystroke}
 * returned `false`).
 *
 * Order of evaluation:
 *  1. NUL byte → blocked.
 *  2. byteLength > {@link PASTE_HARD_CAP_BYTES} → blocked.
 *  3. Bracketed-paste markers in text → confirm.
 *  4. Multiline → confirm.
 *  5. Risky control chars → confirm.
 *  6. byteLength > {@link PASTE_CONFIRM_BYTES} → confirm.
 *  7. Otherwise → safe.
 *
 * The function NEVER mutates `text` and NEVER stores it.
 */
export function decidePaste(text: string): PasteDecision {
  const byteLength = pasteByteLength(text);
  const lineCount = pasteLineCount(text);
  const hasControlChars = containsRiskyControlChars(text);
  const hasBracketedPasteMarkers = containsBracketedPasteMarkers(text);
  const inputs: DecisionInputs = {
    byteLength,
    lineCount,
    hasControlChars,
    hasBracketedPasteMarkers,
  };

  if (text.includes(NUL_BYTE)) {
    return buildDecision("blocked", "nul_byte", inputs);
  }
  if (byteLength > PASTE_HARD_CAP_BYTES) {
    return buildDecision("blocked", "exceeds_hard_cap", inputs);
  }
  if (hasBracketedPasteMarkers) {
    return buildDecision("confirm", "bracketed_paste_markers", inputs);
  }
  if (lineCount > 1) {
    return buildDecision("confirm", "multiline", inputs);
  }
  if (hasControlChars) {
    return buildDecision("confirm", "control_chars", inputs);
  }
  if (byteLength > PASTE_CONFIRM_BYTES) {
    return buildDecision("confirm", "large_payload", inputs);
  }
  return buildDecision("safe", "ok_single_line", inputs);
}

/**
 * Combined entry point: applies the keystroke short-circuit FIRST and
 * defers to {@link decidePaste} otherwise. Returns a `safe` decision
 * with `reasonCode = "ok_keystroke"` for keystroke-likely input and
 * `reasonCode = "ok_empty"` for empty input.
 *
 * The integration layer should call this on every `renderer.onInput`
 * payload and dispatch on `decision.risk`:
 *  - `safe` → forward to `client.sendInput(text)` directly.
 *  - `confirm` → hold the original text in a closure variable, render
 *    the confirm panel using the decision metadata only, and forward
 *    when the operator confirms.
 *  - `blocked` → drop the text; show a non-blocking note keyed off the
 *    reason code.
 */
export function evaluatePaste(text: string): PasteDecision {
  if (text.length === 0) {
    return buildDecision("safe", "ok_empty", {
      byteLength: 0,
      lineCount: 0,
      hasControlChars: false,
      hasBracketedPasteMarkers: false,
    });
  }
  if (isLikelyKeystroke(text)) {
    return buildDecision("safe", "ok_keystroke", {
      byteLength: pasteByteLength(text),
      lineCount: pasteLineCount(text),
      hasControlChars: false,
      hasBracketedPasteMarkers: false,
    });
  }
  return decidePaste(text);
}
