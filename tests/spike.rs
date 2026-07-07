//! Gateway spike parity (design §6). Mock bots speak the REAL gateway wire
//! protocol (GatewayEvent/Reply/Response) over WebSocket against the live plane.
//! Proves: thread/react/streaming-edit parity (1 bot) and a full council to a
//! closed verdict (3 and 5 bots), incl. one-thread-per-session convergence.

use futures_util::{SinkExt, StreamExt};
use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{build_router, orchestrator, state::AppState};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn spawn_server() -> SocketAddr {
    let (addr, _) = spawn_server_with_state().await;
    addr
}

async fn spawn_server_with_state() -> (SocketAddr, Arc<AppState>) {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let state = AppState::new(store);
    spawn_server_with_app_state(state).await
}

async fn spawn_server_with_discovery_token(token: &str) -> SocketAddr {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let state = AppState::new_with_options(
        store,
        None,
        None,
        None,
        Some(token.to_string()),
        "http://control-plane.zeabur.internal:8090".to_string(),
        None,
    );
    let (addr, _) = spawn_server_with_app_state(state).await;
    addr
}

async fn spawn_server_with_app_state(state: Arc<AppState>) -> (SocketAddr, Arc<AppState>) {
    let app = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
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
    (
        v["bot_id"].as_str().unwrap().into(),
        v["token"].as_str().unwrap().into(),
    )
}

