//! Durable, signed provider-neutral runtime event delivery (ADR 008, P5).

use crate::state::AppState;
use crate::store::{now_ms, ControllerEventDelivery};
use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures::future::BoxFuture;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

const SIGNING_KEYS_ENV: &str = "OABCP_CONTROLLER_EVENT_SIGNING_KEYS";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DELIVERY_LEASE_MS: i64 = 30_000;
const DELIVERY_WINDOW_MS: i64 = 5 * 60 * 1000;
const RETRY_DELAYS_MS: [i64; 3] = [10_000, 30_000, 90_000];

#[derive(Debug, Clone)]
pub struct ControllerEventKeys {
    keys: BTreeMap<i64, Vec<u8>>,
}

impl ControllerEventKeys {
    pub fn new(keys: BTreeMap<i64, Vec<u8>>) -> Result<Self> {
        if keys.is_empty() {
            anyhow::bail!("controller event signing keys must not be empty");
        }
        for (version, key) in &keys {
            if *version <= 0 {
                anyhow::bail!("controller event signing key versions must be positive");
            }
            if key.len() < 32 {
                anyhow::bail!("controller event signing key v{version} must be at least 32 bytes");
            }
        }
        Ok(Self { keys })
    }

    pub fn from_env() -> Result<Option<Self>> {
        let Some(raw) = std::env::var(SIGNING_KEYS_ENV).ok() else {
            return Ok(None);
        };
        let encoded: BTreeMap<String, String> =
            serde_json::from_str(&raw).context("parse controller event signing keys JSON")?;
        let mut keys = BTreeMap::new();
        for (version, encoded_key) in encoded {
            let version = version.parse::<i64>().with_context(|| {
                format!("invalid controller event signing key version '{version}'")
            })?;
            let key = URL_SAFE_NO_PAD
                .decode(encoded_key.as_bytes())
                .with_context(|| format!("decode controller event signing key v{version}"))?;
            keys.insert(version, key);
        }
        Ok(Some(Self::new(keys)?))
    }

    pub fn latest_version(&self) -> i64 {
        *self.keys.keys().next_back().expect("keys are non-empty")
    }

    pub fn signing_secret(&self, version: i64, controller_id: &str) -> Result<Vec<u8>> {
        let key = self
            .keys
            .get(&version)
            .with_context(|| format!("unknown controller event signing key v{version}"))?;
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts arbitrary key size");
        mac.update(b"openab-controller-event-v1\0");
        mac.update(controller_id.as_bytes());
        Ok(mac.finalize().into_bytes().to_vec())
    }

    pub fn issued_secret(&self, version: i64, controller_id: &str) -> Result<String> {
        Ok(URL_SAFE_NO_PAD.encode(self.signing_secret(version, controller_id)?))
    }
}

#[derive(Debug, Clone)]
pub struct ControllerEventRequest {
    pub endpoint: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

pub trait ControllerEventTransport: Send + Sync {
    fn post(&self, request: ControllerEventRequest) -> BoxFuture<'static, Result<u16>>;
}

struct ReqwestControllerEventTransport {
    client: reqwest::Client,
}

impl ReqwestControllerEventTransport {
    fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("build controller event HTTP client")?,
        })
    }
}

impl ControllerEventTransport for ReqwestControllerEventTransport {
    fn post(&self, request: ControllerEventRequest) -> BoxFuture<'static, Result<u16>> {
        let client = self.client.clone();
        Box::pin(async move {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            for (name, value) in request.headers {
                headers.insert(
                    HeaderName::from_bytes(name.as_bytes())?,
                    HeaderValue::from_str(&value)?,
                );
            }
            let response = client
                .post(request.endpoint)
                .headers(headers)
                .body(request.body)
                .send()
                .await?;
            Ok(response.status().as_u16())
        })
    }
}

pub struct ControllerEventRuntime {
    pub keys: ControllerEventKeys,
    transport: Arc<dyn ControllerEventTransport>,
}

impl ControllerEventRuntime {
    pub fn new(keys: ControllerEventKeys, transport: Arc<dyn ControllerEventTransport>) -> Self {
        Self { keys, transport }
    }

    pub fn from_env() -> Result<Option<Arc<Self>>> {
        let Some(keys) = ControllerEventKeys::from_env()? else {
            return Ok(None);
        };
        Ok(Some(Arc::new(Self::new(
            keys,
            Arc::new(ReqwestControllerEventTransport::new()?),
        ))))
    }
}

