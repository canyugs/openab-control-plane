//! Orchestration (design §13): the deterministic referee. The plane owns the
//! lifecycle, fanout, and quorum; the chair bot is the only LLM judgment.

use crate::protocol::{Content, GatewayReply, GatewayResponse, SenderInfo, RESPONSE_SCHEMA};
use crate::session::{quorum_reached, DONE_EMOJI};
use crate::state::AppState;
use crate::store::{Message, Session, SessionState};
use crate::{output, routing};
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;

fn bot_sender(id: &str, name: &str) -> SenderInfo {
    SenderInfo {
        id: id.into(),
        name: name.into(),
        display_name: name.into(),
        is_bot: true,
    }
}

/// Fan a stored message out to every roster bot except its author (§10).
fn fanout(state: &AppState, session: &Session, msg: &Message, sender: SenderInfo, mentions: Vec<String>) -> Result<()> {
    let roster = state.store.roster(&session.id)?;
    let thread = state.store.thread_for_session(&session.id)?;
    let author = msg.author_id.as_deref();
    for target in routing::fanout_targets(&roster, author) {
        state.deliver_event(
            &target,
            &session.id,
            thread.as_deref(),
            sender.clone(),
            Content::text(&msg.content),
            mentions.clone(),
            &msg.id,
        );
    }
    Ok(())
}

/// Client posts the opening intent. Stores it, moves open→deliberating, fans the
/// trigger to the whole roster (mentioning the chair so it opens the thread).
pub fn post_client_message(state: &Arc<AppState>, session_id: &str, content: &str) -> Result<Message> {
    let session = match state.store.session(session_id)? {
        Some(s) => s,
        None => anyhow::bail!("unknown session {session_id}"),
    };
    let msg = state.store.add_message(session_id, None, "client", None, content, None)?;
    state.store.advance_state(session_id, SessionState::Open, SessionState::Deliberating)?;

    let mentions = match &session.chair_bot {
        Some(c) => state.store.bot(c)?.map(|b| vec![b.name]).unwrap_or_default(),
        None => vec![],
    };
    let sender = SenderInfo {
        id: "client".into(),
        name: "client".into(),
        display_name: "client".into(),
        is_bot: false,
    };
    let roster = state.store.roster(session_id)?;
    let thread = state.store.thread_for_session(session_id)?;
    for target in routing::fanout_targets(&roster, None) {
        state.deliver_event(
            &target,
            session_id,
            thread.as_deref(),
            sender.clone(),
            Content::text(content),
            mentions.clone(),
            &msg.id,
        );
    }
    state.emit_north("message", session_id, json!({ "message_id": msg.id, "author": "client", "content": content }));
    Ok(msg)
}

/// Dispatch a bot's GatewayReply (the south handler core).
pub fn handle_reply(state: &Arc<AppState>, bot_id: &str, reply: GatewayReply) -> Result<()> {
    let session_id = reply.channel.id.clone();
    let session = match state.store.session(&session_id)? {
        Some(s) => s,
        None => {
            tracing::warn!("reply for unknown session {session_id}");
            return Ok(());
        }
    };
    let bot = state.store.bot(bot_id)?;
    let bot_name = bot.as_ref().map(|b| b.name.clone()).unwrap_or_default();

    match reply.command.as_deref() {
        None => on_send(state, &session, bot_id, &bot_name, &reply)?,
        Some("create_topic") => on_create_topic(state, &session, bot_id, &reply)?,
        Some("add_reaction") => on_reaction(state, &session, bot_id, &reply, true)?,
        Some("remove_reaction") => on_reaction(state, &session, bot_id, &reply, false)?,
        Some("edit_message") => on_edit(state, &session, bot_id, &reply)?,
        Some("delete_message") => {} // no-op for the council; audit keeps history
        Some(other) => tracing::warn!("unknown command {other}"),
    }
    Ok(())
}

