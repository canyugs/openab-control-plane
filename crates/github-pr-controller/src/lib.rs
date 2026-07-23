#![forbid(unsafe_code)]

pub mod config;
pub mod planner;
pub mod shadow;
pub mod store;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::{ComponentReadiness, Config};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use store::{DeliveryAdmission, ProductStore, ShadowAdmission};

type HmacSha256 = Hmac<Sha256>;
const MAX_WEBHOOK_BODY_BYTES: usize = 1024 * 1024;
const DELIVERY_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub struct AppState {
    pub config: Config,
    pub store: Option<Arc<ProductStore>>,
    pub store_error: Option<String>,
}

impl AppState {
    pub fn from_config(config: Config) -> Self {
        match ProductStore::open(&config.db_path) {
            Ok(store) => Self {
                config,
                store: Some(Arc::new(store)),
                store_error: None,
            },
            Err(error) => Self {
                config,
                store: None,
                store_error: Some(error.to_string()),
            },
        }
    }

    #[cfg(test)]
    fn with_store(config: Config, store: ProductStore) -> Self {
        Self {
            config,
            store: Some(Arc::new(store)),
            store_error: None,
        }
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
        let ready = ingress.ready && product_store.ready && !github.enabled;
        ReadinessReport {
            status: if ready { "ready" } else { "not_ready" },
            mode: "plan_only",
            components: Components {
                ingress,
                ocp: ComponentReadiness::disabled("action client disabled in plan-only mode"),
                github,
                product_store,
            },
        }
    }
}

#[derive(Serialize)]
struct ReadinessReport {
    status: &'static str,
    mode: &'static str,
    components: Components,
}

#[derive(Serialize)]
struct Components {
    ingress: ComponentReadiness,
    ocp: ComponentReadiness,
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
        "mode": "plan_only",
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
    let Some(store) = state.store.as_ref() else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "product_store_unavailable"}),
        );
    };

    let repository = payload["repository"]["full_name"].as_str();
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

    let result = decide(&state, delivery_id, event_type, &payload);
    let durable_state = if result["planned"].as_bool() == Some(true) {
        "planned"
    } else {
        "ignored"
    };
    if let Err(error) = store.finish_delivery(delivery_id, durable_state, &result) {
        tracing::error!(%error, %delivery_id, "delivery completion failed");
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"ok": false, "error": "delivery_store_failed"}),
        );
    }

    let status = if durable_state == "planned" {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    response(status, result)
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

fn decide(state: &AppState, delivery_id: &str, event_type: &str, payload: &Value) -> Value {
    let plan = match candidate_plan(state, delivery_id, event_type, payload) {
        Ok(plan) => plan,
        Err(reason) => return json!({"ok": true, "planned": false, "reason": reason}),
    };
    json!({"ok": true, "planned": true, "plan": plan})
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
    use http_body_util::BodyExt;
    use std::collections::BTreeSet;
    use tower::ServiceExt;

    fn test_config() -> Config {
        Config {
            addr: "127.0.0.1:0".into(),
            db_path: ":memory:".into(),
            webhook_secret: Some("fixture-secret".into()),
            shadow_secret: Some("shadow-secret".into()),
            allowed_repos: BTreeSet::from(["example/repo".into()]),
            bot_handle: Some("fixture-council".into()),
            roster: vec!["chair".into(), "rev1".into(), "rev2".into()],
            council_preset: None,
            review_mode: "approve".into(),
            github_app: config::GitHubAppConfig {
                app_id: None,
                installation_id: None,
                private_key: None,
            },
        }
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
