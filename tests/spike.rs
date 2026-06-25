//! Gateway spike parity (design §6). Mock bots speak the REAL gateway wire
//! protocol (GatewayEvent/Reply/Response) over WebSocket against the live plane.
//! Proves: thread/react/streaming-edit parity (1 bot) and a full council to a
//! closed verdict (3 and 5 bots), incl. one-thread-per-session convergence.

use futures_util::{SinkExt, StreamExt};
use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{build_router, state::AppState};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn spawn_server() -> SocketAddr {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let state = AppState::new(store);
    let app = build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn register_bot(base: &str, name: &str, role: &str) -> (String, String) {
    let v: Value = reqwest::Client::new()
        .post(format!("http://{base}/v1/bots"))
        .json(&json!({ "name": name, "role": role }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (v["bot_id"].as_str().unwrap().into(), v["token"].as_str().unwrap().into())
}

async fn open_session(base: &str, roster: &[String], chair: Option<&str>, quorum_n: i64) -> String {
    let v: Value = reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions"))
        .json(&json!({
            "title": "spike", "trigger_ref": "github:pr/acme/widgets#1",
            "roster": roster, "chair_bot": chair, "quorum_n": quorum_n,
        }))
        .send().await.unwrap().json().await.unwrap();
    v["session_id"].as_str().unwrap().into()
}

async fn post_client(base: &str, session_id: &str, content: &str) {
    reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions/{session_id}/messages"))
        .json(&json!({ "content": content }))
        .send().await.unwrap();
}

async fn get_session(base: &str, session_id: &str) -> Value {
    reqwest::Client::new()
        .get(format!("http://{base}/v1/sessions/{session_id}"))
        .send().await.unwrap().json().await.unwrap()
}

// --- wire reply builders (what a stock OAB gateway adapter emits) ---

fn reply(session: &str, content: &str, command: Option<&str>, quote: Option<&str>, req: Option<&str>) -> Message {
    let mut r = json!({
        "channel": { "id": session },
        "content": { "type": "text", "text": content },
    });
    if let Some(c) = command { r["command"] = json!(c); }
    if let Some(q) = quote { r["quote_message_id"] = json!(q); }
    if let Some(rq) = req { r["request_id"] = json!(rq); }
    Message::Text(r.to_string())
}

async fn connect(addr: SocketAddr, token: &str) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (ws, _) = connect_async(format!("ws://{addr}/ws?token={token}")).await.unwrap();
    ws
}

#[derive(Clone, Copy)]
enum Role {
    Chair,
    Reviewer,
}

/// Generic council bot: reacts to the client trigger and (chair) to the verdict
/// prompt. Runs until aborted.
fn spawn_panel_bot(addr: SocketAddr, token: String, session: String, name: String, role: Role) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ws = connect(addr, &token).await;
        let (mut w, mut r) = ws.split();
        while let Some(Ok(msg)) = r.next().await {
            let Message::Text(t) = msg else {
                if matches!(msg, Message::Close(_)) { break; }
                continue;
            };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("event_type").is_none() {
                continue; // a GatewayResponse, not an event
            }
            let sender = v["sender"]["id"].as_str().unwrap_or("");
            let msg_id = v["message_id"].as_str().unwrap_or("").to_string();
            match role {
                Role::Reviewer if sender == "client" => {
                    w.send(reply(&session, &format!("review from {name}: LGTM"), None, None, None)).await.ok();
                    w.send(reply(&session, "🆗", Some("add_reaction"), Some(&msg_id), None)).await.ok();
                }
                Role::Chair if sender == "client" => {
                    w.send(reply(&session, "Council", Some("create_topic"), Some(&msg_id), None)).await.ok();
                }
                Role::Chair if sender == "system" => {
                    w.send(reply(&session, "VERDICT: approved", None, None, None)).await.ok();
                }
                _ => {}
            }
        }
    })
}

