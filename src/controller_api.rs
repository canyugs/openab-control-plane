//! Versioned external-controller action boundary (ADR 008, migration P4).
//!
//! External controllers authenticate with install-scoped, HMAC-hashed bearer
//! tokens and can mutate runtime state only through the same interpreter used
//! by bundled callers. The store owns atomic grant/scope/quota/idempotency
//! admission; this module owns transport validation and stable protocol errors.

use crate::controller::{
    self, ControlledClosePolicy, ControllerAction, ControllerActionResult, ControllerError,
};
use crate::state::AppState;
use crate::store::{
    new_id, now_ms, ControllerActionDenial, ControllerActionStart, ControllerCredentialHash,
    ControllerOpenDecision, ControllerOpenIntent, ControllerSessionBinding,
    NewControllerActionToken,
};
use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use controller_protocol::{
    ActionEnvelope, ActionResultEnvelope, ErrorCode, ErrorEnvelope, ProtocolError, CURRENT_VERSION,
};
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error as _;
use std::sync::Arc;

type HmacSha256 = Hmac<Sha256>;

const MAX_ACTION_BODY_BYTES: usize = 1024 * 1024;
const ACTION_ID_HEADER: &str = "x-oab-action-id";
const SCOPE_HEADER: &str = "x-oab-scope";
const PEPPERS_ENV: &str = "OABCP_CONTROLLER_ACTION_PEPPERS";
const TOKEN_ROTATION_OVERLAP_MS: i64 = 15 * 60 * 1000;

/// Deployment-held versioned HMAC keys. The environment value is a JSON map,
/// for example `{"1":"<base64url-32+-bytes>"}`. SQLite stores only the
/// selected version and the HMAC output.
#[derive(Debug, Clone)]
pub struct ControllerAuthConfig {
    peppers: BTreeMap<i64, Vec<u8>>,
}

impl ControllerAuthConfig {
    pub fn new(peppers: BTreeMap<i64, Vec<u8>>) -> Result<Self> {
        if peppers.is_empty() {
            anyhow::bail!("controller action peppers must not be empty");
        }
        for (version, pepper) in &peppers {
            if *version <= 0 {
                anyhow::bail!("controller action pepper versions must be positive");
            }
            if pepper.len() < 32 {
                anyhow::bail!("controller action pepper v{version} must be at least 32 bytes");
            }
        }
        Ok(Self { peppers })
    }

    pub fn from_env() -> Result<Option<Self>> {
        let Some(raw) = std::env::var(PEPPERS_ENV).ok() else {
            return Ok(None);
        };
        let encoded: BTreeMap<String, String> =
            serde_json::from_str(&raw).context("parse controller action peppers JSON")?;
        let mut peppers = BTreeMap::new();
        for (version, value) in encoded {
            let version = version
                .parse::<i64>()
                .with_context(|| format!("invalid controller action pepper version '{version}'"))?;
            let pepper = URL_SAFE_NO_PAD
                .decode(value.as_bytes())
                .with_context(|| format!("decode controller action pepper v{version}"))?;
            peppers.insert(version, pepper);
        }
        Ok(Some(Self::new(peppers)?))
    }

    pub fn hash_token(&self, pepper_version: i64, token: &str) -> Result<Vec<u8>> {
        let pepper = self
            .peppers
            .get(&pepper_version)
            .with_context(|| format!("unknown controller action pepper v{pepper_version}"))?;
        let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC accepts arbitrary key size");
        mac.update(token.as_bytes());
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn pepper(&self, version: i64) -> Option<&[u8]> {
        self.peppers.get(&version).map(Vec::as_slice)
    }

    fn first_pepper(&self) -> &[u8] {
        self.peppers
            .values()
            .next()
            .expect("ControllerAuthConfig rejects empty maps")
    }

    fn latest_version(&self) -> i64 {
        *self
            .peppers
            .keys()
            .next_back()
            .expect("ControllerAuthConfig rejects empty maps")
    }

    fn credential_hashes(&self, token: &str) -> Vec<ControllerCredentialHash> {
        self.peppers
            .iter()
            .map(|(pepper_version, pepper)| {
                let mut mac =
                    HmacSha256::new_from_slice(pepper).expect("HMAC accepts arbitrary key size");
                mac.update(token.as_bytes());
                ControllerCredentialHash {
                    pepper_version: *pepper_version,
                    token_hash: mac.finalize().into_bytes().to_vec(),
                }
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateControllerInstallation {
    pub id: String,
    pub actions: Vec<String>,
    pub scopes: Vec<String>,
    #[serde(default = "default_max_concurrent_sessions")]
    pub max_concurrent_sessions: i64,
    #[serde(default = "default_max_actions_per_minute")]
    pub max_actions_per_minute: i64,
}

#[derive(Debug, Serialize)]
pub struct IssuedControllerToken {
    pub controller_id: String,
    pub token_id: String,
    /// Returned exactly once. SQLite stores only its versioned HMAC.
    pub action_token: String,
    pub pepper_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlap_until: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetControllerState {
    // ADR 034: every field optional, absent = untouched. The historical
    // enabled-only body is the degenerate case, so old callers keep working.
    pub enabled: Option<bool>,
    pub max_concurrent_sessions: Option<i64>,
    pub max_actions_per_minute: Option<i64>,
    pub actions: Option<Vec<String>>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureControllerEvents {
    pub endpoint: String,
    pub events: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct IssuedControllerEventSecret {
    pub controller_id: String,
    pub endpoint: String,
    pub events: Vec<String>,
    pub key_version: i64,
    /// Returned exactly once. The secret is derived from a deployment-held key;
    /// SQLite stores only its version.
    pub event_signing_secret: String,
}

fn default_max_concurrent_sessions() -> i64 {
    5
}

fn default_max_actions_per_minute() -> i64 {
    60
}

/// Root-operator installation seam. The action token is generated with 256
/// bits from the OS RNG and returned once; only its HMAC lands in SQLite.
pub async fn create_installation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateControllerInstallation>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    let Some(auth) = state.controller_auth.as_ref() else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "controller action auth is not configured",
        );
    };
    let (actions, scopes) = match validate_installation_request(&request) {
        Ok(values) => values,
        Err(message) => return admin_error(StatusCode::BAD_REQUEST, &message),
    };
    let (issued, token) = match generate_controller_token(auth, &request.id, None, now_ms()) {
        Ok(token) => token,
        Err(error) => return admin_store_error(error),
    };
    match state.store.provision_controller_installation(
        &request.id,
        request.max_concurrent_sessions,
        request.max_actions_per_minute,
        &actions,
        &scopes,
        &token,
    ) {
        Ok(true) => json_response(StatusCode::CREATED, &issued, None),
        Ok(false) => admin_error(
            StatusCode::CONFLICT,
            "controller installation already exists",
        ),
        Err(error) => admin_store_error(error),
    }
}

/// Rotate an install token while bounding the old credentials to ADR 008's
/// default 15-minute overlap window.
pub async fn rotate_installation_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(controller_id): Path<String>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    let Some(auth) = state.controller_auth.as_ref() else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "controller action auth is not configured",
        );
    };
    let now = now_ms();
    let overlap_until = now.saturating_add(TOKEN_ROTATION_OVERLAP_MS);
    let (issued, token) =
        match generate_controller_token(auth, &controller_id, Some(overlap_until), now) {
            Ok(token) => token,
            Err(error) => return admin_store_error(error),
        };
    match state
        .store
        .rotate_controller_action_token(&controller_id, &token, overlap_until)
    {
        Ok(true) => json_response(StatusCode::CREATED, &issued, None),
        Ok(false) => admin_error(StatusCode::NOT_FOUND, "unknown controller installation"),
        Err(error) => admin_store_error(error),
    }
}

pub async fn revoke_installation_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((controller_id, token_id)): Path<(String, String)>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    match state
        .store
        .revoke_controller_action_token(&controller_id, &token_id, now_ms())
    {
        Ok(true) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "revoked": true }),
            None,
        ),
        Ok(false) => admin_error(StatusCode::NOT_FOUND, "unknown active controller token"),
        Err(error) => admin_store_error(error),
    }
}

