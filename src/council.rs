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

use crate::controller::{self, ControllerAction, ControllerActionResult, OpenSessionAction};
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

/// Env/default standing council roster (`OABCP_COUNCIL_ROSTER`, comma-separated;
/// default matches the seeded `OABCP_BOTS`). `roster[0]` is the chair; the rest
/// review. A runtime DB override may replace this via `runtime_council_roster`.
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

/// Effective standing roster used by webhook/ask convene paths. A DB override
/// lets operators replace bots without restarting the control-plane; env remains
/// the fallback and bootstrap source.
pub fn runtime_council_roster(state: &Arc<AppState>) -> Result<(Vec<String>, &'static str)> {
    match state.store.standing_roster()? {
        Some(roster) => Ok((roster, "override")),
        None => Ok((council_roster(), "env")),
    }
}

/// Default preset when neither a PR label nor the global env selects one.
const DEFAULT_PRESET: &str = "lite";

/// Review angles per preset (1 / 3 / 5 / 7 angles). Mirrors `scripts/open-council.sh`.
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

/// Global default preset from env `OABCP_COUNCIL_PRESET` (lite|quick|standard|full).
/// `None` → fall through to `DEFAULT_PRESET`.
fn council_preset() -> Option<String> {
    std::env::var("OABCP_COUNCIL_PRESET")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the preset for one convene: a per-PR `review:<preset>` label wins, then the
/// global env, then `DEFAULT_PRESET` (lite). Unknown values are warned and skipped so
/// resolution always lands on a valid preset.
fn pick_preset(label_preset: Option<&str>) -> String {
    if let Some(l) = label_preset {
        if preset_angles(l).is_some() {
            return l.to_string();
        }
        tracing::warn!(label = %l, "unknown review:<preset> label; ignoring");
    }
    if let Some(e) = council_preset() {
        if preset_angles(&e).is_some() {
            return e;
        }
        tracing::warn!(preset = %e, "unknown OABCP_COUNCIL_PRESET (want lite|quick|standard|full); using default");
    }
    DEFAULT_PRESET.to_string()
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
    let text = format!("Review focus assignment:\n{}", lines.join("\n"));
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
pub async fn convene_for_pr(
    state: &Arc<AppState>,
    repo: &str,
    num: u64,
    label_preset: Option<String>,
) -> Result<String> {
    let (roster, _) = runtime_council_roster(state)?;
    let action = review_open_session_action_with_roster(repo, num, label_preset, roster)?;
    let result = controller::execute(state, ControllerAction::OpenSession(action))?;
    Ok(session_id(result))
}

#[cfg(test)]
fn review_open_session_action(
    repo: &str,
    num: u64,
    label_preset: Option<String>,
) -> Result<OpenSessionAction> {
    review_open_session_action_with_roster(repo, num, label_preset, council_roster())
}

fn review_open_session_action_with_roster(
    repo: &str,
    num: u64,
    label_preset: Option<String>,
    roster: Vec<String>,
) -> Result<OpenSessionAction> {
    if roster.is_empty() {
        return Err(anyhow!("empty council roster"));
    }
    // Preset (per-PR label > global env > lite) assigns angles to reviewers, trims
    // idle ones, and sets quorum to the participating reviewers.
    let preset = pick_preset(label_preset.as_deref());
    let angles = preset_angles(&preset).expect("pick_preset returns a valid preset");
    let (eff_roster, quorum, assignment) = assign_angles(&roster, &angles);
    tracing::info!(preset = %preset, quorum, "convene preset resolved");
    let trigger_ref = pr_trigger_ref(repo, num);
    let trigger = render_trigger(repo, num, &assignment);
    let chair_bot = eff_roster
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("assign_angles produced empty roster"))?;
    // A lone-bot roster has no reviewers, so review_council's chair would wait
    // forever for a reviewer quorum that can't arrive (C4). Route it to solo, where
    // the bot's own done closes the session — it self-reviews and posts the verdict.
    let mode = if eff_roster.len() > 1 { "review_council" } else { "solo" };
    Ok(OpenSessionAction {
        title: "council".into(),
        trigger_ref: Some(trigger_ref),
        roster: eff_roster,
        quorum_n: quorum,
        chair_bot: Some(chair_bot),
        mode: mode.into(),
        prompt: trigger,
    })
}

// --- Conversational follow-up (ADR 011) ---------------------------------------

/// Ask pointer trigger — shared shape with the review trigger, but for a single bot
/// answering a question and posting a NEW comment (not the edit-last verdict).
const ASK_TRIGGER_TMPL: &str = include_str!("../scripts/pr-ask-trigger-pointer.tmpl");

/// Session `trigger_ref` for a follow-up ask — comment-scoped so a re-delivered
/// `issue_comment` webhook dedups. Distinct namespace from the PR-level review ref
/// (`github:pr/…`) so an ask never collides with the review session.
pub fn pr_ask_trigger_ref(repo: &str, num: u64, comment_id: Option<u64>) -> String {
    match comment_id {
        Some(id) => format!("github:ask/{repo}#{num}@{id}"),
        None => format!("github:ask/{repo}#{num}"),
    }
}

/// Render the ask pointer trigger: the PR ref + the user's question. No diff/thread
/// inlined — the bot self-fetches (ADR 004).
pub fn render_ask_trigger(repo: &str, num: u64, question: &str) -> String {
    ASK_TRIGGER_TMPL
        .replace("{{REPO}}", repo)
        .replace("{{NUM}}", &num.to_string())
        .replace("{{QUESTION}}", question)
}

/// Answer a follow-up on a PR with a **solo** session (ADR 011): one bot (the chair —
/// the only writer) self-fetches the PR + thread, answers, and posts a NEW comment.
/// Cheaper than a council and the right shape for a single answer; no GitHub I/O here.
/// The controller dedups by the comment-scoped trigger ref, so webhook retries return
/// the active solo session instead of opening duplicate answers for the same comment.
pub async fn convene_ask(
    state: &Arc<AppState>,
    repo: &str,
    num: u64,
    question: &str,
    comment_id: Option<u64>,
) -> Result<String> {
    let (roster, _) = runtime_council_roster(state)?;
    let action = ask_open_session_action_with_roster(repo, num, question, comment_id, roster)?;
    let result = controller::execute(state, ControllerAction::OpenSession(action))?;
    Ok(session_id(result))
}

fn ask_open_session_action_with_roster(
    repo: &str,
    num: u64,
    question: &str,
    comment_id: Option<u64>,
    roster: Vec<String>,
) -> Result<OpenSessionAction> {
    let chair = roster
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("empty council roster"))?;
    let trigger_ref = pr_ask_trigger_ref(repo, num, comment_id);
    let trigger = render_ask_trigger(repo, num, question);
    Ok(OpenSessionAction {
        title: "ask".into(),
        trigger_ref: Some(trigger_ref),
        roster: std::slice::from_ref(&chair).to_vec(),
        quorum_n: 0,
        chair_bot: Some(chair),
        mode: "solo".into(),
        prompt: trigger,
    })
}

