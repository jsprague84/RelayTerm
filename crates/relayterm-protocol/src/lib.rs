//! Wire protocol shared between the backend and web/Tauri clients.
//!
//! The protocol is JSON-over-WebSocket. Messages defined here are the
//! canonical schema; the web client mirrors these shapes.
//!
//! ## Scope (this slice)
//!
//! Today this crate carries the **lifecycle skeleton** for the terminal
//! WebSocket: attach/detach, ping/pong, resize, and a stub input message
//! that the backend acknowledges with a "PTY not implemented yet" error.
//! Real PTY byte streaming, replay-buffer protocol, and the binary frame
//! format all land in later slices and will extend (not replace) the
//! shapes below.
//!
//! No transport behavior lives here — only the shape of payloads.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use relayterm_core::ids::TerminalSessionAttachmentId;
use relayterm_core::{SeqNo, SessionId};
use serde::{Deserialize, Serialize};

/// Encode raw PTY bytes for transport in [`ServerMsg::Output::data`].
///
/// Centralised so the wire encoding is single-sourced — the backend
/// orchestrator and the TS mirror MUST agree byte-for-byte. Standard
/// alphabet + padding is the canonical RFC 4648 form `atob` accepts.
#[must_use]
pub fn output_data_encode(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Decode the inverse of [`output_data_encode`]. Returns `Err` for any
/// invalid base64 (control bytes, non-alphabet characters, broken
/// padding) so the caller can surface a stable
/// [`ErrorCode::InvalidMessage`] without echoing the offending value.
pub fn output_data_decode(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    BASE64.decode(encoded)
}

/// Wire-stable error codes the server emits inside [`ServerMsg::Error`].
///
/// These are public so the web client can match on them. New codes go at
/// the end; never renumber, never repurpose. `as_str()` is the canonical
/// wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Frame did not parse as a [`ClientMsg`] — malformed JSON or unknown
    /// `type` tag. The handler does not echo the payload back.
    InvalidMessage,
    /// A field in a parsed message failed validation (e.g. `cols`/`rows`
    /// out of range). The handler does not echo the offending value.
    InvalidInput,
    /// Reserved legacy code — emitted by earlier slices when the
    /// backend had no PTY surface at all. New deployments emit
    /// [`Self::PtyNotLive`] instead. Kept on the wire so old clients
    /// don't fail to decode the code; new clients SHOULD treat it
    /// identically to `PtyNotLive`.
    PtyNotImplemented,
    /// Client sent [`ClientMsg::Input`] (or resize) but the backend has
    /// no live PTY for this session — startup failed, the shell exited,
    /// or the session has already been closed. The payload bytes are
    /// NEVER reflected back or logged.
    PtyNotLive,
    /// SSH transport / auth / PTY allocation failed during attach. The
    /// session row is transitioned to `closed` server-side; the client
    /// must not retry on this socket.
    SshStartFailed,
    /// Catch-all for backend-side failures the client cannot recover from.
    Internal,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidMessage => "invalid_message",
            Self::InvalidInput => "invalid_input",
            Self::PtyNotImplemented => "pty_not_implemented",
            Self::PtyNotLive => "pty_not_live",
            Self::SshStartFailed => "ssh_start_failed",
            Self::Internal => "internal",
        }
    }
}

/// Acknowledgement-kind tag for [`ServerMsg::Ack`]. Lets the client tell
/// "your resize landed" from "your input was accepted (PTY pending)" etc.
/// without a separate message variant per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckKind {
    Resize,
}

/// Messages the client sends to the backend.
///
/// Tagged by `type` (snake_case). Unknown variants and malformed payloads
/// fail to deserialize — the WebSocket handler maps that to a
/// [`ServerMsg::Error`] with [`ErrorCode::InvalidMessage`] and the original
/// frame is NOT logged or echoed.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Liveness probe. Backend replies with [`ServerMsg::Pong`].
    Ping,
    /// Open or resume an attachment to the addressed terminal session.
    /// `session_id` is informational — the canonical session id comes from
    /// the URL path. `last_seen_seq` is reserved for replay (later slice).
    Attach {
        session_id: Option<SessionId>,
        last_seen_seq: Option<SeqNo>,
        client_id: Option<String>,
    },
    /// User keystroke / paste / etc. from the renderer.
    ///
    /// The backend currently does NOT forward these anywhere — there is no
    /// PTY yet. The handler responds with a [`ServerMsg::Error`] of
    /// [`ErrorCode::PtyNotImplemented`] without including or logging the
    /// payload bytes.
    Input { data: String },
    /// Renderer was resized.
    Resize { cols: u16, rows: u16 },
    /// Client is detaching cleanly. The session and its DB row stay alive.
    Detach,
    /// Client wants to close the session entirely (transition the row to
    /// `closed` and tear down any runtime state). Idempotent on the server.
    Close,
}

