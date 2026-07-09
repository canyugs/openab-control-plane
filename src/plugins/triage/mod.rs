use crate::coordinator::{
    council_on_done, quorum_actions, Action, Coordinator, Ctx, QuorumCouncil,
};

/// Review-flavored wording confuses triage chairs into hunting for a PR
/// (dogfood round 6) — tell them exactly and only what to do.
const TRIAGE_QUORUM_PROMPT: &str = "Quorum reached. Chair: post the complete triage report NOW as ONE message — it must start with the word TRIAGE, contain the Likely Cause / Evidence / Suggested Next Actions / Confidence & Gaps sections, and end with [done] on its own final line. Do not run gh or any PR commands; there is no PR and no external side effect. The report itself is your done-signal.";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::Ctx;
    use crate::store::SessionState;
    use crate::store::Store as _;

    struct FakeCtx {
        session_id: String,
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
            session_id: "ses_fake".into(),
            roster: roster.iter().map(|s| s.to_string()).collect(),
            chair: roster.first().map(|s| s.to_string()),
            final_msg: final_msg.map(String::from),
            quorum_n: 0,
            reactors: vec![],
            state: SessionState::Deliberating,
        }
    }
    impl Ctx for FakeCtx {
        fn session_id(&self) -> &str {
            &self.session_id
        }
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
    fn triage_council_text_done_requires_report_prefix_for_chair() {
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
    }

    #[test]
    fn triage_council_reaction_policy_pins_chair_ack_gate() {
        let cx = ctx(&["chair", "rev"], None);
        assert!(!TriageCouncil.reaction_counts_as_done(&cx, "chair"));
        assert!(TriageCouncil.reaction_counts_as_done(&cx, "rev"));
    }

    #[test]
    fn triage_council_does_not_parse_structured_verdicts() {
        let cx = ctx(&["chair", "rev"], None);
        let text = "final\n[[verdict:request_changes r=1 y=2 g=3]] [done]";
        assert!(TriageCouncil.structured_verdict(&cx, text).is_none());
    }

    #[test]
    fn triage_council_uses_quorum_starters() {
        let roster = vec!["chair".into(), "rev0".into(), "rev1".into()];
        assert_eq!(
            TriageCouncil.starters(&roster, Some("chair")),
            vec!["rev0".to_string(), "rev1".to_string()]
        );
    }

    #[test]
    fn triage_chair_quorum_reaction_does_not_close_without_text_done() {
        // Same footgun as the review_council variant below, hit live by the
        // ADR 014 triage dogfood: a prompt-driven chair auto-🆗s the quorum
        // prompt and the session closed with a "still waiting" verdict.
        // `triage_council` rides QuorumCouncil (for_session default arm) but
        // gets the text-done chair guard; generic `council` keeps native
        // set_done semantics (spike tests pin that contract).
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let rev = store.register_bot("rev", "reviewer", "h2", "t2").unwrap();
        let session = store
            .create_session(
                "t",
                None,
                1,
                Some(&chair.id),
                &[chair.id.clone(), rev.id.clone()],
                "triage_council",
            )
            .unwrap();
        store
            .advance_state(
                &session.id,
                crate::store::SessionState::Open,
                crate::store::SessionState::Quorum,
            )
            .unwrap();
        let quorum_prompt = store
            .add_message(
                &session.id,
                None,
                "system",
                None,
                None,
                "Quorum reached.",
                None,
            )
            .unwrap();

        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::reaction_reply(
                &session.id,
                &quorum_prompt.id,
                crate::session::DONE_EMOJI,
            ),
        )
        .unwrap();
        assert_eq!(
            crate::store::SessionState::from_db_str(
                &store.session(&session.id).unwrap().unwrap().state
            ),
            crate::store::SessionState::Quorum,
            "council chair ack reaction must not close before the text [done]",
        );

        // ack-style [done] without a report must NOT close (dogfood rounds 2/5)
        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(
                &session.id,
                "Acknowledged, standing by.\n[done]",
            ),
        )
        .unwrap();
        assert_eq!(
            crate::store::SessionState::from_db_str(
                &store.session(&session.id).unwrap().unwrap().state
            ),
            crate::store::SessionState::Quorum,
            "chair [done] without a TRIAGE report must not close",
        );

        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(
                &session.id,
                "TRIAGE low — final report\n[done]",
            ),
        )
        .unwrap();
        assert_eq!(
            crate::store::SessionState::from_db_str(
                &store.session(&session.id).unwrap().unwrap().state
            ),
            crate::store::SessionState::Closed,
        );
    }
}
