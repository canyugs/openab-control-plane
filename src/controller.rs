//! Controller action interpreter.
//!
//! Bundled controllers and future external controllers both propose declarative
//! actions; this module is the core-owned boundary that validates and executes
//! them. The first slice supports opening a session with an initial prompt.

use crate::orchestrator;
use crate::state::AppState;
use anyhow::{bail, Result};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerAction {
    OpenSession(OpenSessionAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSessionAction {
    pub title: String,
    pub trigger_ref: Option<String>,
    pub roster: Vec<String>,
    pub quorum_n: i64,
    pub chair_bot: Option<String>,
    pub mode: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerActionResult {
    SessionOpened { session_id: String, deduped: bool },
}

pub fn execute(state: &Arc<AppState>, action: ControllerAction) -> Result<ControllerActionResult> {
    match action {
        ControllerAction::OpenSession(action) => open_session(state, action),
    }
}

fn open_session(
    state: &Arc<AppState>,
    action: OpenSessionAction,
) -> Result<ControllerActionResult> {
    validate_open_session(state, &action)?;

    if let Some(trigger_ref) = action.trigger_ref.as_deref() {
        if let Some(existing) = state.store.active_session_for_trigger(trigger_ref)? {
            return Ok(ControllerActionResult::SessionOpened {
                session_id: existing,
                deduped: true,
            });
        }
    }

    let session = state.store.create_session(
        &action.title,
        action.trigger_ref.as_deref(),
        action.quorum_n,
        action.chair_bot.as_deref(),
        &action.roster,
        &action.mode,
    )?;

    if !action.prompt.trim().is_empty() {
        orchestrator::post_client_message(state, &session.id, &action.prompt)?;
    }

    Ok(ControllerActionResult::SessionOpened {
        session_id: session.id,
        deduped: false,
    })
}

fn validate_open_session(state: &Arc<AppState>, action: &OpenSessionAction) -> Result<()> {
    if action.roster.is_empty() {
        bail!("open_session action needs a non-empty roster");
    }
    if action.quorum_n < 0 {
        bail!("open_session action quorum_n must be non-negative");
    }
    if let Some(chair) = action.chair_bot.as_deref() {
        if !action.roster.iter().any(|bot| bot == chair) {
            bail!("open_session action chair_bot must be in roster");
        }
    }
    for bot in &action.roster {
        if state.store.bot(bot)?.is_none() {
            bail!("open_session action references unknown bot '{bot}'");
        }
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
        AppState::new(store)
    }

    fn review_action() -> OpenSessionAction {
        OpenSessionAction {
            title: "council".into(),
            trigger_ref: Some("github:pr/o/r#1".into()),
            roster: vec!["chair".into(), "rev1".into()],
            quorum_n: 1,
            chair_bot: Some("chair".into()),
            mode: "council".into(),
            prompt: "review o/r#1".into(),
        }
    }

    #[test]
    fn open_session_action_creates_session_and_posts_prompt() {
        let state = state_with_bots();
        let result = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened {
            session_id,
            deduped,
        } = result;
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
    fn open_session_action_dedupes_active_trigger_ref() {
        let state = state_with_bots();
        let first = execute(&state, ControllerAction::OpenSession(review_action())).unwrap();
        let ControllerActionResult::SessionOpened { session_id, .. } = first;

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
    fn open_session_action_validates_roster_and_chair_identity() {
        let state = state_with_bots();

        let mut missing = review_action();
        missing.roster.push("ghost".into());
        assert!(execute(&state, ControllerAction::OpenSession(missing))
            .unwrap_err()
            .to_string()
            .contains("unknown bot"));

        let mut bad_chair = review_action();
        bad_chair.chair_bot = Some("ghost".into());
        assert!(execute(&state, ControllerAction::OpenSession(bad_chair))
            .unwrap_err()
            .to_string()
            .contains("chair_bot must be in roster"));
    }
}
