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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = q.get("token").cloned().unwrap_or_default();
    match identity::verify(state.store.as_ref(), &token) {
        Ok(bot) => ws.on_upgrade(move |socket| handle_conn(state, socket, bot.id)),
        Err(_) => {
            // A bot dialing with an unknown/stale token (e.g. re-minted after a DB
            // reset) loops here → stuck offline. Log it so that failure mode is
            // distinguishable from the double-connect race. Never log the token.
            tracing::warn!("ws upgrade rejected: invalid bot token");
            axum::http::StatusCode::UNAUTHORIZED.into_response()
        }
    }
}

async fn handle_conn(state: Arc<AppState>, socket: WebSocket, bot_id: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let last_activity = Arc::new(AtomicI64::new(crate::store::now_ms()));

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
    tracing::info!("bot {bot_id} connected (gen {conn_gen})");

    // outbound pump: plane → bot
    let (disconnect_tx, mut disconnect_rx) = mpsc::unbounded_channel::<()>();
    let ping_secs = state.ws_ping_secs;
    let ping_activity = last_activity.clone();
    let ping_bot_id = bot_id.clone();
    let send_task = tokio::spawn(async move {
        if ping_secs == 0 {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text)).await.is_err() {
                    let _ = disconnect_tx.send(());
                    break;
                }
            }
            return;
        }

        let mut ping = tokio::time::interval(Duration::from_secs(ping_secs));
        let deadline_ms = (ping_secs as i64) * 3 * 1000;
        loop {
            tokio::select! {
                text = rx.recv() => {
                    let Some(text) = text else {
                        break;
                    };
                    if sink.send(Message::Text(text)).await.is_err() {
                        let _ = disconnect_tx.send(());
                        break;
                    }
                }
                _ = ping.tick() => {
                    let idle_ms = crate::store::now_ms() - ping_activity.load(Ordering::Relaxed);
                    if idle_ms > deadline_ms {
                        tracing::warn!("bot {ping_bot_id} missed websocket pong deadline ({idle_ms}ms idle)");
                        let _ = sink.send(Message::Close(None)).await;
                        let _ = disconnect_tx.send(());
                        break;
                    }
                    tracing::trace!("ping bot {ping_bot_id}");
                    if sink.send(Message::Ping(Vec::new())).await.is_err() {
                        let _ = disconnect_tx.send(());
                        break;
                    }
                }
            }
        }
    });

    // inbound: bot → plane
    loop {
        tokio::select! {
            _ = disconnect_rx.recv() => break,
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else {
                    break;
                };
                last_activity.store(crate::store::now_ms(), Ordering::Relaxed);
                match msg {
                    Message::Text(text) => {
                        match serde_json::from_str::<GatewayReply>(&text) {
                            Ok(reply) => {
                                if let Err(e) = orchestrator::handle_reply(&state, &bot_id, reply) {
                                    tracing::error!("handle_reply error: {e}");
                                }
                            }
                            Err(e) => tracing::warn!("bad reply json from {bot_id}: {e}"),
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    // Order matters (council #100 F1): unregister FIRST so send_to_bot/flush_outbox
    // stop routing to this dead gen, THEN abort our own pump. Aborting first would
    // leave this conn's tx still on the stack (as current) but with a dead
    // send_task — a concurrent send could succeed on the channel, get acked out of
    // the durable outbox, then be dropped when the buffer dies → message lost with
    // no re-flush (the stack model keeps the bot connected, so nothing re-triggers
    // delivery).
    let fully_offline = state.unregister_conn(&bot_id, conn_gen);
    // send_task pumps THIS socket's sink and dies with it regardless of generation;
    // abort it now that the conn is off the stack.
    send_task.abort();
    if fully_offline {
        let _ = state.store.set_connected(&bot_id, false);
        tracing::info!("bot {bot_id} disconnected (gen {conn_gen})");
    } else {
        // Another connection for this bot is still live (double-connect overlap).
        // It is now the current one; the bot stays connected. Flush the outbox so
        // anything queued while this conn was current lands on the promoted conn
        // instead of stranding on the socket that just died. This is the C8 fix:
        // the surviving pod is promoted back to current, no zombie, no reset.
        state.flush_outbox(&bot_id);
        tracing::info!("bot {bot_id} connection closed (gen {conn_gen}); still live on another conn");
    }
}