pub fn validate_https_endpoint(endpoint: &str) -> Result<()> {
    let url = reqwest::Url::parse(endpoint).context("parse controller event endpoint")?;
    if url.scheme() != "https" {
        anyhow::bail!("controller event endpoint must use HTTPS");
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("controller event endpoint must have a host and no userinfo");
    }
    if url.fragment().is_some() {
        anyhow::bail!("controller event endpoint must not contain a fragment");
    }
    Ok(())
}

pub fn request_target(endpoint: &str) -> Result<String> {
    let url = reqwest::Url::parse(endpoint).context("parse controller event endpoint")?;
    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    Ok(target)
}

pub fn signed_request(
    keys: &ControllerEventKeys,
    delivery: &ControllerEventDelivery,
    timestamp_secs: i64,
) -> Result<ControllerEventRequest> {
    let target = request_target(&delivery.endpoint)?;
    let body_hash = hex::encode(Sha256::digest(delivery.body_json.as_bytes()));
    let canonical = format!(
        "v1\n{}\n{}\n{}\nPOST\n{}\n{}",
        delivery.controller_id, delivery.id, timestamp_secs, target, body_hash
    );
    let secret = keys.signing_secret(delivery.key_version, &delivery.controller_id)?;
    let mut mac = HmacSha256::new_from_slice(&secret).expect("HMAC accepts arbitrary key size");
    mac.update(canonical.as_bytes());
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    Ok(ControllerEventRequest {
        endpoint: delivery.endpoint.clone(),
        headers: BTreeMap::from([
            ("X-OAB-Controller-ID".into(), delivery.controller_id.clone()),
            ("X-OAB-Event-ID".into(), delivery.id.clone()),
            ("X-OAB-Timestamp".into(), timestamp_secs.to_string()),
            ("X-OAB-Signature".into(), signature),
        ]),
        body: delivery.body_json.clone(),
    })
}

pub async fn dispatch_once(state: &Arc<AppState>, now: i64) -> Result<usize> {
    let Some(runtime) = state.controller_events.as_ref() else {
        return Ok(0);
    };
    let deliveries = state
        .store
        // One claim per tick preserves durable event order and keeps the lease
        // longer than the ten-second request timeout without batch-tail expiry.
        .claim_controller_events(now, 1, DELIVERY_LEASE_MS)?;
    let count = deliveries.len();
    for delivery in deliveries {
        let result = match signed_request(&runtime.keys, &delivery, now / 1000) {
            Ok(request) => runtime.transport.post(request).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(status) if (200..300).contains(&status) => {
                state
                    .store
                    .complete_controller_event(&delivery.id, now_ms())?;
            }
            outcome => {
                let error = match outcome {
                    Ok(status) => format!("controller endpoint returned HTTP {status}"),
                    Err(error) => error.to_string(),
                };
                let expired = now.saturating_sub(delivery.created_at) >= DELIVERY_WINDOW_MS;
                let next_attempt = if expired || delivery.attempts >= 4 {
                    None
                } else {
                    Some(now.saturating_add(RETRY_DELAYS_MS[(delivery.attempts - 1) as usize]))
                };
                state
                    .store
                    .fail_controller_event(&delivery.id, &error, next_attempt, now_ms())?;
                if next_attempt.is_none() {
                    tracing::error!(
                        controller_id = delivery.controller_id,
                        event_id = delivery.id,
                        %error,
                        "controller event moved to dead letter"
                    );
                }
            }
        }
    }
    Ok(count)
}

