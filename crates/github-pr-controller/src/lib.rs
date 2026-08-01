#![forbid(unsafe_code)]

pub mod closing;
pub mod config;
pub mod github;
pub mod ocp;
pub mod planner;
pub mod runtime_events;
pub mod shadow;
pub mod store;
pub mod verdict;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::{ComponentReadiness, Config, OperatingMode};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use store::{DeliveryAdmission, ProductStore, RuntimeEventAdmission, ShadowAdmission};

#[cfg(test)]
use store::SqliteStore;

type HmacSha256 = Hmac<Sha256>;
const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;
const DELIVERY_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Writes drained per pass. One closed round queues at most three.
const WRITE_DRAIN_BATCH: i64 = 32;

pub struct AppState {
    pub config: Config,
    pub store: Option<Arc<dyn ProductStore>>,
    pub store_error: Option<String>,
    pub action_client: Option<Arc<dyn ocp::OcpActionClient>>,
    pub action_client_error: Option<String>,
    pub event_verifier: Option<Arc<runtime_events::RuntimeEventVerifier>>,
    pub event_verifier_error: Option<String>,
    /// Present only when this runtime is allowed to write (external_canary +
    /// the explicit switch). `None` leaves queued writes pending forever,
    /// which is the correct posture until the P7 cutover.
    pub github: Option<Arc<github::GitHubClient>>,
}

impl AppState {
    pub async fn from_config(config: Config) -> Self {
        let (store, store_error) = match store::open_product_store(&config.db_path).await {
            Ok(store) => (Some(store), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let (action_client, action_client_error) = if matches!(
            config.mode,
            OperatingMode::ExternalCanary | OperatingMode::External
        ) && config.ocp_action.is_complete()
        {
            match ocp::ReqwestOcpActionClient::new(&config.ocp_action) {
                Ok(client) => (
                    Some(Arc::new(client) as Arc<dyn ocp::OcpActionClient>),
                    None,
                ),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (None, None)
        };
        let (event_verifier, event_verifier_error) = match (
            config.ocp_action.controller_id.as_deref(),
            config.event_signing_secret.as_deref(),
        ) {
            (Some(controller_id), Some(secret))
                if matches!(
                    config.mode,
                    OperatingMode::ExternalCanary | OperatingMode::External
                ) =>
            {
                match runtime_events::RuntimeEventVerifier::new(controller_id, secret) {
                    Ok(verifier) => (Some(Arc::new(verifier)), None),
                    Err(error) => (None, Some(error.to_string())),
                }
            }
            _ => (None, None),
        };
        let github = github::GitHubClient::from_config(&config).map(Arc::new);
        Self {
            config,
            store,
            store_error,
            action_client,
            action_client_error,
            event_verifier,
            event_verifier_error,
            github,
        }
    }

    pub fn with_components(
        config: Config,
        store: impl ProductStore + 'static,
        action_client: Option<Arc<dyn ocp::OcpActionClient>>,
        event_verifier: Option<Arc<runtime_events::RuntimeEventVerifier>>,
    ) -> Self {
        let github = github::GitHubClient::from_config(&config).map(Arc::new);
        Self {
            config,
            store: Some(Arc::new(store)),
            store_error: None,
            action_client,
            action_client_error: None,
            event_verifier,
            event_verifier_error: None,
            github,
        }
    }

    #[cfg(test)]
    fn with_store(config: Config, store: SqliteStore) -> Self {
        Self::with_components(config, store, None, None)
    }

    fn product_store_readiness(&self) -> ComponentReadiness {
        if self.store.is_some() {
            ComponentReadiness::ready("controller product store available")
        } else {
            ComponentReadiness::not_ready("controller product store unavailable")
        }
    }

    fn readiness(&self) -> ReadinessReport {
        let ingress = self.config.ingress_readiness();
        let product_store = self.product_store_readiness();
        let github = self.config.github_readiness();
        let ownership = self.config.ownership_readiness();
        let mut ocp = self.config.ocp_readiness();
        if ocp.ready && self.action_client.is_none() {
            ocp = ComponentReadiness::not_ready("scoped OCP action client unavailable");
        }
        let mut runtime_events = self.config.event_readiness();
        if runtime_events.ready && self.event_verifier.is_none() {
            runtime_events = ComponentReadiness::not_ready("runtime-event verifier unavailable");
        }
        let ready = ingress.ready
            && product_store.ready
            && (github.ready || !github.enabled)
            && (ownership.ready || !ownership.enabled)
            && (ocp.ready || !ocp.enabled)
            && (runtime_events.ready || !runtime_events.enabled);
        ReadinessReport {
            status: if ready { "ready" } else { "not_ready" },
            mode: self.config.mode.as_str().to_string(),
            components: Components {
                ingress,
                ownership,
                ocp,
                runtime_events,
                github,
                product_store,
            },
        }
    }
}

#[derive(Serialize)]
struct ReadinessReport {
    status: &'static str,
    mode: String,
    components: Components,
}

#[derive(Serialize)]
struct Components {
    ingress: ComponentReadiness,
    ownership: ComponentReadiness,
    ocp: ComponentReadiness,
    runtime_events: ComponentReadiness,
    github: ComponentReadiness,
    product_store: ComponentReadiness,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .route("/api/v1/github/webhooks", post(handle_webhook))
        .route("/api/v1/shadow/compare", post(handle_shadow_compare))
        .route("/api/v1/shadow/summary", get(shadow_summary))
        .route("/api/v1/openab/events", post(handle_runtime_event))
        .route("/api/v1/canary/summary", get(canary_summary))
        .layer(DefaultBodyLimit::max(MAX_WEBHOOK_BODY_BYTES))
        .with_state(state)
}

pub fn spawn_maintenance(state: &Arc<AppState>) {
    let Some(store) = state.store.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(DELIVERY_PRUNE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            match store.prune_completed_deliveries().await {
                Ok(pruned) if pruned > 0 => {
                    tracing::info!(pruned, "pruned expired webhook deliveries")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "webhook delivery pruning failed"),
            }
        }
    });
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "status": "alive",
        "mode": state.config.mode.as_str(),
        "readiness": state.readiness()
    }))
}

async fn readiness(State(state): State<Arc<AppState>>) -> Response {
    let report = state.readiness();
    let status = if report.status == "ready" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report)).into_response()
}

