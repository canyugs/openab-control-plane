//! ADR 039 phase 2 live demo: two mock bots over the REAL gateway wire, the
//! REAL kernel controller surfaces (signed runtime-event webhooks in, action
//! API in), and the world controller in between — proving
//! delegate → open → terminal → unlock end-to-end.
//!
//! The only shortcuts are transport-level: the kernel's HTTPS event endpoint
//! is rewritten onto the world controller's loopback listener (signatures are
//! computed over path+query, so verification stays real), and the world
//! controller's action client speaks plain HTTP to the kernel's loopback
//! `/v1/controller/actions` with a real installed token.

use futures::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use openab_control_plane::controller_api::ControllerAuthConfig;
use openab_control_plane::controller_events::{
    spawn_dispatcher, ControllerEventKeys, ControllerEventRequest, ControllerEventRuntime,
    ControllerEventTransport,
};
use openab_control_plane::store::{SqliteStore, Store};
use openab_control_plane::{build_router, state::AppState};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use world_controller as wc;

const API_KEY: &str = "world-demo-root-key";
const CONTROLLER_ID: &str = "world-dev";
const SCOPE: &str = "tenant:demo/resource:world";
const EVENT_ENDPOINT: &str = "https://world-controller.test/api/v1/openab/events?version=1";

/// Rewrites the registered HTTPS endpoint's host onto the world controller's
/// loopback listener, preserving path+query (the signature target).
struct LoopbackEventTransport {
    world_addr: SocketAddr,
    client: reqwest::Client,
}

impl ControllerEventTransport for LoopbackEventTransport {
    fn post(&self, request: ControllerEventRequest) -> BoxFuture<'static, anyhow::Result<u16>> {
        let url = reqwest::Url::parse(&request.endpoint).unwrap();
        let target = format!(
            "http://{}{}{}",
            self.world_addr,
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        let client = self.client.clone();
        Box::pin(async move {
            let mut req = client.post(target).body(request.body);
            for (name, value) in request.headers {
                req = req.header(name, value);
            }
            Ok(req.send().await?.status().as_u16())
        })
    }
}

/// The world controller's action client, pointed at the kernel's loopback
/// action API with the real installed token — same envelope, headers, and
/// idempotency semantics as production, minus TLS.
struct LoopbackActionClient {
    kernel_addr: SocketAddr,
    action_token: String,
    client: reqwest::Client,
}

impl wc::ocp::OcpActionClient for LoopbackActionClient {
    fn execute(
        &self,
        action_id: String,
        action: controller_protocol::ControllerAction,
    ) -> wc::ocp::ActionFuture {
        let client = self.client.clone();
        let url = format!("http://{}/v1/controller/actions", self.kernel_addr);
        let token = self.action_token.clone();
        Box::pin(async move {
            let envelope = controller_protocol::ActionEnvelope {
                version: controller_protocol::CURRENT_VERSION,
                action_id: action_id.clone(),
                action,
            };
            let response = client
                .post(url)
                .bearer_auth(token)
                .header("X-OAB-Action-ID", &action_id)
                .header("X-OAB-Scope", SCOPE)
                .json(&envelope)
                .send()
                .await
                .map_err(|_| wc::ocp::ActionFailure::Unavailable)?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .map_err(|_| wc::ocp::ActionFailure::Unavailable)?;
            if status.is_success() {
                return serde_json::from_slice(&body)
                    .map_err(|_| wc::ocp::ActionFailure::InvalidResponse);
            }
            let error: controller_protocol::ErrorEnvelope = serde_json::from_slice(&body)
                .map_err(|_| wc::ocp::ActionFailure::InvalidResponse)?;
            Err(wc::ocp::ActionFailure::Protocol {
                status: status.as_u16(),
                code: error.error.code,
                retryable: error.error.retryable,
            })
        })
    }
}

