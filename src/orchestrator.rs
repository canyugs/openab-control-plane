//! Orchestration (design §13): the deterministic referee. The plane owns the
//! lifecycle, fanout, and quorum; the chair bot is the only LLM judgment.

use crate::protocol::{Content, GatewayReply, GatewayResponse, SenderInfo, RESPONSE_SCHEMA};
use crate::coordinator::{self, Action, Ctx};
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
    let Some(session) = state.store.session(session_id)? else {
        anyhow::bail!("unknown session {session_id}");
    };
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
    // Who is prompted to act now is a coordinator decision: council/solo mention
    // everyone (all start); pipeline mentions only stage 0. Non-starters still get
    // the trigger as context (gates/history) but aren't mentioned, so they wait.
    // A stock OAB bot in a group gates on @mention before a thread exists
    // (gateway.rs is_responder); bot_username == the plane's bot name (served in
    // /bot-config), so a recipient's own name matches its gate.
    let starters = coordinator::for_session(&session.mode).starters(&roster);
    for target in routing::fanout_targets(&roster, None) {
        let tname = state.store.bot(&target)?.map(|b| b.name).unwrap_or_default();
        let mentions = if starters.contains(&target) { vec![tname] } else { vec![] };
        state.deliver_event(
            &target,
            session_id,
            thread.as_deref(),
            sender.clone(),
            Content::text(content),
            mentions,
            &msg.id,
        );
    }
    state.emit_north("message", session_id, json!({ "message_id": msg.id, "author": "client", "content": content }));
    Ok(msg)
}

/// Liveness guarantee (design: "what OCP actually guarantees"). Force a stuck
/// session to a terminal verdict. A silent reviewer otherwise hangs
/// `QuorumCouncil` forever (quorum never reached), and a dead bot can't run its
/// own fallback — so only the plane can guarantee termination. Mode-agnostic:
/// closes with the reviews already in the thread, naming absentees in the
/// verdict. CAS once-only — returns true iff this call performed the close (a
/// normal close racing in wins and this becomes a no-op).
pub fn force_close_timeout(state: &Arc<AppState>, session_id: &str) -> Result<bool> {
    if !state.store.close_if_active(session_id)? {
        return Ok(false); // already terminal
    }
    let roster = state.store.roster(session_id)?;
    let done: std::collections::HashSet<String> = state
        .store
        .reactors_in_session(session_id, DONE_EMOJI)?
        .into_iter()
        .collect();
    let absent: Vec<&str> = roster.iter().map(String::as_str).filter(|b| !done.contains(*b)).collect();
    let verdict = format!(
        "⏱️ Session closed by timeout — {}/{} signaled done.{} (Verdict not synthesized; reviews are in the thread.)",
        done.len(),
        roster.len(),
        if absent.is_empty() { String::new() } else { format!(" Absent: {}.", absent.join(", ")) },
    );
    state.emit_north("verdict", session_id, json!({ "text": verdict, "reason": "timeout" }));
    state.emit_north("state", session_id, json!({ "state": "closed" }));
    tracing::warn!("watchdog force-closed stale session {session_id}");
    Ok(true)
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

/// Outcome of an admission decision (membership plane, ADR 001). The plane
/// guarantees a session roster stays bounded and valid; every add — north-driven
/// today, bot-recruited later — passes through this one gate.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Added,
    AlreadyMember,
    Rejected(&'static str),
}

/// Pure admission policy → unit-tested; `add_to_roster` supplies the live values.
/// `Added` is provisional approval — the caller still performs the insert+backfill.
fn admit(known: bool, already_member: bool, roster_len: usize, max: usize) -> Admission {
    if already_member {
        Admission::AlreadyMember // idempotent re-add, even at capacity
    } else if !known {
        Admission::Rejected("unknown bot") // never registered → would hang the roster
    } else if roster_len >= max {
        Admission::Rejected("roster full") // bounded growth
    } else {
        Admission::Added
    }
}

/// Max session roster size (admission quota). ponytail: env read per add — adds
/// are rare mid-session events; default 16, bump via `OABCP_MAX_ROSTER`.
fn max_roster() -> usize {
    std::env::var("OABCP_MAX_ROSTER")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(16)
}

/// Add a bot to a session mid-flight and backfill the conversation so far,
/// through the admission gate. The history is replayed via the durable outbox
/// (same as live delivery), so it arrives in order whether the bot is online now
/// or connects later — OAB batches the in-thread burst into context. Errors only
/// on an unknown session; rejection/idempotency are reported via `Admission`.
pub fn add_to_roster(state: &Arc<AppState>, session_id: &str, bot_id: &str) -> Result<Admission> {
    if state.store.session(session_id)?.is_none() {
        anyhow::bail!("unknown session {session_id}");
    }
    let roster = state.store.roster(session_id)?;
    let decision = admit(
        state.store.bot(bot_id)?.is_some(),
        roster.iter().any(|b| b == bot_id),
        roster.len(),
        max_roster(),
    );
    if decision != Admission::Added {
        return Ok(decision);
    }
    // approved → insert + backfill. add_session_bot stays the authoritative guard
    // (false on a concurrent double-add → already a member, skip the backfill).
    if !state.store.add_session_bot(session_id, bot_id)? {
        return Ok(Admission::AlreadyMember);
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
    Ok(Admission::Added)
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
        let coord = coordinator::for_session(&session.mode);
        let cx = OrchCtx {
            state,
            session,
            roster: state.store.roster(&session.id)?,
        };
        let actions = coord.on_done(&cx, bot_id);
        run_actions(state, session, actions)?;
    }
    Ok(())
}

/// Read-only view the Coordinator decides from; backed by the store.
struct OrchCtx<'a> {
    state: &'a AppState,
    session: &'a Session,
    roster: Vec<String>,
}

