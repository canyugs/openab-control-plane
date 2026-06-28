//! GitHub webhook ingress + signature verification.
//!
//! This is the single ingress that turns "PR opened" / "`/review` comment" into a
//! council session (ROADMAP Phase 2 "Auto-trigger" — the receiver half). It lives in
//! the plane, never in a pod: one entry point, it opens the session, and it's the
//! only component that needs the webhook secret.
//!
//! pr-agent reference: `servers/github_app.py:handle_github_webhooks` (reads
//! `installation.id`, routes by `X-GitHub-Event`, parses `/command`) and
//! `servers/utils.py:verify_signature` (HMAC-SHA256 over the raw body). Auth here is
//! the signature, NOT the north bearer key — GitHub can't send one.

use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::Arc;

type HmacSha256 = Hmac<Sha256>;

/// A webhook that should open a review session. `parse_trigger` returns `None` for
/// everything else (the plane acks 200 and ignores it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookTrigger {
    pub repo: String,
    pub pr_number: u64,
    /// API URL of the PR — used as the session `trigger_ref`.
    pub pr_url: String,
    /// Which installation sent this. Captured for future multi-install token minting;
    /// today `GitHubApp` mints against the single env installation (Phase-2 gap).
    pub installation_id: Option<u64>,
    /// "auto" for a PR-opened trigger, or the slash command for a comment trigger.
    pub reason: String,
    /// Review preset from a `review:<preset>` label on the PR, read straight from the
    /// webhook payload (no GitHub call). `None` → convene falls back to the global
    /// env / default. Lets a PR pick its own review depth (lite|quick|standard|full).
    pub preset: Option<String>,
}

/// Extract a `review:<preset>` label name (the part after `review:`) from a payload
/// `labels` array, if present. Validation of the preset name happens in `council`.
fn preset_from_labels(labels: &Value) -> Option<String> {
    labels.as_array()?.iter().find_map(|l| {
        l["name"]
            .as_str()
            .and_then(|n| n.strip_prefix("review:"))
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
    })
}

