//! Orchestration (design §13): the deterministic referee. The plane owns the
//! lifecycle, fanout, and quorum; the chair bot is the only LLM judgment.

use crate::protocol::{Content, GatewayReply, GatewayResponse, SenderInfo, RESPONSE_SCHEMA};
use crate::coordinator::{self, Coordinator};
use crate::session::DONE_EMOJI;
use crate::state::AppState;
use crate::store::{Message, Session, SessionState};
use crate::routing;
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;

/// The edit/reaction target message id. A stock OAB gateway adapter carries it
/// in `reply_to` (it sets `quote_message_id: None` except for explicit
/// reply-quotes — see openab-core gateway.rs edit_message/add_reaction). Prefer
/// reply_to; fall back to quote_message_id for clients that use it instead.
fn target_msg(reply: &GatewayReply) -> Option<&str> {
    Some(reply.reply_to.as_str())
        .filter(|s| !s.is_empty())
        .or(reply.quote_message_id.as_deref())
}

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
    // Don't fan a streaming stub to peers: OAB sends a placeholder first
    // ("…"/empty) then fills it via edit_message (which doesn't re-fan), so a
    // peer bot would only ever see the stub and reply "your message got cut
    // off". ponytail: peers reviewing the same trigger don't need each other's
    // stream; the chair's verdict still reads the stored final via GET. Upgrade
    // path: fan the final content on the author's done-signal (🆗).
    let stub = msg.content.trim();
    if stub.is_empty() || stub == "…" {
        return Ok(());
    }
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
/// trigger to the whole roster (mentioning each recipient so its OAB gate opens).
pub fn post_client_message(state: &Arc<AppState>, session_id: &str, content: &str) -> Result<Message> {
    if state.store.session(session_id)?.is_none() {
        anyhow::bail!("unknown session {session_id}");
    }
    let msg = state.store.add_message(session_id, None, "client", None, content, None)?;
    state.store.advance_state(session_id, SessionState::Open, SessionState::Deliberating)?;

    let sender = SenderInfo {
        id: "client".into(),
        name: "client".into(),
        display_name: "client".into(),
        is_bot: false,
    };
    let roster = state.store.roster(session_id)?;
    let thread = state.store.thread_for_session(session_id)?;
    // Mention EACH recipient in its own copy. A stock OAB bot in a group gates on
    // @mention before a thread exists (gateway.rs is_responder); mentioning only
    // the chair left reviewers gated out of the trigger, so they never saw the
    // task. bot_username == the plane's bot name (served in /bot-config), so the
    // recipient's own name matches its gate.
    for target in routing::fanout_targets(&roster, None) {
        let tname = state.store.bot(&target)?.map(|b| b.name).unwrap_or_default();
        state.deliver_event(
            &target,
            session_id,
            thread.as_deref(),
            sender.clone(),
            Content::text(content),
            vec![tname],
            &msg.id,
        );
    }
    state.emit_north("message", session_id, json!({ "message_id": msg.id, "author": "client", "content": content }));
    Ok(msg)
}

/// Reconstruct the sender of a stored message (for history backfill).
fn sender_for(state: &AppState, m: &Message) -> SenderInfo {
    match m.author_kind.as_str() {
        "bot" => {
            let id = m.author_id.as_deref().unwrap_or("");
            let name = state.store.bot(id).ok().flatten().map(|b| b.name).unwrap_or_default();
            bot_sender(id, &name)
        }
        "system" => SenderInfo { id: "system".into(), name: "system".into(), display_name: "system".into(), is_bot: false },
        _ => SenderInfo { id: "client".into(), name: "client".into(), display_name: "client".into(), is_bot: false },
    }
}

