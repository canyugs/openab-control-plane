//! World controller (ADR 039 phase 2): an external controller (ADR 008)
//! owning the cross-session task DAG for the agent-world profile.
//!
//! It consumes `session.intent` (delegate / status) and `session.terminal`
//! from the kernel's signed runtime-event webhook, keeps `tasks` in its own
//! store, and effects everything through ordinary controller actions:
//! `open_session` when a task's deps are all done, `post_message` to answer
//! the asking session. Scheduling itself is deterministic controller code —
//! zero tokens.

pub mod config;
pub mod intent;
pub mod ocp;
pub mod runtime_events;
pub mod store;

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::Config;
use controller_protocol::{
    ControllerAction, ControllerActionResult, OpenSessionAction, PostMessageAction,
};
use intent::Intent;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use store::{now_unix, EventAdmission, Store, Task, TaskStatus};

pub struct AppState {
    pub config: Config,
    pub store: Store,
    pub action_client: Arc<dyn ocp::OcpActionClient>,
    pub event_verifier: runtime_events::RuntimeEventVerifier,
}

impl AppState {
    /// Fail-fast construction: a misconfigured controller exits instead of
    /// serving not-ready forever.
    pub fn from_config(config: Config) -> anyhow::Result<Self> {
        let store = Store::open(&config.db_path)?;
        let action_client = Arc::new(ocp::ReqwestOcpActionClient::new(&config.ocp_action)?);
        let controller_id = config
            .ocp_action
            .controller_id
            .as_deref()
            .unwrap_or_default();
        let secret = config
            .event_signing_secret
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("WORLD_CONTROLLER_EVENT_SIGNING_SECRET is required"))?;
        let event_verifier = runtime_events::RuntimeEventVerifier::new(controller_id, secret)?;
        Ok(Self {
            config,
            store,
            action_client,
            event_verifier,
        })
    }

    pub fn with_components(
        config: Config,
        store: Store,
        action_client: Arc<dyn ocp::OcpActionClient>,
        event_verifier: runtime_events::RuntimeEventVerifier,
    ) -> Self {
        Self {
            config,
            store,
            action_client,
            event_verifier,
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/v1/openab/events", post(handle_runtime_event))
        .with_state(state)
}

/// Periodic scheduler tick: the retry path for an `open_session` that failed
/// while its event was already recorded (event redelivery would dedupe, so
/// nothing else would re-run the scheduler until the next terminal event).
pub fn spawn_scheduler_tick(state: Arc<AppState>, every: std::time::Duration) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match state.store.pending_namespaces() {
                Ok(namespaces) => {
                    for ns in namespaces {
                        run_scheduler(&state, &ns).await;
                    }
                }
                Err(error) => tracing::error!(%error, "pending-namespace scan failed"),
            }
        }
    });
}

fn response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

async fn handle_runtime_event(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let target = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(uri.path());
    let event = match state.event_verifier.verify(
        header(&headers, "x-oab-controller-id"),
        header(&headers, "x-oab-event-id"),
        header(&headers, "x-oab-timestamp"),
        header(&headers, "x-oab-signature"),
        target,
        &body,
        now_unix(),
    ) {
        Ok(event) => event,
        Err(error) => {
            let status = match error {
                runtime_events::VerificationError::InvalidSignature
                | runtime_events::VerificationError::StaleTimestamp => StatusCode::FORBIDDEN,
                _ => StatusCode::BAD_REQUEST,
            };
            return response(status, json!({"ok": false, "error": error.public_code()}));
        }
    };
    let body_hash = hex::encode(Sha256::digest(&body));
    match state
        .store
        .record_runtime_event(&event.event_id, &body_hash)
    {
        Ok(EventAdmission::New) => {
            tracing::info!(
                event_id = event.event_id,
                event_type = event.event_type,
                session_id = event.session_id,
                "accepted signed runtime event"
            );
            // Processed after the event is recorded: a redelivery dedupes, so
            // consequences must not depend on this request succeeding twice.
            // Failed open_session calls are retried by the scheduler tick; a
            // failed status reply is lost (the bot can re-ask).
            process_event(&state, &event).await;
            response(StatusCode::OK, json!({"ok": true, "duplicate": false}))
        }
        Ok(EventAdmission::Duplicate) => {
            response(StatusCode::OK, json!({"ok": true, "duplicate": true}))
        }
        Ok(EventAdmission::Conflict) => response(
            StatusCode::CONFLICT,
            json!({"ok": false, "error": "runtime_event_payload_conflict"}),
        ),
        Err(error) => {
            tracing::error!(%error, "runtime-event receipt persistence failed");
            response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "runtime_event_store_failed"}),
            )
        }
    }
}

