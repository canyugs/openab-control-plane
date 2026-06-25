//! Shared runtime: the bot connection hub (south) + north event broadcast.

use crate::protocol::{
    ChannelInfo, Content, GatewayEvent, SenderInfo, EVENT_SCHEMA,
};
use crate::store::{now_ms, new_id, Store};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

pub struct AppState {
    pub store: Arc<dyn Store>,
    /// bot_id -> (connection generation, outbound sender). The generation lets a
    /// stale disconnect avoid evicting a newer connection (OAB reconnects/restarts
    /// overlap: new pod registers before the old pod's disconnect fires).
    hub: Mutex<HashMap<String, (u64, mpsc::UnboundedSender<String>)>>,
    conn_seq: AtomicU64,
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
            conn_seq: AtomicU64::new(0),
            north_tx,
            platform: "feishu".into(),
            api_key: std::env::var("OABCP_API_KEY").ok(),
        })
    }

    /// Register a connection; returns its generation. Pass it back to
    /// `unregister_conn` so a stale disconnect can't evict a newer connection.
    pub fn register_conn(&self, bot_id: &str, tx: mpsc::UnboundedSender<String>) -> u64 {
        let gen = self.conn_seq.fetch_add(1, Ordering::Relaxed);
        self.hub.lock().unwrap().insert(bot_id.to_string(), (gen, tx));
        gen
    }

    pub fn unregister_conn(&self, bot_id: &str, gen: u64) {
        let mut hub = self.hub.lock().unwrap();
        if hub.get(bot_id).map(|(g, _)| *g) == Some(gen) {
            hub.remove(bot_id);
        }
    }

    /// Send a raw text frame to a connected bot. Returns false if not connected.
    pub fn send_to_bot(&self, bot_id: &str, text: String) -> bool {
        let hub = self.hub.lock().unwrap();
        match hub.get(bot_id) {
            Some((_, tx)) => tx.send(text).is_ok(),
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

    #[cfg(test)]
    pub fn is_connected(&self, bot_id: &str) -> bool {
        self.hub.lock().unwrap().contains_key(bot_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;
    use tokio::sync::mpsc;

    #[test]
    fn stale_disconnect_does_not_evict_newer_connection() {
        let state = AppState::new(Arc::new(SqliteStore::memory().unwrap()));
        let (tx0, _rx0) = mpsc::unbounded_channel();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let gen0 = state.register_conn("bot_a", tx0); // old pod
        let gen1 = state.register_conn("bot_a", tx1); // new pod replaces
        assert_ne!(gen0, gen1);
        state.unregister_conn("bot_a", gen0); // old pod's late disconnect
        assert!(state.is_connected("bot_a"), "newer connection wrongly evicted");
        state.unregister_conn("bot_a", gen1); // current connection drops
        assert!(!state.is_connected("bot_a"));
    }
}