/// Add a bot to a session mid-flight and backfill the conversation so far. The
/// history is replayed through the durable outbox (same as live delivery), so it
/// arrives in order whether the bot is online now or connects later — OAB batches
/// the in-thread burst into context. Returns false if it was already a member.
pub fn add_to_roster(state: &Arc<AppState>, session_id: &str, bot_id: &str) -> Result<bool> {
    if state.store.session(session_id)?.is_none() {
        anyhow::bail!("unknown session {session_id}");
    }
    if !state.store.add_session_bot(session_id, bot_id)? {
        return Ok(false); // already a member — outbox already covers it
    }
    let thread = state.store.thread_for_session(session_id)?;
    for m in state.store.messages(session_id)? {
        if m.author_id.as_deref() == Some(bot_id) {
            continue; // don't echo the joiner's own messages
        }
        state.deliver_event(
            bot_id,
            session_id,
            thread.as_deref(),
            sender_for(state, &m),
            Content::text(&m.content),
            vec![],
            &m.id,
        );
    }
    state.emit_north("roster_add", session_id, json!({ "bot": bot_id }));
    Ok(true)
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
    // Roster authorization (plane-level isolation). The /ws token proves *who*
    // the bot is; this proves it *belongs to this session*. Without it any valid
    // bot could act in any session. This is orthogonal to OAB's own bot-side
    // `allow_*` filters (which decide what a bot responds to) — both layers apply.
    if !state.store.roster(&session_id)?.iter().any(|b| b == bot_id) {
        tracing::warn!("bot {bot_id} not in roster of session {session_id}; dropping reply");
        return Ok(());
    }
    let bot = state.store.bot(bot_id)?;
    let bot_name = bot.as_ref().map(|b| b.name.clone()).unwrap_or_default();

    // Once closed, drop new sends/topics — a bot whose turn was already in
    // flight at close time would otherwise append a post-verdict message (often
    // a "…" stub). Edits still apply (the verdict can finish filling) and
    // reactions are harmless (delivery is already gated in deliver_event).
    let closed = matches!(
        SessionState::from_str(&session.state),
        SessionState::Closed | SessionState::Aborted
    );
    match reply.command.as_deref() {
        None if closed => {}
        Some("create_topic") if closed => {}
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
    // The chair's verdict is closed out on its done-signal (see
    // `maybe_close_verdict`), not here — on_send only ever sees the streaming
    // stub, so closing here would emit `…` as the verdict.
    Ok(())
}

fn on_create_topic(state: &Arc<AppState>, session: &Session, bot_id: &str, reply: &GatewayReply) -> Result<()> {
    let thread_id = state.store.upsert_thread(&session.id, reply.quote_message_id.as_deref())?;
    state.emit_north("thread", &session.id, json!({ "thread_id": thread_id }));
    ack(state, bot_id, reply, Some(&thread_id), None);
    Ok(())
}

fn on_reaction(state: &Arc<AppState>, session: &Session, bot_id: &str, reply: &GatewayReply, add: bool) -> Result<()> {
    let target = target_msg(reply)
        .map(String::from)
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
        let coord = coordinator::for_session(session);
        share_final_with_synthesizer(state, session, coord.as_ref(), bot_id)?;
        maybe_quorum(state, session, coord.as_ref())?;
        maybe_close_verdict(state, session, coord.as_ref(), bot_id)?;
    }
    Ok(())
}

/// The chair's done-signal while the session is in Quorum means its verdict is
/// complete. Close the session and emit the chair's *final* (edit-filled)
/// message as the verdict — not the streaming stub `on_send` would have seen.
/// The Quorum→Closed guard makes this fire only after quorum + only once.
fn maybe_close_verdict(
    state: &Arc<AppState>,
    session: &Session,
    coord: &dyn Coordinator,
    bot_id: &str,
) -> Result<()> {
    if coord.synthesizer(session) != Some(bot_id) {
        return Ok(());
    }
    if !state.store.advance_state(&session.id, SessionState::Quorum, SessionState::Closed)? {
        return Ok(()); // not in quorum yet, or already closed by another reply
    }
    let verdict = state
        .store
        .messages(&session.id)?
        .into_iter()
        .filter(|m| m.author_id.as_deref() == Some(bot_id))
        .next_back()
        .map(|m| m.content)
        .unwrap_or_default();
    // Plane emits the result and closes — it does not act on it. Side-effects
    // (PR comment, label, webhook) are the application's job: a north consumer
    // of these events, or the chair bot's own `gh` call. (design: OCP does NOT
    // own PR logic.)
    state.emit_north("verdict", &session.id, json!({ "text": verdict }));
    state.emit_north("state", &session.id, json!({ "state": "closed" }));
    Ok(())
}

/// On a reviewer's done-signal, deliver its *settled* final reply to the chair
/// so the chair can synthesize a verdict. We suppress streaming-stub fanout (see
/// `fanout`), so without this the chair would only ever see "…" from peers and
/// can't render a quorum verdict. In-thread delivery → no mention needed (OAB
/// bypasses @mention gating inside a thread).
fn share_final_with_synthesizer(
    state: &Arc<AppState>,
    session: &Session,
    coord: &dyn Coordinator,
    bot_id: &str,
) -> Result<()> {
    let Some(chair) = coord.synthesizer(session) else { return Ok(()) };
    if bot_id == chair {
        return Ok(()); // the synthesizer's own done-signal needs no relay
    }
    let last = state
        .store
        .messages(&session.id)?
        .into_iter()
        .filter(|m| m.author_id.as_deref() == Some(bot_id))
        .next_back();
    let Some(msg) = last else { return Ok(()) };
    if msg.content.trim().is_empty() || msg.content.trim() == "…" {
        return Ok(());
    }
    let bname = state.store.bot(bot_id)?.map(|b| b.name).unwrap_or_default();
    let thread = state.store.thread_for_session(&session.id)?;
    state.deliver_event(
        chair,
        &session.id,
        thread.as_deref(),
        bot_sender(bot_id, &bname),
        Content::text(&msg.content),
        vec![],
        &msg.id,
    );
    Ok(())
}