fn session_id(result: ControllerActionResult) -> String {
    match result {
        ControllerActionResult::SessionOpened { session_id, .. } => session_id,
    }
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
        assert!(t.contains("recipient-specific task"));
        assert!(!t.contains("Role gate"));
        assert!(!t.contains("If your bot name"));
        assert!(!t.contains("OpenAB Council review started"));
        assert!(!t.contains("===== DIFF ====="));
        assert!(!t.contains("{{"));
    }

    #[test]
    fn trigger_templates_carry_no_role_protocol() {
        let pointer = render_trigger("canyugs/ocp", 7, "");
        let inline = include_str!("../scripts/pr-review-trigger.tmpl")
            .replace("{{REPO}}", "canyugs/ocp")
            .replace("{{NUM}}", "7")
            .replace("{{TITLE}}", "")
            .replace("{{ANGLE_ASSIGNMENT}}", "")
            .replace("{{DIFF}}", "diff --git a/src/lib.rs b/src/lib.rs");

        for t in [pointer, inline] {
            assert!(!t.contains("--edit-last"));
            assert!(!t.contains("gh pr review"));
            assert!(!t.contains("[[verdict:"));
            assert!(!t.contains("{{"));
        }
    }

    #[test]
    fn review_session_uses_review_council_mode() {
        let action = review_open_session_action("o/r", 1, None).unwrap();
        assert_eq!(action.mode, "review_council");
    }

    #[test]
    fn lone_bot_roster_uses_solo_mode_not_hanging_council() {
        // C4: a 1-bot roster has no reviewers, so review_council would hang the chair
        // on an unreachable quorum. It must open as solo (own done closes it).
        let action =
            review_open_session_action_with_roster("o/r", 1, None, vec!["chair".into()]).unwrap();
        assert_eq!(action.mode, "solo");
        assert_eq!(action.roster, vec!["chair"]);
        assert_eq!(action.quorum_n, 0);
        assert_eq!(action.chair_bot.as_deref(), Some("chair"));
    }

    #[test]
    fn render_trigger_includes_angle_assignment() {
        let t = render_trigger("o/r", 1, "- rev1 → security");
        assert!(t.contains("- rev1 → security"));
    }

    #[test]
    fn preset_angles_scale_1_3_5_7() {
        assert_eq!(preset_angles("lite").map(|v| v.len()), Some(1));
        assert_eq!(preset_angles("quick").map(|v| v.len()), Some(3));
        assert_eq!(preset_angles("standard").map(|v| v.len()), Some(5));
        assert_eq!(preset_angles("full").map(|v| v.len()), Some(7));
        assert!(preset_angles("QUICK").is_none()); // case-sensitive
        assert!(preset_angles("stanard").is_none()); // typo
        assert!(preset_angles("").is_none());
    }

    #[test]
    fn pick_preset_label_over_env_over_default() {
        std::env::remove_var("OABCP_COUNCIL_PRESET");
        // no label, no env → default (lite)
        assert_eq!(pick_preset(None), "lite");
        // valid label wins
        assert_eq!(pick_preset(Some("full")), "full");
        // unknown label ignored → default
        assert_eq!(pick_preset(Some("bogus")), "lite");
        // env override when no label
        std::env::set_var("OABCP_COUNCIL_PRESET", "standard");
        assert_eq!(pick_preset(None), "standard");
        // label still beats env
        assert_eq!(pick_preset(Some("quick")), "quick");
        std::env::remove_var("OABCP_COUNCIL_PRESET");
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

    #[test]
    fn ask_trigger_ref_is_comment_scoped() {
        assert_eq!(
            pr_ask_trigger_ref("o/r", 7, Some(555)),
            "github:ask/o/r#7@555"
        );
        assert_eq!(pr_ask_trigger_ref("o/r", 7, None), "github:ask/o/r#7");
        // distinct namespace from the review ref so they never collide
        assert_ne!(pr_ask_trigger_ref("o/r", 7, None), pr_trigger_ref("o/r", 7));
    }

    #[test]
    fn render_ask_trigger_carries_question_and_self_fetch() {
        let t = render_ask_trigger("canyugs/ocp", 7, "why is this a P1?");
        assert!(t.contains("canyugs/ocp #7"));
        assert!(t.contains("why is this a P1?"));
        // self-fetch (no inlined diff) + a NEW comment (not the edit-last verdict)
        assert!(t.contains("gh pr view 7 --repo canyugs/ocp --comments"));
        assert!(t.contains("gh pr comment 7 --repo canyugs/ocp --body-file"));
        assert!(t.contains("NEW comment"));
        // must not reuse the review verdict's edit-in-place comment signature
        assert!(!t.contains("--create-if-none"));
        assert!(!t.contains("{{"));
    }

    #[test]
    fn ask_trigger_carries_rereview_redirect() {
        let t = render_ask_trigger("canyugs/ocp", 7, "please review again");
        assert!(t.contains(
            "If the question asks for a re-review or another review round, answer: push new commits or comment `/review` to trigger a re-review round."
        ));
    }
}
