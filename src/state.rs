//! Shared runtime: the bot connection hub (south) + north event broadcast.

use crate::protocol::{
    ChannelInfo, Content, GatewayEvent, SenderInfo, EVENT_SCHEMA,
};
use crate::store::{now_ms, new_id, Store};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

pub struct AppState {
    pub store: Arc<dyn Store>,
    /// bot_id -> outbound text frames (serialized GatewayEvent / GatewayResponse).
    hub: Mutex<HashMap<String, mpsc::UnboundedSender<String>>>,
    /// north SSE fanout (serialized NorthEvent JSON).
    pub north_tx: broadcast::Sender<String>,
    /// Platform label on the wire. "feishu" unlocks streaming-edit acks (§2).
    pub platform: String,
    /// North API bearer key. None = open (dev/tests).
    pub api_key: Option<String>,
}

impl AppState {
    pub fn new(store: Arc<dyn Store>) -> Arc<AppState> {
        let (north_tx, _) = broadcast::channel(1024);
        Arc::new(AppState {
            store,
            hub: Mutex::new(HashMap::new()),
            north_tx,
            platform: "feishu".into(),
            api_key: std::env::var("OABCP_API_KEY").ok(),
        })
    }

    pub fn register_conn(&self, bot_id: &str, tx: mpsc::UnboundedSender<String>) {
        self.hub.lock().unwrap().insert(bot_id.to_string(), tx);
    }

    pub fn unregister_conn(&self, bot_id: &str) {
        self.hub.lock().unwrap().remove(bot_id);
    }

    /// Send a raw text frame to a connected bot. Returns false if not connected.
    pub fn send_to_bot(&self, bot_id: &str, text: String) -> bool {
        let hub = self.hub.lock().unwrap();
        match hub.get(bot_id) {
            Some(tx) => tx.send(text).is_ok(),
            None => false,
        }
    }

    /// Build + deliver a GatewayEvent carrying `message` to one bot.
    #[allow(clippy::too_many_arguments)]
    pub fn deliver_event(
        &self,
        bot_id: &str,
        session_id: &str,
        thread_id: Option<&str>,
        sender: SenderInfo,
        content: Content,
        mentions: Vec<String>,
        message_id: &str,
    ) -> bool {
        let event = GatewayEvent {
            schema: EVENT_SCHEMA.into(),
            event_id: new_id("evt"),
            timestamp: now_ms().to_string(),
            platform: self.platform.clone(),
            event_type: "message".into(),
            channel: ChannelInfo {
                id: session_id.to_string(),
                channel_type: "group".into(),
                thread_id: thread_id.map(String::from),
            },
            sender,
            content,
            mentions,
            message_id: message_id.to_string(),
        };
        self.send_to_bot(bot_id, serde_json::to_string(&event).unwrap())
    }

    pub fn emit_north(&self, kind: &str, session_id: &str, payload: serde_json::Value) {
        let ev = json!({
            "type": kind,
            "session_id": session_id,
            "payload": payload,
            "ts": now_ms(),
        });
        let _ = self.north_tx.send(ev.to_string());
    }
}