/// Dispatch one admitted event. Public for tests and reuse; idempotent under
/// replays because every consequence carries a deterministic action id.
pub async fn process_event(state: &AppState, event: &runtime_events::RuntimeEventEnvelope) {
    match event.event_type.as_str() {
        "session.intent" => dispatch_intent(state, event).await,
        "session.terminal" => dispatch_terminal(state, event).await,
        _ => {}
    }
}

async fn dispatch_intent(state: &AppState, event: &runtime_events::RuntimeEventEnvelope) {
    let Some(session_id) = event.session_id.as_deref() else {
        return;
    };
    let bot_id = event.payload["bot_id"].as_str().unwrap_or("");
    let verb = event.payload["verb"].as_str().unwrap_or("");
    let args = event.payload["args"].as_str().unwrap_or("");
    match intent::parse(verb, args) {
        // The kernel transports any verb; unknown ones are logged and ignored.
        Ok(None) => {
            tracing::info!(verb, bot_id, session_id, "ignoring unknown intent verb");
        }
        Err(error) => {
            tracing::info!(verb, bot_id, %error, "rejecting malformed intent");
            reply(
                state,
                &event.event_id,
                session_id,
                &format!("[world] {verb} rejected: {error}"),
            )
            .await;
        }
        Ok(Some(Intent::Delegate {
            ns,
            id,
            to,
            spec,
            deps,
        })) => {
            // Caller-chosen id, else minted from the (unique) event id so a
            // bot that skips id= still gets a referenceable task.
            let id = id.unwrap_or_else(|| sanitize_name(&format!("t-{}", event.event_id)));
            let task = Task {
                ns: ns.clone(),
                id: id.clone(),
                assignee: to.clone(),
                deps,
                status: TaskStatus::Pending,
                spec,
                result: None,
                created_by: bot_id.to_string(),
                session_id: None,
            };
            match state.store.insert_task(&task) {
                Ok(Ok(())) => {
                    reply(
                        state,
                        &event.event_id,
                        session_id,
                        &format!("[world] task {ns}/{id} accepted for {to}"),
                    )
                    .await;
                    run_scheduler(state, &ns).await;
                }
                Ok(Err(error)) => {
                    reply(
                        state,
                        &event.event_id,
                        session_id,
                        &format!("[world] delegate rejected: {error}"),
                    )
                    .await;
                }
                Err(error) => tracing::error!(%error, ns, id, "task insert failed"),
            }
        }
        Ok(Some(Intent::Status { ns, task })) => {
            let content = match render_status(state, &ns, task.as_deref()) {
                Ok(content) => content,
                Err(error) => {
                    tracing::error!(%error, ns, "status query failed");
                    return;
                }
            };
            reply(state, &event.event_id, session_id, &content).await;
        }
    }
}

fn render_status(state: &AppState, ns: &str, task: Option<&str>) -> rusqlite::Result<String> {
    if let Some(id) = task {
        return Ok(match state.store.task(ns, id)? {
            Some(task) => {
                let mut line = format!(
                    "[world] task {ns}/{id}: {} (assignee {}",
                    task.status.as_str(),
                    task.assignee
                );
                if !task.deps.is_empty() {
                    line.push_str(&format!(", deps {}", task.deps.join(",")));
                }
                line.push(')');
                if let Some(result) = task.result.as_deref() {
                    line.push_str(" — ");
                    line.push_str(truncate(result, 400));
                }
                line
            }
            None => format!("[world] no task '{id}' in ns '{ns}'"),
        });
    }
    let tasks = state.store.tasks_in_ns(ns)?;
    if tasks.is_empty() {
        return Ok(format!("[world] {ns}: no tasks"));
    }
    let summary: Vec<String> = tasks
        .iter()
        .map(|t| format!("{}={}", t.id, t.status.as_str()))
        .collect();
    Ok(format!("[world] {ns}: {}", summary.join(" ")))
}

