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
const TRIGGER_TMPL: &str = include_str!("../../../scripts/pr-review-trigger-pointer.tmpl");

pub(super) const REREVIEW_CONTEXT_START: &str = "===== RE-REVIEW CONTEXT =====";
pub(super) const REREVIEW_CONTEXT_END: &str = "===== END RE-REVIEW CONTEXT =====";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRereviewContext {
    /// Prior review round head, carried only when the prior fingerprint was `sha:<sha>`.
    pub base_sha: Option<String>,
    /// Author fix notes from `@handle review [notes]`. `Some("")` preserves a bare command.
    pub author_notes: Option<String>,
    /// `@handle full review`: omit the delta header and tell bots to review from scratch.
    pub from_scratch: bool,
}

/// Session `trigger_ref` for a PR — also the idempotency key (a re-delivered webhook
/// dedups to the open council). Matches open-council.sh's `REF`.
pub fn pr_trigger_ref(repo: &str, num: u64) -> String {
    format!("github:pr/{repo}#{num}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAdmission {
    Allow,
    Deduped {
        session_id: String,
        reason: String,
    },
    Refused {
        session_id: Option<String>,
        reason: String,
    },
}

fn emit_review_refusal(
    state: &Arc<AppState>,
    session_id: Option<&str>,
    trigger_ref: &str,
    reason: &str,
    count: usize,
    limit: usize,
) {
    state.emit_north(
        "github_review_refused",
        session_id.unwrap_or("-"),
        serde_json::json!({
            "trigger_ref": trigger_ref,
            "reason": reason,
            "count": count,
            "limit": limit,
        }),
    );
}

/// Review cost valves. Checked before the supersede txn so a refusal never closes
/// the active round. Hourly caps only auto `synchronize`; explicit commands bypass
/// that cap but all review paths obey the per-PR round budget.
pub fn check_review_admission(
    state: &Arc<AppState>,
    trigger_ref: &str,
    is_synchronize: bool,
) -> Result<ReviewAdmission> {
    let hourly_cap = state.pr_review_config.review_hourly_cap;
    let round_budget = state.pr_review_config.review_round_budget;
    let limit = hourly_cap.max(round_budget).max(1);
    let sessions = state.store.list_sessions(Some(trigger_ref), None, limit)?;
    let active = state.store.active_session_for_trigger(trigger_ref)?;

    if round_budget == 0 || sessions.len() >= round_budget {
        tracing::warn!(
            trigger_ref,
            count = sessions.len(),
            limit = round_budget,
            "review round budget exhausted; refusing convene"
        );
        emit_review_refusal(
            state,
            active.as_deref(),
            trigger_ref,
            "round_budget",
            sessions.len(),
            round_budget,
        );
        return Ok(ReviewAdmission::Refused {
            session_id: active,
            reason: "round_budget".into(),
        });
    }

    if is_synchronize {
        let cutoff = crate::store::now_ms() - 60 * 60 * 1000;
        let hourly = sessions
            .iter()
            .filter(|session| session.created_at >= cutoff)
            .count();
        if hourly_cap == 0 || hourly >= hourly_cap {
            tracing::warn!(
                trigger_ref,
                count = hourly,
                limit = hourly_cap,
                "review hourly cap reached; deduping synchronize"
            );
            emit_review_refusal(
                state,
                active.as_deref(),
                trigger_ref,
                "hourly_cap",
                hourly,
                hourly_cap,
            );
            return match active {
                Some(session_id) => Ok(ReviewAdmission::Deduped {
                    session_id,
                    reason: "hourly_cap".into(),
                }),
                None => Ok(ReviewAdmission::Refused {
                    session_id: None,
                    reason: "hourly_cap".into(),
                }),
            };
        }
    }

    Ok(ReviewAdmission::Allow)
}

/// SEI-819 catch-up: convene reviews the hourly cap dropped, once the window
/// clears. One pass per call (main.rs ticks it). A pending row is stale — and
/// deleted without convening — when any session for the trigger was created
/// after the drop (a manual /review or newer push already covered that head).
/// Catch-up convenes still pass admission, so the cap is honored, not bypassed.
pub async fn sweep_pending_reviews(state: &Arc<AppState>) -> Result<()> {
    for pending in state.store.pending_reviews()? {
        if let Some(created) = state.store.latest_session_created_at(&pending.trigger_ref)? {
            // >= : a covering session in the same ms as the drop still counts
            // (council #247 F1 — strict > left a duplicate-round edge).
            if created >= pending.requested_at {
                state.store.delete_pending_review(&pending.trigger_ref)?;
                continue;
            }
        }
        match check_review_admission(state, &pending.trigger_ref, true)? {
            ReviewAdmission::Allow => {}
            ReviewAdmission::Deduped { .. } => continue, // still capped — retry next tick
            ReviewAdmission::Refused { .. } => continue,
        }
        match convene_for_pr(
            state,
            &pending.repo,
            pending.pr_number as u64,
            pending.preset.clone(),
            pending.fingerprint.clone(),
            None,
        )
        .await
        {
            Ok(result) => {
                state.store.delete_pending_review(&pending.trigger_ref)?;
                // A fingerprint-deduped open means that head was already
                // reviewed — cleared, but nothing to announce.
                let session_id = match result {
                    crate::controller::ControllerActionResult::SessionOpened {
                        session_id,
                        deduped: false,
                    } => Some(session_id),
                    crate::controller::ControllerActionResult::Superseded {
                        session_id, ..
                    } => Some(session_id),
                    _ => None,
                };
                if let Some(session_id) = session_id {
                    state.emit_north(
                        "github_review_catchup",
                        &session_id,
                        serde_json::json!({
                            "trigger_ref": pending.trigger_ref,
                            "fingerprint": pending.fingerprint,
                            "dropped_at": pending.requested_at,
                        }),
                    );
                    tracing::info!(
                        trigger_ref = %pending.trigger_ref,
                        session = %session_id,
                        "cap catch-up convened deferred review"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    trigger_ref = %pending.trigger_ref,
                    "cap catch-up convene failed (kept for retry): {e:#}"
                );
            }
        }
    }
    Ok(())
}

/// Effective standing roster used by webhook/ask convene paths. A DB override
/// lets operators replace bots without restarting the control-plane; injected
/// process configuration remains the fallback and bootstrap source.
pub fn runtime_council_roster(state: &Arc<AppState>) -> Result<(Vec<String>, &'static str)> {
    match state.store.standing_roster()? {
        Some(roster) => Ok((roster, "override")),
        None => Ok((state.pr_review_config.council_roster.clone(), "config")),
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

/// Resolve the preset for one convene: a per-PR `review:<preset>` label wins, then the
/// injected default, then `DEFAULT_PRESET` (lite). Unknown values are warned and skipped so
/// resolution always lands on a valid preset.
fn pick_preset(label_preset: Option<&str>, configured_preset: Option<&str>) -> String {
    if let Some(l) = label_preset {
        if preset_angles(l).is_some() {
            return l.to_string();
        }
        tracing::warn!(label = %l, "unknown review:<preset> label; ignoring");
    }
    if let Some(e) = configured_preset {
        if preset_angles(e).is_some() {
            return e.to_string();
        }
        tracing::warn!(preset = %e, "unknown configured council preset (want lite|quick|standard|full); using default");
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
    render_trigger_with_context(repo, num, angle_assignment, None)
}

pub fn render_trigger_with_context(
    repo: &str,
    num: u64,
    angle_assignment: &str,
    rereview_context: Option<&ReviewRereviewContext>,
) -> String {
    let mut trigger = render_base_trigger(repo, num, angle_assignment);
    if let Some(ctx) = rereview_context {
        trigger.push_str(&render_rereview_context(ctx));
    }
    trigger
}

fn render_base_trigger(repo: &str, num: u64, angle_assignment: &str) -> String {
    TRIGGER_TMPL
        .replace("{{REPO}}", repo)
        .replace("{{NUM}}", &num.to_string())
        .replace("{{TITLE}}", "")
        .replace("{{ANGLE_ASSIGNMENT}}", angle_assignment)
}

fn render_rereview_context(ctx: &ReviewRereviewContext) -> String {
    let mut out = format!("\n\n{REREVIEW_CONTEXT_START}\n");
    if ctx.from_scratch {
        out.push_str("Mode: full review from scratch\n");
    } else if let Some(sha) = ctx.base_sha.as_deref().filter(|sha| !sha.is_empty()) {
        out.push_str(&format!("Delta: review the diff since `{sha}`\n"));
        out.push_str(&format!(
            "Rebase fallback: If `git merge-base --is-ancestor {sha} HEAD` fails, fall back to a full review and say so in the verdict.\n"
        ));
    }
    if let Some(notes) = ctx.author_notes.as_deref() {
        out.push_str("Author fix notes:\n");
        out.push_str(notes);
        if !notes.ends_with('\n') {
            out.push('\n');
        }
    }
    out.push_str(REREVIEW_CONTEXT_END);
    out
}

/// Convene a council for a PR: open a session with the standing roster (chair =
/// roster[0]; optional preset trims reviewers + sets quorum) and post the pointer
/// trigger so the bots start. Returns the new session id. No GitHub I/O happens here.
pub async fn convene_for_pr(
    state: &Arc<AppState>,
    repo: &str,
    num: u64,
    label_preset: Option<String>,
    trigger_fingerprint: Option<String>,
    mut rereview_context: Option<ReviewRereviewContext>,
) -> Result<ControllerActionResult> {
    let (roster, _) = runtime_council_roster(state)?;
    let trigger_ref = pr_trigger_ref(repo, num);
    if let Some(ctx) = rereview_context.as_mut() {
        if !ctx.from_scratch && ctx.base_sha.is_none() {
            ctx.base_sha = latest_prior_review_sha(state, &trigger_ref)?;
        }
    }
    let action = review_open_session_action_with_roster_and_fingerprint(
        repo,
        num,
        label_preset,
        roster,
        trigger_fingerprint,
        rereview_context,
        state.pr_review_config.council_preset.as_deref(),
    )?;
    controller::execute(state, ControllerAction::OpenSession(action)).map_err(Into::into)
}

fn latest_prior_review_sha(state: &Arc<AppState>, trigger_ref: &str) -> Result<Option<String>> {
    Ok(state
        .store
        .list_sessions(Some(trigger_ref), None, 1)?
        .into_iter()
        .find_map(|session| sha_from_fingerprint(session.trigger_fingerprint.as_deref())))
}

fn sha_from_fingerprint(fingerprint: Option<&str>) -> Option<String> {
    fingerprint?
        .strip_prefix("sha:")
        .filter(|sha| !sha.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
fn review_open_session_action(
    repo: &str,
    num: u64,
    label_preset: Option<String>,
) -> Result<OpenSessionAction> {
    review_open_session_action_with_roster(
        repo,
        num,
        label_preset,
        crate::plugins::pr_review::PrReviewConfig::default().council_roster,
    )
}

#[cfg(test)]
fn review_open_session_action_with_roster(
    repo: &str,
    num: u64,
    label_preset: Option<String>,
    roster: Vec<String>,
) -> Result<OpenSessionAction> {
    review_open_session_action_with_roster_and_fingerprint(
        repo,
        num,
        label_preset,
        roster,
        Some(pr_trigger_ref(repo, num)),
        None,
        None,
    )
}

fn review_open_session_action_with_roster_and_fingerprint(
    repo: &str,
    num: u64,
    label_preset: Option<String>,
    roster: Vec<String>,
    trigger_fingerprint: Option<String>,
    rereview_context: Option<ReviewRereviewContext>,
    configured_preset: Option<&str>,
) -> Result<OpenSessionAction> {
    if roster.is_empty() {
        return Err(anyhow!("empty council roster"));
    }
    // Preset (per-PR label > global env > lite) assigns angles to reviewers, trims
    // idle ones, and sets quorum to the participating reviewers.
    let preset = pick_preset(label_preset.as_deref(), configured_preset);
    let angles = preset_angles(&preset).expect("pick_preset returns a valid preset");
    let (eff_roster, quorum, assignment) = assign_angles(&roster, &angles);
    tracing::info!(preset = %preset, quorum, "convene preset resolved");
    let trigger_ref = pr_trigger_ref(repo, num);
    let trigger = render_trigger_with_context(repo, num, &assignment, rereview_context.as_ref());
    let chair_bot = eff_roster
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("assign_angles produced empty roster"))?;
    // A lone-bot roster has no reviewers, so review_council's chair would wait
    // forever for a reviewer quorum that can't arrive (C4). Route it to solo, where
    // the bot's own done closes the session — it self-reviews and posts the verdict.
    let mode = if eff_roster.len() > 1 {
        "review_council"
    } else {
        "solo"
    };
    Ok(OpenSessionAction {
        title: "council".into(),
        trigger_ref: Some(trigger_ref),
        trigger_fingerprint,
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
const ASK_TRIGGER_TMPL: &str = include_str!("../../../scripts/pr-ask-trigger-pointer.tmpl");

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
        trigger_ref: Some(trigger_ref.clone()),
        trigger_fingerprint: Some(trigger_ref),
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
        ControllerActionResult::Superseded { session_id, .. } => session_id,
        ControllerActionResult::MessagePosted { .. } => {
            unreachable!("post_message action cannot produce a council session id")
        }
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
        assert!(!t.contains("Review Council started"));
        assert!(!t.contains("===== DIFF ====="));
        assert!(!t.contains("{{"));
    }

    #[test]
    fn trigger_templates_carry_no_role_protocol() {
        let pointer = render_trigger("canyugs/ocp", 7, "");
        let inline = include_str!("../../../scripts/pr-review-trigger.tmpl")
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
    fn render_trigger_includes_rereview_context() {
        let t = render_trigger_with_context(
            "o/r",
            1,
            "- rev1 → security",
            Some(&ReviewRereviewContext {
                base_sha: Some("abc123".into()),
                author_notes: Some("Fixed F1 without touching F2.".into()),
                from_scratch: false,
            }),
        );

        assert!(t.contains("===== RE-REVIEW CONTEXT ====="));
        assert!(t.contains("Delta: review the diff since `abc123`"));
        assert!(t.contains("Author fix notes:\nFixed F1 without touching F2."));
        assert!(t.contains("===== END RE-REVIEW CONTEXT ====="));
    }

    #[test]
    fn render_trigger_full_review_omits_delta_context() {
        let t = render_trigger_with_context(
            "o/r",
            1,
            "- rev1 → security",
            Some(&ReviewRereviewContext {
                base_sha: Some("abc123".into()),
                author_notes: Some("Start over.".into()),
                from_scratch: true,
            }),
        );

        assert!(t.contains("===== RE-REVIEW CONTEXT ====="));
        assert!(t.contains("Mode: full review from scratch"));
        assert!(!t.contains("diff since `abc123`"));
        assert!(t.contains("Author fix notes:\nStart over."));
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
    fn pick_preset_label_over_config_over_default() {
        // no label, no configured preset → default (lite)
        assert_eq!(pick_preset(None, None), "lite");
        // valid label wins
        assert_eq!(pick_preset(Some("full"), None), "full");
        // unknown label ignored → default
        assert_eq!(pick_preset(Some("bogus"), None), "lite");
        // injected override when no label
        assert_eq!(pick_preset(None, Some("standard")), "standard");
        // label still beats injected config
        assert_eq!(pick_preset(Some("quick"), Some("standard")), "quick");
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
        assert_eq!(
            crate::plugins::pr_review::PrReviewConfig::default().council_roster,
            vec!["chair", "rev1", "rev2"]
        );
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
