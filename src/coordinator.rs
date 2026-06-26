//! Coordination policy (the pluggable lifecycle seam). The orchestrator owns the
//! *mechanism* (fanout, state transitions, delivery, emitting events); a
//! `Coordinator` owns the *policy* — what a done-signal means, when to relay,
//! when to converge, what closes the session. See `docs/coordinators.md`.
//!
//! The orchestrator runs the mechanism, then asks the Coordinator (via `on_done`)
//! what `Action`s to take, and executes them — keeping the CAS guards so a single
//! call can safely emit both a transition and a close, each firing only from its
//! required prior state. v1 ships `QuorumCouncil`; a second mode is a new impl
//! selected in `for_session`, the only seam that changes.

use crate::session::{quorum_reached, DONE_EMOJI};
use crate::store::SessionState;

/// Read-only view a Coordinator decides from (pure → unit-testable).
pub trait Ctx {
    fn roster(&self) -> &[String];
    fn chair(&self) -> Option<&str>;
    fn quorum_n(&self) -> i64;
    /// Distinct bot ids that posted `emoji`.
    fn reactors(&self, emoji: &str) -> Vec<String>;
    /// `bot`'s last *settled* (non-stub) message content, if any.
    fn latest_settled(&self, bot: &str) -> Option<String>;
    fn state(&self) -> SessionState;
}

/// What the orchestrator should do. `Transition`/`Close` are guarded CAS (fire
/// only from `from`); a `Prompt` immediately after a failed `Transition` is
/// suppressed (so the synthesizer is prompted once, on the entering call only).
pub enum Action {
    /// Deliver `from`'s settled final to `to` (skipped if `from` has none).
    Relay { from: String, to: String },
    /// Deliver a system message to `to`.
    Prompt { to: String, content: String },
    /// CAS `from`→`to`; emits `state` on success.
    Transition { from: SessionState, to: SessionState },
    /// CAS `from`→Closed; emits `verdict` + `state:closed` on success.
    Close { from: SessionState, verdict: String },
}

pub trait Coordinator: Send + Sync {
    fn kind(&self) -> &'static str;
    /// A settled done-signal (🆗 add) arrived from `bot`. Return actions.
    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action>;
    /// Roster members *prompted to act* on the opening trigger (i.e. @mentioned).
    /// Non-starters still receive the trigger as context. Default: the whole
    /// roster (council/solo fan-out). `Pipeline` starts only stage 0.
    fn starters(&self, roster: &[String]) -> Vec<String> {
        roster.to_vec()
    }
}

/// v1 lifecycle: reviewers (roster minus chair) signal done; once `quorum_n` of
/// them have, the chair synthesizes and the chair's own done closes the session.
/// Behaviour-identical port of the previously-inline orchestrator flow.
pub struct QuorumCouncil;

impl Coordinator for QuorumCouncil {
    fn kind(&self) -> &'static str {
        "quorum_council"
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        let mut actions = vec![];
        let chair = cx.chair();

        // 1. relay a reviewer's settled final to the chair (was share_final_with_chair)
        if Some(bot) != chair {
            if let Some(c) = chair {
                actions.push(Action::Relay { from: bot.to_string(), to: c.to_string() });
            }
        }

        // 2. quorum reached → enter Quorum + prompt the chair (was maybe_quorum).
        //    The Transition CAS + Prompt-after-failed-Transition suppression make
        //    this fire exactly once, on the call that actually transitions.
        if quorum_reached(cx.roster(), chair, &cx.reactors(DONE_EMOJI), cx.quorum_n()) {
            actions.push(Action::Transition {
                from: SessionState::Deliberating,
                to: SessionState::Quorum,
            });
            if let Some(c) = chair {
                actions.push(Action::Prompt {
                    to: c.to_string(),
                    content: "Quorum reached. Chair, please render the verdict.".to_string(),
                });
            }
        }

        // 3. the chair's own done in Quorum closes with its final (was maybe_close_verdict)
        if Some(bot) == chair {
            actions.push(Action::Close {
                from: SessionState::Quorum,
                verdict: cx.latest_settled(bot).unwrap_or_default(),
            });
        }

        actions
    }
}

/// Single-bot lifecycle: the lone bot's own done closes the session directly.
/// A 1-bot "council" has zero reviewers (roster minus chair = ∅), so quorum is
/// never reachable and `QuorumCouncil` would hang — `Solo` is that fix.
pub struct Solo;

