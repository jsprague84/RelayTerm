//! Terminal-session lifecycle routes.
//!
//! These endpoints manage the *metadata* surface of a terminal session.
//! The orchestrator behind them (`relayterm_terminal::TerminalSessionManager`)
//! deliberately does NOT open SSH channels, allocate PTYs, or stream
//! terminal data in this slice — see the doc-comments on the manager
//! and on `STUB_PTY_NOT_IMPLEMENTED_MESSAGE` for the full contract.
//!
//! Ownership rules mirror the rest of the v1 API:
//! - The caller's user is taken from the `DevUser` extractor.
//! - `create` verifies the referenced server_profile, host, and identity
//!   all belong to the caller; foreign-owned references collapse to the
//!   same 404 the route would return for a missing resource.
//! - `get_by_id`, `close`, the `list` filter, and the WebSocket attach
//!   route all scope to the caller's user, so cross-user existence is
//!   never leaked by id.

use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use relayterm_core::ids::{TerminalSessionAttachmentId, TerminalSessionId, UserId};
use relayterm_core::repository::{
    HostRepository, KnownHostEntryRepository, ServerProfileRepository, SshIdentityRepository,
    TerminalSessionRepository,
};
use relayterm_core::terminal_session::TerminalSessionStatus;
use relayterm_protocol::{
    AckKind, ClientMsg, ErrorCode as ProtoErrorCode, ServerMsg, SessionAttachStatus,
};
use relayterm_terminal::{
    AttachSessionRequest as ManagerAttachRequest,
    CreateTerminalSessionRequest as ManagerCreateRequest, TerminalSessionManager,
    TerminalSessionManagerError,
};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::AppState;
use crate::dev_user::DevUser;
use crate::dto::terminal_session::{
    CloseTerminalSessionResponse, CreateTerminalSessionRequest, CreateTerminalSessionResponse,
    TerminalSessionResponse,
};
use crate::error::ApiError;

const ENTITY: &str = "terminal_session";

/// Cap on the length of the `User-Agent` value persisted as `client_info`
/// on the attachment row. The DB column is `TEXT` (unbounded) but we don't
/// want a malicious / accidental megabyte-long header to land in audit
/// payloads. 256 chars is enough for every legitimate UA string.
const MAX_CLIENT_INFO_LEN: usize = 256;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create).get(list))
        .route("/{id}", get(get_by_id))
        .route("/{id}/close", post(close))
        .route("/{id}/ws", get(ws_attach))
}

