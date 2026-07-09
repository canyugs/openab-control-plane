//! North interface: client ⇄ plane (design §12). REST + SSE. A thin client
//! (web UI / desktop app) opens a session, posts an intent, renders the stream —
//! no chat platform anywhere. v1 auth = bearer API key.

use crate::controller::{
    self, ControllerAction, ControllerActionResult, ControllerError, OpenSessionAction,
    PostMessageAction,
};
use crate::identity;
use crate::orchestrator;
use crate::session::{reviewers, DONE_EMOJI};
use crate::state::AppState;
use crate::store::{BotInventory, BotMetadata, BotMetadataPatch, DeleteBotOutcome};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/stats", get(stats))
        .route("/v1/bots", get(list_bots).post(register_bot))
        .route("/v1/bots/discover", post(discover_bot))
        .route("/v1/bots/:id", patch(patch_bot).delete(delete_bot))
        .route("/v1/sessions", get(list_sessions).post(open_session))
        .route("/v1/session-log", get(session_log_by_query))
        .route("/v1/sessions/:id", get(get_session))
        .route("/v1/sessions/:id/log", get(session_log_by_id))
        .route("/v1/sessions/:id/messages", post(post_message))
        .route("/v1/sessions/:id/roster", post(add_roster))
        .route("/v1/sessions/:id/roster/replace", post(replace_roster))
        .route(
            "/v1/council/roster",
            get(get_council_roster).put(put_council_roster),
        )
        .route("/v1/council/roster/replace", post(replace_council_roster))
        .route("/v1/sessions/:id/stream", get(stream_session))
        // Convene a PR review by ref — the trivial primitive a droppable GitHub
        // Action (or any CI) calls: POST {repo, pr, preset?}. Same convene as the
        // webhook (pointer trigger, bots self-fetch); idempotent per PR.
        .route(
            "/v1/review",
            post(crate::plugins::pr_review::webhook::review_pr),
        )
        // Option A: the plane mints a per-role scoped GitHub installation token for a
        // pod, bound to this session. The pod calls GitHub with it instead of the
        // shared PAT. Closing the session purges it (central revoke).
        .route("/v1/sessions/:id/github-token", post(github_token))
        // served on the internal network to stock OAB pods (like openab-hub's
        // /bot-config); no client auth — the token IS the bot's credential.
        .route("/bot-config/:id", get(bot_config))
        // GitHub webhook ingress — auth is the x-hub-signature-256 HMAC, not the
        // north bearer key, so it's deliberately outside check_auth.
        .route(
            "/api/v1/github_webhooks",
            post(crate::plugins::pr_review::webhook::handle_webhook),
        )
}

pub(crate) fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(ref key) = state.api_key else {
        return Ok(());
    };
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if provided == Some(key.as_str()) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn check_discovery_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = state.bot_discovery_token.as_deref() else {
        return Err(StatusCode::FORBIDDEN);
    };
    if bearer(headers) == Some(expected) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn is_safe_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn normalize_list(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

#[derive(Deserialize)]
struct RegisterBot {
    name: String,
    role: String,
}

/// Read-only observability snapshot (C6): session outcome aggregates from the DB
/// plus a per-bot infra roll-up from the live inventory. Distribution only, NOT
/// a correctness/quality signal; snapshot since this deploy's DB was seeded.
async fn stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let mut snapshot = state
        .store
        .stats(crate::store::now_ms())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let bots = state
        .store
        .list_bots()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let connected = bots.iter().filter(|b| state.is_connected(&b.id)).count();
    let mut by_health: std::collections::BTreeMap<String, usize> = Default::default();
    for b in &bots {
        *by_health.entry(b.health.clone()).or_default() += 1;
    }
    snapshot["bots"] = json!({
        "total": bots.len(),
        "connected": connected,
        "by_health": by_health,
        "detail": bots.iter().map(|b| json!({
            "id": b.id,
            "connected": state.is_connected(&b.id),
            "health": b.health,
            "last_seen_ms": b.last_seen_ms,
            "version": b.version,
        })).collect::<Vec<_>>(),
    });
    Ok(Json(snapshot))
}

async fn register_bot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RegisterBot>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let (bot, token) = identity::issue(state.store.as_ref(), &req.name, &req.role)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({ "bot_id": bot.id, "token": token, "role": bot.role }),
    ))
}

#[derive(Deserialize)]
struct ListBots {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    capability: Option<String>,
    #[serde(default)]
    connected: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    health: Option<String>,
}

async fn list_bots(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ListBots>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let (standing_roster, source) =
        crate::plugins::pr_review::council::runtime_council_roster(&state)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let standing: BTreeSet<_> = standing_roster.iter().cloned().collect();
    let standing_chair = standing_roster.first().cloned();
    let bots = state
        .store
        .list_bots()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|bot| match params.role.as_ref() {
            Some(role) => bot.role == *role,
            None => true,
        })
        .filter(|bot| match params.provider.as_ref() {
            Some(provider) => bot.provider.as_ref() == Some(provider),
            None => true,
        })
        .filter(|bot| match params.capability.as_ref() {
            Some(capability) => bot.capabilities.iter().any(|c| c == capability),
            None => true,
        })
        .filter(|bot| match params.connected {
            Some(connected) => state.is_connected(&bot.id) == connected,
            None => true,
        })
        .filter(|bot| match params.enabled {
            Some(enabled) => bot.enabled == enabled,
            None => true,
        })
        .filter(|bot| match params.health.as_ref() {
            Some(health) => bot.health == *health,
            None => true,
        })
        .map(|bot| {
            let connected = state.is_connected(&bot.id);
            inventory_json(bot, connected, &standing, standing_chair.as_deref())
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "standing_roster": standing_roster,
        "source": source,
        "bots": bots,
    })))
}