pub async fn set_installation_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(controller_id): Path<String>,
    Json(request): Json<SetControllerState>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    if request.enabled.is_none()
        && request.max_concurrent_sessions.is_none()
        && request.max_actions_per_minute.is_none()
        && request.actions.is_none()
        && request.scopes.is_none()
    {
        return admin_error(StatusCode::BAD_REQUEST, "empty patch: nothing to change");
    }
    if request.max_concurrent_sessions.is_some_and(|limit| limit <= 0)
        || request.max_actions_per_minute.is_some_and(|limit| limit <= 0)
    {
        return admin_error(StatusCode::BAD_REQUEST, "controller quotas must be positive");
    }
    let actions = match request.actions.as_deref().map(validate_actions).transpose() {
        Ok(actions) => actions,
        Err(message) => return admin_error(StatusCode::BAD_REQUEST, &message),
    };
    let scopes = match request.scopes.as_deref().map(validate_scopes).transpose() {
        Ok(scopes) => scopes,
        Err(message) => return admin_error(StatusCode::BAD_REQUEST, &message),
    };
    match state.store.patch_controller_installation(
        &controller_id,
        request.enabled,
        request.max_concurrent_sessions,
        request.max_actions_per_minute,
        actions.as_deref(),
        scopes.as_deref(),
    ) {
        // The response is the full post-state, never an echo of the patch:
        // an untouched field reads as its real value, not as null.
        Ok(Some(config)) => json_response(
            StatusCode::OK,
            &serde_json::json!({
                "controller_id": controller_id,
                "enabled": config.enabled,
                "max_concurrent_sessions": config.max_concurrent_sessions,
                "max_actions_per_minute": config.max_actions_per_minute,
                "actions": config.actions,
                "scopes": config.scopes,
            }),
            None,
        ),
        Ok(None) => admin_error(StatusCode::NOT_FOUND, "unknown controller installation"),
        Err(error) => admin_store_error(error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateReviewWaiver {
    pub repo: String,
    pub path_class: Option<String>,
    pub text: String,
    pub origin_pr: Option<String>,
    pub created_by: String,
    pub expires_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReviewWaiver {
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub revoke: bool,
}

#[derive(Debug, Deserialize)]
pub struct ListReviewWaivers {
    pub repo: Option<String>,
    /// Accepts 1/true/yes — query strings are not JSON booleans.
    #[serde(default, deserialize_with = "flag_from_query")]
    pub all: bool,
}

fn flag_from_query<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(matches!(raw.as_str(), "1" | "true" | "yes"))
}

/// ADR 035 P1: the human gate. Only the operator key writes waivers; PR
/// content never reaches this surface.
pub async fn create_review_waiver(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateReviewWaiver>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    let repo = request.repo.trim();
    let text = request.text.trim();
    let created_by = request.created_by.trim();
    if repo.is_empty() || repo.len() > 200 {
        return admin_error(StatusCode::BAD_REQUEST, "repo must be 1..=200 bytes");
    }
    if text.is_empty() || text.len() > 2000 {
        return admin_error(StatusCode::BAD_REQUEST, "text must be 1..=2000 bytes");
    }
    if created_by.is_empty() || created_by.len() > 100 {
        return admin_error(StatusCode::BAD_REQUEST, "created_by must be 1..=100 bytes");
    }
    if request.path_class.as_deref().is_some_and(|v| v.len() > 300) {
        return admin_error(StatusCode::BAD_REQUEST, "path_class must be <=300 bytes");
    }
    if request.origin_pr.as_deref().is_some_and(|v| v.len() > 200) {
        return admin_error(StatusCode::BAD_REQUEST, "origin_pr must be <=200 bytes");
    }
    if request.expires_at <= now_ms() {
        return admin_error(
            StatusCode::BAD_REQUEST,
            "expires_at must be in the future — waivers without expiry are how blindness fossilizes",
        );
    }
    match state.store.create_review_waiver(
        repo,
        request.path_class.as_deref().map(str::trim),
        text,
        request.origin_pr.as_deref().map(str::trim),
        created_by,
        request.expires_at,
    ) {
        Ok(waiver) => json_response(StatusCode::CREATED, &waiver, None),
        Err(error) => admin_store_error(error),
    }
}

pub async fn list_review_waivers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<ListReviewWaivers>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    match state
        .store
        .list_review_waivers(query.repo.as_deref(), query.all, now_ms())
    {
        Ok(waivers) => json_response(StatusCode::OK, &waivers, None),
        Err(error) => admin_store_error(error),
    }
}

pub async fn update_review_waiver(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(waiver_id): Path<String>,
    Json(request): Json<UpdateReviewWaiver>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    if request.expires_at.is_none() && !request.revoke {
        return admin_error(StatusCode::BAD_REQUEST, "empty patch: nothing to change");
    }
    if request.expires_at.is_some_and(|at| at <= now_ms()) {
        return admin_error(StatusCode::BAD_REQUEST, "expires_at must be in the future");
    }
    match state
        .store
        .update_review_waiver(&waiver_id, request.expires_at, request.revoke)
    {
        Ok(true) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "id": waiver_id, "revoked": request.revoke }),
            None,
        ),
        Ok(false) => admin_error(StatusCode::NOT_FOUND, "unknown waiver"),
        Err(error) => admin_store_error(error),
    }
}

pub async fn configure_installation_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(controller_id): Path<String>,
    Json(request): Json<ConfigureControllerEvents>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    let Some(runtime) = state.controller_events.as_ref() else {
        return admin_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "controller event signing keys are not configured",
        );
    };
    if let Err(error) = crate::controller_events::validate_https_endpoint(&request.endpoint) {
        return admin_error(StatusCode::BAD_REQUEST, &error.to_string());
    }
    let events = normalized_unique(&request.events);
    let allowed = [
        "session.opened",
        "session.progress",
        "session.terminal",
        "session.timeout",
        "session.superseded",
        "action.failed",
    ];
    if events.is_empty()
        || events
            .iter()
            .any(|event_type| !allowed.contains(&event_type.as_str()))
    {
        return admin_error(
            StatusCode::BAD_REQUEST,
            "events must contain only supported provider-neutral v1 event names",
        );
    }
    let key_version = runtime.keys.latest_version();
    let event_signing_secret = match runtime.keys.issued_secret(key_version, &controller_id) {
        Ok(secret) => secret,
        Err(error) => return admin_store_error(error),
    };
    match state.store.configure_controller_events(
        &controller_id,
        &request.endpoint,
        key_version,
        &events,
        now_ms(),
    ) {
        Ok(true) => json_response(
            StatusCode::OK,
            &IssuedControllerEventSecret {
                controller_id,
                endpoint: request.endpoint,
                events,
                key_version,
                event_signing_secret,
            },
            None,
        ),
        Ok(false) => admin_error(StatusCode::NOT_FOUND, "unknown controller installation"),
        Err(error) => admin_store_error(error),
    }
}

pub async fn installation_event_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(controller_id): Path<String>,
) -> Response {
    if let Err(error) = check_operator_auth(&state, &headers) {
        return error.response();
    }
    match state.store.controller_event_audit(&controller_id) {
        Ok(entries) => json_response(StatusCode::OK, &entries, None),
        Err(error) => admin_store_error(error),
    }
}

fn validate_installation_request(
    request: &CreateControllerInstallation,
) -> std::result::Result<(Vec<String>, Vec<String>), String> {
    if request.id.is_empty()
        || request.id.len() > 128
        || !request
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "controller id must use 1 to 128 ASCII letters, digits, '.', '_' or '-'".into(),
        );
    }
    if request.max_concurrent_sessions <= 0 || request.max_actions_per_minute <= 0 {
        return Err("controller quotas must be positive".into());
    }
    let actions = validate_actions(&request.actions)?;
    let scopes = validate_scopes(&request.scopes)?;
    Ok((actions, scopes))
}

fn validate_actions(values: &[String]) -> std::result::Result<Vec<String>, String> {
    let allowed_actions = [
        "open_session",
        "post_message",
        "add_roster",
        "close_session",
        "emit_status",
    ];
    let actions = normalized_unique(values);
    if actions.is_empty()
        || actions
            .iter()
            .any(|action| !allowed_actions.contains(&action.as_str()))
    {
        return Err("actions must contain only supported v1 action names".into());
    }
    Ok(actions)
}

fn validate_scopes(values: &[String]) -> std::result::Result<Vec<String>, String> {
    let scopes = normalized_unique(values);
    if scopes.is_empty()
        || scopes
            .iter()
            .any(|scope| scope.contains('*') || scope.len() > 512)
    {
        return Err("scopes must be explicit non-wildcard values up to 512 bytes".into());
    }
    Ok(scopes)
}

fn normalized_unique(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn generate_controller_token(
    auth: &ControllerAuthConfig,
    controller_id: &str,
    overlap_until: Option<i64>,
    not_before: i64,
) -> Result<(IssuedControllerToken, NewControllerActionToken)> {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let action_token = URL_SAFE_NO_PAD.encode(bytes);
    let token_id = new_id("ctok");
    let pepper_version = auth.latest_version();
    let token_hash = auth.hash_token(pepper_version, &action_token)?;
    Ok((
        IssuedControllerToken {
            controller_id: controller_id.to_string(),
            token_id: token_id.clone(),
            action_token,
            pepper_version,
            overlap_until,
        },
        NewControllerActionToken {
            id: token_id,
            token_hash,
            pepper_version,
            not_before,
        },
    ))
}

pub async fn execute_action(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, MAX_ACTION_BODY_BYTES).await {
        Ok(body) => body,
        Err(error)
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>()) =>
        {
            return protocol_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                header_text(&parts.headers, ACTION_ID_HEADER),
                ErrorCode::InvalidRequest,
                "controller action body exceeds 1 MiB",
                false,
                None,
            )
        }
        Err(error) => {
            tracing::warn!(%error, "read controller action body failed");
            return protocol_error_response(
                StatusCode::BAD_REQUEST,
                header_text(&parts.headers, ACTION_ID_HEADER),
                ErrorCode::InvalidRequest,
                "controller action body could not be read",
                false,
                None,
            );
        }
    };
    execute_action_request(&state, &parts.headers, &body)
}

