import { describe, expect, it } from "vitest";
import {
  WebSocketTerminalTransport,
  encodeBinaryFrame,
  type BinaryFrame,
  type ServerMsg,
  type TerminalTransportError,
  type WebSocketLike,
  type WebSocketLikeEventMap,
} from "../src/index.js";

class FakeBrowserSocket implements WebSocketLike {
  readyState = 0;
  binaryType: "blob" | "arraybuffer" = "blob";
  readonly sent: (string | ArrayBufferView | ArrayBuffer)[] = [];
  // The unified listener type erases the per-event payload at storage
  // time; addEventListener / fire still preserve it at the call site.
  readonly listeners: Map<keyof WebSocketLikeEventMap, Set<(e: unknown) => void>> = new Map();

  addEventListener<K extends keyof WebSocketLikeEventMap>(
    type: K,
    cb: (e: WebSocketLikeEventMap[K]) => void,
  ): void {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(cb as (e: unknown) => void);
  }

  removeEventListener<K extends keyof WebSocketLikeEventMap>(
    type: K,
    cb: (e: WebSocketLikeEventMap[K]) => void,
  ): void {
    this.listeners.get(type)?.delete(cb as (e: unknown) => void);
  }

  send(data: string | ArrayBufferView | ArrayBuffer): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    this.fire("close", {
      type: "close",
      code: 1000,
      reason: "test",
      wasClean: true,
    });
  }

  fire<K extends keyof WebSocketLikeEventMap>(
    type: K,
    payload: WebSocketLikeEventMap[K],
  ): void {
    const set = this.listeners.get(type);
    if (!set) return;
    for (const cb of [...set]) {
      (cb as (e: WebSocketLikeEventMap[K]) => void)(payload);
    }
  }
}

describe("WebSocketTerminalTransport", () => {
  it("resolves connect() once the socket fires open", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    expect(transport.readyState).toBe("open");
  });

  it("encodes outbound messages as JSON", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    transport.send({ type: "ping" });
    expect(socket.sent).toEqual(['{"type":"ping"}']);
  });

  it("decodes JSON frames into typed server messages", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    const seen: ServerMsg[] = [];
    transport.onMessage((m) => seen.push(m));
    socket.fire("message", { type: "message", data: '{"type":"pong"}' });
    expect(seen).toEqual([{ type: "pong" }]);
  });

  it("emits decode error for malformed frames without echoing payload", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    const errors: TerminalTransportError[] = [];
    transport.onError((e) => errors.push(e));
    const sentinel = "REDACT-MARKER-FRAME-3CBA";
    socket.fire("message", { type: "message", data: `{"oops":"${sentinel}"}` });
    expect(errors[0]?.kind).toBe("decode");
    for (const err of errors) {
      expect(JSON.stringify(err)).not.toContain(sentinel);
    }
  });

  it("rejects send() before open", () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const errors: TerminalTransportError[] = [];
    transport.onError((e) => errors.push(e));
    transport.send({ type: "ping" });
    expect(errors[0]).toEqual({ kind: "send_before_open" });
  });

  it("decodes binary Output frames into onBinary listeners", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    const seen: BinaryFrame[] = [];
    transport.onBinary((f) => seen.push(f));
    const enc = encodeBinaryFrame("output", 7, new TextEncoder().encode("ok"));
    expect(enc.ok).toBe(true);
    if (!enc.ok) return;
    socket.fire("message", {
      type: "message",
      data: enc.bytes.buffer.slice(
        enc.bytes.byteOffset,
        enc.bytes.byteOffset + enc.bytes.byteLength,
      ),
    });
    expect(seen).toHaveLength(1);
    expect(seen[0]?.kind).toBe("output");
    expect(seen[0]?.seq).toBe(7);
    expect(new TextDecoder().decode(seen[0]?.payload)).toBe("ok");
  });

  it("emits binary_decode error for malformed binary frames without echoing payload", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    const errors: TerminalTransportError[] = [];
    transport.onError((e) => errors.push(e));
    const sentinel = new TextEncoder().encode("REDACT-MARKER-BIN-DECODE");
    socket.fire("message", { type: "message", data: sentinel.buffer });
    expect(errors[0]?.kind).toBe("binary_decode");
    for (const err of errors) {
      expect(JSON.stringify(err)).not.toContain("REDACT-MARKER-BIN-DECODE");
    }
  });

  it("sendBinary forwards raw bytes to the socket", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    const frame = new Uint8Array([0x52, 0x54, 0x42, 0x31, 0x02]);
    transport.sendBinary(frame);
    expect(socket.sent[0]).toBe(frame);
  });

  it("connect() requests arraybuffer binaryType so binary decode is sync", async () => {
    const socket = new FakeBrowserSocket();
    const transport = new WebSocketTerminalTransport({ factory: () => socket });
    const connecting = transport.connect("ws://test/ws");
    socket.fire("open", { type: "open" });
    await connecting;
    expect(socket.binaryType).toBe("arraybuffer");
  });
});
