//! Output adapters (design §14): verdict → side effect. One trait, idempotent
//! via outputs.status. v1 ships a GitHub PR-comment adapter; adding Jira/webhook
//! is a new impl, no core change.

use crate::state::AppState;
use crate::store::Session;
use anyhow::Result;

pub trait OutputAdapter {
    fn kind(&self) -> &'static str;
    /// Perform the side effect. Return Ok on success.
    fn emit(&self, session: &Session, verdict: &str) -> Result<()>;
}

/// Posts the verdict as a GitHub PR comment when `session.trigger_ref` looks
/// like `github:pr/<owner>/<repo>#<n>`. ponytail: shells nothing in tests; the
/// real `gh` call sits behind GH_OUTPUT=1 so CI/tests stay hermetic.
pub struct GithubPrComment;

impl OutputAdapter for GithubPrComment {
    fn kind(&self) -> &'static str {
        "github_pr_comment"
    }

    fn emit(&self, session: &Session, verdict: &str) -> Result<()> {
        let Some(ref tref) = session.trigger_ref else { return Ok(()) };
        if !tref.starts_with("github:pr/") {
            return Ok(());
        }
        if std::env::var("GH_OUTPUT").as_deref() == Ok("1") {
            // github:pr/owner/repo#42 -> owner/repo + 42
            let rest = &tref["github:pr/".len()..];
            if let Some((repo, num)) = rest.split_once('#') {
                let status = std::process::Command::new("gh")
                    .args(["pr", "comment", num, "--repo", repo, "--body", verdict])
                    .status()?;
                anyhow::ensure!(status.success(), "gh pr comment failed");
            }
        } else {
            tracing::info!(target: "output", "verdict for {tref}: {verdict}");
        }
        Ok(())
    }
}

/// Fire all configured adapters for a closed session. Records each as a row and
/// flips status; failures stay `pending` (no silent drop, §14).
pub fn fire(state: &AppState, session: &Session, verdict: &str) -> Result<()> {
    let adapters: Vec<Box<dyn OutputAdapter>> = vec![Box::new(GithubPrComment)];
    for a in adapters {
        let target = session.trigger_ref.clone().unwrap_or_default();
        let out_id = state.store.add_output(&session.id, a.kind(), &target)?;
        match a.emit(session, verdict) {
            Ok(()) => state.store.set_output_status(&out_id, "sent")?,
            Err(e) => {
                tracing::error!("output {} failed: {e}", a.kind());
                // leave as pending for retry
            }
        }
    }
    Ok(())
}