fn inventory_json(
    bot: BotInventory,
    connected: bool,
    standing: &BTreeSet<String>,
    standing_chair: Option<&str>,
) -> Value {
    json!({
        "id": bot.id,
        "name": bot.name,
        "role": bot.role,
        "provider": bot.provider,
        "capabilities": bot.capabilities,
        "connected": connected,
        "enabled": bot.enabled,
        "health": bot.health,
        "note": bot.note,
        "version": bot.version,
        "runtime": bot.runtime,
        "last_seen_ms": bot.last_seen_ms,
        "source": bot.source,
        "rostered": standing.contains(&bot.id),
        "chair": standing_chair == Some(bot.id.as_str()),
    })
}

#[derive(Deserialize)]
struct DiscoverBot {
    id: String,
    #[serde(default)]
    name: Option<String>,
    role: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    runtime: Option<Value>,
}

async fn discover_bot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<DiscoverBot>,
) -> Result<impl IntoResponse, StatusCode> {
    check_discovery_auth(&state, &headers)?;
    if !is_safe_token(&req.id) || !is_safe_token(&req.role) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(provider) = req.provider.as_deref() {
        if !is_safe_token(provider) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let metadata = BotMetadata {
        provider: req.provider,
        capabilities: req.capabilities.map(normalize_list),
        version: req.version,
        runtime: req.runtime,
    };
    let (bot, created) = state
        .store
        .discover_bot(&req.id, req.name.as_deref(), &req.role, &metadata)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let inventory = state
        .store
        .bot_inventory(&bot.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let config_url = bot_config_url(
        &state.config_base_url,
        &bot.id,
        inventory.provider.as_deref(),
    );
    Ok(Json(json!({
        "bot_id": bot.id,
        "created": created,
        "config_url": config_url,
    })))
}

fn bot_config_url(base: &str, bot_id: &str, provider: Option<&str>) -> String {
    debug_assert!(is_safe_token(bot_id));
    let mut url = format!("{}/bot-config/{}", base.trim_end_matches('/'), bot_id);
    if let Some(provider) = provider {
        debug_assert!(is_safe_token(provider));
        // `is_safe_token` restricts this to query-safe token characters.
        url.push_str("?agent=");
        url.push_str(provider);
    }
    url
}

fn nullable_field<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct PatchBot {
    #[serde(default, deserialize_with = "nullable_field")]
    provider: Option<Option<String>>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    health: Option<String>,
    #[serde(default, deserialize_with = "nullable_field")]
    note: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    version: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    runtime: Option<Option<Value>>,
}

async fn patch_bot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<PatchBot>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    if !is_safe_token(&id) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(Some(provider)) = req.provider.as_ref() {
        if !is_safe_token(provider) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    if let Some(health) = req.health.as_deref() {
        if !is_safe_token(health) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let patch = BotMetadataPatch {
        provider: req.provider,
        capabilities: req.capabilities.map(normalize_list),
        enabled: req.enabled,
        health: req.health,
        note: req.note,
        version: req.version,
        runtime: req.runtime,
    };
    if !state
        .store
        .update_bot_metadata(&id, &patch)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let bot = state
        .store
        .bot_inventory(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let (standing_roster, _) = crate::plugins::pr_review::council::runtime_council_roster(&state)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let standing: BTreeSet<_> = standing_roster.iter().cloned().collect();
    let standing_chair = standing_roster.first().cloned();
    let connected = state.is_connected(&bot.id);
    Ok(Json(json!({
        "bot": inventory_json(bot, connected, &standing, standing_chair.as_deref())
    })))
}

async fn delete_bot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    state
        .store
        .bot_inventory(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let (standing_roster, _) = crate::plugins::pr_review::council::runtime_council_roster(&state)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if standing_roster.iter().any(|bot| bot == &id) {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "bot is in the standing roster; remove it first via PUT /v1/council/roster"
            })),
        )
            .into_response());
    }
    if state.is_connected(&id) {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "bot is connected; stop the pod first" })),
        )
            .into_response());
    }

    match state
        .store
        .delete_bot(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        DeleteBotOutcome::Deleted => {
            state.emit_north("bot_deleted", "-", json!({ "bot": id }));
            Ok(Json(json!({ "bot_id": id, "deleted": true })).into_response())
        }
        DeleteBotOutcome::NotFound => Err(StatusCode::NOT_FOUND),
        DeleteBotOutcome::ActiveSession => Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "bot is in an active session" })),
        )
            .into_response()),
    }
}

#[derive(Deserialize)]
struct OpenSession {
    title: String,
    #[serde(default)]
    trigger_ref: Option<String>,
    #[serde(default)]
    trigger_fingerprint: Option<String>,
    #[serde(default)]
    roster: Vec<String>,
    quorum_n: i64,
    #[serde(default)]
    chair_bot: Option<String>,
    #[serde(default = "default_mode")]
    mode: String,
    #[serde(default)]
    prompt: String,
}

fn default_mode() -> String {
    "council".to_string()
}

async fn open_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OpenSession>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    check_auth(&state, &headers).map_err(error_status)?;
    let trigger_fingerprint = req
        .trigger_fingerprint
        .or_else(|| req.trigger_ref.as_ref().cloned());
    let action = OpenSessionAction {
        title: req.title,
        trigger_ref: req.trigger_ref,
        trigger_fingerprint,
        roster: req.roster,
        quorum_n: req.quorum_n,
        chair_bot: req.chair_bot,
        mode: req.mode,
        prompt: req.prompt,
    };
    match controller::execute(&state, ControllerAction::OpenSession(action)) {
        Ok(ControllerActionResult::SessionOpened {
            session_id,
            deduped,
        }) => Ok(Json(
            json!({ "session_id": session_id, "deduped": deduped }),
        )),
        Ok(ControllerActionResult::Superseded { session_id, old_id }) => Ok(Json(json!({
            "session_id": session_id,
            "deduped": false,
            "superseded": true,
            "old_session_id": old_id,
        }))),
        Ok(ControllerActionResult::MessagePosted { .. }) => {
            Err(error_status(StatusCode::INTERNAL_SERVER_ERROR))
        }
        Err(ControllerError::Invalid(message)) => {
            Err((StatusCode::BAD_REQUEST, Json(json!({ "error": message }))))
        }
        Err(ControllerError::Internal(_)) => Err(error_status(StatusCode::INTERNAL_SERVER_ERROR)),
    }
}

