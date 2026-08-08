//! Shared runtime: the bot connection hub (south) + north event broadcast.

use crate::protocol::{ChannelInfo, Content, GatewayEvent, SenderInfo, EVENT_SCHEMA};
use crate::store::{new_id, now_ms, SessionState, Store};
use controller_protocol::audit::{
    AuditActor, AuditCorrelation, AuditEvent, AuditOutcome, AuditTarget, AUDIT_EVENT_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

fn council_config_from_env() -> crate::plugins::council::CouncilConfig {
    crate::plugins::council::CouncilConfig::from_values(|name| std::env::var(name).ok())
}

fn ws_ping_secs_from_env() -> u64 {
    std::env::var("OABCP_WS_PING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(20)
}

fn controller_auth_from_env() -> Option<crate::controller_api::ControllerAuthConfig> {
    match crate::controller_api::ControllerAuthConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "controller action auth configuration rejected; API disabled");
            None
        }
    }
}

fn controller_events_from_env() -> Option<Arc<crate::controller_events::ControllerEventRuntime>> {
    match crate::controller_events::ControllerEventRuntime::from_env() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "controller event configuration rejected; delivery disabled");
            None
        }
    }
}

/// One live south connection: its generation + the outbound frame sender.
type Conn = (u64, mpsc::UnboundedSender<String>);

const AUDIT_SERVICE: &str = "openab-control-plane";

fn message_audit_detail(payload: &Value, operation: Option<&str>) -> Value {
    let content = payload["content"]
        .as_str()
        .or_else(|| payload["body"].as_str())
        .unwrap_or_default();
    let mut detail = json!({
        "message_id": payload["message_id"].clone(),
        "author": payload["author"].clone(),
        "audience": payload["audience"].clone(),
        "content_bytes": content.len(),
        "content_sha256": hex::encode(Sha256::digest(content.as_bytes())),
    });
    if let Some(operation) = operation {
        detail["operation"] = Value::String(operation.into());
    }
    detail
}

/// Convert the existing north projection into a bounded, provider-neutral
/// journal fact. Message bodies and verdict text stay in their canonical
/// domain tables; only metadata and hashes enter `audit_events`.
fn north_audit_event(
    kind: &str,
    session_id: &str,
    payload: &Value,
    now: i64,
) -> Option<AuditEvent> {
    let (audit_kind, outcome, detail) = match kind {
        "state" => {
            let state = payload["state"].as_str();
            let audit_kind = match state {
                Some("closed") => "session.closed",
                Some("aborted") => "session.aborted",
                _ => "session.state_changed",
            };
            (audit_kind, AuditOutcome::Succeeded, payload.clone())
        }
        "message" | "controller_status" => (
            "session.message_recorded",
            AuditOutcome::Succeeded,
            message_audit_detail(payload, None),
        ),
        "message_edit" => (
            "session.message_recorded",
            AuditOutcome::Succeeded,
            message_audit_detail(payload, Some("edit")),
        ),
        "roster_add" | "roster_drop" | "roster_replace" => (
            "session.roster_changed",
            AuditOutcome::Succeeded,
            payload.clone(),
        ),
        "timeout" => (
            "session.timed_out",
            AuditOutcome::Succeeded,
            json!({
                "reason": payload["reason"].clone(),
                "done": payload["done"].clone(),
                "total": payload["total"].clone(),
                "absent": payload["absent"].clone(),
                "trigger_ref": payload["trigger_ref"].clone(),
            }),
        ),
        "bot_health" => (
            "bot.health_changed",
            AuditOutcome::Succeeded,
            payload.clone(),
        ),
        "failover" => ("bot.replaced", AuditOutcome::Succeeded, payload.clone()),
        "chair_reassigned" => ("bot.replaced", AuditOutcome::Succeeded, payload.clone()),
        _ => return None,
    };

    let session = (!session_id.is_empty() && session_id != "-").then(|| session_id.to_string());
    let trigger_ref = payload["trigger_ref"].as_str().map(str::to_string);
    let message_id = payload["message_id"].as_str().map(str::to_string);
    let edit_hash = (kind == "message_edit")
        .then(|| detail["content_sha256"].as_str())
        .flatten();
    let event_key = match (audit_kind, message_id.as_deref(), kind) {
        ("session.message_recorded", Some(message_id), "message_edit") => {
            format!(
                "session.message_recorded:{message_id}:edit:{}",
                edit_hash.unwrap_or("unknown")
            )
        }
        ("session.message_recorded", Some(message_id), _) => {
            format!("session.message_recorded:{message_id}")
        }
        _ => format!("{audit_kind}:{}", new_id("evt")),
    };
    let event_id = format!("aud:{AUDIT_SERVICE}:{event_key}");
    let target = payload["bot"]
        .as_str()
        .filter(|bot| !bot.is_empty())
        .map(|bot| AuditTarget {
            kind: "openab_bot".into(),
            reference: Some(bot.into()),
            ..Default::default()
        })
        .or_else(|| {
            session.as_ref().map(|session_id| AuditTarget {
                kind: "openab_session".into(),
                reference: Some(session_id.clone()),
                ..Default::default()
            })
        });
    let actor = payload["author"].as_str().map(|author| AuditActor {
        kind: if author == "system" {
            "system".into()
        } else if author == "client" {
            "client".into()
        } else {
            "bot".into()
        },
        id: (!matches!(author, "system" | "client")).then(|| author.into()),
        ..Default::default()
    });
    Some(AuditEvent {
        version: AUDIT_EVENT_VERSION,
        event_id,
        event_key,
        occurred_at: now,
        recorded_at: now,
        service: AUDIT_SERVICE.into(),
        kind: audit_kind.into(),
        outcome,
        caused_by: None,
        correlation: AuditCorrelation {
            session_id: session,
            trigger_ref,
            message_id,
            ..Default::default()
        },
        actor,
        target,
        detail,
        error: None,
    })
}

