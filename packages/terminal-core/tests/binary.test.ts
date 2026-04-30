import { describe, expect, it } from "vitest";
import {
  BINARY_HEADER_LEN,
  BINARY_MAGIC_V1,
  BINARY_MAX_PAYLOAD_LEN,
  decodeBinaryFrame,
  encodeBinaryFrame,
} from "../src/index.js";

describe("encodeBinaryFrame / decodeBinaryFrame", () => {
  it("round-trips an Output frame with arbitrary bytes including high-bit", () => {
    const payload = new Uint8Array(256);
    for (let i = 0; i < 256; i++) payload[i] = i;
    const enc = encodeBinaryFrame("output", 42, payload);
    expect(enc.ok).toBe(true);
    if (!enc.ok) return;
    const dec = decodeBinaryFrame(enc.bytes);
    expect(dec.ok).toBe(true);
    if (!dec.ok) return;
    expect(dec.frame.kind).toBe("output");
    expect(dec.frame.seq).toBe(42);
    expect(Array.from(dec.frame.payload)).toEqual(Array.from(payload));
    expect(enc.bytes.byteLength).toBe(BINARY_HEADER_LEN + payload.byteLength);
  });

  it("round-trips an Input frame with seq=0", () => {
    const payload = new TextEncoder().encode("ls -la\n");
    const enc = encodeBinaryFrame("input", 0, payload);
    expect(enc.ok).toBe(true);
    if (!enc.ok) return;
    const dec = decodeBinaryFrame(enc.bytes);
    expect(dec.ok).toBe(true);
    if (!dec.ok) return;
    expect(dec.frame.kind).toBe("input");
    expect(dec.frame.seq).toBe(0);
    expect(new TextDecoder().decode(dec.frame.payload)).toBe("ls -la\n");
  });

  it("matches the Rust encoder byte-for-byte on a known fixture", () => {
    // Header layout sanity. seq=1, payload "hi" → bytes 'h','i'.
    const enc = encodeBinaryFrame("output", 1, new Uint8Array([0x68, 0x69]));
    expect(enc.ok).toBe(true);
    if (!enc.ok) return;
    const expected = new Uint8Array([
      0x52, 0x54, 0x42, 0x31, // magic "RTB1"
      0x01, // kind: output
      0x00, // flags
      0x00, 0x00, // reserved
      0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // seq u64 BE = 1
      0x00, 0x00, 0x00, 0x02, // payload_len u32 BE = 2
      0x68, 0x69, // 'h', 'i'
    ]);
    expect(Array.from(enc.bytes)).toEqual(Array.from(expected));
  });

  it("rejects bad magic without echoing payload", () => {
    const enc = encodeBinaryFrame("output", 1, new Uint8Array([1, 2, 3])).ok
      ? encodeBinaryFrame("output", 1, new Uint8Array([1, 2, 3]))
      : null;
    if (!enc || !enc.ok) throw new Error("encode failed");
    const broken = new Uint8Array(enc.bytes);
    broken[0] = 0x00;
    const dec = decodeBinaryFrame(broken);
    expect(dec.ok).toBe(false);
    if (dec.ok) return;
    expect(dec.failure.kind).toBe("bad_magic");
    expect(JSON.stringify(dec.failure)).not.toContain("\\u0001");
  });

  it("rejects truncated header", () => {
    const dec = decodeBinaryFrame(new Uint8Array(BINARY_HEADER_LEN - 1));
    expect(dec.ok).toBe(false);
    if (dec.ok) return;
    expect(dec.failure.kind).toBe("truncated_header");
  });

  it("rejects unknown kind safely", () => {
    const enc = encodeBinaryFrame("output", 1, new Uint8Array([0x77]));
    if (!enc.ok) throw new Error("encode failed");
    const broken = new Uint8Array(enc.bytes);
    broken[4] = 0xff;
    const dec = decodeBinaryFrame(broken);
    expect(dec.ok).toBe(false);
    if (dec.ok) return;
    expect(dec.failure.kind).toBe("unknown_kind");
  });

  it("rejects length mismatch (truncated and overlong)", () => {
    const enc = encodeBinaryFrame("output", 1, new TextEncoder().encode("hello"));
    if (!enc.ok) throw new Error("encode failed");
    // Truncated payload: drop a byte.
    const truncated = enc.bytes.subarray(0, enc.bytes.byteLength - 1);
    const decT = decodeBinaryFrame(truncated);
    expect(decT.ok).toBe(false);
    if (decT.ok) return;
    expect(decT.failure.kind).toBe("length_mismatch");
    // Overlong: trailing byte after declared payload.
    const overlong = new Uint8Array(enc.bytes.byteLength + 1);
    overlong.set(enc.bytes);
    const decO = decodeBinaryFrame(overlong);
    expect(decO.ok).toBe(false);
    if (decO.ok) return;
    expect(decO.failure.kind).toBe("length_mismatch");
  });

  it("rejects oversized claimed payload before allocating", () => {
    // Hand-rolled header: claim u32::MAX bytes of payload but provide
    // only the header. The decoder MUST reject on the size cap, NOT
    // attempt to read 4 GiB.
    const buf = new Uint8Array(BINARY_HEADER_LEN);
    buf.set(BINARY_MAGIC_V1, 0);
    buf[4] = 0x01; // output kind
    // seq stays 0
    buf[16] = 0xff;
    buf[17] = 0xff;
    buf[18] = 0xff;
    buf[19] = 0xff;
    const dec = decodeBinaryFrame(buf);
    expect(dec.ok).toBe(false);
    if (dec.ok) return;
    expect(dec.failure.kind).toBe("payload_too_large");
  });

  it("encoder refuses oversized payloads", () => {
    const huge = new Uint8Array(BINARY_MAX_PAYLOAD_LEN + 1);
    const enc = encodeBinaryFrame("output", 1, huge);
    expect(enc.ok).toBe(false);
    if (enc.ok) return;
    expect(enc.failure.kind).toBe("payload_too_large");
  });

  it("rejects non-zero flags or reserved bytes", () => {
    const enc = encodeBinaryFrame("output", 1, new Uint8Array([1]));
    if (!enc.ok) throw new Error("encode failed");
    const flagSet = new Uint8Array(enc.bytes);
    flagSet[5] = 0x01;
    const dec1 = decodeBinaryFrame(flagSet);
    expect(dec1.ok).toBe(false);
    if (dec1.ok) return;
    expect(dec1.failure.kind).toBe("non_zero_reserved");

    const reservedSet = new Uint8Array(enc.bytes);
    reservedSet[7] = 0x01;
    const dec2 = decodeBinaryFrame(reservedSet);
    expect(dec2.ok).toBe(false);
    if (dec2.ok) return;
    expect(dec2.failure.kind).toBe("non_zero_reserved");
  });

  it("decode failure shape never embeds payload bytes", () => {
    // Sentinel bytes inside a malformed payload — even when decode
    // fails, the structured failure object MUST NOT carry them.
    const sentinel = new TextEncoder().encode("REDACT-MARKER-BIN-9C");
    const buf = new Uint8Array(BINARY_HEADER_LEN + sentinel.byteLength);
    buf.set(BINARY_MAGIC_V1, 0);
    buf[4] = 0xff; // unknown kind (forces decode failure)
    // payload_len = sentinel.length so length check passes; the unknown
    // kind path is what fails.
    const len = sentinel.byteLength;
    buf[16] = (len >>> 24) & 0xff;
    buf[17] = (len >>> 16) & 0xff;
    buf[18] = (len >>> 8) & 0xff;
    buf[19] = len & 0xff;
    buf.set(sentinel, BINARY_HEADER_LEN);
    const dec = decodeBinaryFrame(buf);
    expect(dec.ok).toBe(false);
    if (dec.ok) return;
    const json = JSON.stringify(dec.failure);
    expect(json).not.toContain("REDACT-MARKER-BIN-9C");
  });

  it("preserves seq above 2^32 (high-half)", () => {
    const seq = 0x1_0000_0007; // 4 GiB + 7
    const enc = encodeBinaryFrame("output", seq, new Uint8Array());
    if (!enc.ok) throw new Error("encode failed");
    const dec = decodeBinaryFrame(enc.bytes);
    expect(dec.ok).toBe(true);
    if (!dec.ok) return;
    expect(dec.frame.seq).toBe(seq);
  });
});