fn error_status(status: StatusCode) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": status.to_string() })))
}

#[derive(Deserialize)]
struct ListSessions {
    #[serde(default)]
    trigger_ref: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

fn validate_state_filter(state: Option<&str>) -> Result<Option<&str>, StatusCode> {
    match state {
        None => Ok(None),
        Some("open" | "deliberating" | "quorum" | "closed" | "aborted") => Ok(state),
        Some(_) => Err(StatusCode::BAD_REQUEST),
    }
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(req): Query<ListSessions>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let state_filter = validate_state_filter(req.state.as_deref())?;
    let limit = req.limit.unwrap_or(20).clamp(1, 100);
    let sessions = state
        .store
        .list_sessions(req.trigger_ref.as_deref(), state_filter, limit)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "sessions": sessions, "limit": limit })))
}

#[derive(Deserialize)]
struct SessionLogQuery {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    trigger_ref: Option<String>,
    #[serde(default)]
    tail_chars: Option<usize>,
}

#[derive(Deserialize)]
struct SessionLogParams {
    #[serde(default)]
    tail_chars: Option<usize>,
}

async fn session_log_by_query(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(req): Query<SessionLogQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let session = match (req.id.as_deref(), req.trigger_ref.as_deref()) {
        (Some(id), _) => state
            .store
            .session(id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?,
        (None, Some(trigger_ref)) => state
            .store
            .list_sessions(Some(trigger_ref), None, 1)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .into_iter()
            .next()
            .ok_or(StatusCode::NOT_FOUND)?,
        (None, None) => return Err(StatusCode::BAD_REQUEST),
    };
    let body = render_session_log(&state, &session, req.tail_chars)?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body))
}

async fn session_log_by_id(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(req): Query<SessionLogParams>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let session = state
        .store
        .session(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let body = render_session_log(&state, &session, req.tail_chars)?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body))
}

fn render_session_log(
    state: &Arc<AppState>,
    session: &crate::store::Session,
    tail_chars: Option<usize>,
) -> Result<String, StatusCode> {
    let tail_chars = tail_chars.unwrap_or(1200).clamp(80, 12000);
    let roster = state
        .store
        .roster(&session.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let messages = state
        .store
        .messages(&session.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let reactions = state
        .store
        .reactions(&session.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut reactions_by_message: HashMap<String, Vec<String>> = HashMap::new();
    let mut done_bots = BTreeSet::new();
    for reaction in &reactions {
        reactions_by_message
            .entry(reaction.message_id.clone())
            .or_default()
            .push(format!("{}:{}", reaction.bot_id, reaction.emoji));
        if reaction.emoji == DONE_EMOJI {
            done_bots.insert(reaction.bot_id.clone());
        }
    }
    let reviewer_ids = reviewers(&roster, session.chair_bot.as_deref());
    let done_reviewers = reviewer_ids
        .iter()
        .filter(|bot| done_bots.contains(*bot))
        .count();

    let mut out = String::new();
    out.push_str(&format!(
        "session {} state={} mode={} trigger_ref={}\n",
        session.id,
        session.state,
        session.mode,
        session.trigger_ref.as_deref().unwrap_or("-"),
    ));
    out.push_str(&format!(
        "created_at={} closed_at={} chair={} quorum={}/{} reviewers_done={}/{}\n",
        session.created_at,
        session
            .closed_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        session.chair_bot.as_deref().unwrap_or("-"),
        done_reviewers,
        session.quorum_n,
        done_reviewers,
        reviewer_ids.len(),
    ));
    out.push_str(&format!("roster: {}\n", roster.join(", ")));
    out.push_str(&format!(
        "done_bots: {}\n",
        if done_bots.is_empty() {
            "-".to_string()
        } else {
            done_bots.into_iter().collect::<Vec<_>>().join(", ")
        }
    ));
    out.push_str(&format!(
        "messages={} reactions={}\n\n",
        messages.len(),
        reactions.len()
    ));

    for msg in messages {
        let author = match msg.author_kind.as_str() {
            "bot" => format!("bot:{}", msg.author_id.as_deref().unwrap_or("-")),
            other => other.to_string(),
        };
        let msg_reactions = reactions_by_message
            .get(&msg.id)
            .map(|items| items.join(", "))
            .unwrap_or_else(|| "-".to_string());
        let done_text = if msg.author_kind == "bot" && is_done_text(&msg.content) {
            " done_text=true"
        } else {
            ""
        };
        out.push_str(&format!(
            "[{}] {} {} len={} reactions=[{}]{}\n",
            msg.created_at,
            msg.id,
            author,
            msg.content.chars().count(),
            msg_reactions,
            done_text,
        ));
        out.push_str(&indent(&tail_text(&msg.content, tail_chars)));
        out.push_str("\n\n");
    }

    Ok(out)
}

fn is_done_text(text: &str) -> bool {
    let t = text.trim();
    t == DONE_EMOJI || t.ends_with("[done]")
}

fn tail_text(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let tail: String = chars[chars.len() - max_chars..].iter().collect();
    format!("...{tail}")
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}\n"))
        .collect::<String>()
}

#[derive(Deserialize)]
struct PostMessage {
    content: String,
}

async fn post_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<PostMessage>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let action = PostMessageAction {
        session_id: id,
        content: req.content,
    };
    match controller::execute(&state, ControllerAction::PostMessage(action)) {
        Ok(ControllerActionResult::MessagePosted { message_id }) => {
            Ok(Json(json!({ "message_id": message_id })))
        }
        Ok(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(ControllerError::Invalid(_)) => Err(StatusCode::NOT_FOUND),
        Err(ControllerError::Internal(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct AddRoster {
    bot_id: String,
}

#[derive(Deserialize)]
struct ReplaceRoster {
    old_bot_id: String,
    new_bot_id: String,
}

async fn add_roster(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<AddRoster>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    use orchestrator::Admission::*;
    // Err = unknown session (404); admission rejection = 409 with a reason.
    match orchestrator::add_to_roster(&state, &id, &req.bot_id)
        .map_err(|_| StatusCode::NOT_FOUND)?
    {
        Added => Ok(Json(json!({ "added": true })).into_response()),
        AlreadyMember => Ok(Json(json!({ "added": false })).into_response()),
        Rejected(reason) => Ok((
            StatusCode::CONFLICT,
            Json(json!({ "added": false, "rejected": reason })),
        )
            .into_response()),
    }
}

async fn replace_roster(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<ReplaceRoster>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    use orchestrator::Replacement::*;
    match orchestrator::replace_roster_bot(&state, &id, &req.old_bot_id, &req.new_bot_id)
        .map_err(|_| StatusCode::NOT_FOUND)?
    {
        Replaced => {
            let roster = state.store.roster(&id).unwrap_or_default();
            Ok(Json(json!({
                "replaced": true,
                "old_bot_id": req.old_bot_id,
                "new_bot_id": req.new_bot_id,
                "roster": roster,
            }))
            .into_response())
        }
        Noop => Ok(Json(json!({ "replaced": false, "noop": true })).into_response()),
        Rejected(reason) => Ok((
            StatusCode::CONFLICT,
            Json(json!({ "replaced": false, "rejected": reason })),
        )
            .into_response()),
    }
}

#[derive(Deserialize)]
struct PutCouncilRoster {
    roster: Vec<String>,
}

async fn get_council_roster(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let (roster, source) = crate::plugins::pr_review::council::runtime_council_roster(&state)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "roster": roster, "source": source })))
}

async fn put_council_roster(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<PutCouncilRoster>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    validate_standing_roster(&state, &req.roster)?;
    state
        .store
        .set_standing_roster(&req.roster)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.emit_north("council_roster", "-", json!({ "roster": req.roster }));
    Ok(Json(json!({ "roster": req.roster, "source": "override" })))
}

async fn replace_council_roster(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ReplaceRoster>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    if req.old_bot_id == req.new_bot_id {
        let (roster, source) = crate::plugins::pr_review::council::runtime_council_roster(&state)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(
            json!({ "replaced": false, "noop": true, "roster": roster, "source": source }),
        )
        .into_response());
    }
    let (mut roster, _) = crate::plugins::pr_review::council::runtime_council_roster(&state)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(idx) = roster.iter().position(|bot| bot == &req.old_bot_id) else {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "replaced": false, "rejected": "old bot not in roster" })),
        )
            .into_response());
    };
    if roster.iter().any(|bot| bot == &req.new_bot_id) {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "replaced": false, "rejected": "replacement already in roster" })),
        )
            .into_response());
    }
    roster[idx] = req.new_bot_id.clone();
    validate_standing_roster(&state, &roster)?;
    state
        .store
        .set_standing_roster(&roster)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.emit_north(
        "council_roster_replace",
        "-",
        json!({ "old_bot": req.old_bot_id, "new_bot": req.new_bot_id, "roster": roster }),
    );
    Ok(Json(json!({
        "replaced": true,
        "old_bot_id": req.old_bot_id,
        "new_bot_id": req.new_bot_id,
        "roster": roster,
        "source": "override",
    }))
    .into_response())
}

