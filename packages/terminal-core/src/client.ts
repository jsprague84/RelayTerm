/**
 * `TerminalSessionClient` — coordinates one terminal-session attachment.
 *
 * The client wraps a `TerminalTransport` and exposes a small, typed
 * surface that a renderer (or diagnostic UI) can drive. It owns:
 *  - the lifecycle state machine (`idle → connecting → attached → …`)
 *  - the attach-handshake bookkeeping (first server frame must be
 *    `session_attached`; otherwise we go to `error`)
 *  - protocol-level event fan-out so consumers don't have to know about
 *    transport-level decode failures
 *
 * The client deliberately does NOT:
 *  - hold any reference to a renderer (renderer-neutral rule)
 *  - implement reconnect/replay logic — `last_seen_seq` is plumbed
 *    through but the future slice owns the policy
 *  - log raw input payloads anywhere (the same redaction rule the
 *    backend enforces applies here as a defence-in-depth measure)
 *
 * Resize-before-attach policy: REJECTED, not queued. Queuing means the
 * client has to decide what to do if the resize never lands (timeout?
 * drop?), and the renderer can simply re-fire on `attached`. Rejection
 * surfaces as a typed `input_rejected_or_stubbed` event with a stable
 * `reason` so the UI can react if it wants.
 */

import { TypedEmitter, type Unsubscribe } from "./events.js";
import {
  type AckMsg,
  type AttachMsg,
  type AttachmentId,
  type ClientMsg,
  type CloseMsg,
  type DetachMsg,
  type ErrorMsg,
  type InputMsg,
  type OutputMsg,
  type PingMsg,
  type PongMsg,
  type ReplayEndMsg,
  type ReplayStartMsg,
  type ReplayWindowLostMsg,
  type ResizeMsg,
  type SeqNo,
  type ServerMsg,
  type SessionAttachedMsg,
  type SessionClosedMsg,
  type SessionDetachedMsg,
  type SessionId,
  encodeOutputData,
} from "./protocol.js";
import {
  encodeBinaryFrame,
  type BinaryFrame,
} from "./binary.js";
import type {
  TerminalCloseEvent,
  TerminalTransport,
  TerminalTransportError,
} from "./transport.js";

export type TerminalSessionState =
  | "idle"
  | "connecting"
  | "attached"
  | "detached"
  | "closed"
  | "error";

/**
 * Client-level error envelope. Distinct from transport errors so consumers
 * can tell "the socket itself misbehaved" from "the protocol said no."
 */
export interface TerminalClientError {
  kind:
    | "transport"
    | "decode"
    | "unexpected_first_frame"
    | "server_error"
    | "send_before_attached"
    | "send_after_terminal";
  /** Server-supplied wire code if `kind === "server_error"`. */
  code?: ErrorMsg["code"];
  /**
   * Short, public message. For server errors this mirrors the backend's
   * static `message` field; for client-side rejections this is a fixed
   * string. The raw payload of any rejected `input` frame is NEVER
   * included.
   */
  message: string;
}

/**
 * Reason the client refused or stubbed an outbound request. Used by the
 * `input_rejected_or_stubbed` event so callers can react without parsing
 * messages.
 */
export interface InputRejectionReason {
  reason:
    | "not_attached"
    | "pty_not_implemented"
    | "after_terminal_state";
  /** Which client message triggered the rejection (no payload). */
  attempted: "input" | "resize" | "ping" | "detach" | "close";
}

interface ClientEvents {
  state_change: TerminalSessionState;
  attached: SessionAttachedMsg;
  detached: SessionDetachedMsg;
  closed: SessionClosedMsg;
  ack: AckMsg;
  resize_ack: AckMsg;
  pong: PongMsg;
  output: OutputMsg;
  replay_start: ReplayStartMsg;
  replay_end: ReplayEndMsg;
  replay_window_lost: ReplayWindowLostMsg;
  error: TerminalClientError;
  input_rejected_or_stubbed: InputRejectionReason;
}

