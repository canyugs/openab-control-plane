//! S16 second-consumer contract: forum Phase 1 builds against this north
//! surface only, using trait defaults and zero plugin code.

use futures_util::{SinkExt, StreamExt};
use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{build_router, state::AppState};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const FORUM_QUESTION: &str =
    "FORUM-S16-QUESTION: How should a forum responder explain the deploy error?";
const FIRST_ANSWER: &str = "FORUM-S16-ANSWER-1: Check the build logs and retry. [done]";
const FOLLOW_UP: &str = "FORUM-S16-FOLLOW-UP: What should I do if it still fails?";
const SECOND_ANSWER: &str = "FORUM-S16-ANSWER-2: Share the latest runtime log. [done]";

#[tokio::test]
async fn forum_shaped_second_consumer_flow_uses_only_north_surface_and_defaults() {
    let (addr, state) = spawn_server().await;
    let base = addr.to_string();
    let mut north_rx = state.north_tx.subscribe();

    let (bot_id, token) = register_bot(&base, "forum-responder", "responder").await;
    let ws = connect(addr, &token).await;
    let (mut w, mut r) = ws.split();

    let session = open_solo_session(&base, &bot_id, FORUM_QUESTION).await;

    let first_trigger = read_bot_event(&mut r).await;
    assert_eq!(first_trigger["sender"]["id"], "client");
    assert_eq!(first_trigger["content"]["text"], FORUM_QUESTION);

    w.send(reply(&session, FIRST_ANSWER)).await.unwrap();

    let first_verdict = wait_for_north_event(&mut north_rx, &session, "verdict").await;
    assert_text_only_verdict_event(&first_verdict, FIRST_ANSWER);
    let first_closed = wait_for_state(&base, &session, "closed").await;
    assert_session_text_only_verdict(&first_closed, FIRST_ANSWER);

    let follow_up = post_client(&base, &session, FOLLOW_UP).await;
    assert_eq!(follow_up.status(), reqwest::StatusCode::OK);
    wait_for_state(&base, &session, "deliberating").await;

    let second_trigger = read_bot_event(&mut r).await;
    assert_eq!(second_trigger["sender"]["id"], "client");
    assert_eq!(second_trigger["content"]["text"], FOLLOW_UP);

    w.send(reply(&session, SECOND_ANSWER)).await.unwrap();

    let second_closed = wait_for_state(&base, &session, "closed").await;
    assert_session_text_only_verdict(&second_closed, SECOND_ANSWER);
}

async fn spawn_server() -> (SocketAddr, Arc<AppState>) {
    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let state = AppState::new(store);
    let app = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

async fn register_bot(base: &str, name: &str, role: &str) -> (String, String) {
    let response = reqwest::Client::new()
        .post(format!("http://{base}/v1/bots"))
        .json(&json!({ "name": name, "role": role }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    (
        value["bot_id"].as_str().unwrap().to_string(),
        value["token"].as_str().unwrap().to_string(),
    )
}

async fn open_solo_session(base: &str, bot_id: &str, prompt: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions"))
        .json(&json!({
            "title": "S16 forum-shaped second consumer",
            "trigger_ref": "forum:s16-second-consumer-proof",
            "roster": [bot_id],
            "chair_bot": bot_id,
            "quorum_n": 0,
            "mode": "solo",
            "prompt": prompt,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["deduped"], false);
    value["session_id"].as_str().unwrap().to_string()
}

async fn post_client(base: &str, session_id: &str, content: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions/{session_id}/messages"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .unwrap()
}

async fn get_session(base: &str, session_id: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("http://{base}/v1/sessions/{session_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn wait_for_state(base: &str, session_id: &str, state: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    let mut last = json!({});
    loop {
        last = get_session(base, session_id).await;
        if last["session"]["state"] == state {
            return last;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session did not reach state {state}: {last}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_north_event(
    rx: &mut tokio::sync::broadcast::Receiver<String>,
    session_id: &str,
    event_type: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for north {event_type} event"
        );
        let Some(raw) = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .ok()
            .and_then(Result::ok)
        else {
            continue;
        };
        let value: Value = serde_json::from_str(&raw).unwrap();
        if value["session_id"] == session_id && value["type"] == event_type {
            return value;
        }
    }
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

async fn read_bot_event<R>(r: &mut R) -> Value
where
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for bot gateway event"
        );
        let Some(Ok(Message::Text(text))) =
            tokio::time::timeout(Duration::from_millis(500), r.next())
                .await
                .ok()
                .flatten()
        else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        if value.get("event_type").is_some() {
            return value;
        }
    }
}

fn reply(session: &str, content: &str) -> Message {
    Message::Text(
        json!({
            "channel": { "id": session },
            "content": { "type": "text", "text": content },
        })
        .to_string(),
    )
}

fn assert_text_only_verdict_event(event: &Value, expected_text: &str) {
    assert_eq!(event["payload"]["text"], expected_text);
    assert!(event["payload"]["decision"].is_null(), "{event}");
    assert!(event["payload"]["findings_red"].is_null(), "{event}");
    assert!(event["payload"]["findings_yellow"].is_null(), "{event}");
    assert!(event["payload"]["findings_green"].is_null(), "{event}");
}

fn assert_session_text_only_verdict(session: &Value, expected_text: &str) {
    assert_eq!(session["session"]["state"], "closed");
    assert!(session["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| {
            message["author_kind"] == "bot"
                && message["content"].as_str().unwrap_or_default() == expected_text
        }));
    assert!(session["session"]["decision"].is_null(), "{session}");
    assert!(session["session"]["findings_red"].is_null(), "{session}");
    assert!(session["session"]["findings_yellow"].is_null(), "{session}");
    assert!(session["session"]["findings_green"].is_null(), "{session}");
}