#[tokio::test]
async fn two_bot_delegate_open_terminal_unlock_end_to_end() {
    // World controller listener first: the kernel's event transport needs its
    // address before the kernel state exists.
    let world_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let world_addr = world_listener.local_addr().unwrap();

    // Kernel plane with real controller auth + signed event delivery.
    let auth = ControllerAuthConfig::new(BTreeMap::from([(1, vec![1u8; 32])])).unwrap();
    let keys = ControllerEventKeys::new(BTreeMap::from([(1, vec![9u8; 32])])).unwrap();
    let events_runtime = Arc::new(ControllerEventRuntime::new(
        keys,
        Arc::new(LoopbackEventTransport {
            world_addr,
            client: reqwest::Client::new(),
        }),
    ));
    let store: Arc<dyn Store> = Arc::new(SqliteStore::memory().unwrap());
    let kernel_state = AppState::new_with_options_and_runtime_config(
        store,
        Some(API_KEY.into()),
        None,
        "http://control-plane.test".into(),
        None,
        0,
        openab_control_plane::plugins::council::CouncilConfig::default(),
        Some(auth),
        Some(events_runtime),
    );
    let kernel_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let kernel_addr = kernel_listener.local_addr().unwrap();
    let kernel_app = build_router(kernel_state.clone());
    tokio::spawn(async move {
        axum::serve(kernel_listener, kernel_app).await.unwrap();
    });
    spawn_dispatcher(kernel_state.clone());
    let base = kernel_addr.to_string();
    let http = reqwest::Client::new();

    // Install the world controller: action grants, scope, event grants.
    let installed: Value = http
        .post(format!("http://{base}/v1/controller-installations"))
        .bearer_auth(API_KEY)
        .json(&json!({
            "id": CONTROLLER_ID,
            "actions": ["open_session", "post_message"],
            "scopes": [SCOPE],
            "max_concurrent_sessions": 5,
            "max_actions_per_minute": 60,
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let action_token = installed["action_token"].as_str().unwrap().to_string();
    let events_config: Value = http
        .post(format!(
            "http://{base}/v1/controller-installations/{CONTROLLER_ID}/events"
        ))
        .bearer_auth(API_KEY)
        .json(&json!({
            "endpoint": EVENT_ENDPOINT,
            "events": ["session.intent", "session.terminal"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let event_signing_secret = events_config["event_signing_secret"]
        .as_str()
        .unwrap()
        .to_string();

    // The world controller itself, on the listener the kernel delivers to.
    let world_state = Arc::new(wc::AppState::with_components(
        wc::config::Config::from_values(|_| None),
        wc::store::Store::memory().unwrap(),
        Arc::new(LoopbackActionClient {
            kernel_addr,
            action_token,
            client: reqwest::Client::new(),
        }),
        wc::runtime_events::RuntimeEventVerifier::new(CONTROLLER_ID, &event_signing_secret)
            .unwrap(),
    ));
    let world_app = wc::router(world_state.clone());
    tokio::spawn(async move {
        axum::serve(world_listener, world_app).await.unwrap();
    });
    wc::spawn_scheduler_tick(world_state.clone(), Duration::from_secs(1));

    // Two mock bots on the real gateway wire.
    let (b0, b0_token) = register_bot(&base, "initiator", "initiator").await;
    let (b1, b1_token) = register_bot(&base, "worker", "worker").await;
    let (mut b0_w, mut b0_r) = connect(kernel_addr, &b0_token).await.split();
    let (mut b1_w, mut b1_r) = connect(kernel_addr, &b1_token).await.split();

    // B0's long-running session is opened by the world controller itself, as
    // the root task of the namespace: session.intent events are enqueued only
    // for the controller that OWNS the session (`controller_sessions`), so an
    // initiator delegating over intents must live in a controller-opened
    // session.
    world_state
        .store
        .insert_task(&wc::store::Task {
            ns: "demo".into(),
            id: "root".into(),
            assignee: b0.clone(),
            deps: vec![],
            status: wc::store::TaskStatus::Pending,
            spec: "Coordinate the demo tasks.".into(),
            result: None,
            created_by: "operator".into(),
            session_id: None,
        })
        .unwrap()
        .unwrap();
    wc::run_scheduler(&world_state, "demo").await;

    let trigger = read_bot_event(&mut b0_r).await;
    assert_eq!(trigger["content"]["text"], "Coordinate the demo tasks.");
    let b0_session = trigger["channel"]["id"].as_str().unwrap().to_string();

    b0_w.send(reply(
        &b0_session,
        &format!("Splitting the work.\n[[intent:delegate to={b1} task=\"write a haiku about dependency graphs\" id=t1 ns=demo]]"),
    ))
    .await
    .unwrap();
    let ack = wait_for_world_reply(&mut b0_r).await;
    assert_eq!(
        ack["content"]["text"],
        format!("[world] task demo/t1 accepted for {b1}")
    );

    b0_w.send(reply(
        &b0_session,
        &format!("And the follow-up.\n[[intent:delegate to={b1} task=\"review the haiku\" id=t2 deps=t1 ns=demo]]"),
    ))
    .await
    .unwrap();
    let ack = wait_for_world_reply(&mut b0_r).await;
    assert_eq!(
        ack["content"]["text"],
        format!("[world] task demo/t2 accepted for {b1}")
    );

    // delegate → open: the worker is triggered in a NEW session with t1's spec.
    let t1_trigger = read_bot_event(&mut b1_r).await;
    assert_eq!(
        t1_trigger["content"]["text"],
        "write a haiku about dependency graphs"
    );
    let t1_session = t1_trigger["channel"]["id"].as_str().unwrap().to_string();
    assert_ne!(t1_session, b0_session);

    // t2 is gated while t1 runs.
    let t2 = world_state.store.task("demo", "t2").unwrap().unwrap();
    assert_eq!(t2.status, wc::store::TaskStatus::Pending);

    // terminal → unlock: the worker finishes t1; its terminal event settles
    // the task and the scheduler opens t2.
    b1_w.send(reply(
        &t1_session,
        "graphs hold every task\nedges wait for edges done\nnothing runs alone [done]",
    ))
    .await
    .unwrap();

    let t2_trigger = read_bot_event(&mut b1_r).await;
    assert_eq!(t2_trigger["content"]["text"], "review the haiku");
    let t2_session = t2_trigger["channel"]["id"].as_str().unwrap().to_string();
    assert_ne!(t2_session, t1_session);

    let t1 = wait_for_task_status(&world_state, "demo", "t1", wc::store::TaskStatus::Done).await;
    assert!(t1.result.unwrap().contains("nothing runs alone"));

    b1_w.send(reply(&t2_session, "ship it. [done]"))
        .await
        .unwrap();
    wait_for_task_status(&world_state, "demo", "t2", wc::store::TaskStatus::Done).await;

    // Any agent can query progress from its own session.
    b0_w.send(reply(
        &b0_session,
        "Where are we?\n[[intent:status ns=demo]]",
    ))
    .await
    .unwrap();
    let status = wait_for_world_reply(&mut b0_r).await;
    assert_eq!(
        status["content"]["text"],
        "[world] demo: root=running t1=done t2=done"
    );
}

async fn wait_for_task_status(
    state: &wc::AppState,
    ns: &str,
    id: &str,
    status: wc::store::TaskStatus,
) -> wc::store::Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(task) = state.store.task(ns, id).unwrap() {
            if task.status == status {
                return task;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {ns}/{id} never reached {}",
            status.as_str()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The next `[world]`-prefixed message on this bot's wire.
async fn wait_for_world_reply<R>(r: &mut R) -> Value
where
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a [world] reply"
        );
        let event = read_bot_event(r).await;
        if event["content"]["text"]
            .as_str()
            .is_some_and(|text| text.starts_with("[world]"))
        {
            return event;
        }
    }
}

async fn register_bot(base: &str, name: &str, role: &str) -> (String, String) {
    let value: Value = reqwest::Client::new()
        .post(format!("http://{base}/v1/bots"))
        .bearer_auth(API_KEY)
        .json(&json!({ "name": name, "role": role }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        value["bot_id"].as_str().unwrap().to_string(),
        value["token"].as_str().unwrap().to_string(),
    )
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
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
