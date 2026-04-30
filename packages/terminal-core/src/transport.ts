/**
 * Renderer-neutral WebSocket transport for the RelayTerm protocol.
 *
 * The transport is responsible for:
 *  - opening a WebSocket
 *  - encoding outbound `ClientMsg` values (JSON control plane) and raw
 *    binary frames (data plane: `Input`)
 *  - decoding inbound text frames into typed `ServerMsg` (or a structured
 *    decode failure that callers can map to a protocol error event)
 *  - decoding inbound binary frames into typed `BinaryFrame`
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
import {
  decodeBinaryFrame,
  type BinaryDecodeFailure,
  type BinaryFrame,
} from "./binary.js";
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
   * - `decode`: a server JSON frame failed to decode; payload is NOT included.
   * - `binary_decode`: an inbound binary frame failed to decode; payload
   *   bytes are NOT included.
   * - `send_before_open`: caller invoked `send` before `connect` resolved.
   */
  kind: "network" | "decode" | "binary_decode" | "send_before_open";
  /** Decode failure detail when `kind === "decode"`. */
  decode?: DecodeFailure;
  /** Decode failure detail when `kind === "binary_decode"`. */
  binaryDecode?: BinaryDecodeFailure;
}

export interface TerminalTransport {
  connect(url: string): Promise<void>;
  send(message: ClientMsg): void;
  /** Send a raw pre-encoded binary frame (data plane). */
  sendBinary(frame: Uint8Array): void;
  close(code?: number, reason?: string): void;
  onMessage(cb: (message: ServerMsg) => void): Unsubscribe;
  /** Subscribe to inbound binary frames (data plane). */
  onBinary(cb: (frame: BinaryFrame) => void): Unsubscribe;
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
  binary: BinaryFrame;
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
  /**
   * Configures the format binary frames are surfaced in to listeners.
   * The transport sets this to `"arraybuffer"` so `MessageEvent.data` is
   * a `Uint8Array`-friendly buffer instead of a `Blob`. Optional on the
   * interface so test fakes don't have to implement it (production
   * `WebSocket` requires it).
   */
  binaryType?: "blob" | "arraybuffer";
  send(data: string | ArrayBufferView | ArrayBuffer): void;
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
      // The data plane carries raw binary frames; ask the browser to
      // surface them as ArrayBuffers (default is `Blob` which would
      // force an async unwrap before decode).
      try {
        socket.binaryType = "arraybuffer";
      } catch {
        // Test fakes / non-browser environments may forbid setting it.
        // Fall through; if a binary frame ever arrives without an
        // ArrayBuffer we'll surface a typed decode error in `#bind`.
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

  sendBinary(frame: Uint8Array): void {
    if (this.#state !== "open" || !this.#socket) {
      this.#emitter.emit("error", { kind: "send_before_open" });
      return;
    }
    // Pass the underlying ArrayBuffer slice. Browsers accept either
    // ArrayBufferView or ArrayBuffer; using the view keeps zero-copy.
    this.#socket.send(frame);
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

  onBinary(cb: (frame: BinaryFrame) => void): Unsubscribe {
    return this.#emitter.on("binary", cb);
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
      const data = event.data;
      if (typeof data === "string") {
        const result = decodeServerMsg(data);
        if (!result.ok) {
          this.#emitter.emit("error", {
            kind: "decode",
            decode: result.failure,
          });
          return;
        }
        this.#emitter.emit("message", result.message);
        return;
      }
      // Binary frame (data plane). Coerce to a Uint8Array view; if the
      // peer somehow sent a Blob we cannot decode synchronously and
      // surface a typed failure instead of leaking bytes.
      const bytes = coerceBinaryData(data);
      if (bytes === null) {
        this.#emitter.emit("error", {
          kind: "binary_decode",
          binaryDecode: { kind: "truncated_header" },
        });
        return;
      }
      const result = decodeBinaryFrame(bytes);
      if (!result.ok) {
        this.#emitter.emit("error", {
          kind: "binary_decode",
          binaryDecode: result.failure,
        });
        return;
      }
      this.#emitter.emit("binary", result.frame);
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

  // ----- helpers -----

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

/**
 * Coerce the inbound binary `MessageEvent.data` into a `Uint8Array`.
 *
 * `WebSocket` with `binaryType === "arraybuffer"` hands us an
 * `ArrayBuffer`; node's `ws` and some test fakes hand a `Uint8Array` /
 * `Buffer` directly. A `Blob` cannot be decoded synchronously (it
 * requires `await blob.arrayBuffer()`), so we return `null` and the
 * caller surfaces a typed decode failure rather than awaiting on the
 * hot path. A correctly-configured production socket never lands in
 * the Blob branch.
 */
function coerceBinaryData(data: unknown): Uint8Array | null {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (
    typeof data === "object" &&
    data !== null &&
    ArrayBuffer.isView(data as ArrayBufferView)
  ) {
    const view = data as ArrayBufferView;
    return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  }
  return null;
}