fn bot_connection_audit_event(bot_id: &str, generation: u64, kind: &str) -> AuditEvent {
    let event_key = format!("{kind}:{bot_id}:{generation}");
    AuditEvent {
        version: AUDIT_EVENT_VERSION,
        event_id: format!("aud:{AUDIT_SERVICE}:{event_key}"),
        event_key,
        occurred_at: now_ms(),
        recorded_at: now_ms(),
        service: AUDIT_SERVICE.into(),
        kind: kind.into(),
        outcome: AuditOutcome::Succeeded,
        caused_by: None,
        correlation: AuditCorrelation::default(),
        actor: None,
        target: Some(AuditTarget {
            kind: "openab_bot".into(),
            reference: Some(bot_id.into()),
            ..Default::default()
        }),
        detail: json!({ "generation": generation }),
        error: None,
    }
}

pub struct AppState {
    pub store: Arc<dyn Store>,
    /// Provider-specific review policy, loaded once by the composition root and
    /// passed to the plugin as immutable data.
    pub council_config: crate::plugins::council::CouncilConfig,
    /// bot_id -> stack of live connections `(generation, outbound sender)`, oldest
    /// first, so the LAST entry is the current one messages route to. Keeping every
    /// live conn (not just the newest) is what fixes the double-connect zombie
    /// (C8): when two same-bot_id pods overlap and the current one drops, the
    /// still-live older conn is promoted back to current instead of the bot being
    /// marked offline. `connected` = "this stack is non-empty".
    hub: Mutex<HashMap<String, Vec<Conn>>>,
    conn_seq: AtomicU64,
    /// Per-bot flush mutexes. Entries are retained for process lifetime; the set
    /// is bounded by bot inventory and avoids reconnect-storm duplicate sends.
    flush_locks: Mutex<HashMap<String, Arc<std::sync::Mutex<()>>>>,
    /// north SSE fanout (serialized NorthEvent JSON).
    pub north_tx: broadcast::Sender<String>,
    /// Platform label on the wire. "feishu" unlocks streaming-edit acks (§2).
    pub platform: String,
    /// North API bearer key. None = open (dev/tests).
    pub api_key: Option<String>,
    /// Versioned deployment-held HMAC peppers for external controller action
    /// tokens. None disables the external action endpoint fail-closed.
    pub controller_auth: Option<crate::controller_api::ControllerAuthConfig>,
    /// Versioned signing keys plus an HTTPS transport for durable controller
    /// runtime events. None keeps event configuration and dispatch fail-closed.
    pub controller_events: Option<Arc<crate::controller_events::ControllerEventRuntime>>,
    /// Serializes action execution after durable admission so concurrent replay
    /// cannot execute the interpreter twice in this single-process SQLite runtime.
    pub controller_action_lock: std::sync::Mutex<()>,
    /// Scoped bootstrap token for `/v1/bots/discover`. None = discovery disabled.
    pub bot_discovery_token: Option<String>,
    /// Public/internal base URL returned by `/v1/bots/discover` for `/bot-config`.
    pub config_base_url: String,
    /// Optional outbound webhook POSTed on session close (ADR 012). None = off.
    pub close_webhook_url: Option<String>,
    /// South WebSocket ping interval in seconds. 0 disables ping deadlines.
    pub ws_ping_secs: u64,
    /// Per-session recruit directives already processed. This bounds repeated
    /// parse paths and the unknown-target provision signal surface.
    pub recruit_seen: Mutex<HashMap<String, HashSet<String>>>,
    /// Message ids whose `[[intent:…]]` was already emitted, keyed
    /// session → bot (ADR 039). Absorbs streamed-edit re-parses; the cap is
    /// per bot, not per session — intents are open to every roster member
    /// (unlike chair-only recruit), so a shared budget would let one looping
    /// bot starve the whole session's intent channel (PR 381 F1).
    pub intent_seen: Mutex<HashMap<String, HashMap<String, HashSet<String>>>>,
    /// Serializes auto-failover roster swaps (ADR 023 Phase 4). Two bots degrading
    /// concurrently would otherwise each read the same roster snapshot and the later
    /// `set_standing_roster` would clobber the earlier swap (council F7). Holding
    /// this across the read-modify-write makes the swap atomic. Sync mutex: the
    /// swap path is fully synchronous (no await held).
    pub failover_lock: std::sync::Mutex<()>,
    /// Process-local dedupe for compatibility counters whose hot paths can run
    /// repeatedly for one logical object. The durable counter still survives
    /// restarts; at most one new use is recorded per `(surface, identity)` per
    /// process lifetime.
    compatibility_seen: Mutex<HashSet<String>>,
}

