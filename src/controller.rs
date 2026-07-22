//! Controller action interpreter.
//!
//! Bundled controllers and future external controllers both propose declarative
//! actions; this module is the core-owned boundary that validates and executes
//! them. P3 covers the complete v1 action vocabulary while transport auth and
//! action-id persistence remain outside this module.

use crate::coordinator;
use crate::orchestrator;
use crate::state::AppState;
use crate::store::{OpeningInput, SessionCreateOutcome};
use std::sync::Arc;

pub use controller_protocol::{
    AddRosterAction, CloseSessionAction, ControllerAction, ControllerActionResult,
    EmitStatusAction, OpenSessionAction, PostMessageAction,
};

#[derive(Debug)]
pub enum ControllerError {
    Invalid(String),
    Forbidden(String),
    NotFound(String),
    Gone(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for ControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControllerError::Invalid(message)
            | ControllerError::Forbidden(message)
            | ControllerError::NotFound(message)
            | ControllerError::Gone(message) => f.write_str(message),
            ControllerError::Internal(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ControllerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ControllerError::Invalid(_)
            | ControllerError::Forbidden(_)
            | ControllerError::NotFound(_)
            | ControllerError::Gone(_) => None,
            ControllerError::Internal(err) => err.source(),
        }
    }
}

impl From<anyhow::Error> for ControllerError {
    fn from(err: anyhow::Error) -> Self {
        ControllerError::Internal(err)
    }
}

impl From<orchestrator::PostClientMessageError> for ControllerError {
    fn from(err: orchestrator::PostClientMessageError) -> Self {
        match err {
            orchestrator::PostClientMessageError::UnknownSession(message) => {
                ControllerError::NotFound(message)
            }
            orchestrator::PostClientMessageError::ReopenRefused(message) => {
                ControllerError::Gone(message)
            }
            orchestrator::PostClientMessageError::Internal(err) => ControllerError::Internal(err),
        }
    }
}

type ControllerResult<T> = std::result::Result<T, ControllerError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlledClosePolicy {
    Allow,
    Deny,
}

/// Execute an action with fail-closed controlled-close policy. Callers that
/// have independently authorized `close_session` must opt in explicitly via
/// `execute_with_close_policy`.
pub fn execute(
    state: &Arc<AppState>,
    action: ControllerAction,
) -> ControllerResult<ControllerActionResult> {
    execute_with_close_policy(state, action, ControlledClosePolicy::Deny)
}

pub fn execute_with_close_policy(
    state: &Arc<AppState>,
    action: ControllerAction,
    close_policy: ControlledClosePolicy,
) -> ControllerResult<ControllerActionResult> {
    match action {
        ControllerAction::OpenSession(action) => open_session(state, action),
        ControllerAction::PostMessage(action) => post_message(state, action),
        ControllerAction::AddRoster(action) => add_roster(state, action),
        ControllerAction::CloseSession(action) => close_session(state, action, close_policy),
        ControllerAction::EmitStatus(action) => emit_status(state, action),
    }
}

fn open_session(
    state: &Arc<AppState>,
    action: OpenSessionAction,
) -> ControllerResult<ControllerActionResult> {
    validate_open_session(state, &action)?;
    let opening_inputs = opening_inputs(&action);

    let (session, outcome) = state.store.create_session_superseding(
        &action.title,
        action.trigger_ref.as_deref(),
        action.trigger_fingerprint.as_deref(),
        action.quorum_n,
        action.chair_bot.as_deref(),
        &action.roster,
        &action.mode,
        &opening_inputs,
    )?;
    match outcome {
        SessionCreateOutcome::Deduped => Ok(ControllerActionResult::SessionOpened {
            session_id: session.id,
            deduped: true,
        }),
        SessionCreateOutcome::Created => {
            if !opening_inputs.is_empty() {
                orchestrator::deliver_opening_inputs(state, &session.id)?;
            } else if !action.prompt.trim().is_empty() {
                orchestrator::post_client_message(state, &session.id, &action.prompt)?;
            }
            Ok(ControllerActionResult::SessionOpened {
                session_id: session.id,
                deduped: false,
            })
        }
        SessionCreateOutcome::Superseded { old_id } => {
            orchestrator::handle_superseded_session(state, &old_id);
            if !opening_inputs.is_empty() {
                orchestrator::deliver_opening_inputs(state, &session.id)?;
            } else if !action.prompt.trim().is_empty() {
                orchestrator::post_client_message(state, &session.id, &action.prompt)?;
            }
            Ok(ControllerActionResult::Superseded {
                session_id: session.id,
                old_id,
            })
        }
    }
}

