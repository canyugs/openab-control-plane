//! Convene a review council from a webhook (ROADMAP Phase 2 "Auto-trigger" — the
//! convene half). Mirrors `scripts/open-council.sh --self-fetch` but runs inside the
//! plane: open a session with the standing roster and post a **pointer** trigger
//! (the PR ref, optional angle assignment — not the diff) so the bots fetch + review
//! the PR with their own `gh`.
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

/// Review angles per preset (mirrors `scripts/open-council.sh`).
fn preset_angles(preset: &str) -> Option<Vec<&'static str>> {
    match preset {
        "quick" => Some(vec!["correctness", "security", "integration"]),
        "standard" => Some(vec!["correctness", "architecture", "security", "testing", "docs"]),
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

/// Selected preset from env `OABCP_COUNCIL_PRESET` (quick|standard|full). `None` →
/// generic review: every reviewer covers everything (today's default). Mirrors the
/// opt-in `--preset` flag on `open-council.sh`.
fn council_preset() -> Option<String> {
    std::env::var("OABCP_COUNCIL_PRESET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Assign angles round-robin onto the reviewers (roster minus chair), mirroring
/// `open-council.sh --preset`: angles ≤ reviewers → the first N reviewers take one
/// each and the extras sit out (trimmed from the session roster so quorum doesn't
/// wait on idle bots); angles > reviewers → all reviewers, some covering several.
/// Returns (effective_roster, quorum_n, assignment_text); empty text if no reviewers.
fn assign_angles(roster: &[String], angles: &[&str]) -> (Vec<String>, i64, String) {
    let reviewers = &roster[1..];
    if reviewers.is_empty() {
        return (roster.to_vec(), 0, String::new());
    }
    let participating: Vec<String> = if angles.len() <= reviewers.len() {
        reviewers[..angles.len()].to_vec()
    } else {
        reviewers.to_vec()
    };
    let mut assigned: Vec<Vec<&str>> = vec![Vec::new(); participating.len()];
    for (i, a) in angles.iter().enumerate() {
        assigned[i % participating.len()].push(a);
    }
    let lines: Vec<String> = participating
        .iter()
        .zip(&assigned)
        .map(|(r, a)| format!("- {} → {}", r, a.join(", ")))
        .collect();
    let text = format!(
        "Angle assignment — cover ONLY the angle(s) on the row matching your bot name; ignore the rest:\n{}",
        lines.join("\n")
    );
    let mut eff = vec![roster[0].clone()];
    eff.extend(participating);
    let quorum = (eff.len() as i64 - 1).max(0);
    (eff, quorum, text)
}

/// Render the pointer trigger. No diff, no title fetch — the plane makes zero GitHub
/// calls; the bots pull what they need. `{{TITLE}}` is left blank (cosmetic; the bots
/// see the real title when they fetch the PR). `angle_assignment` is the preset block
/// (empty = generic review, no angles).
pub fn render_trigger(repo: &str, num: u64, angle_assignment: &str) -> String {
    TRIGGER_TMPL
        .replace("{{REPO}}", repo)
        .replace("{{NUM}}", &num.to_string())
        .replace("{{TITLE}}", "")
        .replace("{{ANGLE_ASSIGNMENT}}", angle_assignment)
}

/// Convene a council for a PR: open a session with the standing roster (chair =
/// roster[0]; optional preset trims reviewers + sets quorum) and post the pointer
/// trigger so the bots start. Returns the new session id. No GitHub I/O happens here.
pub async fn convene_for_pr(state: &Arc<AppState>, repo: &str, num: u64) -> Result<String> {
    let roster = council_roster();
    if roster.is_empty() {
        return Err(anyhow!("empty council roster"));
    }
    // Optional preset: assign angles to reviewers (trim idle ones, quorum = those
    // assigned). No preset → generic review, all reviewers report.
    let generic = || (roster.clone(), (roster.len() as i64 - 1).max(0), String::new());
    let (eff_roster, quorum, assignment) = match council_preset() {
        None => generic(),
        Some(p) => match preset_angles(&p) {
            Some(angles) => assign_angles(&roster, &angles),
            None => {
                tracing::warn!(preset = %p, "unknown OABCP_COUNCIL_PRESET (want quick|standard|full); falling back to generic review");
                generic()
            }
        },
    };
    let trigger_ref = pr_trigger_ref(repo, num);
    let session = state.store.create_session(
        "council",
        Some(&trigger_ref),
        quorum,
        Some(&eff_roster[0]),
        &eff_roster,
        "council",
    )?;
    let trigger = render_trigger(repo, num, &assignment);
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
    fn render_trigger_is_pointer_with_no_inlined_diff() {
        let t = render_trigger("canyugs/ocp", 7, "");
        assert!(t.contains("canyugs/ocp #7"));
        // pointer trigger tells bots to self-fetch; the diff is NOT inlined
        assert!(t.contains("gh pr diff 7 --repo canyugs/ocp"));
        assert!(!t.contains("===== DIFF ====="));
        assert!(!t.contains("{{"));
    }

    #[test]
    fn render_trigger_includes_angle_assignment() {
        let t = render_trigger("o/r", 1, "- rev1 → security");
        assert!(t.contains("- rev1 → security"));
    }

    #[test]
    fn preset_angles_known_and_unknown() {
        assert_eq!(preset_angles("quick").map(|v| v.len()), Some(3));
        assert_eq!(preset_angles("standard").map(|v| v.len()), Some(5));
        assert_eq!(preset_angles("full").map(|v| v.len()), Some(7));
        assert!(preset_angles("QUICK").is_none()); // case-sensitive
        assert!(preset_angles("stanard").is_none()); // typo
        assert!(preset_angles("").is_none());
    }

    #[test]
    fn assign_angles_round_robin_trim_and_solo() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // quick (3 angles) over 2 reviewers → round-robin: rev1 gets 2, rev2 gets 1; quorum 2
        let (eff, q, text) = assign_angles(
            &s(&["chair", "rev1", "rev2"]),
            &["correctness", "security", "integration"],
        );
        assert_eq!(eff, vec!["chair", "rev1", "rev2"]);
        assert_eq!(q, 2);
        assert!(text.contains("rev1 → correctness, integration"));
        assert!(text.contains("rev2 → security"));

        // 1 angle over 2 reviewers → rev2 sits out (trimmed); quorum 1
        let (eff, q, _) = assign_angles(&s(&["chair", "rev1", "rev2"]), &["correctness"]);
        assert_eq!(eff, vec!["chair", "rev1"]);
        assert_eq!(q, 1);

        // solo (no reviewers) → preset is a no-op
        let (eff, q, text) = assign_angles(&s(&["chair"]), &["correctness", "security"]);
        assert_eq!(eff, vec!["chair"]);
        assert_eq!(q, 0);
        assert!(text.is_empty());
    }

    #[test]
    fn roster_default_matches_seeded_bots() {
        std::env::remove_var("OABCP_COUNCIL_ROSTER");
        assert_eq!(council_roster(), vec!["chair", "rev1", "rev2"]);
    }
}
