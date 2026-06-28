//! Convene a review council from a webhook (ROADMAP Phase 2 "Auto-trigger" — the
//! convene half). Mirrors `scripts/open-council.sh` but runs inside the plane: read
//! the PR title + diff, open a session with the standing roster, and post the trigger
//! so the bots start reviewing. The chair posts the verdict from its own pod
//! (`GH_TOKEN`); this only *reads* the diff and kicks off deliberation.
//!
//! Minimal wiring: standing roster, no preset/angle assignment yet (that's the next
//! Phase-2 step). The diff is read via the App installation token when configured,
//! else the pod's `GH_TOKEN` — so it works before App env is provisioned and upgrades
//! to App identity automatically once it is.

use crate::github_app::Role;
use crate::orchestrator;
use crate::state::AppState;
use anyhow::{anyhow, Context, Result};
use std::sync::Arc;

/// Shared with `scripts/open-council.sh` (single source of truth) so the CI/PAT
/// path and the webhook path produce identical review prompts.
const TRIGGER_TMPL: &str = include_str!("../scripts/pr-review-trigger.tmpl");

/// Session `trigger_ref` for a PR — also the idempotency key (so a re-delivered
/// webhook dedups to the open council). Matches open-council.sh's `REF`.
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

/// Render the PR-review trigger from the shared template. `angle_assignment` is the
/// preset assignment block (empty string = generic review, no angles).
pub fn render_trigger(repo: &str, num: u64, title: &str, diff: &str, angle_assignment: &str) -> String {
    TRIGGER_TMPL
        .replace("{{REPO}}", repo)
        .replace("{{NUM}}", &num.to_string())
        .replace("{{TITLE}}", title)
        .replace("{{ANGLE_ASSIGNMENT}}", angle_assignment)
        .replace("{{DIFF}}", diff)
}

/// A GitHub read token for fetching the PR: the App installation token (read scope)
/// if the App is configured, else the pod's `GH_TOKEN`.
async fn read_token(state: &AppState) -> Result<String> {
    if let Some(app) = state.github_app.as_ref() {
        Ok(app.mint_installation_token(Role::Reviewer).await?.token)
    } else {
        std::env::var("GH_TOKEN").context("no GitHub App configured and GH_TOKEN unset")
    }
}

/// Fetch a PR's title (JSON) and unified diff.
async fn fetch_pr(token: &str, repo: &str, num: u64) -> Result<(String, String)> {
    let client = reqwest::Client::new();
    let url = format!("https://api.github.com/repos/{repo}/pulls/{num}");
    let auth = |r: reqwest::RequestBuilder| {
        r.header("Authorization", format!("Bearer {token}"))
            .header("User-Agent", "openab-control-plane")
            .header("X-GitHub-Api-Version", "2022-11-28")
    };
    let meta: serde_json::Value = auth(client.get(&url).header("Accept", "application/vnd.github+json"))
        .send()
        .await?
        .error_for_status()
        .context("GET pull (meta)")?
        .json()
        .await?;
    let title = meta["title"].as_str().unwrap_or("").to_string();
    let diff = auth(client.get(&url).header("Accept", "application/vnd.github.v3.diff"))
        .send()
        .await?
        .error_for_status()
        .context("GET pull (diff)")?
        .text()
        .await?;
    Ok((title, diff))
}

/// Convene a council for a PR: read the diff, open a session with the standing
/// roster (chair = roster[0], quorum = all reviewers), and post the trigger so the
/// bots start. Returns the new session id.
pub async fn convene_for_pr(state: &Arc<AppState>, repo: &str, num: u64) -> Result<String> {
    let token = read_token(state).await?;
    let (title, diff) = fetch_pr(&token, repo, num).await.context("fetch PR")?;
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
    let trigger = render_trigger(repo, num, &title, &diff, &assignment);
    orchestrator::post_client_message(state, &session.id, &trigger)
        .context("post trigger message")?;
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
    fn render_trigger_fills_placeholders() {
        let t = render_trigger("canyugs/ocp", 7, "Fix bug", "diff --git a b", "");
        assert!(t.contains("canyugs/ocp #7 \"Fix bug\""));
        assert!(t.contains("diff --git a b"));
        // no leftover placeholders
        assert!(!t.contains("{{"));
    }

    #[test]
    fn preset_angles_known_and_unknown() {
        assert_eq!(preset_angles("quick").map(|v| v.len()), Some(3));
        assert_eq!(preset_angles("standard").map(|v| v.len()), Some(5));
        assert_eq!(preset_angles("full").map(|v| v.len()), Some(7));
        // unrecognized → None → caller falls back to generic review (with a warn)
        assert!(preset_angles("QUICK").is_none()); // case-sensitive
        assert!(preset_angles("stanard").is_none()); // typo
        assert!(preset_angles("").is_none());
    }

    #[test]
    fn assign_angles_round_robin_trim_and_solo() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // quick (3 angles) over 2 reviewers → round-robin: rev1 gets 2, rev2 gets 1; quorum 2
        let (eff, q, text) = assign_angles(&s(&["chair", "rev1", "rev2"]), &["correctness", "security", "integration"]);
        assert_eq!(eff, vec!["chair", "rev1", "rev2"]);
        assert_eq!(q, 2);
        assert!(text.contains("rev1 → correctness, integration"));
        assert!(text.contains("rev2 → security"));

        // 3 angles over 5 reviewers → first 3 review one each, rev4/rev5 trimmed; quorum 3
        let (eff2, q2, _) = assign_angles(
            &s(&["chair", "rev1", "rev2", "rev3", "rev4", "rev5"]),
            &["correctness", "security", "integration"],
        );
        assert_eq!(eff2, vec!["chair", "rev1", "rev2", "rev3"]);
        assert_eq!(q2, 3);

        // solo (no reviewers) → no-op
        let (eff3, q3, text3) = assign_angles(&s(&["chair"]), &["correctness"]);
        assert_eq!(eff3, vec!["chair"]);
        assert_eq!(q3, 0);
        assert!(text3.is_empty());
    }

    #[test]
    fn roster_default_matches_seeded_bots() {
        // (env-independent default; OABCP_COUNCIL_ROSTER overrides at runtime)
        std::env::remove_var("OABCP_COUNCIL_ROSTER");
        assert_eq!(council_roster(), vec!["chair", "rev1", "rev2"]);
    }
}
