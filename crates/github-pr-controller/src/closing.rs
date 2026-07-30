//! Turning a closed session into GitHub writes.
//!
//! A `session.terminal` event arrives naming only a session id and carrying the
//! chair's final text. This module decides what that means for the pull
//! request, persists the round, and queues the writes. Nothing here talks to
//! GitHub — the outbox drain does, and only when a write client exists.
//!
//! The split matters: deciding is pure and testable, sending is fallible and
//! retried. A redelivered event re-decides the same way and the outbox's
//! `UNIQUE(session_id, kind)` swallows the duplicate.

use serde_json::{json, Value};

use crate::store::{ReviewFinding, SessionTarget};
use crate::verdict::{ParsedResult, VerdictTrailer};

/// The commit status context this controller owns. Same string the embedded
/// plugin uses, so a canary repo's branch protection needs no change.
pub const STATUS_CONTEXT: &str = "openab/council";

pub const KIND_COMMENT: &str = "comment";
pub const KIND_STATUS: &str = "status";
pub const KIND_REVIEW: &str = "review";

/// The invisible identity a write carries so a retry can recognise its own
/// earlier success. A crash between sending and marking done replays the write
/// after the claim lease lapses, and neither a comment nor a review is
/// idempotent on GitHub's side — this marker is what the pre-send reconcile
/// looks for (council F5 on #305; the P7 gate).
pub fn round_marker(session_id: &str) -> String {
    format!("<!-- openab-round:{session_id} -->")
}

/// What a terminal event turns into. `round` is persisted first; `writes` are
/// queued in this order and drained independently.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosingPlan {
    pub decision: String,
    pub red: i64,
    pub yellow: i64,
    pub green: i64,
    /// What the council says it read — recorded with the round and its
    /// findings. NOT what the commit status is posted against; see `plan_close`.
    pub head_sha: Option<String>,
    pub findings: Vec<ReviewFinding>,
    pub writes: Vec<(&'static str, Value)>,
}

