/**
 * `@relayterm/terminal-core` — renderer-neutral protocol/client foundation.
 *
 * This package is the integration seam between the backend's typed
 * WebSocket protocol and any renderer (xterm.js, libghostty-vt, etc.).
 * It deliberately has zero renderer dependencies — see `renderer.ts`
 * for the abstract interface adapters implement, and `client.ts` for
 * the lifecycle state machine that drives them.
 */

export {
  type AckKind,
  type AckMsg,
  type AttachMsg,
  type AttachmentId,
  type ClientMsg,
  type CloseMsg,
  type DecodeFailure,
  type DecodeResult,
  type DetachMsg,
  type ErrorMsg,
  type InputMsg,
  type OutputMsg,
  type PingMsg,
  type PongMsg,
  type ReplayWindowLostMsg,
  type ResizeMsg,
  type SeqNo,
  type ServerErrorCode,
  type ServerMsg,
  type ServerMsgType,
  type SessionAttachStatus,
  type SessionAttachedMsg,
  type SessionClosedMsg,
  type SessionDetachedMsg,
  type SessionId,
  decodeServerMsg,
  encodeClientMsg,
} from "./protocol.js";

export { type Listener, type Unsubscribe } from "./events.js";

export {
  type TerminalCloseEvent,
  type TerminalTransport,
  type TerminalTransportError,
  type TransportReadyState,
  type WebSocketFactory,
  type WebSocketLike,
  type WebSocketLikeEventMap,
  type WebSocketTransportOptions,
  WebSocketTerminalTransport,
} from "./transport.js";

export {
  type RendererInput,
  type RendererOutput,
  type TerminalPreferences,
  type TerminalRenderer,
  type TerminalThemePreferences,
} from "./renderer.js";

export {
  type AttachOptions,
  type InputRejectionReason,
  type TerminalClientError,
  type TerminalSessionClientOptions,
  type TerminalSessionState,
  TerminalSessionClient,
} from "./client.js";
