use controller_protocol::OpenSessionAction;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const REVIEW_OPT_IN_LABEL: &str = "oab-review";
const DEFAULT_PRESET: &str = "lite";
const REVIEW_TRIGGER_TEMPLATE: &str = include_str!("../templates/pr-review-trigger-pointer.tmpl");
const ASK_TRIGGER_TEMPLATE: &str = include_str!("../templates/pr-ask-trigger-pointer.tmpl");
const REREVIEW_CONTEXT_START: &str = "===== RE-REVIEW CONTEXT =====";
const REREVIEW_CONTEXT_END: &str = "===== END RE-REVIEW CONTEXT =====";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub repository: String,
    pub pr_number: u64,
    pub reason: String,
    pub trigger_fingerprint: Option<String>,
    pub preset: Option<String>,
    pub question: Option<String>,
    pub comment_id: Option<u64>,
    pub review_notes: Option<String>,
    pub review_from_scratch: bool,
    pub author_trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPlan {
    pub source_delivery_id: String,
    pub repository: String,
    pub pr_number: u64,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub title: String,
    pub trigger_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_fingerprint: Option<String>,
    pub roster: Vec<String>,
    pub quorum_n: i64,
    pub chair_bot: String,
    pub mode: String,
    pub prompt: String,
    pub recipient_inputs: BTreeMap<String, String>,
    pub dedupe: DedupeProjection,
    pub terminal_projection: TerminalProjection,
    pub proposed_writes: Vec<ProposedWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedWrite {
    pub target: String,
    pub operation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupeProjection {
    pub object_key: String,
    pub fingerprint: Option<String>,
    pub same_active_fingerprint: String,
    pub different_active_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalProjection {
    pub result_identity: String,
    pub verdict_source: String,
    pub findings_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParitySnapshot {
    pub repository: String,
    pub pr_number: u64,
    pub reason: String,
    pub preset: Option<String>,
    pub open_session: OpenSessionAction,
    pub dedupe: DedupeProjection,
    pub terminal_projection: TerminalProjection,
    pub proposed_writes: Vec<ProposedWrite>,
}

struct MentionReview {
    notes: String,
    from_scratch: bool,
}

impl SessionPlan {
    pub fn open_session_action(&self) -> OpenSessionAction {
        OpenSessionAction {
            title: self.title.clone(),
            trigger_ref: Some(self.trigger_ref.clone()),
            trigger_fingerprint: self.trigger_fingerprint.clone(),
            roster: self.roster.clone(),
            quorum_n: self.quorum_n,
            chair_bot: Some(self.chair_bot.clone()),
            mode: self.mode.clone(),
            prompt: self.prompt.clone(),
            recipient_inputs: self.recipient_inputs.clone(),
        }
    }

    pub fn parity_snapshot(&self) -> ParitySnapshot {
        ParitySnapshot {
            repository: self.repository.clone(),
            pr_number: self.pr_number,
            reason: self.reason.clone(),
            preset: self.preset.clone(),
            open_session: self.open_session_action(),
            dedupe: self.dedupe.clone(),
            terminal_projection: self.terminal_projection.clone(),
            proposed_writes: self.proposed_writes.clone(),
        }
    }
}

pub fn parse_trigger(event: &str, body: &Value, bot_handle: Option<&str>) -> Option<Trigger> {
    match event {
        "pull_request" => parse_pull_request(body),
        "issue_comment" => parse_issue_comment(body, bot_handle),
        _ => None,
    }
}

pub fn build_plan(
    delivery_id: &str,
    trigger: Trigger,
    configured_roster: &[String],
    configured_preset: Option<&str>,
    review_mode: &str,
) -> SessionPlan {
    let is_ask = trigger.reason == "ask";
    let (roster, quorum_n, prompt, resolved_preset) = if is_ask {
        let roster: Vec<String> = configured_roster.first().cloned().into_iter().collect();
        (roster, 0, render_ask_prompt(&trigger), None)
    } else {
        let preset = pick_preset(trigger.preset.as_deref(), configured_preset);
        let angles = preset_angles(&preset).expect("pick_preset returns a valid preset");
        let (roster, quorum, assignment) = assign_angles(configured_roster, &angles);
        (
            roster,
            quorum,
            render_review_prompt(&trigger, &assignment),
            Some(preset),
        )
    };
    let chair_bot = roster.first().cloned().unwrap_or_else(|| "chair".into());
    let trigger_ref = if is_ask {
        match trigger.comment_id {
            Some(comment_id) => format!(
                "github:ask/{repo}#{number}@{comment_id}",
                repo = trigger.repository,
                number = trigger.pr_number,
            ),
            None => format!(
                "github:ask/{repo}#{number}",
                repo = trigger.repository,
                number = trigger.pr_number,
            ),
        }
    } else {
        format!(
            "github:pr/{repo}#{number}",
            repo = trigger.repository,
            number = trigger.pr_number
        )
    };
    let trigger_fingerprint = if is_ask {
        Some(trigger_ref.clone())
    } else {
        trigger.trigger_fingerprint
    };
    let mode = if is_ask || roster.len() <= 1 {
        "solo"
    } else {
        "review_council"
    };
    let dedupe = DedupeProjection {
        object_key: trigger_ref.clone(),
        fingerprint: trigger_fingerprint.clone(),
        same_active_fingerprint: "dedupe".into(),
        different_active_fingerprint: if is_ask {
            "distinct_comment_key".into()
        } else {
            "supersede".into()
        },
    };
    let terminal_projection = TerminalProjection {
        result_identity: trigger_ref.clone(),
        verdict_source: "terminal_final_message".into(),
        findings_source: (!is_ask).then(|| "openab_findings_block".into()),
    };
    let proposed_writes = proposed_writes(is_ask, review_mode);

    SessionPlan {
        source_delivery_id: delivery_id.to_string(),
        repository: trigger.repository,
        pr_number: trigger.pr_number,
        reason: trigger.reason.clone(),
        preset: resolved_preset,
        title: if is_ask {
            "ask".into()
        } else {
            "council".into()
        },
        trigger_ref,
        trigger_fingerprint,
        roster,
        quorum_n,
        chair_bot,
        mode: mode.into(),
        prompt,
        recipient_inputs: BTreeMap::new(),
        dedupe,
        terminal_projection,
        proposed_writes,
    }
}

fn proposed_writes(is_ask: bool, review_mode: &str) -> Vec<ProposedWrite> {
    if is_ask {
        return vec![ProposedWrite {
            target: "github_pull_request".into(),
            operation: "create_followup_comment".into(),
        }];
    }
    let mut writes = vec![
        ProposedWrite {
            target: "github_pull_request".into(),
            operation: "create_then_update_round_comment".into(),
        },
        ProposedWrite {
            target: "github_commit".into(),
            operation: "set_openab_council_status".into(),
        },
    ];
    if review_mode != "status" {
        writes.push(ProposedWrite {
            target: "github_pull_request_review".into(),
            operation: if review_mode == "enforce" {
                "submit_approve_or_request_changes"
            } else {
                "submit_approve_on_approve"
            }
            .into(),
        });
    }
    writes
}

fn parse_pull_request(body: &Value) -> Option<Trigger> {
    let action = body["action"].as_str()?;
    if !matches!(
        action,
        "opened" | "reopened" | "ready_for_review" | "synchronize"
    ) {
        return None;
    }
    let pr = &body["pull_request"];
    if action != "ready_for_review" && pr["draft"].as_bool() == Some(true) {
        return None;
    }
    let association = pr["author_association"].as_str().unwrap_or_default();
    let labels = &pr["labels"];
    Some(Trigger {
        repository: body["repository"]["full_name"].as_str()?.to_string(),
        pr_number: pr["number"].as_u64()?,
        reason: "auto".into(),
        trigger_fingerprint: pr["head"]["sha"].as_str().map(|sha| format!("sha:{sha}")),
        preset: preset_from_labels(labels),
        question: None,
        comment_id: None,
        review_notes: None,
        review_from_scratch: false,
        author_trusted: can_command(association) || has_label(labels, REVIEW_OPT_IN_LABEL),
    })
}

fn parse_issue_comment(body: &Value, bot_handle: Option<&str>) -> Option<Trigger> {
    if body["action"].as_str()? != "created"
        || body["issue"]["pull_request"]["url"].as_str().is_none()
        || body["comment"]["user"]["type"].as_str() == Some("Bot")
    {
        return None;
    }
    let comment = body["comment"]["body"].as_str().unwrap_or_default();
    let trimmed = comment.trim();
    let (reason, question, review_notes, review_from_scratch) =
        if starts_with_command(trimmed, "/review") {
            ("/review", None, None, false)
        } else if let Some(review) = parse_mention_review(trimmed, bot_handle) {
            ("/review", None, Some(review.notes), review.from_scratch)
        } else if let Some(question) = parse_ask(trimmed, bot_handle) {
            ("ask", Some(question), None, false)
        } else {
            return None;
        };
    let comment_id = body["comment"]["id"].as_u64()?;
    Some(Trigger {
        repository: body["repository"]["full_name"].as_str()?.to_string(),
        pr_number: body["issue"]["number"].as_u64()?,
        reason: reason.into(),
        trigger_fingerprint: Some(format!("cmd:{comment_id}")),
        preset: preset_from_labels(&body["issue"]["labels"]),
        question,
        comment_id: Some(comment_id),
        review_notes,
        review_from_scratch,
        author_trusted: can_command(
            body["comment"]["author_association"]
                .as_str()
                .unwrap_or_default(),
        ),
    })
}

fn render_review_prompt(trigger: &Trigger, angle_assignment: &str) -> String {
    let mut prompt = REVIEW_TRIGGER_TEMPLATE
        .replace("{{REPO}}", &trigger.repository)
        .replace("{{NUM}}", &trigger.pr_number.to_string())
        .replace("{{TITLE}}", "")
        .replace("{{ANGLE_ASSIGNMENT}}", angle_assignment);
    if trigger.review_notes.is_some() || trigger.review_from_scratch {
        prompt.push_str(&format!("\n\n{REREVIEW_CONTEXT_START}\n"));
        if trigger.review_from_scratch {
            prompt.push_str("Mode: full review from scratch\n");
        }
        if let Some(notes) = trigger.review_notes.as_deref() {
            prompt.push_str("Author fix notes:\n");
            prompt.push_str(notes);
            if !notes.ends_with('\n') {
                prompt.push('\n');
            }
        }
        prompt.push_str(REREVIEW_CONTEXT_END);
    }
    prompt
}

fn render_ask_prompt(trigger: &Trigger) -> String {
    ASK_TRIGGER_TEMPLATE
        .replace("{{REPO}}", &trigger.repository)
        .replace("{{NUM}}", &trigger.pr_number.to_string())
        .replace(
            "{{QUESTION}}",
            trigger.question.as_deref().unwrap_or_default(),
        )
}

fn pick_preset(label_preset: Option<&str>, configured_preset: Option<&str>) -> String {
    [label_preset, configured_preset]
        .into_iter()
        .flatten()
        .find(|preset| preset_angles(preset).is_some())
        .unwrap_or(DEFAULT_PRESET)
        .to_string()
}

fn preset_angles(preset: &str) -> Option<Vec<&'static str>> {
    match preset {
        "lite" => Some(vec!["correctness"]),
        "quick" => Some(vec!["correctness", "security", "integration"]),
        "standard" => Some(vec![
            "correctness",
            "architecture",
            "security",
            "testing",
            "docs",
        ]),
        "full" => Some(vec![
            "correctness",
            "architecture",
            "security",
            "testing",
            "docs",
            "performance",
            "spec",
        ]),
        _ => None,
    }
}

fn assign_angles(roster: &[String], angles: &[&str]) -> (Vec<String>, i64, String) {
    let Some(chair) = roster.first() else {
        return (Vec::new(), 0, String::new());
    };
    let reviewers = &roster[1..];
    if reviewers.is_empty() {
        return (vec![chair.clone()], 0, String::new());
    }
    let participating: Vec<String> = if angles.len() <= reviewers.len() {
        reviewers[..angles.len()].to_vec()
    } else {
        reviewers.to_vec()
    };
    let mut assigned = vec![Vec::new(); participating.len()];
    for (index, angle) in angles.iter().enumerate() {
        assigned[index % participating.len()].push(*angle);
    }
    let lines = participating
        .iter()
        .zip(&assigned)
        .map(|(reviewer, angles)| format!("- {reviewer} → {}", angles.join(", ")))
        .collect::<Vec<_>>();
    let assignment = format!("Review focus assignment:\n{}", lines.join("\n"));
    let mut effective = vec![chair.clone()];
    effective.extend(participating);
    let quorum = (effective.len() as i64 - 1).max(0);
    (effective, quorum, assignment)
}

fn can_command(association: &str) -> bool {
    matches!(association, "OWNER" | "MEMBER" | "COLLABORATOR")
}

fn has_label(labels: &Value, expected: &str) -> bool {
    labels.as_array().is_some_and(|labels| {
        labels
            .iter()
            .any(|label| label["name"].as_str() == Some(expected))
    })
}

fn preset_from_labels(labels: &Value) -> Option<String> {
    labels.as_array()?.iter().find_map(|label| {
        label["name"]
            .as_str()?
            .strip_prefix("review:")
            .map(str::trim)
            .filter(|preset| !preset.is_empty())
            .map(str::to_string)
    })
}

fn starts_with_command(comment: &str, command: &str) -> bool {
    comment
        .strip_prefix(command)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

fn leading_mention<'a>(comment: &'a str, handle: Option<&str>) -> Option<&'a str> {
    let handle = handle?;
    let rest = comment.strip_prefix('@')?;
    let name = rest.get(..handle.len())?;
    if !name.eq_ignore_ascii_case(handle) {
        return None;
    }
    let tail = &rest[handle.len()..];
    if tail.starts_with(|character: char| character.is_alphanumeric() || character == '-') {
        return None;
    }
    Some(tail.trim_start_matches("[bot]").trim_start())
}

fn parse_mention_review(comment: &str, handle: Option<&str>) -> Option<MentionReview> {
    let rest = leading_mention(comment, handle)?;
    if let Some(rest) = strip_ci_word(rest, "full") {
        return Some(MentionReview {
            notes: strip_ci_word(rest, "review")?.to_string(),
            from_scratch: true,
        });
    }
    Some(MentionReview {
        notes: strip_ci_word(rest, "review")?.to_string(),
        from_scratch: false,
    })
}

fn strip_ci_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let prefix = text.get(..word.len())?;
    if !prefix.eq_ignore_ascii_case(word) {
        return None;
    }
    let rest = &text[word.len()..];
    (rest.is_empty() || rest.starts_with(char::is_whitespace))
        .then(|| rest.trim_start_matches(char::is_whitespace))
}

fn parse_ask(comment: &str, handle: Option<&str>) -> Option<String> {
    if let Some(rest) = comment.strip_prefix("/ask") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some(rest.trim().to_string());
        }
    }
    let rest = leading_mention(comment, handle)?;
    if strip_ci_word(rest, "review").is_some() || strip_ci_word(rest, "full").is_some() {
        return None;
    }
    let question = rest.trim();
    (!question.is_empty()).then(|| question.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let body = match name {
            "opened" => include_str!("../../../tests/fixtures/github/pull_request_opened.json"),
            "ready" => {
                include_str!("../../../tests/fixtures/github/pull_request_ready_for_review.json")
            }
            "draft" => {
                include_str!("../../../tests/fixtures/github/pull_request_draft_opened.json")
            }
            "review" => include_str!("../../../tests/fixtures/github/issue_comment_review.json"),
            "ask" => include_str!("../../../tests/fixtures/github/issue_comment_ask.json"),
            "mention" => {
                include_str!("../../../tests/fixtures/github/issue_comment_mention_review.json")
            }
            _ => panic!("unknown fixture"),
        };
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn replays_existing_trigger_fixtures() {
        let opened = parse_trigger("pull_request", &fixture("opened"), None).unwrap();
        assert_eq!(opened.trigger_fingerprint.as_deref(), Some("sha:abc123"));
        assert_eq!(opened.preset.as_deref(), Some("full"));
        assert!(opened.author_trusted);
        assert!(parse_trigger("pull_request", &fixture("ready"), None).is_some());
        assert!(parse_trigger("pull_request", &fixture("draft"), None).is_none());

        let review = parse_trigger("issue_comment", &fixture("review"), None).unwrap();
        assert_eq!(review.reason, "/review");
        assert_eq!(review.preset.as_deref(), Some("quick"));
        let ask = parse_trigger("issue_comment", &fixture("ask"), None).unwrap();
        assert_eq!(ask.question.as_deref(), Some("why is this a blocker?"));
        let mention = parse_trigger(
            "issue_comment",
            &fixture("mention"),
            Some("fixture-council"),
        )
        .unwrap();
        assert!(mention
            .review_notes
            .as_deref()
            .unwrap()
            .contains("regression test"));
    }

    #[test]
    fn opened_plan_matches_embedded_action_and_only_proposes_writes() {
        let trigger = parse_trigger("pull_request", &fixture("opened"), None).unwrap();
        let plan = build_plan(
            "delivery-1",
            trigger,
            &["chair".into(), "rev1".into(), "rev2".into()],
            None,
            "approve",
        );
        assert_eq!(plan.trigger_ref, "github:pr/example/repo#42");
        assert_eq!(plan.quorum_n, 2);
        assert_eq!(
            plan.dedupe,
            DedupeProjection {
                object_key: "github:pr/example/repo#42".into(),
                fingerprint: Some("sha:abc123".into()),
                same_active_fingerprint: "dedupe".into(),
                different_active_fingerprint: "supersede".into(),
            }
        );
        assert_eq!(
            plan.terminal_projection,
            TerminalProjection {
                result_identity: "github:pr/example/repo#42".into(),
                verdict_source: "terminal_final_message".into(),
                findings_source: Some("openab_findings_block".into()),
            }
        );
        assert_eq!(
            plan.proposed_writes,
            vec![
                ProposedWrite {
                    target: "github_pull_request".into(),
                    operation: "create_then_update_round_comment".into(),
                },
                ProposedWrite {
                    target: "github_commit".into(),
                    operation: "set_openab_council_status".into(),
                },
                ProposedWrite {
                    target: "github_pull_request_review".into(),
                    operation: "submit_approve_on_approve".into(),
                },
            ]
        );

        let actual = serde_json::to_value(plan.open_session_action()).unwrap();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/github/pull_request_opened.plan.json"
        ))
        .unwrap();
        assert_eq!(
            actual, expected,
            "controller plan must match embedded bytes"
        );
    }

    #[test]
    fn controller_templates_are_byte_identical_to_embedded_templates() {
        assert_eq!(
            REVIEW_TRIGGER_TEMPLATE,
            include_str!("../../../scripts/pr-review-trigger-pointer.tmpl")
        );
        assert_eq!(
            ASK_TRIGGER_TEMPLATE,
            include_str!("../../../scripts/pr-ask-trigger-pointer.tmpl")
        );
    }

    #[test]
    fn bare_or_prefix_colliding_mentions_do_not_create_empty_asks() {
        assert_eq!(parse_ask("@fixture-council", Some("fixture-council")), None);
        assert_eq!(
            parse_ask("@fixture-council-admin help", Some("fixture-council")),
            None
        );
        assert_eq!(
            parse_ask("@fixture-council help", Some("fixture-council")),
            Some("help".into())
        );
    }

    #[test]
    fn ask_and_review_modes_pin_dedupe_projection_and_write_ownership() {
        let roster = ["chair".into(), "rev1".into(), "rev2".into()];
        let ask = build_plan(
            "delivery-ask",
            parse_trigger("issue_comment", &fixture("ask"), None).unwrap(),
            &roster,
            None,
            "approve",
        );
        assert_eq!(ask.trigger_ref, "github:ask/example/repo#42@7002");
        assert_eq!(ask.trigger_fingerprint, Some(ask.trigger_ref.clone()));
        assert_eq!(ask.quorum_n, 0);
        assert_eq!(ask.mode, "solo");
        assert_eq!(
            ask.dedupe.different_active_fingerprint,
            "distinct_comment_key"
        );
        assert_eq!(ask.terminal_projection.findings_source, None);
        assert_eq!(
            ask.proposed_writes,
            vec![ProposedWrite {
                target: "github_pull_request".into(),
                operation: "create_followup_comment".into(),
            }]
        );

        let trigger = parse_trigger("pull_request", &fixture("opened"), None).unwrap();
        let status = build_plan("delivery-status", trigger.clone(), &roster, None, "status");
        assert_eq!(status.proposed_writes.len(), 2);
        assert!(status
            .proposed_writes
            .iter()
            .all(|write| write.target != "github_pull_request_review"));

        let enforce = build_plan("delivery-enforce", trigger, &roster, None, "enforce");
        assert_eq!(
            enforce.proposed_writes.last().unwrap().operation,
            "submit_approve_or_request_changes"
        );
    }
}