fn execute_action_request(state: &Arc<AppState>, headers: &HeaderMap, body: &[u8]) -> Response {
    let Some(auth) = state.controller_auth.as_ref() else {
        return protocol_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            None,
            ErrorCode::Internal,
            "external controller action API is not configured",
            true,
            None,
        );
    };
    let Some(token) = bearer(headers) else {
        return unauthorized_response();
    };
    let controller_id = match authenticate_controller(state, auth, token) {
        Ok(Some(controller_id)) => controller_id,
        Ok(None) => return unauthorized_response(),
        Err(error) => {
            tracing::error!(%error, "controller action authentication failed");
            return protocol_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
                ErrorCode::Internal,
                "controller authentication unavailable",
                true,
                None,
            );
        }
    };
    if body.len() > MAX_ACTION_BODY_BYTES {
        return protocol_error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            None,
            ErrorCode::InvalidRequest,
            "controller action body exceeds 1 MiB",
            false,
            None,
        );
    }

    let envelope: ActionEnvelope = match serde_json::from_slice(body) {
        Ok(envelope) => envelope,
        Err(_) => {
            return protocol_error_response(
                StatusCode::BAD_REQUEST,
                header_text(headers, ACTION_ID_HEADER),
                ErrorCode::InvalidRequest,
                "invalid controller action envelope",
                false,
                None,
            )
        }
    };
    let action_id = envelope.action_id.clone();
    if envelope.version != CURRENT_VERSION {
        return json_response(
            StatusCode::BAD_REQUEST,
            &ErrorEnvelope::unsupported_version(Some(action_id), envelope.version),
            None,
        );
    }
    let Some(header_action_id) = header_text(headers, ACTION_ID_HEADER) else {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            Some(action_id),
            ErrorCode::InvalidRequest,
            "missing X-OAB-Action-ID header",
            false,
            None,
        );
    };
    if header_action_id != action_id {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            Some(action_id),
            ErrorCode::InvalidRequest,
            "X-OAB-Action-ID does not match envelope action_id",
            false,
            None,
        );
    }
    if action_id.is_empty() || action_id.len() > 200 {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            Some(action_id),
            ErrorCode::InvalidRequest,
            "action_id must contain 1 to 200 bytes",
            false,
            None,
        );
    }
    let Some(scope) = header_text(headers, SCOPE_HEADER) else {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            Some(action_id),
            ErrorCode::InvalidRequest,
            "missing X-OAB-Scope header",
            false,
            None,
        );
    };
    if scope.is_empty() || scope.len() > 512 {
        return protocol_error_response(
            StatusCode::BAD_REQUEST,
            Some(action_id),
            ErrorCode::InvalidRequest,
            "scope must contain 1 to 512 bytes",
            false,
            None,
        );
    }

    // One in-process serialization point closes the duplicate-action race
    // around interpreter execution. SQLite's IMMEDIATE transaction still owns
    // the durable admission decision and protects multi-threaded store callers.
    let _execution_guard = state.controller_action_lock.lock().unwrap();
    let credential_hashes = auth.credential_hashes(token);
    let mut request_digest = Sha256::new();
    request_digest.update(scope.as_bytes());
    request_digest.update([0]);
    request_digest.update(body);
    let request_hash = request_digest.finalize().to_vec();
    let action_kind = action_kind(&envelope.action);
    let open_intent = match &envelope.action {
        ControllerAction::OpenSession(action) => {
            let Some(trigger_ref) = action.trigger_ref.as_deref() else {
                return protocol_error_response(
                    StatusCode::BAD_REQUEST,
                    Some(action_id),
                    ErrorCode::InvalidRequest,
                    "external open_session requires trigger_ref",
                    false,
                    None,
                );
            };
            Some(ControllerOpenIntent {
                trigger_ref: trigger_ref.to_string(),
                trigger_fingerprint: action.trigger_fingerprint.clone(),
            })
        }
        _ => None,
    };
    let session_id = action_session_id(&envelope.action);
    let started = match state.store.begin_controller_action(
        &controller_id,
        &credential_hashes,
        &action_id,
        &request_hash,
        action_kind,
        &scope,
        session_id,
        open_intent.as_ref(),
        now_ms(),
    ) {
        Ok(started) => started,
        Err(error) => return internal_store_error(Some(action_id), error),
    };
    let open_decision = match started {
        ControllerActionStart::Replay(replay) => {
            let status = u16::try_from(replay.http_status)
                .ok()
                .and_then(|status| StatusCode::from_u16(status).ok())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            return raw_json_response(status, replay.response_json, None);
        }
        ControllerActionStart::InProgress => {
            return protocol_error_response(
                StatusCode::CONFLICT,
                Some(action_id),
                ErrorCode::Conflict,
                "controller action is already in progress",
                true,
                None,
            )
        }
        ControllerActionStart::OutcomeUnknown => return protocol_error_response(
            StatusCode::CONFLICT,
            Some(action_id),
            ErrorCode::Conflict,
            "previous action execution outcome is unknown; reconcile before using a new action_id",
            false,
            None,
        ),
        ControllerActionStart::RequestMismatch => {
            return protocol_error_response(
                StatusCode::CONFLICT,
                Some(action_id),
                ErrorCode::Conflict,
                "action_id was already used with a different request",
                false,
                None,
            )
        }
        ControllerActionStart::Denied(denial) => {
            return denial_response(Some(action_id), denial, now_ms())
        }
        ControllerActionStart::Started { open_decision } => open_decision,
    };

    let (status, body, binding) = if let Some(ControllerOpenDecision::Deduplicate(existing)) =
        open_decision.as_ref()
    {
        let result = ActionResultEnvelope {
            version: CURRENT_VERSION,
            action_id: action_id.clone(),
            result: ControllerActionResult::SessionOpened {
                session_id: existing.session_id.clone(),
                deduped: true,
            },
        };
        (
            StatusCode::OK,
            serde_json::to_string(&result).expect("protocol result serializes"),
            None,
        )
    } else {
        let mut action = envelope.action;
        let binding_input = if let (ControllerAction::OpenSession(open), Some(intent)) =
            (&mut action, open_intent)
        {
            // ADR 035 P2: inject the chair's waiver block HERE, while the raw
            // `github:pr/…` trigger_ref still exists — the hashed rewrite one
            // line down is opaque to the repo parser (live-verification miss,
            // 2026-08-02: the waiver never reached the chair).
            if let Some(chair) = open.chair_bot.clone() {
                if !open.recipient_inputs.is_empty() {
                    if let Some(block) = crate::controller::waiver_block_for_trigger(
                        state,
                        Some(intent.trigger_ref.as_str()),
                    ) {
                        if let Some(input) = open.recipient_inputs.get_mut(&chair) {
                            input.push_str(&block);
                        }
                    }
                }
            }
            open.trigger_ref = Some(controller_trigger_ref(&controller_id, &intent.trigger_ref));
            Some((intent.trigger_ref, intent.trigger_fingerprint))
        } else {
            None
        };
        match execute_interpreted_action(state, action) {
            Ok(result) => {
                let binding = binding_input.and_then(|(trigger_ref, fingerprint)| {
                    result_session_id(&result).map(|session_id| ControllerSessionBinding {
                        controller_id: controller_id.clone(),
                        scope: scope.clone(),
                        trigger_ref,
                        trigger_fingerprint: fingerprint,
                        session_id: session_id.to_string(),
                    })
                });
                let result = ActionResultEnvelope {
                    version: CURRENT_VERSION,
                    action_id: action_id.clone(),
                    result,
                };
                (
                    StatusCode::OK,
                    serde_json::to_string(&result).expect("protocol result serializes"),
                    binding,
                )
            }
            Err(error) => {
                let (status, code, message, retryable) = map_controller_error(error);
                let error = ErrorEnvelope {
                    version: CURRENT_VERSION,
                    action_id: Some(action_id.clone()),
                    error: ProtocolError {
                        code,
                        message,
                        retryable,
                    },
                };
                (
                    status,
                    serde_json::to_string(&error).expect("protocol error serializes"),
                    None,
                )
            }
        }
    };

    if let Err(error) = state.store.finish_controller_action(
        &controller_id,
        &action_id,
        i64::from(status.as_u16()),
        &body,
        binding.as_ref(),
        now_ms(),
    ) {
        tracing::error!(%error, controller_id, action_id, "persist controller action result failed");
        return protocol_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some(action_id),
            ErrorCode::Internal,
            "controller action result persistence failed",
            true,
            None,
        );
    }
    raw_json_response(status, body, None)
}

fn authenticate_controller(
    state: &AppState,
    auth: &ControllerAuthConfig,
    token: &str,
) -> Result<Option<String>> {
    let valid_shape = URL_SAFE_NO_PAD
        .decode(token.as_bytes())
        .is_ok_and(|bytes| bytes.len() == 32);
    let candidates = state.store.active_controller_action_tokens(now_ms())?;
    let mut matched: Option<String> = None;
    let mut matches = 0usize;
    for candidate in &candidates {
        let pepper = auth
            .pepper(candidate.pepper_version)
            .unwrap_or_else(|| auth.first_pepper());
        let mut mac = HmacSha256::new_from_slice(pepper).expect("HMAC accepts arbitrary key size");
        mac.update(token.as_bytes());
        if mac.verify_slice(&candidate.token_hash).is_ok()
            && auth.pepper(candidate.pepper_version).is_some()
            && valid_shape
        {
            matches += 1;
            matched = Some(candidate.controller_id.clone());
        }
    }
    if candidates.is_empty() {
        // Keep the empty-installation path from becoming a trivial fast oracle.
        let mut mac = HmacSha256::new_from_slice(auth.first_pepper())
            .expect("HMAC accepts arbitrary key size");
        mac.update(token.as_bytes());
        let _ = mac.verify_slice(&[0; 32]);
    }
    Ok((matches == 1).then_some(matched).flatten())
}

fn execute_interpreted_action(
    state: &Arc<AppState>,
    action: ControllerAction,
) -> Result<ControllerActionResult, ControllerError> {
    if matches!(action, ControllerAction::CloseSession(_)) {
        controller::execute_with_close_policy(state, action, ControlledClosePolicy::Allow)
    } else {
        controller::execute(state, action)
    }
}

fn action_kind(action: &ControllerAction) -> &'static str {
    match action {
        ControllerAction::OpenSession(_) => "open_session",
        ControllerAction::PostMessage(_) => "post_message",
        ControllerAction::AddRoster(_) => "add_roster",
        ControllerAction::CloseSession(_) => "close_session",
        ControllerAction::EmitStatus(_) => "emit_status",
    }
}

fn action_session_id(action: &ControllerAction) -> Option<&str> {
    match action {
        ControllerAction::OpenSession(_) => None,
        ControllerAction::PostMessage(action) => Some(&action.session_id),
        ControllerAction::AddRoster(action) => Some(&action.session_id),
        ControllerAction::CloseSession(action) => Some(&action.session_id),
        ControllerAction::EmitStatus(action) => Some(&action.session_id),
    }
}