/// Decide the round from the parsed chair text.
///
/// The unparseable case is deliberate and not an error: the session really did
/// close, so we say so with a comment and an `error` status — but we submit
/// **no formal review**, because we have no verdict to stand behind. Silence
/// here is what made two failed rounds on PR #304 look identical to rounds
/// still in flight.
pub fn plan_close(
    target: &SessionTarget,
    parsed: &ParsedResult,
    comment_id: Option<i64>,
    session_id: &str,
) -> ClosingPlan {
    let marker = round_marker(session_id);
    let trailer = parsed.trailer.as_ref();
    let (red, yellow, green) = trailer
        .map(|t| {
            (
                t.red.unwrap_or_default(),
                t.yellow.unwrap_or_default(),
                t.green.unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let decision = trailer
        .map(|t| t.decision.clone())
        .unwrap_or_else(|| "unknown".to_string());
    // Two head shas, two different levels of trust.
    //
    // `target.head_sha` came from the webhook GitHub signed: it is the commit
    // this session was opened for. The chair's findings block carries a sha it
    // *claims* to have reviewed — useful provenance, but agent output, and an
    // agent that named someone else's commit could park a green
    // `openab/council` status on code no one reviewed and satisfy branch
    // protection with it (council F1, #305).
    //
    // So the status — the write with authority — is pinned to the webhook sha.
    // The claimed sha is recorded with the findings, where it describes what
    // was read without granting anything. A council that reviewed a newer
    // commit than the one it was convened for is a supersede, and supersede
    // opens a new session with its own webhook sha.
    let status_sha = target.head_sha.clone();
    let reviewed_sha = parsed
        .findings
        .as_ref()
        .and_then(|block| block.head_sha.clone())
        .or_else(|| target.head_sha.clone());

    let findings = parsed
        .findings
        .as_ref()
        .map(|block| {
            block
                .findings
                .iter()
                .map(|f| ReviewFinding {
                    stable_id: f.id.clone(),
                    severity: f.severity.clone(),
                    status: f.status.clone(),
                    title: f.title.clone(),
                    path: f.path.clone(),
                    line: f.line,
                    raised_by: f.raised_by.clone(),
                    angle: f.angle.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    let mut writes = vec![(
        KIND_COMMENT,
        json!({
            "repo": target.repo,
            "pr_number": target.pr_number,
            // Present → PATCH that comment, absent → create a new one. The
            // round's own id is learned from the response.
            "comment_id": comment_id,
            "body": format!("{}\n\n{marker}", comment_body(parsed, trailer)),
        }),
    )];
    if let Some(sha) = status_sha.as_deref() {
        writes.push((
            KIND_STATUS,
            json!({
                "repo": target.repo,
                "sha": sha,
                "state": status_state(trailer),
                "context": STATUS_CONTEXT,
                "description": status_description(trailer),
            }),
        ));
    }
    if let Some(trailer) = trailer {
        writes.push((
            KIND_REVIEW,
            json!({
                "repo": target.repo,
                "pr_number": target.pr_number,
                // Blocking counts outrank the word; `VerdictTrailer` has
                // already applied that rule, so this reads the decision.
                "event": if trailer.blocking() || trailer.decision == "request_changes" {
                    "REQUEST_CHANGES"
                } else {
                    "APPROVE"
                },
                "body": format!("{}\n\n{marker}", review_body(trailer)),
            }),
        ));
    }

    ClosingPlan {
        decision,
        red,
        yellow,
        green,
        head_sha: reviewed_sha,
        findings,
        writes,
    }
}

fn status_state(trailer: Option<&VerdictTrailer>) -> &'static str {
    match trailer {
        None => "error",
        Some(t) if t.blocking() || t.decision == "request_changes" => "failure",
        Some(_) => "success",
    }
}

fn status_description(trailer: Option<&VerdictTrailer>) -> String {
    match trailer {
        None => "council closed without a parseable verdict".to_string(),
        Some(t) => format!(
            "{} · 🔴{} 🟡{} 🟢{}",
            t.decision,
            t.red.unwrap_or_default(),
            t.yellow.unwrap_or_default(),
            t.green.unwrap_or_default()
        ),
    }
}

fn review_body(trailer: &VerdictTrailer) -> String {
    format!(
        "Council {} — 🔴{} 🟡{} 🟢{}. Details in the review comment.",
        trailer.decision,
        trailer.red.unwrap_or_default(),
        trailer.yellow.unwrap_or_default(),
        trailer.green.unwrap_or_default()
    )
}

/// The chair's report is the comment. Machine parts that mean nothing to a
/// human reader are stripped from the tail: the verdict trailer and the
/// `[done]` marker. The findings block stays — it is an HTML comment, so it is
/// invisible in the rendered comment and keeps the round self-describing.
fn comment_body(parsed: &ParsedResult, trailer: Option<&VerdictTrailer>) -> String {
    let text = parsed.source.trim_end();
    if trailer.is_none() {
        return format!(
            "{text}\n\n---\n⚠️ The council closed without a parseable verdict. \
             No formal review was submitted."
        );
    }
    let mut lines: Vec<&str> = text.lines().collect();
    while let Some(last) = lines.last() {
        let stripped = last.trim();
        if stripped.is_empty() || stripped == "[done]" || stripped.contains("[[verdict:") {
            lines.pop();
        } else {
            break;
        }
    }
    lines.join("\n").trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::parse_final_messages;

    fn target() -> SessionTarget {
        SessionTarget {
            repo: "example/repo".into(),
            pr_number: 7,
            head_sha: Some("openingsha".into()),
        }
    }

    fn kinds(plan: &ClosingPlan) -> Vec<&str> {
        plan.writes.iter().map(|(kind, _)| *kind).collect()
    }

    fn write<'a>(plan: &'a ClosingPlan, kind: &str) -> &'a Value {
        &plan
            .writes
            .iter()
            .find(|(k, _)| *k == kind)
            .expect("write present")
            .1
    }

    #[test]
    fn an_approve_becomes_a_comment_a_success_status_and_a_formal_approval() {
        let parsed = parse_final_messages(&["LGTM\n[[verdict:approve r=0 y=0 g=2]] [done]".into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t");
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_STATUS, KIND_REVIEW]);
        assert_eq!(plan.decision, "approve");
        assert_eq!((plan.red, plan.yellow, plan.green), (0, 0, 2));
        assert_eq!(write(&plan, KIND_STATUS)["state"], "success");
        assert_eq!(write(&plan, KIND_REVIEW)["event"], "APPROVE");
        assert_eq!(
            write(&plan, KIND_COMMENT)["body"],
            format!("LGTM\n\n{}", round_marker("ses_t")),
            "the comment carries its round marker"
        );
    }

    #[test]
    fn anything_blocking_becomes_a_request_changes_review() {
        // 🟡 alone blocks, and it blocks even when the chair wrote `approve` —
        // the counts already overrode the word in the parser.
        let parsed =
            parse_final_messages(&["report\n[[verdict:approve r=0 y=1 g=2]] [done]".into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t");
        assert_eq!(plan.decision, "request_changes");
        assert_eq!(write(&plan, KIND_STATUS)["state"], "failure");
        assert_eq!(write(&plan, KIND_REVIEW)["event"], "REQUEST_CHANGES");
    }

    #[test]
    fn an_unparseable_close_says_so_and_submits_no_review() {
        let parsed = parse_final_messages(&["the council rambled and stopped".into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t");
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_STATUS]);
        assert_eq!(plan.decision, "unknown");
        assert_eq!(write(&plan, KIND_STATUS)["state"], "error");
        assert!(write(&plan, KIND_COMMENT)["body"]
            .as_str()
            .unwrap()
            .contains("without a parseable verdict"));
    }

    #[test]
    fn the_status_is_pinned_to_the_webhook_sha_not_the_one_the_chair_claims() {
        // An agent-named sha must never decide where a green status lands: it
        // could park one on a commit nobody reviewed (council F1, #305). The
        // claimed sha is still recorded — it describes what was read.
        let parsed = parse_final_messages(&[concat!(
            "report\n",
            "<!-- openab-findings\n",
            "{\"head_sha\":\"reviewedsha\",\"findings\":[",
            "{\"id\":\"F1\",\"severity\":\"yellow\",\"title\":\"races\"}]}\n-->\n",
            "[[verdict:request_changes r=0 y=1 g=0]] [done]"
        )
        .into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t");
        assert_eq!(plan.head_sha.as_deref(), Some("reviewedsha"));
        assert_eq!(
            write(&plan, KIND_STATUS)["sha"],
            "openingsha",
            "the status goes to the commit GitHub told us about"
        );
        assert_eq!(plan.findings.len(), 1);
        assert_eq!(plan.findings[0].stable_id, "F1");
    }

    #[test]
    fn a_known_comment_id_turns_the_comment_into_an_upsert() {
        let parsed = parse_final_messages(&["LGTM\n[[verdict:approve r=0 y=0 g=1]] [done]".into()]);
        let plan = plan_close(&target(), &parsed, Some(4242), "ses_t");
        assert_eq!(write(&plan, KIND_COMMENT)["comment_id"], 4242);
    }

    #[test]
    fn a_session_with_no_head_sha_anywhere_skips_the_status_rather_than_guessing() {
        let mut target = target();
        target.head_sha = None;
        let parsed = parse_final_messages(&["LGTM\n[[verdict:approve r=0 y=0 g=1]] [done]".into()]);
        let plan = plan_close(&target, &parsed, None, "ses_t");
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_REVIEW]);
    }

    #[test]
    fn the_comment_drops_machine_tails_but_keeps_the_findings_block() {
        let parsed = parse_final_messages(&[concat!(
            "## Verdict\n\nprose here\n\n",
            "<!-- openab-findings\n{\"findings\":[]}\n-->\n",
            "[[verdict:approve r=0 y=0 g=0]] [done]"
        )
        .into()]);
        let body = write(&plan_close(&target(), &parsed, None, "ses_t"), KIND_COMMENT)["body"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(body.starts_with("## Verdict"));
        assert!(
            body.contains("openab-findings"),
            "block is invisible, keep it"
        );
        assert!(!body.contains("[[verdict:"), "{body}");
        assert!(!body.contains("[done]"), "{body}");
    }
}