async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = state.config.webhook_secret.as_deref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "webhook_hmac_not_configured"}),
        );
    };
    let signature = header(&headers, "x-hub-signature-256");
    if !verify_signature(secret, &body, signature) {
        return response(
            StatusCode::FORBIDDEN,
            json!({"ok": false, "error": "invalid_signature"}),
        );
    }

    let Some(delivery_id) = header(&headers, "x-github-delivery") else {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": "missing_delivery_id"}),
        );
    };
    if !valid_delivery_id(delivery_id) {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": "invalid_delivery_id"}),
        );
    }
    let Some(event_type) = header(&headers, "x-github-event") else {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": "missing_event_type"}),
        );
    };
    if !valid_event_type(event_type) {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": "invalid_event_type"}),
        );
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => {
            return response(
                StatusCode::BAD_REQUEST,
                json!({"ok": false, "error": "invalid_json"}),
            )
        }
    };
    let repository = payload["repository"]["full_name"].as_str();
    if matches!(state.config.mode, OperatingMode::ExternalCanary)
        && repository != state.config.canary_repository.as_deref()
    {
        return response(
            StatusCode::CONFLICT,
            json!({"ok": false, "error": "repository_not_owned"}),
        );
    }
    if state.readiness().status != "ready" {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "controller_not_ready"}),
        );
    }
    let Some(store) = state.store.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "product_store_unavailable"}),
        );
    };

    let payload_hash = hex::encode(Sha256::digest(&body));
    match store
        .begin_delivery(delivery_id, event_type, repository, &payload_hash)
        .await
    {
        Ok(DeliveryAdmission::New) => {}
        Ok(DeliveryAdmission::Duplicate { state, .. }) if state == "processing" => {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                    "ok": false,
                    "duplicate": true,
                    "error": "delivery_in_progress"
                }),
            )
        }
        Ok(DeliveryAdmission::Duplicate { state, result }) => {
            return response(
                StatusCode::OK,
                json!({
                    "ok": true,
                    "duplicate": true,
                    "state": state,
                    "result": result
                }),
            )
        }
        Ok(DeliveryAdmission::Conflict) => {
            return response(
                StatusCode::CONFLICT,
                json!({"ok": false, "error": "delivery_payload_conflict"}),
            )
        }
        Err(error) => {
            tracing::error!(%error, %delivery_id, "delivery admission failed");
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "delivery_store_failed"}),
            );
        }
    }

    let (durable_state, result) = match candidate_plan(&state, delivery_id, event_type, &payload)
        .await
    {
        Err(reason) => (
            "ignored",
            json!({"ok": true, "planned": false, "reason": reason}),
        ),
        Ok(plan) if matches!(state.config.mode, OperatingMode::PlanOnly) => (
            "planned",
            json!({"ok": true, "planned": true, "plan": plan}),
        ),
        Ok(plan) => {
            let action_id = format!("github-delivery-{delivery_id}");
            let Some(client) = state.action_client.as_ref() else {
                let result = json!({"ok": false, "error": "ocp_action_unavailable"});
                let _ = store.release_delivery_for_retry(delivery_id, &result).await;
                return response(StatusCode::SERVICE_UNAVAILABLE, result);
            };
            match client
                .open_session(action_id.clone(), plan.open_session_action())
                .await
            {
                Ok(action_result) => {
                    // The terminal event will name only this session id. What
                    // it is *about* is provider knowledge the kernel does not
                    // keep, so record it now or the close is unactionable.
                    if let Some(session_id) = opened_session_id(&action_result) {
                        // A comment-triggered round has no webhook sha. Ask
                        // GitHub for the PR's head so the closing status has a
                        // commit to pin to — otherwise an approved re-review
                        // can never overwrite the failure status it answers.
                        // Review rounds only: an /ask session closes with no
                        // verdict trailer, so giving it a sha would turn every
                        // answered question into an `error` status stomping the
                        // last real verdict (council F1, #312).
                        let mut head_sha = plan.head_sha().map(str::to_string);
                        if head_sha.is_none() && plan.reason != "ask" {
                            if let Some(github) = state.github.as_ref() {
                                match github
                                    .pull_head_sha(&plan.repository, plan.pr_number as i64)
                                    .await
                                {
                                    Ok(sha) => head_sha = Some(sha),
                                    Err(error) => tracing::warn!(
                                        %error,
                                        session_id,
                                        "pr head lookup failed; round will close without a status"
                                    ),
                                }
                            }
                        }
                        if let Err(error) = store
                            .record_session_target(
                                session_id,
                                &plan.repository,
                                plan.pr_number as i64,
                                head_sha.as_deref(),
                            )
                            .await
                        {
                            tracing::error!(%error, session_id, "session target persistence failed");
                        }
                        // The opening comment is queued HERE, not on the
                        // plane's session.opened event: that event races the
                        // session-target write above and lost in practice
                        // (dev, 2026-08-01) — the action result is the
                        // race-free "session exists" signal. Ask sessions get
                        // no round comment; they answer in their own reply.
                        if plan.reason != "ask" {
                            let round = store
                                .next_round(&plan.repository, plan.pr_number as i64)
                                .await
                                .unwrap_or(1);
                            let payload = json!({
                                "repo": plan.repository,
                                "pr_number": plan.pr_number,
                                "round": round,
                            });
                            if let Err(error) = store
                                .enqueue_write(
                                    session_id,
                                    closing::KIND_COMMENT_OPEN,
                                    &payload,
                                )
                                .await
                            {
                                tracing::error!(
                                    %error,
                                    session_id,
                                    "opening-comment enqueue failed"
                                );
                            }
                            spawn_write_drain(&state);
                        }
                    }
                    (
                        "acted",
                        json!({
                            "ok": true,
                            "planned": true,
                            "acted": true,
                            "action_id": action_id,
                            "action_result": action_result,
                            "plan": plan,
                        }),
                    )
                }
                Err(error) => {
                    tracing::warn!(
                        %delivery_id,
                        action_id,
                        error = ?error,
                        "external canary action failed; retaining provider retry path"
                    );
                    let result = json!({"ok": false, "error": error.public_code()});
                    if let Err(store_error) =
                        store.release_delivery_for_retry(delivery_id, &result).await
                    {
                        tracing::error!(%store_error, %delivery_id, "retryable delivery persistence failed");
                    }
                    return response(StatusCode::SERVICE_UNAVAILABLE, result);
                }
            }
        }
    };
    if let Err(error) = store
        .finish_delivery(delivery_id, durable_state, &result)
        .await
    {
        tracing::error!(%error, %delivery_id, "delivery completion failed");
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "delivery_store_failed"}),
        );
    }

    let status = if matches!(durable_state, "planned" | "acted") {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    response(status, result)
}

