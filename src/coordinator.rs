//! Coordination policy (the pluggable lifecycle seam). The orchestrator owns the
//! *mechanism* (client-trigger fanout, state transitions, delivery, emitting
//! events); a `Coordinator` owns the *policy* — what a done-signal means, when
//! to relay, when to converge, what closes the session. See
//! `docs/coordinators.md`.
//!
//! The orchestrator runs the mechanism, then asks the Coordinator (via `on_done`)
//! what `Action`s to take, and executes them — keeping the CAS guards so a single
//! call can safely emit both a transition and a close, each firing only from its
//! required prior state. v1 ships `QuorumCouncil`; a second mode is a new impl
//! selected in `lookup`, the only seam that changes.

use crate::session::quorum_reached;
use crate::store::SessionState;

/// Read-only view a Coordinator decides from (pure → unit-testable).
pub trait Ctx {
    fn roster(&self) -> &[String];
    fn chair(&self) -> Option<&str>;
    fn quorum_n(&self) -> i64;
    /// Distinct bot ids with counted done-votes.
    fn done_voters(&self) -> Vec<String>;
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
    Transition {
        from: SessionState,
        to: SessionState,
    },
    /// CAS `from`→Closed; emits `verdict` + `state:closed` on success.
    Close { from: SessionState, verdict: String },
}

pub trait Coordinator: Send + Sync {
    fn kind(&self) -> &'static str;
    /// A settled done-signal (🆗 add) arrived from `bot`. Return actions.
    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action>;
    /// Roster members *prompted to act* on the opening trigger (i.e. @mentioned).
    /// A9: before the topic exists, non-starters are skipped by the stock OAB
    /// mention gate and the event is dropped, not deferred. The orchestrator
    /// re-delivers the opening trigger in-thread to a non-starter chair once;
    /// other future non-starter trigger delivery should be an explicit
    /// Coordinator method. Default: the whole roster (council/solo fan-out).
    /// `Pipeline` starts only stage 0.
    fn starters(&self, roster: &[String], _chair: Option<&str>) -> Vec<String> {
        roster.to_vec()
    }
    /// Does a 🆗 reaction from `bot` count as its done-signal? Native OAB
    /// contract: yes (set_done → 🆗 closes). Prompt-driven chairs in
    /// review/triage auto-🆗 the quorum prompt — those coordinators return
    /// false for their chair; completion there is the explicit text [done].
    fn reaction_counts_as_done(&self, _cx: &dyn Ctx, _bot: &str) -> bool {
        true
    }
    /// Does a done-signal found in message *text* from `bot` count, given the
    /// full text? Default: yes. TriageCouncil requires the chair's [done] to
    /// ride the TRIAGE report itself.
    fn accepts_text_done(&self, _cx: &dyn Ctx, _bot: &str, _text: &str) -> bool {
        true
    }
    /// The roster changed outside a done-signal (liveness trim/replace). Default:
    /// nothing; quorum modes re-check whether the already-recorded done-count now
    /// meets the (possibly shrunk) quorum.
    fn on_roster_change(&self, _cx: &dyn Ctx) -> Vec<Action> {
        vec![]
    }
}

/// Shared quorum policy: once `quorum_n` reviewers signalled done, enter Quorum
/// and prompt the chair to synthesize. Reached from a done-signal and from a
/// liveness roster trim (a shrunk quorum can make the recorded count sufficient).
/// `prompt` is per-coordinator — a review chair completes GitHub side effects,
/// a triage chair must post the report and nothing else.
fn quorum_actions(cx: &dyn Ctx, prompt: &str) -> Vec<Action> {
    let mut actions = vec![];
    let chair = cx.chair();
    if quorum_reached(cx.roster(), chair, &cx.done_voters(), cx.quorum_n()) {
        actions.push(Action::Transition {
            from: SessionState::Deliberating,
            to: SessionState::Quorum,
        });
        if let Some(c) = chair {
            actions.push(Action::Prompt {
                to: c.to_string(),
                content: prompt.to_string(),
            });
        }
    }
    actions
}

const COUNCIL_QUORUM_PROMPT: &str = "Quorum reached. Chair, synthesize the final verdict, complete any side effect required by the opening trigger, and only then end your final message with [done]. Do not send [done] before the required side effect succeeds.";

/// Review-flavored wording confuses triage chairs into hunting for a PR
/// (dogfood round 6) — tell them exactly and only what to do.
const TRIAGE_QUORUM_PROMPT: &str = "Quorum reached. Chair: post the complete triage report NOW as ONE message — it must start with the word TRIAGE, contain the Likely Cause / Evidence / Suggested Next Actions / Confidence & Gaps sections, and end with [done] on its own final line. Do not run gh or any PR commands; there is no PR and no external side effect. The report itself is your done-signal.";