export interface AttachOptions {
  /** WebSocket URL to connect. */
  url: string;
  /**
   * Informational session id sent on the `attach` frame. The canonical
   * id comes from the URL path on the backend, so this is for client
   * bookkeeping only.
   */
  sessionId?: SessionId;
  /**
   * Resume bookmark for the in-memory replay buffer. When supplied,
   * the server emits any buffered `output` frames newer than
   * `lastSeenSeq` (bracketed by `replay_start` / `replay_end`) BEFORE
   * resuming the live stream. If the bookmark predates the bounded
   * buffer's window, the server emits a single `replay_window_lost`
   * frame with diagnostic seq metadata and continues live attach. A
   * brand-new attach should leave this `undefined` rather than passing
   * `0` — `0` is also treated as "no bookmark," but `undefined` is
   * the explicit shape.
   */
  lastSeenSeq?: SeqNo;
  /** Stable client identifier (e.g. browser tab id). */
  clientId?: string;
}

export interface TerminalSessionClientOptions {
  transport: TerminalTransport;
}

export class TerminalSessionClient {
  readonly #transport: TerminalTransport;
  readonly #emitter = new TypedEmitter<ClientEvents>();
  readonly #unsubscribers: Unsubscribe[] = [];
  #state: TerminalSessionState = "idle";
  #disposed = false;
  /** Pending `attach` payload, queued so we can send it once the socket opens. */
  #pendingAttach: AttachOptions | null = null;
  /**
   * Captured from the `session_attached` frame, used to synthesize a
   * `SessionDetachedMsg` when the transport drops without an explicit
   * `Detach` or `Close` frame. Without this stash the renderer would only
   * see `state_change → detached` and would miss the typed `detached`
   * event the SPEC contract guarantees.
   */
  #attachedIds: { sessionId: SessionId; attachmentId: AttachmentId } | null = null;
  /**
   * Highest output `seq` observed on the wire (replayed or live).
   * Reset to `0` only on construction; persists across `state_change`
   * transitions so a renderer that reconnects can read it back via
   * `lastSeenSeq` and pass it to the next `attach()` call.
   */
  #lastSeenSeq: SeqNo = 0;