fn on_edit(state: &Arc<AppState>, session: &Session, bot_id: &str, reply: &GatewayReply) -> Result<()> {
    if let Some(target) = target_msg(reply) {
        state.store.edit_message(target, &reply.content.text)?;
        state.emit_north("message_edit", &session.id, json!({ "message_id": target, "content": reply.content.text }));
        ack(state, bot_id, reply, None, Some(target));
    }
    Ok(())
}

/// Count DONE reactors; if the coordinator says converged, move
/// deliberating→quorum (once) and prompt the synthesizer.
fn maybe_quorum(state: &Arc<AppState>, session: &Session, coord: &dyn Coordinator) -> Result<()> {
    let roster = state.store.roster(&session.id)?;
    let done = state.store.reactors_in_session(&session.id, DONE_EMOJI)?;
    if !coord.converged(session, &roster, &done) {
        return Ok(());
    }
    if !state.store.advance_state(&session.id, SessionState::Deliberating, SessionState::Quorum)? {
        return Ok(()); // someone else already advanced it
    }
    state.emit_north("state", &session.id, json!({ "state": "quorum" }));

    if let Some(chair) = coord.synthesizer(session) {
        let chair_name = state.store.bot(chair)?.map(|b| b.name).unwrap_or_default();
        let prompt = coord.converge_prompt();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ReplyChannel;
    use crate::state::AppState;
    use crate::store::{SqliteStore, Store};

    fn msg_reply(session: &str, text: &str) -> GatewayReply {
        GatewayReply {
            schema: String::new(),
            reply_to: String::new(),
            platform: String::new(),
            channel: ReplyChannel { id: session.into(), thread_id: None },
            content: Content::text(text),
            command: None,
            request_id: None,
            quote_message_id: None,
        }
    }

    #[test]
    fn late_joiner_is_backfilled_with_history() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let latecomer = store.register_bot("late", "reviewer", "h2", "t2").unwrap();
        let session = store
            .create_session("t", None, 0, Some(&chair.id), &[chair.id.clone()])
            .unwrap();
        store.advance_state(&session.id, SessionState::Open, SessionState::Deliberating).unwrap();
        // history exists before the latecomer joins
        store.add_message(&session.id, None, "client", None, "the task", None).unwrap();
        store.add_message(&session.id, None, "bot", Some(&chair.id), "chair's take", None).unwrap();

        // latecomer joins → backfill enqueues the prior messages into its outbox
        let added = add_to_roster(&state, &session.id, &latecomer.id).unwrap();
        assert!(added);
        let queued: Vec<_> = store.pending_outbox(&latecomer.id).unwrap();
        assert_eq!(queued.len(), 2, "both prior messages backfilled");
        assert!(queued.iter().any(|(_, f)| f.contains("the task")));
        assert!(queued.iter().any(|(_, f)| f.contains("chair's take")));

        // re-adding is a no-op (no duplicate backfill)
        assert!(!add_to_roster(&state, &session.id, &latecomer.id).unwrap());
        assert_eq!(store.pending_outbox(&latecomer.id).unwrap().len(), 2);
    }

    #[test]
    fn roster_authorization_gates_non_members() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let member = store.register_bot("member", "chair", "h1", "t1").unwrap();
        let outsider = store.register_bot("outsider", "reviewer", "h2", "t2").unwrap();
        let session = store
            .create_session("t", None, 0, Some(&member.id), &[member.id.clone()])
            .unwrap();

        // outsider holds a valid token but is not in the roster → reply dropped
        handle_reply(&state, &outsider.id, msg_reply(&session.id, "sneaky")).unwrap();
        assert!(
            store.messages(&session.id).unwrap().iter().all(|m| m.content != "sneaky"),
            "non-roster bot's message must not be stored"
        );

        // roster member → accepted
        handle_reply(&state, &member.id, msg_reply(&session.id, "legit")).unwrap();
        assert!(
            store.messages(&session.id).unwrap().iter().any(|m| m.content == "legit"),
            "roster member's message must be stored"
        );
    }
}
