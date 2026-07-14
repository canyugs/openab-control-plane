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
            && matches!(f.status.as_str(), "open" | "resolved" | "dismissed")
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
    fn review_close_populates_findings_ledger() {
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let session = store
            .create_session(
                "review",
                Some("github:pr/o/r#7"),
                0,
                Some(&chair.id),
                std::slice::from_ref(&chair.id),
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

        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(&session.id, BLOCK),
        )
        .unwrap();

        let rows = store
            .review_findings(Some("o/r"), Some(7), None, None, 10)
            .unwrap();
        assert_eq!(rows.len(), 2);
        // Newest-first: F2 has the higher rowid.
        let f1 = rows.iter().find(|r| r.stable_id == "F1").unwrap();
        assert_eq!(f1.session_id, session.id);
        assert_eq!((f1.severity.as_str(), f1.status.as_str()), ("red", "open"));
        assert_eq!(f1.head_sha.as_deref(), Some("abc123"));
        assert_eq!(f1.raised_by.as_deref(), Some("rev1"));
        assert_eq!(f1.angle.as_deref(), Some("correctness"));
        assert_eq!((f1.path.as_deref(), f1.line), (Some("src/a.rs"), Some(7)));
        assert_eq!(
            rows.iter()
                .find(|r| r.stable_id == "F2")
                .map(|r| r.status.as_str()),
            Some("resolved")
        );

        // Filters narrow.
        assert_eq!(
            store
                .review_findings(None, None, Some("resolved"), None, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .review_findings(Some("other/repo"), None, None, None, 10)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn solo_close_writes_no_ledger_rows_even_with_block() {
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let bot = store.register_bot("solo", "reviewer", "h1", "t1").unwrap();
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

        crate::orchestrator::handle_reply(
            &state,
            &bot.id,
            crate::orchestrator::test_support::msg_reply(&session.id, BLOCK),
        )
        .unwrap();

        assert!(store
            .review_findings(None, None, None, None, 10)
            .unwrap()
            .is_empty());
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