/// `Debug` is implemented manually so [`ClientMsg::Input::data`] never
/// appears in tracing logs or panic backtraces. The handler also takes
/// care never to format a raw frame, but masking at the type level is the
/// last line of defense if a future code path forgets.
impl fmt::Debug for ClientMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ping => f.debug_struct("Ping").finish(),
            Self::Attach {
                session_id,
                last_seen_seq,
                client_id,
            } => f
                .debug_struct("Attach")
                .field("session_id", session_id)
                .field("last_seen_seq", last_seen_seq)
                .field("client_id", client_id)
                .finish(),
            Self::Input { data } => f
                .debug_struct("Input")
                .field("data_len", &data.len())
                .field("data", &"<redacted terminal input>")
                .finish(),
            Self::Resize { cols, rows } => f
                .debug_struct("Resize")
                .field("cols", cols)
                .field("rows", rows)
                .finish(),
            Self::Detach => f.debug_struct("Detach").finish(),
            Self::Close => f.debug_struct("Close").finish(),
        }
    }
}

/// Messages the backend sends to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Reply to [`ClientMsg::Ping`].
    Pong,
    /// Reply to [`ClientMsg::Attach`]. The handler has validated session
    /// ownership, written the attachment metadata row, and registered the
    /// in-memory attachment runtime entry. `message` explicitly disclaims
    /// PTY readiness — see the static `STUB_PTY_NOT_IMPLEMENTED_MESSAGE`
    /// in the API for the exact wording, mirrored back here for clients
    /// that want to surface it verbatim.
    SessionAttached {
        session_id: SessionId,
        attachment_id: TerminalSessionAttachmentId,
        status: SessionAttachStatus,
        message: String,
    },
    /// Generic "we accepted your message" reply for non-state-changing
    /// non-data messages where a pong-shaped reply isn't enough (e.g.
    /// resize succeeded and the new dims are recorded).
    Ack { kind: AckKind },
    /// PTY output bytes from the remote shell.
    ///
    /// `data` is the **base64-encoded** raw PTY byte stream (standard
    /// alphabet, with padding). Base64 is used because the underlying
    /// JSON-over-WebSocket protocol cannot carry arbitrary binary bytes
    /// inside a string field — `\xff` is not valid UTF-8. A binary frame
    /// format is future work; clients SHOULD route every `Output` through
    /// [`output_data_decode`] / [`output_data_encode`] (or an equivalent)
    /// and write the raw bytes to the renderer. `seq` is a monotonic
    /// per-session counter the orchestrator stamps so the future replay
    /// slice can reason about ordering — it is NOT a guarantee that any
    /// gap in `seq` is recoverable in this slice.
    Output { seq: SeqNo, data: String },
    /// Replay window has expired; the client must reset.
    ReplayWindowLost,
    /// Server confirms the attachment has been recorded as detached.
    /// The session row stays alive; only this client's attachment is closed.
    SessionDetached {
        session_id: SessionId,
        attachment_id: TerminalSessionAttachmentId,
    },
    /// Server confirms the session has transitioned to `closed`.
    SessionClosed { session_id: SessionId },
    /// Backend-side error surfaced to the client. `code` is the wire-stable
    /// classifier; `message` is a short, static, public string. Neither
    /// field carries raw input or operator detail.
    Error { code: ErrorCode, message: String },
}

