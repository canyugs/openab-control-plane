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
    ControllerSessionBinding, NewControllerActionToken, SessionState,
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
    pub enabled: bool,
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
    match state
        .store
        .set_controller_installation_enabled(&controller_id, request.enabled)
    {
        Ok(true) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "controller_id": controller_id, "enabled": request.enabled }),
            None,
        ),
        Ok(false) => admin_error(StatusCode::NOT_FOUND, "unknown controller installation"),
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
    let allowed_actions = [
        "open_session",
        "post_message",
        "add_roster",
        "close_session",
        "emit_status",
    ];
    let actions = normalized_unique(&request.actions);
    if actions.is_empty()
        || actions
            .iter()
            .any(|action| !allowed_actions.contains(&action.as_str()))
    {
        return Err("actions must contain only supported v1 action names".into());
    }
    let scopes = normalized_unique(&request.scopes);
    if scopes.is_empty()
        || scopes
            .iter()
            .any(|scope| scope.contains('*') || scope.len() > 512)
    {
        return Err("scopes must be explicit non-wildcard values up to 512 bytes".into());
    }
    Ok((actions, scopes))
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
    let external_open = match &envelope.action {
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
            let existing = match state
                .store
                .controller_session_for_trigger(&controller_id, trigger_ref)
            {
                Ok(existing) => existing,
                Err(error) => return internal_store_error(Some(action_id), error),
            };
            Some((
                trigger_ref.to_string(),
                action.trigger_fingerprint.clone(),
                existing,
            ))
        }
        _ => None,
    };

    let existing_session_is_active = match external_open
        .as_ref()
        .and_then(|(_, _, existing)| existing.as_ref())
    {
        Some(binding) => match state.store.session(&binding.session_id) {
            Ok(Some(session)) => !matches!(
                SessionState::from_db_str(&session.state),
                SessionState::Closed | SessionState::Aborted
            ),
            Ok(None) => false,
            Err(error) => return internal_store_error(Some(action_id), error),
        },
        None => false,
    };
    let deduped_session = external_open
        .as_ref()
        .and_then(|(_, fingerprint, existing)| {
            existing.as_ref().and_then(|binding| {
                (existing_session_is_active
                    && matches!(
                        (binding.trigger_fingerprint.as_deref(), fingerprint.as_deref()),
                        (Some(stored), Some(incoming)) if stored == incoming
                    ))
                .then(|| binding.session_id.clone())
            })
        });
    let opens_new_session = match external_open.as_ref() {
        None => false,
        Some((_, _, None)) => true,
        Some((_, _, Some(_))) => !existing_session_is_active,
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
        opens_new_session,
        now_ms(),
    ) {
        Ok(started) => started,
        Err(error) => return internal_store_error(Some(action_id), error),
    };
    match started {
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
        ControllerActionStart::Started => {}
    }

    if external_open
        .as_ref()
        .and_then(|(_, _, existing)| existing.as_ref())
        .is_some_and(|binding| binding.scope != scope)
    {
        let error = ErrorEnvelope {
            version: CURRENT_VERSION,
            action_id: Some(action_id.clone()),
            error: ProtocolError {
                code: ErrorCode::Forbidden,
                message: "controller trigger is bound to another scope".into(),
                retryable: false,
            },
        };
        let body = serde_json::to_string(&error).expect("protocol error serializes");
        if let Err(error) = state.store.finish_controller_action(
            &controller_id,
            &action_id,
            i64::from(StatusCode::FORBIDDEN.as_u16()),
            &body,
            None,
            now_ms(),
        ) {
            return internal_store_error(Some(action_id), error);
        }
        return raw_json_response(StatusCode::FORBIDDEN, body, None);
    }

    let (status, body, binding) = if let Some(session_id) = deduped_session {
        let result = ActionResultEnvelope {
            version: CURRENT_VERSION,
            action_id: action_id.clone(),
            result: ControllerActionResult::SessionOpened {
                session_id,
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
        let binding_input =
            if let (ControllerAction::OpenSession(open), Some((trigger_ref, fingerprint, _))) =
                (&mut action, external_open)
            {
                open.trigger_ref = Some(controller_trigger_ref(&controller_id, &trigger_ref));
                Some((trigger_ref, fingerprint))
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
        ControllerActionDenial::SessionOwnership => protocol_error_response(
            StatusCode::FORBIDDEN,
            action_id,
            ErrorCode::Forbidden,
            "session is not owned by this controller scope",
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
    use crate::controller::{CloseSessionAction, OpenSessionAction, PostMessageAction};
    use crate::store::{SqliteStore, Store};
    use axum::body::to_bytes;
    use serde_json::Value;

    const SCOPE: &str = "tenant:alpha/resource:one";

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
                true,
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
            Json(SetControllerState { enabled: false }),
        )
        .await;
        assert_eq!(disabled.status(), StatusCode::OK);
        assert_eq!(
            request(
                &state,
                &new_token,
                SCOPE,
                &open_action("act-managed-disabled", "object:managed-5", "v1"),
            )
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
