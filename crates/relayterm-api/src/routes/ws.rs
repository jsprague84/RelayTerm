//! WebSocket placeholder for the terminal stream.
//!
//! Accepts the upgrade and echoes a single `Error` message telling the client
//! the SSH path isn't wired yet, then closes. Real session orchestration will
//! replace this body.

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use relayterm_protocol::ServerMsg;
use tracing::warn;

use crate::AppState;

async fn upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    let payload = serde_json::to_string(&ServerMsg::Error {
        message: "ssh session backend not yet implemented".to_owned(),
    })
    .expect("static payload always serialises");

    if let Err(err) = socket.send(Message::Text(payload.into())).await {
        warn!(?err, "failed to send placeholder error frame");
    }

    let _ = socket.send(Message::Close(None)).await;
}

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/ws/terminal", get(upgrade))
}