fn validate_standing_roster(state: &Arc<AppState>, roster: &[String]) -> Result<(), StatusCode> {
    if roster.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut seen = BTreeSet::new();
    for (idx, bot_id) in roster.iter().enumerate() {
        if bot_id.trim().is_empty() || !seen.insert(bot_id.as_str()) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let bot = state
            .store
            .bot(bot_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        if idx == 0 && bot.role != "chair" {
            return Err(StatusCode::CONFLICT);
        }
        if idx > 0 && bot.role == "chair" {
            return Err(StatusCode::CONFLICT);
        }
    }
    Ok(())
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let session = state
        .store
        .session(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let messages = state.store.messages(&id).unwrap_or_default();
    let roster = state.store.roster(&id).unwrap_or_default();
    let reactions = state.store.reactions(&id).unwrap_or_default();
    Ok(Json(
        json!({ "session": session, "messages": messages, "roster": roster, "reactions": reactions }),
    ))
}

#[derive(Deserialize)]
struct BotConfigParams {
    /// Which agent CLI this bot runs. A mixed-provider council sets a different
    /// value per pod (in its `/bot-config` fetch URL); omit for an all-Claude one.
    agent: Option<String>,
}

const DEFAULT_AGENT_WORKING_DIR: &str = "/home/node";
const DEFAULT_AGENT_INHERIT_ENV: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GROK_CODE_XAI_API_KEY",
    "KIRO_API_KEY",
    "COPILOT_GITHUB_TOKEN",
    "GH_TOKEN",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentProfile {
    command: String,
    args: Vec<String>,
    working_dir: String,
    inherit_env: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AgentProfileOverride {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    inherit_env: Option<Vec<String>>,
}

struct AgentProfileEnv {
    overrides: Result<Option<HashMap<String, AgentProfileOverride>>, String>,
    working_dir_override: Option<String>,
}

/// Maps a provider name to the OAB `[agent]` profile. OAB splits command and
/// args (`config.rs`: `command: String`, `args: Vec<String>`), so multi-word ACP
/// invocations must remain split here. Operators can override or add profiles
/// with `OABCP_AGENT_PROFILES`; unknown names still pass through as a raw command.
fn agent_profile(agent: &str) -> Result<AgentProfile, String> {
    static CONFIG: OnceLock<AgentProfileEnv> = OnceLock::new();
    let config = CONFIG.get_or_init(|| AgentProfileEnv {
        overrides: parse_agent_profile_overrides(
            std::env::var("OABCP_AGENT_PROFILES").ok().as_deref(),
        ),
        working_dir_override: std::env::var("OABCP_AGENT_WORKING_DIR").ok(),
    });
    let overrides = match &config.overrides {
        Ok(overrides) => overrides.as_ref(),
        Err(err) => return Err(err.clone()),
    };
    agent_profile_from_overrides(agent, overrides, config.working_dir_override.as_deref())
}

#[cfg(test)]
fn agent_profile_from(
    agent: &str,
    profiles_json: Option<&str>,
    working_dir_override: Option<&str>,
) -> Result<AgentProfile, String> {
    let overrides = parse_agent_profile_overrides(profiles_json)?;
    agent_profile_from_overrides(agent, overrides.as_ref(), working_dir_override)
}

fn parse_agent_profile_overrides(
    profiles_json: Option<&str>,
) -> Result<Option<HashMap<String, AgentProfileOverride>>, String> {
    profiles_json
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|raw| {
            serde_json::from_str(raw).map_err(|err| format!("invalid OABCP_AGENT_PROFILES: {err}"))
        })
        .transpose()
}

fn agent_profile_from_overrides(
    agent: &str,
    overrides: Option<&HashMap<String, AgentProfileOverride>>,
    working_dir_override: Option<&str>,
) -> Result<AgentProfile, String> {
    let mut profile = builtin_agent_profile(agent);

    if let Some(overrides) = overrides {
        if let Some(override_profile) = overrides.get(agent) {
            let mut base = profile.unwrap_or_else(|| AgentProfile {
                command: String::new(),
                args: Vec::new(),
                working_dir: DEFAULT_AGENT_WORKING_DIR.to_string(),
                inherit_env: Vec::new(),
            });
            if let Some(command) = &override_profile.command {
                base.command = command.clone();
            }
            if let Some(args) = &override_profile.args {
                base.args = args.clone();
            }
            if let Some(working_dir) = &override_profile.working_dir {
                base.working_dir = working_dir.clone();
            }
            if let Some(inherit_env) = &override_profile.inherit_env {
                base.inherit_env = inherit_env.clone();
            }
            if base.command.is_empty() {
                return Err(format!(
                    "agent profile '{agent}' is custom and must set command"
                ));
            }
            profile = Some(base);
        }
    }

    let mut profile = profile.unwrap_or_else(|| AgentProfile {
        command: agent.to_string(),
        args: Vec::new(),
        working_dir: DEFAULT_AGENT_WORKING_DIR.to_string(),
        inherit_env: Vec::new(),
    });
    if let Some(working_dir) = working_dir_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        profile.working_dir = working_dir.to_string();
    }
    Ok(profile)
}

fn builtin_agent_profile(agent: &str) -> Option<AgentProfile> {
    match agent {
        "claude" | "claude-agent-acp" => Some(AgentProfile {
            command: "claude-agent-acp".into(),
            args: vec![],
            working_dir: "/home/node".into(),
            inherit_env: vec![],
        }),
        "codex" => Some(AgentProfile {
            command: "codex-acp".into(),
            args: vec![],
            working_dir: "/home/node".into(),
            inherit_env: vec![],
        }),
        "gemini" => Some(AgentProfile {
            command: "gemini".into(),
            args: vec!["--acp".into()],
            working_dir: "/home/node".into(),
            inherit_env: vec![],
        }),
        "grok" => Some(AgentProfile {
            command: "grok".into(),
            args: vec!["agent".into(), "stdio".into()],
            working_dir: "/home/node".into(),
            inherit_env: vec![],
        }),
        "kiro" => Some(AgentProfile {
            command: "kiro-cli".into(),
            args: vec!["acp".into(), "--trust-all-tools".into()],
            working_dir: "/home/agent".into(),
            inherit_env: vec![],
        }),
        "copilot" => Some(AgentProfile {
            command: "copilot".into(),
            args: vec!["--acp".into(), "--stdio".into()],
            working_dir: "/home/node".into(),
            inherit_env: vec![],
        }),
        _ => None,
    }
}

fn agent_inherit_env(profile: &AgentProfile) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut envs = Vec::new();
    for value in DEFAULT_AGENT_INHERIT_ENV
        .iter()
        .map(|value| (*value).to_string())
        .chain(profile.inherit_env.iter().cloned())
        .chain(extra_agent_inherit_env().iter().cloned())
    {
        if seen.insert(value.clone()) {
            envs.push(value);
        }
    }
    envs
}