impl Ctx for OrchCtx<'_> {
    fn roster(&self) -> &[String] {
        &self.roster
    }
    fn chair(&self) -> Option<&str> {
        self.session.chair_bot.as_deref()
    }
    fn quorum_n(&self) -> i64 {
        self.session.quorum_n
    }
    fn reactors(&self, emoji: &str) -> Vec<String> {
        self.state
            .store
            .reactors_in_session(&self.session.id, emoji)
            .unwrap_or_default()
    }
    /// `bot`'s last non-stub message (skips empty / "…" streaming stubs).
    fn latest_settled(&self, bot: &str) -> Option<String> {
        self.state
            .store
            .messages(&self.session.id)
            .ok()?
            .into_iter()
            .filter(|m| m.author_id.as_deref() == Some(bot))
            .filter(|m| {
                let t = m.content.trim();
                !t.is_empty() && t != "…"
            })
            .next_back()
            .map(|m| m.content)
    }
    fn state(&self) -> SessionState {
        SessionState::from_str(&self.session.state)
    }
}

/// Execute the coordinator's actions. `Transition`/`Close` are CAS-guarded (fire
/// only from their `from` state); a `Prompt` right after a failed `Transition` is
/// suppressed, so the synthesizer is prompted exactly once — on the call that
/// actually enters the new state. The plane emits results and closes; it never
/// acts on the verdict (side-effects are the app's job — design: OCP doesn't own
/// PR logic).
fn run_actions(state: &Arc<AppState>, session: &Session, actions: Vec<Action>) -> Result<()> {
    let mut transition_failed = false;
    for action in actions {
        match action {
            Action::Relay { from, to } => {
                transition_failed = false;
                relay_settled(state, session, &from, &to)?;
            }
            Action::Prompt { to, content } => {
                if transition_failed {
                    continue; // its transition didn't happen — don't prompt
                }
                deliver_system_prompt(state, session, &to, &content)?;
            }
            Action::Transition { from, to } => {
                let to_str = to.as_str();
                let ok = state.store.advance_state(&session.id, from, to)?;
                transition_failed = !ok;
                if ok {
                    state.emit_north("state", &session.id, json!({ "state": to_str }));
                }
            }
            Action::Close { from, verdict } => {
                transition_failed = false;
                if state.store.advance_state(&session.id, from, SessionState::Closed)? {
                    state.emit_north("verdict", &session.id, json!({ "text": verdict }));
                    state.emit_north("state", &session.id, json!({ "state": "closed" }));
                }
            }
        }
    }
    Ok(())
}

/// Deliver `from`'s settled final to `to`, in-thread (no mention needed — OAB
/// bypasses @mention gating inside a thread). Skips if `from` has no settled
/// message (streaming stubs were already filtered out by `latest_settled`).
fn relay_settled(state: &Arc<AppState>, session: &Session, from: &str, to: &str) -> Result<()> {
    let msgs = state.store.messages(&session.id)?;
    let Some(msg) = msgs
        .into_iter()
        .filter(|m| m.author_id.as_deref() == Some(from))
        .filter(|m| {
            let t = m.content.trim();
            !t.is_empty() && t != "…"
        })
        .next_back()
    else {
        return Ok(());
    };
    let bname = state.store.bot(from)?.map(|b| b.name).unwrap_or_default();
    let thread = state.store.thread_for_session(&session.id)?;
    state.deliver_event(
        to,
        &session.id,
        thread.as_deref(),
        bot_sender(from, &bname),
        Content::text(&msg.content),
        vec![],
        &msg.id,
    );
    Ok(())
}

