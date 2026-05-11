//! Terminal-session lifecycle routes.
//!
//! These endpoints manage the *metadata* surface of a terminal session.
//! The orchestrator behind them (`relayterm_terminal::TerminalSessionManager`)
//! deliberately does NOT open SSH channels, allocate PTYs, or stream
//! terminal data in this slice — see the doc-comments on the manager
//! and on `STUB_PTY_NOT_IMPLEMENTED_MESSAGE` for the full contract.
//!
//! Ownership rules mirror the rest of the v1 API:
//! - The caller's user is taken from the cookie-backed
//!   `AuthenticatedUser` extractor.
//! - `create` verifies the referenced server_profile, host, and identity
//!   all belong to the caller; foreign-owned references collapse to the
//!   same 404 the route would return for a missing resource.
//! - `get_by_id`, `close`, the `list` filter, and the WebSocket attach
//!   route all scope to the caller's user, so cross-user existence is
//!   never leaked by id.
//!
//! ## CSRF
//!
//! State-changing browser-write routes (`create`, `close`) run the
//! shared [`CsrfGuard`] extractor before any DB / auth / body work — a
//! bad or missing `Origin` header is rejected with 403 before the
//! request body is parsed. The WebSocket attach route is `GET` and
//! therefore exempt; its auth check is the cookie-backed
//! [`AuthenticatedUser`] extractor that runs before the upgrade
//! handshake completes (so missing/invalid cookies surface as a clean
//! HTTP 401, never an opened-then-closed socket).

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
use relayterm_core::SeqNo;
use relayterm_core::ids::{TerminalSessionAttachmentId, TerminalSessionId, UserId};
use relayterm_core::repository::{
    HostRepository, KnownHostEntryRepository, ServerProfileRepository, SshIdentityRepository,
    TerminalSessionRepository,
};
use relayterm_core::terminal_session::TerminalSessionStatus;
use relayterm_protocol::{
    AckKind, BinaryFrameKind, ClientMsg, ErrorCode as ProtoErrorCode, ServerMsg,
    SessionAttachStatus, decode_binary_frame, encode_binary_frame,
};
use relayterm_ssh::{SshPtyConfig, SshPtyError, SshPtyTarget};
use relayterm_terminal::{
    AttachSessionRequest as ManagerAttachRequest,
    CreateTerminalSessionRequest as ManagerCreateRequest, LIVE_PTY_CREATE_MESSAGE, OutputFrame,
    ReplayRange, ReplayWindowLost, TerminalSessionManager, TerminalSessionManagerError,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::AppState;
use crate::auth::{AuthenticatedUser, CsrfGuard};
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
/// Creates terminal-session metadata AND starts a live SSH PTY backed
/// by the configured `(profile, host, identity)` trio. The flow:
/// 1. Resolve the trio scoped to the caller (foreign-owned ids collapse
///    to 404).
/// 2. Verify the host has at least one active, trusted, non-revoked
///    `known_host_entries` row (else 409 `host_key`).
/// 3. Decrypt the SSH identity inside the vault (5xx on data-integrity
///    failure; 503 if the vault is disabled).
/// 4. Write the metadata row in `Starting` status.
/// 5. Hand the decrypted PEM + accept-pin set to the SSH PTY bridge.
/// 6. On bridge success, transition the row to `Active` and bind the
///    live runtime to the manager.
/// 7. On bridge failure, transition the row to `Closed` with a
///    `closed { reason: ssh_start_failed, category }` event and
///    return the appropriate typed error.
///
/// Decrypted private-key bytes live ONLY in the `SshPtyTarget` for the
/// duration of the start call; the `Zeroizing` buffer wipes them on
/// drop.
async fn create(
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(req): Json<CreateTerminalSessionRequest>,
) -> Result<(StatusCode, Json<CreateTerminalSessionResponse>), ApiError> {
    let user_id = user.user_id();
    // Resolve the (profile, host, identity) trio scoped to the caller.
    // Any miss — by id OR by ownership — collapses to a single 404 entity
    // ("terminal_session") so cross-user existence is never leaked.
    let profile = state
        .db
        .server_profiles()
        .get(req.server_profile_id)
        .await?
        .filter(|p| p.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    // Disabled profiles refuse new launches. Existing live sessions
    // continue running — disable is a launch-time gate, not a runtime
    // kill switch. The wire shape (`409 conflict { entity:
    // "server_profile", reason: "disabled" }`) is pinned in SPEC.md.
    if profile.is_disabled() {
        return Err(ApiError::Conflict {
            entity: "server_profile",
            reason: Some("disabled"),
        });
    }
    let host = state
        .db
        .hosts()
        .get(profile.host_id)
        .await?
        .filter(|h| h.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    let identity = state
        .db
        .ssh_identities()
        .get(profile.ssh_identity_id)
        .await?
        .filter(|i| i.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;

    // Precondition: host key MUST already be pinned and trusted (and not
    // revoked). We do NOT perform a live preflight here — that's the
    // caller's responsibility via `POST /trust-host-key`. The accept-pin
    // set is passed straight to the bridge so the host-key check happens
    // BEFORE any client signature reaches the wire.
    let known = state.db.known_host_entries().list_for_host(host.id).await?;
    let accept_pins: Vec<_> = known
        .iter()
        .filter(|e| e.trusted_at.is_some() && e.revoked_at.is_none())
        .map(|e| (e.key_type, e.fingerprint_sha256.clone()))
        .collect();
    if accept_pins.is_empty() {
        return Err(ApiError::Conflict {
            entity: "host_key",
            reason: None,
        });
    }

    // Phase 1B.1 per-user live-PTY ceiling (`docs/session-quotas.md`
    // § 4.1). Sits AFTER ownership + host-key gating so a refusal
    // cannot be used to probe for foreign / disabled / untrusted
    // profiles, and BEFORE vault decrypt + SSH side effects so a
    // refused request does no outbound work, no decryption cycle, and
    // no target-host probe. The orchestrator's in-memory registry is
    // the authoritative tracker — counting from DB would race the
    // registry and let a user create more PTYs than this process can
    // actually hold (§ 4.1 rationale point 1).
    let cap = state.terminal_sessions.max_live_pty_per_user().get() as usize;
    let current = state.terminal_sessions.count_live_pty_for_user(user_id);
    if current >= cap {
        // Operator-side log line: public metadata only. Per
        // `docs/session-quotas.md` § 8.3 — `user_id` is public-shape
        // (already in many existing log lines via `AuthenticatedUser`
        // extraction); `current_count` and `cap` describe deployment
        // state, not user content. NEVER log session ids, profile
        // ids, host ids, identity ids, peer banners, or wire bodies.
        // No `audit_events` row — quota refusals are operational, not
        // security-relevant (§ 8.2).
        warn!(
            user_id = %user_id,
            scope = "per_user_live",
            current_count = current,
            cap = cap,
            "terminal session quota refused"
        );
        return Err(ApiError::TooManySessions);
    }

    // Decrypt the identity. Vault disabled → 503 (matches the rest of
    // the SSH-side routes). The decrypted PEM is held in a Zeroizing
    // buffer for the rest of this function.
    let vault = state.vault.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable("vault disabled — pty start not available".to_owned())
    })?;
    let private_key_pem = vault.decrypt_private_key(&identity.encrypted_private_key)?;

    // Write the metadata row + Created event + register the runtime
    // placeholder. After this the manager owns the row's runtime; we
    // call `start_live_pty` to promote it once the bridge succeeds.
    let create_outcome = state
        .terminal_sessions
        .create_session(ManagerCreateRequest {
            owner_id: user_id,
            server_profile_id: profile.id,
            cols: req.cols,
            rows: req.rows,
        })
        .await?;
    let session_id = create_outcome.session.id;

    // Build the bridge target. `username_override` (if any) supersedes
    // the host's `default_username`; that's the existing precedence the
    // auth-check route uses.
    let username = profile
        .username_override
        .as_ref()
        .map(|u| u.as_str().to_owned())
        .unwrap_or_else(|| host.default_username.as_str().to_owned());
    let pty_config = SshPtyConfig::new(
        host.hostname.as_str().to_owned(),
        host.port.get(),
        username,
        accept_pins,
        create_outcome.session.cols,
        create_outcome.session.rows,
    );
    let target = SshPtyTarget {
        config: pty_config,
        private_key_pem,
    };

    let started = match state.pty_bridge.start(target).await {
        Ok(s) => s,
        Err(err) => {
            // Map the bridge error to a typed API status BEFORE we touch
            // the DB so the operator-facing detail is logged once with
            // a precise classifier.
            let (api_err, category) = map_pty_start_error(&err);
            let _ = state
                .terminal_sessions
                .record_pty_start_failed(user_id, session_id, category)
                .await;
            return Err(api_err);
        }
    };

    // Bridge succeeded — promote the session to live.
    let session = state
        .terminal_sessions
        .start_live_pty(user_id, session_id, started)
        .await?;

    let body = CreateTerminalSessionResponse {
        session: session.into(),
        message: LIVE_PTY_CREATE_MESSAGE,
        pty_live: true,
    };
    let _ = create_outcome; // create_outcome.message ("...not implemented yet") is intentionally not surfaced once the PTY succeeded
    Ok((StatusCode::CREATED, Json(body)))
}

/// Map a bridge error to a wire-stable (ApiError, category) pair. The
/// category is recorded on the `closed` lifecycle event the manager
/// writes when startup fails; the ApiError carries operator detail
/// for tracing only.
fn map_pty_start_error(err: &SshPtyError) -> (ApiError, &'static str) {
    match err {
        SshPtyError::InvalidIdentity => (
            ApiError::Internal("ssh identity material is malformed".to_owned()),
            "invalid_identity",
        ),
        SshPtyError::Transport(_) => (
            ApiError::BadGateway("ssh transport failure during pty start".to_owned()),
            "transport",
        ),
        SshPtyError::HostKeyNotTrusted => (
            ApiError::Conflict {
                entity: "host_key",
                reason: None,
            },
            "host_key_not_trusted",
        ),
        SshPtyError::AuthenticationFailed => (
            ApiError::Conflict {
                entity: "ssh_auth",
                reason: None,
            },
            "authentication_failed",
        ),
        SshPtyError::PtyStartFailed => (
            ApiError::BadGateway("ssh pty/shell start failed".to_owned()),
            "pty_alloc",
        ),
        SshPtyError::BridgeClosed => (
            ApiError::Internal("bridge closed before start completed".to_owned()),
            "bridge_closed",
        ),
    }
}

async fn list(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<TerminalSessionResponse>>, ApiError> {
    let sessions = state
        .db
        .terminal_sessions()
        .list_for_user(user.user_id())
        .await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(TerminalSessionResponse::from)
            .collect(),
    ))
}

async fn get_by_id(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<TerminalSessionResponse>, ApiError> {
    let session = state
        .db
        .terminal_sessions()
        .get(id)
        .await?
        .filter(|s| s.owner_id == user.user_id())
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
    _csrf: CsrfGuard,
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<TerminalSessionId>,
) -> Result<Json<CloseTerminalSessionResponse>, ApiError> {
    let outcome = state
        .terminal_sessions
        .close_session(id, user.user_id())
        .await?;
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
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(id): Path<TerminalSessionId>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let user_id = user.user_id();
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
        .filter(|s| s.owner_id == user_id)
        .ok_or(ApiError::NotFound { entity: ENTITY })?;
    if session.status == TerminalSessionStatus::Closed {
        return Err(ApiError::Conflict {
            entity: ENTITY,
            reason: None,
        });
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
    /// `Some` once the attach handshake bound a live PTY to this socket;
    /// the broadcast subscription is owned here for the duration.
    live: Option<LiveSubscription>,
    /// `true` after the first client `Attach` frame has been observed
    /// (or the implicit no-replay path was taken). The protocol allows
    /// at most one client-initiated `Attach` per socket: the first
    /// carries the optional `last_seen_seq` that drives replay; any
    /// subsequent `Attach` frame is a protocol violation. `false`
    /// initially so we know to honour the first one.
    replay_handshake_done: bool,
    /// Minimum live `seq` the broadcast subscription is allowed to
    /// emit. The replay path snapshots the buffer and emits frames
    /// 1..=N synchronously, but the broadcast subscriber has been
    /// queuing the SAME N frames in parallel since attach. Without a
    /// floor, the renderer would see every replayed frame twice. The
    /// floor is set to `range.latest_seq` after a successful replay;
    /// the broadcast handler drops any incoming frame whose `seq <=
    /// floor` before sending it on the wire.
    min_live_seq: u64,
}

struct LiveSubscription {
    /// Subscribe handle for the per-session output broadcast. Takes a
    /// lagging-on-overflow stance via `broadcast::Receiver`.
    rx: broadcast::Receiver<OutputFrame>,
}

/// Run the attach / per-message loop for one WebSocket connection.
///
/// Lifecycle:
/// 1. Call [`TerminalSessionManager::attach_session`] to write the
///    attachment row, register the in-memory runtime entry, and (when
///    a live PTY is bound) hand back the [`LiveRuntimeView`].
/// 2. Send [`ServerMsg::SessionAttached`] with `Active` (live PTY) or
///    `AttachedStub` (placeholder).
/// 3. Multiplex client frames AND broadcast `Output` frames until the
///    socket drops, the client sends `Close`/`Detach`, or the live PTY
///    tears down.
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

    let live_subscription = outcome.live.as_ref().map(|view| LiveSubscription {
        rx: view.output_tx.subscribe(),
    });
    let attach_status = if outcome.live.is_some() {
        SessionAttachStatus::Active
    } else {
        SessionAttachStatus::AttachedStub
    };

    let attached = ServerMsg::SessionAttached {
        session_id: outcome.session.id,
        attachment_id: outcome.attachment.id,
        status: attach_status,
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
        live: live_subscription,
        replay_handshake_done: false,
        min_live_seq: 0,
    };

    loop {
        // Multiplex: socket recv vs broadcast recv. If the live PTY is
        // not bound, the broadcast branch is replaced with a never-
        // resolving future so the loop only fires on socket frames.
        let recv_socket = socket.recv();
        if let Some(sub) = state.live.as_mut() {
            tokio::select! {
                biased;
                client_frame = recv_socket => {
                    if !handle_recv_outcome(
                        &mut socket,
                        &manager,
                        user_id,
                        session_id,
                        &mut state,
                        client_frame,
                    )
                    .await
                    {
                        break;
                    }
                }
                pty_frame = sub.rx.recv() => {
                    match pty_frame {
                        Ok(frame) => {
                            // Drop frames the replay handshake already
                            // sent. The replay snapshot and the live
                            // broadcast subscription both observe the
                            // SAME backlog after an `Attach` with
                            // `last_seen_seq`, so without this floor the
                            // renderer would see the older frames twice.
                            if frame.seq <= state.min_live_seq {
                                continue;
                            }
                            // Output bytes from the remote PTY. Emitted
                            // as a binary `Output` frame on the data
                            // plane — JSON/base64 inflation belongs to
                            // the legacy fallback and is not used here.
                            // NEVER log the raw payload at any level.
                            if !send_binary_output(&mut socket, frame.seq, &frame.data).await {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Slow consumer: the broadcast's bounded
                            // queue overflowed and dropped some frames.
                            // The renderer will see a `seq` gap in
                            // the live stream; on the next reconnect
                            // it can request replay by passing
                            // `last_seen_seq`, and the bounded replay
                            // buffer will fill the gap when possible
                            // (or surface `replay_window_lost`).
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // The PTY tore down. The manager's forwarder
                            // marks the row closed; we just exit.
                            break;
                        }
                    }
                }
            }
        } else {
            let client_frame = recv_socket.await;
            if !handle_recv_outcome(
                &mut socket,
                &manager,
                user_id,
                session_id,
                &mut state,
                client_frame,
            )
            .await
            {
                break;
            }
        }
    }

    // Cleanup: if the user closed the session through this socket the
    // attachment is already gone from the registry. Otherwise mark the
    // attachment detached. The manager's `detach_attachment` helper
    // is the single lifecycle entry point — when this is the last
    // attachment of a live PTY it schedules a bounded TTL close (see
    // SPEC.md "Detached-session TTL contract"). The PTY survives until
    // a reattach cancels the timer, the TTL expires, or an explicit
    // close arrives. Idempotent in two ways: (a) `detach_session` is
    // COALESCE-on-detached_at so a race with the explicit Detach frame
    // can't write a second `Detached` event; (b) the helper skips the
    // schedule when the detach observed the row as already detached,
    // so a second cleanup-tail pass cannot install a duplicate timer
    // or churn the row state.
    if !state.detached && !state.closed {
        if let Err(err) = manager
            .detach_attachment(user_id, session_id, state.attachment_id, None)
            .await
        {
            warn!(?err, "failed to mark attachment detached on socket exit");
        }
    }
}

/// Handle one outcome of `socket.recv()`. Returns `false` to break the
/// outer loop (e.g. transport closed, client said `Close`/`Detach`).
async fn handle_recv_outcome(
    socket: &mut WebSocket,
    manager: &Arc<TerminalSessionManager>,
    user_id: UserId,
    session_id: TerminalSessionId,
    state: &mut SocketState,
    frame: Option<Result<Message, axum::Error>>,
) -> bool {
    match frame {
        Some(Ok(Message::Text(text))) => {
            handle_text_frame(socket, manager, user_id, session_id, state, &text).await
        }
        Some(Ok(Message::Binary(bytes))) => {
            handle_binary_frame(socket, manager, user_id, session_id, state, &bytes).await
        }
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
            // axum handles WebSocket-protocol pings transparently;
            // application-level liveness uses ClientMsg::Ping.
            true
        }
        Some(Ok(Message::Close(_))) | None => false,
        Some(Err(err)) => {
            debug!(?err, "websocket transport error; closing");
            false
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
        ClientMsg::Attach {
            session_id: _,
            last_seen_seq,
            client_id: _,
        } => {
            // The route already wrote the attachment row on upgrade and
            // sent `SessionAttached`. The client's optional follow-up
            // `Attach` frame carries `last_seen_seq` for the replay
            // handshake. Allow at most one such frame per socket — a
            // second `Attach` is a protocol violation (it cannot trigger
            // a re-attach without orphaning the first row).
            if state.replay_handshake_done {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        code: ProtoErrorCode::InvalidMessage,
                        message: "already attached".to_owned(),
                    },
                )
                .await;
                return true;
            }
            state.replay_handshake_done = true;
            // No bookmark → no replay request, just continue live attach.
            // Do NOT replay the entire buffer to a brand-new attach —
            // that would dump pre-attach scrollback to a renderer that
            // didn't ask for it. The product contract says replay is
            // strictly opt-in via `last_seen_seq`.
            //
            // `Some(0)` is treated as `None` here: seq numbers start at
            // 1, so a bookmark of 0 carries no resume information beyond
            // "no frames seen yet." Collapsing the two shapes at the
            // wire boundary keeps `min_live_seq` at its 0 default, which
            // is load-bearing for the no-bookmark path — every frame
            // the broadcast subscriber queued since upgrade passes the
            // floor check and reaches the renderer.
            let bookmark = last_seen_seq.map(|s| s.0).filter(|seq| *seq > 0);
            if bookmark.is_none() {
                return true;
            }
            // Replay only makes sense when a live PTY runtime exists for
            // the session. Stub sessions have no buffer; treat as a
            // no-op (the client's bookmark refers to a vanished PTY).
            let Some(replay) = manager.replay_since(session_id, bookmark) else {
                return true;
            };
            match replay {
                Ok(range) => {
                    // Raise the live-seq floor BEFORE emitting so any
                    // broadcast frames the subscriber has already
                    // queued won't double-deliver — drop them in the
                    // pty_frame branch.
                    state.min_live_seq = state.min_live_seq.max(range.latest_seq);
                    emit_replay_range(socket, range).await;
                }
                Err(lost) => {
                    // Same floor: skip ahead so the live stream picks
                    // up at the next stamped frame, not at one the
                    // renderer was told it missed.
                    state.min_live_seq = state.min_live_seq.max(lost.latest);
                    emit_replay_window_lost(socket, lost).await;
                }
            }
        }
        ClientMsg::Input { data } => {
            // Forward to the live PTY if bound. The payload is NEVER
            // logged or echoed — the manager's `write_pty_input`
            // takes ownership and hands raw bytes to the SSH layer,
            // which streams them straight to the remote shell.
            if state.live.is_none() {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        code: ProtoErrorCode::PtyNotLive,
                        message: "no live pty for this session".to_owned(),
                    },
                )
                .await;
            } else {
                // Backwards-compat fallback for clients that still wrap
                // keystrokes in JSON `input { data: string }`. Control
                // sequences, paste, and Unicode all round-trip via
                // UTF-8. The default wire shape for input is the binary
                // `Input` frame (RTB1) routed by `handle_binary_frame`.
                let bytes = data.into_bytes();
                if let Err(err) = manager.write_pty_input(user_id, session_id, bytes).await {
                    send_error(socket, &err).await;
                }
                // Success path: NO ack — input is fire-and-forget;
                // the renderer sees the echo as Output bytes. An ack
                // here would inflate per-keystroke wire traffic.
            }
        }
        ClientMsg::Resize { cols, rows } => {
            match manager
                .resize_session(user_id, session_id, cols, rows)
                .await
            {
                Ok(_) => {
                    // The metadata-only resize landed; now tell the live
                    // PTY. When the PTY is NOT live (post-restart stub
                    // row, or a session whose runtime tore down without
                    // being closed yet), surface `pty_not_live` so the
                    // renderer doesn't believe the SSH side tracked the
                    // resize. Per SPEC: "input/resize attempted on a
                    // session whose live runtime is not present" → the
                    // typed `pty_not_live` error.
                    match manager
                        .apply_pty_resize(user_id, session_id, cols, rows)
                        .await
                    {
                        Ok(true) => {
                            send_msg(
                                socket,
                                &ServerMsg::Ack {
                                    kind: AckKind::Resize,
                                },
                            )
                            .await;
                        }
                        Ok(false) => {
                            send_msg(
                                socket,
                                &ServerMsg::Error {
                                    code: ProtoErrorCode::PtyNotLive,
                                    message: "no live pty for this session".to_owned(),
                                },
                            )
                            .await;
                        }
                        Err(err) => {
                            send_error(socket, &err).await;
                        }
                    }
                }
                Err(err) => {
                    send_error(socket, &err).await;
                }
            }
        }
        ClientMsg::Detach => {
            // The manager's `detach_attachment` helper detaches the
            // attachment row AND, if this is the last attachment of a
            // live PTY, schedules a TTL close so the PTY survives a
            // brief reconnect window — see SPEC.md "Detached-session
            // TTL contract." Reconnect within `DETACHED_LIVE_PTY_TTL`
            // cancels the close; outside it the session is reaped and
            // a fresh attach surfaces 409 from the upgrade gate.
            match manager
                .detach_attachment(user_id, session_id, state.attachment_id, None)
                .await
            {
                Ok(out) => {
                    state.detached = true;
                    send_msg(
                        socket,
                        &ServerMsg::SessionDetached {
                            session_id: out.detach.session.id,
                            attachment_id: out.detach.attachment.id,
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
        TerminalSessionManagerError::PtyNotLive => {
            (ProtoErrorCode::PtyNotLive, "no live pty for this session")
        }
        TerminalSessionManagerError::PtyStart(inner) => {
            warn!(?inner, "pty bridge error during in-flight session");
            (ProtoErrorCode::SshStartFailed, "ssh pty error")
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

/// Drain a `ReplayRange` to the socket as `ReplayStart` → buffered
/// `Output` frames → `ReplayEnd`. Empty ranges still emit nothing — a
/// client that asked to resume from `latest_seq` itself doesn't need
/// the bracketing frames. The `seq` on each replayed `Output` is the
/// original sequence the orchestrator stamped (NOT renumbered) so a
/// client that bridges replay → live frames sees one continuous stream.
async fn emit_replay_range(socket: &mut WebSocket, range: ReplayRange) {
    if range.frames.is_empty() {
        return;
    }
    let from_seq = SeqNo(range.frames.first().expect("non-empty checked above").seq);
    let to_seq = SeqNo(range.frames.last().expect("non-empty checked above").seq);
    if !send_msg(socket, &ServerMsg::ReplayStart { from_seq, to_seq }).await {
        return;
    }
    for frame in &range.frames {
        // Replayed output uses the SAME binary envelope as live output;
        // the orchestrator-stamped seq is preserved so a renderer that
        // bridges replay → live frames sees one continuous stream.
        if !send_binary_output(socket, frame.seq, &frame.data).await {
            return;
        }
    }
    let _ = send_msg(
        socket,
        &ServerMsg::ReplayEnd {
            latest_seq: SeqNo(range.latest_seq),
        },
    )
    .await;
}

/// Surface a window-lost replay as the typed wire frame. The session is
/// NOT closed — the handler continues live attach so the renderer can
/// reset its grid and resume from the next live frame.
async fn emit_replay_window_lost(socket: &mut WebSocket, lost: ReplayWindowLost) {
    let _ = send_msg(
        socket,
        &ServerMsg::ReplayWindowLost {
            requested_seq: SeqNo(lost.requested),
            oldest_available_seq: lost.oldest_available.map(SeqNo),
            latest_seq: SeqNo(lost.latest),
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

/// Emit a PTY output frame on the binary data plane. Returns `false` if
/// the send failed (transport already gone) so the caller can short-
/// circuit. The encoder caps payload at the protocol limit; an oversize
/// PTY chunk is logged and dropped on the floor — losing a frame is
/// preferable to bringing down the socket, and the bounded replay
/// buffer covers the gap on the next reconnect.
async fn send_binary_output(socket: &mut WebSocket, seq: u64, data: &[u8]) -> bool {
    let encoded = match encode_binary_frame(BinaryFrameKind::Output, seq, data) {
        Ok(buf) => buf,
        Err(err) => {
            // We never log the payload itself — only seq + classifier.
            warn!(?err, seq, len = data.len(), "binary output encode failed");
            return true;
        }
    };
    socket.send(Message::Binary(encoded.into())).await.is_ok()
}

/// Decode an inbound binary frame and route it to the right handler.
/// Currently the only valid client-bound binary kind is
/// [`BinaryFrameKind::Input`]; an Output frame on the receive side is a
/// protocol violation. Returns `false` to break the receive loop.
async fn handle_binary_frame(
    socket: &mut WebSocket,
    manager: &Arc<TerminalSessionManager>,
    user_id: UserId,
    session_id: TerminalSessionId,
    state: &mut SocketState,
    bytes: &[u8],
) -> bool {
    let frame = match decode_binary_frame(bytes) {
        Ok(frame) => frame,
        Err(err) => {
            // Static classifier only — we never reflect bytes from a
            // malformed frame in case it carried terminal input.
            debug!(?err, "binary frame decode failed");
            let _ = send_msg(
                socket,
                &ServerMsg::Error {
                    code: ProtoErrorCode::InvalidMessage,
                    message: "invalid binary frame".to_owned(),
                },
            )
            .await;
            return true;
        }
    };
    match frame.kind {
        BinaryFrameKind::Input => {
            if state.live.is_none() {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        code: ProtoErrorCode::PtyNotLive,
                        message: "no live pty for this session".to_owned(),
                    },
                )
                .await;
            } else if let Err(err) = manager
                .write_pty_input(user_id, session_id, frame.payload)
                .await
            {
                send_error(socket, &err).await;
            }
        }
        BinaryFrameKind::Output => {
            // Output is server → client only. A client sending Output
            // is malformed; reject without echoing payload.
            let _ = send_msg(
                socket,
                &ServerMsg::Error {
                    code: ProtoErrorCode::InvalidMessage,
                    message: "client must not send output frames".to_owned(),
                },
            )
            .await;
        }
    }
    true
}
