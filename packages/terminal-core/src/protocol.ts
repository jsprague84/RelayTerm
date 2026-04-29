/**
 * Wire-protocol types and codec mirroring `relayterm_protocol`.
 *
 * The Rust crate is the source of truth for tag strings and shape.
 * Anything in this file that drifts is a bug — the backend will reject
 * the frame as `invalid_message`. Keep variant tags, field names, and
 * `code` strings in lockstep.
 *
 * Encoding is JSON-over-WebSocket. Binary frames are not part of the
 * protocol skeleton and the backend rejects them.
 */

/** Wire-stable error codes the server emits inside `error` frames. */
export type ServerErrorCode =
  | "invalid_message"
  | "invalid_input"
  | "pty_not_implemented"
  | "internal";

/** Acknowledgement-kind tag for `ack` frames. */
export type AckKind = "resize";

/** Lifecycle status of a freshly attached client. */
export type SessionAttachStatus = "attached_stub";

/** Branded string aliases for ids that travel over the wire. */
export type SessionId = string;
export type AttachmentId = string;

/** Sequence number reserved for the future replay slice. */
export type SeqNo = number;

export interface PingMsg {
  type: "ping";
}

export interface AttachMsg {
  type: "attach";
  session_id?: SessionId | null;
  last_seen_seq?: SeqNo | null;
  client_id?: string | null;
}

export interface InputMsg {
  type: "input";
  data: string;
}

export interface ResizeMsg {
  type: "resize";
  cols: number;
  rows: number;
}

export interface DetachMsg {
  type: "detach";
}

export interface CloseMsg {
  type: "close";
}

export type ClientMsg =
  | PingMsg
  | AttachMsg
  | InputMsg
  | ResizeMsg
  | DetachMsg
  | CloseMsg;

export interface PongMsg {
  type: "pong";
}

export interface SessionAttachedMsg {
  type: "session_attached";
  session_id: SessionId;
  attachment_id: AttachmentId;
  status: SessionAttachStatus;
  message: string;
}

export interface AckMsg {
  type: "ack";
  kind: AckKind;
}

export interface OutputMsg {
  type: "output";
  seq: SeqNo;
  data: string;
}

export interface ReplayWindowLostMsg {
  type: "replay_window_lost";
}

export interface SessionDetachedMsg {
  type: "session_detached";
  session_id: SessionId;
  attachment_id: AttachmentId;
}

export interface SessionClosedMsg {
  type: "session_closed";
  session_id: SessionId;
}

export interface ErrorMsg {
  type: "error";
  code: ServerErrorCode;
  message: string;
}

export type ServerMsg =
  | PongMsg
  | SessionAttachedMsg
  | AckMsg
  | OutputMsg
  | ReplayWindowLostMsg
  | SessionDetachedMsg
  | SessionClosedMsg
  | ErrorMsg;

export type ServerMsgType = ServerMsg["type"];

const SERVER_MSG_TYPES: readonly ServerMsgType[] = [
  "pong",
  "session_attached",
  "ack",
  "output",
  "replay_window_lost",
  "session_detached",
  "session_closed",
  "error",
];

/**
 * Decoded result that never throws: the caller always gets either a typed
 * server message or a structured failure. We deliberately do NOT include
 * the offending payload in `decoded.failure` so an attacker probing the
 * decoder can't induce log lines that echo their input.
 */
export type DecodeResult =
  | { ok: true; message: ServerMsg }
  | { ok: false; failure: DecodeFailure };

export type DecodeFailure =
  | { kind: "invalid_json" }
  | {
      kind: "unknown_type";
      /**
       * Server-supplied `type` tag that did not match any known variant,
       * sanitized and length-capped before being surfaced. Callers MAY
       * log this for triage, but MUST NOT echo it back unchanged in any
       * UI surface — a hostile or buggy peer could put control bytes in
       * the tag. The sanitizer keeps `[A-Za-z0-9_]` only, caps at 32
       * chars, and uses the literal `"<missing>"` when no `type` field
       * is present.
       */
      received: string;
    }
  | { kind: "invalid_shape"; received: ServerMsgType };

