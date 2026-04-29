import { describe, expect, it } from "vitest";
import {
  WebSocketTerminalTransport,
  type ServerMsg,
  type TerminalTransportError,
  type WebSocketLike,
  type WebSocketLikeEventMap,
} from "../src/index.js";

class FakeBrowserSocket implements WebSocketLike {
  readyState = 0;
  readonly sent: string[] = [];
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

  send(data: string): void {
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
});
