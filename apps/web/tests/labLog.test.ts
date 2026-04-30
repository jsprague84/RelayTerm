import { describe, expect, it } from "vitest";
import { encodeOutputData } from "@relayterm/terminal-core";
import {
  CELL_GRID_MAX,
  CELL_GRID_MIN,
  inputByteLength,
  outputLogText,
  redactInputLogText,
  safeDecodeOutput,
  validateCellGrid,
} from "../src/lib/dev/labLog.js";

/**
 * Sentinel that should NEVER appear in any log line, error envelope, or
 * decode failure. The redaction rule (SPEC §"Live SSH PTY bridge contract"
 * + §"Frontend terminal-core contract") forbids the lab from echoing
 * payload bytes through any user-visible channel; this sentinel is the
 * canary that proves the rule.
 */
const SENTINEL = "RELAY_SENTINEL_INPUT_LAB_PAYLOAD_72FE";

describe("redactInputLogText", () => {
  it("formats only the byte count", () => {
    expect(redactInputLogText(0)).toBe("input sent <redacted>, bytes=0");
    expect(redactInputLogText(42)).toBe("input sent <redacted>, bytes=42");
  });

  it("function signature does not accept the payload", () => {
    // The redaction rule is encoded at the type level: the only way to
    // call this function is with a number. A consumer cannot leak the
    // payload through this surface even if they wanted to.
    const fn: (n: number) => string = redactInputLogText;
    expect(fn(7)).not.toContain(SENTINEL);
  });
});

describe("outputLogText", () => {
  it("formats seq and byte count", () => {
    expect(outputLogText(0, 0)).toBe("output seq=0, bytes=0");
    expect(outputLogText(123, 9999)).toBe("output seq=123, bytes=9999");
  });
});

describe("safeDecodeOutput", () => {
  it("decodes valid base64 round-tripped through the protocol encoder", () => {
    const raw = new Uint8Array([0x00, 0x7f, 0x80, 0xff, 0x41, 0x42]);
    const result = safeDecodeOutput(encodeOutputData(raw));
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.bytes).toEqual(raw);
  });

  it("returns invalid_base64 for malformed input without echoing payload", () => {
    const result = safeDecodeOutput(`!!!${SENTINEL}!!!`);
    expect(result).toEqual({ ok: false, reason: "invalid_base64" });
    expect(JSON.stringify(result)).not.toContain(SENTINEL);
  });

  it("treats an empty string as a valid empty payload", () => {
    // base64("") === "" and atob("") === "" — the protocol allows a
    // zero-byte chunk, so the lab must not collapse it to an error.
    const result = safeDecodeOutput("");
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.bytes).toEqual(new Uint8Array(0));
  });
});

describe("validateCellGrid", () => {
  it("accepts the inclusive bounds", () => {
    expect(validateCellGrid(CELL_GRID_MIN, CELL_GRID_MIN)).toEqual({ ok: true });
    expect(validateCellGrid(CELL_GRID_MAX, CELL_GRID_MAX)).toEqual({ ok: true });
    expect(validateCellGrid(80, 24)).toEqual({ ok: true });
  });

  it("rejects below-min", () => {
    expect(validateCellGrid(0, 24)).toEqual({ ok: false, reason: "below-min" });
    expect(validateCellGrid(80, 0)).toEqual({ ok: false, reason: "below-min" });
    expect(validateCellGrid(-1, 24)).toEqual({ ok: false, reason: "below-min" });
  });

  it("rejects above-max", () => {
    expect(validateCellGrid(CELL_GRID_MAX + 1, 24)).toEqual({
      ok: false,
      reason: "above-max",
    });
    expect(validateCellGrid(80, CELL_GRID_MAX + 1)).toEqual({
      ok: false,
      reason: "above-max",
    });
  });

  it("rejects non-integers (NaN, fractions, infinities)", () => {
    expect(validateCellGrid(Number.NaN, 24)).toEqual({
      ok: false,
      reason: "non-integer",
    });
    expect(validateCellGrid(80.5, 24)).toEqual({
      ok: false,
      reason: "non-integer",
    });
    expect(validateCellGrid(80, Number.POSITIVE_INFINITY)).toEqual({
      ok: false,
      reason: "non-integer",
    });
  });
});

describe("inputByteLength", () => {
  it("counts ASCII strings as one byte per char", () => {
    expect(inputByteLength("abc")).toBe(3);
  });

  it("counts unicode strings by their UTF-8 byte length, not code units", () => {
    // "🌍" is one code point but 4 UTF-8 bytes. JS string `.length` would
    // report 2 (UTF-16 surrogate pair) which is the wrong value for a
    // wire-byte log line.
    expect(inputByteLength("🌍")).toBe(4);
    expect(inputByteLength("héllo")).toBe(6);
  });

  it("returns the byteLength of a Uint8Array as-is", () => {
    expect(inputByteLength(new Uint8Array([1, 2, 3, 4, 5]))).toBe(5);
    expect(inputByteLength(new Uint8Array(0))).toBe(0);
  });

  it("does not echo the payload anywhere observable", () => {
    // Pin the rule with a sentinel: even though `inputByteLength` is a
    // pure number, regressions that toString() the input and concat it
    // would surface here.
    const out = inputByteLength(SENTINEL);
    expect(String(out)).not.toContain(SENTINEL);
  });
});
