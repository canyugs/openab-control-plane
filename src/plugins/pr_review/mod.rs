pub mod council;
pub mod findings;
pub mod tasks;
pub mod verdict;
pub mod webhook;

use crate::coordinator::{Coordinator, Ctx, QuorumCouncil, StructuredVerdict};

/// Immutable PR-review policy assembled once at the process boundary.
///
/// The plugin consumes this value and never reaches into process environment
/// state itself. That keeps webhook parsing, admission, and prompt rendering
/// deterministic and makes the review plugin extractable from the OCP binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReviewConfig {
    pub bot_handle: Option<String>,
    pub allowed_repos: Vec<String>,
    pub review_round_budget: usize,
    pub review_hourly_cap: usize,
    pub council_preset: Option<String>,
    pub council_roster: Vec<String>,
    pub review_mode: ReviewMode,
    pub plane_status_notice: bool,
}

impl Default for PrReviewConfig {
    fn default() -> Self {
        Self {
            bot_handle: None,
            allowed_repos: Vec::new(),
            review_round_budget: 10,
            review_hourly_cap: 3,
            council_preset: None,
            council_roster: vec!["chair".into(), "rev1".into(), "rev2".into()],
            review_mode: ReviewMode::Approve,
            plane_status_notice: false,
        }
    }
}

impl PrReviewConfig {
    /// Build from an explicit key/value source. The composition root owns the
    /// actual environment lookup; the plugin only owns normalization/defaults.
    pub(crate) fn from_values(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let mut config = Self::default();
        config.bot_handle = lookup("OABCP_BOT_HANDLE")
            .and_then(|raw| normalize_bot_handle(&raw));
        config.allowed_repos = csv_value(lookup("OABCP_ALLOWED_REPOS")).unwrap_or_default();
        config.review_round_budget =
            usize_value(lookup("OABCP_REVIEW_ROUND_BUDGET"), 10);
        config.review_hourly_cap = usize_value(lookup("OABCP_REVIEW_HOURLY_CAP"), 3);
        config.council_roster = csv_value(lookup("OABCP_COUNCIL_ROSTER"))
            .filter(|roster| !roster.is_empty())
            .unwrap_or_else(|| config.council_roster.clone());
        config.council_preset = lookup("OABCP_COUNCIL_PRESET")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .and_then(|value| {
                if matches!(value.as_str(), "lite" | "quick" | "standard" | "full") {
                    Some(value)
                } else {
                    tracing::warn!(preset = %value, "unknown OABCP_COUNCIL_PRESET (want lite|quick|standard|full); using default");
                    None
                }
            });
        config.review_mode = lookup("OABCP_COUNCIL_REVIEW_MODE")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .and_then(|value| {
                ReviewMode::parse(&value).or_else(|| {
                    tracing::warn!(mode = %value, "unknown OABCP_COUNCIL_REVIEW_MODE (want status|approve|enforce); using approve");
                    None
                })
            })
            .unwrap_or(ReviewMode::Approve);
        config.plane_status_notice = matches!(
            lookup("OABCP_PLANE_STATUS_NOTICE").as_deref(),
            Some("1") | Some("true")
        );
        config
    }

    pub fn repo_allowed(&self, repo: &str) -> bool {
        self.allowed_repos.is_empty() || self.allowed_repos.iter().any(|allowed| allowed == repo)
    }
}

fn csv_value(value: Option<String>) -> Option<Vec<String>> {
    value.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn usize_value(value: Option<String>, default: usize) -> usize {
    value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn normalize_bot_handle(raw: &str) -> Option<String> {
    let handle = raw.trim().trim_start_matches('@').trim();
    if handle.is_empty() {
        None
    } else {
        Some(handle.to_string())
    }
}

/// How the chair turns a verdict into a GitHub PR review (ADR 013 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    Status,
    Approve,
    Enforce,
}

impl ReviewMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "status" => Some(Self::Status),
            "approve" => Some(Self::Approve),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }
}

/// PR-review council lifecycle: same quorum/close policy as `QuorumCouncil`, but
/// the chair is also prompted on the opening trigger so it can create the
/// in-progress PR comment before reviewers finish.
pub struct ReviewCouncil {
    config: PrReviewConfig,
}

impl ReviewCouncil {
    pub fn new(config: PrReviewConfig) -> Self {
        Self { config }
    }
}