async fn run_council(reviewer_count: usize) {
    let addr = spawn_server().await;
    let base = addr.to_string();

    let (chair_id, chair_tok) = register_bot(&base, "gandalf", "chair").await;
    let mut roster = vec![chair_id.clone()];
    let mut handles = vec![];

    let mut reviewers = vec![];
    for i in 0..reviewer_count {
        let (id, tok) = register_bot(&base, &format!("rev{i}"), "reviewer").await;
        roster.push(id.clone());
        reviewers.push((id, tok, format!("rev{i}")));
    }

    let session = open_session(&base, &roster, Some(&chair_id), reviewer_count as i64).await;

    handles.push(spawn_panel_bot(addr, chair_tok, session.clone(), "gandalf".into(), Role::Chair));
    for (_, tok, name) in &reviewers {
        handles.push(spawn_panel_bot(addr, tok.clone(), session.clone(), name.clone(), Role::Reviewer));
    }
    // let everyone connect before the trigger
    tokio::time::sleep(Duration::from_millis(200)).await;

    post_client(&base, &session, "Please review PR #1").await;

    // poll to closed
    let mut closed = false;
    let mut last = json!({});
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = get_session(&base, &session).await;
        if last["session"]["state"] == "closed" {
            closed = true;
            break;
        }
    }
    for h in handles { h.abort(); }

    assert!(closed, "session did not close ({reviewer_count} reviewers): {last}");

    let messages = last["messages"].as_array().unwrap();
    // verdict present
    assert!(
        messages.iter().any(|m| m["content"].as_str().unwrap_or("").contains("VERDICT")),
        "no verdict message"
    );
    // one-thread-per-session convergence: at most one distinct non-null thread
    let mut threads: Vec<&str> = messages
        .iter()
        .filter_map(|m| m["thread_id"].as_str())
        .collect();
    threads.sort();
    threads.dedup();
    assert!(threads.len() <= 1, "thread did not converge: {threads:?}");
    // every reviewer's review reached the store (fanout + persistence)
    for (_, _, name) in &reviewers {
        assert!(
            messages.iter().any(|m| m["content"].as_str().unwrap_or("").contains(name.as_str())),
            "missing review from {name}"
        );
    }
}

#[tokio::test]
async fn single_bot_parity_thread_react_streaming() {
    let addr = spawn_server().await;
    let base = addr.to_string();
    let (bot_id, tok) = register_bot(&base, "solo", "chair").await;
    let session = open_session(&base, &[bot_id.clone()], None, 0).await;

    let ws = connect(addr, &tok).await;
    let (mut w, mut r) = ws.split();
    tokio::time::sleep(Duration::from_millis(150)).await;
    post_client(&base, &session, "review this").await;

    // wait for the trigger event
    let trigger = loop {
        if let Some(Ok(Message::Text(t))) = r.next().await {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("event_type").is_some() {
                break v["message_id"].as_str().unwrap().to_string();
            }
        }
    };

    // 1. create_topic (request_id → expect GatewayResponse.thread_id)
    w.send(reply(&session, "Council", Some("create_topic"), Some(&trigger), Some("r1"))).await.unwrap();
    let thread_id = read_response(&mut r, "r1").await["thread_id"].as_str().unwrap().to_string();
    assert!(!thread_id.is_empty(), "no thread_id from create_topic");

    // 2. status reaction
    w.send(reply(&session, "👀", Some("add_reaction"), Some(&trigger), None)).await.unwrap();

    // 3. streaming reply: send stub (request_id → message_id), then edit it
    w.send(reply(&session, "thinking…", None, None, Some("r2"))).await.unwrap();
    let message_id = read_response(&mut r, "r2").await["message_id"].as_str().unwrap().to_string();
    w.send(reply(&session, "final answer", Some("edit_message"), Some(&message_id), None)).await.unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    let s = get_session(&base, &session).await;
    let messages = s["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "final answer"),
        "streaming edit not applied: {messages:?}"
    );
}

async fn read_response<S>(r: &mut S, req: &str) -> Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        if let Some(Ok(Message::Text(t))) = r.next().await {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("request_id").and_then(|x| x.as_str()) == Some(req) {
                return v;
            }
        }
    }
}

#[tokio::test]
async fn council_3_bots() {
    run_council(2).await; // chair + 2 reviewers = 3 bots
}

#[tokio::test]
async fn council_5_bots() {
    run_council(4).await; // chair + 4 reviewers = 5 bots
}