/**
 * Encode a client message to a JSON string suitable for `WebSocket.send`.
 *
 * `JSON.stringify` is enough — the shape is constrained at the type level
 * and the backend's serde tags match these literals. Errors here are
 * impossible for legal `ClientMsg` values, but a defensive try/catch is
 * cheap and keeps callers from having to handle a thrown error from a
 * pure encode call.
 */
export function encodeClientMsg(msg: ClientMsg): string {
  return JSON.stringify(msg);
}

/**
 * Parse a server frame.
 *
 * The function never throws and never embeds the raw payload in its
 * return value. Callers map a non-`ok` result to a typed protocol error
 * event, which is the only thing UI code ever sees.
 */
export function decodeServerMsg(raw: string): DecodeResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { ok: false, failure: { kind: "invalid_json" } };
  }
  if (!isPlainObject(parsed)) {
    return { ok: false, failure: { kind: "invalid_json" } };
  }
  const tag = parsed["type"];
  if (typeof tag !== "string") {
    return { ok: false, failure: { kind: "unknown_type", received: "<missing>" } };
  }
  if (!isKnownServerType(tag)) {
    // `tag` is server-controlled and can carry anything a buggy or
    // hostile peer puts in the `type` field. Sanitize before exposing it
    // through the public `DecodeFailure` shape.
    return {
      ok: false,
      failure: { kind: "unknown_type", received: sanitizeTypeTag(tag) },
    };
  }
  if (!matchesShape(tag, parsed)) {
    return { ok: false, failure: { kind: "invalid_shape", received: tag } };
  }
  return { ok: true, message: parsed as unknown as ServerMsg };
}

const TAG_SAFE_CHAR = /^[A-Za-z0-9_]$/;
const TAG_MAX_LEN = 32;

function sanitizeTypeTag(value: string): string {
  let out = "";
  for (const ch of value) {
    if (out.length >= TAG_MAX_LEN) break;
    out += TAG_SAFE_CHAR.test(ch) ? ch : "_";
  }
  return out.length === 0 ? "<unsanitizable>" : out;
}

function isKnownServerType(tag: string): tag is ServerMsgType {
  return (SERVER_MSG_TYPES as readonly string[]).includes(tag);
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Cheap structural validation per variant. We stop at presence + primitive
 * type checks; the goal is to reject obviously malformed frames so the
 * client state machine can't crash on a missing field, not to be a full
 * schema validator. The Rust side already enforces shape at the wire.
 */
function matchesShape(tag: ServerMsgType, value: Record<string, unknown>): boolean {
  switch (tag) {
    case "pong":
    case "replay_window_lost":
      return true;
    case "session_attached":
      return (
        typeof value["session_id"] === "string" &&
        typeof value["attachment_id"] === "string" &&
        value["status"] === "attached_stub" &&
        typeof value["message"] === "string"
      );
    case "ack":
      return value["kind"] === "resize";
    case "output":
      return (
        typeof value["seq"] === "number" && typeof value["data"] === "string"
      );
    case "session_detached":
      return (
        typeof value["session_id"] === "string" &&
        typeof value["attachment_id"] === "string"
      );
    case "session_closed":
      return typeof value["session_id"] === "string";
    case "error":
      return (
        isKnownErrorCode(value["code"]) && typeof value["message"] === "string"
      );
  }
}

const SERVER_ERROR_CODES: readonly ServerErrorCode[] = [
  "invalid_message",
  "invalid_input",
  "pty_not_implemented",
  "internal",
];

function isKnownErrorCode(value: unknown): value is ServerErrorCode {
  return (
    typeof value === "string" &&
    (SERVER_ERROR_CODES as readonly string[]).includes(value)
  );
}