impl AppState {
    /// Record a migration/deprecation counter without making the observed path
    /// depend on telemetry availability. The structured warning provides a log
    /// signal while the store row provides restart-durable release evidence.
    pub fn record_compatibility_use(&self, surface: &str, amount: i64) {
        if let Err(error) = self.store.record_compatibility_use(surface, amount) {
            tracing::warn!(surface, amount, %error, "compatibility usage telemetry failed");
        }
    }

    pub fn record_compatibility_use_once(&self, surface: &str, identity: &str) {
        let key = format!("{surface}\0{identity}");
        if self.compatibility_seen.lock().unwrap().insert(key) {
            self.record_compatibility_use(surface, 1);
        }
    }

    pub fn new(store: Arc<dyn Store>) -> Arc<AppState> {
        Self::new_with_options_and_runtime_config(
            store,
            std::env::var("OABCP_API_KEY").ok(),
            std::env::var("OABCP_BOT_DISCOVERY_TOKEN").ok(),
            std::env::var("OABCP_CONFIG_BASE_URL")
                .unwrap_or_else(|_| "http://control-plane.zeabur.internal:8090".to_string()),
            std::env::var("OABCP_SESSION_CLOSE_WEBHOOK").ok(),
            ws_ping_secs_from_env(),
            council_config_from_env(),
            controller_auth_from_env(),
            controller_events_from_env(),
        )
    }