impl Default for ReviewCouncil {
    fn default() -> Self {
        Self::new(PrReviewConfig::default())
    }
}

impl Coordinator for ReviewCouncil {
    fn kind(&self) -> &'static str {
        "review_council"
    }

    fn starters(&self, roster: &[String], _chair: Option<&str>) -> Vec<String> {
        roster.to_vec()
    }

    fn recipient_trigger_text(&self, cx: &dyn Ctx, recipient: &str, text: &str) -> String {
        tasks::review_recipient_trigger_text(cx.chair(), recipient, text, &self.config)
    }

    fn reaction_counts_as_done(&self, cx: &dyn Ctx, bot: &str) -> bool {
        // Prompt-driven chairs often acknowledge the system quorum prompt with
        // an automatic 🆗 reaction. Review chair completion is the explicit text
        // [done] after the synthesized PR verdict and side effects.
        cx.chair() != Some(bot)
    }

    fn on_done(&self, cx: &dyn Ctx, bot: &str) -> Vec<crate::coordinator::Action> {
        QuorumCouncil.on_done(cx, bot)
    }

    fn on_roster_change(&self, cx: &dyn Ctx) -> Vec<crate::coordinator::Action> {
        QuorumCouncil.on_roster_change(cx)
    }

    fn structured_verdict(&self, cx: &dyn Ctx, verdict_text: &str) -> Option<StructuredVerdict> {
        QuorumCouncil.structured_verdict(cx, verdict_text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::{Action, Ctx};
    use crate::store::SessionState;
    use crate::store::Store as _;
    use std::collections::HashMap;

    #[test]
    fn explicit_config_source_normalizes_every_review_policy_value() {
        let values = HashMap::from([
            ("OABCP_BOT_HANDLE", " @nellen "),
            ("OABCP_ALLOWED_REPOS", "nuphos/core, nuphos/ops"),
            ("OABCP_REVIEW_ROUND_BUDGET", "7"),
            ("OABCP_REVIEW_HOURLY_CAP", "2"),
            ("OABCP_COUNCIL_PRESET", "standard"),
            ("OABCP_COUNCIL_ROSTER", "chair, security, tests"),
            ("OABCP_COUNCIL_REVIEW_MODE", "enforce"),
            ("OABCP_PLANE_STATUS_NOTICE", "true"),
        ]);
        let config = PrReviewConfig::from_values(|name| values.get(name).map(|v| v.to_string()));

        assert_eq!(config.bot_handle.as_deref(), Some("nellen"));
        assert_eq!(config.allowed_repos, vec!["nuphos/core", "nuphos/ops"]);
        assert_eq!(config.review_round_budget, 7);
        assert_eq!(config.review_hourly_cap, 2);
        assert_eq!(config.council_preset.as_deref(), Some("standard"));
        assert_eq!(config.council_roster, vec!["chair", "security", "tests"]);
        assert_eq!(config.review_mode, ReviewMode::Enforce);
        assert!(config.plane_status_notice);
    }

    #[test]
    fn invalid_explicit_config_preserves_safe_defaults() {
        let values = HashMap::from([
            ("OABCP_REVIEW_ROUND_BUDGET", "not-a-number"),
            ("OABCP_REVIEW_HOURLY_CAP", ""),
            ("OABCP_COUNCIL_PRESET", "unknown"),
            ("OABCP_COUNCIL_ROSTER", " , "),
            ("OABCP_COUNCIL_REVIEW_MODE", "unknown"),
            ("OABCP_PLANE_STATUS_NOTICE", "yes"),
        ]);
        let config = PrReviewConfig::from_values(|name| values.get(name).map(|v| v.to_string()));

        assert_eq!(config, PrReviewConfig::default());
    }

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
    fn review_council_recipient_trigger_text_rewrites_only_review_triggers() {
        let cx = ctx(&["chair", "rev"], None);
        let council = ReviewCouncil::default();
        let trigger = "PR Review Council — canyugs/openab-control-plane #17 \"\"\n\nReview focus assignment:\n- rev → security";

        let chair_text = council.recipient_trigger_text(&cx, "chair", trigger);
        assert!(chair_text.contains("Task: manage the GitHub PR status comment"));
        assert!(chair_text.contains("gh pr comment 17 --repo canyugs/openab-control-plane"));

        let reviewer_text = council.recipient_trigger_text(&cx, "rev", trigger);
        assert!(reviewer_text.contains("Task: review GitHub PR canyugs/openab-control-plane #17"));
        assert!(reviewer_text.contains("focus: security"));

        let non_review = "plain forum trigger";
        assert_eq!(
            council.recipient_trigger_text(&cx, "chair", non_review),
            non_review
        );
    }

    #[test]
    fn review_council_reaction_policy_pins_chair_ack_gate() {
        let cx = ctx(&["chair", "rev"], None);
        let council = ReviewCouncil::default();
        assert!(!council.reaction_counts_as_done(&cx, "chair"));
        assert!(council.reaction_counts_as_done(&cx, "rev"));
    }

    #[test]
    fn review_council_structured_verdict_policy_parses_trailer() {
        let cx = ctx(&["chair", "rev"], None);
        let text = "final\n[[verdict:request_changes r=1 y=2 g=3]] [done]";
        let council = ReviewCouncil::default();
        let verdict = council
            .structured_verdict(&cx, text)
            .expect("review_council should parse trailer");
        assert_eq!(
            verdict,
            StructuredVerdict {
                decision: "request_changes".into(),
                red: Some(1),
                yellow: Some(2),
                green: Some(3),
            },
        );
        assert!(council
            .structured_verdict(&cx, "plain final [done]")
            .is_none());
    }

    #[test]
    fn review_council_starters_include_chair_for_status_comment() {
        let roster = vec!["chair".into(), "rev0".into(), "rev1".into()];
        assert_eq!(ReviewCouncil::default().starters(&roster, Some("chair")), roster);
    }

    #[test]
    fn review_council_delegates_quorum_close_policy() {
        let cx = FakeCtx {
            quorum_n: 1,
            reactors: vec!["rev0".into()],
            state: SessionState::Quorum,
            ..ctx(&["chair", "rev0"], Some("VERDICT"))
        };
        let from = ReviewCouncil::default()
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
    fn review_chair_quorum_reaction_does_not_close_without_text_done() {
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
                "review_council",
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
                "Quorum reached. Chair, synthesize.",
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
            "chair ack reaction to the quorum prompt must not close the review",
        );

        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(
                &session.id,
                "LGTM ✅ — final verdict\n[done]",
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

    #[test]
    fn structured_verdict_policy_covers_all_coordinators() {
        let cx = ctx(&["chair", "rev"], None);
        let text = "final\n[[verdict:request_changes r=1 y=2 g=3]] [done]";

        let verdict = crate::coordinator::QuorumCouncil
            .structured_verdict(&cx, text)
            .expect("quorum_council should parse trailer");
        assert_eq!(
            verdict,
            crate::coordinator::StructuredVerdict {
                decision: "request_changes".into(),
                red: Some(1),
                yellow: Some(2),
                green: Some(3),
            },
            "quorum_council maps trailer fields"
        );
        assert!(
            crate::coordinator::QuorumCouncil
                .structured_verdict(&cx, "plain final [done]")
                .is_none(),
            "quorum_council returns None without a trailer"
        );

        for (name, coord) in [
            (
                "solo",
                Box::new(crate::coordinator::Solo) as Box<dyn crate::coordinator::Coordinator>,
            ),
            ("pipeline", Box::new(crate::coordinator::Pipeline)),
        ] {
            assert!(
                coord.structured_verdict(&cx, text).is_none(),
                "{name} must not parse even valid-looking trailers"
            );
        }
    }

    #[test]
    fn recipient_trigger_text_default_is_verbatim_passthrough() {
        let cx = ctx(&["chair", "rev"], None);
        let trigger = "PR Review Council — canyugs/openab-control-plane #17 \"\"\n\nReview focus assignment:\n- rev → security";
        let cases: Vec<(&str, Box<dyn crate::coordinator::Coordinator>)> = vec![
            (
                "quorum_council",
                Box::new(crate::coordinator::QuorumCouncil),
            ),
            ("solo", Box::new(crate::coordinator::Solo)),
            ("pipeline", Box::new(crate::coordinator::Pipeline)),
        ];

        for (name, coord) in cases {
            assert_eq!(
                coord.recipient_trigger_text(&cx, "rev", trigger),
                trigger,
                "{name} must deliver triggers verbatim"
            );
        }
    }
}