fn on_send(state: &Arc<AppState>, session: &Session, bot_id: &str, bot_name: &str, reply: &GatewayReply) -> Result<()> {
    let thread = reply
        .channel
        .thread_id
        .clone()
        .or(state.store.thread_for_session(&session.id)?);
    let msg = state.store.add_message(
        &session.id,
        thread.as_deref(),
        "bot",
        Some(bot_id),
        &reply.content.text,
        reply.quote_message_id.as_deref(),
    )?;
    fanout(state, session, &msg, bot_sender(bot_id, bot_name), vec![])?;
    state.emit_north("message", &session.id, json!({ "message_id": msg.id, "author": bot_name, "content": reply.content.text }));
    ack(state, bot_id, reply, None, Some(&msg.id));

    // Chair's message while in quorum = the verdict → close.
    if session.chair_bot.as_deref() == Some(bot_id)
        && state.store.advance_state(&session.id, SessionState::Quorum, SessionState::Closed)?
    {
        state.emit_north("verdict", &session.id, json!({ "text": reply.content.text }));
        state.emit_north("state", &session.id, json!({ "state": "closed" }));
        output::fire(state, session, &reply.content.text)?;
    }
    Ok(())
}

fn on_create_topic(state: &Arc<AppState>, session: &Session, bot_id: &str, reply: &GatewayReply) -> Result<()> {
    let thread_id = state.store.upsert_thread(&session.id, reply.quote_message_id.as_deref())?;
    state.emit_north("thread", &session.id, json!({ "thread_id": thread_id }));
    ack(state, bot_id, reply, Some(&thread_id), None);
    Ok(())
}

fn on_reaction(state: &Arc<AppState>, session: &Session, bot_id: &str, reply: &GatewayReply, add: bool) -> Result<()> {
    let target = reply
        .quote_message_id
        .clone()
        .unwrap_or_else(|| session.id.clone());
    let emoji = &reply.content.text;
    if add {
        state.store.add_reaction(&target, bot_id, emoji)?;
    } else {
        state.store.remove_reaction(&target, bot_id, emoji)?;
    }
    state.emit_north("reaction", &session.id, json!({ "bot": bot_id, "emoji": emoji, "add": add }));
    ack(state, bot_id, reply, None, None);

    if add && emoji == DONE_EMOJI {
        maybe_quorum(state, session)?;
    }
    Ok(())
}

fn on_edit(state: &Arc<AppState>, session: &Session, bot_id: &str, reply: &GatewayReply) -> Result<()> {
    if let Some(target) = &reply.quote_message_id {
        state.store.edit_message(target, &reply.content.text)?;
        state.emit_north("message_edit", &session.id, json!({ "message_id": target, "content": reply.content.text }));
        ack(state, bot_id, reply, None, Some(target));
    }
    Ok(())
}

/// Count DONE reactors; if quorum, move deliberating→quorum (once) and prompt
/// the chair for a verdict.
fn maybe_quorum(state: &Arc<AppState>, session: &Session) -> Result<()> {
    let roster = state.store.roster(&session.id)?;
    let done = state.store.reactors_in_session(&session.id, DONE_EMOJI)?;
    if !quorum_reached(&roster, session.chair_bot.as_deref(), &done, session.quorum_n) {
        return Ok(());
    }
    if !state.store.advance_state(&session.id, SessionState::Deliberating, SessionState::Quorum)? {
        return Ok(()); // someone else already advanced it
    }
    state.emit_north("state", &session.id, json!({ "state": "quorum" }));

    if let Some(chair) = &session.chair_bot {
        let chair_name = state.store.bot(chair)?.map(|b| b.name).unwrap_or_default();
        let prompt = "Quorum reached. Chair, please render the verdict.";
        let msg = state.store.add_message(&session.id, state.store.thread_for_session(&session.id)?.as_deref(), "system", None, prompt, None)?;
        let thread = state.store.thread_for_session(&session.id)?;
        state.deliver_event(
            chair,
            &session.id,
            thread.as_deref(),
            SenderInfo { id: "system".into(), name: "system".into(), display_name: "system".into(), is_bot: false },
            Content::text(prompt),
            vec![chair_name],
            &msg.id,
        );
        state.emit_north("message", &session.id, json!({ "message_id": msg.id, "author": "system", "content": prompt }));
    }
    Ok(())
}

// --- ack helpers (only when the reply carried a request_id, §2 streaming) ---

fn ack(state: &AppState, bot_id: &str, reply: &GatewayReply, thread_id: Option<&str>, message_id: Option<&str>) {
    let Some(req) = &reply.request_id else { return };
    let resp = GatewayResponse {
        schema: RESPONSE_SCHEMA.into(),
        request_id: req.clone(),
        success: true,
        thread_id: thread_id.map(String::from),
        message_id: message_id.map(String::from),
        error: None,
    };
    state.send_to_bot(bot_id, serde_json::to_string(&resp).unwrap());
}