/// Deliver a system message to `to` (e.g. the synthesizer prompt).
fn deliver_system_prompt(state: &Arc<AppState>, session: &Session, to: &str, content: &str) -> Result<()> {
    let to_name = state.store.bot(to)?.map(|b| b.name).unwrap_or_default();
    let thread = state.store.thread_for_session(&session.id)?;
    let msg = state
        .store
        .add_message(&session.id, thread.as_deref(), "system", None, content, None)?;
    state.deliver_event(
        to,
        &session.id,
        thread.as_deref(),
        SenderInfo { id: "system".into(), name: "system".into(), display_name: "system".into(), is_bot: false },
        Content::text(content),
        vec![to_name],
        &msg.id,
    );
    state.emit_north("message", &session.id, json!({ "message_id": msg.id, "author": "system", "content": content }));
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
            .create_session("t", None, 0, Some(&chair.id), &[chair.id.clone()], "council")
            .unwrap();
        store.advance_state(&session.id, SessionState::Open, SessionState::Deliberating).unwrap();
        // history exists before the latecomer joins
        store.add_message(&session.id, None, "client", None, "the task", None).unwrap();
        store.add_message(&session.id, None, "bot", Some(&chair.id), "chair's take", None).unwrap();

        // latecomer joins → backfill enqueues the prior messages into its outbox
        assert_eq!(add_to_roster(&state, &session.id, &latecomer.id).unwrap(), Admission::Added);
        let queued: Vec<_> = store.pending_outbox(&latecomer.id).unwrap();
        assert_eq!(queued.len(), 2, "both prior messages backfilled");
        assert!(queued.iter().any(|(_, f)| f.contains("the task")));
        assert!(queued.iter().any(|(_, f)| f.contains("chair's take")));

        // re-adding is a no-op (no duplicate backfill)
        assert_eq!(add_to_roster(&state, &session.id, &latecomer.id).unwrap(), Admission::AlreadyMember);
        assert_eq!(store.pending_outbox(&latecomer.id).unwrap().len(), 2);
    }

    #[test]
    fn admit_policy_decides() {
        assert_eq!(admit(true, false, 3, 16), Admission::Added);
        assert_eq!(admit(false, false, 3, 16), Admission::Rejected("unknown bot"));
        assert_eq!(admit(true, false, 16, 16), Admission::Rejected("roster full"));
        // already-a-member wins over both unknown and full (idempotent re-add)
        assert_eq!(admit(false, true, 99, 16), Admission::AlreadyMember);
    }

    #[test]
    fn add_to_roster_rejects_unregistered_bot() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let session = store
            .create_session("t", None, 0, Some(&chair.id), &[chair.id.clone()], "council")
            .unwrap();

        // a bot id that was never POST /v1/bots'd must not enter the roster
        let outcome = add_to_roster(&state, &session.id, "ghost-bot").unwrap();
        assert_eq!(outcome, Admission::Rejected("unknown bot"));
        assert!(!store.roster(&session.id).unwrap().iter().any(|b| b == "ghost-bot"));
        assert!(store.pending_outbox("ghost-bot").unwrap().is_empty(), "no backfill for a rejected bot");
    }

    #[test]
    fn roster_authorization_gates_non_members() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let member = store.register_bot("member", "chair", "h1", "t1").unwrap();
        let outsider = store.register_bot("outsider", "reviewer", "h2", "t2").unwrap();
        let session = store
            .create_session("t", None, 0, Some(&member.id), &[member.id.clone()], "council")
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

    #[test]
    fn watchdog_force_closes_stuck_session_once() {
        let store = Arc::new(SqliteStore::memory().unwrap());
        let state = AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let rev = store.register_bot("rev", "reviewer", "h2", "t2").unwrap();
        // quorum needs 1 reviewer done; nobody signals → QuorumCouncil hangs forever
        let session = store
            .create_session("t", None, 1, Some(&chair.id), &[chair.id.clone(), rev.id.clone()], "council")
            .unwrap();
        store.advance_state(&session.id, SessionState::Open, SessionState::Deliberating).unwrap();

        // the watchdog's scan finds it; the close drives it terminal
        assert!(store.active_sessions_before(crate::store::now_ms() + 1).unwrap().contains(&session.id));
        assert!(force_close_timeout(&state, &session.id).unwrap(), "stuck session is closed");
        assert_eq!(
            SessionState::from_str(&store.session(&session.id).unwrap().unwrap().state),
            SessionState::Closed,
        );
        // once-only: a second fire (or a normal close racing) is a no-op, and the
        // session no longer appears as a watchdog candidate
        assert!(!force_close_timeout(&state, &session.id).unwrap(), "second fire is a no-op");
        assert!(!store.active_sessions_before(crate::store::now_ms() + 1).unwrap().contains(&session.id));
    }
}