fn result_session_id(result: &ControllerActionResult) -> Option<&str> {
    match result {
        ControllerActionResult::SessionOpened { session_id, .. }
        | ControllerActionResult::Superseded { session_id, .. } => Some(session_id),
        _ => None,
    }
}

fn controller_trigger_ref(controller_id: &str, trigger_ref: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(controller_id.as_bytes());
    digest.update([0]);
    digest.update(trigger_ref.as_bytes());
    format!(
        "controller:{controller_id}:{}",
        hex::encode(digest.finalize())
    )
}

fn denial_response(
    action_id: Option<String>,
    denial: ControllerActionDenial,
    now: i64,
) -> Response {
    match denial {
        ControllerActionDenial::Credential => unauthorized_response(),
        ControllerActionDenial::Grant => protocol_error_response(
            StatusCode::FORBIDDEN,
            action_id,
            ErrorCode::Forbidden,
            "controller action is not granted",
            false,
            None,
        ),
        ControllerActionDenial::Scope => protocol_error_response(
            StatusCode::FORBIDDEN,
            action_id,
            ErrorCode::Forbidden,
            "controller scope is not granted",
            false,
            None,
        ),
        ControllerActionDenial::Disabled => protocol_error_response(
            StatusCode::FORBIDDEN,
            action_id,
            ErrorCode::Forbidden,
            "controller installation is disabled; only actions on its in-flight sessions are allowed",
            false,
            None,
        ),
        ControllerActionDenial::SessionOwnership => protocol_error_response(
            StatusCode::FORBIDDEN,
            action_id,
            ErrorCode::Forbidden,
            "session is not owned by this controller scope",
            false,
            None,
        ),
        ControllerActionDenial::TriggerScope => protocol_error_response(
            StatusCode::FORBIDDEN,
            action_id,
            ErrorCode::Forbidden,
            "controller trigger is bound to another scope",
            false,
            None,
        ),
        ControllerActionDenial::RateQuota { limit, reset_at } => {
            let remaining_ms = reset_at.saturating_sub(now);
            let retry_after = remaining_ms.saturating_add(999) / 1000;
            let retry_after = retry_after.max(1);
            protocol_error_response(
                StatusCode::TOO_MANY_REQUESTS,
                action_id,
                ErrorCode::RateLimited,
                &format!("accepted action rate quota exceeded (limit {limit} per minute)"),
                true,
                Some(retry_after),
            )
        }
        ControllerActionDenial::ConcurrentSessionQuota { limit, current } => {
            protocol_error_response(
                StatusCode::CONFLICT,
                action_id,
                ErrorCode::Conflict,
                &format!("concurrent session quota exceeded (limit {limit}, current {current})"),
                false,
                None,
            )
        }
    }
}

fn map_controller_error(error: ControllerError) -> (StatusCode, ErrorCode, String, bool) {
    match error {
        ControllerError::Invalid(message) => (
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            message,
            false,
        ),
        ControllerError::Forbidden(message) => {
            (StatusCode::FORBIDDEN, ErrorCode::Forbidden, message, false)
        }
        ControllerError::NotFound(message) => {
            (StatusCode::NOT_FOUND, ErrorCode::NotFound, message, false)
        }
        ControllerError::Gone(message) => (StatusCode::GONE, ErrorCode::Gone, message, false),
        ControllerError::Internal(error) => {
            tracing::error!(%error, "controller interpreter failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "controller action execution failed".into(),
                true,
            )
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn unauthorized_response() -> Response {
    protocol_error_response(
        StatusCode::UNAUTHORIZED,
        None,
        ErrorCode::Unauthorized,
        "invalid controller action credentials",
        false,
        None,
    )
}

#[derive(Debug, Clone, Copy)]
enum OperatorAuthError {
    Unavailable,
    Unauthorized,
}

impl OperatorAuthError {
    fn response(self) -> Response {
        match self {
            OperatorAuthError::Unavailable => admin_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "operator API key is not configured",
            ),
            OperatorAuthError::Unauthorized => {
                admin_error(StatusCode::UNAUTHORIZED, "unauthorized")
            }
        }
    }
}

fn check_operator_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> std::result::Result<(), OperatorAuthError> {
    let Some(expected) = state.api_key.as_deref() else {
        return Err(OperatorAuthError::Unavailable);
    };
    let Some(provided) = bearer(headers) else {
        return Err(OperatorAuthError::Unauthorized);
    };
    let key = b"openab-control-plane/operator-auth/v1";
    let mut expected_mac = HmacSha256::new_from_slice(key).expect("fixed HMAC key is valid");
    expected_mac.update(expected.as_bytes());
    let expected_tag = expected_mac.finalize().into_bytes();
    let mut provided_mac = HmacSha256::new_from_slice(key).expect("fixed HMAC key is valid");
    provided_mac.update(provided.as_bytes());
    if provided_mac.verify_slice(&expected_tag).is_ok() {
        Ok(())
    } else {
        Err(OperatorAuthError::Unauthorized)
    }
}

fn admin_error(status: StatusCode, message: &str) -> Response {
    json_response(status, &serde_json::json!({ "error": message }), None)
}

fn admin_store_error(error: anyhow::Error) -> Response {
    tracing::error!(%error, "controller installation store operation failed");
    admin_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "controller installation store unavailable",
    )
}

fn internal_store_error(action_id: Option<String>, error: anyhow::Error) -> Response {
    tracing::error!(%error, "controller action store operation failed");
    protocol_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        action_id,
        ErrorCode::Internal,
        "controller action store unavailable",
        true,
        None,
    )
}

fn protocol_error_response(
    status: StatusCode,
    action_id: Option<String>,
    code: ErrorCode,
    message: &str,
    retryable: bool,
    retry_after: Option<i64>,
) -> Response {
    json_response(
        status,
        &ErrorEnvelope {
            version: CURRENT_VERSION,
            action_id,
            error: ProtocolError {
                code,
                message: message.to_string(),
                retryable,
            },
        },
        retry_after,
    )
}

fn json_response<T: serde::Serialize>(
    status: StatusCode,
    value: &T,
    retry_after: Option<i64>,
) -> Response {
    raw_json_response(
        status,
        serde_json::to_string(value).expect("protocol response serializes"),
        retry_after,
    )
}

