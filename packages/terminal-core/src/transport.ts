/**
 * Renderer-neutral WebSocket transport for the RelayTerm protocol.
 *
 * The transport is responsible for:
 *  - opening a WebSocket
 *  - encoding outbound `ClientMsg` values
 *  - decoding inbound text frames into typed `ServerMsg` (or a structured
 *    decode failure that callers can map to a protocol error event)
 *  - emitting transport-level close/error events
 *
 * It deliberately knows nothing about renderers, attach/detach state, or
 * application-level retries. Those belong to `TerminalSessionClient`.
 *
 * Logging note: this file never logs raw frames. A decode failure surfaces
 * as a structured event without the offending payload, because that frame
 * may have been an `Input` frame the client was about to send and we don't
 * want renderer bugs leaking keystrokes into a console.
 */

import {
  decodeServerMsg,
  encodeClientMsg,
  type ClientMsg,
  type DecodeFailure,
  type ServerMsg,
} from "./protocol.js";
import { TypedEmitter, type Unsubscribe } from "./events.js";

export interface TerminalCloseEvent {
  /** WebSocket close code, or `null` if closed before the handshake. */
  code: number | null;
  /** Human-readable reason, never includes raw frame data. */
  reason: string;
  /** Whether the close was a clean protocol-level close. */
  wasClean: boolean;
}

export interface TerminalTransportError {
  /**
   * Structured kind so callers don't have to substring-match.
   * - `network`: underlying socket fired `error` (no message text exposed —
   *   browsers redact it anyway).
   * - `decode`: a server frame failed to decode; payload is NOT included.
   * - `send_before_open`: caller invoked `send` before `connect` resolved.
   */
  kind: "network" | "decode" | "send_before_open";
  /** Decode failure detail when `kind === "decode"`. */
  decode?: DecodeFailure;
}

export interface TerminalTransport {
  connect(url: string): Promise<void>;
  send(message: ClientMsg): void;
  close(code?: number, reason?: string): void;
  onMessage(cb: (message: ServerMsg) => void): Unsubscribe;
  onClose(cb: (event: TerminalCloseEvent) => void): Unsubscribe;
  onError(cb: (error: TerminalTransportError) => void): Unsubscribe;
  /** Current readyState mirror, for state-machine consumers. */
  readonly readyState: TransportReadyState;
}

export type TransportReadyState =
  | "idle"
  | "connecting"
  | "open"
  | "closing"
  | "closed";

interface TransportEvents {
  message: ServerMsg;
  close: TerminalCloseEvent;
  error: TerminalTransportError;
}

/**
 * Minimal slice of the browser `WebSocket` interface that we depend on.
 * Defined here so tests can pass an in-memory fake without pulling in DOM
 * lib in the test environment.
 */
export interface WebSocketLike {
  readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener<K extends keyof WebSocketLikeEventMap>(
    type: K,
    listener: (event: WebSocketLikeEventMap[K]) => void,
  ): void;
  removeEventListener<K extends keyof WebSocketLikeEventMap>(
    type: K,
    listener: (event: WebSocketLikeEventMap[K]) => void,
  ): void;
}

export interface WebSocketLikeEventMap {
  open: { type: "open" };
  message: { type: "message"; data: unknown };
  close: { type: "close"; code: number; reason: string; wasClean: boolean };
  error: { type: "error" };
}

/**
 * Factory for the underlying socket. The default uses the global
 * `WebSocket` constructor; tests override this to inject a fake.
 */
export type WebSocketFactory = (url: string) => WebSocketLike;

const DEFAULT_FACTORY: WebSocketFactory = (url) => {
  if (typeof WebSocket === "undefined") {
    throw new Error("global WebSocket is not available in this environment");
  }
  return new WebSocket(url) as unknown as WebSocketLike;
};

export interface WebSocketTransportOptions {
  factory?: WebSocketFactory;
}

export class WebSocketTerminalTransport implements TerminalTransport {
  readonly #factory: WebSocketFactory;
  readonly #emitter = new TypedEmitter<TransportEvents>();
  #socket: WebSocketLike | null = null;
  #state: TransportReadyState = "idle";
  // We hold listener references so we can remove them on close. Without
  // explicit removal a long-lived host (e.g. a Tauri shell) accumulates
  // closures across reconnects.
  #onOpen: ((e: WebSocketLikeEventMap["open"]) => void) | null = null;
  #onMessage: ((e: WebSocketLikeEventMap["message"]) => void) | null = null;
  #onClose: ((e: WebSocketLikeEventMap["close"]) => void) | null = null;
  #onError: ((e: WebSocketLikeEventMap["error"]) => void) | null = null;
  #connectResolve: (() => void) | null = null;
  #connectReject: ((err: Error) => void) | null = null;