fn opening_inputs(action: &OpenSessionAction) -> Vec<OpeningInput> {
    if action.recipient_inputs.is_empty() {
        return Vec::new();
    }
    action
        .roster
        .iter()
        .map(|recipient| OpeningInput {
            recipient: recipient.clone(),
            content: action
                .recipient_inputs
                .get(recipient)
                .unwrap_or(&action.prompt)
                .clone(),
        })
        .collect()
}

fn post_message(
    state: &Arc<AppState>,
    action: PostMessageAction,
) -> ControllerResult<ControllerActionResult> {
    if state.store.session(&action.session_id)?.is_none() {
        return Err(ControllerError::NotFound(format!(
            "unknown session {}",
            action.session_id
        )));
    }

    let msg = orchestrator::post_client_message(state, &action.session_id, &action.content)?;
    Ok(ControllerActionResult::MessagePosted { message_id: msg.id })
}

fn add_roster(
    state: &Arc<AppState>,
    action: AddRosterAction,
) -> ControllerResult<ControllerActionResult> {
    if action.bots.is_empty() {
        return Err(ControllerError::Invalid(
            "add_roster action needs at least one bot".into(),
        ));
    }
    if action.bots.iter().any(|bot| bot.trim().is_empty()) {
        return Err(ControllerError::Invalid(
            "add_roster action contains an empty bot id".into(),
        ));
    }
    if state.store.session(&action.session_id)?.is_none() {
        return Err(ControllerError::NotFound(format!(
            "unknown session {}",
            action.session_id
        )));
    }
    for (recipient, content) in &action.recipient_inputs {
        if !action.bots.iter().any(|bot| bot == recipient) {
            return Err(ControllerError::Invalid(format!(
                "add_roster action recipient_inputs references unrequested bot '{recipient}'"
            )));
        }
        if content.trim().is_empty() {
            return Err(ControllerError::Invalid(format!(
                "add_roster action recipient input for '{recipient}' is empty"
            )));
        }
    }

    let current_roster = state.store.roster(&action.session_id)?;
    let new_bots = action
        .bots
        .iter()
        .filter(|bot| !current_roster.iter().any(|member| member == *bot))
        .collect::<std::collections::BTreeSet<_>>();
    let messages = state.store.messages(&action.session_id)?;
    let has_targeted_client_context = messages.iter().any(|message| {
        message.author_kind == "client" && message.author_id.is_none() && message.audience.is_some()
    });
    let has_broadcast_client_context = messages.iter().any(|message| {
        message.author_kind == "client" && message.author_id.is_none() && message.audience.is_none()
    });
    if has_targeted_client_context && !has_broadcast_client_context {
        if let Some(bot) = new_bots
            .iter()
            .find(|bot| !action.recipient_inputs.contains_key(**bot))
        {
            return Err(ControllerError::Invalid(format!(
                "add_roster action needs recipient input for '{bot}' because session client context is audience-scoped"
            )));
        }
    }

    let opening_inputs = action
        .recipient_inputs
        .iter()
        .map(|(recipient, content)| OpeningInput {
            recipient: recipient.clone(),
            content: content.clone(),
        })
        .collect::<Vec<_>>();
    match orchestrator::add_to_roster_batch(
        state,
        &action.session_id,
        &action.bots,
        &opening_inputs,
    ) {
        Ok(orchestrator::BatchAdmission::Added {
            added,
            already_members,
        }) => Ok(ControllerActionResult::RosterAdded {
            session_id: action.session_id,
            added,
            already_members,
        }),
        Ok(orchestrator::BatchAdmission::Rejected("terminal session")) => Err(
            ControllerError::Gone(format!("session {} is terminal", action.session_id)),
        ),
        Ok(orchestrator::BatchAdmission::Rejected(reason)) => {
            Err(ControllerError::Invalid(reason.into()))
        }
        Err(error) => Err(ControllerError::Internal(error)),
    }
}