/// `POST /api/v1/terminal-sessions`.
///
/// Creates terminal-session metadata and an in-memory runtime placeholder.
/// PTY startup and SSH channel allocation are NOT implemented in this
/// slice — the response carries a static `message` that names the stub
/// scope explicitly so the client cannot mistake "row created" for
/// "shell ready."
async fn create(
    State(state): State<AppState>,
    user: DevUser,
    Json(req): Json<CreateTerminalSessionRequest>,
) -> Result<(StatusCode, Json<CreateTerminalSessionResponse>), ApiError> {
    // Resolve the (profile, host, identity) trio scoped to the caller.
    // Any miss — by id OR by ownership — collapses to a single 404 entity
    // ("terminal_session") so cross-user existence is never leaked.
    let profile = state
        .db
        .server_profiles()
        .get(req.server_profile_id)
        .await?
        .filter(|p| p.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let host = state
        .db
        .hosts()
        .get(profile.host_id)
        .await?
        .filter(|h| h.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let _identity = state
        .db
        .ssh_identities()
        .get(profile.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    // Precondition: host key MUST already be pinned and trusted (and not
    // revoked). We do NOT perform a live preflight here — that's the
    // caller's responsibility via `POST /trust-host-key`. Refusing to
    // create a session without a trusted pin keeps the future PTY-bearing
    // implementation from accidentally connecting to an unverified peer.
    let known = state.db.known_host_entries().list_for_host(host.id).await?;
    let any_trusted = known
        .iter()
        .any(|e| e.trusted_at.is_some() && e.revoked_at.is_none());
    if !any_trusted {
        return Err(ApiError::Conflict { entity: "host_key" });
    }

    let outcome = state
        .terminal_sessions
        .create_session(ManagerCreateRequest {
            owner_id: user.0,
            server_profile_id: profile.id,
            cols: req.cols,
            rows: req.rows,
        })
        .await?;

    let body = CreateTerminalSessionResponse {
        session: outcome.session.into(),
        message: outcome.message,
    };
    Ok((StatusCode::CREATED, Json(body)))
}

async fn list(
    State(state): State<AppState>,
    user: DevUser,
) -> Result<Json<Vec<TerminalSessionResponse>>, ApiError> {
    let sessions = state.db.terminal_sessions().list_for_user(user.0).await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(TerminalSessionResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<TerminalSessionResponse>, ApiError> {
    let session = state
        .db
        .terminal_sessions()
        .get(id)
        .await?
        .filter(|s| s.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    Ok(Json(session.into()))
}

/// `POST /api/v1/terminal-sessions/:id/close`.
///
/// Idempotent: closing an already-closed session returns 200 with
/// `already_closed = true`. The manager handles ownership filtering —
/// foreign-owned ids surface as the same 404 the route would emit for a
/// missing id.
async fn close(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<CloseTerminalSessionResponse>, ApiError> {
    let outcome = state.terminal_sessions.close_session(id, user.0).await?;
    Ok(Json(CloseTerminalSessionResponse {
        session: outcome.session.into(),
        already_closed: outcome.already_closed,
    }))
}

/// `GET /api/v1/terminal-sessions/:id/ws`.
///
/// Upgrades the connection to a WebSocket for the typed terminal protocol
/// defined in [`relayterm_protocol`]. Pre-upgrade ownership and lifecycle
/// validation runs here so the client gets an HTTP-level error (401/404/409)
/// when the request is malformed; only a fully-validated upgrade ever
/// reaches [`run_attached_socket`].
///
/// **Scope (this slice)**: the upgraded socket exists to exercise the
/// attach/detach lifecycle and the typed protocol. It does NOT open an
/// SSH channel, allocate a PTY, or forward terminal bytes. See the
/// per-message handler comments in [`run_attached_socket`] for the
/// exact behavior of each [`ClientMsg`] variant.
async fn ws_attach(
    State(state): State<AppState>,
    user: DevUser,
    Path(id): Path<TerminalSessionId>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Pre-upgrade ownership / lifecycle gate. We deliberately resolve the
    // session here (instead of inside `on_upgrade`) so a missing/foreign/
    // closed session produces an HTTP 404/409 response BEFORE the
    // WebSocket handshake completes — clients see a clean error envelope
    // rather than an opaque socket open-then-close.
    let session = state
        .db
        .terminal_sessions()
        .get(id)
        .await?
        .filter(|s| s.owner_id == user.0)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    if session.status == TerminalSessionStatus::Closed {
        return Err(ApiError::Conflict { entity: ENTITY });
    }

    // Capture audit-only client metadata at the boundary. Header lookup is
    // best-effort: a missing `User-Agent` is fine (None is recorded). The
    // value is length-capped before it lands in the DB to keep a hostile
    // header from inflating the audit payload.
    let client_info = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            if s.len() > MAX_CLIENT_INFO_LEN {
                s[..MAX_CLIENT_INFO_LEN].to_owned()
            } else {
                s.to_owned()
            }
        });

    let manager = state.terminal_sessions.clone();
    let user_id = user.0;
    let session_id = session.id;
    Ok(ws.on_upgrade(move |socket| async move {
        run_attached_socket(socket, manager, user_id, session_id, client_info).await;
    }))
}

/// State held inside the WebSocket task. Tracks just enough to make
/// detach/close idempotent without re-querying the manager.
struct SocketState {
    attachment_id: TerminalSessionAttachmentId,
    detached: bool,
    closed: bool,
}

/// Run the attach / per-message loop for one WebSocket connection.
///
/// Lifecycle:
/// 1. Call [`TerminalSessionManager::attach_session`] to write the
///    attachment row and register the in-memory runtime entry. Failure
///    here is rare (race with `close`) — emit an `Error` frame and exit.
/// 2. Send [`ServerMsg::SessionAttached`] with the static stub message.
/// 3. Read frames until the socket drops or the client sends `Close`.
/// 4. On exit, ensure the attachment is detached (idempotent) so the
///    audit row reflects reality even on abrupt drops.
async fn run_attached_socket(
    mut socket: WebSocket,
    manager: Arc<TerminalSessionManager>,
    user_id: UserId,
    session_id: TerminalSessionId,
    client_info: Option<String>,
) {
    let outcome = match manager
        .attach_session(ManagerAttachRequest {
            owner_id: user_id,
            session_id,
            client_info,
            // Real source-address capture needs ConnectInfo plumbing on
            // the listener; out of scope for this slice. Audit-only field;
            // None is the explicit "unknown" value.
            remote_addr: None,
        })
        .await
    {
        Ok(out) => out,
        Err(err) => {
            // Race between the pre-upgrade check and the attach call (the
            // session was closed in between, or a transient repo error).
            // Emit a typed error frame and close — the HTTP layer is gone
            // so we can't return a status code anymore.
            send_error(&mut socket, &err).await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };

    let attached = ServerMsg::SessionAttached {
        session_id: outcome.session.id,
        attachment_id: outcome.attachment.id,
        status: SessionAttachStatus::AttachedStub,
        message: outcome.message.to_owned(),
    };
    if !send_msg(&mut socket, &attached).await {
        // Failed to send the attach ack — the socket is already gone.
        // Still write the detach bookkeeping so the row reflects reality.
        let _ = manager
            .detach_session(user_id, session_id, outcome.attachment.id, None)
            .await;
        return;
    }

    let mut state = SocketState {
        attachment_id: outcome.attachment.id,
        detached: false,
        closed: false,
    };

    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                if !handle_text_frame(
                    &mut socket,
                    &manager,
                    user_id,
                    session_id,
                    &mut state,
                    &text,
                )
                .await
                {
                    break;
                }
            }
            Some(Ok(Message::Binary(_))) => {
                // Binary frames have no defined meaning in the JSON-only
                // protocol skeleton. Reject without echoing so a probing
                // client can't confuse the audit log with arbitrary bytes.
                let _ = send_msg(
                    &mut socket,
                    &ServerMsg::Error {
                        code: ProtoErrorCode::InvalidMessage,
                        message: "binary frames are not accepted".to_owned(),
                    },
                )
                .await;
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                // axum handles WebSocket-protocol pings transparently;
                // application-level liveness uses ClientMsg::Ping.
            }
            Some(Ok(Message::Close(_))) | None => break,
            Some(Err(err)) => {
                debug!(?err, "websocket transport error; closing");
                break;
            }
        }
    }

    // Cleanup: if the user closed the session through this socket the
    // attachment is already gone from the registry and the row's
    // `detached_at` will stay NULL forever (the close path subsumes
    // detach for audit purposes). Otherwise mark the attachment detached
    // — idempotent on already-detached rows.
    if !state.detached && !state.closed {
        if let Err(err) = manager
            .detach_session(user_id, session_id, state.attachment_id, None)
            .await
        {
            warn!(?err, "failed to mark attachment detached on socket exit");
        }
    }
}

/// Process one text frame. Returns `false` to break the receive loop
/// (after a successful `Close`, for example).
async fn handle_text_frame(
    socket: &mut WebSocket,
    manager: &Arc<TerminalSessionManager>,
    user_id: UserId,
    session_id: TerminalSessionId,
    state: &mut SocketState,
    text: &str,
) -> bool {
    let msg = match serde_json::from_str::<ClientMsg>(text) {
        Ok(msg) => msg,
        Err(_) => {
            // Do NOT include the offending frame in the response or any
            // log line — it may carry terminal input bytes the client
            // was about to send via `Input`. The static "invalid_message"
            // body is all the client gets.
            let _ = send_msg(
                socket,
                &ServerMsg::Error {
                    code: ProtoErrorCode::InvalidMessage,
                    message: "invalid message".to_owned(),
                },
            )
            .await;
            return true;
        }
    };

    match msg {
        ClientMsg::Ping => {
            send_msg(socket, &ServerMsg::Pong).await;
        }
        ClientMsg::Attach { .. } => {
            // The route already attached this socket on upgrade. A second
            // explicit Attach is meaningless in this slice; reject it
            // rather than re-attach (which would orphan the first row).
            send_msg(
                socket,
                &ServerMsg::Error {
                    code: ProtoErrorCode::InvalidMessage,
                    message: "already attached".to_owned(),
                },
            )
            .await;
        }
        ClientMsg::Input { data: _ } => {
            // No PTY exists; the bytes have nowhere to go. We must NOT
            // reflect the payload back, log it, or forward it anywhere —
            // the only side effect of this match arm is the static stub
            // error frame.
            send_msg(
                socket,
                &ServerMsg::Error {
                    code: ProtoErrorCode::PtyNotImplemented,
                    message: "PTY streaming is not implemented yet".to_owned(),
                },
            )
            .await;
        }
        ClientMsg::Resize { cols, rows } => {
            match manager
                .resize_session(user_id, session_id, cols, rows)
                .await
            {
                Ok(_) => {
                    send_msg(
                        socket,
                        &ServerMsg::Ack {
                            kind: AckKind::Resize,
                        },
                    )
                    .await;
                }
                Err(err) => {
                    send_error(socket, &err).await;
                }
            }
        }
        ClientMsg::Detach => {
            match manager
                .detach_session(user_id, session_id, state.attachment_id, None)
                .await
            {
                Ok(out) => {
                    state.detached = true;
                    send_msg(
                        socket,
                        &ServerMsg::SessionDetached {
                            session_id: out.session.id,
                            attachment_id: out.attachment.id,
                        },
                    )
                    .await;
                    let _ = socket.send(Message::Close(None)).await;
                    return false;
                }
                Err(err) => {
                    send_error(socket, &err).await;
                }
            }
        }
        ClientMsg::Close => {
            match manager.close_session(session_id, user_id).await {
                Ok(out) => {
                    // close drops live attachments from the registry,
                    // so flag both so the cleanup tail doesn't
                    // double-detach.
                    state.detached = true;
                    state.closed = true;
                    send_msg(
                        socket,
                        &ServerMsg::SessionClosed {
                            session_id: out.session.id,
                        },
                    )
                    .await;
                    let _ = socket.send(Message::Close(None)).await;
                    return false;
                }
                Err(err) => {
                    send_error(socket, &err).await;
                }
            }
        }
    }
    true
}

/// Map a manager error to a wire-stable [`ServerMsg::Error`] frame and
/// send it. Internal repository details never reach the client; only the
/// classified `ErrorCode` plus a short static message is emitted.
///
/// Note: `NotFound` and `SessionClosed` are already screened by the
/// pre-upgrade gate in [`ws_attach`], so reaching them inside an open
/// socket means a race fired between the pre-check and the manager
/// call (e.g. another caller closed the session in between). Both
/// surface as `internal` rather than `invalid_message` because the
/// client sent nothing wrong — the session disappeared underneath them.
/// Operator detail still goes to the log.
async fn send_error(socket: &mut WebSocket, err: &TerminalSessionManagerError) {
    let (code, message) = match err {
        TerminalSessionManagerError::InvalidDimensions { .. } => {
            (ProtoErrorCode::InvalidInput, "invalid terminal dimensions")
        }
        TerminalSessionManagerError::NotFound => {
            warn!(?err, "post-upgrade NotFound race in WebSocket handler");
            (ProtoErrorCode::Internal, "internal error")
        }
        TerminalSessionManagerError::SessionClosed => {
            warn!(?err, "post-upgrade SessionClosed race in WebSocket handler");
            (ProtoErrorCode::Internal, "internal error")
        }
        TerminalSessionManagerError::Repository(_) => {
            // Operator detail goes to the log via the Debug impl; the
            // wire body stays generic so SQL fragments or constraint
            // names never leak.
            warn!(?err, "repository error in WebSocket handler");
            (ProtoErrorCode::Internal, "internal error")
        }
    };
    send_msg(
        socket,
        &ServerMsg::Error {
            code,
            message: message.to_owned(),
        },
    )
    .await;
}

/// Serialize and send a [`ServerMsg`]. Returns `false` if the send failed
/// (transport already gone) so the caller can short-circuit.
async fn send_msg(socket: &mut WebSocket, msg: &ServerMsg) -> bool {
    let payload = match serde_json::to_string(msg) {
        Ok(p) => p,
        Err(err) => {
            warn!(?err, "failed to serialize server message");
            return false;
        }
    };
    socket.send(Message::Text(payload.into())).await.is_ok()
}
