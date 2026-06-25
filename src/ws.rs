//! South interface: the gateway `/ws` server (design §11). A stock OpenAB bot's
//! `[gateway]` adapter dials in here; we push GatewayEvent and consume
//! GatewayReply. Connection identity = per-bot token (§10).

use crate::identity;
use crate::orchestrator;
use crate::protocol::GatewayReply;
use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = q.get("token").cloned().unwrap_or_default();
    match identity::verify(state.store.as_ref(), &token) {
        Ok(bot) => ws.on_upgrade(move |socket| handle_conn(state, socket, bot.id)),
        Err(_) => axum::http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_conn(state: Arc<AppState>, socket: WebSocket, bot_id: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    let conn_gen = state.register_conn(&bot_id, tx);
    let _ = state.store.set_connected(&bot_id, true);
    tracing::info!("bot {bot_id} connected");

    // outbound pump: plane → bot
    let send_task = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });

    // inbound: bot → plane
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Text(text) = msg {
            match serde_json::from_str::<GatewayReply>(&text) {
                Ok(reply) => {
                    if let Err(e) = orchestrator::handle_reply(&state, &bot_id, reply) {
                        tracing::error!("handle_reply error: {e}");
                    }
                }
                Err(e) => tracing::warn!("bad reply json from {bot_id}: {e}"),
            }
        } else if matches!(msg, Message::Close(_)) {
            break;
        }
    }

    state.unregister_conn(&bot_id, conn_gen);
    let _ = state.store.set_connected(&bot_id, false);
    send_task.abort();
    tracing::info!("bot {bot_id} disconnected");
}