/// v1 lifecycle: reviewers (roster minus chair) signal done; once `quorum_n` of
/// them have, the chair synthesizes and the chair's own done closes the session.
pub struct QuorumCouncil;

impl Coordinator for QuorumCouncil {
    fn kind(&self) -> &'static str {
        "quorum_council"
    }

    fn on_roster_change(&self, cx: &dyn Ctx) -> Vec<Action> {
        quorum_actions(cx, COUNCIL_QUORUM_PROMPT)
    }

    fn starters(&self, roster: &[String], chair: Option<&str>) -> Vec<String> {
        roster
            .iter()
            .filter(|bot| Some(bot.as_str()) != chair)
            .cloned()
            .collect()
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        council_on_done(cx, bot, COUNCIL_QUORUM_PROMPT)
    }
}

/// Shared quorum-council done-handling; `prompt` is the per-coordinator chair
/// synthesis instruction.
fn council_on_done(cx: &dyn Ctx, bot: &str, prompt: &str) -> Vec<Action> {
    let mut actions = vec![];
    let chair = cx.chair();

    // 1. relay a reviewer's settled final to the chair (was share_final_with_chair)
    if Some(bot) != chair {
        if let Some(c) = chair {
            actions.push(Action::Relay {
                from: bot.to_string(),
                to: c.to_string(),
            });
        }
    }

    // 2. quorum reached → enter Quorum + prompt the chair (was maybe_quorum).
    //    The Transition CAS + Prompt-after-failed-Transition suppression make
    //    this fire exactly once, on the call that actually transitions.
    actions.extend(quorum_actions(cx, prompt));

    // 3. The chair's own done closes only after reviewer quorum. This prevents
    //    an opening-trigger chair response from closing the PR review before
    //    reviewers have contributed or before the chair has posted the PR
    //    comment side-effect. Liveness still comes from the watchdog.
    if Some(bot) == chair && cx.state() == SessionState::Quorum {
        actions.push(Action::Close {
            from: SessionState::Quorum,
            verdict: cx.latest_settled(bot).unwrap_or_default(),
        });
    } else if Some(bot) == chair {
        tracing::debug!(
            bot,
            state = ?cx.state(),
            "chair done ignored before reviewer quorum"
        );
    }

    actions
}

/// ADR 014 triage council: QuorumCouncil lifecycle with a triage-specific chair
/// prompt (no GitHub side effects — the report is the deliverable). The chair's
/// text-done additionally requires the TRIAGE report prefix (orchestrator gate).
pub struct TriageCouncil;

impl Coordinator for TriageCouncil {
    fn kind(&self) -> &'static str {
        "triage_council"
    }

    fn starters(&self, roster: &[String], chair: Option<&str>) -> Vec<String> {
        QuorumCouncil.starters(roster, chair)
    }

    fn reaction_counts_as_done(&self, cx: &dyn Ctx, bot: &str) -> bool {
        // Prompt-driven chairs often acknowledge the system quorum prompt with
        // an automatic 🆗 reaction. That must not close the session before the
        // chair posts the synthesized final; chair completion is explicit text.
        cx.chair() != Some(bot)
    }

    fn accepts_text_done(&self, cx: &dyn Ctx, bot: &str, text: &str) -> bool {
        // Triage chairs habitually append [done] to acknowledgments. In
        // triage_council the report is the chair's done-signal, so the chair's
        // [done] only counts when attached to the mandated report prefix.
        cx.chair() != Some(bot) || text.trim_start().starts_with("TRIAGE")
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        council_on_done(cx, bot, TRIAGE_QUORUM_PROMPT)
    }

    fn on_roster_change(&self, cx: &dyn Ctx) -> Vec<Action> {
        quorum_actions(cx, TRIAGE_QUORUM_PROMPT)
    }
}

/// PR-review council lifecycle: same quorum/close policy as `QuorumCouncil`, but
/// the chair is also prompted on the opening trigger so it can create the
/// in-progress PR comment before reviewers finish.
pub struct ReviewCouncil;