  constructor(options: TerminalSessionClientOptions) {
    this.#transport = options.transport;
    this.#unsubscribers.push(
      this.#transport.onMessage((msg) => this.#onMessage(msg)),
      this.#transport.onBinary((frame) => this.#onBinary(frame)),
      this.#transport.onClose((event) => this.#onTransportClose(event)),
      this.#transport.onError((err) => this.#onTransportError(err)),
    );
  }

  get state(): TerminalSessionState {
    return this.#state;
  }

  /**
   * Highest output `seq` this client has observed (replayed or live).
   * `0` until the first `output` frame lands. Pass this back to the
   * next `attach()` call as `lastSeenSeq` to request replay across a
   * reconnect — the bounded server-side buffer fills the gap when
   * possible and emits `replay_window_lost` when the bookmark predates
   * the buffer.
   */
  get lastSeenSeq(): SeqNo {
    return this.#lastSeenSeq;
  }

  /**
   * Open the WebSocket and send `attach` once the socket reaches `open`.
   *
   * The backend's `ws_attach` route already attaches the session on
   * upgrade, so the wire `attach` frame is technically redundant today.
   * We still send it because the protocol contract makes it the
   * client-driven handshake, and a future slice may use the carried
   * `last_seen_seq` for replay coordination.
   */
  async attach(options: AttachOptions): Promise<void> {
    if (this.#disposed) {
      throw new Error("client disposed");
    }
    if (this.#state !== "idle") {
      throw new Error(`cannot attach from state ${this.#state}`);
    }
    this.#pendingAttach = options;
    this.#setState("connecting");
    try {
      await this.#transport.connect(options.url);
    } catch (err) {
      this.#pendingAttach = null;
      this.#emitError({
        kind: "transport",
        message: errMessage(err, "websocket failed to open"),
      });
      this.#setState("error");
      throw err;
    }
    // Send the attach frame immediately on open. We do this from the
    // resolving connect rather than an extra "open" event so the order
    // is deterministic across transports.
    const attach: AttachMsg = {
      type: "attach",
      session_id: options.sessionId ?? null,
      last_seen_seq: options.lastSeenSeq ?? null,
      client_id: options.clientId ?? null,
    };
    this.#transport.send(attach);
  }

  sendPing(): void {
    if (!this.#requireAttached("ping")) return;
    const ping: PingMsg = { type: "ping" };
    this.#transport.send(ping);
  }

  /**
   * Send a keystroke / paste payload to the live PTY.
   *
   * Defaults to the binary `Input` envelope on the data plane (RTB1 v1):
   * raw bytes are forwarded straight to the SSH PTY's stdin without a
   * base64 round-trip. Pass `{ legacyJson: true }` to fall back to the
   * JSON `input` frame — useful only for diagnostic/dev paths that need
   * to exercise the legacy decoder. Strings are UTF-8 encoded; the
   * caller can also hand a `Uint8Array` to bypass encoding entirely
   * (e.g. for renderers that already produce bytes).
   *
   * Logging guarantee: the payload bytes never reach a tracing log,
   * client error envelope, or rejection event — same redaction rule
   * the backend enforces.
   */
  sendInput(
    data: string | Uint8Array,
    options: { legacyJson?: boolean } = {},
  ): void {
    if (!this.#requireAttached("input")) return;
    if (options.legacyJson) {
      const text = typeof data === "string" ? data : new TextDecoder().decode(data);
      const input: InputMsg = { type: "input", data: text };
      this.#transport.send(input);
      return;
    }
    const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
    const result = encodeBinaryFrame("input", 0, bytes);
    if (!result.ok) {
      // Only failure is `payload_too_large`. Surface a typed client
      // error so a renderer with a misbehaving paste doesn't end up
      // spinning silently. The payload is NOT included.
      this.#emitError({
        kind: "transport",
        message: "input frame exceeds maximum payload size",
      });
      return;
    }
    this.#transport.sendBinary(result.bytes);
  }

  sendResize(cols: number, rows: number): void {
    if (!this.#requireAttached("resize")) return;
    const resize: ResizeMsg = { type: "resize", cols, rows };
    this.#transport.send(resize);
  }

  detach(): void {
    // `detach` is a teardown verb: calling it before attach (idle /
    // connecting) or after a terminal state is treated as a no-op rather
    // than a user mistake — there is nothing useful for the renderer to
    // react to. Only an in-flight `attached` session can issue the wire
    // frame.
    if (this.#state !== "attached") {
      return;
    }
    const detach: DetachMsg = { type: "detach" };
    this.#transport.send(detach);
  }

  close(): void {
    // Same teardown semantics as `detach`: silent no-op outside the
    // `attached` state. The diagnostic UI / renderer can poll `state`
    // before calling close if it cares.
    if (this.#state !== "attached") {
      return;
    }
    const close: CloseMsg = { type: "close" };
    this.#transport.send(close);
  }

  /** Stop listening to the transport. Idempotent. */
  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    for (const unsub of this.#unsubscribers) {
      unsub();
    }
    this.#unsubscribers.length = 0;
    this.#emitter.removeAll();
  }

  on<K extends keyof ClientEvents>(
    event: K,
    listener: (payload: ClientEvents[K]) => void,
  ): Unsubscribe {
    return this.#emitter.on(event, listener);
  }

  #requireAttached(attempted: InputRejectionReason["attempted"]): boolean {
    if (this.#state === "attached") return true;
    if (
      this.#state === "closed" ||
      this.#state === "detached" ||
      this.#state === "error"
    ) {
      this.#emitter.emit("input_rejected_or_stubbed", {
        reason: "after_terminal_state",
        attempted,
      });
      this.#emitError({
        kind: "send_after_terminal",
        message: `cannot send ${attempted} after terminal state ${this.#state}`,
      });
      return false;
    }
    this.#emitter.emit("input_rejected_or_stubbed", {
      reason: "not_attached",
      attempted,
    });
    this.#emitError({
      kind: "send_before_attached",
      message: `cannot send ${attempted} from state ${this.#state}`,
    });
    return false;
  }

  #onMessage(msg: ServerMsg): void {
    if (this.#state === "connecting") {
      // The very first frame the backend emits on a successful upgrade
      // is `session_attached`. Anything else means the protocol broke
      // (or a stray decode happened); collapse to error.
      if (msg.type !== "session_attached") {
        this.#emitError({
          kind: "unexpected_first_frame",
          message: `expected session_attached, received ${msg.type}`,
        });
        this.#setState("error");
        return;
      }
      this.#pendingAttach = null;
      this.#attachedIds = {
        sessionId: msg.session_id,
        attachmentId: msg.attachment_id,
      };
      this.#setState("attached");
      this.#emitter.emit("attached", msg);
      return;
    }

    switch (msg.type) {
      case "session_attached":
        // Attached again? The backend never re-sends this, but treat it
        // as a protocol violation rather than silently overwriting.
        this.#emitError({
          kind: "unexpected_first_frame",
          message: "duplicate session_attached frame",
        });
        return;
      case "pong":
        this.#emitter.emit("pong", msg);
        return;
      case "ack":
        this.#emitter.emit("ack", msg);
        if (msg.kind === "resize") {
          this.#emitter.emit("resize_ack", msg);
        }
        return;
      case "output":
        // Track the highest seq we've seen so the renderer can pass it
        // back as `lastSeenSeq` on the next attach. Replayed and live
        // frames arrive on the same `output` channel and carry their
        // original seq, so we can update unconditionally.
        if (msg.seq > this.#lastSeenSeq) {
          this.#lastSeenSeq = msg.seq;
        }
        this.#emitter.emit("output", msg);
        return;
      case "replay_start":
        this.#emitter.emit("replay_start", msg);
        return;
      case "replay_end":
        if (msg.latest_seq > this.#lastSeenSeq) {
          this.#lastSeenSeq = msg.latest_seq;
        }
        this.#emitter.emit("replay_end", msg);
        return;
      case "replay_window_lost":
        // Skip ahead to the server's latest_seq — the renderer is
        // expected to reset its grid; the next live frame will be
        // `latest_seq + 1` and the bookmark must reflect that the
        // missed frames are unrecoverable.
        if (msg.latest_seq > this.#lastSeenSeq) {
          this.#lastSeenSeq = msg.latest_seq;
        }
        this.#emitter.emit("replay_window_lost", msg);
        return;
      case "session_detached":
        this.#emitter.emit("detached", msg);
        this.#setState("detached");
        return;
      case "session_closed":
        this.#emitter.emit("closed", msg);
        this.#setState("closed");
        return;
      case "error":
        if (msg.code === "pty_not_implemented" || msg.code === "pty_not_live") {
          // Either: (a) the legacy stub slice rejecting input; or (b) a
          // live session whose PTY tore down. The input was not
          // delivered. Surface the dedicated rejection event so the UI
          // doesn't have to special-case server vs client rejection.
          // We deliberately do NOT also emit the generic `error` event
          // here — a consumer listening to both would otherwise handle
          // the same stubbed rejection twice. Genuine server-side
          // failures still flow through the `error` event below.
          this.#emitter.emit("input_rejected_or_stubbed", {
            reason: "pty_not_implemented",
            attempted: "input",
          });
          return;
        }
        this.#emitError({
          kind: "server_error",
          code: msg.code,
          message: msg.message,
        });
        return;
    }
  }

  #onBinary(frame: BinaryFrame): void {
    if (frame.kind !== "output") {
      // Server only emits binary Output frames; an Input frame from
      // the server is a protocol violation. Surface as a typed error
      // — payload is NOT echoed.
      this.#emitError({
        kind: "decode",
        message: "unexpected binary frame kind from server",
      });
      return;
    }
    if (this.#state === "connecting") {
      // Binary frames before the JSON `session_attached` is a protocol
      // violation. Same as the JSON path: collapse to error.
      this.#emitError({
        kind: "unexpected_first_frame",
        message: "received binary frame before session_attached",
      });
      this.#setState("error");
      return;
    }
    if (frame.seq > this.#lastSeenSeq) {
      this.#lastSeenSeq = frame.seq;
    }
    // Re-encode payload as base64 so the public `output` event shape
    // stays identical to what the legacy JSON decoder produced. Renderer
    // code that already calls `decodeOutputData(msg.data)` keeps working
    // without change. A future slice may switch the public event to
    // carry a `Uint8Array` directly; that's deferred.
    const msg: OutputMsg = {
      type: "output",
      seq: frame.seq,
      data: encodeOutputData(frame.payload),
    };
    this.#emitter.emit("output", msg);
  }

  #onTransportClose(event: TerminalCloseEvent): void {
    if (this.#state === "closed" || this.#state === "detached") {
      return;
    }
    if (this.#state === "connecting") {
      // Closed before attach landed.
      this.#emitError({
        kind: "transport",
        message: `socket closed before attach (code=${event.code ?? "null"})`,
      });
      this.#setState("error");
      return;
    }
    // Attached then dropped without an explicit detach/close. The audit
    // bookkeeping happens server-side; the client just transitions to
    // detached so consumers know live streaming has stopped. We synthesize
    // a `session_detached` frame from the ids captured at attach time so
    // listeners that only watch the typed `detached` event (rather than
    // `state_change`) get a payload — the SPEC contract names "transport
    // closed after attach" as a path that fires `detached`.
    if (this.#attachedIds) {
      const synthetic: SessionDetachedMsg = {
        type: "session_detached",
        session_id: this.#attachedIds.sessionId,
        attachment_id: this.#attachedIds.attachmentId,
      };
      this.#emitter.emit("detached", synthetic);
    }
    this.#setState("detached");
  }

  #onTransportError(err: TerminalTransportError): void {
    switch (err.kind) {
      case "decode":
        this.#emitError({
          kind: "decode",
          message: `failed to decode server frame (${err.decode?.kind ?? "unknown"})`,
        });
        return;
      case "binary_decode":
        // Binary frame failed structural check. The classifier kind is
        // safe to surface (no payload bytes are ever included by the
        // codec); callers see "binary frame: <kind>" so the source of
        // the protocol break is obvious.
        this.#emitError({
          kind: "decode",
          message: `failed to decode binary frame (${err.binaryDecode?.kind ?? "unknown"})`,
        });
        return;
      case "network":
        this.#emitError({
          kind: "transport",
          message: "websocket transport error",
        });
        return;
      case "send_before_open":
        this.#emitError({
          kind: "transport",
          message: "tried to send before websocket open",
        });
        return;
    }
  }

  #emitError(error: TerminalClientError): void {
    this.#emitter.emit("error", error);
  }

  #setState(next: TerminalSessionState): void {
    if (this.#state === next) return;
    this.#state = next;
    this.#emitter.emit("state_change", next);
  }
}

function errMessage(err: unknown, fallback: string): string {
  if (err instanceof Error && err.message) return err.message;
  return fallback;
}

export type {
  ClientMsg,
  ServerMsg,
  AttachMsg,
  PingMsg,
  InputMsg,
  ResizeMsg,
  DetachMsg,
  CloseMsg,
  PongMsg,
  AckMsg,
  OutputMsg,
  ReplayEndMsg,
  ReplayStartMsg,
  ReplayWindowLostMsg,
  SessionAttachedMsg,
  SessionDetachedMsg,
  SessionClosedMsg,
  ErrorMsg,
};