fn extra_agent_inherit_env() -> &'static [String] {
    static INHERIT_ENV: OnceLock<Vec<String>> = OnceLock::new();
    INHERIT_ENV.get_or_init(|| {
        parse_extra_agent_inherit_env(std::env::var("OABCP_AGENT_INHERIT_ENV").ok().as_deref())
    })
}

fn parse_extra_agent_inherit_env(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn toml_string(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for ch in value.chars() {
        match ch {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\u{08}' => encoded.push_str("\\b"),
            '\t' => encoded.push_str("\\t"),
            '\n' => encoded.push_str("\\n"),
            '\u{0c}' => encoded.push_str("\\f"),
            '\r' => encoded.push_str("\\r"),
            _ => {
                let code = ch as u32;
                if code <= 0x1f || code == 0x7f || (0x80..=0x9f).contains(&code) {
                    if code <= 0xffff {
                        write!(&mut encoded, "\\u{code:04X}")
                            .expect("writing to String cannot fail");
                    } else {
                        write!(&mut encoded, "\\U{code:08X}")
                            .expect("writing to String cannot fail");
                    }
                } else {
                    encoded.push(ch);
                }
            }
        }
    }
    encoded.push('"');
    encoded
}

fn toml_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| toml_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn sh_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn chair_pre_boot_hook_script(working_dir: &str) -> String {
    format!(
        "#!/bin/sh\nset -e\nexport HOME={home}\nif [ ! -x \"$HOME/bin/get-gh-app-token.sh\" ]; then echo \"github app auth not provisioned (no get-gh-app-token.sh); skipping\"; exit 0; fi\nauth() {{ \"$HOME/bin/get-gh-app-token.sh\" | gh auth login --with-token; }}\nauth\ngit config --global credential.helper '!gh auth git-credential' || true\n# refresh before the 1h installation-token expiry\n( while sleep 3000; do auth || true; done ) &\n",
        home = sh_single_quote(working_dir)
    )
}

/// Serves a stock OAB pod its full config.toml with `[gateway]` pointing back at
/// this plane. Mirrors openab-hub's `/bot-config/{id}`.
///
/// Frozen compatibility surface (ADR 010 B2): bugfix-only until demotion/removal.
/// `tests/bot_config_freeze.rs` snapshot-guards the full response body; any
/// deliberate render change must regenerate those goldens and cite ADR 010 B2.
async fn bot_config(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<BotConfigParams>,
) -> Result<impl IntoResponse, StatusCode> {
    let bot = state
        .store
        .bot(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    // ADR 016: in externalized mode, serve an env reference instead of the token.
    // OpenAB expands `${OABCP_BOT_TOKEN}` from the pod's own env at boot, so the
    // response body carries no secret and an unauthenticated fetch leaks nothing.
    let token = if crate::identity::externalize_tokens() {
        "${OABCP_BOT_TOKEN}".to_string()
    } else {
        state
            .store
            .bot_token_plain(&id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            // An externalized bot stores an empty plaintext; if the flag is later
            // flipped off, treat that as "no servable token" (404), not `token = ""`.
            .filter(|t| !t.is_empty())
            .ok_or(StatusCode::NOT_FOUND)?
    };
    let ws_url = std::env::var("OABCP_WS_URL")
        .unwrap_or_else(|_| "ws://openab-control-plane.zeabur.internal:8080/ws".into());
    // BYOK, per-bot provider. `?agent=` (set per pod) picks the CLI; default is
    // Claude, or OABCP_AGENT_COMMAND for an all-one-provider council. The actual
    // credential is never handled here — inherit_env whitelists every known
    // provider key and the pod carries whatever the deployer set (env_clear
    // drops the rest), so each bot can be on its own key or subscription.
    let agent = params
        .agent
        .or_else(|| std::env::var("OABCP_AGENT_COMMAND").ok())
        .unwrap_or_else(|| "claude".into());
    let profile = agent_profile(&agent).map_err(|err| {
        tracing::warn!(agent = %agent, error = %err, "failed to resolve agent profile");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let args_line = if profile.args.is_empty() {
        String::new()
    } else {
        format!("\nargs = {}", toml_array(&profile.args))
    };
    let command = toml_string(&profile.command);
    let working_dir = toml_string(&profile.working_dir);
    let inherit_env = toml_array(&agent_inherit_env(&profile));
    // The chair writes to the PR, so it posts as the shared GitHub App (clean
    // `[bot]` attribution) — not its pod PAT. A pre_boot hook mints an installation
    // token and `gh auth login`s as the App, then refreshes before the 1h expiry
    // (mirrors multi-agent-review-ops). The App key + minter live on the chair's
    // persistent volume (~/.github-app.pem, ~/bin/get-gh-app-token.sh), NOT in env
    // (the hook env is sanitized). Reviewers don't write, so they keep GH_TOKEN.
    // Safe no-op (`on_failure = "warn"` + exit 0) until the volume is provisioned.
    let hooks_section = if bot.role == "chair" {
        let hook_script = chair_pre_boot_hook_script(&profile.working_dir);
        format!(
            "\n[hooks.pre_boot]\non_failure = \"warn\"\ntimeout_seconds = 60\ninline = {}\n",
            toml_string(&hook_script)
        )
    } else {
        String::new()
    };
    let ws_url = toml_string(&ws_url);
    let token = toml_string(&token);
    let name = toml_string(&bot.name);
    let toml = format!(
        r#"[gateway]
url = {ws_url}
platform = "feishu"
token = {token}
# Explicit: survives OAB's trust-pyramid Phase 3 default flip (L3 deny-all).
# This surface is private and token-authed; senders are the plane itself
# ("client"/"system") plus roster bots, and the roster is dynamic — an
# allowed_users list would go stale after recruit/replace. The WS token is
# the trust boundary, not sender ids.
allow_all_users = true
allow_bot_messages = true
bot_username = {name}
streaming = true
# OCP backfill and council cross-talk intentionally arrive as in-thread bursts;
# per-thread mode lets OAB batch that burst into one context turn.
message_processing_mode = "per-thread"

[agent]
command = {command}{args_line}
working_dir = {working_dir}
inherit_env = {inherit_env}

[pool]
max_sessions = 4
session_ttl_hours = 2

[reactions]
# A6: a cosmetic OAB flag must not be able to erase quorum votes.
remove_after_reply = false
{hooks_section}"#,
        hooks_section = hooks_section,
    );
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        toml,
    ))
}

#[derive(Deserialize)]
struct GithubTokenReq {
    /// Which bot is asking. Required: the token scope is derived from this bot's
    /// stored role, never from a caller-supplied role string — otherwise any caller
    /// could request `{"role":"chair"}` and receive a write token (role escalation).
    bot_id: String,
}

/// Mint (or reuse) a per-role scoped GitHub installation token for this session.
/// 501 if the plane is in PAT mode (no App configured); 404 for an unknown session
/// or bot. The role is always derived from the bot's stored role, so a reviewer can
/// never obtain a write token by asking for one.
async fn github_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<GithubTokenReq>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    let session = state
        .store
        .session(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if matches!(
        crate::store::SessionState::from_db_str(&session.state),
        crate::store::SessionState::Closed | crate::store::SessionState::Aborted
    ) {
        return Err(StatusCode::GONE);
    }
    let Some(app) = state.github_app.as_ref() else {
        // PAT mode — no App provisioned yet. The pod keeps using the shared GH_TOKEN.
        return Err(StatusCode::NOT_IMPLEMENTED);
    };
    // Authoritative from the bot record — the request carries no role.
    let bot = state
        .store
        .bot(&req.bot_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    // The bot must belong to *this* session — otherwise a caller could mint a token
    // for session B using a bot from session A. (Role is still bounded to the bot's
    // stored role, so this is defense-in-depth, not the only guard.)
    let roster = state
        .store
        .roster(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !roster.iter().any(|b| b == &req.bot_id) {
        return Err(StatusCode::FORBIDDEN);
    }
    let role = crate::github_app::Role::from_bot_role(&bot.role);
    let token = identity::github_token_for(
        state.store.as_ref(),
        app,
        &state.github_mint_lock,
        &id,
        role,
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(json!({ "token": token, "role": role.as_str() })).into_response())
}

async fn stream_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    check_auth(&state, &headers)?;
    let rx = state.north_tx.subscribe();
    let stream = north_json_stream(rx, id).map(|raw| Ok(Event::default().data(raw)));
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn north_json_stream(
    rx: broadcast::Receiver<String>,
    session_id: String,
) -> impl Stream<Item = String> {
    BroadcastStream::new(rx).filter_map(move |r| match r {
        Ok(raw) => {
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            if v.get("session_id").and_then(|s| s.as_str()) == Some(session_id.as_str()) {
                Some(raw)
            } else {
                None
            }
        }
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            // `skipped` is the count of dropped GLOBAL events from the plane-wide
            // north broadcast channel. It can include other sessions' events, so
            // this is a gap signal, not a per-session count. Clients should
            // re-fetch authoritative state via GET /v1/sessions/:id/log or
            // GET /v1/sessions/:id, then keep reading this same stream:
            // BroadcastStream yields Lagged once, repositions to the oldest
            // retained value, and continues until the channel closes.
            Some(
                json!({
                    "type": "resync",
                    "session_id": session_id.as_str(),
                    "payload": {
                        "reason": "lagged",
                        "skipped": skipped,
                    },
                    "ts": crate::store::now_ms(),
                })
                .to_string(),
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        agent_profile_from, bot_config, chair_pre_boot_hook_script, parse_extra_agent_inherit_env,
        toml_string, AgentProfile, BotConfigParams,
    };
    use crate::state::AppState;
    use crate::store::now_ms;
    use crate::store::{SessionState, SqliteStore, Store};
    use axum::body::to_bytes;
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::Json;
    use futures::StreamExt;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use tokio::time::{timeout, Duration};

    fn north_event(session_id: &str, seq: u64) -> String {
        json!({
            "type": "message",
            "session_id": session_id,
            "payload": { "seq": seq },
            "ts": now_ms(),
        })
        .to_string()
    }

    fn state_with_review_bots() -> Arc<AppState> {
        let store = Arc::new(SqliteStore::memory().unwrap());
        store
            .seed_bot("chair", "chair", "chair", "h1", "t1")
            .unwrap();
        store
            .seed_bot("rev1", "rev1", "reviewer", "h2", "t2")
            .unwrap();
        store
            .seed_bot("rev2", "rev2", "reviewer", "h3", "t3")
            .unwrap();
        AppState::new_with_options(
            store,
            None,
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
        )
    }

    #[tokio::test]
    async fn open_session_with_prompt_creates_session_and_message_atomically() {
        let state = state_with_review_bots();

        let response = super::open_session(
            State(state.clone()),
            HeaderMap::new(),
            Json(super::OpenSession {
                title: "prompted council".into(),
                trigger_ref: Some("test:prompted".into()),
                trigger_fingerprint: None,
                roster: vec!["chair".into(), "rev1".into()],
                quorum_n: 1,
                chair_bot: Some("chair".into()),
                mode: "council".into(),
                prompt: "please review this".into(),
            }),
        )
        .await
        .unwrap();

        let session_id = response["session_id"].as_str().unwrap().to_string();
        assert_eq!(response["deduped"], json!(false));

        let messages = state.store.messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].author_kind, "client");
        assert_eq!(messages[0].content, "please review this");
    }

    #[tokio::test]
    async fn github_token_refused_for_closed_session() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        store
            .seed_bot("chair", "chair", "chair", "h1", "t1")
            .unwrap();
        let session = store
            .create_session(
                "closed",
                Some("github:pr/o/r#9"),
                0,
                Some("chair"),
                &["chair".into()],
                "solo",
            )
            .unwrap();
        store.set_state(&session.id, SessionState::Closed).unwrap();
        let state = AppState::new_with_options(
            store,
            None,
            None,
            None,
            None,
            "http://control-plane.test".into(),
            None,
        );

        let err = super::github_token(
            State(state),
            HeaderMap::new(),
            Path(session.id),
            Json(super::GithubTokenReq {
                bot_id: "chair".into(),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(err, StatusCode::GONE);
    }

    #[tokio::test]
    async fn lagged_subscriber_gets_resync_and_stream_continues() {
        let (tx, rx) = broadcast::channel::<String>(2);
        let mut stream = Box::pin(super::north_json_stream(rx, "ses_1".to_string()));

        for seq in 1..=5 {
            tx.send(north_event("ses_1", seq)).unwrap();
        }

        let first: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
        assert_eq!(first["type"], "resync");
        assert_eq!(first["session_id"], "ses_1");
        assert_eq!(first["payload"]["reason"], "lagged");
        assert_eq!(first["payload"]["skipped"], 3);

        let second: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
        let third: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
        assert_eq!(second["payload"]["seq"], 4);
        assert_eq!(third["payload"]["seq"], 5);

        tx.send(north_event("ses_1", 6)).unwrap();
        let fourth: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
        assert_eq!(fourth["payload"]["seq"], 6);
    }

    #[tokio::test]
    async fn no_resync_without_lag() {
        let (tx, rx) = broadcast::channel::<String>(8);
        let mut stream = Box::pin(super::north_json_stream(rx, "ses_1".to_string()));

        for seq in 1..=3 {
            tx.send(north_event("ses_1", seq)).unwrap();
        }

        for seq in 1..=3 {
            let event: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
            assert_eq!(event["type"], "message");
            assert_eq!(event["payload"]["seq"], seq);
        }
        assert!(timeout(Duration::from_millis(10), stream.next())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn resync_emitted_even_when_dropped_events_are_other_sessions() {
        let (tx, rx) = broadcast::channel::<String>(2);
        let mut stream = Box::pin(super::north_json_stream(rx, "ses_1".to_string()));

        for seq in 1..=5 {
            tx.send(north_event("ses_2", seq)).unwrap();
        }

        let first: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
        assert_eq!(first["type"], "resync");
        assert_eq!(first["session_id"], "ses_1");
        assert_eq!(first["payload"]["reason"], "lagged");
        assert_eq!(first["payload"]["skipped"], 3);

        assert!(timeout(Duration::from_millis(10), stream.next())
            .await
            .is_err());

        tx.send(north_event("ses_1", 6)).unwrap();
        let next: Value = serde_json::from_str(&stream.next().await.unwrap()).unwrap();
        assert_eq!(next["type"], "message");
        assert_eq!(next["session_id"], "ses_1");
        assert_eq!(next["payload"]["seq"], 6);
    }

    #[test]
    fn maps_known_provider_profiles_and_splits_args() {
        assert_eq!(
            agent_profile_from("claude", None, None).unwrap(),
            AgentProfile {
                command: "claude-agent-acp".into(),
                args: vec![],
                working_dir: "/home/node".into(),
                inherit_env: vec![],
            }
        );
        assert_eq!(
            agent_profile_from("gemini", None, None).unwrap().args,
            vec!["--acp"]
        );
        assert_eq!(
            agent_profile_from("kiro", None, None).unwrap(),
            AgentProfile {
                command: "kiro-cli".into(),
                args: vec!["acp".into(), "--trust-all-tools".into()],
                working_dir: "/home/agent".into(),
                inherit_env: vec![],
            }
        );
    }

    #[test]
    fn unknown_agent_passes_through_as_raw_command() {
        assert_eq!(
            agent_profile_from("my-custom-acp", None, None).unwrap(),
            AgentProfile {
                command: "my-custom-acp".into(),
                args: vec![],
                working_dir: "/home/node".into(),
                inherit_env: vec![],
            }
        );
    }

    #[test]
    fn custom_agent_profile_sets_permissions_working_dir_and_env() {
        let profiles = r#"{
          "cursor": {
            "command": "cursor-agent",
            "args": ["--acp", "--allow-all-tools"],
            "working_dir": "/home/agent",
            "inherit_env": ["CURSOR_API_KEY"]
          }
        }"#;

        assert_eq!(
            agent_profile_from("cursor", Some(profiles), None).unwrap(),
            AgentProfile {
                command: "cursor-agent".into(),
                args: vec!["--acp".into(), "--allow-all-tools".into()],
                working_dir: "/home/agent".into(),
                inherit_env: vec!["CURSOR_API_KEY".into()],
            }
        );
    }

    #[test]
    fn profile_json_can_override_builtin_args() {
        let profiles = r#"{
          "kiro": { "args": ["acp", "--trust-all-tools", "--verbose"] }
        }"#;

        assert_eq!(
            agent_profile_from("kiro", Some(profiles), None)
                .unwrap()
                .args,
            vec!["acp", "--trust-all-tools", "--verbose"]
        );
    }

    #[test]
    fn working_dir_override_applies_after_profile_resolution() {
        assert_eq!(
            agent_profile_from("kiro", None, Some("/workspace"))
                .unwrap()
                .working_dir,
            "/workspace"
        );
    }

    #[test]
    fn custom_profile_requires_command() {
        let err = agent_profile_from("custom", Some(r#"{"custom":{"args":["--acp"]}}"#), None)
            .unwrap_err();
        assert!(err.contains("must set command"));
    }

    #[test]
    fn chair_hook_inline_is_toml_basic_string_safe() {
        let script = chair_pre_boot_hook_script("/home/o'malley");
        let encoded = toml_string(&script);
        assert!(encoded.starts_with('"'));
        assert!(!encoded.contains('\n'));
        assert!(!encoded.contains("'''"));
        assert!(encoded.contains("/home/o"));
        assert!(encoded.contains("malley"));
    }

    #[tokio::test]
    async fn bot_config_pins_per_thread_processing() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let bot = store
            .register_bot("rev", "reviewer", "hash", "plain-token")
            .unwrap();

        let response = bot_config(
            State(state),
            Path(bot.id),
            Query(BotConfigParams { agent: None }),
        )
        .await
        .unwrap()
        .into_response();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let rendered = std::str::from_utf8(&body).unwrap();

        assert!(rendered.contains("message_processing_mode = \"per-thread\""));
    }

    #[test]
    fn toml_string_escapes_toml_basic_string_controls() {
        assert_eq!(
            toml_string("quote\" slash\\ tab\t newline\n form\u{0c} carriage\r back\u{08}"),
            "\"quote\\\" slash\\\\ tab\\t newline\\n form\\f carriage\\r back\\b\""
        );
        assert_eq!(
            toml_string("\u{1f}\u{7f}\u{85}\u{9f}"),
            "\"\\u001F\\u007F\\u0085\\u009F\""
        );
    }

    #[test]
    fn parses_extra_agent_inherit_env_once_ready_shape() {
        assert_eq!(
            parse_extra_agent_inherit_env(Some(" KIRO_API_KEY, , OPENAI_API_KEY ,GH_TOKEN ")),
            vec!["KIRO_API_KEY", "OPENAI_API_KEY", "GH_TOKEN"]
        );
        assert!(parse_extra_agent_inherit_env(None).is_empty());
    }
}