/// Lifecycle status of a freshly attached client.
///
/// Wire-stable; new variants append. Existing clients SHOULD treat any
/// unknown variant defensively (no PTY assumed) rather than failing to
/// decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttachStatus {
    /// Reserved for the placeholder slice. Emitted only when no live PTY
    /// is wired (e.g. the session reached `attached` before SSH startup).
    /// Sending an [`ClientMsg::Input`] returns [`ErrorCode::PtyNotLive`]
    /// or [`ErrorCode::PtyNotImplemented`] (legacy code) depending on
    /// the build.
    AttachedStub,
    /// Attachment exists AND the backend has a live PTY: input flows to
    /// the remote shell, [`ServerMsg::Output`] frames stream back,
    /// resize is honoured. Replay/resume across reconnects is NOT yet
    /// guaranteed by this variant — it carries no `last_seen_seq` —
    /// that is a future, deliberate slice.
    Active,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_msg_tags_are_stable() {
        // Locked: changing any of these tags is a wire-breaking change.
        // The web/Tauri client matches on these exact strings.
        for (msg, expected_type) in [
            (ClientMsg::Ping, "ping"),
            (
                ClientMsg::Attach {
                    session_id: None,
                    last_seen_seq: None,
                    client_id: None,
                },
                "attach",
            ),
            (
                ClientMsg::Input {
                    data: "x".to_owned(),
                },
                "input",
            ),
            (ClientMsg::Resize { cols: 80, rows: 24 }, "resize"),
            (ClientMsg::Detach, "detach"),
            (ClientMsg::Close, "close"),
        ] {
            let v = serde_json::to_value(&msg).unwrap();
            assert_eq!(v["type"], expected_type, "tag drift for {msg:?}");
        }
    }

    #[test]
    fn server_msg_tags_are_stable() {
        let attachment_id = TerminalSessionAttachmentId::new();
        let session_id = SessionId::new();
        for (msg, expected_type) in [
            (ServerMsg::Pong, "pong"),
            (
                ServerMsg::SessionAttached {
                    session_id,
                    attachment_id,
                    status: SessionAttachStatus::AttachedStub,
                    message: "msg".to_owned(),
                },
                "session_attached",
            ),
            (
                ServerMsg::Ack {
                    kind: AckKind::Resize,
                },
                "ack",
            ),
            (
                ServerMsg::Output {
                    seq: SeqNo(1),
                    data: String::new(),
                },
                "output",
            ),
            (ServerMsg::ReplayWindowLost, "replay_window_lost"),
            (
                ServerMsg::SessionDetached {
                    session_id,
                    attachment_id,
                },
                "session_detached",
            ),
            (ServerMsg::SessionClosed { session_id }, "session_closed"),
            (
                ServerMsg::Error {
                    code: ErrorCode::Internal,
                    message: "boom".to_owned(),
                },
                "error",
            ),
        ] {
            let v = serde_json::to_value(&msg).unwrap();
            assert_eq!(v["type"], expected_type, "tag drift");
        }
    }

    #[test]
    fn error_code_strings_are_stable() {
        // Locked wire constants. New codes append; never renumber.
        for (code, expected) in [
            (ErrorCode::InvalidMessage, "invalid_message"),
            (ErrorCode::InvalidInput, "invalid_input"),
            (ErrorCode::PtyNotImplemented, "pty_not_implemented"),
            (ErrorCode::PtyNotLive, "pty_not_live"),
            (ErrorCode::SshStartFailed, "ssh_start_failed"),
            (ErrorCode::Internal, "internal"),
        ] {
            assert_eq!(code.as_str(), expected);
            assert_eq!(serde_json::to_value(code).unwrap(), expected);
        }
    }

    #[test]
    fn output_data_codec_round_trips_arbitrary_bytes() {
        // Includes high-bit / control bytes that would never survive
        // a naive utf-8 wrap. Base64 carries them losslessly.
        let raw: Vec<u8> = (0..=255u8).collect();
        let encoded = output_data_encode(&raw);
        let decoded = output_data_decode(&encoded).expect("round-trip decode");
        assert_eq!(decoded, raw);
    }

    #[test]
    fn output_data_decode_rejects_invalid_base64() {
        let res = output_data_decode("!!!not-base64!!!");
        assert!(res.is_err(), "invalid base64 must surface an error");
    }

    #[test]
    fn session_attach_status_active_serializes_to_active() {
        let v = serde_json::to_value(SessionAttachStatus::Active).unwrap();
        assert_eq!(v, "active");
    }

    #[test]
    fn unknown_client_msg_type_fails_to_parse() {
        let raw = json!({"type": "totally-unknown", "data": "x"});
        let res: Result<ClientMsg, _> = serde_json::from_value(raw);
        assert!(res.is_err(), "unknown type tag must fail to deserialize");
    }

    #[test]
    fn malformed_resize_fails_to_parse() {
        // cols missing — shape must be enforced strictly so the handler
        // can produce a single canonical "invalid_message" response.
        let raw = json!({"type": "resize", "rows": 24});
        let res: Result<ClientMsg, _> = serde_json::from_value(raw);
        assert!(res.is_err());
    }

    #[test]
    fn input_debug_does_not_leak_payload() {
        // Uses a sentinel byte sequence the test asserts is NOT present
        // in the formatted Debug. If a future change ever stringifies
        // `data` this test fails loudly.
        let sentinel = "REDACT-MARKER-INPUT-7C42";
        let msg = ClientMsg::Input {
            data: sentinel.to_owned(),
        };
        let debug = format!("{msg:?}");
        assert!(
            !debug.contains(sentinel),
            "Debug output for ClientMsg::Input must NOT contain the payload, got: {debug}",
        );
        assert!(
            debug.contains("data_len"),
            "Debug should still surface the length so logs are useful: {debug}",
        );
    }

    #[test]
    fn input_round_trips_through_json() {
        // Even though Debug masks the payload, JSON serialization keeps it
        // — that's the contract; the protocol is responsible for delivery,
        // not redaction. The masking is purely a logging guard.
        let msg = ClientMsg::Input {
            data: "hello".to_owned(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMsg = serde_json::from_str(&json).unwrap();
        match back {
            ClientMsg::Input { data } => assert_eq!(data, "hello"),
            other => panic!("round-trip changed variant: {other:?}"),
        }
    }
}