  constructor(options: WebSocketTransportOptions = {}) {
    this.#factory = options.factory ?? DEFAULT_FACTORY;
  }

  get readyState(): TransportReadyState {
    return this.#state;
  }

  connect(url: string): Promise<void> {
    if (this.#state !== "idle" && this.#state !== "closed") {
      return Promise.reject(
        new Error(`transport already ${this.#state}; create a new instance`),
      );
    }
    return new Promise((resolve, reject) => {
      this.#connectResolve = resolve;
      this.#connectReject = reject;
      this.#state = "connecting";
      let socket: WebSocketLike;
      try {
        socket = this.#factory(url);
      } catch (err) {
        this.#state = "closed";
        this.#connectResolve = null;
        this.#connectReject = null;
        reject(err instanceof Error ? err : new Error(String(err)));
        return;
      }
      this.#socket = socket;
      this.#bind(socket);
    });
  }

  send(message: ClientMsg): void {
    if (this.#state !== "open" || !this.#socket) {
      this.#emitter.emit("error", { kind: "send_before_open" });
      return;
    }
    this.#socket.send(encodeClientMsg(message));
  }

  close(code?: number, reason?: string): void {
    if (!this.#socket) {
      this.#state = "closed";
      return;
    }
    this.#state = "closing";
    this.#socket.close(code, reason);
  }

  onMessage(cb: (message: ServerMsg) => void): Unsubscribe {
    return this.#emitter.on("message", cb);
  }

  onClose(cb: (event: TerminalCloseEvent) => void): Unsubscribe {
    return this.#emitter.on("close", cb);
  }

  onError(cb: (error: TerminalTransportError) => void): Unsubscribe {
    return this.#emitter.on("error", cb);
  }

  #bind(socket: WebSocketLike): void {
    this.#onOpen = () => {
      this.#state = "open";
      const resolve = this.#connectResolve;
      this.#connectResolve = null;
      this.#connectReject = null;
      resolve?.();
    };
    this.#onMessage = (event) => {
      // The protocol is JSON-only. ArrayBuffer / Blob frames are rejected
      // upstream by the backend; here we surface a decode error rather
      // than try to decode binary data.
      if (typeof event.data !== "string") {
        this.#emitter.emit("error", {
          kind: "decode",
          decode: { kind: "invalid_json" },
        });
        return;
      }
      const result = decodeServerMsg(event.data);
      if (!result.ok) {
        this.#emitter.emit("error", {
          kind: "decode",
          decode: result.failure,
        });
        return;
      }
      this.#emitter.emit("message", result.message);
    };
    this.#onClose = (event) => {
      const wasConnecting = this.#state === "connecting";
      this.#state = "closed";
      if (wasConnecting && this.#connectReject) {
        const reject = this.#connectReject;
        this.#connectResolve = null;
        this.#connectReject = null;
        reject(new Error(`websocket closed before open (code=${event.code})`));
      }
      this.#emitter.emit("close", {
        code: event.code,
        reason: event.reason,
        wasClean: event.wasClean,
      });
      this.#unbind();
    };
    this.#onError = () => {
      this.#emitter.emit("error", { kind: "network" });
    };

    socket.addEventListener("open", this.#onOpen);
    socket.addEventListener("message", this.#onMessage);
    socket.addEventListener("close", this.#onClose);
    socket.addEventListener("error", this.#onError);
  }

  #unbind(): void {
    const socket = this.#socket;
    if (!socket) {
      return;
    }
    if (this.#onOpen) socket.removeEventListener("open", this.#onOpen);
    if (this.#onMessage) socket.removeEventListener("message", this.#onMessage);
    if (this.#onClose) socket.removeEventListener("close", this.#onClose);
    if (this.#onError) socket.removeEventListener("error", this.#onError);
    this.#onOpen = null;
    this.#onMessage = null;
    this.#onClose = null;
    this.#onError = null;
    this.#socket = null;
  }
}
