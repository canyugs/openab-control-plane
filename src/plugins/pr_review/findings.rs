//! ADR 020 findings ledger — machine input side.
//!
//! The chair's final thread message may carry a hidden structured block:
//!
//! ```markdown
//! <!-- openab-findings
//! {"head_sha":"abc123","findings":[
//!   {"id":"F1","severity":"red","status":"open","title":"…","path":"src/x.rs",
//!    "line":42,"raised_by":"rev1","angle":"correctness"}]}
//! -->
//! ```
//!
//! Markdown stays the human report; this block is the machine source of truth
//! (ADR 020 "Input format"). Parsing is all-or-nothing like the verdict
//! trailer: any malformed or invalid entry rejects the whole block, and the
//! session still closes normally — the ledger just gets no rows.

use serde::Deserialize;

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
    /// Reviewer bot that raised it (or `chair`), when known.
    #[serde(default)]
    pub raised_by: Option<String>,
    /// Review angle the finding is attributed to (per-angle SNR, ADR 021 D3).
    #[serde(default)]
    pub angle: Option<String>,
    /// ADR 035 P2: set when `status` is `waived` — the ledger row this
    /// finding matched. Drives the fired counters, nothing else.
    #[serde(default)]
    pub waiver_id: Option<String>,
}

fn default_status() -> String {
    "open".to_string()
}

/// Extract and validate the last `<!-- openab-findings … -->` block in `text`.
/// Last block wins (the chair may have quoted an earlier draft). Returns None
/// on any malformed JSON or invalid enum value — never a partial parse.
pub fn parse_findings_block(text: &str) -> Option<FindingsBlock> {
    const OPEN: &str = "<!-- openab-findings";
    let start = text.rfind(OPEN)?;
    let rest = &text[start + OPEN.len()..];
    // A literal `-->` inside a title/path would truncate the JSON at the first
    // occurrence — try each `-->` candidate until one yields valid JSON.
    let block: FindingsBlock = rest
        .match_indices("-->")
        .find_map(|(end, _)| serde_json::from_str(rest[..end].trim()).ok())?;
    let valid = block.findings.iter().all(|f| {
        matches!(f.severity.as_str(), "red" | "yellow" | "green")
            // `waived` since ADR 035 P2 — a finding matched to an operator
            // waiver at synthesis; carries `waiver_id` for the fired counter.
            && matches!(f.status.as_str(), "open" | "resolved" | "dismissed" | "waived")
            && !f.id.trim().is_empty()
            && !f.title.trim().is_empty()
    });
    valid.then_some(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::store::Store as _;

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
    fn controller_council_close_bumps_waiver_counters() {
        // Controller-opened sessions use the generic "council" mode with a
        // hashed trigger_ref — the close hook must still parse the chair's
        // block (found live: OCP#333 round 2 landed "waived" on GitHub while
        // the fired counter stayed empty). The findings LEDGER now lives on
        // the controller; the waiver bump is what remains kernel-side.
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let waiver = store
            .create_review_waiver(
                "o/r",
                None,
                "accepted eval trade-off",
                None,
                "operator",
                crate::store::now_ms() + 86_400_000,
            )
            .unwrap();
        let session = store
            .create_session(
                "review",
                Some("controller:github-canary:deadbeef"),
                0,
                Some(&chair.id),
                std::slice::from_ref(&chair.id),
                "council",
            )
            .unwrap();
        store
            .advance_state(
                &session.id,
                crate::store::SessionState::Open,
                crate::store::SessionState::Quorum,
            )
            .unwrap();

        let verdict = format!(
            "report\n<!-- openab-findings\n{{\"findings\":[\
             {{\"id\":\"F1\",\"severity\":\"yellow\",\"status\":\"waived\",\
              \"title\":\"waived eval\",\"waiver_id\":\"{}\"}}]}}\n-->\n\
             [[verdict:approve r=0 y=0 g=0]] [done]",
            waiver.id
        );
        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(&session.id, &verdict),
        )
        .unwrap();

        let waivers = store
            .list_review_waivers(Some("o/r"), true, crate::store::now_ms())
            .unwrap();
        assert_eq!(waivers[0].fired_count, 1, "hashed-ref council close bumps");
    }

    #[test]
    fn block_split_across_two_chair_messages_still_bumps_waivers() {
        // Live failure (zeabur.com#702 round 4): the transport's message-length
        // split put the block opener in one chair message and the JSON tail +
        // verdict in the next. The close must parse the joined settled span —
        // with the ledger on the controller, the kernel-side casualty of
        // getting this wrong is a silently skipped waiver bump.
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let waiver = store
            .create_review_waiver(
                "o/r",
                None,
                "split-block trade-off",
                None,
                "operator",
                crate::store::now_ms() + 86_400_000,
            )
            .unwrap();
        let session = store
            .create_session(
                "review",
                Some("controller:github-canary:cafef00d"),
                0,
                Some(&chair.id),
                std::slice::from_ref(&chair.id),
                "council",
            )
            .unwrap();
        store
            .advance_state(
                &session.id,
                crate::store::SessionState::Open,
                crate::store::SessionState::Quorum,
            )
            .unwrap();

        let verdict = format!(
            "report\n<!-- openab-findings\n{{\"findings\":[\
             {{\"id\":\"F1\",\"severity\":\"yellow\",\"status\":\"waived\",\
              \"title\":\"waived across the split\",\"waiver_id\":\"{}\"}}]}}\n-->\n\
             [[verdict:approve r=0 y=0 g=0]] [done]",
            waiver.id
        );
        let (part1, part2) = verdict.split_at(verdict.find("\"findings\":[").unwrap() + 12);
        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(&session.id, part1),
        )
        .unwrap();
        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(&session.id, part2),
        )
        .unwrap();

        let waivers = store
            .list_review_waivers(Some("o/r"), true, crate::store::now_ms())
            .unwrap();
        assert_eq!(waivers[0].fired_count, 1, "joined-span parse still fires");
    }

    #[test]
    fn solo_close_does_not_bump_waivers_even_with_block() {
        // The harvest gate is "council mode with a chair": a solo session's
        // block must not fire waiver counters (it used to be "must not write
        // ledger rows" — the ledger moved to the controller, the gate stays).
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let bot = store.register_bot("solo", "reviewer", "h1", "t1").unwrap();
        let waiver = store
            .create_review_waiver(
                "o/r",
                None,
                "should not fire",
                None,
                "operator",
                crate::store::now_ms() + 86_400_000,
            )
            .unwrap();
        let session = store
            .create_session("solo", None, 0, None, std::slice::from_ref(&bot.id), "solo")
            .unwrap();
        store
            .advance_state(
                &session.id,
                crate::store::SessionState::Open,
                crate::store::SessionState::Deliberating,
            )
            .unwrap();

        let block = format!(
            "<!-- openab-findings\n{{\"findings\":[\
             {{\"id\":\"F1\",\"severity\":\"yellow\",\"status\":\"waived\",\
              \"title\":\"x\",\"waiver_id\":\"{}\"}}]}}\n-->\n[done]",
            waiver.id
        );
        crate::orchestrator::handle_reply(
            &state,
            &bot.id,
            crate::orchestrator::test_support::msg_reply(&session.id, &block),
        )
        .unwrap();

        let waivers = store
            .list_review_waivers(Some("o/r"), true, crate::store::now_ms())
            .unwrap();
        assert_eq!(waivers[0].fired_count, 0, "solo close must not harvest");
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
}
