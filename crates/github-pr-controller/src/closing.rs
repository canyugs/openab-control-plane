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
// The round comment's two pre-verdict states: posted the moment the council
// convenes (create only if the session's marker is absent, so a fast close
// can never be clobbered), and rewritten if the session ends without a
// verdict (update only if the marker exists; markers are per-session, and a
// session gets exactly one terminal state, so this can never touch a
// verdict).
pub const KIND_COMMENT_OPEN: &str = "comment_open";
pub const KIND_COMMENT_ABANDON: &str = "comment_abandon";

/// The invisible identity a write carries so a retry can recognise its own
/// earlier success. A crash between sending and marking done replays the write
/// after the claim lease lapses, and neither a comment nor a review is
/// idempotent on GitHub's side — this marker is what the pre-send reconcile
/// looks for (council F5 on #305; the P7 gate).
pub fn round_marker(session_id: &str) -> String {
    format!("<!-- openab-round:{session_id} -->")
}

/// The opening post's own identity, distinct from the verdict's
/// `round_marker` so the two are separate comments: the verdict's pre-send
/// reconcile must never adopt the "started" post (operator decision
/// 2026-08-02 — keep the started notice in the timeline instead of
/// rewriting it in place). The abandon tombstone still rewrites the
/// opening post, so it reconciles on this marker too.
pub fn open_marker(session_id: &str) -> String {
    format!("<!-- openab-round-open:{session_id} -->")
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
    /// ADR 035: waiver ids named by `status:"waived"` findings — the terminal
    /// path bumps their repo-scoped fired counters once per first-time round.
    pub fired_waivers: Vec<String>,
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
    is_ask: bool,
) -> ClosingPlan {
    let marker = round_marker(session_id);
    if is_ask {
        // A follow-up question, not a review round (SEI-929). Its settled final
        // message IS the answer — the ask template forbids tool narration and
        // self-posting — so post it as a plain comment with no verdict, no
        // status, and no review. No "no parseable verdict" warning either: an
        // ask legitimately has no verdict.
        return ClosingPlan {
            decision: "ask".to_string(),
            red: 0,
            yellow: 0,
            green: 0,
            head_sha: target.head_sha.clone(),
            findings: vec![],
            fired_waivers: vec![],
            writes: vec![(
                KIND_COMMENT,
                json!({
                    "repo": target.repo,
                    "pr_number": target.pr_number,
                    "comment_id": comment_id,
                    "body": format!("{}\n\n{marker}", answer_body(parsed)),
                }),
            )],
        };
    }
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
    let fired_waivers: Vec<String> = parsed
        .findings
        .as_ref()
        .map(|block| {
            block
                .findings
                .iter()
                .filter(|f| f.status == "waived")
                .filter_map(|f| f.waiver_id.clone())
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
                "body": format!("{}\n\n{marker}", review_body(trailer, reviewed_sha.as_deref())),
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
        fired_waivers,
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
    // Commit status descriptions reject 4-byte UTF-8 ("Description doesn't
    // accept 4-byte Unicode", 422) — no emoji here, unlike the review body.
    match trailer {
        None => "council closed without a parseable verdict".to_string(),
        Some(t) => format!(
            "{} · red {} · yellow {} · green {}",
            t.decision,
            t.red.unwrap_or_default(),
            t.yellow.unwrap_or_default(),
            t.green.unwrap_or_default()
        ),
    }
}

fn review_body(trailer: &VerdictTrailer, reviewed_sha: Option<&str>) -> String {
    let at = reviewed_sha
        .map(|sha| format!(" Reviewed at {sha}."))
        .unwrap_or_default();
    format!(
        "Council {} — 🔴{} 🟡{} 🟢{}.{at} Details in the review comment.",
        trailer.decision,
        trailer.red.unwrap_or_default(),
        trailer.yellow.unwrap_or_default(),
        trailer.green.unwrap_or_default()
    )
}

/// The follow-up answer as posted (SEI-929). The settled final message is the
/// answer; only machine tails are removed — a trailing `[done]` and any stray
/// `[[verdict:…]]` line the model added out of habit. No council anchor and no
/// "no verdict" warning: an ask legitimately has neither. An empty answer
/// becomes a short notice, never a blank comment.
fn answer_body(parsed: &ParsedResult) -> String {
    let text = parsed.source.trim_end();
    let mut lines: Vec<&str> = text.lines().collect();
    if let Some(at) = lines.iter().position(|line| line.contains("[[verdict:")) {
        lines.truncate(at);
    }
    while let Some(last) = lines.last() {
        let stripped = last.trim();
        if stripped.is_empty() || stripped == "[done]" {
            lines.pop();
        } else {
            break;
        }
    }
    let body = unfence_tables(lines).join("\n").trim().to_string();
    if body.is_empty() {
        return "This follow-up produced no answer.".to_string();
    }
    body
}

/// Where the chair's report begins. The task template requires the verdict
/// comment to start with this line, which makes it a protocol anchor: anything
/// before it in the settled text is working noise, not report.
const REPORT_START: &str = "<!-- openab-council -->";

/// Posted when the session closed with no verdict we can stand behind.
const NO_VERDICT_NOTICE: &str = "⚠️ The council closed without a parseable verdict. \
     No formal review was submitted.";

/// The structured findings block by its own delimiters. It survives an
/// anchorless close because — unlike the free-form report prose — it has stable
/// markers, so it can be lifted out of a broken settled text and kept with the
/// degraded comment to keep the round self-describing.
fn findings_block(text: &str) -> Option<&str> {
    let start = text.find("<!-- openab-findings")?;
    let end = text[start..].find("-->").map(|rel| start + rel + 3)?;
    Some(&text[start..end])
}

/// The comment for an anchorless close that still parsed a verdict (a chair
/// whose synthesis turn failed mid-write). We refuse to publish the raw text,
/// but the verdict and findings are structured data we trust — the status and
/// review are posted from the same trailer — so the comment states the verdict
/// and carries the findings block, without the lost report prose.
fn degraded_body(trailer: &VerdictTrailer, findings: Option<&str>) -> String {
    let mut body = format!(
        "⚠️ The council reached **{}** — 🔴{} 🟡{} 🟢{} — but its written report \
         could not be recovered (the synthesis turn failed mid-write). The \
         verdict and findings stand; comment `@opencodezebra review` to re-run \
         the council for a full report.",
        trailer.decision,
        trailer.red.unwrap_or_default(),
        trailer.yellow.unwrap_or_default(),
        trailer.green.unwrap_or_default(),
    );
    if let Some(block) = findings {
        body.push_str("\n\n");
        body.push_str(block);
    }
    body
}

/// The chair's report is the comment. The settled text arrives with working
/// noise around it — the Kiro CLI transcribes its tool calls into message
/// bodies, and the chair thinks aloud before the report — so the body starts
/// at the `<!-- openab-council -->` anchor the template mandates (everything
/// before it is dropped), and machine parts are stripped from the tail: the
/// verdict trailer and the `[done]` marker. The findings block stays — it is
/// an HTML comment, so it is invisible in the rendered comment and keeps the
/// round self-describing.
fn comment_body(parsed: &ParsedResult, trailer: Option<&VerdictTrailer>) -> String {
    let text = parsed.source.trim_end();
    // LAST anchor, not first: the working noise can itself contain the anchor
    // (nuphos#725 round 3 — a Kiro task echo of the report template opened the
    // settled text, and anchoring there published 4KB of tool transcript), and
    // the protocol puts the real report last. A re-draft supersedes its draft
    // the same way.
    //
    // The anchor is authoritative: it is the ONLY marker that says "the report
    // starts here". Without it we cannot separate report from working-noise —
    // not by denylisting tool lines (the chair's free-form narration has no
    // stable shape and reads exactly like report prose), so we must never
    // publish the raw text. A mid-synthesis error is the real no-anchor case:
    // the runtime prepends a banner and the chair never emits the anchor
    // (backend#2418 round 4, a -32603 error that leaked the whole transcript;
    // infra-zeabur-system#208, a Solo follow-up). Rebuild from structured data.
    let Some(at) = text.rfind(REPORT_START) else {
        return match trailer {
            None => NO_VERDICT_NOTICE.to_string(),
            Some(t) => degraded_body(t, findings_block(text)),
        };
    };
    let text = &text[at..];
    if trailer.is_none() {
        // A real report (it has the anchor) that only failed to emit its
        // verdict line — keep it, flag the missing verdict below.
        return format!("{text}\n\n---\n{NO_VERDICT_NOTICE}");
    }
    let mut lines: Vec<&str> = text.lines().collect();
    // The report ends at its trailer. Anything after that line is working
    // noise the chair emitted past the verdict (nuphos#725 round 6 carried
    // octobroker transcript between the trailer and a second draft), so the
    // cut is at the first trailer line, not just trailing machine lines.
    if let Some(at) = lines.iter().position(|line| line.contains("[[verdict:")) {
        lines.truncate(at);
    }
    while let Some(last) = lines.last() {
        let stripped = last.trim();
        if stripped.is_empty() || stripped == "[done]" {
            lines.pop();
        } else {
            break;
        }
    }
    unfence_tables(lines).join("\n").trim_end().to_string()
}

/// Every chair model tried (Kiro and Claude alike) wraps the Findings tables
/// in code fences when writing the report as a chat message — GitHub then
/// renders them as preformatted text. The steering rule against it is
/// ignored, so the fence is undone here: a fenced block whose non-empty
/// lines all start with `|` (and at least one does) is unwrapped.
fn unfence_tables(lines: Vec<&str>) -> Vec<&str> {
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().starts_with("```") {
            if let Some(close) = lines[i + 1..]
                .iter()
                .position(|line| line.trim().starts_with("```"))
            {
                let block = &lines[i + 1..i + 1 + close];
                let is_table = block.iter().any(|line| line.trim_start().starts_with('|'))
                    && block
                        .iter()
                        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('|'));
                if is_table {
                    out.extend_from_slice(block);
                    i += close + 2;
                    continue;
                }
            }
        }
        out.push(lines[i]);
        i += 1;
    }
    out
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
            reason: None,
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
        let parsed = parse_final_messages(
            &["<!-- openab-council -->\nLGTM\n[[verdict:approve r=0 y=0 g=2]] [done]".into()],
        );
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_STATUS, KIND_REVIEW]);
        assert_eq!(plan.decision, "approve");
        assert_eq!((plan.red, plan.yellow, plan.green), (0, 0, 2));
        assert_eq!(write(&plan, KIND_STATUS)["state"], "success");
        let description = write(&plan, KIND_STATUS)["description"].as_str().unwrap();
        assert!(
            description.chars().all(|c| c <= '\u{FFFF}'),
            "GitHub rejects 4-byte Unicode in status descriptions: {description}"
        );
        assert_eq!(description, "approve · red 0 · yellow 0 · green 2");
        assert_eq!(write(&plan, KIND_REVIEW)["event"], "APPROVE");
        assert_eq!(
            write(&plan, KIND_COMMENT)["body"],
            format!("<!-- openab-council -->\nLGTM\n\n{}", round_marker("ses_t")),
            "the comment carries its round marker"
        );
    }

    #[test]
    fn fenced_findings_tables_are_unwrapped_but_code_blocks_survive() {
        // Shape of nuphos#664: the chair fences the table header and body as
        // separate blocks. A real code block in the same report must remain.
        let report = "<!-- openab-council -->\n\
             CHANGES REQUESTED ⚠️ — summary.\n\n\
             ## Findings\n\n\
             ```\n\
             | ID | Severity | Title |\n\
             ```\n\
             ```\n\
             | -- | -------- | ----- |\n\
             | F1 | 🟡 | tab reset |\n\
             ```\n\n\
             ```rust\n\
             let keep = me;\n\
             ```\n\
             [[verdict:request_changes r=0 y=1 g=0]] [done]";
        let parsed = parse_final_messages(&[report.into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(!body.contains("```\n| ID"), "table header unfenced");
        assert!(body.contains("| ID | Severity | Title |\n| -- | -------- | ----- |"));
        assert!(body.contains("```rust\nlet keep = me;\n```"));
    }

    #[test]
    fn the_comment_starts_at_the_report_anchor_not_the_working_noise() {
        // Shape of #309 round 3: the Kiro chair transcribes tool calls and
        // thinks aloud before the report in one message, and the footer,
        // findings block, and trailer arrive in a later message.
        let synthesis = "✅ `Creating task list: Synthesize round 3 verdict`\n\
             ✅ `Running: printf '%s' '{\"pullNumber\":309}' | octobroker-mcp call pull_request_read`\n\
             Good — the head SHA is unchanged. I've verified the file. No issues.\
             <!-- openab-council -->\n\
             LGTM ✅ — Docs-only change.\n\
             Reviewed at 701a1bf (round 3)\n\n\
             ## Delta since d3fbb56\n\n- One appended line.";
        let closing = "🔴×0 🟡×0 🟢×1 · 💬 Comment `@bot <question>` for a follow-up\n\n\
             <!-- openab-findings\n\
             {\"head_sha\":\"701a1bf\",\"findings\":[]}\n\
             -->\n\
             [[verdict:approve r=0 y=0 g=1]] [done]";
        let parsed = parse_final_messages(&[synthesis.into(), closing.into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(
            body.starts_with("<!-- openab-council -->"),
            "report anchor opens the comment: {body}"
        );
        assert!(
            !body.contains("✅ `") && !body.contains("No issues."),
            "tool echoes and self-talk are dropped: {body}"
        );
        assert!(
            body.contains("## Delta since") && body.contains("🔴×0 🟡×0 🟢×1"),
            "the report and its footer survive: {body}"
        );
        assert!(
            body.contains("openab-findings") && !body.contains("[[verdict:"),
            "findings block stays, trailer goes: {body}"
        );
    }

    #[test]
    fn noise_containing_the_anchor_cannot_steal_the_report() {
        // Shape of nuphos#725 round 3: a Kiro task echo QUOTES the report
        // template — anchor string included — before the tool transcript, so
        // anchoring on the first occurrence published the whole transcript.
        // The real report is the last anchor.
        let noisy = "<!-- openab-council --> ; CHANGES REQUESTED ⚠️ — draft title ; R...`\n\
             ✅ `Running: /home/agent/bin/octobroker-mcp comment zeabur nuphos 725 < /tmp/verdict.md`\n\
             Now I need to verify the finding myself before writing the verdict.\n\
             <!-- openab-council -->\n\
             CHANGES REQUESTED ⚠️ — the real report.\n\n\
             ## Findings\n\n| F1 | 🟡 | real |\n\n\
             [[verdict:request_changes r=0 y=1 g=0]] [done]";
        let parsed = parse_final_messages(&[noisy.into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(
            body.starts_with("<!-- openab-council -->\nCHANGES REQUESTED ⚠️ — the real report."),
            "the LAST anchor opens the comment: {body}"
        );
        assert!(
            !body.contains("octobroker") && !body.contains("Now I need"),
            "the quoted-anchor noise is dropped: {body}"
        );
    }

    #[test]
    fn nothing_past_the_trailer_is_published() {
        // Shape of nuphos#725 round 6: the chair emitted a full report, its
        // trailer, MORE tool transcript, then a second draft. Everything from
        // the first trailer line on is machine tail, except that a re-draft
        // with its own anchor supersedes the lot.
        let one_draft_then_noise = "<!-- openab-council -->\n\
             CHANGES REQUESTED ⚠️ — the report.\n\n\
             [[verdict:request_changes r=0 y=2 g=1]] [done]\n\
             ✅ `Running: printf '%s' '{\"pullNumber\":725}' | /home/agent/bin/octobroker-mcp call pull_request_read`\n\
             Let me fetch the head SHA again.";
        let parsed = parse_final_messages(&[
            one_draft_then_noise.into(),
            "footer\n[[verdict:request_changes r=0 y=2 g=1]] [done]".into(),
        ]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(
            !body.contains("octobroker") && !body.contains("Let me fetch"),
            "post-trailer transcript is dropped: {body}"
        );
        assert!(
            body.contains("CHANGES REQUESTED ⚠️ — the report.\n\n<!-- openab-round:"),
            "the report ends at its trailer, then the round marker: {body}"
        );

        let redraft = "<!-- openab-council -->\n\
             CHANGES REQUESTED ⚠️ — superseded draft.\n\
             [[verdict:request_changes r=0 y=2 g=1]] [done]\n\
             ✅ `Running: octobroker-mcp call pull_request_read`\n\
             <!-- openab-council -->\n\
             CHANGES REQUESTED ⚠️ — the final draft.\n\
             [[verdict:request_changes r=0 y=2 g=1]] [done]";
        let parsed = parse_final_messages(&[redraft.into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(
            body.contains("the final draft") && !body.contains("superseded draft"),
            "a re-draft with its own anchor supersedes the lot: {body}"
        );
        assert!(
            !body.contains("octobroker"),
            "transcript between drafts is dropped: {body}"
        );
    }

    #[test]
    fn the_review_names_the_sha_it_stands_behind() {
        let parsed = parse_final_messages(&[
            "report\n<!-- openab-findings\n{\"head_sha\":\"feedc0de\",\"findings\":[]}\n-->\n[[verdict:approve r=0 y=0 g=1]] [done]".into(),
        ]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_REVIEW)["body"].as_str().unwrap();
        assert!(
            body.contains("Reviewed at feedc0de"),
            "review timeline entry must name the reviewed sha: {body}"
        );
    }

    #[test]
    fn anything_blocking_becomes_a_request_changes_review() {
        // 🟡 alone blocks, and it blocks even when the chair wrote `approve` —
        // the counts already overrode the word in the parser.
        let parsed =
            parse_final_messages(&["report\n[[verdict:approve r=0 y=1 g=2]] [done]".into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        assert_eq!(plan.decision, "request_changes");
        assert_eq!(write(&plan, KIND_STATUS)["state"], "failure");
        assert_eq!(write(&plan, KIND_REVIEW)["event"], "REQUEST_CHANGES");
    }

    #[test]
    fn an_unparseable_close_says_so_and_submits_no_review() {
        let parsed = parse_final_messages(&["the council rambled and stopped".into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_STATUS]);
        assert_eq!(plan.decision, "unknown");
        assert_eq!(write(&plan, KIND_STATUS)["state"], "error");
        assert!(write(&plan, KIND_COMMENT)["body"]
            .as_str()
            .unwrap()
            .contains("without a parseable verdict"));
    }

    #[test]
    fn an_anchorless_unparseable_close_never_publishes_the_transcript() {
        // A Solo follow-up (@bot reply) whose settled text is a raw Kiro
        // transcript with no report anchor and no verdict must not be dumped
        // into the PR — infra-zeabur-system#208 leaked 7KB of tool log this way.
        let transcript =
            "✅ Running: gh pr diff 208\n<thinking>secret plan</thinking>\nanswer.md\n[done]";
        let parsed = parse_final_messages(&[transcript.into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(body.contains("without a parseable verdict"));
        assert!(
            !body.contains("Running") && !body.contains("thinking"),
            "raw transcript leaked into the comment: {body}"
        );
    }

    #[test]
    fn an_anchored_close_missing_only_its_trailer_keeps_the_report() {
        // The other no-trailer case: a real report (has the anchor) that just
        // failed to emit a verdict line. That body is trustworthy — keep it.
        let parsed = parse_final_messages(&[format!(
            "noise before\n{REPORT_START}\nThe report body says X."
        )]);
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(body.contains("The report body says X."));
        assert!(!body.contains("noise before"));
        assert!(body.contains("without a parseable verdict"));
    }

    #[test]
    fn an_ask_close_posts_only_a_plain_answer_comment() {
        // A follow-up (@bot <question>): the settled message is the answer.
        // No status, no review, no verdict warning — just the comment (SEI-929).
        let parsed = parse_final_messages(&[
            "Regarding F1: the mapping is keyed by request region, so cgk1 is correct.\n[done]"
                .into(),
        ]);
        let plan = plan_close(&target(), &parsed, None, "ses_ask", true);
        assert_eq!(kinds(&plan), [KIND_COMMENT]);
        assert_eq!(plan.decision, "ask");
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(body.contains("cgk1 is correct"));
        assert!(!body.contains("[done]"), "machine tail leaked: {body}");
        assert!(
            !body.contains("parseable verdict"),
            "ask has no verdict — must not warn: {body}"
        );
    }

    #[test]
    fn an_ask_close_strips_a_stray_verdict_trailer_the_model_added() {
        let parsed = parse_final_messages(&[
            "Short answer here.\n[[verdict:approve r=0 y=0 g=0]]\n[done]".into(),
        ]);
        let plan = plan_close(&target(), &parsed, None, "ses_ask", true);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(body.contains("Short answer here."));
        assert!(!body.contains("[[verdict:"), "verdict trailer leaked: {body}");
        assert_eq!(kinds(&plan), [KIND_COMMENT]);
    }

    #[test]
    fn an_empty_ask_close_posts_a_notice_not_a_blank_comment() {
        let parsed = parse_final_messages(&["[done]".into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_ask", true);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(body.contains("produced no answer"), "got: {body}");
    }

    #[test]
    fn an_anchorless_close_with_a_verdict_posts_a_summary_not_the_transcript() {
        // backend#2418 round 4: a -32603 mid-synthesis error prepended an error
        // banner and a tool transcript, the chair never emitted the council
        // anchor, but a verdict still parsed. The comment must carry the verdict
        // and findings — never the banner or the transcript.
        let parsed = parse_final_messages(&[concat!(
            "⚠️ **Internal Error** (code: -32603)\n",
            "✅ `Running: octobroker-mcp call pull_request_read`\n",
            "Received rev-codex's report. Waiting for rev-claude.\n",
            "<!-- openab-findings\n",
            "{\"head_sha\":\"96f66e1\",\"findings\":[{\"id\":\"F7\",\"severity\":\"green\",\"title\":\"skip\"}]}\n-->\n",
            "[[verdict:approve r=0 y=0 g=2]] [done]"
        )
        .into()]);
        let plan = plan_close(&target(), &parsed, None, "ses_r4", false);
        // Verdict still drives status + review.
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_STATUS, KIND_REVIEW]);
        let body = write(&plan, KIND_COMMENT)["body"].as_str().unwrap();
        assert!(
            !body.contains("Internal Error")
                && !body.contains("Running")
                && !body.contains("rev-claude"),
            "banner/transcript leaked: {body}"
        );
        assert!(body.contains("approve"), "verdict summary missing: {body}");
        assert!(
            body.contains("openab-findings") && body.contains("F7"),
            "findings block should survive: {body}"
        );
        assert!(!body.contains("[[verdict:"), "raw trailer leaked: {body}");
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
        let plan = plan_close(&target(), &parsed, None, "ses_t", false);
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
        let plan = plan_close(&target(), &parsed, Some(4242), "ses_t", false);
        assert_eq!(write(&plan, KIND_COMMENT)["comment_id"], 4242);
    }

    #[test]
    fn a_session_with_no_head_sha_anywhere_skips_the_status_rather_than_guessing() {
        let mut target = target();
        target.head_sha = None;
        let parsed = parse_final_messages(&["LGTM\n[[verdict:approve r=0 y=0 g=1]] [done]".into()]);
        let plan = plan_close(&target, &parsed, None, "ses_t", false);
        assert_eq!(kinds(&plan), [KIND_COMMENT, KIND_REVIEW]);
    }

    #[test]
    fn the_comment_drops_machine_tails_but_keeps_the_findings_block() {
        let parsed = parse_final_messages(&[concat!(
            "<!-- openab-council -->\n## Verdict\n\nprose here\n\n",
            "<!-- openab-findings\n{\"findings\":[]}\n-->\n",
            "[[verdict:approve r=0 y=0 g=0]] [done]"
        )
        .into()]);
        let body = write(&plan_close(&target(), &parsed, None, "ses_t", false), KIND_COMMENT)["body"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(body.starts_with("<!-- openab-council -->\n## Verdict"));
        assert!(
            body.contains("openab-findings"),
            "block is invisible, keep it"
        );
        assert!(!body.contains("[[verdict:"), "{body}");
        assert!(!body.contains("[done]"), "{body}");
    }
}