fn raw_json_response(status: StatusCode, body: String, retry_after: Option<i64>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(seconds) = retry_after {
        if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{
        AddRosterAction, CloseSessionAction, OpenSessionAction, PostMessageAction,
    };
    use crate::controller_events::{
        dispatch_once, ControllerEventKeys, ControllerEventRequest, ControllerEventRuntime,
        ControllerEventTransport,
    };
    use crate::store::{SessionState, SqliteStore, Store};
    use axum::body::to_bytes;
    use futures::future::BoxFuture;
    use github_pr_controller::ocp::{ActionFailure, ActionFuture, OcpActionClient};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use std::sync::Mutex;
    use tower::ServiceExt;

    const SCOPE: &str = "tenant:alpha/resource:one";

    #[derive(Default)]
    struct RecordingEventTransport {
        requests: Mutex<Vec<ControllerEventRequest>>,
    }

    impl ControllerEventTransport for RecordingEventTransport {
        fn post(&self, request: ControllerEventRequest) -> BoxFuture<'static, Result<u16>> {
            self.requests.lock().unwrap().push(request);
            Box::pin(async { Ok(204) })
        }
    }

    struct InProcessOcpActionClient {
        state: Arc<AppState>,
        token: String,
        scope: String,
    }

    impl OcpActionClient for InProcessOcpActionClient {
        fn open_session(&self, action_id: String, action: OpenSessionAction) -> ActionFuture {
            let state = self.state.clone();
            let token = self.token.clone();
            let scope = self.scope.clone();
            Box::pin(async move {
                let envelope = ActionEnvelope {
                    version: CURRENT_VERSION,
                    action_id,
                    action: ControllerAction::OpenSession(action),
                };
                let response = request(&state, &token, &scope, &envelope);
                let status = response.status();
                let body = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|_| ActionFailure::Unavailable)?
                    .to_bytes();
                if status.is_success() {
                    serde_json::from_slice(&body).map_err(|_| ActionFailure::InvalidResponse)
                } else {
                    let error: ErrorEnvelope = serde_json::from_slice(&body)
                        .map_err(|_| ActionFailure::InvalidResponse)?;
                    Err(ActionFailure::Protocol {
                        status: status.as_u16(),
                        code: error.error.code,
                        retryable: error.error.retryable,
                    })
                }
            })
        }
    }

    #[derive(Default)]
    struct InProcessControllerEventTransport {
        router: Mutex<Option<axum::Router>>,
    }

    impl ControllerEventTransport for InProcessControllerEventTransport {
        fn post(&self, request: ControllerEventRequest) -> BoxFuture<'static, Result<u16>> {
            let router = self.router.lock().unwrap().clone();
            Box::pin(async move {
                let app = router.context("external controller router not attached")?;
                let target = crate::controller_events::request_target(&request.endpoint)?;
                let mut builder = Request::post(target);
                for (name, value) in request.headers {
                    builder = builder.header(name, value);
                }
                let response = app.oneshot(builder.body(Body::from(request.body))?).await?;
                Ok(response.status().as_u16())
            })
        }
    }

    fn auth_config() -> ControllerAuthConfig {
        ControllerAuthConfig::new(BTreeMap::from([(1, vec![7; 32]), (2, vec![9; 32])])).unwrap()
    }

    fn token(byte: u8) -> String {
        URL_SAFE_NO_PAD.encode([byte; 32])
    }

    fn seed_bots(store: &SqliteStore) {
        for (id, role) in [
            ("chair", "chair"),
            ("rev1", "reviewer"),
            ("rev2", "reviewer"),
        ] {
            store.seed_bot(id, id, role, "hash", "token").unwrap();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn install(
        store: &SqliteStore,
        auth: &ControllerAuthConfig,
        controller_id: &str,
        token_id: &str,
        token: &str,
        pepper_version: i64,
        scope: &str,
        max_concurrent: i64,
        rate: i64,
    ) {
        store
            .upsert_controller_installation(controller_id, max_concurrent, rate)
            .unwrap();
        store
            .put_controller_action_token(
                token_id,
                controller_id,
                &auth.hash_token(pepper_version, token).unwrap(),
                pepper_version,
                now_ms() - 1,
                Some(now_ms() + 900_000),
            )
            .unwrap();
        for action in [
            "open_session",
            "post_message",
            "add_roster",
            "close_session",
            "emit_status",
        ] {
            store
                .set_controller_action_grant(controller_id, action, true)
                .unwrap();
        }
        store
            .set_controller_scope_binding(controller_id, scope, true)
            .unwrap();
    }

    fn setup(
        max_concurrent: i64,
        rate: i64,
    ) -> (
        Arc<AppState>,
        Arc<SqliteStore>,
        ControllerAuthConfig,
        String,
    ) {
        let store = Arc::new(SqliteStore::memory().unwrap());
        seed_bots(&store);
        let auth = auth_config();
        let token = token(1);
        install(
            &store,
            &auth,
            "ctrl-a",
            "tok-a-1",
            &token,
            1,
            SCOPE,
            max_concurrent,
            rate,
        );
        let state = AppState::new_with_controller_auth(store.clone(), auth.clone());
        (state, store, auth, token)
    }

    fn open_action(action_id: &str, trigger_ref: &str, fingerprint: &str) -> ActionEnvelope {
        ActionEnvelope {
            version: CURRENT_VERSION,
            action_id: action_id.into(),
            action: ControllerAction::OpenSession(OpenSessionAction {
                title: "external council".into(),
                trigger_ref: Some(trigger_ref.into()),
                trigger_fingerprint: Some(fingerprint.into()),
                roster: vec!["chair".into(), "rev1".into()],
                quorum_n: 1,
                chair_bot: Some("chair".into()),
                mode: "council".into(),
                prompt: "Inspect the external request.".into(),
                recipient_inputs: Default::default(),
            }),
        }
    }

    fn post_action(action_id: &str, session_id: &str, content: &str) -> ActionEnvelope {
        ActionEnvelope {
            version: CURRENT_VERSION,
            action_id: action_id.into(),
            action: ControllerAction::PostMessage(PostMessageAction {
                session_id: session_id.into(),
                content: content.into(),
            }),
        }
    }

    fn close_action(action_id: &str, session_id: &str) -> ActionEnvelope {
        ActionEnvelope {
            version: CURRENT_VERSION,
            action_id: action_id.into(),
            action: ControllerAction::CloseSession(CloseSessionAction {
                session_id: session_id.into(),
                reason: "controller test close".into(),
            }),
        }
    }

    fn headers(token: &str, action_id: &str, scope: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers.insert(ACTION_ID_HEADER, HeaderValue::from_str(action_id).unwrap());
        headers.insert(SCOPE_HEADER, HeaderValue::from_str(scope).unwrap());
        headers
    }

    fn github_controller_webhook(
        delivery_id: &str,
        event_type: &str,
        body: String,
    ) -> Request<Body> {
        let mut mac = HmacSha256::new_from_slice(b"fixture-secret").unwrap();
        mac.update(body.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::post("/api/v1/github/webhooks")
            .header("x-github-delivery", delivery_id)
            .header("x-github-event", event_type)
            .header("x-hub-signature-256", signature)
            .body(Body::from(body))
            .unwrap()
    }

    fn bot_reply(session_id: &str, content: &str) -> crate::protocol::GatewayReply {
        crate::protocol::GatewayReply {
            schema: String::new(),
            reply_to: String::new(),
            platform: String::new(),
            channel: crate::protocol::ReplyChannel {
                id: session_id.into(),
                thread_id: None,
            },
            content: crate::protocol::Content::text(content),
            command: None,
            request_id: None,
            quote_message_id: None,
        }
    }

    fn request(
        state: &Arc<AppState>,
        token: &str,
        scope: &str,
        envelope: &ActionEnvelope,
    ) -> Response {
        let body = serde_json::to_vec(envelope).unwrap();
        execute_action_request(state, &headers(token, &envelope.action_id, scope), &body)
    }

    async fn response(response: Response) -> (StatusCode, Value, HeaderMap) {
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap(), headers)
    }

    async fn opened_session_id(result: Response) -> String {
        let (status, body, _) = response(result).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["result"]["data"]["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn token_rotation_overlap_expiry_and_revocation_are_enforced() {
        let (state, store, auth, old_token) = setup(5, 60);
        let unknown_token = token(99);
        for invalid_token in ["not-base64", unknown_token.as_str()] {
            let invalid = request(
                &state,
                invalid_token,
                SCOPE,
                &open_action("act-invalid", "object:invalid", "v1"),
            );
            let (status, body, _) = response(invalid).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"]["code"], "unauthorized");
        }
        let new_token = token(2);
        install(
            &store, &auth, "ctrl-a", "tok-a-2", &new_token, 2, SCOPE, 5, 60,
        );

        let old = request(
            &state,
            &old_token,
            SCOPE,
            &open_action("act-old", "object:old", "v1"),
        );
        assert_eq!(old.status(), StatusCode::OK);
        let new = request(
            &state,
            &new_token,
            SCOPE,
            &open_action("act-new", "object:new", "v1"),
        );
        assert_eq!(new.status(), StatusCode::OK);

        store
            .revoke_controller_action_token("ctrl-a", "tok-a-1", now_ms())
            .unwrap();
        let revoked = request(
            &state,
            &old_token,
            SCOPE,
            &open_action("act-revoked", "object:revoked", "v1"),
        );
        let (status, body, _) = response(revoked).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let expired_token = token(3);
        store
            .put_controller_action_token(
                "tok-a-expired",
                "ctrl-a",
                &auth.hash_token(1, &expired_token).unwrap(),
                1,
                now_ms() - 10_000,
                Some(now_ms() - 1),
            )
            .unwrap();
        assert_eq!(
            request(
                &state,
                &expired_token,
                SCOPE,
                &open_action("act-expired", "object:expired", "v1"),
            )
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[test]
    fn admission_revalidates_a_token_revoked_after_initial_authentication() {
        let (state, store, auth, token) = setup(5, 60);
        assert_eq!(
            authenticate_controller(&state, &auth, &token).unwrap(),
            Some("ctrl-a".into())
        );
        let credential_hashes = auth.credential_hashes(&token);
        store
            .revoke_controller_action_token("ctrl-a", "tok-a-1", now_ms())
            .unwrap();
        let admitted = store
            .begin_controller_action(
                "ctrl-a",
                &credential_hashes,
                "act-revocation-race",
                &[7; 32],
                "open_session",
                SCOPE,
                None,
                None,
                now_ms(),
            )
            .unwrap();
        assert_eq!(
            admitted,
            ControllerActionStart::Denied(ControllerActionDenial::Credential)
        );
    }

    #[tokio::test]
    async fn grants_scopes_and_session_ownership_are_fail_closed() {
        let (state, store, auth, token_a) = setup(5, 60);
        store
            .set_controller_action_grant("ctrl-a", "open_session", false)
            .unwrap();
        let denied = request(
            &state,
            &token_a,
            SCOPE,
            &open_action("act-grant", "object:grant", "v1"),
        );
        let (status, body, _) = response(denied).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["message"], "controller action is not granted");
        store
            .set_controller_action_grant("ctrl-a", "open_session", true)
            .unwrap();

        let bad_scope = request(
            &state,
            &token_a,
            "tenant:other/resource:one",
            &open_action("act-scope", "object:scope", "v1"),
        );
        assert_eq!(bad_scope.status(), StatusCode::FORBIDDEN);

        let session_id = opened_session_id(request(
            &state,
            &token_a,
            SCOPE,
            &open_action("act-owned", "object:owned", "v1"),
        ))
        .await;
        let token_b = token(4);
        install(
            &store, &auth, "ctrl-b", "tok-b-1", &token_b, 1, SCOPE, 5, 60,
        );
        let foreign = request(
            &state,
            &token_b,
            SCOPE,
            &post_action("act-foreign", &session_id, "Do not accept."),
        );
        let (status, body, _) = response(foreign).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body["error"]["message"],
            "session is not owned by this controller scope"
        );
    }

    #[tokio::test]
    async fn rate_and_concurrent_session_quotas_return_stable_errors() {
        let (state, _store, _auth, token) = setup(5, 1);
        assert_eq!(
            request(
                &state,
                &token,
                SCOPE,
                &open_action("act-rate-1", "object:rate-1", "v1"),
            )
            .status(),
            StatusCode::OK
        );
        let limited = request(
            &state,
            &token,
            SCOPE,
            &open_action("act-rate-2", "object:rate-2", "v1"),
        );
        let (status, body, headers) = response(limited).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["code"], "rate_limited");
        assert!(headers.get(header::RETRY_AFTER).is_some());

        let (state, _store, _auth, token) = setup(1, 60);
        assert_eq!(
            request(
                &state,
                &token,
                SCOPE,
                &open_action("act-cap-1", "object:cap-1", "v1"),
            )
            .status(),
            StatusCode::OK
        );
        let limited = request(
            &state,
            &token,
            SCOPE,
            &open_action("act-cap-2", "object:cap-2", "v1"),
        );
        let (status, body, _) = response(limited).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"]["message"],
            "concurrent session quota exceeded (limit 1, current 1)"
        );
    }

    #[tokio::test]
    async fn action_replay_is_idempotent_and_request_mismatch_conflicts() {
        let (state, store, _auth, token) = setup(5, 60);
        let session_id = opened_session_id(request(
            &state,
            &token,
            SCOPE,
            &open_action("act-open", "object:replay", "v1"),
        ))
        .await;
        let post = post_action("act-post", &session_id, "One durable follow-up.");
        let first = response(request(&state, &token, SCOPE, &post)).await;
        let replay = response(request(&state, &token, SCOPE, &post)).await;
        assert_eq!(first.0, StatusCode::OK);
        assert_eq!(first.1, replay.1);
        assert_eq!(store.messages(&session_id).unwrap().len(), 2);

        let mismatch = request(
            &state,
            &token,
            SCOPE,
            &post_action("act-post", &session_id, "Different body."),
        );
        let (status, body, _) = response(mismatch).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"]["message"],
            "action_id was already used with a different request"
        );
        assert_eq!(store.messages(&session_id).unwrap().len(), 2);

        store
            .set_controller_action_grant("ctrl-a", "post_message", false)
            .unwrap();
        let denied_replay = response(request(&state, &token, SCOPE, &post)).await;
        assert_eq!(denied_replay.0, StatusCode::FORBIDDEN);
        assert_eq!(
            denied_replay.1["error"]["message"],
            "controller action is not granted"
        );
        store
            .set_controller_action_grant("ctrl-a", "post_message", true)
            .unwrap();
        store
            .set_controller_scope_binding("ctrl-a", SCOPE, false)
            .unwrap();
        let denied_scope_replay = response(request(&state, &token, SCOPE, &post)).await;
        assert_eq!(denied_scope_replay.0, StatusCode::FORBIDDEN);
        assert_eq!(
            denied_scope_replay.1["error"]["message"],
            "controller scope is not granted"
        );
    }

    #[tokio::test]
    async fn action_route_rejects_oversized_body_before_full_buffering() {
        let (state, _store, _auth, token) = setup(5, 60);
        let request = Request::builder()
            .method("POST")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(ACTION_ID_HEADER, "act-oversized")
            .header(SCOPE_HEADER, SCOPE)
            .body(Body::from(vec![b'x'; MAX_ACTION_BODY_BYTES + 1]))
            .unwrap();
        let (status, body, _) = response(execute_action(State(state), request).await).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["action_id"], "act-oversized");
    }

    #[tokio::test]
    async fn concurrent_replay_executes_the_interpreter_once() {
        let (state, store, _auth, token) = setup(5, 60);
        let action = open_action("act-race", "object:race", "v1");
        let body = serde_json::to_vec(&action).unwrap();
        let headers = headers(&token, &action.action_id, SCOPE);
        let (left, right) = std::thread::scope(|scope| {
            let left_state = state.clone();
            let left_headers = headers.clone();
            let left_body = body.clone();
            let left =
                scope.spawn(move || execute_action_request(&left_state, &left_headers, &left_body));
            let right_state = state.clone();
            let right = scope.spawn(move || execute_action_request(&right_state, &headers, &body));
            (left.join().unwrap(), right.join().unwrap())
        });
        let left = response(left).await;
        let right = response(right).await;
        assert_eq!(left.0, StatusCode::OK);
        assert_eq!(left.1, right.1);
        assert_eq!(store.list_sessions(None, None, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn trigger_dedupe_is_controller_scoped_and_fingerprint_aware() {
        let (state, store, auth, token_a) = setup(5, 60);
        let token_b = token(5);
        install(
            &store, &auth, "ctrl-b", "tok-b-1", &token_b, 1, SCOPE, 5, 60,
        );
        let first_a = opened_session_id(request(
            &state,
            &token_a,
            SCOPE,
            &open_action("act-a-1", "object:shared", "sha:1"),
        ))
        .await;
        let first_b = opened_session_id(request(
            &state,
            &token_b,
            SCOPE,
            &open_action("act-b-1", "object:shared", "sha:1"),
        ))
        .await;
        assert_ne!(first_a, first_b, "controller namespace prevents collision");

        let dedupe = response(request(
            &state,
            &token_a,
            SCOPE,
            &open_action("act-a-2", "object:shared", "sha:1"),
        ))
        .await;
        assert_eq!(dedupe.1["result"]["data"]["session_id"], first_a);
        assert_eq!(dedupe.1["result"]["data"]["deduped"], true);

        let supersede = response(request(
            &state,
            &token_a,
            SCOPE,
            &open_action("act-a-3", "object:shared", "sha:2"),
        ))
        .await;
        assert_eq!(supersede.0, StatusCode::OK);
        assert_eq!(supersede.1["result"]["type"], "superseded");
        let second_a = supersede.1["result"]["data"]["session_id"]
            .as_str()
            .unwrap();
        assert_ne!(second_a, first_a);
        assert_eq!(
            SessionState::from_db_str(&store.session(&first_a).unwrap().unwrap().state),
            SessionState::Closed
        );
        assert_eq!(
            store
                .controller_session_for_trigger("ctrl-a", "object:shared")
                .unwrap()
                .unwrap()
                .session_id,
            second_a
        );

        assert_eq!(
            request(&state, &token_a, SCOPE, &close_action("act-a-4", second_a),).status(),
            StatusCode::OK
        );
        let reopened = response(request(
            &state,
            &token_a,
            SCOPE,
            &open_action("act-a-5", "object:shared", "sha:2"),
        ))
        .await;
        assert_eq!(reopened.0, StatusCode::OK);
        assert_eq!(reopened.1["result"]["data"]["deduped"], false);
        assert_ne!(reopened.1["result"]["data"]["session_id"], second_a);
    }

    #[tokio::test]
    async fn operator_install_rotate_revoke_and_disable_lifecycle_is_usable() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        seed_bots(&store);
        let auth = auth_config();
        let state = AppState::new_with_options_and_runtime_config(
            store.clone(),
            Some("root-operator-key".into()),
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::pr_review::PrReviewConfig::default(),
            Some(auth),
            None,
        );
        let mut operator_headers = HeaderMap::new();
        operator_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer root-operator-key"),
        );
        let created = create_installation(
            State(state.clone()),
            operator_headers.clone(),
            Json(CreateControllerInstallation {
                id: "ctrl-managed".into(),
                actions: vec!["open_session".into(), "post_message".into()],
                scopes: vec![SCOPE.into()],
                max_concurrent_sessions: 2,
                max_actions_per_minute: 10,
            }),
        )
        .await;
        let (status, created, _) = response(created).await;
        assert_eq!(status, StatusCode::CREATED);
        let old_token = created["action_token"].as_str().unwrap().to_string();
        let old_token_id = created["token_id"].as_str().unwrap().to_string();
        assert_eq!(URL_SAFE_NO_PAD.decode(&old_token).unwrap().len(), 32);
        assert_eq!(
            request(
                &state,
                &old_token,
                SCOPE,
                &open_action("act-managed-1", "object:managed-1", "v1"),
            )
            .status(),
            StatusCode::OK
        );

        let rotated = rotate_installation_token(
            State(state.clone()),
            operator_headers.clone(),
            Path("ctrl-managed".into()),
        )
        .await;
        let (status, rotated, _) = response(rotated).await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(rotated["overlap_until"].as_i64().unwrap() > now_ms());
        let new_token = rotated["action_token"].as_str().unwrap().to_string();
        assert_eq!(
            request(
                &state,
                &old_token,
                SCOPE,
                &open_action("act-managed-old-overlap", "object:managed-2", "v1"),
            )
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            request(
                &state,
                &new_token,
                SCOPE,
                &open_action("act-managed-new", "object:managed-3", "v1"),
            )
            .status(),
            StatusCode::CONFLICT,
            "the installation quota is two active sessions, proving the new token authenticated"
        );

        let revoked = revoke_installation_token(
            State(state.clone()),
            operator_headers.clone(),
            Path(("ctrl-managed".into(), old_token_id)),
        )
        .await;
        assert_eq!(revoked.status(), StatusCode::OK);
        assert_eq!(
            request(
                &state,
                &old_token,
                SCOPE,
                &open_action("act-managed-revoked", "object:managed-4", "v1"),
            )
            .status(),
            StatusCode::UNAUTHORIZED
        );

        let disabled = set_installation_state(
            State(state.clone()),
            operator_headers,
            Path("ctrl-managed".into()),
            Json(SetControllerState {
                enabled: Some(false),
                max_concurrent_sessions: None,
                max_actions_per_minute: None,
                actions: None,
                scopes: None,
            }),
        )
        .await;
        assert_eq!(disabled.status(), StatusCode::OK);
        // ADR 034: a disabled registration still authenticates; NEW work is
        // refused with an explicit 403, not a credential 401.
        assert_eq!(
            request(
                &state,
                &new_token,
                SCOPE,
                &open_action("act-managed-disabled", "object:managed-5", "v1"),
            )
            .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn the_legacy_enabled_only_patch_body_still_deserializes() {
        let legacy: SetControllerState = serde_json::from_str(r#"{"enabled": false}"#).unwrap();
        assert_eq!(legacy.enabled, Some(false));
        assert!(legacy.max_concurrent_sessions.is_none());
        assert!(legacy.max_actions_per_minute.is_none());
        assert!(legacy.actions.is_none());
        assert!(legacy.scopes.is_none());
    }

    #[tokio::test]
    async fn waiver_crud_is_operator_gated_and_expiry_filtered() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new_with_options_and_runtime_config(
            store.clone(),
            Some("root-operator-key".into()),
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::pr_review::PrReviewConfig::default(),
            Some(auth_config()),
            None,
        );
        let mut operator = HeaderMap::new();
        operator.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer root-operator-key"),
        );
        let body = |expires: i64| CreateReviewWaiver {
            repo: "zeabur/nuphos".into(),
            path_class: None,
            text: "fail-open on the login redirect is an accepted trade-off".into(),
            origin_pr: Some("zeabur/nuphos#652".into()),
            created_by: "canyu".into(),
            expires_at: expires,
        };

        // No operator key, no write.
        let unauthorized = create_review_waiver(
            State(state.clone()),
            HeaderMap::new(),
            Json(body(now_ms() + 3_600_000)),
        )
        .await;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        // Expiry is mandatory and must be in the future.
        let fossil = create_review_waiver(
            State(state.clone()),
            operator.clone(),
            Json(body(now_ms() - 1)),
        )
        .await;
        assert_eq!(fossil.status(), StatusCode::BAD_REQUEST);

        let created = create_review_waiver(
            State(state.clone()),
            operator.clone(),
            Json(body(now_ms() + 3_600_000)),
        )
        .await;
        let (status, created, _) = response(created).await;
        assert_eq!(status, StatusCode::CREATED);
        let waiver_id = created["id"].as_str().unwrap().to_string();

        let active = store
            .list_review_waivers(Some("zeabur/nuphos"), false, now_ms())
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].fired_count, 0);

        // Revoke removes it from the active view but not from history.
        let revoked = update_review_waiver(
            State(state.clone()),
            operator.clone(),
            Path(waiver_id.clone()),
            Json(UpdateReviewWaiver {
                expires_at: None,
                revoke: true,
            }),
        )
        .await;
        assert_eq!(revoked.status(), StatusCode::OK);
        assert!(store
            .list_review_waivers(Some("zeabur/nuphos"), false, now_ms())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .list_review_waivers(None, true, now_ms())
                .unwrap()
                .len(),
            1
        );

        // Guard rails.
        let unknown = update_review_waiver(
            State(state.clone()),
            operator.clone(),
            Path("wvr_nope".into()),
            Json(UpdateReviewWaiver {
                expires_at: None,
                revoke: true,
            }),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        let empty = update_review_waiver(
            State(state.clone()),
            operator.clone(),
            Path(waiver_id.clone()),
            Json(UpdateReviewWaiver {
                expires_at: None,
                revoke: false,
            }),
        )
        .await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        // Re-revoking never rewrites history: the first revocation time wins.
        let first_revoked_at = store.list_review_waivers(None, true, now_ms()).unwrap()[0]
            .revoked_at
            .unwrap();
        let again = update_review_waiver(
            State(state),
            operator,
            Path(waiver_id),
            Json(UpdateReviewWaiver {
                expires_at: None,
                revoke: true,
            }),
        )
        .await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(
            store.list_review_waivers(None, true, now_ms()).unwrap()[0].revoked_at,
            Some(first_revoked_at)
        );

        // Query strings are not JSON: ?all=1 must parse as true.
        let query: ListReviewWaivers =
            serde_urlencoded::from_str("all=1").expect("all=1 parses");
        assert!(query.all);
        let query: ListReviewWaivers = serde_urlencoded::from_str("").unwrap();
        assert!(!query.all);
    }

    #[tokio::test]
    async fn a_disabled_registration_drains_its_own_sessions_and_admits_nothing_new() {
        let (state, store, _auth, token) = setup(5, 60);
        let session_id = opened_session_id(request(
            &state,
            &token,
            SCOPE,
            &open_action("act-open-drain", "object:drain", "v1"),
        ))
        .await;

        store
            .set_controller_installation_enabled("ctrl-a", false)
            .unwrap();

        // ADR 034: in-flight work still finishes — actions on the session
        // this registration opened pass while disabled...
        let post = post_action("act-drain-post", &session_id, "Still finishing.");
        let (status, _, _) = response(request(&state, &token, SCOPE, &post)).await;
        assert_eq!(status, StatusCode::OK);

        // ...but new admission is refused, explicitly, not as a 401.
        let denied = response(request(
            &state,
            &token,
            SCOPE,
            &open_action("act-open-denied", "object:drain-2", "v1"),
        ))
        .await;
        assert_eq!(denied.0, StatusCode::FORBIDDEN);
        assert_eq!(
            denied.1["error"]["message"],
            "controller installation is disabled; only actions on its in-flight sessions are allowed"
        );
    }

    #[tokio::test]
    async fn a_patch_changes_limits_and_grants_in_place_without_replacement() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        seed_bots(&store);
        let auth = auth_config();
        let action_token = token(7);
        install(
            &store, &auth, "ctrl-a", "tok-a-1", &action_token, 1, SCOPE, 1, 60,
        );
        let state = AppState::new_with_options_and_runtime_config(
            store.clone(),
            Some("root-operator-key".into()),
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::pr_review::PrReviewConfig::default(),
            Some(auth),
            None,
        );
        let mut operator_headers = HeaderMap::new();
        operator_headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer root-operator-key"),
        );

        // Concurrency 1: the first session fills the quota.
        let first = request(
            &state,
            &action_token,
            SCOPE,
            &open_action("act-p-1", "object:p1", "v1"),
        );
        assert_eq!(first.status(), StatusCode::OK);
        let quota_hit = response(request(
            &state,
            &action_token,
            SCOPE,
            &open_action("act-p-2", "object:p2", "v1"),
        ))
        .await;
        assert_eq!(quota_hit.0, StatusCode::CONFLICT);

        // One PATCH raises the limit — no restart, no replacement
        // registration; the very next admission check sees it.
        let patched = set_installation_state(
            State(state.clone()),
            operator_headers.clone(),
            Path("ctrl-a".into()),
            Json(SetControllerState {
                enabled: None,
                max_concurrent_sessions: Some(5),
                max_actions_per_minute: Some(120),
                actions: None,
                scopes: None,
            }),
        )
        .await;
        let (patched_status, patched_body, _) = response(patched).await;
        assert_eq!(patched_status, StatusCode::OK);
        // The response is the full post-state — untouched fields carry their
        // real values, not nulls.
        assert_eq!(patched_body["enabled"], true);
        assert_eq!(patched_body["max_concurrent_sessions"], 5);
        assert_eq!(patched_body["max_actions_per_minute"], 120);
        assert!(patched_body["actions"].as_array().is_some_and(|a| !a.is_empty()));
        assert_eq!(
            request(
                &state,
                &action_token,
                SCOPE,
                &open_action("act-p-3", "object:p3", "v1"),
            )
            .status(),
            StatusCode::OK
        );

        // Replace-set the grants down to open_session only; post_message
        // dies with a grant denial, not a credential one.
        let session_id = opened_session_id(request(
            &state,
            &action_token,
            SCOPE,
            &open_action("act-p-4", "object:p4", "v1"),
        ))
        .await;
        let narrowed = set_installation_state(
            State(state.clone()),
            operator_headers.clone(),
            Path("ctrl-a".into()),
            Json(SetControllerState {
                enabled: None,
                max_concurrent_sessions: None,
                max_actions_per_minute: None,
                actions: Some(vec!["open_session".into()]),
                scopes: None,
            }),
        )
        .await;
        assert_eq!(narrowed.status(), StatusCode::OK);
        let denied = response(request(
            &state,
            &action_token,
            SCOPE,
            &post_action("act-p-5", &session_id, "should be refused"),
        ))
        .await;
        assert_eq!(denied.0, StatusCode::FORBIDDEN);
        assert_eq!(denied.1["error"]["message"], "controller action is not granted");

        // Guard rails: an empty patch and an unknown id are explicit errors.
        let empty = set_installation_state(
            State(state.clone()),
            operator_headers.clone(),
            Path("ctrl-a".into()),
            Json(SetControllerState {
                enabled: None,
                max_concurrent_sessions: None,
                max_actions_per_minute: None,
                actions: None,
                scopes: None,
            }),
        )
        .await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
        let unknown = set_installation_state(
            State(state.clone()),
            operator_headers,
            Path("ctrl-nope".into()),
            Json(SetControllerState {
                enabled: Some(true),
                max_concurrent_sessions: None,
                max_actions_per_minute: None,
                actions: None,
                scopes: None,
            }),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

        // Every PATCH left a before/after audit row (ADR 034 §1).
        let audit = store.controller_event_audit("ctrl-a").unwrap();
        let patches: Vec<_> = audit
            .iter()
            .filter(|row| row.kind == "installation_patched")
            .collect();
        assert_eq!(patches.len(), 2, "two effective patches, two audit rows");
        let detail: Value = serde_json::from_str(&patches[0].detail).unwrap();
        assert!(detail["before"].is_object() && detail["after"].is_object());
    }

    #[tokio::test]
    async fn controller_actions_and_signed_runtime_events_form_a_bidirectional_flow() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        seed_bots(&store);
        let auth = auth_config();
        let action_token = token(42);
        install(
            &store,
            &auth,
            "ctrl-duplex",
            "tok-duplex",
            &action_token,
            1,
            SCOPE,
            5,
            60,
        );
        let transport = Arc::new(RecordingEventTransport::default());
        let event_keys = ControllerEventKeys::new(BTreeMap::from([(1, vec![13; 32])])).unwrap();
        let state = AppState::new_with_options_and_runtime_config(
            store.clone(),
            None,
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::pr_review::PrReviewConfig::default(),
            Some(auth),
            Some(Arc::new(ControllerEventRuntime::new(
                event_keys,
                transport.clone(),
            ))),
        );
        store
            .configure_controller_events(
                "ctrl-duplex",
                "https://controller.example.test/runtime-events?version=1",
                1,
                &[
                    "session.opened".into(),
                    "session.progress".into(),
                    "session.terminal".into(),
                ],
                now_ms(),
            )
            .unwrap();

        let opened = request(
            &state,
            &action_token,
            SCOPE,
            &open_action("act-duplex-open", "object:duplex", "v1"),
        );
        let session_id = opened_session_id(opened).await;
        assert_eq!(dispatch_once(&state, now_ms()).await.unwrap(), 1);
        let opened_event: Value =
            serde_json::from_str(&transport.requests.lock().unwrap()[0].body).unwrap();
        assert_eq!(opened_event["event_type"], "session.opened");
        assert_eq!(opened_event["session_id"], session_id);

        let follow_up = request(
            &state,
            &action_token,
            SCOPE,
            &post_action(
                "act-duplex-follow-up",
                &session_id,
                "continue from opened event",
            ),
        );
        assert_eq!(follow_up.status(), StatusCode::OK);
        assert_eq!(dispatch_once(&state, now_ms()).await.unwrap(), 1);
        let progress_event: Value =
            serde_json::from_str(&transport.requests.lock().unwrap()[1].body).unwrap();
        assert_eq!(progress_event["event_type"], "session.progress");
        assert_eq!(progress_event["session_id"], session_id);

        let add_roster = ActionEnvelope {
            version: CURRENT_VERSION,
            action_id: "act-duplex-add-roster".into(),
            action: ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
        };
        let added = request(&state, &action_token, SCOPE, &add_roster);
        assert_eq!(added.status(), StatusCode::OK);
        assert!(store.roster(&session_id).unwrap().contains(&"rev2".into()));

        let closed = request(
            &state,
            &action_token,
            SCOPE,
            &close_action("act-duplex-close", &session_id),
        );
        assert_eq!(closed.status(), StatusCode::OK);
        assert_eq!(dispatch_once(&state, now_ms()).await.unwrap(), 1);
        let requests = transport.requests.lock().unwrap();
        let terminal_event: Value = serde_json::from_str(&requests[2].body).unwrap();
        assert_eq!(terminal_event["event_type"], "session.terminal");
        assert_eq!(terminal_event["session_id"], session_id);
        for request in requests.iter() {
            assert!(request.headers["X-OAB-Signature"].starts_with("sha256="));
            assert_eq!(request.headers["X-OAB-Controller-ID"], "ctrl-duplex");
        }
    }

    #[tokio::test]
    async fn external_github_canary_runs_real_actions_and_receives_signed_terminal_events() {
        const CONTROLLER_ID: &str = "github-canary";
        const CANARY_SCOPE: &str = "tenant:dev/resource:github-canary";
        const FIXTURE: &str = include_str!("../tests/fixtures/github/pull_request_opened.json");

        let store = Arc::new(SqliteStore::memory().unwrap());
        seed_bots(&store);
        let auth = auth_config();
        let action_token = token(77);
        install(
            &store,
            &auth,
            CONTROLLER_ID,
            "tok-github-canary",
            &action_token,
            1,
            CANARY_SCOPE,
            3,
            60,
        );
        for forbidden in ["post_message", "add_roster", "close_session", "emit_status"] {
            store
                .set_controller_action_grant(CONTROLLER_ID, forbidden, false)
                .unwrap();
        }

        let event_transport = Arc::new(InProcessControllerEventTransport::default());
        let event_keys = ControllerEventKeys::new(BTreeMap::from([(1, vec![31; 32])])).unwrap();
        let event_secret = event_keys.issued_secret(1, CONTROLLER_ID).unwrap();
        let ocp_state = AppState::new_with_options_and_runtime_config(
            store.clone(),
            None,
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
            0,
            crate::plugins::pr_review::PrReviewConfig::default(),
            Some(auth),
            Some(Arc::new(ControllerEventRuntime::new(
                event_keys,
                event_transport.clone(),
            ))),
        );
        store
            .configure_controller_events(
                CONTROLLER_ID,
                "https://controller.example.test/api/v1/openab/events?version=1",
                1,
                &[
                    "session.opened".into(),
                    "session.progress".into(),
                    "session.terminal".into(),
                    "session.timeout".into(),
                    "session.superseded".into(),
                    "action.failed".into(),
                ],
                now_ms(),
            )
            .unwrap();

        let controller_config = github_pr_controller::config::Config {
            addr: "127.0.0.1:0".into(),
            db_path: ":memory:".into(),
            mode: github_pr_controller::config::OperatingMode::ExternalCanary,
            webhook_secret: Some("fixture-secret".into()),
            shadow_secret: None,
            observer_secret: Some("observer-secret".into()),
            canary_repository: Some("example/repo".into()),
            allowed_repos: std::collections::BTreeSet::from(["example/repo".into()]),
            bot_handle: Some("fixture-council".into()),
            roster: vec!["chair".into(), "rev1".into(), "rev2".into()],
            council_preset: None,
            review_mode: "approve".into(),
            ocp_action: github_pr_controller::config::OcpActionConfig {
                base_url: Some("https://control-plane.test".into()),
                action_token: Some(action_token.clone()),
                scope: Some(CANARY_SCOPE.into()),
                controller_id: Some(CONTROLLER_ID.into()),
            },
            event_signing_secret: Some(event_secret.clone()),
            github_app: github_pr_controller::config::GitHubAppConfig {
                app_id: None,
                installation_id: None,
                private_key: None,
            },
            // This e2e drives the controller against an in-process plane, not
            // GitHub — the write client stays off.
            enable_writes: false,
            github_api_base: "https://api.github.com".into(),
        };
        let verifier = github_pr_controller::runtime_events::RuntimeEventVerifier::new(
            CONTROLLER_ID,
            &event_secret,
        )
        .unwrap();
        let action_client = InProcessOcpActionClient {
            state: ocp_state.clone(),
            token: action_token.clone(),
            scope: CANARY_SCOPE.into(),
        };
        let controller_state = Arc::new(github_pr_controller::AppState::with_components(
            controller_config,
            github_pr_controller::store::SqliteStore::open(":memory:").unwrap(),
            Some(Arc::new(action_client)),
            Some(Arc::new(verifier)),
        ));
        let controller_router = github_pr_controller::router(controller_state.clone());
        *event_transport.router.lock().unwrap() = Some(controller_router.clone());

        let embedded_surface = format!(
            "embedded_github_webhook_repo:{}",
            hex::encode(Sha256::digest(b"example/repo"))
        );
        let embedded_count = || {
            store
                .compatibility_usage()
                .unwrap()
                .into_iter()
                .find(|usage| usage.surface == embedded_surface)
                .map(|usage| usage.uses)
                .unwrap_or(0)
        };
        let embedded_baseline = embedded_count();

        let first = controller_router
            .clone()
            .oneshot(github_controller_webhook(
                "canary-e2e-1",
                "pull_request",
                FIXTURE.into(),
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first: Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let first_session = first["action_result"]["result"]["data"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        let duplicate = controller_router
            .clone()
            .oneshot(github_controller_webhook(
                "canary-e2e-1",
                "pull_request",
                FIXTURE.into(),
            ))
            .await
            .unwrap();
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert_eq!(store.list_sessions(None, None, 20).unwrap().len(), 1);

        while dispatch_once(&ocp_state, now_ms()).await.unwrap() > 0 {}
        let opened_summary = controller_state
            .store
            .as_ref()
            .unwrap()
            .canary_summary()
            .await
            .unwrap();
        assert_eq!(opened_summary.runtime_event_types["session.opened"], 1);

        let mut synchronize: Value = serde_json::from_str(FIXTURE).unwrap();
        synchronize["action"] = serde_json::json!("synchronize");
        synchronize["pull_request"]["head"]["sha"] = serde_json::json!("def456");
        let superseded = controller_router
            .clone()
            .oneshot(github_controller_webhook(
                "canary-e2e-2",
                "pull_request",
                synchronize.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(superseded.status(), StatusCode::ACCEPTED);
        let superseded: Value =
            serde_json::from_slice(&superseded.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            superseded["action_result"]["result"]["data"]["old_id"],
            first_session
        );
        let active_session = superseded["action_result"]["result"]["data"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        while dispatch_once(&ocp_state, now_ms()).await.unwrap() > 0 {}

        crate::orchestrator::handle_reply(
            &ocp_state,
            "rev1",
            bot_reply(&active_session, "review one [done]"),
        )
        .unwrap();
        crate::orchestrator::handle_reply(
            &ocp_state,
            "rev2",
            bot_reply(&active_session, "review two [done]"),
        )
        .unwrap();
        crate::orchestrator::handle_reply(
            &ocp_state,
            "chair",
            bot_reply(&active_session, "final verdict [done]"),
        )
        .unwrap();
        assert_eq!(
            SessionState::from_db_str(&store.session(&active_session).unwrap().unwrap().state),
            SessionState::Closed
        );
        while dispatch_once(&ocp_state, now_ms()).await.unwrap() > 0 {}

        let mut timeout_payload: Value = serde_json::from_str(FIXTURE).unwrap();
        timeout_payload["pull_request"]["head"]["sha"] = serde_json::json!("timeout789");
        let timeout_open = controller_router
            .clone()
            .oneshot(github_controller_webhook(
                "canary-e2e-3",
                "pull_request",
                timeout_payload.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(timeout_open.status(), StatusCode::ACCEPTED);
        let timeout_open: Value =
            serde_json::from_slice(&timeout_open.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let timeout_session = timeout_open["action_result"]["result"]["data"]["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(crate::orchestrator::force_close_timeout(&ocp_state, &timeout_session).unwrap());
        while dispatch_once(&ocp_state, now_ms()).await.unwrap() > 0 {}

        let summary = controller_state
            .store
            .as_ref()
            .unwrap()
            .canary_summary()
            .await
            .unwrap();
        assert_eq!(summary.acted_deliveries, 3);
        assert!(summary.runtime_event_types["session.superseded"] >= 1);
        assert!(summary.runtime_event_types["session.terminal"] >= 1);
        assert!(summary.runtime_event_types["session.timeout"] >= 1);
        assert_eq!(embedded_count(), embedded_baseline);

        let forbidden_follow_up = request(
            &ocp_state,
            &action_token,
            CANARY_SCOPE,
            &post_action(
                "canary-forbidden-follow-up",
                &timeout_session,
                "not granted",
            ),
        );
        assert_eq!(forbidden_follow_up.status(), StatusCode::FORBIDDEN);
    }
}