impl Coordinator for Solo {
    fn kind(&self) -> &'static str {
        "solo"
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        vec![Action::Close {
            from: SessionState::Deliberating,
            verdict: cx.latest_settled(bot).unwrap_or_default(),
        }]
    }
}

/// Sequential handoff stage0→stage1→…→stageN. Only stage 0 starts; each bot's
/// done relays its output to the next stage and prompts it; the last stage's
/// done closes with its final as the verdict. Stage order = roster order. Proves
/// the seam generalizes beyond parallel fan-in (no quorum, no chair).
pub struct Pipeline;

impl Coordinator for Pipeline {
    fn kind(&self) -> &'static str {
        "pipeline"
    }

    fn starters(&self, roster: &[String]) -> Vec<String> {
        roster.first().cloned().into_iter().collect()
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        let roster = cx.roster();
        let Some(i) = roster.iter().position(|b| b == bot) else {
            return vec![]; // not a member (shouldn't happen — roster-gated)
        };
        match roster.get(i + 1) {
            // hand off: relay this stage's output to the next, then prompt it
            Some(next) => vec![
                Action::Relay { from: bot.to_string(), to: next.to_string() },
                Action::Prompt {
                    to: next.to_string(),
                    content: "Your turn — continue the review, building on the prior stage's output above."
                        .to_string(),
                },
            ],
            // last stage's done closes the session with its final
            None => vec![Action::Close {
                from: SessionState::Deliberating,
                verdict: cx.latest_settled(bot).unwrap_or_default(),
            }],
        }
    }
}

/// Pick the coordinator for a session's `mode`. The only place a mode is mapped
/// to a policy; a new mode is a new arm + impl, nothing else changes.
pub fn for_session(mode: &str) -> Box<dyn Coordinator> {
    match mode {
        "solo" => Box::new(Solo),
        "pipeline" => Box::new(Pipeline),
        _ => Box::new(QuorumCouncil),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCtx {
        roster: Vec<String>,
        final_msg: Option<String>,
    }
    impl Ctx for FakeCtx {
        fn roster(&self) -> &[String] {
            &self.roster
        }
        fn chair(&self) -> Option<&str> {
            self.roster.first().map(String::as_str)
        }
        fn quorum_n(&self) -> i64 {
            0
        }
        fn reactors(&self, _: &str) -> Vec<String> {
            vec![]
        }
        fn latest_settled(&self, _: &str) -> Option<String> {
            self.final_msg.clone()
        }
        fn state(&self) -> SessionState {
            SessionState::Deliberating
        }
    }

    #[test]
    fn for_session_dispatches_mode() {
        assert_eq!(for_session("solo").kind(), "solo");
        assert_eq!(for_session("council").kind(), "quorum_council");
        assert_eq!(for_session("anything-else").kind(), "quorum_council");
    }

    #[test]
    fn solo_lone_bot_closes_directly_with_its_final() {
        let cx = FakeCtx { roster: vec!["solo".into()], final_msg: Some("verdict".into()) };
        let actions = Solo.on_done(&cx, "solo");
        assert_eq!(actions.len(), 1, "solo emits exactly one Close, no quorum gate");
        match &actions[0] {
            Action::Close { from, verdict } => {
                assert_eq!(*from, SessionState::Deliberating);
                assert_eq!(verdict, "verdict");
            }
            _ => panic!("expected Close"),
        }
    }

    #[test]
    fn pipeline_starts_only_stage_zero() {
        let roster = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(Pipeline.starters(&roster), vec!["a".to_string()]);
        // council/solo (default) start everyone
        assert_eq!(QuorumCouncil.starters(&roster), roster);
    }

    #[test]
    fn pipeline_hands_off_then_closes_on_last() {
        let cx = FakeCtx {
            roster: vec!["a".into(), "b".into(), "c".into()],
            final_msg: Some("c's report".into()),
        };
        // middle stage hands to the next
        let mid = Pipeline.on_done(&cx, "a");
        assert!(
            matches!(mid.as_slice(),
                [Action::Relay { from, to }, Action::Prompt { to: pt, .. }]
                if from == "a" && to == "b" && pt == "b"),
            "stage a should relay→b and prompt b",
        );
        // last stage closes with its final, no further handoff
        let last = Pipeline.on_done(&cx, "c");
        assert!(
            matches!(last.as_slice(),
                [Action::Close { from: SessionState::Deliberating, verdict }]
                if verdict == "c's report"),
            "last stage should close with its report",
        );
    }
}
