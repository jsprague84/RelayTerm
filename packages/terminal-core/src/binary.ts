/**
 * Binary frame envelope for the RelayTerm terminal data plane (TS mirror
 * of `relayterm_protocol::binary`).
 *
 * The hot terminal data path — PTY output and keystroke input — uses
 * binary WebSocket frames carrying the envelope below. The control
 * plane (attach/detach/resize/replay control, errors, lifecycle)
 * remains JSON; see `protocol.ts`.
 *
 * Wire format (big-endian, 20-byte header):
 *
 * ```
 *  offset | size | field
 *  -------|------|----------------------------------------------
 *      0  |   4  | magic = "RTB1"
 *      4  |   1  | kind: 0x01=Output, 0x02=Input
 *      5  |   1  | flags (reserved, must be 0; readers ignore unknown bits)
 *      6  |   2  | reserved (must be 0; readers ignore)
 *      8  |   8  | seq u64 BE (Output: stamped; Input: 0)
 *     16  |   4  | payload_len u32 BE
 *     20  |   N  | payload bytes
 * ```
 *
 * Logging note: codec failures NEVER include the offending bytes in the
 * structured failure shape. Callers map a failure to a typed protocol
 * error event without echoing payload, mirroring the JSON decoder.
 */

/** Magic bytes prefixing every v1 binary frame (`b"RTB1"`). */
export const BINARY_MAGIC_V1: Uint8Array = Uint8Array.of(0x52, 0x54, 0x42, 0x31);

/** Length of the fixed binary frame header. */
export const BINARY_HEADER_LEN = 20;

/** Hard cap on a single binary frame's payload bytes (1 MiB). */
export const BINARY_MAX_PAYLOAD_LEN = 1024 * 1024;

/** Kind tag distinguishing binary terminal data frames. */
export type BinaryFrameKind = "output" | "input";

const KIND_OUTPUT_BYTE = 0x01;
const KIND_INPUT_BYTE = 0x02;

function kindToByte(kind: BinaryFrameKind): number {
  return kind === "output" ? KIND_OUTPUT_BYTE : KIND_INPUT_BYTE;
}

function kindFromByte(byte: number): BinaryFrameKind | null {
  if (byte === KIND_OUTPUT_BYTE) return "output";
  if (byte === KIND_INPUT_BYTE) return "input";
  return null;
}

/** Decoded binary frame. The payload is owned (no aliasing of the input). */
export interface BinaryFrame {
  kind: BinaryFrameKind;
  /** Sequence number stamped by the orchestrator (Output) or 0 (Input). */
  seq: number;
  payload: Uint8Array;
}

/**
 * Failure modes from {@link decodeBinaryFrame}. The offending bytes are
 * NEVER carried in the failure shape — callers map a failure to a
 * static, classifier-only event so a hostile peer can't induce a log
 * line that echoes their input.
 */
export type BinaryDecodeFailure =
  | { kind: "truncated_header" }
  | { kind: "bad_magic" }
  | { kind: "non_zero_reserved" }
  | { kind: "payload_too_large" }
  | { kind: "length_mismatch" }
  | { kind: "unknown_kind" };

export type BinaryDecodeResult =
  | { ok: true; frame: BinaryFrame }
  | { ok: false; failure: BinaryDecodeFailure };

export type BinaryEncodeFailure = { kind: "payload_too_large" };

export type BinaryEncodeResult =
  | { ok: true; bytes: Uint8Array }
  | { ok: false; failure: BinaryEncodeFailure };

/**
 * Encode a binary frame. Refuses payloads larger than
 * {@link BINARY_MAX_PAYLOAD_LEN} so the caller doesn't accidentally put
 * a frame on the wire the peer would itself reject. `seq` is reduced to
 * a u64 — JS numbers safely cover seqs up to 2^53; no realistic terminal
 * session reaches that.
 */
