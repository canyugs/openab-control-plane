#![forbid(unsafe_code)]

pub mod config;
pub mod ocp;
pub mod planner;
pub mod runtime_events;
pub mod shadow;
pub mod store;

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

type HmacSha256 = Hmac<Sha256>;
const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;
const DELIVERY_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub struct AppState {
    pub config: Config,
    pub store: Option<Arc<ProductStore>>,
    pub store_error: Option<String>,
    pub action_client: Option<Arc<dyn ocp::OcpActionClient>>,
    pub action_client_error: Option<String>,
    pub event_verifier: Option<Arc<runtime_events::RuntimeEventVerifier>>,
    pub event_verifier_error: Option<String>,
}

impl AppState {
    pub fn from_config(config: Config) -> Self {
        let (store, store_error) = match ProductStore::open(&config.db_path) {
            Ok(store) => (Some(Arc::new(store)), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let (action_client, action_client_error) =
            if matches!(config.mode, OperatingMode::ExternalCanary)
                && config.ocp_action.is_complete()
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
                if matches!(config.mode, OperatingMode::ExternalCanary) =>
            {
                match runtime_events::RuntimeEventVerifier::new(controller_id, secret) {
                    Ok(verifier) => (Some(Arc::new(verifier)), None),
                    Err(error) => (None, Some(error.to_string())),
                }
            }
            _ => (None, None),
        };
        Self {
            config,
            store,
            store_error,
            action_client,
            action_client_error,
            event_verifier,
            event_verifier_error,
        }
    }

    pub fn with_components(
        config: Config,
        store: ProductStore,
        action_client: Option<Arc<dyn ocp::OcpActionClient>>,
        event_verifier: Option<Arc<runtime_events::RuntimeEventVerifier>>,
    ) -> Self {
        Self {
            config,
            store: Some(Arc::new(store)),
            store_error: None,
            action_client,
            action_client_error: None,
            event_verifier,
            event_verifier_error: None,
        }
    }

    #[cfg(test)]
    fn with_store(config: Config, store: ProductStore) -> Self {
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
        let github = self.config.github_app.readiness();
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
            && !github.enabled
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
            match store.prune_completed_deliveries() {
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
    match store.begin_delivery(delivery_id, event_type, repository, &payload_hash) {
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

    let (durable_state, result) = match candidate_plan(&state, delivery_id, event_type, &payload) {
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
                let _ = store.release_delivery_for_retry(delivery_id, &result);
                return response(StatusCode::SERVICE_UNAVAILABLE, result);
            };
            match client
                .open_session(action_id.clone(), plan.open_session_action())
                .await
            {
                Ok(action_result) => (
                    "acted",
                    json!({
                        "ok": true,
                        "planned": true,
                        "acted": true,
                        "action_id": action_id,
                        "action_result": action_result,
                        "plan": plan,
                    }),
                ),
                Err(error) => {
                    tracing::warn!(
                        %delivery_id,
                        action_id,
                        error = ?error,
                        "external canary action failed; retaining provider retry path"
                    );
                    let result = json!({"ok": false, "error": error.public_code()});
                    if let Err(store_error) = store.release_delivery_for_retry(delivery_id, &result)
                    {
                        tracing::error!(%store_error, %delivery_id, "retryable delivery persistence failed");
                    }
                    return response(StatusCode::SERVICE_UNAVAILABLE, result);
                }
            }
        }
    };
    if let Err(error) = store.finish_delivery(delivery_id, durable_state, &result) {
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
    match store.record_runtime_event(&body_hash, &event) {
        Ok(RuntimeEventAdmission::New) => {
            tracing::info!(
                event_id = event.event_id,
                event_type = event.event_type,
                session_id = event.session_id,
                "accepted signed runtime event"
            );
            response(StatusCode::OK, json!({"ok": true, "duplicate": false}))
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
    match store.canary_summary() {
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
    ) {
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
    match store.record_shadow_comparison(&request_hash, repository, &report) {
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
    match store.shadow_summary() {
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

fn candidate_plan(
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
    Ok(planner::build_plan(
        delivery_id,
        trigger,
        &state.config.roster,
        state.config.council_preset.as_deref(),
        &state.config.review_mode,
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
            ProductStore::memory().unwrap(),
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
        let state = Arc::new(AppState::with_store(
            config,
            ProductStore::memory().unwrap(),
        ));
        let response = router(state)
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn signed_shadow_comparison_records_exact_fixture_parity() {
        const BODY: &str = include_str!("../../../tests/fixtures/github/pull_request_opened.json");
        let state = Arc::new(AppState::with_store(
            test_config(),
            ProductStore::memory().unwrap(),
        ));
        let payload: Value = serde_json::from_str(BODY).unwrap();
        let embedded = shadow::ParityOutcome::Planned {
            snapshot: Box::new(
                candidate_plan(&state, "delivery-shadow", "pull_request", &payload)
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
            ProductStore::memory().unwrap(),
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
            ProductStore::memory().unwrap(),
            Some(Arc::new(client)),
            Some(Arc::new(verifier)),
        ));
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
            ProductStore::memory().unwrap(),
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
            ProductStore::memory().unwrap(),
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
            ProductStore::memory().unwrap(),
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
            ProductStore::memory().unwrap(),
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
            ProductStore::memory().unwrap(),
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