fn truncate(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

async fn dispatch_terminal(state: &AppState, event: &runtime_events::RuntimeEventEnvelope) {
    let Some(session_id) = event.session_id.as_deref() else {
        return;
    };
    // Sessions this controller did not open (e.g. the delegator's own) map to
    // no task and are ignored.
    let task = match state.store.task_by_session(session_id) {
        Ok(Some(task)) => task,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(%error, session_id, "task lookup by session failed");
            return;
        }
    };
    let reason = event.payload["reason"].as_str().unwrap_or("");
    let (status, result) = if reason == "normal" {
        let joined = event.payload["final_messages"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        (TaskStatus::Done, joined)
    } else {
        (TaskStatus::Failed, format!("session closed: {reason}"))
    };
    match state
        .store
        .mark_terminal(&task.ns, &task.id, status, Some(&result))
    {
        Ok(true) => {
            tracing::info!(
                ns = task.ns,
                id = task.id,
                status = status.as_str(),
                "task settled by session terminal"
            );
            run_scheduler(state, &task.ns).await;
        }
        Ok(false) => {}
        Err(error) => tracing::error!(%error, session_id, "task terminal update failed"),
    }
}

/// The DAG scheduler, one sentence: all deps done → open the task's session.
/// Also settles the negative case — a failed dep fails its dependents, so a
/// chain never waits on a task that can no longer happen.
pub async fn run_scheduler(state: &AppState, ns: &str) {
    loop {
        let tasks = match state.store.tasks_in_ns(ns) {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::error!(%error, ns, "scheduler task scan failed");
                return;
            }
        };
        let status_of = |id: &str| tasks.iter().find(|t| t.id == id).map(|t| t.status);
        let mut changed = false;
        for task in tasks.iter().filter(|t| t.status == TaskStatus::Pending) {
            let failed_dep = task
                .deps
                .iter()
                .find(|dep| status_of(dep).is_none_or(|s| s == TaskStatus::Failed));
            if let Some(dep) = failed_dep {
                let note = format!("dependency '{dep}' failed");
                match state
                    .store
                    .mark_terminal(ns, &task.id, TaskStatus::Failed, Some(&note))
                {
                    Ok(true) => changed = true,
                    Ok(false) => {}
                    Err(error) => {
                        tracing::error!(%error, ns, id = task.id, "dep-failure mark failed")
                    }
                }
                continue;
            }
            if !task
                .deps
                .iter()
                .all(|dep| status_of(dep) == Some(TaskStatus::Done))
            {
                continue;
            }
            match open_task_session(state, task).await {
                Ok(session_id) => {
                    if let Err(error) = state.store.mark_running(ns, &task.id, &session_id) {
                        tracing::error!(%error, ns, id = task.id, "mark-running failed");
                    } else {
                        tracing::info!(ns, id = task.id, session_id, "task session opened");
                        changed = true;
                    }
                }
                // Non-retryable rejection (unknown bot, invalid params): the
                // task can never open — settle it so dependents fail fast.
                Err(ocp::ActionFailure::Protocol {
                    retryable: false,
                    status,
                    code,
                }) => {
                    let note = format!("open_session rejected (http {status}, {code:?})");
                    tracing::warn!(ns, id = task.id, note, "task failed to open");
                    match state
                        .store
                        .mark_terminal(ns, &task.id, TaskStatus::Failed, Some(&note))
                    {
                        Ok(true) => changed = true,
                        Ok(false) => {}
                        Err(error) => {
                            tracing::error!(%error, ns, id = task.id, "open-failure mark failed")
                        }
                    }
                }
                // Transient: leave pending, the scheduler tick retries with
                // the same deterministic action id.
                Err(error) => {
                    tracing::warn!(%error, ns, id = task.id, "open_session unavailable; will retry");
                }
            }
        }
        if !changed {
            return;
        }
    }
}

