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
    // Liveness recovery: a reconnected bot is healthy again. Only unwind the
    // sweep's own `unreachable` flip — never an operator-set health value.
    if let Ok(Some(inv)) = state.store.bot_inventory(&bot_id) {
        if inv.health == "unreachable" {
            let patch = crate::store::BotMetadataPatch {
                health: Some("ok".into()),
                ..Default::default()
            };
            let _ = state.store.update_bot_metadata(&bot_id, &patch);
            state.emit_north(
                "bot_health",
                "-",
                serde_json::json!({ "bot": bot_id, "health": "ok" }),
            );
        }
    }
    // Replay anything queued while this bot was offline (durable outbox).
    state.flush_outbox(&bot_id);
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

    // Unconditional: send_task pumps THIS socket's sink and dies with it,
    // regardless of generation — a superseded old conn still aborts its own task.
    send_task.abort();
    // Only mark the bot offline if THIS connection is still the current one. On a
    // rolling reconnect the new conn (gen N+1) registers before this old one (gen N)
    // tears down; unregister_conn returns false for the superseded gen, so we must
    // not flip `connected` false and strand a bot that is actually live on the new tx.
    if state.unregister_conn(&bot_id, conn_gen) {
        let _ = state.store.set_connected(&bot_id, false);
        tracing::info!("bot {bot_id} disconnected");
    }
}
