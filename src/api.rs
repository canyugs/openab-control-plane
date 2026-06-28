//! North interface: client ⇄ plane (design §12). REST + SSE. A thin client
//! (web UI / desktop app) opens a session, posts an intent, renders the stream —
//! no chat platform anywhere. v1 auth = bearer API key.

use crate::identity;
use crate::orchestrator;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/bots", post(register_bot))
        .route("/v1/sessions", post(open_session))
        .route("/v1/sessions/:id", get(get_session))
        .route("/v1/sessions/:id/messages", post(post_message))
        .route("/v1/sessions/:id/roster", post(add_roster))
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
    let Some(ref key) = state.api_key else { return Ok(()) };
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
    Ok(Json(json!({ "bot_id": bot.id, "token": token, "role": bot.role })))
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

async fn add_roster(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<AddRoster>,
) -> Result<axum::response::Response, StatusCode> {
    check_auth(&state, &headers)?;
    use orchestrator::Admission::*;
    // Err = unknown session (404); admission rejection = 409 with a reason.
    match orchestrator::add_to_roster(&state, &id, &req.bot_id).map_err(|_| StatusCode::NOT_FOUND)? {
        Added => Ok(Json(json!({ "added": true })).into_response()),
        AlreadyMember => Ok(Json(json!({ "added": false })).into_response()),
        Rejected(reason) => {
            Ok((StatusCode::CONFLICT, Json(json!({ "added": false, "rejected": reason }))).into_response())
        }
    }
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
    Ok(Json(json!({ "session": session, "messages": messages, "roster": roster })))
}

#[derive(Deserialize)]
struct BotConfigParams {
    /// Which agent CLI this bot runs. A mixed-provider council sets a different
    /// value per pod (in its `/bot-config` fetch URL); omit for an all-Claude one.
    agent: Option<String>,
}

/// Maps a provider name to the OAB `[agent]` `command` + `args`. OAB splits the
/// two (`config.rs`: `command: String`, `args: Vec<String>`), so multi-word
/// invocations like `gemini --acp` must be split here. Unknown names pass
/// through as a raw command (escape hatch for agents not in the table).
fn agent_command(agent: &str) -> (String, Vec<&'static str>) {
    match agent {
        "claude" | "claude-agent-acp" => ("claude-agent-acp".into(), vec![]),
        "codex" => ("codex-acp".into(), vec![]),
        "gemini" => ("gemini".into(), vec!["--acp"]),
        "grok" => ("grok".into(), vec!["agent", "stdio"]),
        "kiro" => ("kiro-cli".into(), vec!["acp", "--trust-all-tools"]),
        "copilot" => ("copilot".into(), vec!["--acp", "--stdio"]),
        other => (other.to_string(), vec![]),
    }
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
    let (command, args) = agent_command(&agent);
    let args_line = if args.is_empty() {
        String::new()
    } else {
        let joined = args
            .iter()
            .map(|a| format!("{a:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("\nargs = [{joined}]")
    };
    // The chair writes to the PR, so it posts as the shared GitHub App (clean
    // `[bot]` attribution) — not its pod PAT. A pre_boot hook mints an installation
    // token and `gh auth login`s as the App, then refreshes before the 1h expiry
    // (mirrors multi-agent-review-ops). The App key + minter live on the chair's
    // persistent volume (~/.github-app.pem, ~/bin/get-gh-app-token.sh), NOT in env
    // (the hook env is sanitized). Reviewers don't write, so they keep GH_TOKEN.
    // Safe no-op (`on_failure = "warn"` + exit 0) until the volume is provisioned.
    let hooks_section = if bot.role == "chair" {
        "\n[hooks.pre_boot]\non_failure = \"warn\"\ntimeout_seconds = 60\ninline = '''\n#!/bin/sh\nset -e\nexport HOME=/home/node\nif [ ! -x \"$HOME/bin/get-gh-app-token.sh\" ]; then echo \"github app auth not provisioned (no get-gh-app-token.sh); skipping\"; exit 0; fi\nauth() { \"$HOME/bin/get-gh-app-token.sh\" | gh auth login --with-token; }\nauth\ngit config --global credential.helper '!gh auth git-credential' || true\n# refresh before the 1h installation-token expiry\n( while sleep 3000; do auth || true; done ) &\n'''\n"
    } else {
        ""
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
command = "{command}"{args_line}
working_dir = "/home/node"
inherit_env = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "GROK_CODE_XAI_API_KEY", "KIRO_API_KEY", "COPILOT_GITHUB_TOKEN", "GH_TOKEN"]

[pool]
max_sessions = 4
session_ttl_hours = 2
{hooks_section}"#,
        name = bot.name,
        hooks_section = hooks_section,
    );
    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], toml))
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
    let token =
        identity::github_token_for(state.store.as_ref(), app, &state.github_mint_lock, &id, role)
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
    use super::agent_command;

    #[test]
    fn maps_known_providers_and_splits_args() {
        assert_eq!(agent_command("claude"), ("claude-agent-acp".into(), vec![]));
        assert_eq!(agent_command("codex"), ("codex-acp".into(), vec![]));
        assert_eq!(agent_command("gemini"), ("gemini".into(), vec!["--acp"]));
        assert_eq!(
            agent_command("kiro"),
            ("kiro-cli".into(), vec!["acp", "--trust-all-tools"])
        );
    }

    #[test]
    fn unknown_agent_passes_through_as_raw_command() {
        assert_eq!(agent_command("my-custom-acp"), ("my-custom-acp".into(), vec![]));
    }
}
