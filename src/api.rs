//! North interface: client ⇄ plane (design §12). REST + SSE. A thin client
//! (web UI / desktop app) opens a session, posts an intent, renders the stream —
//! no chat platform anywhere. v1 auth = bearer API key.

use crate::identity;
use crate::orchestrator;
use crate::session::{reviewers, DONE_EMOJI};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/bots", post(register_bot))
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
        .route("/v1/review", post(review_pr))
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
            post(crate::github_webhook::handle_webhook),
        )
}

fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
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

#[derive(Deserialize)]
struct RegisterBot {
    name: String,
    role: String,
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
struct OpenSession {
    title: String,
    #[serde(default)]
    trigger_ref: Option<String>,
    #[serde(default)]
    roster: Vec<String>,
    quorum_n: i64,
    #[serde(default)]
    chair_bot: Option<String>,
    #[serde(default = "default_mode")]
    mode: String,
}

fn default_mode() -> String {
    "council".to_string()
}

async fn open_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OpenSession>,
) -> Result<impl IntoResponse, StatusCode> {
    check_auth(&state, &headers)?;
    let s = state
        .store
        .create_session(
            &req.title,
            req.trigger_ref.as_deref(),
            req.quorum_n,
            req.chair_bot.as_deref(),
            &req.roster,
            &req.mode,
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "session_id": s.id })))
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
    let msg = orchestrator::post_client_message(&state, &id, &req.content)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(json!({ "message_id": msg.id })))
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
    let (roster, source) = crate::council::runtime_council_roster(&state)
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
        let (roster, source) = crate::council::runtime_council_roster(&state)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(
            json!({ "replaced": false, "noop": true, "roster": roster, "source": source }),
        )
        .into_response());
    }
    let (mut roster, _) = crate::council::runtime_council_roster(&state)
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

/// Maps a provider name to the OAB `[agent]` profile. OAB splits command and
/// args (`config.rs`: `command: String`, `args: Vec<String>`), so multi-word ACP
/// invocations must remain split here. Operators can override or add profiles
/// with `OABCP_AGENT_PROFILES`; unknown names still pass through as a raw command.
fn agent_profile(agent: &str) -> Result<AgentProfile, String> {
    let profiles_json = std::env::var("OABCP_AGENT_PROFILES").ok();
    let working_dir = std::env::var("OABCP_AGENT_WORKING_DIR").ok();
    agent_profile_from(agent, profiles_json.as_deref(), working_dir.as_deref())
}

fn agent_profile_from(
    agent: &str,
    profiles_json: Option<&str>,
    working_dir_override: Option<&str>,
) -> Result<AgentProfile, String> {
    let mut profile = builtin_agent_profile(agent);

    if let Some(raw) = profiles_json.map(str::trim).filter(|v| !v.is_empty()) {
        let overrides: HashMap<String, AgentProfileOverride> = serde_json::from_str(raw)
            .map_err(|err| format!("invalid OABCP_AGENT_PROFILES: {err}"))?;
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
        .chain(
            std::env::var("OABCP_AGENT_INHERIT_ENV")
                .ok()
                .into_iter()
                .flat_map(|value| {
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                }),
        )
    {
        if seen.insert(value.clone()) {
            envs.push(value);
        }
    }
    envs
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
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

/// Serves a stock OAB pod its full config.toml with `[gateway]` pointing back at
/// this plane. Mirrors openab-hub's `/bot-config/{id}`. The chair's "open a
/// thread + @mention reviewers" behavior comes from the trigger message, not
/// steering — so the config stays minimal.
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
    let token = state
        .store
        .bot_token_plain(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
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
        format!(
            "\n[hooks.pre_boot]\non_failure = \"warn\"\ntimeout_seconds = 60\ninline = '''\n#!/bin/sh\nset -e\nexport HOME={home}\nif [ ! -x \"$HOME/bin/get-gh-app-token.sh\" ]; then echo \"github app auth not provisioned (no get-gh-app-token.sh); skipping\"; exit 0; fi\nauth() {{ \"$HOME/bin/get-gh-app-token.sh\" | gh auth login --with-token; }}\nauth\ngit config --global credential.helper '!gh auth git-credential' || true\n# refresh before the 1h installation-token expiry\n( while sleep 3000; do auth || true; done ) &\n'''\n",
            home = sh_single_quote(&profile.working_dir)
        )
    } else {
        String::new()
    };
    let toml = format!(
        r#"[gateway]
url = "{ws_url}"
platform = "feishu"
token = "{token}"
allow_bot_messages = true
bot_username = "{name}"
streaming = true

[agent]
command = {command}{args_line}
working_dir = {working_dir}
inherit_env = {inherit_env}

[pool]
max_sessions = 4
session_ttl_hours = 2
{hooks_section}"#,
        name = bot.name,
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
struct ReviewReq {
    repo: String,
    pr: u64,
    /// Optional preset override (lite|quick|standard|full) for this PR; falls back
    /// to the global env / default. Same precedence as a `review:<preset>` label.
    #[serde(default)]
    preset: Option<String>,
}

/// Convene a council to review a PR — the north REST primitive a droppable GitHub
/// Action (or any CI) calls. Same convene path as the webhook (pointer trigger, bots
/// self-fetch); idempotent (re-runs dedup to the open council for that PR).
async fn review_pr(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ReviewReq>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    let trigger_ref = crate::council::pr_trigger_ref(&req.repo, req.pr);
    if let Some(existing) = state
        .store
        .active_session_for_trigger(&trigger_ref)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Ok(Json(json!({ "session_id": existing, "deduped": true })).into_response());
    }
    let sid = crate::council::convene_for_pr(&state, &req.repo, req.pr, req.preset)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({ "session_id": sid })).into_response())
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
    let Some(app) = state.github_app.as_ref() else {
        // PAT mode — no App provisioned yet. The pod keeps using the shared GH_TOKEN.
        return Err(StatusCode::NOT_IMPLEMENTED);
    };
    state
        .store
        .session(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
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
    let stream = BroadcastStream::new(rx).filter_map(move |r| {
        let raw = r.ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        if v.get("session_id").and_then(|s| s.as_str()) == Some(id.as_str()) {
            Some(Ok(Event::default().data(raw)))
        } else {
            None
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::{agent_profile_from, AgentProfile};

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
}
