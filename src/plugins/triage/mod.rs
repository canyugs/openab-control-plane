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
}