pub fn spawn_dispatcher(state: Arc<AppState>) {
    if state.controller_events.is_none() {
        tracing::info!("controller event dispatcher disabled: signing keys not configured");
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if let Err(error) = dispatch_once(&state, now_ms()).await {
                tracing::error!(%error, "controller event dispatch failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        ControllerActionStart, ControllerCredentialHash, ControllerOpenIntent,
        ControllerSessionBinding, SqliteStore, Store,
    };
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTransport {
        responses: Mutex<VecDeque<Result<u16>>>,
        requests: Mutex<Vec<ControllerEventRequest>>,
    }

    impl FakeTransport {
        fn with_statuses(statuses: impl IntoIterator<Item = u16>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(statuses.into_iter().map(Ok).collect()),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    impl ControllerEventTransport for FakeTransport {
        fn post(&self, request: ControllerEventRequest) -> BoxFuture<'static, Result<u16>> {
            self.requests.lock().unwrap().push(request);
            let result = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(204));
            Box::pin(async move { result })
        }
    }

    fn keys() -> ControllerEventKeys {
        ControllerEventKeys::new(BTreeMap::from([(1, vec![11; 32])])).unwrap()
    }

    fn fixture_accepts(
        request: &ControllerEventRequest,
        expected_controller: &str,
        secret: &[u8],
        now_secs: i64,
        seen: &mut BTreeMap<String, i64>,
    ) -> bool {
        let controller_id = &request.headers["X-OAB-Controller-ID"];
        let event_id = &request.headers["X-OAB-Event-ID"];
        let timestamp = request.headers["X-OAB-Timestamp"].parse::<i64>().unwrap();
        seen.retain(|_, accepted_at| now_secs.saturating_sub(*accepted_at) < 600);
        if controller_id != expected_controller
            || now_secs.abs_diff(timestamp) > 300
            || seen.contains_key(event_id)
        {
            return false;
        }
        let target = request_target(&request.endpoint).unwrap();
        let body_hash = hex::encode(Sha256::digest(request.body.as_bytes()));
        let canonical =
            format!("v1\n{controller_id}\n{event_id}\n{timestamp}\nPOST\n{target}\n{body_hash}");
        let Some(signature) = request.headers["X-OAB-Signature"].strip_prefix("sha256=") else {
            return false;
        };
        let Ok(signature) = hex::decode(signature) else {
            return false;
        };
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(canonical.as_bytes());
        if mac.verify_slice(&signature).is_err() {
            return false;
        }
        seen.insert(event_id.clone(), now_secs);
        true
    }

    fn state_with_transport(
        store: Arc<SqliteStore>,
        transport: Arc<dyn ControllerEventTransport>,
    ) -> Arc<AppState> {
        AppState::new_with_options_and_runtime_config(
            store,
            None,
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::pr_review::PrReviewConfig::default(),
            None,
            Some(Arc::new(ControllerEventRuntime::new(keys(), transport))),
        )
    }

    fn seed_opened_event(store: &SqliteStore, now: i64) -> String {
        store
            .upsert_controller_installation("ctrl-events", 5, 60)
            .unwrap();
        store
            .put_controller_action_token("tok-events", "ctrl-events", &[4; 32], 1, now - 1, None)
            .unwrap();
        store
            .set_controller_action_grant("ctrl-events", "open_session", true)
            .unwrap();
        store
            .set_controller_scope_binding("ctrl-events", "scope:events", true)
            .unwrap();
        store
            .configure_controller_events(
                "ctrl-events",
                "https://controller.example.test/hook?source=ocp%2Fv1",
                1,
                &["session.opened".into()],
                now,
            )
            .unwrap();
        let session = store
            .create_session("events", Some("controller:events"), 1, None, &[], "solo")
            .unwrap();
        let intent = ControllerOpenIntent {
            trigger_ref: "trigger:events".into(),
            trigger_fingerprint: Some("v1".into()),
        };
        assert!(matches!(
            store
                .begin_controller_action(
                    "ctrl-events",
                    &[ControllerCredentialHash {
                        pepper_version: 1,
                        token_hash: vec![4; 32],
                    }],
                    "act-events",
                    &[8; 32],
                    "open_session",
                    "scope:events",
                    None,
                    Some(&intent),
                    now,
                )
                .unwrap(),
            ControllerActionStart::Started { .. }
        ));
        store
            .finish_controller_action(
                "ctrl-events",
                "act-events",
                200,
                "{}",
                Some(&ControllerSessionBinding {
                    controller_id: "ctrl-events".into(),
                    scope: "scope:events".into(),
                    trigger_ref: "trigger:events".into(),
                    trigger_fingerprint: Some("v1".into()),
                    session_id: session.id.clone(),
                }),
                now,
            )
            .unwrap();
        session.id
    }

    #[test]
    fn canonical_signature_uses_exact_target_and_body_bytes() {
        let delivery = ControllerEventDelivery {
            id: "evt-1".into(),
            controller_id: "ctrl-1".into(),
            session_id: Some("ses-1".into()),
            event_type: "session.terminal".into(),
            endpoint: "https://example.test/a%2Fb?x=1%202".into(),
            key_version: 1,
            body_json: "{\"z\":1,\"a\":2}".into(),
            attempts: 1,
            created_at: 1,
        };
        let request = signed_request(&keys(), &delivery, 1_700_000_000).unwrap();
        assert_eq!(
            request_target(&delivery.endpoint).unwrap(),
            "/a%2Fb?x=1%202"
        );
        let canonical = format!(
            "v1\nctrl-1\nevt-1\n1700000000\nPOST\n/a%2Fb?x=1%202\n{}",
            hex::encode(Sha256::digest(delivery.body_json.as_bytes()))
        );
        let secret = keys().signing_secret(1, "ctrl-1").unwrap();
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(canonical.as_bytes());
        assert_eq!(
            request.headers["X-OAB-Signature"],
            format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
        );
        assert_eq!(request.body, delivery.body_json);
    }

    #[test]
    fn event_endpoint_requires_https_host_without_userinfo_or_fragment() {
        assert!(validate_https_endpoint("https://controller.example.test/events?x=1").is_ok());
        for endpoint in [
            "http://controller.example.test/events",
            "https://user@example.test/events",
            "https://controller.example.test/events#fragment",
            "not-a-url",
        ] {
            assert!(
                validate_https_endpoint(endpoint).is_err(),
                "endpoint should be rejected: {endpoint}"
            );
        }
    }

    #[test]
    fn conformance_receiver_rejects_stale_timestamps_and_replays() {
        let delivery = ControllerEventDelivery {
            id: "evt-replay".into(),
            controller_id: "ctrl-replay".into(),
            session_id: None,
            event_type: "action.failed".into(),
            endpoint: "https://example.test/events".into(),
            key_version: 1,
            body_json: "{\"event\":\"failure\"}".into(),
            attempts: 1,
            created_at: 1,
        };
        let secret = keys().signing_secret(1, "ctrl-replay").unwrap();
        let mut seen = BTreeMap::new();
        let valid = signed_request(&keys(), &delivery, 1_700_000_000).unwrap();
        assert!(fixture_accepts(
            &valid,
            "ctrl-replay",
            &secret,
            1_700_000_100,
            &mut seen
        ));
        assert!(!fixture_accepts(
            &valid,
            "ctrl-replay",
            &secret,
            1_700_000_101,
            &mut seen
        ));
        let stale = signed_request(&keys(), &delivery, 1_699_999_000).unwrap();
        assert!(!fixture_accepts(
            &stale,
            "ctrl-replay",
            &secret,
            1_700_000_100,
            &mut BTreeMap::new()
        ));
    }

    #[tokio::test]
    async fn retries_at_10_30_90_seconds_then_dead_letters_with_audit() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let start = 1_000_000;
        seed_opened_event(&store, start);
        let transport = FakeTransport::with_statuses([500, 500, 500, 500]);
        let state = state_with_transport(store.clone(), transport.clone());

        assert_eq!(dispatch_once(&state, start).await.unwrap(), 1);
        assert_eq!(dispatch_once(&state, start + 9_999).await.unwrap(), 0);
        assert_eq!(dispatch_once(&state, start + 10_000).await.unwrap(), 1);
        assert_eq!(dispatch_once(&state, start + 39_999).await.unwrap(), 0);
        assert_eq!(dispatch_once(&state, start + 40_000).await.unwrap(), 1);
        assert_eq!(dispatch_once(&state, start + 129_999).await.unwrap(), 0);
        assert_eq!(dispatch_once(&state, start + 130_000).await.unwrap(), 1);

        let audit = store.controller_event_audit("ctrl-events").unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].kind, "dead_letter");
        assert_eq!(transport.requests.lock().unwrap().len(), 4);
    }

    #[test]
    fn expired_delivery_lease_is_reclaimed_after_restart() {
        let store = SqliteStore::memory().unwrap();
        let start = 2_000_000;
        seed_opened_event(&store, start);
        store
            .set_controller_installation_enabled("ctrl-events", false)
            .unwrap();
        assert!(store
            .claim_controller_events(start, 1, 30_000)
            .unwrap()
            .is_empty());
        store
            .set_controller_installation_enabled("ctrl-events", true)
            .unwrap();
        store
            .configure_controller_events(
                "ctrl-events",
                "https://replacement.example.test/events",
                1,
                &["session.opened".into()],
                start + 1,
            )
            .unwrap();
        let first = store.claim_controller_events(start, 1, 30_000).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].attempts, 1);
        assert_eq!(
            first[0].endpoint, "https://controller.example.test/hook?source=ocp%2Fv1",
            "queued deliveries keep the endpoint approved when they were persisted"
        );
        assert!(store
            .claim_controller_events(start + 29_999, 1, 30_000)
            .unwrap()
            .is_empty());
        let reclaimed = store
            .claim_controller_events(start + 30_000, 1, 30_000)
            .unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].id, first[0].id);
        assert_eq!(reclaimed[0].attempts, 2);
    }
}
