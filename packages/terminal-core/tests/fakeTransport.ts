import {
  type BinaryFrame,
  type ClientMsg,
  type ServerMsg,
  type TerminalCloseEvent,
  type TerminalTransport,
  type TerminalTransportError,
  type TransportReadyState,
  type Unsubscribe,
} from "../src/index.js";
import { TypedEmitter } from "../src/events.js";

interface Events {
  message: ServerMsg;
  binary: BinaryFrame;
  close: TerminalCloseEvent;
  error: TerminalTransportError;
}

/**
 * Minimal in-memory transport for unit tests. The test code drives the
 * transport directly via `simulate*` methods; the production
 * `WebSocketTerminalTransport` is exercised by integration tests against
 * the real backend in a later slice.
 */
export class FakeTransport implements TerminalTransport {
  readonly sent: ClientMsg[] = [];
  readonly sentBinary: Uint8Array[] = [];
  readonly emitter = new TypedEmitter<Events>();
  state: TransportReadyState = "idle";
  /** When false, `connect()` rejects with a fake transport error. */
  shouldOpen = true;

  get readyState(): TransportReadyState {
    return this.state;
  }

  async connect(_url: string): Promise<void> {
    this.state = "connecting";
    if (!this.shouldOpen) {
      this.state = "closed";
      throw new Error("fake transport refused to open");
    }
    this.state = "open";
  }

  send(message: ClientMsg): void {
    if (this.state !== "open") {
      this.emitter.emit("error", { kind: "send_before_open" });
      return;
    }
    this.sent.push(message);
  }

  sendBinary(frame: Uint8Array): void {
    if (this.state !== "open") {
      this.emitter.emit("error", { kind: "send_before_open" });
      return;
    }
    // Defensive copy so a caller mutating its buffer can't change what
    // the test sees later.
    this.sentBinary.push(new Uint8Array(frame));
  }

  close(_code?: number, _reason?: string): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.emitter.emit("close", {
      code: 1000,
      reason: "fake",
      wasClean: true,
    });
  }

  onMessage(cb: (msg: ServerMsg) => void): Unsubscribe {
    return this.emitter.on("message", cb);
  }

  onBinary(cb: (frame: BinaryFrame) => void): Unsubscribe {
    return this.emitter.on("binary", cb);
  }

  onClose(cb: (e: TerminalCloseEvent) => void): Unsubscribe {
    return this.emitter.on("close", cb);
  }

  onError(cb: (e: TerminalTransportError) => void): Unsubscribe {
    return this.emitter.on("error", cb);
  }

  // --- driver helpers used by tests ---

  simulateServerMsg(msg: ServerMsg): void {
    this.emitter.emit("message", msg);
  }

  simulateBinary(frame: BinaryFrame): void {
    this.emitter.emit("binary", frame);
  }

  simulateClose(event?: Partial<TerminalCloseEvent>): void {
    this.state = "closed";
    this.emitter.emit("close", {
      code: event?.code ?? 1006,
      reason: event?.reason ?? "abnormal",
      wasClean: event?.wasClean ?? false,
    });
  }

  simulateTransportError(error: TerminalTransportError): void {
    this.emitter.emit("error", error);
  }
}
