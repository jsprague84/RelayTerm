/**
 * Pure helpers used by the dev-only xterm live-terminal lab.
 *
 * Why a separate module: the Svelte component itself is awkward to unit-
 * test, but the *rules* it enforces — input/output redaction, cell-grid
 * validation, base64 decode failure handling — are pure functions and
 * worth pinning. Component glue stays in the `.svelte` file; anything
 * with branching logic that the SPEC promises a contract for goes here.
 *
 * Critical contracts:
 *  - {@link redactInputLogText} NEVER takes the payload as a parameter.
 *    The function signature is the redaction rule: callers cannot
 *    accidentally log bytes through this surface because it doesn't
 *    accept them.
 *  - {@link safeDecodeOutput} catches any decoder throw (browser `atob`
 *    raises `DOMException` on invalid input) and reports it as a typed
 *    failure WITHOUT the offending payload. The lab maps the failure to
 *    a static log line.
 *  - {@link validateCellGrid} mirrors the backend's `1..=4096` clamp so
 *    the lab can refuse before sending an obviously invalid resize frame.
 */

import { decodeOutputData } from "@relayterm/terminal-core";

/** Wire-side cell-grid bounds — same as `relayterm_protocol::ResizeMsg`. */
export const CELL_GRID_MIN = 1;
export const CELL_GRID_MAX = 4096;

/**
 * Format the redaction-safe log line for an outbound `input` frame.
 *
 * The function deliberately does NOT accept the payload itself — that
 * way no caller can leak input bytes through this channel even by
 * mistake. The byte count comes from {@link inputByteLength} which the
 * caller computes off the original payload before it is sent.
 */
export function redactInputLogText(byteLength: number): string {
  return `input sent <redacted>, bytes=${byteLength}`;
}

/**
 * Format the log line for an inbound `output` frame. Output is rendered
 * inside the terminal — that's its purpose — but the diagnostic event
 * log only gets a length and a sequence number.
 */
export function outputLogText(seq: number, byteLength: number): string {
  return `output seq=${seq}, bytes=${byteLength}`;
}

export type OutputDecodeResult =
  | { ok: true; bytes: Uint8Array }
  | { ok: false; reason: "invalid_base64" };

/**
 * Decode an `output` frame's `data` field, never throwing. The wrapper
 * exists so the Svelte event handler can map a decode failure to a
 * typed log line WITHOUT including the offending base64 string in any
 * error envelope. The underlying {@link decodeOutputData} is the single-
 * sourced wire-format helper from `@relayterm/terminal-core`.
 */
export function safeDecodeOutput(data: string): OutputDecodeResult {
  try {
    return { ok: true, bytes: decodeOutputData(data) };
  } catch {
    return { ok: false, reason: "invalid_base64" };
  }
}

export type CellGridValidation =
  | { ok: true }
  | { ok: false; reason: "non-integer" | "below-min" | "above-max" };

/**
 * Validate cell-grid dimensions before a resize frame is sent. The
 * backend already rejects out-of-range values with `invalid_input`; we
 * refuse client-side so a typo in the lab UI can't generate a wire
 * round-trip just to learn `81 != "eighty-one"`.
 */
export function validateCellGrid(cols: number, rows: number): CellGridValidation {
  if (!Number.isInteger(cols) || !Number.isInteger(rows)) {
    return { ok: false, reason: "non-integer" };
  }
  if (cols < CELL_GRID_MIN || rows < CELL_GRID_MIN) {
    return { ok: false, reason: "below-min" };
  }
  if (cols > CELL_GRID_MAX || rows > CELL_GRID_MAX) {
    return { ok: false, reason: "above-max" };
  }
  return { ok: true };
}

/**
 * Compute the UTF-8 byte length of a renderer-input payload. xterm hands
 * us strings; a future binary-frame slice may hand us `Uint8Array`. The
 * count is what the wire frame would actually carry, not the JS string
 * length (which is UTF-16 code units and disagrees on non-ASCII).
 *
 * The payload is consumed only to count bytes — it never escapes this
 * function.
 */
export function inputByteLength(data: string | Uint8Array): number {
  if (typeof data === "string") {
    return new TextEncoder().encode(data).length;
  }
  return data.byteLength;
}
