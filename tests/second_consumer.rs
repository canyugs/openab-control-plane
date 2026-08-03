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

const REPORT_QUESTION: &str = "REPORT-QUESTION: Summarize what happened during the outage.";
const REPORT_PART_1: &str = "REPORT-PART-1: The outage began at 09:00 with elevated error rates.";
const REPORT_PART_2: &str = "REPORT-PART-2: The root cause was a misconfigured cache layer.";
const REPORT_PART_3: &str = "REPORT-PART-3: Mitigation is deployed and traffic recovered. [done]";

const TIMEOUT_TRIGGER_REF: &str = "forum:s16-watchdog-timeout-proof";
const TIMEOUT_PROMPT: &str = "TIMEOUT-PROMPT: Two responders, one of whom will go silent.";
const TIMEOUT_PARTIAL_REPORT: &str =
    "TIMEOUT-PARTIAL: The cache layer looks misconfigured; check the TTL. [done]";

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

/// ADR 028 second-consumer proof: a bot's long answer arrives as several
/// chunks (plus a streaming stub); the close records WHICH messages are the
/// result, and `/result` serves the joined text — no client-side join.
#[tokio::test]
async fn chunked_answer_result_is_identified_at_close_and_joinable() {
    let (addr, _state) = spawn_server().await;
    let base = addr.to_string();

    let (bot_id, token) = register_bot(&base, "report-writer", "responder").await;
    let ws = connect(addr, &token).await;
    let (mut w, mut r) = ws.split();

    let session = open_solo_session(&base, &bot_id, REPORT_QUESTION).await;

    let trigger = read_bot_event(&mut r).await;
    assert_eq!(trigger["content"]["text"], REPORT_QUESTION);

    // Before any result exists, both read surfaces say null — the same shape a
    // legacy (pre-ADR 028) session presents.
    let open_detail = get_session(&base, &session).await;
    assert!(open_detail["result"].is_null(), "{open_detail}");
    let open_result = get_json(&base, &format!("v1/sessions/{session}/result")).await;
    assert!(open_result["result"].is_null(), "{open_result}");

    w.send(reply(&session, REPORT_PART_1)).await.unwrap();
    w.send(reply(&session, "…")).await.unwrap(); // streaming stub — not part of the result
    w.send(reply(&session, REPORT_PART_2)).await.unwrap();
    w.send(reply(&session, REPORT_PART_3)).await.unwrap();

    let closed = wait_for_state(&base, &session, "closed").await;

    // Detail response carries the result identity: the settling author and the
    // three chunk ids, oldest→newest (the stub is excluded).
    assert_eq!(closed["result"]["author_id"], bot_id);
    let ids: Vec<&str> = closed["result"]["message_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "{closed}");
    let contents: Vec<&str> = ids
        .iter()
        .map(|id| {
            closed["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == *id)
                .unwrap_or_else(|| panic!("result id {id} not in messages"))["content"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert_eq!(contents, vec![REPORT_PART_1, REPORT_PART_2, REPORT_PART_3]);

    // The result route serves the full joined text — the whole answer, not the
    // last chunk — under the same {"result": …} envelope as the absent case.
    let full_text = format!("{REPORT_PART_1}\n{REPORT_PART_2}\n{REPORT_PART_3}");
    let result = get_json(&base, &format!("v1/sessions/{session}/result")).await;
    assert_eq!(result["result"]["author_id"], bot_id);
    assert_eq!(
        result["result"]["message_ids"],
        closed["result"]["message_ids"]
    );
    assert_eq!(result["result"]["text"], full_text);

    // A post-close edit is rejected: the recorded result text is immutable.
    // The follow-up reaction IS processed (same socket, in order), so once it
    // is visible the edit attempt has definitely been handled.
    w.send(edit_reply(&session, ids[0], "TAMPERED"))
        .await
        .unwrap();
    w.send(reaction_reply(&session, ids[0], "👀"))
        .await
        .unwrap();
    let detail = wait_for_reaction(&base, &session, "👀").await;
    let first_chunk = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == ids[0])
        .unwrap();
    assert_eq!(
        first_chunk["content"], REPORT_PART_1,
        "post-close edit must not apply"
    );
    let result = get_json(&base, &format!("v1/sessions/{session}/result")).await;
    assert_eq!(
        result["result"]["text"], full_text,
        "result text is immutable after close"
    );
}

/// ADR 031 invariant #10, timeout half: a consumer that never touches GitHub
/// must be able to see a watchdog close, and everything it needs must be on
/// the north surface. The success/terminal/reopen halves are above; this is
/// the path where a bot goes silent and only the plane can end the session.
#[tokio::test]
async fn watchdog_timeout_is_legible_on_the_north_surface_alone() {
    let (addr, state) = spawn_server().await;
    let base = addr.to_string();
    let mut north_rx = state.north_tx.subscribe();

    let (chair_id, chair_token) = register_bot(&base, "forum-chair", "chair").await;
    let (speaker_id, speaker_token) = register_bot(&base, "forum-speaker", "reviewer").await;
    let (silent_id, silent_token) = register_bot(&base, "forum-silent", "reviewer").await;

    // Connect all three first: a bot that never joined would be absent for an
    // uninteresting reason. These are live and simply do not all finish.
    let mut chair_ws = connect(addr, &chair_token).await;
    let mut speaker_ws = connect(addr, &speaker_token).await;
    let mut silent_ws = connect(addr, &silent_token).await;

    let session_id = open_council_session(
        &base,
        &[&chair_id, &speaker_id, &silent_id],
        &chair_id,
        TIMEOUT_PROMPT,
    )
    .await;
    for ws in [&mut chair_ws, &mut speaker_ws, &mut silent_ws] {
        let event = read_bot_event(ws).await;
        assert_eq!(event["event_type"], "message");
    }

    // One reviewer delivers, the other never does — the session cannot reach
    // quorum on its own and would hang forever without the watchdog.
    speaker_ws
        .send(reply(&session_id, TIMEOUT_PARTIAL_REPORT))
        .await
        .unwrap();
    // Wait for the report to be *processed*, not merely sent: the session is
    // already `deliberating` before the reply, so gating on state races the
    // done-vote and the close would count 0/3 (caught by CI, not locally).
    wait_for_message(&base, &session_id, TIMEOUT_PARTIAL_REPORT).await;

    // Drive the watchdog's own entry point rather than waiting out a real
    // deadline. A cutoff that predates the report must NOT close: the staleness
    // predicate in the close CAS is what lets a landing verdict beat the
    // watchdog, and it has to hold on the provider-neutral path too.
    let deferred = openab_control_plane::orchestrator::force_close_timeout(
        &state,
        &session_id,
        openab_control_plane::store::now_ms() - 60_000,
    )
    .unwrap();
    assert!(
        !deferred,
        "a session with activity newer than the cutoff is not stale"
    );
    assert_eq!(
        get_session(&base, &session_id).await["session"]["state"],
        "deliberating",
        "the deferred scan left the session running"
    );

    let closed = openab_control_plane::orchestrator::force_close_timeout(
        &state,
        &session_id,
        openab_control_plane::store::now_ms() + 1,
    )
    .unwrap();
    assert!(closed, "the watchdog performed the close");

    let event = wait_for_north_event(&mut north_rx, &session_id, "timeout").await;
    let payload = &event["payload"];
    assert_eq!(payload["reason"], "timeout");
    assert_eq!(payload["done"], 1);
    assert_eq!(payload["total"], 3);
    assert_eq!(
        payload["absent"],
        json!([chair_id, silent_id]),
        "the chair never synthesized and the silent reviewer never reported"
    );
    assert_eq!(payload["trigger_ref"], TIMEOUT_TRIGGER_REF);

    let session = get_session(&base, &session_id).await;
    assert_eq!(session["session"]["state"], "closed");
    // The close reason lives on the north event, not on the session row — a
    // consumer learns why a session ended by subscribing, which is the whole
    // point of the surface being sufficient.
    let verdict = payload["verdict"].as_str().unwrap();
    assert!(
        verdict.starts_with("TIMEOUT: session closed by watchdog"),
        "verdict names the close reason: {verdict}"
    );
    assert!(
        verdict.contains(&silent_id),
        "the silent bot is named as absent: {verdict}"
    );
    assert!(
        !verdict.contains(&speaker_id),
        "a bot that delivered is not listed absent: {verdict}"
    );
    assert!(
        verdict.contains("1/3 signaled done"),
        "the verdict counts the delivered reports: {verdict}"
    );
    assert!(
        verdict.contains("No verdict synthesized"),
        "no chair synthesis is claimed: {verdict}"
    );

    // A timeout never guesses a result — the delivered report stays in the
    // thread, but nothing is promoted to the session result.
    assert!(
        session["result"].is_null(),
        "timeout close records no result: {}",
        session["result"]
    );
    let messages = session["messages"].as_array().unwrap();
    assert!(
        messages
            .iter()
            .any(|m| m["content"].as_str().unwrap_or_default() == TIMEOUT_PARTIAL_REPORT),
        "the one delivered report survives the close"
    );

    // Once-only: a second scan (or a normal close racing in) is a no-op.
    let again = openab_control_plane::orchestrator::force_close_timeout(
        &state,
        &session_id,
        openab_control_plane::store::now_ms() + 1,
    )
    .unwrap();
    assert!(!again, "the close CAS fires exactly once");

    // Nothing in this flow named a provider: the trigger_ref is forum-shaped
    // and every read above came from the north API or the event stream.
    assert_eq!(session["session"]["trigger_ref"], TIMEOUT_TRIGGER_REF);
}

async fn open_council_session(
    base: &str,
    roster: &[&str],
    chair_bot: &str,
    prompt: &str,
) -> String {
    let response = reqwest::Client::new()
        .post(format!("http://{base}/v1/sessions"))
        .json(&json!({
            "title": "forum-shaped council that never finishes",
            "trigger_ref": TIMEOUT_TRIGGER_REF,
            "roster": roster,
            "chair_bot": chair_bot,
            "quorum_n": 2,
            "mode": "council",
            "prompt": prompt,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value: Value = response.json().await.unwrap();
    value["session_id"].as_str().unwrap().to_string()
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
    get_json(base, &format!("v1/sessions/{session_id}")).await
}

async fn get_json(base: &str, path: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("http://{base}/{path}"))
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

/// Poll until a message with this exact content is visible on the session.
/// The done-vote is recorded in the same handler, so its arrival means the
/// reply has been fully applied.
async fn wait_for_message(base: &str, session_id: &str, content: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        let session = get_session(base, session_id).await;
        if session["messages"]
            .as_array()
            .is_some_and(|messages| {
                messages
                    .iter()
                    .any(|m| m["content"].as_str() == Some(content))
            })
        {
            return session;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for message to be applied: {content}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
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

fn edit_reply(session: &str, target: &str, content: &str) -> Message {
    Message::Text(
        json!({
            "channel": { "id": session },
            "content": { "type": "text", "text": content },
            "command": "edit_message",
            "reply_to": target,
        })
        .to_string(),
    )
}

fn reaction_reply(session: &str, target: &str, emoji: &str) -> Message {
    Message::Text(
        json!({
            "channel": { "id": session },
            "content": { "type": "text", "text": emoji },
            "command": "add_reaction",
            "reply_to": target,
        })
        .to_string(),
    )
}

async fn wait_for_reaction(base: &str, session_id: &str, emoji: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        let detail = get_session(base, session_id).await;
        let seen = detail["reactions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["emoji"] == emoji);
        if seen {
            return detail;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "reaction {emoji} never appeared: {detail}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