export function encodeBinaryFrame(
  kind: BinaryFrameKind,
  seq: number,
  payload: Uint8Array,
): BinaryEncodeResult {
  if (payload.byteLength > BINARY_MAX_PAYLOAD_LEN) {
    return { ok: false, failure: { kind: "payload_too_large" } };
  }
  const total = BINARY_HEADER_LEN + payload.byteLength;
  const buf = new Uint8Array(total);
  // Magic
  buf[0] = 0x52;
  buf[1] = 0x54;
  buf[2] = 0x42;
  buf[3] = 0x31;
  buf[4] = kindToByte(kind);
  // flags + reserved already zero by Uint8Array init
  // seq u64 BE — split high/low so we don't lose precision past 2^32.
  // Math.floor(seq / 2^32) safely returns 0 for seqs that fit in u32.
  const high = Math.floor(seq / 0x100000000);
  const low = seq >>> 0;
  buf[8] = (high >>> 24) & 0xff;
  buf[9] = (high >>> 16) & 0xff;
  buf[10] = (high >>> 8) & 0xff;
  buf[11] = high & 0xff;
  buf[12] = (low >>> 24) & 0xff;
  buf[13] = (low >>> 16) & 0xff;
  buf[14] = (low >>> 8) & 0xff;
  buf[15] = low & 0xff;
  // payload_len u32 BE
  const len = payload.byteLength >>> 0;
  buf[16] = (len >>> 24) & 0xff;
  buf[17] = (len >>> 16) & 0xff;
  buf[18] = (len >>> 8) & 0xff;
  buf[19] = len & 0xff;
  buf.set(payload, BINARY_HEADER_LEN);
  return { ok: true, bytes: buf };
}

/**
 * Decode a binary frame. Never throws; returns a structured failure on
 * any malformed input. The decoder enforces the payload-len cap BEFORE
 * touching the payload region so a hostile peer cannot OOM the page by
 * stamping `0xFFFFFFFF`.
 */
export function decodeBinaryFrame(buf: Uint8Array): BinaryDecodeResult {
  if (buf.byteLength < BINARY_HEADER_LEN) {
    return { ok: false, failure: { kind: "truncated_header" } };
  }
  if (
    buf[0] !== 0x52 ||
    buf[1] !== 0x54 ||
    buf[2] !== 0x42 ||
    buf[3] !== 0x31
  ) {
    return { ok: false, failure: { kind: "bad_magic" } };
  }
  const kindByte = buf[4]!;
  const flags = buf[5]!;
  const reservedHi = buf[6]!;
  const reservedLo = buf[7]!;
  if (flags !== 0 || reservedHi !== 0 || reservedLo !== 0) {
    return { ok: false, failure: { kind: "non_zero_reserved" } };
  }
  const high =
    (buf[8]! << 24) |
    (buf[9]! << 16) |
    (buf[10]! << 8) |
    buf[11]!;
  const low =
    ((buf[12]! << 24) >>> 0) |
    (buf[13]! << 16) |
    (buf[14]! << 8) |
    buf[15]!;
  // Reassemble u64. `>>> 0` on `high` keeps it unsigned; multiplying by
  // 2^32 stays exact for seq values below 2^53 (which covers every
  // realistic session by many orders of magnitude).
  const seq = (high >>> 0) * 0x100000000 + (low >>> 0);
  const lenHigh =
    ((buf[16]! << 24) >>> 0) |
    (buf[17]! << 16) |
    (buf[18]! << 8) |
    buf[19]!;
  const payloadLen = lenHigh >>> 0;
  if (payloadLen > BINARY_MAX_PAYLOAD_LEN) {
    return { ok: false, failure: { kind: "payload_too_large" } };
  }
  if (buf.byteLength !== BINARY_HEADER_LEN + payloadLen) {
    return { ok: false, failure: { kind: "length_mismatch" } };
  }
  const kind = kindFromByte(kindByte);
  if (kind === null) {
    return { ok: false, failure: { kind: "unknown_kind" } };
  }
  // Copy into a fresh Uint8Array so the caller's lifetime is independent
  // of the decoded buffer (browsers may reuse the underlying ArrayBuffer
  // across `MessageEvent`s in some implementations).
  const payload = new Uint8Array(payloadLen);
  payload.set(buf.subarray(BINARY_HEADER_LEN, BINARY_HEADER_LEN + payloadLen));
  return { ok: true, frame: { kind, seq, payload } };
}