fn close_session(
    state: &Arc<AppState>,
    action: CloseSessionAction,
    policy: ControlledClosePolicy,
) -> ControllerResult<ControllerActionResult> {
    if policy == ControlledClosePolicy::Deny {
        return Err(ControllerError::Forbidden(
            "close_session action is not authorized by controller policy".into(),
        ));
    }
    if state.store.session(&action.session_id)?.is_none() {
        return Err(ControllerError::NotFound(format!(
            "unknown session {}",
            action.session_id
        )));
    }
    let reason = action.reason.trim();
    if reason.is_empty() || reason.len() > 256 {
        return Err(ControllerError::Invalid(
            "close_session action reason must contain 1..=256 bytes".into(),
        ));
    }
    let closed = orchestrator::close_session_by_controller(state, &action.session_id, reason)
        .map_err(ControllerError::Internal)?;
    Ok(ControllerActionResult::SessionClosed {
        session_id: action.session_id,
        closed,
    })
}

fn emit_status(
    state: &Arc<AppState>,
    action: EmitStatusAction,
) -> ControllerResult<ControllerActionResult> {
    if action.target.trim().is_empty() {
        return Err(ControllerError::Invalid(
            "emit_status action target must not be empty".into(),
        ));
    }
    if action.body.trim().is_empty() {
        return Err(ControllerError::Invalid(
            "emit_status action body must not be empty".into(),
        ));
    }
    if state.store.session(&action.session_id)?.is_none() {
        return Err(ControllerError::NotFound(format!(
            "unknown session {}",
            action.session_id
        )));
    }
    let status = orchestrator::emit_controller_status(
        state,
        &action.session_id,
        &action.target,
        &action.body,
    )
    .map_err(ControllerError::Internal)?;
    Ok(ControllerActionResult::StatusEmitted {
        session_id: action.session_id,
        status_id: status.id,
    })
}