async fn handle_runtime_event(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !state.config.event_readiness().ready {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "runtime_event_receiver_not_configured"}),
        );
    }
    let Some(verifier) = state.event_verifier.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "runtime_event_receiver_not_configured"}),
        );
    };
    let target = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(uri.path());
    let event = match verifier.verify(
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
    let Some(store) = state.store.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "product_store_unavailable"}),
        );
    };
    let body_hash = hex::encode(Sha256::digest(&body));
    match store.record_runtime_event(&body_hash, &event).await {
        Ok(RuntimeEventAdmission::New) => {
            tracing::info!(
                event_id = event.event_id,
                event_type = event.event_type,
                session_id = event.session_id,
                "accepted signed runtime event"
            );
            let closed = dispatch_terminal(&state, store, &event).await;
            dispatch_abandoned(&state, store, &event).await;
            response(
                StatusCode::OK,
                json!({"ok": true, "duplicate": false, "closed": closed}),
            )
        }
        Ok(RuntimeEventAdmission::Duplicate) => {
            response(StatusCode::OK, json!({"ok": true, "duplicate": true}))
        }
        Ok(RuntimeEventAdmission::Conflict) => response(
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

/// The session id an `open_session` result carries, whether it opened a new
/// session or superseded an older one.
fn opened_session_id(result: &controller_protocol::ActionResultEnvelope) -> Option<&str> {
    match &result.result {
        controller_protocol::ControllerActionResult::SessionOpened { session_id, .. }
        | controller_protocol::ControllerActionResult::Superseded { session_id, .. } => {
            Some(session_id)
        }
        _ => None,
    }
}

/// Turn a normal-close `session.terminal` into a persisted round and queued
/// GitHub writes. Returns false when the event is not ours to act on.
///
/// Deliberately infallible from the caller's side: the event receipt has
/// already been recorded, so a failure here must not make the plane retry the
/// whole delivery — the outbox is what retries.
/// A session that ends without a verdict (superseded by a newer round, or
/// timed out) must not leave its opening post claiming to review forever —
/// rewrite it to say what happened. The opening post itself is queued from
/// the open_session action result in `handle_webhook`, not from here: the
/// plane's session.opened event races the session-target write and loses.
async fn dispatch_abandoned(
    state: &Arc<AppState>,
    store: &Arc<dyn ProductStore>,
    event: &runtime_events::RuntimeEventEnvelope,
) -> bool {
    let kind = match event.event_type.as_str() {
        "session.superseded" | "session.timeout" => closing::KIND_COMMENT_ABANDON,
        _ => return false,
    };
    let Some(session_id) = event.session_id.as_deref() else {
        return false;
    };
    let target = match store.session_target(session_id).await {
        Ok(Some(target)) => target,
        Ok(None) => return false,
        Err(error) => {
            tracing::error!(%error, session_id, "session target lookup failed");
            return false;
        }
    };
    let marker = closing::round_marker(session_id);
    let payload = serde_json::json!({
        "repo": target.repo,
        "pr_number": target.pr_number,
        "body": format!(
            "<!-- openab-council -->\n\
             Review Council round closed without a verdict — superseded \
             by a newer round or timed out.\n\n{marker}"
        ),
    });
    match store.enqueue_write(session_id, kind, &payload).await {
        Ok(_) => {
            spawn_write_drain(state);
            true
        }
        Err(error) => {
            tracing::error!(%error, session_id, kind, "opening-comment enqueue failed");
            false
        }
    }
}

async fn dispatch_terminal(
    state: &Arc<AppState>,
    store: &Arc<dyn ProductStore>,
    event: &runtime_events::RuntimeEventEnvelope,
) -> bool {
    if event.event_type != "session.terminal" {
        return false;
    }
    let Some(session_id) = event.session_id.as_deref() else {
        return false;
    };
    // Timeout and supersede close a session without a result. There is nothing
    // to say about the pull request that is not already said — and a review
    // submitted on a guess would be worse than silence.
    if event.payload["reason"].as_str() != Some("normal") {
        return false;
    }
    let target = match store.session_target(session_id).await {
        Ok(Some(target)) => target,
        // Not a session this controller opened. The kernel's own plugin owns
        // it; acting would be the two-writer failure invariant #4 forbids.
        Ok(None) => return false,
        Err(error) => {
            tracing::error!(%error, session_id, "session target lookup failed");
            return false;
        }
    };
    let final_messages: Vec<String> = event.payload["final_messages"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let parsed = verdict::parse_final_messages(&final_messages);
    // Every round posts its OWN comment (operator decision, 2026-08-01):
    // updating one standing comment in place erased each earlier round's
    // report from the pull request — four rounds of audit trail collapsed
    // into one surviving body. The in-place design (council F3, #305) solved
    // duplicate-comment noise; the round marker plus per-round bodies keep
    // idempotency without destroying history.
    let plan = closing::plan_close(&target, &parsed, None, session_id);

    let round = match store
        .record_review_round(&store::ReviewRound {
            repo: target.repo.clone(),
            pr_number: target.pr_number,
            session_id: session_id.to_string(),
            head_sha: plan.head_sha.clone(),
            decision: plan.decision.clone(),
            red: plan.red,
            yellow: plan.yellow,
            green: plan.green,
        })
        .await
    {
        Ok(round) => round,
        Err(error) => {
            tracing::error!(%error, session_id, "review round persistence failed");
            return false;
        }
    };
    if !round.first_time {
        // A redelivered terminal event. The round and its writes already exist;
        // re-queueing them would be how a verdict gets posted twice.
        tracing::info!(
            session_id,
            round = round.round,
            "terminal event redelivered"
        );
        return true;
    }
    if let Err(error) = store
        .record_review_findings(
            session_id,
            &target.repo,
            target.pr_number,
            plan.head_sha.as_deref(),
            &plan.findings,
        )
        .await
    {
        tracing::error!(%error, session_id, "findings persistence failed");
    }
    for (kind, payload) in &plan.writes {
        if let Err(error) = store.enqueue_write(session_id, kind, payload).await {
            tracing::error!(%error, session_id, kind, "write enqueue failed");
        }
    }
    tracing::info!(
        session_id,
        round = round.round,
        decision = plan.decision,
        writes = plan.writes.len(),
        "queued github writes for closed round"
    );
    spawn_write_drain(state);
    true
}

/// Drain the outbox in the background. Without a write client the rows simply
/// stay pending — which is the whole posture before the P7 cutover: the
/// controller knows exactly what it would post and posts none of it.
fn spawn_write_drain(state: &Arc<AppState>) {
    let (Some(store), Some(github)) = (state.store.clone(), state.github.clone()) else {
        return;
    };
    tokio::spawn(async move {
        let pending = match store.claim_writes(WRITE_DRAIN_BATCH).await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::error!(%error, "outbox read failed");
                return;
            }
        };
        for write in pending {
            match perform_write(&github, store.as_ref(), &write).await {
                Ok(()) => {
                    if let Err(error) = store.mark_write_done(write.id).await {
                        tracing::error!(%error, id = write.id, "outbox completion failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(id = write.id, kind = write.kind, %error, "github write failed");
                    if let Err(error) = store.mark_write_failed(write.id, &error.to_string()).await
                    {
                        tracing::error!(%error, id = write.id, "outbox failure record failed");
                    }
                }
            }
        }
    });
}

async fn perform_write(
    github: &github::GitHubClient,
    store: &dyn ProductStore,
    write: &store::PendingWrite,
) -> anyhow::Result<()> {
    let payload = &write.payload;
    let repo = payload["repo"].as_str().unwrap_or_default();
    match write.kind.as_str() {
        closing::KIND_COMMENT => {
            let body = payload["body"].as_str().unwrap_or_default();
            match payload["comment_id"].as_i64() {
                Some(comment_id) => github.update_comment(repo, comment_id, body).await?,
                None => {
                    let issue = payload["pr_number"].as_i64().unwrap_or_default();
                    // Reconcile before creating: a crash after the create but
                    // before mark-done replays this write once the claim lease
                    // lapses, and a second create is a second comment. The
                    // body carries the round marker, so the earlier success is
                    // findable — adopt it and refresh its body instead.
                    let marker = closing::round_marker(&write.session_id);
                    let comment_id = match github.find_marked_comment(repo, issue, &marker).await? {
                        Some(existing) => {
                            github.update_comment(repo, existing, body).await?;
                            existing
                        }
                        None => github.create_comment(repo, issue, body).await?,
                    };
                    // Learned here, used by every later round of this PR.
                    store
                        .set_round_comment_id(&write.session_id, comment_id)
                        .await?;
                }
            }
            Ok(())
        }
        closing::KIND_COMMENT_OPEN => {
            let issue = payload["pr_number"].as_i64().unwrap_or_default();
            let marker = closing::round_marker(&write.session_id);
            // Create only if the session's marker is absent: a fast close (or
            // a replay of this write) means the round comment already exists
            // in a later state, and "started" must never overwrite it.
            if github.find_marked_comment(repo, issue, &marker).await?.is_none() {
                let round = payload["round"].as_i64().unwrap_or(1);
                let baseline = github
                    .pull_baseline(repo, issue)
                    .await
                    .unwrap_or_else(|_| "Baseline: unavailable".into());
                let body = format!(
                    "<!-- openab-council -->\n\
                     Review Council started (round {round}).\n\n\
                     {baseline}\n\n\
                     The council is reviewing this pull request; this comment \
                     will be updated with this round's verdict.\n\n{marker}"
                );
                github.create_comment(repo, issue, &body).await?;
            }
            Ok(())
        }
        closing::KIND_COMMENT_ABANDON => {
            let issue = payload["pr_number"].as_i64().unwrap_or_default();
            let marker = closing::round_marker(&write.session_id);
            // Update only if the marker exists. A session gets exactly one
            // terminal state, so the found comment can only be this round's
            // own "started" post — never a verdict.
            if let Some(existing) = github.find_marked_comment(repo, issue, &marker).await? {
                let body = payload["body"].as_str().unwrap_or_default();
                github.update_comment(repo, existing, body).await?;
            }
            Ok(())
        }
        closing::KIND_STATUS => {
            let state = match payload["state"].as_str() {
                Some("success") => github::StatusState::Success,
                Some("failure") => github::StatusState::Failure,
                _ => github::StatusState::Error,
            };
            github
                .set_status(
                    repo,
                    payload["sha"].as_str().unwrap_or_default(),
                    state,
                    payload["context"]
                        .as_str()
                        .unwrap_or(closing::STATUS_CONTEXT),
                    payload["description"].as_str().unwrap_or_default(),
                )
                .await
        }
        closing::KIND_REVIEW => {
            let event = match payload["event"].as_str() {
                Some("APPROVE") => github::ReviewEvent::Approve,
                Some("REQUEST_CHANGES") => github::ReviewEvent::RequestChanges,
                other => anyhow::bail!("unknown review event {other:?}"),
            };
            let pr_number = payload["pr_number"].as_i64().unwrap_or_default();
            // Same reconcile as the comment: a replayed submit must find the
            // review it already submitted, not add a second one.
            let marker = closing::round_marker(&write.session_id);
            if github
                .find_marked_review(repo, pr_number, &marker)
                .await?
                .is_some()
            {
                tracing::info!(
                    session_id = write.session_id,
                    "review already on the pull request; reconciled"
                );
                return Ok(());
            }
            // The review timeline entry must say where its evidence lives: the
            // comment write ran earlier in this same drain and recorded its
            // id, so link this round's report directly. Absent id (comment
            // write still failing) degrades to the unlinked body.
            let mut body = payload["body"].as_str().unwrap_or_default().to_string();
            if let Ok(Some(comment_id)) = store.round_comment_id(&write.session_id).await {
                body = body.replace(
                    "Details in the review comment.",
                    &format!(
                        "Full report: https://github.com/{repo}/pull/{pr_number}#issuecomment-{comment_id}"
                    ),
                );
            }
            github
                .submit_review(repo, pr_number, event, &body)
                .await
                .map(|_| ())
        }
        other => anyhow::bail!("unknown write kind {other}"),
    }
}

async fn canary_summary(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(secret) = state.config.observer_secret.as_deref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "observation_hmac_not_configured"}),
        );
    };
    if !verify_signature(secret, &[], header(&headers, "x-canary-signature-256")) {
        return response(
            StatusCode::FORBIDDEN,
            json!({"ok": false, "error": "invalid_observation_signature"}),
        );
    }
    let Some(store) = state.store.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "product_store_unavailable"}),
        );
    };
    match store.canary_summary().await {
        Ok(summary) => response(StatusCode::OK, json!({"ok": true, "summary": summary})),
        Err(error) => {
            tracing::error!(%error, "canary summary failed");
            response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "canary_store_failed"}),
            )
        }
    }
}

