//! Convene a review council from a webhook (ROADMAP Phase 2 "Auto-trigger" — the
//! convene half). Mirrors `scripts/open-council.sh --self-fetch` but runs inside the
//! plane: open a session with the standing roster and post a **pointer** trigger
//! (the PR ref, not the diff) so the bots fetch + review the PR with their own `gh`.
//!
//! The plane never calls GitHub (ADR 004 — GitHub I/O belongs to the pods): reviewers
//! self-fetch the diff (`gh pr diff`), the chair posts the verdict with its own `gh`
//! (authenticated as the shared App at the pod level). The trigger is auth-agnostic —
//! identity is whatever the pod's `gh` is logged in as.

use crate::orchestrator;
use crate::state::AppState;
use anyhow::{anyhow, Result};
use std::sync::Arc;

/// Pointer trigger — shared with `scripts/open-council.sh --self-fetch` via
/// `include_str!` so the CI/manual path and the webhook path post identical prompts.
const TRIGGER_TMPL: &str = include_str!("../scripts/pr-review-trigger-pointer.tmpl");

/// Session `trigger_ref` for a PR — also the idempotency key (a re-delivered webhook
/// dedups to the open council). Matches open-council.sh's `REF`.
pub fn pr_trigger_ref(repo: &str, num: u64) -> String {
    format!("github:pr/{repo}#{num}")
}

/// Standing council roster (env `OABCP_COUNCIL_ROSTER`, comma-separated; default
/// matches the seeded `OABCP_BOTS`). `roster[0]` is the chair; the rest review.
pub fn council_roster() -> Vec<String> {
    std::env::var("OABCP_COUNCIL_ROSTER")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec!["chair".into(), "rev1".into(), "rev2".into()])
}

/// Render the pointer trigger. No diff, no title fetch — the plane makes zero GitHub
/// calls; the bots pull what they need. `{{TITLE}}` is left blank (cosmetic; the bots
/// see the real title when they fetch the PR).
pub fn render_trigger(repo: &str, num: u64) -> String {
    TRIGGER_TMPL
        .replace("{{REPO}}", repo)
        .replace("{{NUM}}", &num.to_string())
        .replace("{{TITLE}}", "")
        .replace("{{ANGLE_ASSIGNMENT}}", "") // preset/angle assignment is the next step
}

/// Convene a council for a PR: open a session with the standing roster (chair =
/// roster[0], quorum = all reviewers) and post the pointer trigger so the bots start.
/// Returns the new session id. No GitHub I/O happens here.
pub async fn convene_for_pr(state: &Arc<AppState>, repo: &str, num: u64) -> Result<String> {
    let roster = council_roster();
    if roster.is_empty() {
        return Err(anyhow!("empty council roster"));
    }
    let quorum = (roster.len() as i64 - 1).max(0); // all reviewers must report
    let trigger_ref = pr_trigger_ref(repo, num);
    let session = state.store.create_session(
        "council",
        Some(&trigger_ref),
        quorum,
        Some(&roster[0]),
        &roster,
        "council",
    )?;
    let trigger = render_trigger(repo, num);
    orchestrator::post_client_message(state, &session.id, &trigger)?;
    Ok(session.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_ref_is_stable_and_matches_open_council() {
        assert_eq!(pr_trigger_ref("o/r", 7), "github:pr/o/r#7");
    }

    #[test]
    fn render_trigger_fills_ref_and_inlines_no_diff() {
        let t = render_trigger("canyugs/ocp", 7);
        assert!(t.contains("canyugs/ocp #7"));
        // pointer trigger tells bots to self-fetch; the diff is NOT inlined
        assert!(t.contains("gh pr diff 7 --repo canyugs/ocp"));
        assert!(!t.contains("===== DIFF ====="));
        assert!(!t.contains("{{"));
    }

    #[test]
    fn roster_default_matches_seeded_bots() {
        std::env::remove_var("OABCP_COUNCIL_ROSTER");
        assert_eq!(council_roster(), vec!["chair", "rev1", "rev2"]);
    }
}