fn validate_open_session(
    state: &Arc<AppState>,
    action: &OpenSessionAction,
) -> ControllerResult<()> {
    if action.roster.is_empty() {
        return Err(ControllerError::Invalid(
            "open_session action needs a non-empty roster".into(),
        ));
    }
    if action.quorum_n < 0 {
        return Err(ControllerError::Invalid(
            "open_session action quorum_n must be non-negative".into(),
        ));
    }
    if coordinator::lookup(&action.mode).is_none() {
        return Err(ControllerError::Invalid(format!(
            "open_session action has unknown mode '{}'",
            action.mode
        )));
    }
    if let Some(chair) = action.chair_bot.as_deref() {
        if !action.roster.iter().any(|bot| bot == chair) {
            return Err(ControllerError::Invalid(
                "open_session action chair_bot must be in roster".into(),
            ));
        }
    }
    let reviewer_capacity = action
        .roster
        .iter()
        .filter(|bot| Some(bot.as_str()) != action.chair_bot.as_deref())
        .count();
    if action.quorum_n as usize > reviewer_capacity {
        return Err(ControllerError::Invalid(format!(
            "open_session action quorum_n ({}) exceeds reviewer count ({})",
            action.quorum_n, reviewer_capacity
        )));
    }
    let mut unique_roster = std::collections::BTreeSet::new();
    for bot in &action.roster {
        if !unique_roster.insert(bot) {
            return Err(ControllerError::Invalid(format!(
                "open_session action roster contains duplicate bot '{bot}'"
            )));
        }
        if state.store.bot(bot)?.is_none() {
            return Err(ControllerError::Invalid(format!(
                "open_session action references unknown bot '{bot}'"
            )));
        }
    }
    for (recipient, content) in &action.recipient_inputs {
        if !action.roster.iter().any(|bot| bot == recipient) {
            return Err(ControllerError::Invalid(format!(
                "open_session action recipient_inputs references non-roster bot '{recipient}'"
            )));
        }
        if content.trim().is_empty() {
            return Err(ControllerError::Invalid(format!(
                "open_session action recipient input for '{recipient}' is empty"
            )));
        }
    }
    if !action.recipient_inputs.is_empty()
        && action.prompt.trim().is_empty()
        && action
            .roster
            .iter()
            .any(|bot| !action.recipient_inputs.contains_key(bot))
    {
        return Err(ControllerError::Invalid(
            "open_session action needs prompt fallback or recipient input for every roster bot"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::store::{SessionState, SqliteStore, Store};

    fn state_with_bots() -> Arc<AppState> {
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
        AppState::new(store)
    }

    fn review_action() -> OpenSessionAction {
        OpenSessionAction {
            title: "council".into(),
            trigger_ref: Some("github:pr/o/r#1".into()),
            trigger_fingerprint: Some("sha:head1".into()),
            roster: vec!["chair".into(), "rev1".into()],
            quorum_n: 1,
            chair_bot: Some("chair".into()),
            mode: "council".into(),
            prompt: "review o/r#1".into(),
            recipient_inputs: Default::default(),
        }
    }

    fn pending_frames_for_session(store: &dyn Store, bot_id: &str, session_id: &str) -> usize {
        store
            .pending_outbox(bot_id)
            .unwrap()
            .into_iter()
            .filter(|(_, frame)| {
                serde_json::from_str::<serde_json::Value>(frame)
                    .ok()
                    .and_then(|value| value["channel"]["id"].as_str().map(str::to_string))
                    .as_deref()
                    == Some(session_id)
            })
            .count()
    }

    #[test]
    fn open_session_action_creates_session_and_posts_prompt() {
        let state = state_with_bots();
        let result = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened {
            session_id,
            deduped,
        } = result
        else {
            panic!("open should create a session");
        };
        assert!(!deduped);

        let session = state.store.session(&session_id).unwrap().unwrap();
        assert_eq!(
            SessionState::from_db_str(&session.state),
            SessionState::Deliberating
        );
        assert_eq!(session.trigger_ref.as_deref(), Some("github:pr/o/r#1"));
        assert_eq!(
            state.store.roster(&session_id).unwrap(),
            vec!["chair", "rev1"]
        );
        let messages = state.store.messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "review o/r#1");
    }

    #[test]
    fn serialized_protocol_action_executes_through_in_process_adapter() {
        let state = state_with_bots();
        let envelope = controller_protocol::ActionEnvelope {
            version: controller_protocol::CURRENT_VERSION,
            action_id: "act_adapter_001".into(),
            action: ControllerAction::OpenSession(review_action()),
        };
        let encoded = serde_json::to_vec(&envelope).unwrap();
        let decoded: controller_protocol::ActionEnvelope =
            serde_json::from_slice(&encoded).unwrap();

        let result = execute(&state, decoded.action).unwrap();
        assert!(matches!(
            result,
            ControllerActionResult::SessionOpened { deduped: false, .. }
        ));
    }

    #[test]
    fn post_message_action_posts_client_message() {
        let state = state_with_bots();
        let mut action = review_action();
        action.trigger_ref = Some("github:pr/o/r#post".into());
        action.prompt.clear();
        let result = execute(&state, ControllerAction::OpenSession(action)).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = result else {
            panic!("open should create a session");
        };

        let result = execute(
            &state,
            ControllerAction::PostMessage(PostMessageAction {
                session_id: session_id.clone(),
                content: "follow-up".into(),
            }),
        )
        .unwrap();
        let ControllerActionResult::MessagePosted { message_id } = result else {
            panic!("post should create a message");
        };

        let messages = state.store.messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, message_id);
        assert_eq!(messages[0].author_kind, "client");
        assert_eq!(messages[0].content, "follow-up");
    }

    #[test]
    fn post_message_action_rejects_unknown_session() {
        let state = state_with_bots();
        let err = execute(
            &state,
            ControllerAction::PostMessage(PostMessageAction {
                session_id: "ses_missing".into(),
                content: "hello".into(),
            }),
        )
        .unwrap_err();

        assert!(
            matches!(err, ControllerError::NotFound(message) if message == "unknown session ses_missing")
        );
    }

    fn terminal_session(state: &Arc<AppState>, mode: &str, terminal: SessionState) -> String {
        let (chair, roster, quorum_n) = if mode == "solo" {
            (Some("chair"), vec!["chair".to_string()], 0)
        } else {
            (
                Some("chair"),
                vec!["chair".to_string(), "rev1".to_string()],
                1,
            )
        };
        let session = state
            .store
            .create_session("terminal", None, quorum_n, chair, &roster, mode)
            .unwrap();
        state.store.set_state(&session.id, terminal).unwrap();
        session.id
    }

    #[test]
    fn post_message_reopen_matrix_is_coordinator_owned() {
        let state = state_with_bots();
        for mode in [
            "solo",
            "council",
            "pipeline",
            "review_council",
            "triage_council",
        ] {
            let session_id = terminal_session(&state, mode, SessionState::Closed);
            let before = state.store.messages(&session_id).unwrap().len();
            let result = execute(
                &state,
                ControllerAction::PostMessage(PostMessageAction {
                    session_id: session_id.clone(),
                    content: format!("follow-up for {mode}"),
                }),
            );

            if mode == "solo" {
                assert!(matches!(
                    result.unwrap(),
                    ControllerActionResult::MessagePosted { .. }
                ));
                assert_eq!(
                    SessionState::from_db_str(
                        &state.store.session(&session_id).unwrap().unwrap().state
                    ),
                    SessionState::Deliberating
                );
                assert_eq!(state.store.messages(&session_id).unwrap().len(), before + 1);
            } else {
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        err,
                        ControllerError::Gone(message)
                            if message.contains(&session_id)
                                && message.contains(mode)
                                && message.contains("does not reopen on client messages")
                    ),
                    "mode {mode} should refuse terminal follow-up with Gone",
                );
                assert_eq!(
                    SessionState::from_db_str(
                        &state.store.session(&session_id).unwrap().unwrap().state
                    ),
                    SessionState::Closed
                );
                assert_eq!(state.store.messages(&session_id).unwrap().len(), before);
            }
        }
    }

    #[test]
    fn post_message_aborted_reopen_policy_matches_closed() {
        let state = state_with_bots();

        let solo_id = terminal_session(&state, "solo", SessionState::Aborted);
        execute(
            &state,
            ControllerAction::PostMessage(PostMessageAction {
                session_id: solo_id.clone(),
                content: "resume solo".into(),
            }),
        )
        .unwrap();
        assert_eq!(
            SessionState::from_db_str(&state.store.session(&solo_id).unwrap().unwrap().state),
            SessionState::Deliberating
        );
        assert_eq!(state.store.messages(&solo_id).unwrap().len(), 1);

        let council_id = terminal_session(&state, "council", SessionState::Aborted);
        let before = state.store.messages(&council_id).unwrap().len();
        let err = execute(
            &state,
            ControllerAction::PostMessage(PostMessageAction {
                session_id: council_id.clone(),
                content: "resume council".into(),
            }),
        )
        .unwrap_err();
        assert!(matches!(err, ControllerError::Gone(_)));
        assert_eq!(
            SessionState::from_db_str(&state.store.session(&council_id).unwrap().unwrap().state),
            SessionState::Aborted
        );
        assert_eq!(state.store.messages(&council_id).unwrap().len(), before);
    }

    #[test]
    fn open_session_with_prompt_and_follow_up_both_land_in_session() {
        let state = state_with_bots();
        let mut action = review_action();
        action.trigger_ref = Some("github:pr/o/r#prompt-followup".into());
        action.prompt = "initial prompt".into();
        let result = execute(&state, ControllerAction::OpenSession(action)).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = result else {
            panic!("open should create a session");
        };

        let result = execute(
            &state,
            ControllerAction::PostMessage(PostMessageAction {
                session_id: session_id.clone(),
                content: "follow-up".into(),
            }),
        )
        .unwrap();
        assert!(matches!(
            result,
            ControllerActionResult::MessagePosted { .. }
        ));

        let messages = state.store.messages(&session_id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].author_kind, "client");
        assert_eq!(messages[0].content, "initial prompt");
        assert_eq!(messages[1].author_kind, "client");
        assert_eq!(messages[1].content, "follow-up");
    }

    #[test]
    fn open_session_action_dedupes_active_trigger_ref() {
        let state = state_with_bots();
        let first = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = first else {
            panic!("first open should create a session");
        };

        let second = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        assert_eq!(
            second,
            ControllerActionResult::SessionOpened {
                session_id: session_id.clone(),
                deduped: true,
            }
        );
        assert_eq!(state.store.messages(&session_id).unwrap().len(), 1);
    }

    #[test]
    fn open_session_reports_superseded_with_old_id() {
        let state = state_with_bots();
        let first = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened {
            session_id: old_id,
            deduped,
        } = first
        else {
            panic!("first open should create a session");
        };
        assert!(!deduped);

        let mut next = review_action();
        next.trigger_fingerprint = Some("sha:head2".into());
        next.prompt = "review new head".into();
        let second = execute(&state, ControllerAction::OpenSession(next)).unwrap();

        let ControllerActionResult::Superseded {
            session_id: new_id,
            old_id: reported_old,
        } = second
        else {
            panic!("new fingerprint should supersede");
        };
        assert_eq!(reported_old, old_id);
        assert_ne!(new_id, old_id);
        assert_eq!(
            SessionState::from_db_str(&state.store.session(&old_id).unwrap().unwrap().state),
            SessionState::Closed
        );
        assert_eq!(state.store.messages(&new_id).unwrap().len(), 1);
        assert_eq!(state.store.messages(&old_id).unwrap().len(), 1);
    }

    #[test]
    fn open_session_supersede_cleans_up_without_caller_cooperation() {
        let state = state_with_bots();
        let first = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened {
            session_id: old_id, ..
        } = first
        else {
            panic!("first open should create a session");
        };
        state
            .store
            .cache_installation_token(&old_id, "reviewer", "ghs_old", i64::MAX)
            .unwrap();
        assert!(pending_frames_for_session(state.store.as_ref(), "rev1", &old_id) > 0);

        let mut north = state.north_tx.subscribe();
        let mut next = review_action();
        next.trigger_fingerprint = Some("sha:head2".into());
        next.prompt = "review new head".into();
        let second = execute(&state, ControllerAction::OpenSession(next)).unwrap();
        let ControllerActionResult::Superseded {
            session_id: new_id,
            old_id: reported_old,
        } = second
        else {
            panic!("new fingerprint should supersede");
        };

        assert_eq!(reported_old, old_id);
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev1", &old_id),
            0
        );
        assert!(
            pending_frames_for_session(state.store.as_ref(), "rev1", &new_id) > 0,
            "new session prompt should still be queued"
        );
        assert!(state
            .store
            .session_installation_tokens(&old_id)
            .unwrap()
            .is_empty());

        let events = std::iter::from_fn(|| north.try_recv().ok())
            .map(|raw| serde_json::from_str::<serde_json::Value>(&raw).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            event["type"] == "state"
                && event["session_id"] == old_id
                && event["payload"]
                    == serde_json::json!({
                        "state": "closed",
                        "reason": "superseded"
                    })
        }));
    }

    #[test]
    fn open_session_action_without_trigger_ref_does_not_dedupe() {
        let state = state_with_bots();
        let mut action = review_action();
        action.trigger_ref = None;

        let first = execute(&state, ControllerAction::OpenSession(action.clone())).unwrap();
        let second = execute(&state, ControllerAction::OpenSession(action)).unwrap();
        let ControllerActionResult::SessionOpened {
            session_id: first_id,
            deduped: first_deduped,
        } = first
        else {
            panic!("first open should create a session");
        };
        let ControllerActionResult::SessionOpened {
            session_id: second_id,
            deduped: second_deduped,
        } = second
        else {
            panic!("second open should create a session");
        };

        assert!(!first_deduped);
        assert!(!second_deduped);
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn open_session_action_skips_blank_prompt() {
        let state = state_with_bots();
        let mut action = review_action();
        action.trigger_ref = Some("github:pr/o/r#blank".into());
        action.prompt = " \n ".into();

        let result = execute(&state, ControllerAction::OpenSession(action)).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = result else {
            panic!("blank prompt open should create a session");
        };

        let session = state.store.session(&session_id).unwrap().unwrap();
        assert_eq!(
            SessionState::from_db_str(&session.state),
            SessionState::Open
        );
        assert!(state.store.messages(&session_id).unwrap().is_empty());
    }

    #[test]
    fn open_session_action_validates_roster_and_chair_identity() {
        let state = state_with_bots();

        let mut empty = review_action();
        empty.roster.clear();
        assert!(execute(&state, ControllerAction::OpenSession(empty))
            .unwrap_err()
            .to_string()
            .contains("non-empty roster"));

        let mut missing = review_action();
        missing.roster.push("ghost".into());
        assert!(execute(&state, ControllerAction::OpenSession(missing))
            .unwrap_err()
            .to_string()
            .contains("unknown bot"));

        let mut duplicate = review_action();
        duplicate.roster.push("rev1".into());
        assert!(execute(&state, ControllerAction::OpenSession(duplicate))
            .unwrap_err()
            .to_string()
            .contains("duplicate bot 'rev1'"));

        let mut bad_chair = review_action();
        bad_chair.chair_bot = Some("ghost".into());
        assert!(execute(&state, ControllerAction::OpenSession(bad_chair))
            .unwrap_err()
            .to_string()
            .contains("chair_bot must be in roster"));
    }

    #[test]
    fn open_session_action_validates_quorum_and_mode() {
        let state = state_with_bots();

        let mut negative = review_action();
        negative.quorum_n = -1;
        assert!(execute(&state, ControllerAction::OpenSession(negative))
            .unwrap_err()
            .to_string()
            .contains("non-negative"));

        let mut too_high = review_action();
        too_high.quorum_n = 2;
        assert!(execute(&state, ControllerAction::OpenSession(too_high))
            .unwrap_err()
            .to_string()
            .contains("exceeds reviewer count"));

        let mut bad_mode = review_action();
        bad_mode.mode = "mystery".into();
        assert!(execute(&state, ControllerAction::OpenSession(bad_mode))
            .unwrap_err()
            .to_string()
            .contains("unknown mode"));
    }

    #[test]
    fn open_session_persists_and_delivers_recipient_inputs_atomically() {
        let state = state_with_bots();
        let mut action = review_action();
        action
            .recipient_inputs
            .insert("rev1".into(), "Inspect the failure path.".into());

        let result = execute(&state, ControllerAction::OpenSession(action)).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = result else {
            panic!("recipient inputs should open a session");
        };
        let session = state.store.session(&session_id).unwrap().unwrap();
        assert_eq!(
            SessionState::from_db_str(&session.state),
            SessionState::Deliberating
        );
        let messages = state.store.messages(&session_id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].audience.as_deref(), Some("chair"));
        assert_eq!(messages[0].content, "review o/r#1");
        assert_eq!(messages[1].audience.as_deref(), Some("rev1"));
        assert_eq!(messages[1].content, "Inspect the failure path.");
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "chair", &session_id),
            1
        );
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev1", &session_id),
            1
        );
    }

    #[test]
    fn open_session_rejects_invalid_recipient_inputs_without_partial_state() {
        let state = state_with_bots();
        let mut action = review_action();
        action
            .recipient_inputs
            .insert("ghost".into(), "Do hidden work.".into());

        let error = execute(&state, ControllerAction::OpenSession(action)).unwrap_err();
        assert!(error.to_string().contains("non-roster bot 'ghost'"));
        assert!(state
            .store
            .list_sessions(None, None, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn add_roster_action_is_batch_atomic_and_backfills_new_members() {
        let state = state_with_bots();
        let opened = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = opened else {
            panic!("session should open");
        };

        let error = execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into(), "ghost".into()],
                recipient_inputs: Default::default(),
            }),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown bot"));
        assert_eq!(
            state.store.roster(&session_id).unwrap(),
            vec!["chair", "rev1"]
        );

        let result = execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
        )
        .unwrap();
        assert_eq!(
            result,
            ControllerActionResult::RosterAdded {
                session_id: session_id.clone(),
                added: vec!["rev2".into()],
                already_members: vec![],
            }
        );
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev2", &session_id),
            1,
            "new member receives opening history through normal backfill"
        );

        let replay = execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
        )
        .unwrap();
        assert_eq!(
            replay,
            ControllerActionResult::RosterAdded {
                session_id,
                added: vec![],
                already_members: vec!["rev2".into()],
            }
        );
    }

    #[test]
    fn add_roster_requires_and_delivers_input_for_audience_scoped_sessions() {
        let state = state_with_bots();
        let mut open = review_action();
        open.recipient_inputs = std::collections::BTreeMap::from([
            ("chair".into(), "Chair the targeted review.".into()),
            ("rev1".into(), "Inspect the targeted change.".into()),
        ]);
        let opened = execute(&state, ControllerAction::OpenSession(open)).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = opened else {
            panic!("targeted session should open");
        };

        let error = execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("needs recipient input for 'rev2'"));
        assert_eq!(
            state.store.roster(&session_id).unwrap(),
            vec!["chair", "rev1"],
            "a context-free late join must not mutate membership"
        );

        let result = execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: std::collections::BTreeMap::from([(
                    "rev2".into(),
                    "Inspect the newly assigned subsystem.".into(),
                )]),
            }),
        )
        .unwrap();
        assert!(matches!(
            result,
            ControllerActionResult::RosterAdded { ref added, .. } if added == &["rev2"]
        ));
        let messages = state.store.messages(&session_id).unwrap();
        let late_input = messages
            .iter()
            .find(|message| message.audience.as_deref() == Some("rev2"))
            .expect("late join input is persisted");
        assert_eq!(late_input.content, "Inspect the newly assigned subsystem.");
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev2", &session_id),
            1,
            "the new member receives its targeted prompt through backfill"
        );
    }

    #[test]
    fn reserved_actions_report_unknown_and_terminal_sessions() {
        let state = state_with_bots();
        let unknown = "ses_missing".to_string();
        for action in [
            ControllerAction::AddRoster(AddRosterAction {
                session_id: unknown.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
            ControllerAction::EmitStatus(EmitStatusAction {
                session_id: unknown.clone(),
                target: "operator".into(),
                body: "Waiting.".into(),
            }),
        ] {
            assert!(matches!(
                execute(&state, action).unwrap_err(),
                ControllerError::NotFound(message) if message == "unknown session ses_missing"
            ));
        }
        let missing_close = execute_with_close_policy(
            &state,
            ControllerAction::CloseSession(CloseSessionAction {
                session_id: unknown,
                reason: "cancelled".into(),
            }),
            ControlledClosePolicy::Allow,
        )
        .unwrap_err();
        assert!(matches!(missing_close, ControllerError::NotFound(_)));

        let session_id = terminal_session(&state, "council", SessionState::Closed);
        let error = execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id,
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
        )
        .unwrap_err();
        assert!(matches!(error, ControllerError::Gone(_)));
    }

    #[test]
    fn controlled_close_requires_policy_and_runs_terminal_cleanup_once() {
        let state = state_with_bots();
        let opened = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = opened else {
            panic!("session should open");
        };
        let action = CloseSessionAction {
            session_id: session_id.clone(),
            reason: "controller_cancelled".into(),
        };

        let denied = execute_with_close_policy(
            &state,
            ControllerAction::CloseSession(action.clone()),
            ControlledClosePolicy::Deny,
        )
        .unwrap_err();
        assert!(matches!(denied, ControllerError::Forbidden(_)));
        assert_ne!(
            SessionState::from_db_str(&state.store.session(&session_id).unwrap().unwrap().state),
            SessionState::Closed
        );

        let closed = execute_with_close_policy(
            &state,
            ControllerAction::CloseSession(action.clone()),
            ControlledClosePolicy::Allow,
        )
        .unwrap();
        assert_eq!(
            closed,
            ControllerActionResult::SessionClosed {
                session_id: session_id.clone(),
                closed: true,
            }
        );
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev1", &session_id),
            0
        );
        let replay = execute_with_close_policy(
            &state,
            ControllerAction::CloseSession(action),
            ControlledClosePolicy::Allow,
        )
        .unwrap();
        assert_eq!(
            replay,
            ControllerActionResult::SessionClosed {
                session_id,
                closed: false,
            }
        );
    }

    #[test]
    fn emit_status_persists_audit_without_delivering_to_bots() {
        let state = state_with_bots();
        let mut open = review_action();
        open.prompt.clear();
        let opened = execute(&state, ControllerAction::OpenSession(open)).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = opened else {
            panic!("session should open");
        };
        let mut north = state.north_tx.subscribe();

        let result = execute(
            &state,
            ControllerAction::EmitStatus(EmitStatusAction {
                session_id: session_id.clone(),
                target: "operator".into(),
                body: "Waiting for evidence.".into(),
            }),
        )
        .unwrap();
        let ControllerActionResult::StatusEmitted { status_id, .. } = result else {
            panic!("status should be emitted");
        };
        let messages = state.store.messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, status_id);
        assert_eq!(messages[0].author_kind, "status");
        assert_eq!(messages[0].audience.as_deref(), Some("operator"));
        assert_eq!(messages[0].content, "Waiting for evidence.");
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev1", &session_id),
            0
        );
        execute(
            &state,
            ControllerAction::AddRoster(AddRosterAction {
                session_id: session_id.clone(),
                bots: vec!["rev2".into()],
                recipient_inputs: Default::default(),
            }),
        )
        .unwrap();
        assert_eq!(
            pending_frames_for_session(state.store.as_ref(), "rev2", &session_id),
            0,
            "operator status must not enter late-joiner conversation context"
        );
        let event: serde_json::Value = serde_json::from_str(&north.try_recv().unwrap()).unwrap();
        assert_eq!(event["type"], "controller_status");
        assert_eq!(event["payload"]["target"], "operator");
    }

    #[test]
    fn validate_accepts_every_dispatchable_mode() {
        let state = state_with_bots();

        for mode in [
            "council",
            "review_council",
            "triage_council",
            "solo",
            "pipeline",
        ] {
            assert!(
                coordinator::lookup(mode).is_some(),
                "mode {mode} should dispatch"
            );
            let mut action = review_action();
            action.trigger_ref = Some(format!("test:{mode}"));
            action.mode = mode.into();
            assert!(
                validate_open_session(&state, &action).is_ok(),
                "mode {mode} should validate"
            );
        }

        let mut action = review_action();
        action.mode = "mystery".into();
        assert!(coordinator::lookup(&action.mode).is_none());
        assert!(validate_open_session(&state, &action)
            .unwrap_err()
            .to_string()
            .contains("unknown mode"));
    }
}