async fn handle_shadow_compare(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(secret) = state.config.shadow_secret.as_deref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "shadow_hmac_not_configured"}),
        );
    };
    if !verify_signature(secret, &body, header(&headers, "x-shadow-signature-256")) {
        return response(
            StatusCode::FORBIDDEN,
            json!({"ok": false, "error": "invalid_shadow_signature"}),
        );
    }
    let request: shadow::ShadowCompareRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return response(
                StatusCode::BAD_REQUEST,
                json!({"ok": false, "error": "invalid_shadow_request"}),
            )
        }
    };
    if !valid_delivery_id(&request.comparison_id)
        || !valid_delivery_id(&request.delivery_id)
        || !valid_event_type(&request.event_type)
    {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": "invalid_shadow_identity"}),
        );
    }

    let controller = match candidate_plan(
        &state,
        &request.delivery_id,
        &request.event_type,
        &request.payload,
    )
    .await
    {
        Ok(plan) => shadow::ParityOutcome::Planned {
            snapshot: Box::new(plan.parity_snapshot()),
        },
        Err(reason) => shadow::ParityOutcome::Ignored {
            reason: reason.into(),
        },
    };
    let repository = request.payload["repository"]["full_name"].as_str();
    let report = shadow::compare(request.comparison_id, request.embedded, Some(controller));
    let request_hash = hex::encode(Sha256::digest(&body));
    let Some(store) = state.store.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "product_store_unavailable"}),
        );
    };
    match store
        .record_shadow_comparison(&request_hash, repository, &report)
        .await
    {
        Ok(ShadowAdmission::New) => response(
            StatusCode::OK,
            json!({"ok": true, "duplicate": false, "report": report}),
        ),
        Ok(ShadowAdmission::Duplicate) => response(
            StatusCode::OK,
            json!({"ok": true, "duplicate": true, "report": report}),
        ),
        Ok(ShadowAdmission::Conflict) => response(
            StatusCode::CONFLICT,
            json!({"ok": false, "error": "comparison_payload_conflict"}),
        ),
        Err(error) => {
            tracing::error!(%error, "shadow comparison persistence failed");
            response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "shadow_store_failed"}),
            )
        }
    }
}

async fn shadow_summary(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(secret) = state.config.shadow_secret.as_deref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "shadow_hmac_not_configured"}),
        );
    };
    if !verify_signature(secret, &[], header(&headers, "x-shadow-signature-256")) {
        return response(
            StatusCode::FORBIDDEN,
            json!({"ok": false, "error": "invalid_shadow_signature"}),
        );
    }
    let Some(store) = state.store.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "product_store_unavailable"}),
        );
    };
    match store.shadow_summary().await {
        Ok(summary) => response(StatusCode::OK, json!({"ok": true, "summary": summary})),
        Err(error) => {
            tracing::error!(%error, "shadow summary failed");
            response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "shadow_store_failed"}),
            )
        }
    }
}

async fn candidate_plan(
    state: &AppState,
    delivery_id: &str,
    event_type: &str,
    payload: &Value,
) -> Result<planner::SessionPlan, &'static str> {
    let Some(trigger) =
        planner::parse_trigger(event_type, payload, state.config.bot_handle.as_deref())
    else {
        return Err("not_a_trigger");
    };
    if !state.config.allowed_repos.is_empty()
        && !state.config.allowed_repos.contains(&trigger.repository)
    {
        return Err("repo_not_allowed");
    }
    if !trigger.author_trusted {
        return Err("author_not_trusted");
    }
    // The round number is ours to know: `review_rounds` counts what this
    // controller actually closed on this pull request.
    let round = match state.store.as_ref() {
        Some(store) => match store
            .next_round(&trigger.repository, trigger.pr_number as i64)
            .await
        {
            Ok(round) => round,
            Err(error) => {
                tracing::warn!(%error, "round lookup failed; assuming round 1");
                1
            }
        },
        None => 1,
    };
    Ok(planner::build_plan_for_round(
        delivery_id,
        trigger,
        &state.config.roster,
        state.config.council_preset.as_deref(),
        &state.config.review_mode,
        round,
        state.config.bot_handle.as_deref(),
    ))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .filter(|value| !value.is_empty())
}

