//! Shared runtime: the bot connection hub (south) + north event broadcast.

use crate::github_app::GitHubApp;
use crate::protocol::{ChannelInfo, Content, GatewayEvent, SenderInfo, EVENT_SCHEMA};
use crate::store::{new_id, now_ms, SessionState, Store};
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
    /// GitHub App credential for minting per-role scoped installation tokens.
    /// None = PAT mode (pr-agent's `deployment_type = "user"`): pods fall back to the
    /// shared `GH_TOKEN` until the App is provisioned (ROADMAP Phase 1).
    pub github_app: Option<GitHubApp>,
    /// Webhook HMAC secret (`x-hub-signature-256`). None = the webhook endpoint
    /// rejects every request (403) — a missing secret is fail-closed, not fail-open.
    pub github_webhook_secret: Option<String>,
    /// Scoped bootstrap token for `/v1/bots/discover`. None = discovery disabled.
    pub bot_discovery_token: Option<String>,
    /// Public/internal base URL returned by `/v1/bots/discover` for `/bot-config`.
    pub config_base_url: String,
    /// Serializes installation-token minting so concurrent requests for the same
    /// `(session, role)` can't each mint a distinct live token (check-then-act race).
    /// Coarse (one lock for all mints) but mints are rare at council scale; make it
    /// keyed if mint volume ever grows.
    pub github_mint_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(store: Arc<dyn Store>) -> Arc<AppState> {
        Self::new_with_options(
            store,
            std::env::var("OABCP_API_KEY").ok(),
            GitHubApp::from_env(),
            std::env::var("GITHUB_WEBHOOK_SECRET").ok(),
            std::env::var("OABCP_BOT_DISCOVERY_TOKEN").ok(),
            std::env::var("OABCP_CONFIG_BASE_URL")
                .unwrap_or_else(|_| "http://control-plane.zeabur.internal:8090".to_string()),
        )
    }

    pub fn new_with_options(
        store: Arc<dyn Store>,
        api_key: Option<String>,
        github_app: Option<GitHubApp>,
        github_webhook_secret: Option<String>,
        bot_discovery_token: Option<String>,
        config_base_url: String,
    ) -> Arc<AppState> {
        let (north_tx, _) = broadcast::channel(1024);
        Arc::new(AppState {
            store,
            hub: Mutex::new(HashMap::new()),
            conn_seq: AtomicU64::new(0),
            north_tx,
            platform: "feishu".into(),
            api_key,
            github_app,
            github_webhook_secret,
            bot_discovery_token,
            config_base_url,
            github_mint_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Register a connection; returns its generation. Pass it back to
    /// `unregister_conn` so a stale disconnect can't evict a newer connection.
    pub fn register_conn(&self, bot_id: &str, tx: mpsc::UnboundedSender<String>) -> u64 {
        let gen = self.conn_seq.fetch_add(1, Ordering::Relaxed);
        self.hub
            .lock()
            .unwrap()
            .insert(bot_id.to_string(), (gen, tx));
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

    /// Flush a bot's durable outbox in order: send each queued frame and ack it.
    /// Stops at the first frame that can't be sent (bot offline) so order holds
    /// and the rest waits for the next connect. Call on reconnect and after each
    /// enqueue. Returns true if anything was delivered.
    pub fn flush_outbox(&self, bot_id: &str) -> bool {
        let pending = self.store.pending_outbox(bot_id).unwrap_or_default();
        let mut delivered = false;
        for (seq, frame) in pending {
            if self.send_to_bot(bot_id, frame) {
                let _ = self.store.ack_outbox(seq);
                delivered = true;
            } else {
                break;
            }
        }
        delivered
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
        // Don't deliver to bots once the session is closed/aborted — otherwise
        // late relays/fanout keep live OAB bots chatting past the verdict.
        // ponytail: one extra store read per delivery; fine at council scale.
        if let Ok(Some(s)) = self.store.session(session_id) {
            if matches!(
                SessionState::from_db_str(&s.state),
                SessionState::Closed | SessionState::Aborted
            ) {
                return false;
            }
        }
        let event = GatewayEvent {
            schema: EVENT_SCHEMA.into(),
            event_id: new_id("evt"),
            timestamp: now_ms().to_string(),
            platform: self.platform.clone(),
            event_type: "message".into(),
            channel: ChannelInfo {
                id: session_id.to_string(),
                // "supergroup" (not "group") so a stock OAB bot opens a forum
                // topic on the trigger (openab-core gateway.rs only threads for
                // supergroup). Both are `is_group` in OAB, so gating is identical;
                // this just unlocks create_topic → one-thread-per-session (§9).
                channel_type: "supergroup".into(),
                thread_id: thread_id.map(String::from),
            },
            sender,
            content,
            mentions,
            message_id: message_id.to_string(),
        };
        // Durable path: queue then flush. A disconnected bot keeps the frame and
        // gets it on reconnect (flush_outbox) instead of losing it.
        let frame = serde_json::to_string(&event).unwrap();
        if self
            .store
            .enqueue_outbox(bot_id, session_id, &frame)
            .is_err()
        {
            return self.send_to_bot(bot_id, frame); // fall back to best-effort
        }
        self.flush_outbox(bot_id)
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
    use crate::store::{SqliteStore, Store};
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
        assert!(
            state.is_connected("bot_a"),
            "newer connection wrongly evicted"
        );
        state.unregister_conn("bot_a", gen1); // current connection drops
        assert!(!state.is_connected("bot_a"));
    }

    #[test]
    fn offline_bot_gets_queued_frames_on_reconnect() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let sender = SenderInfo {
            id: "client".into(),
            name: "client".into(),
            display_name: "client".into(),
            is_bot: false,
        };
        // deliver to a bot that is NOT connected → queued durably, not delivered
        let sent = state.deliver_event(
            "bot_x",
            "ses_1",
            None,
            sender,
            Content::text("hello while offline"),
            vec![],
            "msg_1",
        );
        assert!(!sent, "no live connection → nothing delivered yet");
        assert_eq!(
            store.pending_outbox("bot_x").unwrap().len(),
            1,
            "frame queued"
        );

        // bot connects → flush replays the missed frame, outbox drains
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.register_conn("bot_x", tx);
        assert!(state.flush_outbox("bot_x"));
        assert_eq!(
            store.pending_outbox("bot_x").unwrap().len(),
            0,
            "outbox drained"
        );
        let frame = rx.try_recv().expect("frame delivered on reconnect");
        assert!(frame.contains("hello while offline"));
    }
}
