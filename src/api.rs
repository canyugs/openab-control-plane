//! North interface: client ⇄ plane (design §12). REST + SSE. A thin client
//! (web UI / desktop app) opens a session, posts an intent, renders the stream —
//! no chat platform anywhere. v1 auth = bearer API key.

use crate::identity;
use crate::orchestrator;
use crate::state::AppState;
use axum::extract::{Path, State};
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
        .route("/v1/sessions/:id/stream", get(stream_session))
        // served on the internal network to stock OAB pods (like openab-hub's
        // /bot-config); no client auth — the token IS the bot's credential.
        .route("/bot-config/:id", get(bot_config))
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
    Ok(Json(json!({ "session": session, "messages": messages })))
}

/// Serves a stock OAB pod its full config.toml with `[gateway]` pointing back at
/// this plane. Mirrors openab-hub's `/bot-config/{id}`. The chair's "open a
/// thread + @mention reviewers" behavior comes from the trigger message, not
/// steering — so the config stays minimal.
async fn bot_config(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
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
    let toml = format!(
        r#"[gateway]
url = "{ws_url}"
platform = "feishu"
token = "{token}"
allow_bot_messages = true
bot_username = "{name}"
streaming = true

[agent]
command = "claude-agent-acp"
working_dir = "/home/node"
inherit_env = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]

[pool]
max_sessions = 4
session_ttl_hours = 2
"#,
        name = bot.name,
    );
    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], toml))
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