fn valid_delivery_id(value: &str) -> bool {
    value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_event_type(value: &str) -> bool {
    value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

pub fn verify_signature(secret: &str, body: &[u8], signature_header: Option<&str>) -> bool {
    let Some(signature) = signature_header.and_then(|value| value.strip_prefix("sha256=")) else {
        return false;
    };
    let Ok(expected) = hex::decode(signature) else {
        return false;
    };
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use controller_protocol::{ActionResultEnvelope, ControllerActionResult, OpenSessionAction};
    use http_body_util::BodyExt;
    use std::collections::{BTreeSet, VecDeque};
    use std::sync::Mutex;
    use tower::ServiceExt;

    type ActionCalls = Arc<Mutex<Vec<(String, OpenSessionAction)>>>;

    struct RecordingActionClient {
        calls: ActionCalls,
        failures: Mutex<VecDeque<bool>>,
    }

    impl RecordingActionClient {
        fn new(failures: impl IntoIterator<Item = bool>) -> (Self, ActionCalls) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: calls.clone(),
                    failures: Mutex::new(failures.into_iter().collect()),
                },
                calls,
            )
        }
    }

    impl ocp::OcpActionClient for RecordingActionClient {
        fn open_session(&self, action_id: String, action: OpenSessionAction) -> ocp::ActionFuture {
            self.calls
                .lock()
                .unwrap()
                .push((action_id.clone(), action.clone()));
            let fail = self.failures.lock().unwrap().pop_front().unwrap_or(false);
            Box::pin(async move {
                if fail {
                    return Err(ocp::ActionFailure::Unavailable);
                }
                let result = if action.trigger_fingerprint.as_deref() == Some("sha:def456") {
                    ControllerActionResult::Superseded {
                        session_id: "ses_2".into(),
                        old_id: "ses_1".into(),
                    }
                } else {
                    ControllerActionResult::SessionOpened {
                        session_id: "ses_1".into(),
                        deduped: false,
                    }
                };
                Ok(ActionResultEnvelope {
                    version: controller_protocol::CURRENT_VERSION,
                    action_id,
                    result,
                })
            })
        }
    }

    fn test_config() -> Config {
        Config {
            addr: "127.0.0.1:0".into(),
            db_path: ":memory:".into(),
            mode: OperatingMode::PlanOnly,
            webhook_secret: Some("fixture-secret".into()),
            shadow_secret: Some("shadow-secret".into()),
            observer_secret: None,
            canary_repository: None,
            allowed_repos: BTreeSet::from(["example/repo".into()]),
            bot_handle: Some("fixture-council".into()),
            roster: vec!["chair".into(), "rev1".into(), "rev2".into()],
            council_preset: None,
            review_mode: "approve".into(),
            ocp_action: config::OcpActionConfig {
                base_url: None,
                action_token: None,
                scope: None,
                controller_id: None,
            },
            event_signing_secret: None,
            github_app: config::GitHubAppConfig {
                app_id: None,
                installation_id: None,
                private_key: None,
            },
            enable_writes: false,
            github_api_base: "https://api.github.com".into(),
        }
    }

    fn external_config(event_secret: &[u8]) -> Config {
        let mut config = test_config();
        config.mode = OperatingMode::ExternalCanary;
        config.canary_repository = Some("example/repo".into());
        config.ocp_action = config::OcpActionConfig {
            base_url: Some("https://ocp.example.test".into()),
            action_token: Some("fixture-action-token".into()),
            scope: Some("tenant:dev/resource:canary".into()),
            controller_id: Some("github-canary".into()),
        };
        config.event_signing_secret = Some(URL_SAFE_NO_PAD.encode(event_secret));
        config.observer_secret = Some("observer-secret".into());
        config
    }

    #[tokio::test]
    async fn external_mode_actually_wires_up_and_reaches_ready() {
        // Council F1 on #315: config-level readiness said external was live
        // while the AppState constructors still gated on ExternalCanary, so
        // the mode could never build an action client or event verifier. This
        // exercises the real wiring end to end.
        let mut config = external_config(b"0123456789abcdef0123456789abcdef");
        config.mode = OperatingMode::External;
        config.canary_repository = None;
        config.github_app = config::GitHubAppConfig {
            app_id: Some("4235962".into()),
            installation_id: Some("144934354".into()),
            private_key: Some("not-a-pem-but-present".into()),
        };
        config.enable_writes = true;
        let state = AppState::from_config(config).await;
        assert!(state.action_client.is_some(), "action client must exist");
        assert!(state.event_verifier.is_some(), "event verifier must exist");
        assert!(state.github.is_some(), "write client must exist");
        let readiness = state.readiness();
        assert_eq!(readiness.status, "ready", "external mode must reach ready");
    }

    fn signed_request(delivery: &str, body: &'static str) -> Request<Body> {
        let mut mac = HmacSha256::new_from_slice(b"fixture-secret").unwrap();
        mac.update(body.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::post("/api/v1/github/webhooks")
            .header("x-github-event", "pull_request")
            .header("x-github-delivery", delivery)
            .header("x-hub-signature-256", signature)
            .body(Body::from(body))
            .unwrap()
    }

    fn signed_owned_request(delivery: &str, event: &str, body: String) -> Request<Body> {
        let mut mac = HmacSha256::new_from_slice(b"fixture-secret").unwrap();
        mac.update(body.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::post("/api/v1/github/webhooks")
            .header("x-github-event", event)
            .header("x-github-delivery", delivery)
            .header("x-hub-signature-256", signature)
            .body(Body::from(body))
            .unwrap()
    }

    fn signed_shadow_request(request: &shadow::ShadowCompareRequest) -> Request<Body> {
        let body = serde_json::to_vec(request).unwrap();
        let mut mac = HmacSha256::new_from_slice(b"shadow-secret").unwrap();
        mac.update(&body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::post("/api/v1/shadow/compare")
            .header("x-shadow-signature-256", signature)
            .body(Body::from(body))
            .unwrap()
    }

    fn signed_shadow_summary_request() -> Request<Body> {
        let mut mac = HmacSha256::new_from_slice(b"shadow-secret").unwrap();
        mac.update(&[]);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::get("/api/v1/shadow/summary")
            .header("x-shadow-signature-256", signature)
            .body(Body::empty())
            .unwrap()
    }

    fn signed_runtime_event_request(
        secret: &[u8],
        event_id: &str,
        target: &str,
        body: String,
    ) -> Request<Body> {
        let timestamp = now_unix();
        let body_hash = hex::encode(Sha256::digest(body.as_bytes()));
        let canonical =
            format!("v1\ngithub-canary\n{event_id}\n{timestamp}\nPOST\n{target}\n{body_hash}");
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(canonical.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::post(target)
            .header("x-oab-controller-id", "github-canary")
            .header("x-oab-event-id", event_id)
            .header("x-oab-timestamp", timestamp)
            .header("x-oab-signature", signature)
            .body(Body::from(body))
            .unwrap()
    }

    fn signed_canary_summary_request() -> Request<Body> {
        let mut mac = HmacSha256::new_from_slice(b"observer-secret").unwrap();
        mac.update(&[]);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::get("/api/v1/canary/summary")
            .header("x-canary-signature-256", signature)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn readiness_reports_disabled_external_clients() {
        let state = Arc::new(AppState::with_store(
            test_config(),
            SqliteStore::memory().unwrap(),
        ));
        let response = router(state)
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["mode"], "plan_only");
        assert_eq!(body["components"]["ocp"]["enabled"], false);
        assert_eq!(body["components"]["github"]["enabled"], false);
    }

    #[tokio::test]
    async fn readiness_does_not_disclose_product_store_errors() {
        let state = Arc::new(AppState {
            config: test_config(),
            store: None,
            store_error: Some("unable to open /private/secret/controller.db".into()),
            action_client: None,
            action_client_error: None,
            event_verifier: None,
            event_verifier_error: None,
            github: None,
        });
        let response = router(state)
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("controller product store unavailable"));
        assert!(!body.contains("/private/secret"));
    }

    #[tokio::test]
    async fn plan_only_readiness_rejects_github_write_credentials() {
        let mut config = test_config();
        config.github_app.app_id = Some("1".into());
        config.github_app.installation_id = Some("2".into());
        config.github_app.private_key = Some("private".into());
        let state = Arc::new(AppState::with_store(config, SqliteStore::memory().unwrap()));
        let response = router(state)
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn canary_readiness_needs_the_write_switch_alongside_credentials() {
        // Credentials without the switch is now the *loud* failure — before P4
        // any credential at all was rejected, so this is the one case whose
        // meaning changed and it must stay a 503 rather than become ready.
        let event_secret = vec![9; 32];
        let mut config = external_config(&event_secret);
        config.github_app.app_id = Some("1".into());
        config.github_app.installation_id = Some("2".into());
        config.github_app.private_key = Some("private".into());
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let state = Arc::new(AppState::with_components(
            config.clone(),
            SqliteStore::memory().unwrap(),
            Some(Arc::new(RecordingActionClient::new([]).0)),
            Some(Arc::new(verifier)),
        ));
        let response = router(state)
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        // With the switch on, the same deployment is ready and the write client
        // can be built — the two gates cannot drift apart.
        config.enable_writes = true;
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let state = Arc::new(AppState::with_components(
            config.clone(),
            SqliteStore::memory().unwrap(),
            Some(Arc::new(RecordingActionClient::new([]).0)),
            Some(Arc::new(verifier)),
        ));
        let response = router(state)
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(github::GitHubClient::from_config(&config).is_some());
    }

    #[tokio::test]
    async fn a_supersede_rewrites_the_round_comment_and_session_opened_queues_nothing() {
        let event_secret = vec![7; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let store = SqliteStore::memory().unwrap();
        store
            .record_session_target("ses_1", "example/repo", 7, Some("openingsha"))
            .unwrap();
        let state = Arc::new(AppState::with_components(
            config,
            store,
            Some(Arc::new(RecordingActionClient::new([]).0)),
            Some(Arc::new(verifier)),
        ));
        let store = state.store.clone().unwrap();
        let app = router(state.clone());

        let opened = json!({
            "version": "1",
            "event_id": "cev_open_1",
            "controller_id": "github-canary",
            "event_type": "session.opened",
            "session_id": "ses_1",
            "occurred_at": 1_000,
            "payload": {"trigger_ref": "github:pr/example/repo#7"}
        })
        .to_string();
        let response = app
            .clone()
            .oneshot(signed_runtime_event_request(
                &event_secret,
                "cev_open_1",
                "/api/v1/openab/events?version=1",
                opened,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // session.opened queues nothing: it races the session-target write,
        // so the opening comment is queued from the open_session action
        // result instead (see handle_webhook).
        assert_eq!(store.pending_writes(10).await.unwrap().len(), 0);

        // A supersede queues the rewrite that retires the "started" post —
        // marked with the session's own marker, so it can never touch another
        // round's verdict.
        let superseded = json!({
            "version": "1",
            "event_id": "cev_sup_1",
            "controller_id": "github-canary",
            "event_type": "session.superseded",
            "session_id": "ses_1",
            "occurred_at": 2_000,
            "payload": {"reason": "superseded"}
        })
        .to_string();
        let response = app
            .oneshot(signed_runtime_event_request(
                &event_secret,
                "cev_sup_1",
                "/api/v1/openab/events?version=1",
                superseded,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let pending = store.pending_writes(10).await.unwrap();
        let abandon = pending
            .iter()
            .find(|w| w.kind == closing::KIND_COMMENT_ABANDON)
            .expect("abandon write queued");
        let body = abandon.payload["body"].as_str().unwrap();
        assert!(body.contains("without a verdict"));
        assert!(body.contains(&closing::round_marker("ses_1")));
    }

    #[tokio::test]
    async fn a_closed_round_becomes_persisted_state_and_queued_writes() {
        let event_secret = vec![7; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let store = SqliteStore::memory().unwrap();
        store
            .record_session_target("ses_1", "example/repo", 7, Some("openingsha"))
            .unwrap();
        let state = Arc::new(AppState::with_components(
            config,
            store,
            Some(Arc::new(RecordingActionClient::new([]).0)),
            Some(Arc::new(verifier)),
        ));
        let store = state.store.clone().unwrap();
        let app = router(state.clone());

        let body = json!({
            "version": "1",
            "event_id": "cev_close_1",
            "controller_id": "github-canary",
            "event_type": "session.terminal",
            "session_id": "ses_1",
            "occurred_at": 1_000,
            "payload": {
                "reason": "normal",
                "final_messages": [
                    "## Verdict\n\nprose\n<!-- openab-findings\n{\"head_sha\":\"reviewedsha\",\"findings\":[{\"id\":\"F1\",\"severity\":\"yellow\",\"title\":\"races on close\"}]}\n-->\n[[verdict:request_changes r=0 y=1 g=2]] [done]"
                ]
            }
        })
        .to_string();
        let response = app
            .clone()
            .oneshot(signed_runtime_event_request(
                &event_secret,
                "cev_close_1",
                "/api/v1/openab/events?version=1",
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["closed"], true);

        // The round landed with the verdict the chair's counts imply, and with
        // the sha the council actually read rather than the opening one.
        let pending = store.pending_writes(10).await.unwrap();
        assert_eq!(
            pending.iter().map(|w| w.kind.as_str()).collect::<Vec<_>>(),
            ["comment", "status", "review"]
        );
        let status = pending.iter().find(|w| w.kind == "status").unwrap();
        assert_eq!(
            status.payload["sha"], "openingsha",
            "the status is pinned to the webhook sha, not the chair's claim"
        );
        assert_eq!(status.payload["state"], "failure");
        let review = pending.iter().find(|w| w.kind == "review").unwrap();
        assert_eq!(review.payload["event"], "REQUEST_CHANGES");

        // Nothing was sent: no write client in this configuration. That is the
        // pre-cutover posture — the controller knows what it would post.
        assert!(state.github.is_none());

        // At-least-once redelivery must not queue a second verdict. A fresh
        // event id, the same session — exactly what a retry after a lost
        // acknowledgement looks like.
        let redelivery = body.replace("cev_close_1", "cev_close_2");
        let response = app
            .oneshot(signed_runtime_event_request(
                &event_secret,
                "cev_close_2",
                "/api/v1/openab/events?version=1",
                redelivery,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(store.pending_writes(10).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_replayed_write_reconciles_instead_of_posting_twice() {
        // Council F5 on #305, the P7 gate: a crash between sending and marking
        // done replays the write after the claim lease lapses, and neither a
        // comment nor a review is idempotent. The reconcile must find the
        // earlier success by its round marker and not post again.
        use axum::extract::Path as AxPath;
        use axum::routing::{get as axum_get, post as axum_post};

        #[derive(Clone, Default)]
        struct Github {
            comments: Arc<std::sync::Mutex<Vec<(i64, String, String)>>>,
            reviews: Arc<std::sync::Mutex<Vec<(i64, String, String)>>>,
        }
        let gh = Github::default();
        let app = Router::new()
            .route(
                "/app/installations/:id/access_tokens",
                axum_post(|| async { Json(json!({"token": "ghs_test"})) }),
            )
            .route(
                "/repos/:o/:n/issues/:num/comments",
                axum_get({
                    let gh = gh.clone();
                    move || {
                        let gh = gh.clone();
                        async move {
                            let list: Vec<Value> = gh
                                .comments
                                .lock()
                                .unwrap()
                                .iter()
                                .rev() // newest first, like the real API sorted desc
                                .map(|(id, body, login)| {
                                    json!({"id": id, "body": body, "user": {"login": login}})
                                })
                                .collect();
                            Json(Value::Array(list))
                        }
                    }
                })
                .post({
                    let gh = gh.clone();
                    move |Json(body): Json<Value>| {
                        let gh = gh.clone();
                        async move {
                            let mut comments = gh.comments.lock().unwrap();
                            let id = 1000 + comments.len() as i64;
                            comments.push((
                                id,
                                body["body"].as_str().unwrap_or_default().into(),
                                "fixture-council[bot]".into(),
                            ));
                            Json(json!({"id": id}))
                        }
                    }
                }),
            )
            .route(
                "/repos/:o/:n/issues/comments/:id",
                axum::routing::patch(
                    |AxPath((_, _, id)): AxPath<(String, String, i64)>| async move {
                        Json(json!({"id": id}))
                    },
                ),
            )
            .route(
                "/repos/:o/:n/statuses/:sha",
                axum_post(|| async { Json(json!({"id": 1})) }),
            )
            .route(
                "/repos/:o/:n/pulls/:num/reviews",
                axum_get({
                    let gh = gh.clone();
                    move || {
                        let gh = gh.clone();
                        async move {
                            let list: Vec<Value> = gh
                                .reviews
                                .lock()
                                .unwrap()
                                .iter()
                                .rev()
                                .map(|(id, body, login)| {
                                    json!({"id": id, "body": body, "user": {"login": login}})
                                })
                                .collect();
                            Json(Value::Array(list))
                        }
                    }
                })
                .post({
                    let gh = gh.clone();
                    move |Json(body): Json<Value>| {
                        let gh = gh.clone();
                        async move {
                            let mut reviews = gh.reviews.lock().unwrap();
                            let id = 2000 + reviews.len() as i64;
                            reviews.push((
                                id,
                                body["body"].as_str().unwrap_or_default().into(),
                                "fixture-council[bot]".into(),
                            ));
                            Json(json!({"id": id, "state": "CHANGES_REQUESTED"}))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut config = Config::from_values(|_| None);
        config.mode = OperatingMode::ExternalCanary;
        config.enable_writes = true;
        config.canary_repository = Some("example/repo".into());
        config.github_api_base = format!("http://{addr}");
        config.bot_handle = Some("fixture-council".into());
        config.github_app = config::GitHubAppConfig {
            app_id: Some("1".into()),
            installation_id: Some("2".into()),
            private_key: Some("unused".into()),
        };
        let client =
            github::GitHubClient::from_config(&config).expect("writes enabled builds a client");
        client.seed_test_token("ghs_test");
        let store = SqliteStore::memory().unwrap();

        // The round's writes, as dispatch would queue them.
        let marker = closing::round_marker("ses_1");
        // A hostile participant has already planted decoys carrying the same
        // marker — the id is public the moment any council comment exists.
        // Neither may be adopted: the comment is not PATCHable by us, and the
        // decoy review must not suppress the real one (council F1, #307).
        gh.comments.lock().unwrap().push((
            900,
            format!(
                "decoy

{marker}"
            ),
            "attacker".into(),
        ));
        gh.reviews.lock().unwrap().push((
            901,
            format!(
                "decoy

{marker}"
            ),
            "attacker".into(),
        ));
        store
            .enqueue_write(
                "ses_1",
                closing::KIND_COMMENT,
                &json!({"repo": "example/repo", "pr_number": 7, "comment_id": null,
                        "body": format!("verdict body\n\n{marker}")}),
            )
            .unwrap();
        store
            .enqueue_write(
                "ses_1",
                closing::KIND_REVIEW,
                &json!({"repo": "example/repo", "pr_number": 7, "event": "REQUEST_CHANGES",
                        "body": format!("blocked\n\n{marker}")}),
            )
            .unwrap();

        // First drain pass: both writes go out.
        for write in store.claim_writes(10).unwrap() {
            perform_write(&client, &store, &write).await.unwrap();
            // Deliberately NOT marking done — this is the crash. The rows keep
            // their claim; GitHub has the comment and the review.
        }
        assert_eq!(
            gh.comments.lock().unwrap().len(),
            2,
            "decoy + the real comment: the create ignored the attacker's marker"
        );
        assert_eq!(gh.reviews.lock().unwrap().len(), 2);

        // The lease lapses and another drain claims the same rows. Without the
        // reconcile this second pass double-posts.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let replayed = store.claim_writes_for_test_after_lease(10).unwrap();
        assert_eq!(replayed.len(), 2, "the crash left both rows claimable");
        for write in &replayed {
            perform_write(&client, &store, write).await.unwrap();
            store.mark_write_done(write.id).unwrap();
        }
        assert_eq!(
            gh.comments.lock().unwrap().len(),
            2,
            "the replayed create adopted OUR marked comment, not the decoy"
        );
        assert_eq!(
            gh.reviews.lock().unwrap().len(),
            2,
            "the replayed submit found OUR marked review, not the decoy"
        );
        // And the adopted comment id is now the round's anchor.
        assert_eq!(
            store.last_comment_id("example/repo", 7).unwrap(),
            None,
            "no round row in this test; anchor write is a no-op"
        );
    }

    #[tokio::test]
    async fn with_writes_enabled_the_outbox_drains_to_real_requests() {
        // The end the whole workstream is for: a closed round becomes a
        // comment, a status, and a formal review actually sent over HTTP.
        use axum::extract::Path;
        use axum::routing::post as axum_post;

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorder = seen.clone();
        let fake = Router::new()
            .route(
                "/app/installations/:id/access_tokens",
                axum_post(|| async { Json(json!({"token": "ghs_test"})) }),
            )
            .route(
                "/repos/:owner/:name/issues/:number/comments",
                // The reconcile GETs before the create POSTs; nothing exists
                // yet, so the listing is empty.
                axum::routing::get(|| async { Json(json!([])) }).post({
                    let seen = seen.clone();
                    move |Json(body): Json<Value>| {
                        let seen = seen.clone();
                        async move {
                            seen.lock().unwrap().push(format!(
                                "comment:{}",
                                body["body"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .lines()
                                    .next()
                                    .unwrap_or_default()
                            ));
                            Json(json!({"id": 4242}))
                        }
                    }
                }),
            )
            .route(
                "/repos/:owner/:name/statuses/:sha",
                axum_post({
                    let seen = seen.clone();
                    move |Path((_, _, sha)): Path<(String, String, String)>,
                          Json(body): Json<Value>| {
                        let seen = seen.clone();
                        async move {
                            seen.lock().unwrap().push(format!(
                                "status:{sha}:{}",
                                body["state"].as_str().unwrap_or_default()
                            ));
                            Json(json!({"id": 1}))
                        }
                    }
                }),
            )
            .route(
                "/repos/:owner/:name/pulls/:number/reviews",
                axum::routing::get(|| async { Json(json!([])) }).post({
                    let seen = recorder;
                    move |Json(body): Json<Value>| {
                        let seen = seen.clone();
                        async move {
                            let event = body["event"].as_str().unwrap_or_default().to_string();
                            seen.lock().unwrap().push(format!("review:{event}"));
                            Json(json!({"id": 77, "state": "CHANGES_REQUESTED"}))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, fake).await;
        });

        let event_secret = vec![7; 32];
        let mut config = external_config(&event_secret);
        config.enable_writes = true;
        config.github_api_base = format!("http://{addr}");
        config.github_app = config::GitHubAppConfig {
            app_id: Some("1".into()),
            installation_id: Some("2".into()),
            private_key: Some("unused-the-token-is-seeded".into()),
        };
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let store = SqliteStore::memory().unwrap();
        store
            .record_session_target("ses_1", "example/repo", 7, Some("openingsha"))
            .unwrap();
        let state = Arc::new(AppState::with_components(
            config,
            store,
            Some(Arc::new(RecordingActionClient::new([]).0)),
            Some(Arc::new(verifier)),
        ));
        state
            .github
            .as_ref()
            .expect("writes enabled builds a client")
            .seed_test_token("ghs_test");
        let store = state.store.clone().unwrap();

        let body = json!({
            "version": "1",
            "event_id": "cev_drain_1",
            "controller_id": "github-canary",
            "event_type": "session.terminal",
            "session_id": "ses_1",
            "occurred_at": 1_000,
            "payload": {
                "reason": "normal",
                "final_messages": ["## Verdict\n\nblocked\n[[verdict:request_changes r=1 y=0 g=0]] [done]"]
            }
        })
        .to_string();
        let response = router(state.clone())
            .oneshot(signed_runtime_event_request(
                &event_secret,
                "cev_drain_1",
                "/api/v1/openab/events?version=1",
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The drain is spawned; wait for the outbox to empty.
        for _ in 0..100 {
            if store.pending_writes(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            store.pending_writes(10).await.unwrap().is_empty(),
            "every queued write should have been sent"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "comment:## Verdict",
                "status:openingsha:failure",
                "review:REQUEST_CHANGES"
            ]
        );
        // The comment id came back from GitHub and is now the upsert anchor
        // for the next round of this pull request.
        assert_eq!(
            store.last_comment_id("example/repo", 7).await.unwrap(),
            Some(4242)
        );
    }

    #[tokio::test]
    async fn terminal_events_we_do_not_own_or_cannot_act_on_are_left_alone() {
        let event_secret = vec![7; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let store = SqliteStore::memory().unwrap();
        store
            .record_session_target("ses_ours", "example/repo", 7, Some("sha"))
            .unwrap();
        let state = Arc::new(AppState::with_components(
            config,
            store,
            Some(Arc::new(RecordingActionClient::new([]).0)),
            Some(Arc::new(verifier)),
        ));
        let store = state.store.clone().unwrap();
        let app = router(state);

        let cases = [
            // A session the embedded plugin owns — acting would be the
            // two-writer failure invariant #4 forbids.
            ("cev_a", "ses_theirs", "normal"),
            // Timeout and supersede have no result to report; a review
            // submitted on a guess is worse than silence.
            ("cev_b", "ses_ours", "timeout"),
            ("cev_c", "ses_ours", "superseded"),
        ];
        for (event_id, session_id, reason) in cases {
            let body = json!({
                "version": "1",
                "event_id": event_id,
                "controller_id": "github-canary",
                "event_type": "session.terminal",
                "session_id": session_id,
                "occurred_at": 1_000,
                "payload": {
                    "reason": reason,
                    "final_messages": ["done [[verdict:approve r=0 y=0 g=1]] [done]"]
                }
            })
            .to_string();
            let response = app
                .clone()
                .oneshot(signed_runtime_event_request(
                    &event_secret,
                    event_id,
                    "/api/v1/openab/events?version=1",
                    body,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(value["closed"], false, "{reason} on {session_id}");
        }
        assert!(store.pending_writes(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn signed_shadow_comparison_records_exact_fixture_parity() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let state = Arc::new(AppState::with_store(
            test_config(),
            SqliteStore::memory().unwrap(),
        ));
        let payload: Value = serde_json::from_str(BODY).unwrap();
        let embedded = shadow::ParityOutcome::Planned {
            snapshot: Box::new(
                candidate_plan(&state, "delivery-shadow", "pull_request", &payload)
                    .await
                    .unwrap()
                    .parity_snapshot(),
            ),
        };
        let request = shadow::ShadowCompareRequest {
            comparison_id: "comparison-1".into(),
            delivery_id: "delivery-shadow".into(),
            event_type: "pull_request".into(),
            payload,
            embedded: Some(embedded),
        };
        let app = router(state);
        let response = app
            .clone()
            .oneshot(signed_shadow_request(&request))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["report"]["exact_match"], true);
        assert_eq!(body["report"]["promotion_blocked"], false);

        let duplicate = app
            .clone()
            .oneshot(signed_shadow_request(&request))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::OK);
        let duplicate: Value =
            serde_json::from_slice(&duplicate.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(duplicate["duplicate"], true);

        let mut conflicting = request;
        let shadow::ParityOutcome::Planned { snapshot } = conflicting.embedded.as_mut().unwrap()
        else {
            unreachable!();
        };
        snapshot.open_session.prompt = "drift".into();
        let conflict = app
            .clone()
            .oneshot(signed_shadow_request(&conflicting))
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/shadow/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);

        let response = app.oneshot(signed_shadow_summary_request()).await.unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["summary"]["total"], 1);
        assert_eq!(body["summary"]["exact_matches"], 1);
    }

    #[tokio::test]
    async fn signed_fixture_produces_a_plan_and_dedupes_delivery() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let state = Arc::new(AppState::with_store(
            test_config(),
            SqliteStore::memory().unwrap(),
        ));
        let app = router(state);
        let first = app
            .clone()
            .oneshot(signed_request("delivery-1", BODY))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let body: Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(body["plan"]["source_delivery_id"], "delivery-1");
        assert_eq!(body["plan"]["proposed_writes"].as_array().unwrap().len(), 3);

        let duplicate = app
            .oneshot(signed_request("delivery-1", BODY))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&duplicate.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["duplicate"], true);
        assert_eq!(body["state"], "planned");
    }

    #[tokio::test]
    async fn external_canary_acts_once_per_delivery_and_supersedes_by_fingerprint() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let event_secret = vec![8; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let (client, calls) = RecordingActionClient::new([false, false]);
        let state = Arc::new(AppState::with_components(
            config,
            SqliteStore::memory().unwrap(),
            Some(Arc::new(client)),
            Some(Arc::new(verifier)),
        ));
        let store = state.store.clone().unwrap();
        let app = router(state);

        let first = app
            .clone()
            .oneshot(signed_owned_request(
                "canary-delivery-1",
                "pull_request",
                BODY.into(),
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first: Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(first["action_result"]["result"]["type"], "session_opened");

        // Opening the session queued the round comment — from the action
        // result, race-free, before any runtime event arrives.
        let pending = store.pending_writes(10).await.unwrap();
        let open = pending
            .iter()
            .find(|w| w.kind == closing::KIND_COMMENT_OPEN)
            .expect("opening comment queued");
        assert_eq!(open.payload["round"], 1);

        let duplicate = app
            .clone()
            .oneshot(signed_owned_request(
                "canary-delivery-1",
                "pull_request",
                BODY.into(),
            ))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(calls.lock().unwrap().len(), 1);

        let mut synchronize: Value = serde_json::from_str(BODY).unwrap();
        synchronize["action"] = json!("synchronize");
        synchronize["pull_request"]["head"]["sha"] = json!("def456");
        let superseded = app
            .oneshot(signed_owned_request(
                "canary-delivery-2",
                "pull_request",
                synchronize.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(superseded.status(), StatusCode::ACCEPTED);
        let superseded: Value =
            serde_json::from_slice(&superseded.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(superseded["action_result"]["result"]["type"], "superseded");
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "github-delivery-canary-delivery-1");
        assert_eq!(
            calls[1].1.trigger_fingerprint.as_deref(),
            Some("sha:def456")
        );
    }

    #[tokio::test]
    async fn external_canary_rejects_an_unowned_repository_before_action_dispatch() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let event_secret = vec![8; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let (client, calls) = RecordingActionClient::new([]);
        let state = Arc::new(AppState::with_components(
            config,
            SqliteStore::memory().unwrap(),
            Some(Arc::new(client)),
            Some(Arc::new(verifier)),
        ));
        let mut payload: Value = serde_json::from_str(BODY).unwrap();
        payload["repository"]["full_name"] = json!("other/repo");

        let response = router(state)
            .oneshot(signed_owned_request(
                "wrong-repository",
                "pull_request",
                payload.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn signed_runtime_events_are_deduped_and_visible_as_aggregates_only() {
        let event_secret = vec![8; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let (client, _) = RecordingActionClient::new([]);
        let state = Arc::new(AppState::with_components(
            config,
            SqliteStore::memory().unwrap(),
            Some(Arc::new(client)),
            Some(Arc::new(verifier)),
        ));
        let app = router(state);
        let target = "/api/v1/openab/events?version=1";
        let event_id = "cev-timeout-1";
        let body = json!({
            "version": "1",
            "event_id": event_id,
            "controller_id": "github-canary",
            "event_type": "session.timeout",
            "session_id": "ses-canary-1",
            "occurred_at": now_unix() * 1000,
            "payload": {"reason": "timeout", "private_detail": "must-not-persist"}
        })
        .to_string();

        let accepted = app
            .clone()
            .oneshot(signed_runtime_event_request(
                &event_secret,
                event_id,
                target,
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        let accepted: Value =
            serde_json::from_slice(&accepted.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(accepted["duplicate"], false);

        let duplicate = app
            .clone()
            .oneshot(signed_runtime_event_request(
                &event_secret,
                event_id,
                target,
                body,
            ))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::OK);
        let duplicate: Value =
            serde_json::from_slice(&duplicate.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(duplicate["duplicate"], true);

        let conflicting_body = json!({
            "version": "1",
            "event_id": event_id,
            "controller_id": "github-canary",
            "event_type": "action.failed",
            "session_id": "ses-canary-1",
            "occurred_at": now_unix() * 1000,
            "payload": {"reason": "changed"}
        })
        .to_string();
        let conflict = app
            .clone()
            .oneshot(signed_runtime_event_request(
                &event_secret,
                event_id,
                target,
                conflicting_body,
            ))
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);

        let unauthorized = app
            .clone()
            .oneshot(
                Request::get("/api/v1/canary/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);
        let summary = app.oneshot(signed_canary_summary_request()).await.unwrap();
        assert_eq!(summary.status(), StatusCode::OK);
        let summary: Value =
            serde_json::from_slice(&summary.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(summary["summary"]["runtime_events"], 1);
        assert_eq!(
            summary["summary"]["runtime_event_types"]["session.timeout"],
            1
        );
        assert!(summary.to_string().find("private_detail").is_none());
    }

    #[tokio::test]
    async fn external_canary_outage_retries_same_action_id_without_embedded_fallback() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let event_secret = vec![8; 32];
        let config = external_config(&event_secret);
        let verifier = runtime_events::RuntimeEventVerifier::new(
            "github-canary",
            config.event_signing_secret.as_deref().unwrap(),
        )
        .unwrap();
        let (client, calls) = RecordingActionClient::new([true, false]);
        let state = Arc::new(AppState::with_components(
            config,
            SqliteStore::memory().unwrap(),
            Some(Arc::new(client)),
            Some(Arc::new(verifier)),
        ));
        let app = router(state);

        let outage = app
            .clone()
            .oneshot(signed_owned_request(
                "retry-delivery",
                "pull_request",
                BODY.into(),
            ))
            .await
            .unwrap();
        assert_eq!(outage.status(), StatusCode::SERVICE_UNAVAILABLE);

        let recovered = app
            .oneshot(signed_owned_request(
                "retry-delivery",
                "pull_request",
                BODY.into(),
            ))
            .await
            .unwrap();
        assert_eq!(recovered.status(), StatusCode::ACCEPTED);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, calls[1].0);
    }

    #[tokio::test]
    async fn in_progress_duplicate_returns_retryable_status() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let state = Arc::new(AppState::with_store(
            test_config(),
            SqliteStore::memory().unwrap(),
        ));
        let payload_hash = hex::encode(Sha256::digest(BODY.as_bytes()));
        state
            .store
            .as_ref()
            .unwrap()
            .begin_delivery(
                "delivery-processing",
                "pull_request",
                Some("example/repo"),
                &payload_hash,
            )
            .await
            .unwrap();

        let response = router(state)
            .oneshot(signed_request("delivery-processing", BODY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["error"], "delivery_in_progress");
    }

    #[tokio::test]
    async fn rejects_invalid_hmac_before_touching_store() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let state = Arc::new(AppState::with_store(
            test_config(),
            SqliteStore::memory().unwrap(),
        ));
        let request = Request::post("/api/v1/github/webhooks")
            .header("x-github-event", "pull_request")
            .header("x-github-delivery", "delivery-invalid")
            .header("x-hub-signature-256", "sha256=00")
            .body(Body::from(BODY))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn delivery_and_event_headers_are_bounded() {
        assert!(valid_delivery_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!valid_delivery_id("bad/delivery"));
        assert!(!valid_delivery_id(&"a".repeat(129)));
        assert!(valid_event_type("pull_request_review"));
        assert!(!valid_event_type("Pull-Request"));
    }
}