impl Coordinator for ReviewCouncil {
    fn kind(&self) -> &'static str {
        "review_council"
    }

    fn starters(&self, roster: &[String], _chair: Option<&str>) -> Vec<String> {
        roster.to_vec()
    }

    fn reaction_counts_as_done(&self, cx: &dyn Ctx, bot: &str) -> bool {
        // Prompt-driven chairs often acknowledge the system quorum prompt with
        // an automatic 🆗 reaction. Review chair completion is the explicit text
        // [done] after the synthesized PR verdict and side effects.
        cx.chair() != Some(bot)
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<Action> {
        QuorumCouncil.on_done(cx, bot)
    }

    fn on_roster_change(&self, cx: &dyn Ctx) -> Vec<Action> {
        QuorumCouncil.on_roster_change(cx)
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

    fn starters(&self, roster: &[String], _chair: Option<&str>) -> Vec<String> {
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

/// Pick a known coordinator for a session's `mode`. The only place a mode is
/// mapped to a policy; a new mode is a new arm + impl, nothing else changes.
pub fn lookup(mode: &str) -> Option<Box<dyn Coordinator>> {
    match mode {
        "council" => Some(Box::new(QuorumCouncil)),
        "review_council" => Some(Box::new(ReviewCouncil)),
        "triage_council" => Some(Box::new(TriageCouncil)),
        "solo" => Some(Box::new(Solo)),
        "pipeline" => Some(Box::new(Pipeline)),
        _ => None,
    }
}

/// Pick the coordinator for persisted session rows. Unknown legacy rows retain
/// the historical fallback to `QuorumCouncil`; new opens validate through
/// `lookup` first.
pub fn for_session(mode: &str) -> Box<dyn Coordinator> {
    lookup(mode).unwrap_or_else(|| Box::new(QuorumCouncil))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeCtx {
        roster: Vec<String>,
        chair: Option<String>,
        final_msg: Option<String>,
        quorum_n: i64,
        reactors: Vec<String>,
        state: SessionState,
    }
    /// A Deliberating ctx with no done-signals yet (the common starting point).
    fn ctx(roster: &[&str], final_msg: Option<&str>) -> FakeCtx {
        FakeCtx {
            roster: roster.iter().map(|s| s.to_string()).collect(),
            chair: roster.first().map(|s| s.to_string()),
            final_msg: final_msg.map(String::from),
            quorum_n: 0,
            reactors: vec![],
            state: SessionState::Deliberating,
        }
    }
    impl Ctx for FakeCtx {
        fn roster(&self) -> &[String] {
            &self.roster
        }
        fn chair(&self) -> Option<&str> {
            self.chair.as_deref()
        }
        fn quorum_n(&self) -> i64 {
            self.quorum_n
        }
        fn done_voters(&self) -> Vec<String> {
            self.reactors.clone()
        }
        fn latest_settled(&self, _: &str) -> Option<String> {
            self.final_msg.clone()
        }
        fn state(&self) -> SessionState {
            self.state.clone()
        }
    }

    #[test]
    fn for_session_dispatches_mode() {
        assert_eq!(for_session("solo").kind(), "solo");
        assert_eq!(for_session("review_council").kind(), "review_council");
        assert_eq!(for_session("triage_council").kind(), "triage_council");
        assert_eq!(for_session("council").kind(), "quorum_council");
        assert_eq!(for_session("anything-else").kind(), "quorum_council");
    }

    #[test]
    fn lookup_knows_exactly_the_dispatchable_modes() {
        assert_eq!(lookup("council").unwrap().kind(), "quorum_council");
        assert_eq!(lookup("review_council").unwrap().kind(), "review_council");
        assert_eq!(lookup("triage_council").unwrap().kind(), "triage_council");
        assert_eq!(lookup("solo").unwrap().kind(), "solo");
        assert_eq!(lookup("pipeline").unwrap().kind(), "pipeline");
        assert!(lookup("anything-else").is_none());
    }

    #[test]
    fn reaction_done_policy_covers_all_coordinators() {
        let cx = ctx(&["chair", "rev"], None);
        let cases: Vec<(&str, Box<dyn Coordinator>, bool, bool)> = vec![
            ("quorum_council", Box::new(QuorumCouncil), true, true),
            ("triage_council", Box::new(TriageCouncil), false, true),
            ("review_council", Box::new(ReviewCouncil), false, true),
            ("solo", Box::new(Solo), true, true),
            ("pipeline", Box::new(Pipeline), true, true),
        ];

        for (name, coord, chair_counts, non_chair_counts) in cases {
            assert_eq!(
                coord.reaction_counts_as_done(&cx, "chair"),
                chair_counts,
                "{name} chair reaction policy"
            );
            assert_eq!(
                coord.reaction_counts_as_done(&cx, "rev"),
                non_chair_counts,
                "{name} non-chair reaction policy"
            );
        }
    }

    #[test]
    fn text_done_policy_covers_all_coordinators() {
        let cx = ctx(&["chair", "rev"], None);
        let report = "  TRIAGE high — final report\n[done]";
        let ack = "ok then [done]";

        assert!(
            TriageCouncil.accepts_text_done(&cx, "chair", report),
            "triage chair report text counts"
        );
        assert!(
            !TriageCouncil.accepts_text_done(&cx, "chair", ack),
            "triage chair bare ack text does not count"
        );
        assert!(
            TriageCouncil.accepts_text_done(&cx, "rev", ack),
            "triage reviewer text keeps default done semantics"
        );

        let default_cases: Vec<(&str, Box<dyn Coordinator>)> = vec![
            ("quorum_council", Box::new(QuorumCouncil)),
            ("review_council", Box::new(ReviewCouncil)),
            ("solo", Box::new(Solo)),
            ("pipeline", Box::new(Pipeline)),
        ];
        for (name, coord) in default_cases {
            assert!(
                coord.accepts_text_done(&cx, "chair", ack),
                "{name} chair text done keeps default semantics"
            );
            assert!(
                coord.accepts_text_done(&cx, "rev", ack),
                "{name} non-chair text done keeps default semantics"
            );
        }
    }

    #[test]
    fn solo_lone_bot_closes_directly_with_its_final() {
        let cx = ctx(&["solo"], Some("verdict"));
        let actions = Solo.on_done(&cx, "solo");
        assert_eq!(
            actions.len(),
            1,
            "solo emits exactly one Close, no quorum gate"
        );
        match &actions[0] {
            Action::Close { from, verdict } => {
                assert_eq!(*from, SessionState::Deliberating);
                assert_eq!(verdict, "verdict");
            }
            _ => panic!("expected Close"),
        }
    }

    /// The chair may see the opening trigger as context, but it must not be able
    /// to close the council from `Deliberating`. It should wait for reviewer
    /// quorum and the explicit system prompt before writing the PR verdict.
    #[test]
    fn quorum_council_chair_done_does_not_close_before_quorum() {
        let cx = FakeCtx {
            quorum_n: 2,      // both reviewers must signal for a quorum…
            reactors: vec![], // …but none did → quorum unreachable
            state: SessionState::Deliberating,
            ..ctx(&["chair", "rev0", "rev1"], Some("VERDICT"))
        };
        let closes: Vec<_> = QuorumCouncil
            .on_done(&cx, "chair")
            .into_iter()
            .filter(|a| matches!(a, Action::Close { .. }))
            .collect();
        assert!(closes.is_empty(), "chair done before quorum must not close");
    }

    /// The designed path is unchanged: once reviewers reached quorum (state is
    /// `Quorum`), the chair's done still closes from `Quorum`.
    #[test]
    fn quorum_council_chair_done_closes_from_quorum_when_reached() {
        let cx = FakeCtx {
            quorum_n: 1,
            reactors: vec!["rev0".into()],
            state: SessionState::Quorum,
            ..ctx(&["chair", "rev0"], Some("VERDICT"))
        };
        let from = QuorumCouncil
            .on_done(&cx, "chair")
            .into_iter()
            .find_map(|a| match a {
                Action::Close { from, .. } => Some(from),
                _ => None,
            })
            .expect("chair's done emits a Close");
        assert_eq!(from, SessionState::Quorum);
    }

    #[test]
    fn pipeline_starts_only_stage_zero() {
        let roster = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(Pipeline.starters(&roster, None), vec!["a".to_string()]);
        assert_eq!(
            QuorumCouncil.starters(&roster, Some("a")),
            vec!["b".to_string(), "c".to_string()]
        );
        assert_eq!(Solo.starters(&roster, None), roster);
    }

    #[test]
    fn quorum_council_starters_excludes_chair_by_identity_not_position() {
        let roster = vec!["rev0".into(), "chair".into(), "rev1".into()];
        assert_eq!(
            QuorumCouncil.starters(&roster, Some("chair")),
            vec!["rev0".to_string(), "rev1".to_string()]
        );
    }

    #[test]
    fn review_council_starters_include_chair_for_status_comment() {
        let roster = vec!["chair".into(), "rev0".into(), "rev1".into()];
        assert_eq!(ReviewCouncil.starters(&roster, Some("chair")), roster);
    }

    #[test]
    fn pipeline_hands_off_then_closes_on_last() {
        let cx = ctx(&["a", "b", "c"], Some("c's report"));
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