/// Verify GitHub's `x-hub-signature-256` HMAC-SHA256 over the raw body, in constant
/// time. Mirrors pr-agent's `verify_signature`. Returns false on any malformed input.
pub fn verify_signature(secret: &str, body: &[u8], signature_header: Option<&str>) -> bool {
    let Some(sig) = signature_header else { return false };
    let Some(hex_sig) = sig.strip_prefix("sha256=") else { return false };
    let Ok(expected) = hex::decode(hex_sig) else { return false };
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// Decide whether a webhook should open a review session:
///   - `pull_request` opened / reopened / ready_for_review → auto-review.
///   - `issue_comment` created on a PR whose body starts with `/review` → on-demand.
///
/// Everything else → `None`.
pub fn parse_trigger(event: &str, body: &Value) -> Option<WebhookTrigger> {
    let installation_id = body["installation"]["id"].as_u64();
    match event {
        "pull_request" => {
            let action = body["action"].as_str()?;
            if !matches!(action, "opened" | "reopened" | "ready_for_review") {
                return None;
            }
            let pr = &body["pull_request"];
            Some(WebhookTrigger {
                repo: body["repository"]["full_name"].as_str()?.to_string(),
                pr_number: pr["number"].as_u64()?,
                pr_url: pr["url"].as_str()?.to_string(),
                installation_id,
                reason: "auto".into(),
                preset: preset_from_labels(&pr["labels"]),
            })
        }
        "issue_comment" => {
            if body["action"].as_str()? != "created" {
                return None;
            }
            // An issue is a PR only when it carries a `pull_request` node — comments
            // on plain issues must not open a review session.
            let pr_url = body["issue"]["pull_request"]["url"].as_str()?;
            let comment = body["comment"]["body"].as_str().unwrap_or("");
            let cmd = comment.trim();
            if !cmd.starts_with("/review") {
                return None;
            }
            Some(WebhookTrigger {
                repo: body["repository"]["full_name"].as_str()?.to_string(),
                pr_number: body["issue"]["number"].as_u64()?,
                pr_url: pr_url.to_string(),
                installation_id,
                // Normalized to a fixed value — never reflect raw comment text into
                // logs / north events.
                reason: "/review".into(),
                preset: preset_from_labels(&body["issue"]["labels"]),
            })
        }
        _ => None,
    }
}

/// `POST /api/v1/github_webhooks`. Verify → parse → (on a trigger) open a session.
pub async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Response, StatusCode> {
    // 1. Signature — fail-closed. No configured secret → reject everything (a
    //    missing secret must never mean "skip verification"). Invalid signature → 403.
    let Some(secret) = state.github_webhook_secret.as_deref() else {
        tracing::error!("GITHUB_WEBHOOK_SECRET unset — rejecting webhook (fail-closed)");
        return Err(StatusCode::FORBIDDEN);
    };
    let sig = headers.get("x-hub-signature-256").and_then(|v| v.to_str().ok());
    if !verify_signature(secret, &body, sig) {
        return Err(StatusCode::FORBIDDEN);
    }

    // 2. Parse the event.
    let event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let payload: Value = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    // 3. Decide. Non-triggers are acked and ignored (GitHub expects a 2xx).
    let Some(trigger) = parse_trigger(event, &payload) else {
        return Ok(Json(json!({ "ok": true, "triggered": false })).into_response());
    };

    // 4. Convene the council for this PR: read the diff, open a session with the
    //    standing roster, post the trigger so the bots start (see `council`).
    //    KNOWN GAPS: no per-repo allowlist (#11) and no permission gate on `/review`
    //    (#12) — any signed webhook / any reader can convene. Bounded by sessions
    //    being cheap; gate before production (GITHUB_ALLOWED_REPOS + collaborator).
    let trigger_ref = crate::council::pr_trigger_ref(&trigger.repo, trigger.pr_number);

    // Idempotency: GitHub re-delivers on 5xx (and a PR can get repeated `/review`s).
    // If a council is already open for this PR, return it instead of convening a dup.
    if let Some(existing) = state
        .store
        .active_session_for_trigger(&trigger_ref)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Ok(Json(json!({ "ok": true, "triggered": true, "session_id": existing, "deduped": true })).into_response());
    }

    match crate::council::convene_for_pr(&state, &trigger.repo, trigger.pr_number, trigger.preset.clone()).await {
        Ok(session_id) => {
            state.emit_north(
                "github_trigger",
                &session_id,
                json!({
                    "repo": trigger.repo,
                    "pr_number": trigger.pr_number,
                    "pr_url": trigger.pr_url,
                    "installation_id": trigger.installation_id,
                    "reason": trigger.reason,
                }),
            );
            tracing::info!(
                session = %session_id, repo = %trigger.repo,
                pr = trigger.pr_number, reason = %trigger.reason,
                "convened council from GitHub webhook"
            );
            Ok(Json(json!({ "ok": true, "triggered": true, "session_id": session_id })).into_response())
        }
        Err(e) => {
            // 500 lets GitHub retry a transient failure (the idempotency check above
            // prevents a duplicate council if a retry lands after a partial success).
            tracing::error!(repo = %trigger.repo, pr = trigger.pr_number, "convene failed: {e:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn signature_roundtrips_and_rejects_tampering() {
        let secret = "s3cr3t";
        let body = br#"{"action":"opened"}"#;
        let good = sign(secret, body);
        assert!(verify_signature(secret, body, Some(&good)));
        // wrong secret, tampered body, missing header, and bad prefix all fail
        assert!(!verify_signature("wrong", body, Some(&good)));
        assert!(!verify_signature(secret, br#"{"action":"closed"}"#, Some(&good)));
        assert!(!verify_signature(secret, body, None));
        assert!(!verify_signature(secret, body, Some("md5=deadbeef")));
    }

    #[test]
    fn pull_request_opened_triggers_auto_review() {
        let body = json!({
            "action": "opened",
            "installation": { "id": 99 },
            "repository": { "full_name": "canyugs/ocp" },
            "pull_request": { "number": 7, "url": "https://api.github.com/repos/canyugs/ocp/pulls/7" }
        });
        let t = parse_trigger("pull_request", &body).expect("should trigger");
        assert_eq!(t.repo, "canyugs/ocp");
        assert_eq!(t.pr_number, 7);
        assert_eq!(t.installation_id, Some(99));
        assert_eq!(t.reason, "auto");
        assert_eq!(t.preset, None); // no review:<preset> label
    }

    #[test]
    fn review_label_sets_preset_from_payload() {
        // pull_request labels
        let pr = json!({
            "action": "opened",
            "repository": { "full_name": "o/r" },
            "pull_request": { "number": 1, "url": "u", "labels": [{"name":"bug"},{"name":"review:full"}] }
        });
        assert_eq!(parse_trigger("pull_request", &pr).unwrap().preset.as_deref(), Some("full"));
        // issue_comment reads the issue's labels
        let ic = json!({
            "action": "created",
            "repository": { "full_name": "o/r" },
            "issue": { "number": 1, "pull_request": { "url": "u" }, "labels": [{"name":"review:quick"}] },
            "comment": { "body": "/review" }
        });
        assert_eq!(parse_trigger("issue_comment", &ic).unwrap().preset.as_deref(), Some("quick"));
        // no review: label → None
        let none = json!({
            "action": "opened",
            "repository": { "full_name": "o/r" },
            "pull_request": { "number": 1, "url": "u", "labels": [{"name":"enhancement"}] }
        });
        assert_eq!(parse_trigger("pull_request", &none).unwrap().preset, None);
    }

    #[test]
    fn pull_request_closed_does_not_trigger() {
        let body = json!({
            "action": "closed",
            "repository": { "full_name": "canyugs/ocp" },
            "pull_request": { "number": 7, "url": "u" }
        });
        assert!(parse_trigger("pull_request", &body).is_none());
    }

    #[test]
    fn review_command_on_pr_triggers() {
        let body = json!({
            "action": "created",
            "repository": { "full_name": "canyugs/ocp" },
            "issue": { "number": 12, "pull_request": { "url": "https://api.github.com/repos/canyugs/ocp/pulls/12" } },
            "comment": { "body": "/review please" }
        });
        let t = parse_trigger("issue_comment", &body).expect("should trigger");
        assert_eq!(t.pr_number, 12);
        assert_eq!(t.reason, "/review");
    }

    #[test]
    fn plain_comment_and_non_pr_issue_do_not_trigger() {
        // ordinary chatter on a PR
        let chatter = json!({
            "action": "created",
            "repository": { "full_name": "r" },
            "issue": { "number": 1, "pull_request": { "url": "u" } },
            "comment": { "body": "lgtm thanks" }
        });
        assert!(parse_trigger("issue_comment", &chatter).is_none());
        // /review on a plain issue (no pull_request node) must be ignored
        let issue = json!({
            "action": "created",
            "repository": { "full_name": "r" },
            "issue": { "number": 1 },
            "comment": { "body": "/review" }
        });
        assert!(parse_trigger("issue_comment", &issue).is_none());
    }

    #[test]
    fn unknown_event_ignored() {
        assert!(parse_trigger("push", &json!({})).is_none());
    }
}