    pub fn new_with_options(
        store: Arc<dyn Store>,
        api_key: Option<String>,
        bot_discovery_token: Option<String>,
        config_base_url: String,
        close_webhook_url: Option<String>,
    ) -> Arc<AppState> {
        Self::new_with_options_and_ws_ping_secs(
            store,
            api_key,
            bot_discovery_token,
            config_base_url,
            close_webhook_url,
            ws_ping_secs_from_env(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_options_and_ws_ping_secs(
        store: Arc<dyn Store>,
        api_key: Option<String>,
        bot_discovery_token: Option<String>,
        config_base_url: String,
        close_webhook_url: Option<String>,
        ws_ping_secs: u64,
    ) -> Arc<AppState> {
        Self::new_with_options_and_runtime_config(
            store,
            api_key,
            bot_discovery_token,
            config_base_url,
            close_webhook_url,
            ws_ping_secs,
            crate::plugins::council::CouncilConfig::default(),
            None,
            None,
        )
    }

    /// Test/embedding constructor for the external-controller action boundary.
    pub fn new_with_controller_auth(
        store: Arc<dyn Store>,
        controller_auth: crate::controller_api::ControllerAuthConfig,
    ) -> Arc<AppState> {
        Self::new_with_options_and_runtime_config(
            store,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::council::CouncilConfig::default(),
            Some(controller_auth),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_options_and_runtime_config(
        store: Arc<dyn Store>,
        api_key: Option<String>,
        bot_discovery_token: Option<String>,
        config_base_url: String,
        close_webhook_url: Option<String>,
        ws_ping_secs: u64,
        council_config: crate::plugins::council::CouncilConfig,
        controller_auth: Option<crate::controller_api::ControllerAuthConfig>,
        controller_events: Option<Arc<crate::controller_events::ControllerEventRuntime>>,
    ) -> Arc<AppState> {
        let (north_tx, _) = broadcast::channel(1024);
        Arc::new(AppState {
            store,
            council_config,
            hub: Mutex::new(HashMap::new()),
            conn_seq: AtomicU64::new(0),
            flush_locks: Mutex::new(HashMap::new()),
            north_tx,
            platform: "feishu".into(),
            api_key,
            controller_auth,
            controller_events,
            controller_action_lock: std::sync::Mutex::new(()),
            bot_discovery_token,
            config_base_url,
            close_webhook_url,
            ws_ping_secs,
            recruit_seen: Mutex::new(HashMap::new()),
            intent_seen: Mutex::new(HashMap::new()),
            failover_lock: std::sync::Mutex::new(()),
            compatibility_seen: Mutex::new(HashSet::new()),
        })
    }

    /// Register a connection; returns its generation. Pass it back to
    /// `unregister_conn` on teardown. The new conn becomes the current one; any
    /// existing conns for this bot stay live underneath it (see C8).
    pub fn register_conn(&self, bot_id: &str, tx: mpsc::UnboundedSender<String>) -> u64 {
        let gen = self.conn_seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut hub = self.hub.lock().unwrap();
            let stack = hub.entry(bot_id.to_string()).or_default();
            // A second live connection for the same bot_id (fresh-pod double-dial /
            // overlapping reconnect). No longer a hazard — the older conn is kept and
            // can be promoted back — but still worth surfacing as the C8 fingerprint.
            if let Some((old_gen, _)) = stack.last() {
                tracing::warn!(
                    "bot {bot_id} second live connection gen {old_gen}->{gen} (overlap)"
                );
            }
            stack.push((gen, tx));
        }
        if let Err(error) =
            self.store
                .append_audit_event(&bot_connection_audit_event(bot_id, gen, "bot.connected"))
        {
            tracing::warn!(%error, bot_id, generation = gen, "bot connection journal append failed");
        }
        gen
    }

    /// Remove connection `gen` from the bot's stack. Returns true iff the bot now
    /// has NO live connections left — i.e. the caller should mark it offline. If a
    /// superseded/older conn remains (the double-connect case), returns false: the
    /// bot is still reachable on that conn and must stay `connected`.
    pub fn unregister_conn(&self, bot_id: &str, gen: u64) -> bool {
        let (removed, fully_offline) = {
            let mut hub = self.hub.lock().unwrap();
            if let Some(stack) = hub.get_mut(bot_id) {
                let removed = stack.iter().any(|(g, _)| *g == gen);
                if removed {
                    stack.retain(|(g, _)| *g != gen);
                }
                let fully_offline = removed && stack.is_empty();
                if fully_offline {
                    hub.remove(bot_id);
                }
                (removed, fully_offline)
            } else {
                (false, false)
            }
        };
        if removed {
            if let Err(error) = self.store.append_audit_event(&bot_connection_audit_event(
                bot_id,
                gen,
                "bot.disconnected",
            )) {
                tracing::warn!(%error, bot_id, generation = gen, "bot disconnection journal append failed");
            }
        }
        fully_offline
    }

    /// Send a raw text frame to a bot's current (most recent live) connection.
    /// Returns false if not connected.
    pub fn send_to_bot(&self, bot_id: &str, text: String) -> bool {
        let hub = self.hub.lock().unwrap();
        match hub.get(bot_id).and_then(|stack| stack.last()) {
            Some((_, tx)) => tx.send(text).is_ok(),
            None => false,
        }
    }

    /// Flush a bot's durable outbox in order: send each queued frame and ack it.
    /// Stops at the first frame that can't be sent (bot offline) so order holds
    /// and the rest waits for the next connect. Call on reconnect and after each
    /// enqueue. Returns true if anything was delivered.
    pub fn flush_outbox(&self, bot_id: &str) -> bool {
        let flush_lock = {
            let mut locks = self.flush_locks.lock().unwrap();
            locks
                .entry(bot_id.to_string())
                .or_insert_with(|| Arc::new(std::sync::Mutex::new(())))
                .clone()
        };
        // Lock hierarchy: flush_lock(bot) -> hub lock (send_to_bot) -> store conn.
        // No code acquires those in reverse order, and callers do not await while
        // holding this std mutex.
        let _guard = flush_lock.lock().unwrap();
        // This remains at-least-once across process crashes between send and ack;
        // socket-confirmed ack is Stage 2. Within a live process, one flusher owns
        // the full read -> send -> ack loop for this bot.
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
        // Idempotency key: one logical message per bot. A retry / backfill re-enqueue
        // hits the UNIQUE index and is dropped instead of double-queuing (C2). message_id
        // is globally unique, so bot_id scopes it to this recipient.
        let idem_key = format!("{bot_id}:{message_id}");
        if self
            .store
            .enqueue_outbox(bot_id, session_id, &idem_key, &frame)
            .is_err()
        {
            return self.send_to_bot(bot_id, frame); // fall back to best-effort
        }
        self.flush_outbox(bot_id)
    }

    pub fn is_connected(&self, bot_id: &str) -> bool {
        // unregister_conn removes a bot's entry the moment its stack empties, so
        // "present" == "has a live conn" — no need to also check non-empty (#100 F4).
        self.hub.lock().unwrap().contains_key(bot_id)
    }

    pub fn emit_north(&self, kind: &str, session_id: &str, payload: serde_json::Value) {
        let now = now_ms();
        if let Some(audit) = north_audit_event(kind, session_id, &payload, now) {
            if let Err(error) = self.store.append_audit_event(&audit) {
                tracing::warn!(
                    %error,
                    kind,
                    session_id,
                    "north event investigation journal append failed"
                );
            }
        }
        let ev = json!({
            "type": kind,
            "session_id": session_id,
            "payload": payload,
            "ts": now,
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
        let gen1 = state.register_conn("bot_a", tx1); // new pod overlaps
        assert_ne!(gen0, gen1);
        // Old pod's late disconnect: a newer conn is still live → returns false, so
        // ws.rs does NOT flip `connected` false for the still-live bot.
        assert!(
            !state.unregister_conn("bot_a", gen0),
            "a live newer conn remains → must report bot still connected"
        );
        assert!(
            state.is_connected("bot_a"),
            "newer connection wrongly evicted"
        );
        // Last connection drops: stack now empty → returns true → ws.rs marks offline.
        assert!(
            state.unregister_conn("bot_a", gen1),
            "removing the last conn must report the bot fully offline"
        );
        assert!(!state.is_connected("bot_a"));
    }

    #[test]
    fn current_conn_death_promotes_surviving_older_conn() {
        // The C8 zombie: two same-bot_id pods overlap, the CURRENT (newer) conn
        // dies, and the older conn is still live. It must be promoted back to
        // current — bot stays connected and messages route to it — not marked
        // offline. rx0 stays in scope so tx0 is a live sender.
        let state = AppState::new(Arc::new(SqliteStore::memory().unwrap()));
        let (tx0, _rx0) = mpsc::unbounded_channel();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let _gen0 = state.register_conn("bot_a", tx0); // survivor pod, connects first
        let gen1 = state.register_conn("bot_a", tx1); // doomed pod, connects second (current)
                                                      // doomed conn dies while survivor is still live → not fully offline.
        assert!(
            !state.unregister_conn("bot_a", gen1),
            "surviving older conn remains → bot must stay connected"
        );
        assert!(
            state.is_connected("bot_a"),
            "survivor wrongly evicted → zombie"
        );
        // Messages now route to the promoted survivor (tx0), not the dead conn.
        assert!(
            state.send_to_bot("bot_a", "ping".into()),
            "promoted conn must accept sends"
        );
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

    #[test]
    fn deliver_event_dedups_same_message_id_after_ack() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store);
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.register_conn("bot_x", tx);
        let sender = SenderInfo {
            id: "client".into(),
            name: "client".into(),
            display_name: "client".into(),
            is_bot: false,
        };

        assert!(state.deliver_event(
            "bot_x",
            "ses_1",
            None,
            sender.clone(),
            Content::text("hello"),
            vec![],
            "msg_1",
        ));
        assert!(!state.deliver_event(
            "bot_x",
            "ses_1",
            None,
            sender,
            Content::text("hello"),
            vec![],
            "msg_1",
        ));

        let frames: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("msg_1"));
    }

    #[test]
    fn concurrent_flush_outbox_sends_each_pending_frame_once() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let frames = 50;
        for i in 0..frames {
            store
                .enqueue_outbox(
                    "bot_x",
                    "ses_1",
                    &format!("bot_x:msg_{i}"),
                    &format!("frame_{i}"),
                )
                .unwrap();
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.register_conn("bot_x", tx);

        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let state = state.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                state.flush_outbox("bot_x")
            }));
        }
        barrier.wait();
        for handle in handles {
            let _ = handle.join().unwrap();
        }

        let delivered: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let unique: std::collections::HashSet<_> = delivered.iter().cloned().collect();
        assert_eq!(delivered.len(), frames);
        assert_eq!(unique.len(), frames);
        assert!(store.pending_outbox("bot_x").unwrap().is_empty());
    }
}