#[tokio::test]
async fn bot_inventory_discover_and_patch_metadata() {
    let addr = spawn_server_with_discovery_token("spike-discovery-token").await;
    let base = addr.to_string();
    let client = reqwest::Client::new();

    let discovered: Value = client
        .post(format!("http://{base}/v1/bots/discover"))
        .bearer_auth("spike-discovery-token")
        .json(&json!({
            "id": "rev-codex-1",
            "name": "Codex reviewer",
            "role": "reviewer",
            "provider": "codex",
            "capabilities": ["review", "gh-read", "review"],
            "version": "openab:test",
            "runtime": {
                "kind": "kubernetes",
                "namespace": "oabcp-local",
                "workload": "deployment/rev-codex-1"
            }
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discovered["created"], true);
    assert_eq!(discovered["bot_id"], "rev-codex-1");
    assert!(discovered["config_url"]
        .as_str()
        .unwrap()
        .ends_with("/bot-config/rev-codex-1?agent=codex"));

    let filtered: Value = client
        .get(format!(
            "http://{base}/v1/bots?provider=codex&capability=review"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bots = filtered["bots"].as_array().unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0]["id"], "rev-codex-1");
    assert_eq!(bots[0]["name"], "Codex reviewer");
    assert_eq!(bots[0]["source"], "discovered");
    assert_eq!(bots[0]["capabilities"], json!(["review", "gh-read"]));
    assert_eq!(bots[0]["rostered"], false);

    let refreshed: Value = client
        .post(format!("http://{base}/v1/bots/discover"))
        .bearer_auth("spike-discovery-token")
        .json(&json!({
            "id": "rev-codex-1",
            "name": "Attempted rename",
            "role": "chair",
            "provider": "codex",
            "capabilities": ["review"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refreshed["created"], false);
    assert!(refreshed["config_url"]
        .as_str()
        .unwrap()
        .ends_with("/bot-config/rev-codex-1?agent=codex"));

    let after_refresh: Value = client
        .get(format!(
            "http://{base}/v1/bots?provider=codex&capability=review"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bots = after_refresh["bots"].as_array().unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0]["name"], "Codex reviewer");
    assert_eq!(bots[0]["role"], "reviewer");
    assert_eq!(bots[0]["capabilities"], json!(["review"]));
    assert_eq!(bots[0]["version"], "openab:test");
    assert_eq!(bots[0]["runtime"]["kind"], "kubernetes");

    let partial_refreshed: Value = client
        .post(format!("http://{base}/v1/bots/discover"))
        .bearer_auth("spike-discovery-token")
        .json(&json!({
            "id": "rev-codex-1",
            "role": "reviewer"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(partial_refreshed["created"], false);
    assert!(partial_refreshed["config_url"]
        .as_str()
        .unwrap()
        .ends_with("/bot-config/rev-codex-1?agent=codex"));

    let after_partial_refresh: Value = client
        .get(format!(
            "http://{base}/v1/bots?provider=codex&capability=review"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bots = after_partial_refresh["bots"].as_array().unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0]["capabilities"], json!(["review"]));
    assert_eq!(bots[0]["version"], "openab:test");
    assert_eq!(bots[0]["runtime"]["workload"], "deployment/rev-codex-1");

    let patched: Value = client
        .patch(format!("http://{base}/v1/bots/rev-codex-1"))
        .json(&json!({
            "enabled": false,
            "health": "quota_exhausted",
            "note": "Claude weekly quota exhausted"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["bot"]["enabled"], false);
    assert_eq!(patched["bot"]["health"], "quota_exhausted");
    assert_eq!(patched["bot"]["source"], "discovered");
    assert_eq!(patched["bot"]["rostered"], false);

    let cleared: Value = client
        .patch(format!("http://{base}/v1/bots/rev-codex-1"))
        .json(&json!({
            "provider": null,
            "note": null,
            "version": null,
            "runtime": null
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cleared["bot"]["provider"], Value::Null);
    assert_eq!(cleared["bot"]["note"], Value::Null);
    assert_eq!(cleared["bot"]["version"], Value::Null);
    assert_eq!(cleared["bot"]["runtime"], Value::Null);

    let degraded: Value = client
        .get(format!(
            "http://{base}/v1/bots?enabled=false&health=quota_exhausted"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bots = degraded["bots"].as_array().unwrap();
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0]["name"], "Codex reviewer");
    assert_eq!(bots[0]["role"], "reviewer");
    assert_eq!(bots[0]["note"], Value::Null);

    client
        .post(format!("http://{base}/v1/bots/discover"))
        .bearer_auth("spike-discovery-token")
        .json(&json!({
            "id": "rev-quoted",
            "name": "Codex \"reviewer\"\n[agent]\ncommand = \"bad\"",
            "role": "reviewer",
            "provider": "codex"
        }))
        .send()
        .await
        .unwrap();
    let config = client
        .get(format!("http://{base}/bot-config/rev-quoted?agent=codex"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(config
        .contains("bot_username = \"Codex \\\"reviewer\\\"\\n[agent]\\ncommand = \\\"bad\\\"\""));
    assert!(!config.lines().any(|line| line == "command = \"bad\""));
    assert!(config.contains("[reactions]\n# A6: a cosmetic OAB flag must not be able to erase quorum votes.\nremove_after_reply = false"));
}

async fn open_session(base: &str, roster: &[String], chair: Option<&str>, quorum_n: i64) -> String {
    let v: Value = reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions"))
        .json(&json!({
            "title": "spike", "trigger_ref": "github:pr/acme/widgets#1",
            "roster": roster, "chair_bot": chair, "quorum_n": quorum_n,
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
    reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions/{session_id}/messages"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .unwrap();
}

async fn get_session(base: &str, session_id: &str) -> Value {
    reqwest::Client::new()
        .get(format!("http://{base}/v1/sessions/{session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

// --- wire reply builders (what a stock OAB gateway adapter emits) ---

fn reply(
    session: &str,
    content: &str,
    command: Option<&str>,
    quote: Option<&str>,
    req: Option<&str>,
) -> Message {
    let mut r = json!({
        "channel": { "id": session },
        "content": { "type": "text", "text": content },
    });
    if let Some(c) = command {
        r["command"] = json!(c);
    }
    // A stock OAB gateway adapter carries the edit/reaction target in `reply_to`,
    // not `quote_message_id` (openab-core gateway.rs). Mirror that so this spike
    // exercises the real wire shape.
    if let Some(q) = quote {
        r["reply_to"] = json!(q);
    }
    if let Some(rq) = req {
        r["request_id"] = json!(rq);
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

#[derive(Clone, Copy)]
enum Role {
    Chair,
    Reviewer,
}

/// Generic council bot: reacts to the client trigger and (chair) to the verdict
/// prompt. Runs until aborted.
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
                continue; // a GatewayResponse, not an event
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
                        None,
                    ))
                    .await
                    .ok();
                    w.send(reply(
                        &session,
                        "🆗",
                        Some("add_reaction"),
                        Some(&msg_id),
                        None,
                    ))
                    .await
                    .ok();
                }
                Role::Chair if sender == "client" => {
                    w.send(reply(
                        &session,
                        "Council",
                        Some("create_topic"),
                        Some(&msg_id),
                        None,
                    ))
                    .await
                    .ok();
                }
                Role::Chair if sender == "system" => {
                    w.send(reply(&session, "VERDICT: approved", None, None, None))
                        .await
                        .ok();
                    // done-signal after the verdict turn (real OAB set_done → 🆗);
                    // the plane closes the session on the chair's done, not its send.
                    w.send(reply(
                        &session,
                        "🆗",
                        Some("add_reaction"),
                        Some(&msg_id),
                        None,
                    ))
                    .await
                    .ok();
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

    handles.push(spawn_panel_bot(
        addr,
        chair_tok,
        session.clone(),
        "gandalf".into(),
        Role::Chair,
    ));
    for (_, tok, name) in &reviewers {
        handles.push(spawn_panel_bot(
            addr,
            tok.clone(),
            session.clone(),
            name.clone(),
            Role::Reviewer,
        ));
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
    for h in handles {
        h.abort();
    }

    assert!(
        closed,
        "session did not close ({reviewer_count} reviewers): {last}"
    );

    let messages = last["messages"].as_array().unwrap();
    // verdict present
    assert!(
        messages
            .iter()
            .any(|m| m["content"].as_str().unwrap_or("").contains("VERDICT")),
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
            messages
                .iter()
                .any(|m| m["content"].as_str().unwrap_or("").contains(name.as_str())),
            "missing review from {name}"
        );
    }
}

#[tokio::test]
async fn single_bot_parity_thread_react_streaming() {
    let addr = spawn_server().await;
    let base = addr.to_string();
    let (bot_id, tok) = register_bot(&base, "solo", "chair").await;
    let session = open_session(&base, std::slice::from_ref(&bot_id), None, 0).await;

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
    w.send(reply(
        &session,
        "Council",
        Some("create_topic"),
        Some(&trigger),
        Some("r1"),
    ))
    .await
    .unwrap();
    let thread_id = read_response(&mut r, "r1").await["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!thread_id.is_empty(), "no thread_id from create_topic");

    // 2. status reaction
    w.send(reply(
        &session,
        "👀",
        Some("add_reaction"),
        Some(&trigger),
        None,
    ))
    .await
    .unwrap();

    // 3. streaming reply: send stub (request_id → message_id), then edit it
    w.send(reply(&session, "thinking…", None, None, Some("r2")))
        .await
        .unwrap();
    let message_id = read_response(&mut r, "r2").await["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    w.send(reply(
        &session,
        "final answer",
        Some("edit_message"),
        Some(&message_id),
        None,
    ))
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    let s = get_session(&base, &session).await;
    let messages = s["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(|m| m["content"] == "final answer"),
        "streaming edit not applied: {messages:?}"
    );
}

/// Solo mode closes a 1-bot session — the case `QuorumCouncil` can't (a lone
/// chair has zero reviewers, so quorum is never reachable). Over the real wire.
#[tokio::test]
async fn solo_single_bot_closes() {
    let addr = spawn_server().await;
    let base = addr.to_string();
    let (bot_id, tok) = register_bot(&base, "solo", "chair").await;

    // mode="solo" — inline (the shared helper opens council); roster is the lone bot.
    let session: String = {
        let v: Value = reqwest::Client::new()
            .post(format!("http://{base}/v1/sessions"))
            .json(&json!({
                "title": "spike-solo", "roster": [bot_id.clone()],
                "chair_bot": bot_id, "quorum_n": 0, "mode": "solo",
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["session_id"].as_str().unwrap().into()
    };

    let ws = connect(addr, &tok).await;
    let (mut w, mut r) = ws.split();
    tokio::time::sleep(Duration::from_millis(150)).await;
    post_client(&base, &session, "review this").await;

    // on the client trigger: post the verdict, then the 🆗 done-signal on it
    let trigger = loop {
        if let Some(Ok(Message::Text(t))) = r.next().await {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("event_type").is_some() && v["sender"]["id"] == "client" {
                break v["message_id"].as_str().unwrap().to_string();
            }
        }
    };
    w.send(reply(&session, "VERDICT: solo approved", None, None, None))
        .await
        .unwrap();
    w.send(reply(
        &session,
        "🆗",
        Some("add_reaction"),
        Some(&trigger),
        None,
    ))
    .await
    .unwrap();

    let mut closed = false;
    let mut last = json!({});
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = get_session(&base, &session).await;
        if last["session"]["state"] == "closed" {
            closed = true;
            break;
        }
    }
    assert!(closed, "solo session did not close: {last}");
    assert!(
        last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"].as_str().unwrap_or("").contains("VERDICT")),
        "no verdict in closed solo session"
    );
}

/// Pipeline mode: a 3-stage sequential handoff closes in order over the wire.
/// Only stage 0 is mentioned on the trigger; each stage's 🆗 relays to the next
/// and prompts it; the last stage's 🆗 closes. Proves the seam handles a
/// structurally-different (non-fan-in) mode with no orchestrator special-casing.
#[tokio::test]
async fn pipeline_three_stages_closes_in_order() {
    let addr = spawn_server().await;
    let base = addr.to_string();

    let mut bots = vec![];
    for i in 0..3 {
        bots.push(register_bot(&base, &format!("s{i}"), "reviewer").await);
    }
    let roster: Vec<String> = bots.iter().map(|(id, _)| id.clone()).collect();

    let session: String = {
        let v: Value = reqwest::Client::new()
            .post(format!("http://{base}/v1/sessions"))
            .json(&json!({
                "title": "spike-pipeline", "roster": roster,
                "quorum_n": 0, "mode": "pipeline",
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["session_id"].as_str().unwrap().into()
    };

    let mut handles = vec![];
    for (i, (_, tok)) in bots.iter().enumerate() {
        handles.push(spawn_pipeline_bot(
            addr,
            tok.clone(),
            session.clone(),
            format!("s{i}"),
        ));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    post_client(&base, &session, "Review PR #1 through the pipeline").await;

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
    for h in handles {
        h.abort();
    }
    assert!(closed, "pipeline did not close: {last}");

    // every stage ran, and strictly in order (messages are created_at-ordered)
    let messages = last["messages"].as_array().unwrap();
    let pos = |name: &str| {
        messages
            .iter()
            .position(|m| m["content"].as_str().unwrap_or("") == format!("stage {name} output"))
    };
    let (p0, p1, p2) = (pos("s0"), pos("s1"), pos("s2"));
    assert!(
        p0.is_some() && p1.is_some() && p2.is_some(),
        "a stage never ran: {messages:?}"
    );
    assert!(
        p0 < p1 && p1 < p2,
        "stages did not run in sequence: {p0:?} {p1:?} {p2:?}"
    );
}

/// Pipeline stage bot: acts only when @mentioned (stage 0 on the trigger, later
/// stages on the handoff prompt) — proving non-starters wait. Posts its stage
/// output then the 🆗 done-signal.
fn spawn_pipeline_bot(
    addr: SocketAddr,
    token: String,
    session: String,
    name: String,
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
            let mentioned = v["mentions"]
                .as_array()
                .map(|a| a.iter().any(|m| m.as_str() == Some(name.as_str())))
                .unwrap_or(false);
            let msg_id = v["message_id"].as_str().unwrap_or("").to_string();
            // act on my turn only: a mentioned trigger (client) or handoff (system)
            if mentioned && (sender == "client" || sender == "system") {
                w.send(reply(
                    &session,
                    &format!("stage {name} output"),
                    None,
                    None,
                    None,
                ))
                .await
                .ok();
                w.send(reply(
                    &session,
                    "🆗",
                    Some("add_reaction"),
                    Some(&msg_id),
                    None,
                ))
                .await
                .ok();
            }
        }
    })
}

/// Self-recruitment over the wire (membership inc2): the chair's `[[recruit:ID]]`
/// adds a member through the admission gate; a reviewer's is denied (authz). No
/// new gateway command — recruit rides a normal message's text.
#[tokio::test]
async fn chair_recruits_through_admission_gate() {
    let addr = spawn_server().await;
    let base = addr.to_string();
    let (chair_id, chair_tok) = register_bot(&base, "chair", "chair").await;
    let (rev_id, rev_tok) = register_bot(&base, "rev0", "reviewer").await;
    // registered, but NOT in the initial roster — recruitment must pull them in
    let (specialist_id, _) = register_bot(&base, "specialist", "reviewer").await;
    let (sneaky_id, _) = register_bot(&base, "sneaky", "reviewer").await;

    let session = open_session(
        &base,
        &[chair_id.clone(), rev_id.clone()],
        Some(&chair_id),
        1,
    )
    .await;

    let chair_ws = connect(addr, &chair_tok).await;
    let (mut chair_w, _cr) = chair_ws.split();
    let rev_ws = connect(addr, &rev_tok).await;
    let (mut rev_w, _rr) = rev_ws.split();
    tokio::time::sleep(Duration::from_millis(150)).await;
    post_client(&base, &session, "review this").await;

    // chair recruits the specialist (authorized) — embedded in a normal message
    chair_w
        .send(reply(
            &session,
            &format!("need a security pass [[recruit:{specialist_id}]]"),
            None,
            None,
            None,
        ))
        .await
        .unwrap();
    // reviewer tries to recruit (NOT authorized) — must be denied
    rev_w
        .send(reply(
            &session,
            &format!("sneaking one in [[recruit:{sneaky_id}]]"),
            None,
            None,
            None,
        ))
        .await
        .unwrap();

    // poll roster until the specialist appears
    let mut roster: Vec<String> = vec![];
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let s = get_session(&base, &session).await;
        roster = s["roster"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if roster.contains(&specialist_id) {
            break;
        }
    }
    assert!(
        roster.contains(&specialist_id),
        "chair's recruit must join the roster: {roster:?}"
    );
    assert!(
        !roster.contains(&sneaky_id),
        "a reviewer's recruit must be denied: {roster:?}"
    );
}

#[tokio::test]
async fn dynamic_replace_api_updates_session_and_standing_roster() {
    let (addr, state) = spawn_server_with_state().await;
    let base = addr.to_string();
    let (chair_id, _) = register_bot(&base, "chair", "chair").await;
    let (old_id, _) = register_bot(&base, "old", "reviewer").await;
    let (new_id, _) = register_bot(&base, "new", "reviewer").await;
    let session = open_session(
        &base,
        &[chair_id.clone(), old_id.clone()],
        Some(&chair_id),
        1,
    )
    .await;

    post_client(&base, &session, "review this").await;
    assert!(!state.store.pending_outbox(&old_id).unwrap().is_empty());

    let replace: Value = reqwest::Client::new()
        .post(format!(
            "http://{base}/v1/sessions/{session}/roster/replace"
        ))
        .json(&json!({ "old_bot_id": old_id.clone(), "new_bot_id": new_id.clone() }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replace["replaced"], true);
    assert_eq!(replace["roster"], json!([chair_id.clone(), new_id.clone()]));
    assert!(state.store.pending_outbox(&old_id).unwrap().is_empty());
    assert!(
        state
            .store
            .pending_outbox(&new_id)
            .unwrap()
            .iter()
            .any(|(_, frame)| frame.contains("review this")),
        "replacement bot should receive backfilled history",
    );

    let standing: Value = reqwest::Client::new()
        .put(format!("http://{base}/v1/council/roster"))
        .json(&json!({ "roster": [chair_id.clone(), new_id.clone()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(standing["source"], "override");
    assert_eq!(
        standing["roster"],
        json!([chair_id.clone(), new_id.clone()])
    );

    let review: Value = reqwest::Client::new()
        .post(format!("http://{base}/v1/review"))
        .json(&json!({ "repo": "o/r", "pr": 99 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let review_session = review["session_id"].as_str().unwrap();
    let opened = get_session(&base, review_session).await;
    assert_eq!(opened["roster"], json!([chair_id, new_id]));
}

/// The live `openabdev/openab#1187` fix: real bots signal completion in message
/// TEXT (`[done]`), not via the `add_reaction` 🆗 the quorum path counts. Prove a
/// council closes on text done-signals with ZERO reactions sent.
#[tokio::test]
async fn council_closes_on_text_done_signal() {
    let addr = spawn_server().await;
    let base = addr.to_string();
    let (chair_id, chair_tok) = register_bot(&base, "chair", "chair").await;
    let (rev_id, rev_tok) = register_bot(&base, "rev0", "reviewer").await;
    let session = open_session(
        &base,
        &[chair_id.clone(), rev_id.clone()],
        Some(&chair_id),
        1,
    )
    .await;

    let h1 = spawn_text_done_bot(addr, chair_tok, session.clone(), Role::Chair);
    let h2 = spawn_text_done_bot(addr, rev_tok, session.clone(), Role::Reviewer);
    tokio::time::sleep(Duration::from_millis(200)).await;
    post_client(&base, &session, "review this").await;

    let mut closed = false;
    let mut last = json!({});
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = get_session(&base, &session).await;
        if last["session"]["state"] == "closed" {
            closed = true;
            break;
        }
    }
    h1.abort();
    h2.abort();
    assert!(
        closed,
        "council must close on text [done] (no reactions sent): {last}"
    );
    // ADR 013: the [[verdict:…]] trailer in the chair final lands as columns.
    assert_eq!(last["session"]["decision"], "approve", "structured decision: {last}");
    assert_eq!(last["session"]["findings_red"], 0);
    assert_eq!(last["session"]["findings_yellow"], 1);
    assert_eq!(last["session"]["findings_green"], 2);
    assert!(
        last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"].as_str().unwrap_or("").contains("VERDICT")),
        "verdict missing from closed session",
    );
}

/// Bot that signals completion via TEXT `[done]` only — never sends add_reaction.
fn spawn_text_done_bot(
    addr: SocketAddr,
    token: String,
    session: String,
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
                    w.send(reply(&session, "review: LGTM [done]", None, None, None))
                        .await
                        .ok();
                }
                Role::Chair if sender == "client" => {
                    w.send(reply(
                        &session,
                        "Council",
                        Some("create_topic"),
                        Some(&msg_id),
                        None,
                    ))
                    .await
                    .ok();
                }
                Role::Chair if sender == "system" => {
                    w.send(reply(
                        &session,
                        "VERDICT: approved [[verdict:approve r=0 y=1 g=2]] [done]",
                        None,
                        None,
                        None,
                    ))
                    .await
                    .ok();
                }
                _ => {}
            }
        }
    })
}

/// Chair that synthesizes proactively if it sees the client trigger. Newer council
/// routing does not mention the chair on the opening trigger, but the trigger is
/// still delivered as context; this bot mirrors the old failure mode where a chair
/// responded too early anyway.
fn spawn_chair_proactive_bot(
    addr: SocketAddr,
    token: String,
    session: String,
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
            if v["sender"]["id"] == "client" {
                let msg_id = v["message_id"].as_str().unwrap_or("").to_string();
                w.send(reply(
                    &session,
                    "Council",
                    Some("create_topic"),
                    Some(&msg_id),
                    None,
                ))
                .await
                .ok();
                w.send(reply(
                    &session,
                    "VERDICT: approved [done]",
                    None,
                    None,
                    None,
                ))
                .await
                .ok();
            }
        }
    })
}

/// Reviewer that deliberates (posts a review) but NEVER emits a done-signal —
/// the silent reviewer that left quorum unreachable on #4.
fn spawn_reviewer_no_done_bot(
    addr: SocketAddr,
    token: String,
    session: String,
    name: String,
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
            if v["sender"]["id"] == "client" {
                w.send(reply(
                    &session,
                    &format!("review from {name}: needs work"),
                    None,
                    None,
                    None,
                ))
                .await
                .ok();
                // no done-signal — intentionally
            }
        }
    })
}

/// A chair that replies to the opening trigger must not be able to close the
/// council before reviewer quorum. The session remains active for the watchdog
/// liveness fallback to close with whatever work is already in the thread.
#[tokio::test]
async fn chair_done_before_quorum_waits_for_watchdog() {
    let (addr, state) = spawn_server_with_state().await;
    let base = addr.to_string();
    let (chair_id, chair_tok) = register_bot(&base, "chair", "chair").await;
    let (rev0_id, rev0_tok) = register_bot(&base, "rev0", "reviewer").await;
    let (rev1_id, rev1_tok) = register_bot(&base, "rev1", "reviewer").await;
    // quorum_n = 2: BOTH reviewers would have to signal for a formal quorum.
    let session = open_session(
        &base,
        &[chair_id.clone(), rev0_id.clone(), rev1_id.clone()],
        Some(&chair_id),
        2,
    )
    .await;

    let hc = spawn_chair_proactive_bot(addr, chair_tok, session.clone());
    let h0 = spawn_reviewer_no_done_bot(addr, rev0_tok, session.clone(), "rev0".into());
    let h1 = spawn_reviewer_no_done_bot(addr, rev1_tok, session.clone(), "rev1".into());
    tokio::time::sleep(Duration::from_millis(200)).await;
    post_client(&base, &session, "review this").await;

    let mut last = json!({});
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = get_session(&base, &session).await;
        if last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"].as_str().unwrap_or("").contains("VERDICT"))
        {
            break;
        }
    }

    assert_eq!(
        last["session"]["state"], "deliberating",
        "chair early [done] must not close before reviewer quorum: {last}",
    );
    assert!(
        last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"].as_str().unwrap_or("").contains("VERDICT")),
        "chair's early verdict should be preserved for timeout synthesis",
    );

    let mut north = state.north_tx.subscribe();
    assert!(
        orchestrator::force_close_timeout(&state, &session).unwrap(),
        "watchdog primitive should force-close the stuck council",
    );
    let mut saw_timeout = false;
    let mut saw_timeout_verdict = false;
    while let Ok(raw) = north.try_recv() {
        let ev: Value = serde_json::from_str(&raw).unwrap();
        if ev["type"] == "timeout" {
            saw_timeout = true;
            assert_eq!(ev["payload"]["done"], 1);
            assert_eq!(ev["payload"]["total"], 3);
            assert_eq!(
                ev["payload"]["absent"],
                json!([rev0_id.clone(), rev1_id.clone()])
            );
            assert!(
                ev["payload"]["verdict"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("TIMEOUT:"),
                "timeout payload should carry a machine-recognizable verdict: {ev}",
            );
        }
        if ev["type"] == "verdict" && ev["payload"]["reason"] == "timeout" {
            saw_timeout_verdict = true;
            assert!(
                ev["payload"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("TIMEOUT:"),
                "timeout verdict should be machine-recognizable: {ev}",
            );
        }
    }
    assert!(saw_timeout, "watchdog did not emit a timeout north event");
    assert!(
        saw_timeout_verdict,
        "watchdog did not emit a timeout verdict event"
    );
    last = get_session(&base, &session).await;
    hc.abort();
    h0.abort();
    h1.abort();
    assert_eq!(
        last["session"]["state"], "closed",
        "watchdog did not close session: {last}"
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
