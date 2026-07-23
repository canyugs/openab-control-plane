use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const REVIEW_OPT_IN_LABEL: &str = "oab-review";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub repository: String,
    pub pr_number: u64,
    pub reason: String,
    pub trigger_fingerprint: Option<String>,
    pub preset: Option<String>,
    pub question: Option<String>,
    pub review_notes: Option<String>,
    pub author_trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    pub proposed_writes: Vec<ProposedWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposedWrite {
    pub target: String,
    pub operation: String,
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
) -> SessionPlan {
    let roster = if trigger.reason == "ask" {
        configured_roster.first().cloned().into_iter().collect()
    } else {
        configured_roster.to_vec()
    };
    let chair_bot = roster.first().cloned().unwrap_or_else(|| "chair".into());
    let quorum_n = if trigger.reason == "ask" {
        1
    } else {
        std::cmp::max(1, roster.len().saturating_sub(1)) as i64
    };
    let trigger_ref = if trigger.reason == "ask" {
        format!(
            "github:ask/{repo}#{number}/{fingerprint}",
            repo = trigger.repository,
            number = trigger.pr_number,
            fingerprint = trigger.trigger_fingerprint.as_deref().unwrap_or("unknown")
        )
    } else {
        format!(
            "github:pr/{repo}#{number}",
            repo = trigger.repository,
            number = trigger.pr_number
        )
    };
    let prompt = render_prompt(&trigger);

    SessionPlan {
        source_delivery_id: delivery_id.to_string(),
        repository: trigger.repository,
        pr_number: trigger.pr_number,
        reason: trigger.reason.clone(),
        preset: trigger.preset,
        title: if trigger.reason == "ask" {
            "github-followup".into()
        } else {
            "council".into()
        },
        trigger_ref,
        trigger_fingerprint: trigger.trigger_fingerprint,
        roster,
        quorum_n,
        chair_bot,
        mode: if trigger.reason == "ask" {
            "github_followup".into()
        } else {
            "review_council".into()
        },
        prompt,
        recipient_inputs: BTreeMap::new(),
        proposed_writes: Vec::new(),
    }
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
        review_notes: None,
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
    let (reason, question, review_notes) = if starts_with_command(trimmed, "/review") {
        ("/review", None, None)
    } else if let Some(notes) = parse_mention_review(trimmed, bot_handle) {
        ("/review", None, Some(notes))
    } else if let Some(question) = parse_ask(trimmed, bot_handle) {
        ("ask", Some(question), None)
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
        review_notes,
        author_trusted: can_command(
            body["comment"]["author_association"]
                .as_str()
                .unwrap_or_default(),
        ),
    })
}

fn render_prompt(trigger: &Trigger) -> String {
    if trigger.reason == "ask" {
        return format!(
            "Answer the follow-up on {repo} #{number}: {question}",
            repo = trigger.repository,
            number = trigger.pr_number,
            question = trigger.question.as_deref().unwrap_or_default()
        );
    }
    let mut prompt = format!(
        "Review {repo} #{number}. Fetch the pull request and inspect the change in context.",
        repo = trigger.repository,
        number = trigger.pr_number
    );
    if let Some(notes) = trigger
        .review_notes
        .as_deref()
        .filter(|notes| !notes.is_empty())
    {
        prompt.push_str("\n\nAuthor-provided re-review notes:\n");
        prompt.push_str(notes);
    }
    prompt
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

fn parse_mention_review(comment: &str, handle: Option<&str>) -> Option<String> {
    let rest = leading_mention(comment, handle)?;
    let rest = strip_ci_word(rest, "full").unwrap_or(rest);
    strip_ci_word(rest, "review").map(str::to_string)
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
    fn plan_has_no_proposed_external_writes() {
        let trigger = parse_trigger("pull_request", &fixture("opened"), None).unwrap();
        let plan = build_plan(
            "delivery-1",
            trigger,
            &["chair".into(), "rev1".into(), "rev2".into()],
        );
        assert_eq!(plan.trigger_ref, "github:pr/example/repo#42");
        assert_eq!(plan.quorum_n, 2);
        assert!(plan.proposed_writes.is_empty());
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
}
