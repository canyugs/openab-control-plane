//! Golden snapshots for the frozen v1 kernel wire contracts (ADR 018 ruling 6).
//!
//! These are intentionally integration-level fixtures: one real council session
//! drives the production REST, north event, close-webhook, store, and WebSocket
//! paths. Dynamic values are normalized by an explicit allowlist below.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{build_router, state::AppState};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const DISCOVERY_TOKEN: &str = "wire-shape-discovery-token";
const GOLDEN_DIR: &str = "tests/golden/wire";

#[tokio::test]
async fn frozen_v1_wire_shapes_match_goldens() {
    let (webhook_url, mut webhook_rx) = spawn_close_webhook_listener().await;
    let (addr, state) = spawn_server_with_close_webhook(webhook_url).await;
    let base = addr.to_string();
    let client = reqwest::Client::new();

    let chair = discover_bot(&base, &state, "wire-chair", "Gandalf", "chair").await;
    let reviewer_a = discover_bot(&base, &state, "wire-rev-a", "Ada", "reviewer").await;
    let reviewer_b = discover_bot(&base, &state, "wire-rev-b", "Linus", "reviewer").await;
    let roster = vec![
        chair.id.clone(),
        reviewer_a.id.clone(),
        reviewer_b.id.clone(),
    ];

    let session = open_session(&base, &roster, Some(&chair.id), 2).await;
    let mut north_rx = state.north_tx.subscribe();

    let handles = vec![
        spawn_panel_bot(
            addr,
            chair.token,
            session.clone(),
            "Gandalf".into(),
            Role::Chair,
        ),
        spawn_panel_bot(
            addr,
            reviewer_a.token,
            session.clone(),
            "Ada".into(),
            Role::Reviewer,
        ),
        spawn_panel_bot(
            addr,
            reviewer_b.token,
            session.clone(),
            "Linus".into(),
            Role::Reviewer,
        ),
    ];

    tokio::time::sleep(Duration::from_millis(200)).await;
    post_client(&base, &session, "Please review PR #156").await;

    let verdict_sse = wait_for_north_event(&mut north_rx, &session, "verdict").await;
    let close_webhook = tokio::time::timeout(Duration::from_secs(5), webhook_rx.recv())
        .await
        .expect("timed out waiting for ADR 012 close webhook")
        .expect("close webhook listener stopped before receiving payload");
    wait_for_closed(&base, &session).await;

    let sessions = client
        .get(format!("http://{base}/v1/sessions"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let stats = client
        .get(format!("http://{base}/v1/stats"))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();

    assert_golden("sessions.json", sessions);
    assert_golden("verdict_sse.json", verdict_sse);
    assert_golden("close_webhook.json", close_webhook);
    assert_golden("stats.json", stats);

    for handle in handles {
        handle.abort();
    }
}

async fn spawn_server_with_close_webhook(close_webhook_url: String) -> (SocketAddr, Arc<AppState>) {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let state = AppState::new_with_options(
        store,
        None,
        None,
        None,
        Some(DISCOVERY_TOKEN.to_string()),
        "http://control-plane.zeabur.internal:8090".to_string(),
        Some(close_webhook_url),
    );
    let app = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

async fn spawn_close_webhook_listener() -> (String, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = Router::new()
        .route("/", post(capture_close_webhook))
        .with_state(tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/"), rx)
}

async fn capture_close_webhook(
    State(tx): State<mpsc::UnboundedSender<Value>>,
    body: Bytes,
) -> StatusCode {
    let value = serde_json::from_slice(&body).unwrap();
    tx.send(value).unwrap();
    StatusCode::NO_CONTENT
}

struct BotCred {
    id: String,
    token: String,
}

async fn discover_bot(
    base: &str,
    state: &Arc<AppState>,
    id: &str,
    name: &str,
    role: &str,
) -> BotCred {
    let response = reqwest::Client::new()
        .post(format!("http://{base}/v1/bots/discover"))
        .bearer_auth(DISCOVERY_TOKEN)
        .json(&json!({
            "id": id,
            "name": name,
            "role": role,
            "provider": "wire",
            "capabilities": ["review"],
            "version": "openab:test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let token = state
        .store
        .bot_token_plain(id)
        .unwrap()
        .expect("discovered bot should have a plaintext test token");
    BotCred {
        id: id.to_string(),
        token,
    }
}

async fn open_session(base: &str, roster: &[String], chair: Option<&str>, quorum_n: i64) -> String {
    let v: Value = reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions"))
        .json(&json!({
            "title": "wire-shape council",
            "trigger_ref": "github:pr/acme/widgets#156",
            "roster": roster,
            "chair_bot": chair,
            "quorum_n": quorum_n,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["session_id"].as_str().unwrap().into()
}

async fn post_client(base: &str, session_id: &str, content: &str) {
    let response = reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions/{session_id}/messages"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

async fn wait_for_closed(base: &str, session_id: &str) -> Value {
    let client = reqwest::Client::new();
    let mut last = json!({});
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = client
            .get(format!("http://{base}/v1/sessions/{session_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if last["session"]["state"] == "closed" {
            return last;
        }
    }
    panic!("session did not close: {last}");
}

async fn wait_for_north_event(
    rx: &mut tokio::sync::broadcast::Receiver<String>,
    session_id: &str,
    event_type: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for north {event_type} event"
        );
        let raw = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .ok()
            .and_then(Result::ok);
        let Some(raw) = raw else {
            continue;
        };
        let value: Value = serde_json::from_str(&raw).unwrap();
        if value["session_id"] == session_id && value["type"] == event_type {
            return value;
        }
    }
}

#[derive(Clone, Copy)]
enum Role {
    Chair,
    Reviewer,
}

fn spawn_panel_bot(
    addr: SocketAddr,
    token: String,
    session: String,
    name: String,
    role: Role,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ws = connect(addr, &token).await;
        let (mut w, mut r) = ws.split();
        while let Some(Ok(msg)) = r.next().await {
            let Message::Text(t) = msg else {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
                continue;
            };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("event_type").is_none() {
                continue;
            }
            let sender = v["sender"]["id"].as_str().unwrap_or("");
            let msg_id = v["message_id"].as_str().unwrap_or("").to_string();
            match role {
                Role::Reviewer if sender == "client" => {
                    w.send(reply(
                        &session,
                        &format!("review from {name}: LGTM"),
                        None,
                        None,
                    ))
                    .await
                    .ok();
                    w.send(reply(&session, "🆗", Some("add_reaction"), Some(&msg_id)))
                        .await
                        .ok();
                }
                Role::Chair if sender == "client" => {
                    w.send(reply(
                        &session,
                        "Council",
                        Some("create_topic"),
                        Some(&msg_id),
                    ))
                    .await
                    .ok();
                }
                Role::Chair if sender == "system" => {
                    w.send(reply(
                        &session,
                        "VERDICT: approved [[verdict:approve r=0 y=0 g=2]]",
                        None,
                        None,
                    ))
                    .await
                    .ok();
                    w.send(reply(&session, "🆗", Some("add_reaction"), Some(&msg_id)))
                        .await
                        .ok();
                }
                _ => {}
            }
        }
    })
}

fn reply(session: &str, content: &str, command: Option<&str>, quote: Option<&str>) -> Message {
    let mut r = json!({
        "channel": { "id": session },
        "content": { "type": "text", "text": content },
    });
    if let Some(c) = command {
        r["command"] = json!(c);
    }
    if let Some(q) = quote {
        r["reply_to"] = json!(q);
    }
    Message::Text(r.to_string())
}

async fn connect(
    addr: SocketAddr,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (ws, _) = connect_async(format!("ws://{addr}/ws?token={token}"))
        .await
        .unwrap();
    ws
}

fn assert_golden(name: &str, mut actual: Value) {
    normalize_value(&mut actual, dynamic_keys_for(name));
    let rendered = serde_json::to_string_pretty(&actual).unwrap() + "\n";
    let path = Path::new(GOLDEN_DIR).join(name);
    if std::env::var_os("OABCP_REGEN_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rendered).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read golden {}: {err}. The four wire shapes are frozen v1 contracts per ADR 018 ruling 6; regenerate with OABCP_REGEN_GOLDEN=1 cargo test --test wire_shapes",
            path.display()
        )
    });
    assert_eq!(
        rendered,
        expected,
        "wire-shape golden mismatch for {}. The four wire shapes are frozen v1 contracts per ADR 018 ruling 6; evolution belongs to the M4 ADR. Regenerate intentionally with OABCP_REGEN_GOLDEN=1 cargo test --test wire_shapes",
        path.display()
    );
}

fn normalize_value(value: &mut Value, dynamic_keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if dynamic_keys.contains(&key.as_str()) && !child.is_null() {
                    *child = Value::String("<dyn>".to_string());
                } else {
                    normalize_value(child, dynamic_keys);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_value(item, dynamic_keys);
            }
        }
        _ => {}
    }
}

fn dynamic_keys_for(name: &str) -> &'static [&'static str] {
    match name {
        "sessions.json" => &[
            // Store-generated session id plus wall-clock timestamps.
            "id",
            "created_at",
            "updated_at",
            "closed_at",
        ],
        "verdict_sse.json" => &[
            // North envelope session id plus emit timestamp.
            "session_id",
            "ts",
        ],
        "close_webhook.json" => &[
            // ADR 012 envelope session id plus webhook timestamp.
            "session_id",
            "ts",
        ],
        "stats.json" => &[
            // Bot connection timestamp and scheduler-dependent verdict durations.
            "last_seen_ms",
            "p50",
            "p95",
        ],
        _ => panic!("no dynamic-key allowlist registered for {name}"),
    }
}
