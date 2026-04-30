import { describe, expect, it } from "vitest";
import {
  decodeOutputData,
  decodeServerMsg,
  encodeClientMsg,
  encodeOutputData,
  type ClientMsg,
  type ServerMsg,
} from "../src/index.js";

describe("encodeClientMsg", () => {
  it("encodes ping with stable tag", () => {
    expect(encodeClientMsg({ type: "ping" })).toBe('{"type":"ping"}');
  });

  it("encodes attach including nullable fields explicitly", () => {
    const json = encodeClientMsg({
      type: "attach",
      session_id: null,
      last_seen_seq: null,
      client_id: null,
    });
    const parsed = JSON.parse(json) as Record<string, unknown>;
    expect(parsed["type"]).toBe("attach");
    expect(parsed["session_id"]).toBeNull();
    expect(parsed["last_seen_seq"]).toBeNull();
    expect(parsed["client_id"]).toBeNull();
  });

  it("encodes resize with numeric dims", () => {
    const json = encodeClientMsg({ type: "resize", cols: 80, rows: 24 });
    expect(JSON.parse(json)).toEqual({ type: "resize", cols: 80, rows: 24 });
  });

  it("round-trips known client messages through JSON", () => {
    const messages: ClientMsg[] = [
      { type: "ping" },
      { type: "attach", session_id: "abc", last_seen_seq: 7, client_id: "tab-1" },
      { type: "input", data: "ls -la" },
      { type: "resize", cols: 120, rows: 40 },
      { type: "detach" },
      { type: "close" },
    ];
    for (const msg of messages) {
      const back = JSON.parse(encodeClientMsg(msg));
      expect(back).toEqual(msg);
    }
  });
});

describe("decodeServerMsg", () => {
  const wellFormed: ServerMsg[] = [
    { type: "pong" },
    {
      type: "session_attached",
      session_id: "00000000-0000-0000-0000-000000000001",
      attachment_id: "00000000-0000-0000-0000-000000000002",
      status: "attached_stub",
      message: "attached to placeholder",
    },
    {
      type: "session_attached",
      session_id: "00000000-0000-0000-0000-000000000001",
      attachment_id: "00000000-0000-0000-0000-000000000002",
      status: "active",
      message: "attached live",
    },
    { type: "ack", kind: "resize" },
    { type: "output", seq: 17, data: "hello" },
    { type: "error", code: "pty_not_live", message: "no live pty" },
    { type: "error", code: "ssh_start_failed", message: "ssh pty error" },
    { type: "replay_start", from_seq: 4, to_seq: 6 },
    { type: "replay_end", latest_seq: 6 },
    {
      type: "replay_window_lost",
      requested_seq: 1,
      oldest_available_seq: 5,
      latest_seq: 7,
    },
    {
      type: "replay_window_lost",
      requested_seq: 9,
      oldest_available_seq: null,
      latest_seq: 0,
    },
    {
      type: "session_detached",
      session_id: "s",
      attachment_id: "a",
    },
    { type: "session_closed", session_id: "s" },
    { type: "error", code: "invalid_message", message: "bad frame" },
  ];

  it("decodes every server-message variant", () => {
    for (const msg of wellFormed) {
      const result = decodeServerMsg(JSON.stringify(msg));
      expect(result.ok, JSON.stringify(msg)).toBe(true);
      if (result.ok) {
        expect(result.message).toEqual(msg);
      }
    }
  });

  it("returns invalid_json for non-JSON input", () => {
    const result = decodeServerMsg("not-json");
    expect(result).toEqual({ ok: false, failure: { kind: "invalid_json" } });
  });

  it("returns invalid_json for non-object roots", () => {
    expect(decodeServerMsg("42")).toEqual({
      ok: false,
      failure: { kind: "invalid_json" },
    });
    expect(decodeServerMsg("[1,2]")).toEqual({
      ok: false,
      failure: { kind: "invalid_json" },
    });
  });

  it("returns unknown_type for absent or unknown tag", () => {
    expect(decodeServerMsg("{}")).toEqual({
      ok: false,
      failure: { kind: "unknown_type", received: "<missing>" },
    });
    const unknown = decodeServerMsg('{"type":"definitely_not_real"}');
    expect(unknown.ok).toBe(false);
    if (!unknown.ok) {
      expect(unknown.failure.kind).toBe("unknown_type");
    }
  });

  it("returns invalid_shape when required fields are missing", () => {
    const missingMessage = decodeServerMsg(
      '{"type":"session_attached","session_id":"s","attachment_id":"a","status":"attached_stub"}',
    );
    expect(missingMessage).toEqual({
      ok: false,
      failure: { kind: "invalid_shape", received: "session_attached" },
    });
  });

  it("rejects unknown ack kinds", () => {
    const result = decodeServerMsg('{"type":"ack","kind":"totally_new"}');
    expect(result).toEqual({
      ok: false,
      failure: { kind: "invalid_shape", received: "ack" },
    });
  });

  it("rejects error frames with unknown codes", () => {
    const result = decodeServerMsg(
      '{"type":"error","code":"galaxy_brain","message":"x"}',
    );
    expect(result).toEqual({
      ok: false,
      failure: { kind: "invalid_shape", received: "error" },
    });
  });

  it("never embeds the raw payload in a decode failure", () => {
    const sentinel = "REDACT-MARKER-DECODE-9F11";
    const result = decodeServerMsg(`{"type":"unknown","payload":"${sentinel}"}`);
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(JSON.stringify(result.failure)).not.toContain(sentinel);
    }
  });
});

describe("output data codec", () => {
  it("round-trips arbitrary bytes including high-bit values", () => {
    // Mirror of the Rust-side test: every byte from 0x00..=0xFF must
    // survive the base64 encode/decode pair losslessly. A naive utf-8
    // wrap would mangle high-bit bytes.
    const raw = new Uint8Array(256);
    for (let i = 0; i < 256; i++) raw[i] = i;
    const decoded = decodeOutputData(encodeOutputData(raw));
    expect(decoded).toEqual(raw);
  });

  it("decodes a known fixture matching the Rust encoder", () => {
    // 'hello world' base64 → 'aGVsbG8gd29ybGQ='. The fixture is the
    // contract: any deviation is a protocol-level break.
    const decoded = decodeOutputData("aGVsbG8gd29ybGQ=");
    const expected = new TextEncoder().encode("hello world");
    expect(decoded).toEqual(expected);
  });

  it("throws on malformed base64 input (caller must catch)", () => {
    // The browser `atob` throws `DOMException("InvalidCharacterError")`
    // on a non-base64 input; Node's `atob` throws a plain `Error`.
    // Either way the call must throw — callers (e.g. the live-terminal
    // lab's `safeDecodeOutput`) wrap this in a try/catch and surface a
    // typed failure WITHOUT echoing the offending payload.
    expect(() => decodeOutputData("!!!definitely-not-base64!!!")).toThrow();
  });
});