async fn open_task_session(state: &AppState, task: &Task) -> Result<String, ocp::ActionFailure> {
    // Task identity rides the controller's own store plus the opaque
    // trigger ref — never kernel session metadata (ADR 039 §4).
    let action_id = format!("open:{}:{}", task.ns, task.id);
    let action = ControllerAction::OpenSession(OpenSessionAction {
        title: format!("task {}/{}", task.ns, task.id),
        trigger_ref: Some(format!("task:{}:{}", task.ns, task.id)),
        trigger_fingerprint: None,
        roster: vec![task.assignee.clone()],
        quorum_n: 0,
        chair_bot: Some(task.assignee.clone()),
        mode: "solo".into(),
        prompt: task.spec.clone(),
        recipient_inputs: Default::default(),
    });
    let result = state.action_client.execute(action_id, action).await?;
    match result.result {
        ControllerActionResult::SessionOpened { session_id, .. } => Ok(session_id),
        _ => Err(ocp::ActionFailure::InvalidResponse),
    }
}

/// Post a reply into the asking session. Best-effort: the action id is
/// deterministic per event, so a replayed event cannot double-post, and a
/// lost reply is recoverable by asking again.
async fn reply(state: &AppState, event_id: &str, session_id: &str, content: &str) {
    let action = ControllerAction::PostMessage(PostMessageAction {
        session_id: session_id.to_string(),
        content: content.to_string(),
    });
    if let Err(error) = state
        .action_client
        .execute(format!("reply:{event_id}"), action)
        .await
    {
        tracing::warn!(%error, session_id, "reply post_message failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use controller_protocol::ActionResultEnvelope;
    use std::sync::Mutex;

    /// Records every executed action; opens sessions as `ses_<action_id>`.
    struct MockClient {
        executed: Mutex<Vec<(String, ControllerAction)>>,
        fail_open: Mutex<Option<ocp::ActionFailure>>,
    }

    impl MockClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                executed: Mutex::new(Vec::new()),
                fail_open: Mutex::new(None),
            })
        }

        fn actions(&self) -> Vec<(String, ControllerAction)> {
            self.executed.lock().unwrap().clone()
        }

        fn posted_messages(&self) -> Vec<PostMessageAction> {
            self.actions()
                .into_iter()
                .filter_map(|(_, action)| match action {
                    ControllerAction::PostMessage(post) => Some(post),
                    _ => None,
                })
                .collect()
        }

        fn opened(&self) -> Vec<(String, OpenSessionAction)> {
            self.actions()
                .into_iter()
                .filter_map(|(id, action)| match action {
                    ControllerAction::OpenSession(open) => Some((id, open)),
                    _ => None,
                })
                .collect()
        }
    }

    impl ocp::OcpActionClient for MockClient {
        fn execute(&self, action_id: String, action: ControllerAction) -> ocp::ActionFuture {
            if let ControllerAction::OpenSession(_) = &action {
                if let Some(failure) = self.fail_open.lock().unwrap().clone() {
                    return Box::pin(async move { Err(failure) });
                }
            }
            self.executed
                .lock()
                .unwrap()
                .push((action_id.clone(), action.clone()));
            Box::pin(async move {
                let result = match action {
                    ControllerAction::OpenSession(_) => ControllerActionResult::SessionOpened {
                        session_id: format!("ses_{action_id}"),
                        deduped: false,
                    },
                    ControllerAction::PostMessage(_) => ControllerActionResult::MessagePosted {
                        message_id: format!("msg_{action_id}"),
                    },
                    _ => unreachable!("world controller only opens sessions and posts messages"),
                };
                Ok(ActionResultEnvelope {
                    version: controller_protocol::CURRENT_VERSION,
                    action_id,
                    result,
                })
            })
        }
    }

    fn state_with_mock() -> (Arc<AppState>, Arc<MockClient>) {
        let client = MockClient::new();
        let config = Config::from_values(|_| None);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "world-test",
            &base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, [7u8; 32]),
        )
        .unwrap();
        let state =
            AppState::with_components(config, Store::memory().unwrap(), client.clone(), verifier);
        (Arc::new(state), client)
    }

    fn intent_event(
        event_id: &str,
        session_id: &str,
        verb: &str,
        args: &str,
    ) -> runtime_events::RuntimeEventEnvelope {
        runtime_events::RuntimeEventEnvelope {
            version: "1".into(),
            event_id: event_id.into(),
            controller_id: "world-test".into(),
            event_type: "session.intent".into(),
            session_id: Some(session_id.into()),
            occurred_at: 1_000,
            payload: json!({
                "message_id": format!("msg_{event_id}"),
                "bot_id": "b0",
                "verb": verb,
                "args": args,
            }),
        }
    }

    fn terminal_event(
        event_id: &str,
        session_id: &str,
        reason: &str,
        final_messages: &[&str],
    ) -> runtime_events::RuntimeEventEnvelope {
        runtime_events::RuntimeEventEnvelope {
            version: "1".into(),
            event_id: event_id.into(),
            controller_id: "world-test".into(),
            event_type: "session.terminal".into(),
            session_id: Some(session_id.into()),
            occurred_at: 2_000,
            payload: json!({
                "state": "closed",
                "reason": reason,
                "final_messages": final_messages,
            }),
        }
    }

    #[tokio::test]
    async fn delegate_intent_inserts_task_and_opens_session_when_unblocked() {
        let (state, client) = state_with_mock();
        process_event(
            &state,
            &intent_event(
                "cev_1",
                "ses_b0",
                "delegate",
                "to=b1 task=\"research the topic\" id=research ns=demo",
            ),
        )
        .await;

        let task = state.store.task("demo", "research").unwrap().unwrap();
        assert_eq!(task.assignee, "b1");
        assert_eq!(task.spec, "research the topic");
        assert_eq!(task.created_by, "b0");
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.session_id.as_deref(), Some("ses_open:demo:research"));

        let opened = client.opened();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].0, "open:demo:research");
        assert_eq!(opened[0].1.mode, "solo");
        assert_eq!(opened[0].1.roster, vec!["b1".to_string()]);
        assert_eq!(opened[0].1.prompt, "research the topic");
        assert_eq!(
            opened[0].1.trigger_ref.as_deref(),
            Some("task:demo:research")
        );

        let replies = client.posted_messages();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].session_id, "ses_b0");
        assert!(replies[0].content.contains("task demo/research accepted"));
    }

    #[tokio::test]
    async fn deps_gate_until_terminal_then_unlock() {
        let (state, client) = state_with_mock();
        process_event(
            &state,
            &intent_event(
                "cev_1",
                "ses_b0",
                "delegate",
                "to=b1 task=\"step one\" id=b1 ns=demo",
            ),
        )
        .await;
        process_event(
            &state,
            &intent_event(
                "cev_2",
                "ses_b0",
                "delegate",
                "to=b2 task=\"step two\" id=b2 deps=b1 ns=demo",
            ),
        )
        .await;
        process_event(
            &state,
            &intent_event(
                "cev_3",
                "ses_b0",
                "delegate",
                "to=b3 task=\"step three\" id=b3 deps=b1 ns=demo",
            ),
        )
        .await;

        // Only b1 opened; b2/b3 gated on it.
        assert_eq!(client.opened().len(), 1);
        assert_eq!(
            state.store.task("demo", "b2").unwrap().unwrap().status,
            TaskStatus::Pending
        );

        process_event(
            &state,
            &terminal_event(
                "cev_4",
                "ses_open:demo:b1",
                "normal",
                &["all findings written. [done]"],
            ),
        )
        .await;

        let b1 = state.store.task("demo", "b1").unwrap().unwrap();
        assert_eq!(b1.status, TaskStatus::Done);
        assert_eq!(b1.result.as_deref(), Some("all findings written. [done]"));

        let opened: Vec<String> = client.opened().into_iter().map(|(id, _)| id).collect();
        assert_eq!(opened, vec!["open:demo:b1", "open:demo:b2", "open:demo:b3"]);
        assert_eq!(
            state.store.task("demo", "b2").unwrap().unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            state.store.task("demo", "b3").unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn status_intent_replies_into_the_asking_session() {
        let (state, client) = state_with_mock();
        process_event(
            &state,
            &intent_event(
                "cev_1",
                "ses_b0",
                "delegate",
                "to=b1 task=\"step one\" id=b1 ns=demo",
            ),
        )
        .await;
        process_event(
            &state,
            &intent_event("cev_2", "ses_ask", "status", "ns=demo"),
        )
        .await;
        process_event(
            &state,
            &intent_event("cev_3", "ses_ask", "status", "task=b1 ns=demo"),
        )
        .await;
        process_event(
            &state,
            &intent_event("cev_4", "ses_ask", "status", "task=ghost ns=demo"),
        )
        .await;

        let replies = client.posted_messages();
        assert_eq!(replies[1].session_id, "ses_ask");
        assert_eq!(replies[1].content, "[world] demo: b1=running");
        assert!(replies[2].content.contains("task demo/b1: running"));
        assert!(replies[3].content.contains("no task 'ghost'"));
    }

    #[tokio::test]
    async fn unknown_verbs_are_ignored_and_bad_args_rejected() {
        let (state, client) = state_with_mock();
        process_event(
            &state,
            &intent_event("cev_1", "ses_b0", "teleport", "to=mars"),
        )
        .await;
        assert!(client.actions().is_empty());
        assert!(state.store.tasks_in_ns("default").unwrap().is_empty());

        process_event(
            &state,
            &intent_event("cev_2", "ses_b0", "delegate", "task=\"nobody to do it\""),
        )
        .await;
        let replies = client.posted_messages();
        assert_eq!(replies.len(), 1);
        assert!(replies[0].content.contains("delegate rejected"));
        assert!(state.store.tasks_in_ns("default").unwrap().is_empty());
    }

    #[tokio::test]
    async fn abnormal_terminal_fails_task_and_dependents() {
        let (state, client) = state_with_mock();
        process_event(
            &state,
            &intent_event(
                "cev_1",
                "ses_b0",
                "delegate",
                "to=b1 task=\"step one\" id=b1 ns=demo",
            ),
        )
        .await;
        process_event(
            &state,
            &intent_event(
                "cev_2",
                "ses_b0",
                "delegate",
                "to=b2 task=\"step two\" id=b2 deps=b1 ns=demo",
            ),
        )
        .await;

        process_event(
            &state,
            &terminal_event("cev_3", "ses_open:demo:b1", "timeout", &[]),
        )
        .await;

        let b1 = state.store.task("demo", "b1").unwrap().unwrap();
        assert_eq!(b1.status, TaskStatus::Failed);
        assert_eq!(b1.result.as_deref(), Some("session closed: timeout"));
        let b2 = state.store.task("demo", "b2").unwrap().unwrap();
        assert_eq!(b2.status, TaskStatus::Failed);
        assert_eq!(b2.result.as_deref(), Some("dependency 'b1' failed"));
        // Only b1 ever opened.
        assert_eq!(client.opened().len(), 1);
    }

    #[tokio::test]
    async fn terminal_for_foreign_sessions_is_ignored() {
        let (state, client) = state_with_mock();
        process_event(
            &state,
            &terminal_event("cev_1", "ses_unrelated", "normal", &["whatever"]),
        )
        .await;
        assert!(client.actions().is_empty());
    }

    #[tokio::test]
    async fn transient_open_failure_leaves_task_pending_for_the_tick() {
        let (state, client) = state_with_mock();
        *client.fail_open.lock().unwrap() = Some(ocp::ActionFailure::Unavailable);
        process_event(
            &state,
            &intent_event(
                "cev_1",
                "ses_b0",
                "delegate",
                "to=b1 task=\"step one\" id=b1 ns=demo",
            ),
        )
        .await;
        assert_eq!(
            state.store.task("demo", "b1").unwrap().unwrap().status,
            TaskStatus::Pending
        );

        // OCP is back: the tick's scheduler pass opens it.
        *client.fail_open.lock().unwrap() = None;
        run_scheduler(&state, "demo").await;
        assert_eq!(
            state.store.task("demo", "b1").unwrap().unwrap().status,
            TaskStatus::Running
        );
    }
}
