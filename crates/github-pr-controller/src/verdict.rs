//! Reading a closed council session's result out of a terminal event.
//!
//! A behavior-equal port of the kernel's `pr_review` parsers (ADR 013 verdict
//! trailer, ADR 020 findings block). Two implementations of one grammar is a
//! liability, so the tests below are the kernel's own vectors carried over
//! verbatim — if the two ever disagree, a test here fails, which is the parity
//! evidence ADR 031 invariant #3 asks for.
//!
//! The kernel parses the two halves from different text and so must this:
//!
//! * the **trailer** from the closing message alone (`orchestrator.rs`
//!   `Action::Close`, which passes `verdict`);
//! * the **findings block** from the whole settled span joined with `\n`,
//!   because the transport splits long messages and the block can straddle a
//!   split (live loss: zeabur.com#702 round 4).
//!
//! `final_messages` on the terminal event carries that span, closing message
//! last (P1, `docs/controller-action-api.md`).

use serde::Deserialize;

/// Parsed `[[verdict:…]]` trailer: chair decision plus optional 🔴/🟡/🟢 counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictTrailer {
    pub decision: String, // "approve" | "request_changes"
    pub red: Option<i64>,
    pub yellow: Option<i64>,
    pub green: Option<i64>,
}

impl VerdictTrailer {
    /// Anything actionable and open blocks. 🟢 never does.
    pub fn blocking(&self) -> bool {
        self.red.unwrap_or(0) > 0 || self.yellow.unwrap_or(0) > 0
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FindingsBlock {
    #[serde(default)]
    pub head_sha: Option<String>,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Finding {
    /// PR-scoped stable id, e.g. `F1`.
    pub id: String,
    /// `red` | `yellow` | `green`.
    pub severity: String,
    /// `open` | `resolved` | `dismissed`. Defaults to `open`.
    #[serde(default = "default_status")]
    pub status: String,
    pub title: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub raised_by: Option<String>,
    #[serde(default)]
    pub angle: Option<String>,
}

fn default_status() -> String {
    "open".to_string()
}

/// What a terminal event's `final_messages` says about the review. Both halves
/// are independently optional: a chair can post a trailer with no block, and a
/// malformed block must not cost us the verdict (or the reverse).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedResult {
    pub trailer: Option<VerdictTrailer>,
    pub findings: Option<FindingsBlock>,
}

/// Parse both halves out of a terminal event's `final_messages`, each from the
/// text the kernel uses for it.
pub fn parse_final_messages(final_messages: &[String]) -> ParsedResult {
    let Some(closing) = final_messages.last() else {
        return ParsedResult::default();
    };
    ParsedResult {
        trailer: parse_verdict_trailer(closing),
        findings: parse_findings_block(&final_messages.join("\n")),
    }
}

/// Lines outside ``` fences, trimmed, empties dropped. An unterminated fence
/// swallows the rest of the text — fail closed, as the kernel does.
fn unfenced_lines(text: &str) -> Vec<&str> {
    text.split("```")
        .step_by(2)
        .flat_map(str::lines)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

/// Parse `[[verdict:approve|request_changes r=N y=N g=N]]` only from the final
/// non-empty unfenced line. Counts are optional; if several trailers share that
/// line, the last wins. Anything malformed rejects the whole trailer.
pub fn parse_verdict_trailer(text: &str) -> Option<VerdictTrailer> {
    let line = unfenced_lines(text).into_iter().next_back()?;
    let start = line.rfind("[[verdict:")?;
    let rest = &line[start + "[[verdict:".len()..];
    let inner = &rest[..rest.find("]]")?];
    let mut parts = inner.split_whitespace();
    let decision = parts.next()?;
    if decision != "approve" && decision != "request_changes" {
        return None;
    }
    let (mut red, mut yellow, mut green) = (None, None, None);
    for part in parts {
        let (key, value) = part.split_once('=')?;
        let n: i64 = value.parse().ok().filter(|n| *n >= 0)?;
        match key {
            "r" => red = Some(n),
            "y" => yellow = Some(n),
            "g" => green = Some(n),
            _ => return None,
        }
    }
    // The counts are the decision, not the word next to them — a chair can
    // stamp `approve` beside open findings and nothing in the prompt stops it.
    //
    // Derivation runs in ONE direction on partial counts: a lone `r=3` is
    // unambiguous and escalates, but `r=0` alone says nothing about yellows, so
    // it must never downgrade a request_changes. Downgrading needs the full
    // picture; escalating never does. No counts at all → the chair's word.
    let mut decision = decision.to_string();
    let blocking = red.unwrap_or(0) > 0 || yellow.unwrap_or(0) > 0;
    let complete = red.is_some() && yellow.is_some();
    let derived = if blocking {
        Some("request_changes")
    } else if complete {
        Some("approve")
    } else {
        None
    };
    if let Some(derived) = derived {
        if derived != decision {
            tracing::warn!(
                chair_decision = %decision,
                derived = %derived,
                red = ?red,
                yellow = ?yellow,
                "verdict trailer disagrees with its own counts; using the counts"
            );
            decision = derived.to_string();
        }
    }
    Some(VerdictTrailer {
        decision,
        red,
        yellow,
        green,
    })
}

/// Extract and validate the last `<!-- openab-findings … -->` block. Last block
/// wins (the chair may have quoted a draft). All-or-nothing: any malformed JSON
/// or invalid enum rejects the block rather than yielding a partial ledger.
pub fn parse_findings_block(text: &str) -> Option<FindingsBlock> {
    const OPEN: &str = "<!-- openab-findings";
    let start = text.rfind(OPEN)?;
    let rest = &text[start + OPEN.len()..];
    // A literal `-->` inside a title or path would truncate the JSON at the
    // first occurrence — try each candidate until one parses.
    let block: FindingsBlock = rest
        .match_indices("-->")
        .find_map(|(end, _)| serde_json::from_str(rest[..end].trim()).ok())?;
    let valid = block.findings.iter().all(|f| {
        matches!(f.severity.as_str(), "red" | "yellow" | "green")
            && matches!(f.status.as_str(), "open" | "resolved" | "dismissed")
            && !f.id.trim().is_empty()
            && !f.title.trim().is_empty()
    });
    valid.then_some(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- vectors carried over from the kernel's verdict.rs tests ----

    #[test]
    fn counts_decide_the_verdict() {
        for (trailer, want) in [
            ("[[verdict:approve r=0 y=0 g=4]] [done]", "approve"),
            ("[[verdict:approve r=0 y=2 g=1]] [done]", "request_changes"),
            ("[[verdict:approve r=1 y=0 g=0]] [done]", "request_changes"),
            ("[[verdict:request_changes r=0 y=0 g=3]] [done]", "approve"),
            (
                "[[verdict:request_changes r=2 y=1 g=0]] [done]",
                "request_changes",
            ),
        ] {
            assert_eq!(
                parse_verdict_trailer(trailer).unwrap().decision,
                want,
                "trailer {trailer}"
            );
        }
        assert_eq!(
            parse_verdict_trailer("[[verdict:approve]] [done]")
                .unwrap()
                .decision,
            "approve"
        );
        for trailer in [
            "[[verdict:approve r=3]] [done]",
            "[[verdict:approve y=1]] [done]",
            "[[verdict:approve r=3 g=2]] [done]",
        ] {
            assert_eq!(
                parse_verdict_trailer(trailer).unwrap().decision,
                "request_changes",
                "trailer {trailer}"
            );
        }
        assert_eq!(
            parse_verdict_trailer("[[verdict:request_changes r=0]] [done]")
                .unwrap()
                .decision,
            "request_changes"
        );
        assert_eq!(
            parse_verdict_trailer("[[verdict:request_changes g=9]] [done]")
                .unwrap()
                .decision,
            "request_changes"
        );
    }

    #[test]
    fn verdict_trailer_parsing() {
        let t = parse_verdict_trailer(
            "Report…\n\nVerdict: request changes\n[[verdict:request_changes r=1 y=3 g=5]] [done]",
        )
        .unwrap();
        assert_eq!(t.decision, "request_changes");
        assert_eq!((t.red, t.yellow, t.green), (Some(1), Some(3), Some(5)));

        let t = parse_verdict_trailer("LGTM [[verdict:approve]] [done]").unwrap();
        assert_eq!(t.decision, "approve");
        assert_eq!((t.red, t.yellow, t.green), (None, None, None));

        let t =
            parse_verdict_trailer("[[verdict:approve]] … [[verdict:request_changes r=2]]").unwrap();
        assert_eq!(t.decision, "request_changes");
        assert_eq!(t.red, Some(2));

        let t = parse_verdict_trailer(
            "quoted bad draft:\n> [[verdict:maybe r=1]]\n\n[[verdict:approve r=0 y=1 g=2]] [done]",
        )
        .unwrap();
        assert_eq!(t.decision, "request_changes");
        assert_eq!((t.red, t.yellow, t.green), (Some(0), Some(1), Some(2)));

        assert!(parse_verdict_trailer("[[verdict:approve]]\nfinal prose after trailer").is_none());
        assert!(parse_verdict_trailer("```\n[[verdict:approve]] [done]\n```").is_none());
        assert_eq!(
            parse_verdict_trailer("[[verdict:approve]] [done]")
                .unwrap()
                .decision,
            "approve"
        );

        assert!(parse_verdict_trailer("no trailer here [done]").is_none());
        assert!(parse_verdict_trailer("[[verdict:maybe r=1]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve r=lots]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve r=-1]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve x=1]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve r=1").is_none()); // unclosed
    }

    #[test]
    fn unfenced_lines_drops_fenced_segments_fail_closed() {
        assert_eq!(
            unfenced_lines("alpha\n```\n[[recruit:x]]\n```\nbeta"),
            vec!["alpha", "beta"]
        );
        assert_eq!(
            unfenced_lines("alpha\n```\n[[recruit:x]]\nbeta"),
            vec!["alpha"]
        );
    }

    // ---- vectors carried over from the kernel's findings.rs tests ----

    const BLOCK: &str = r#"Report prose.
<!-- openab-findings
{"head_sha":"abc123","findings":[
 {"id":"F1","severity":"red","title":"races on close","path":"src/a.rs","line":7,
  "raised_by":"rev1","angle":"correctness"},
 {"id":"F2","severity":"yellow","status":"resolved","title":"stale doc"}]}
-->
[[verdict:request_changes r=1 y=1 g=0]] [done]"#;

    #[test]
    fn parses_full_block() {
        let b = parse_findings_block(BLOCK).unwrap();
        assert_eq!(b.head_sha.as_deref(), Some("abc123"));
        assert_eq!(b.findings.len(), 2);
        let f1 = &b.findings[0];
        assert_eq!(
            (f1.id.as_str(), f1.severity.as_str(), f1.status.as_str()),
            ("F1", "red", "open") // status defaults to open
        );
        assert_eq!(f1.path.as_deref(), Some("src/a.rs"));
        assert_eq!(f1.line, Some(7));
        assert_eq!(f1.raised_by.as_deref(), Some("rev1"));
        assert_eq!(f1.angle.as_deref(), Some("correctness"));
        assert_eq!(b.findings[1].status, "resolved");
    }

    #[test]
    fn last_block_wins() {
        let text = format!(
            "quoted draft:\n<!-- openab-findings\n{{\"findings\":[{{\"id\":\"F9\",\"severity\":\"green\",\"title\":\"old\"}}]}}\n-->\n{BLOCK}"
        );
        let b = parse_findings_block(&text).unwrap();
        assert_eq!(b.findings[0].id, "F1");
    }

    #[test]
    fn arrow_inside_title_does_not_truncate_block() {
        let b = parse_findings_block(
            "<!-- openab-findings\n{\"findings\":[{\"id\":\"F1\",\"severity\":\"red\",\"title\":\"maps a --> b wrongly\"}]}\n-->\n[done]",
        )
        .unwrap();
        assert_eq!(b.findings[0].title, "maps a --> b wrongly");
    }

    #[test]
    fn empty_findings_list_is_valid() {
        let b =
            parse_findings_block("<!-- openab-findings\n{\"findings\":[]}\n-->\n[done]").unwrap();
        assert!(b.findings.is_empty());
        assert!(b.head_sha.is_none());
    }

    #[test]
    fn malformed_rejects_whole_block() {
        // Bad severity.
        assert!(parse_findings_block(
            "<!-- openab-findings\n{\"findings\":[{\"id\":\"F1\",\"severity\":\"purple\",\"title\":\"x\"}]}\n-->"
        )
        .is_none());
        // Bad status.
        assert!(parse_findings_block(
            "<!-- openab-findings\n{\"findings\":[{\"id\":\"F1\",\"severity\":\"red\",\"status\":\"wontfix\",\"title\":\"x\"}]}\n-->"
        )
        .is_none());
        // Empty id / title.
        assert!(parse_findings_block(
            "<!-- openab-findings\n{\"findings\":[{\"id\":\" \",\"severity\":\"red\",\"title\":\"x\"}]}\n-->"
        )
        .is_none());
        // Broken JSON.
        assert!(parse_findings_block("<!-- openab-findings\n{\"findings\":[\n-->").is_none());
        // Unclosed comment.
        assert!(parse_findings_block("<!-- openab-findings\n{\"findings\":[]}").is_none());
        // No block at all.
        assert!(parse_findings_block("plain verdict [done]").is_none());
    }

    // ---- the span-shaped entrypoint the controller actually calls ----

    #[test]
    fn a_block_split_across_two_messages_still_parses() {
        // The kernel's `block_split_across_two_chair_messages_still_lands_in_ledger`,
        // expressed against `final_messages` instead of the store: the transport
        // split the block opener from its JSON tail (zeabur.com#702 round 4).
        let (part1, part2) = BLOCK.split_at(BLOCK.find("\"findings\":[").unwrap() + 12);
        let parsed = parse_final_messages(&[part1.to_string(), part2.to_string()]);
        let findings = parsed.findings.expect("the joined span carries the block");
        assert_eq!(findings.findings.len(), 2);
        assert_eq!(findings.head_sha.as_deref(), Some("abc123"));
        let trailer = parsed.trailer.expect("the trailer rides the closing part");
        assert_eq!(trailer.decision, "request_changes");
        assert!(trailer.blocking());
    }

    #[test]
    fn the_trailer_comes_from_the_closing_message_only() {
        // The kernel parses the trailer from the closing text, not the joined
        // span, so a trailer left behind in an earlier part is not the verdict.
        let parsed = parse_final_messages(&[
            "draft [[verdict:approve r=0 y=0 g=1]] [done]".to_string(),
            "final report\n[[verdict:request_changes r=1 y=0 g=0]] [done]".to_string(),
        ]);
        assert_eq!(parsed.trailer.unwrap().decision, "request_changes");
    }

    #[test]
    fn halves_fail_independently() {
        // A malformed block must not cost the verdict, and prose-only text
        // parses to neither — the session still closed, we just write less.
        let parsed = parse_final_messages(&[
            "<!-- openab-findings\n{\"findings\":[\n-->\n[[verdict:approve r=0 y=0 g=2]] [done]"
                .to_string(),
        ]);
        assert_eq!(parsed.trailer.unwrap().decision, "approve");
        assert!(parsed.findings.is_none());

        assert_eq!(
            parse_final_messages(&["just prose, no machine parts".to_string()]),
            ParsedResult::default()
        );
        assert_eq!(parse_final_messages(&[]), ParsedResult::default());
    }
}
