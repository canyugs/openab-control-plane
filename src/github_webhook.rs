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
    /// "auto" for a PR-opened trigger, `/review` for a review command, or `"ask"` for a
    /// conversational follow-up (`@mention` / `/ask`, ADR 006).
    pub reason: String,
    /// Review preset from a `review:<preset>` label on the PR, read straight from the
    /// webhook payload (no GitHub call). `None` → convene falls back to the global
    /// env / default. Lets a PR pick its own review depth (lite|quick|standard|full).
    pub preset: Option<String>,
    /// The follow-up question text — set only when `reason == "ask"` (ADR 006).
    pub question: Option<String>,
    /// Triggering comment id — the idempotency key for an `ask` (a re-delivered
    /// `issue_comment` webhook must not double-answer). `None` for PR-event triggers.
    pub comment_id: Option<u64>,
}

/// Users allowed to command the bot via a comment (`/review`, `/ask`, `@mention`).
/// Read from the webhook payload's `author_association` — no GitHub call. Write-ish
/// roles only: anyone else's command is ignored (matters most for `/ask`, which spends
/// tokens on demand — ADR 006).
fn can_command(author_association: &str) -> bool {
    matches!(author_association, "OWNER" | "MEMBER" | "COLLABORATOR")
}

/// Per-repo allowlist gate. `allowlist` = the `OABCP_ALLOWED_REPOS` value (comma-sep
/// `owner/repo`). Unset/empty → allow all (opt-in, no regression). Pure for testing.
fn repo_allowed(repo: &str, allowlist: Option<&str>) -> bool {
    match allowlist.map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(list) => list.split(',').map(str::trim).any(|r| r == repo),
    }
}

/// The bot's GitHub handle for `@mention` parsing (env `OABCP_BOT_HANDLE`, e.g.
/// `zeabur-council`). Unset → only the explicit `/ask` command works, not `@mention`.
fn mention_handle() -> Option<String> {
    std::env::var("OABCP_BOT_HANDLE")
        .ok()
        .map(|s| s.trim().trim_start_matches('@').to_string())
        .filter(|s| !s.is_empty())
}

/// Extract a follow-up question from a PR comment (ADR 006). A comment is an "ask" if
/// it starts with `/ask` **or** @mentions the bot handle; the returned string is the
/// question with the command/mention stripped (may be empty — a bare ping = "look at
/// this PR"). `None` if it's neither. Pure (handle passed in) for testing.
fn parse_ask_comment(comment: &str, handle: Option<&str>) -> Option<String> {
    let c = comment.trim();
    // `/ask` — require a word boundary so `/asked` / `/asking` don't match and launch
    // a session with a nonsense question.
    if let Some(rest) = c.strip_prefix("/ask") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some(rest.trim().to_string());
        }
    }
    if let Some(h) = handle {
        let tag = format!("@{h}");
        if let Some(pos) = c.find(&tag) {
            let tail = &c[pos + tag.len()..];
            // Word boundary after the handle (GitHub handles are `[A-Za-z0-9-]`), so a
            // handle `council` doesn't match the *different* user `@council-admin`.
            let boundary =
                tail.is_empty() || !tail.starts_with(|ch: char| ch.is_alphanumeric() || ch == '-');
            if boundary {
                let rest = tail.trim_start_matches("[bot]").trim(); // tolerate `@handle[bot]`
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Match a slash command only when the command is exact or followed by whitespace, so
/// `/reviewer` does not trigger `/review`.
fn starts_with_slash_command(comment: &str, command: &str) -> bool {
    let Some(rest) = comment.strip_prefix(command) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
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
///   - `pull_request` opened / reopened / ready_for_review / synchronize → auto-review.
///   - `issue_comment` created by a write-ish user on a PR whose body starts with
///     `/review` → on-demand.
///
/// Everything else → `None`.
pub fn parse_trigger(event: &str, body: &Value) -> Option<WebhookTrigger> {
    let installation_id = body["installation"]["id"].as_u64();
    match event {
        "pull_request" => {
            let action = body["action"].as_str()?;
            if !matches!(action, "opened" | "reopened" | "ready_for_review" | "synchronize") {
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
                question: None,
                comment_id: None,
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
            let repo = body["repository"]["full_name"].as_str()?.to_string();
            let pr_number = body["issue"]["number"].as_u64()?;
            let cmd = comment.trim();
            if starts_with_slash_command(cmd, "/review") {
                let assoc = body["comment"]["author_association"].as_str().unwrap_or("");
                if !can_command(assoc) {
                    return None;
                }
                return Some(WebhookTrigger {
                    repo,
                    pr_number,
                    pr_url: pr_url.to_string(),
                    installation_id,
                    // Normalized to a fixed value — never reflect raw comment text into
                    // logs / north events.
                    reason: "/review".into(),
                    preset: preset_from_labels(&body["issue"]["labels"]),
                    question: None,
                    comment_id: None,
                });
            }
            // Conversational follow-up (ADR 006): `/ask` or an `@mention` of the bot,
            // answered by a solo session. Permission-gated (token spend on demand) —
            // only a write-ish commenter may ask; everyone else is ignored.
            if let Some(question) = parse_ask_comment(comment, mention_handle().as_deref()) {
                let assoc = body["comment"]["author_association"].as_str().unwrap_or("");
                if !can_command(assoc) {
                    return None;
                }
                return Some(WebhookTrigger {
                    repo,
                    pr_number,
                    pr_url: pr_url.to_string(),
                    installation_id,
                    reason: "ask".into(),
                    preset: None,
                    question: Some(question),
                    comment_id: body["comment"]["id"].as_u64(),
                });
            }
            None
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

    // 4. Per-repo allowlist (opt-in via `OABCP_ALLOWED_REPOS`; unset = allow all). A
    //    disallowed repo is acked and ignored — a signed webhook from an un-listed repo
    //    must not convene. (`/ask` is also commenter-permission-gated in parse_trigger.)
    if !repo_allowed(&trigger.repo, std::env::var("OABCP_ALLOWED_REPOS").ok().as_deref()) {
        tracing::warn!(repo = %trigger.repo, "repo not in OABCP_ALLOWED_REPOS — ignoring webhook");
        return Ok(Json(json!({ "ok": true, "triggered": false, "reason": "repo_not_allowed" })).into_response());
    }

    // 5. Conversational follow-up (ADR 006) takes the ask path: a solo session answers
    //    the question. Idempotency keys on the comment id (a re-delivered issue_comment
    //    must not double-answer) — distinct from the PR-level review dedup below.
    if trigger.reason == "ask" {
        let ask_ref = crate::council::pr_ask_trigger_ref(
            &trigger.repo,
            trigger.pr_number,
            trigger.comment_id,
        );
        if let Some(existing) = state
            .store
            .active_session_for_trigger(&ask_ref)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        {
            return Ok(Json(json!({ "ok": true, "triggered": true, "session_id": existing, "deduped": true })).into_response());
        }
        let question = trigger.question.clone().unwrap_or_default();
        return match crate::council::convene_ask(
            &state,
            &trigger.repo,
            trigger.pr_number,
            &question,
            trigger.comment_id,
        )
        .await
        {
            Ok(session_id) => {
                // Telemetry parity with the review path's `github_trigger` event.
                state.emit_north(
                    "github_ask",
                    &session_id,
                    json!({
                        "repo": trigger.repo,
                        "pr_number": trigger.pr_number,
                        "comment_id": trigger.comment_id,
                        "reason": trigger.reason,
                    }),
                );
                tracing::info!(session = %session_id, repo = %trigger.repo, pr = trigger.pr_number, "answered follow-up from GitHub webhook");
                Ok(Json(json!({ "ok": true, "triggered": true, "session_id": session_id })).into_response())
            }
            Err(e) => {
                tracing::error!(repo = %trigger.repo, pr = trigger.pr_number, "ask convene failed: {e:#}");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    // 6. Review path: convene a council for this PR (pointer trigger; bots self-fetch).
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
    fn pull_request_synchronize_triggers_auto_review() {
        let body = json!({
            "action": "synchronize",
            "installation": { "id": 99 },
            "repository": { "full_name": "canyugs/ocp" },
            "pull_request": { "number": 7, "url": "https://api.github.com/repos/canyugs/ocp/pulls/7" }
        });
        let t = parse_trigger("pull_request", &body).expect("should trigger");
        assert_eq!(t.repo, "canyugs/ocp");
        assert_eq!(t.pr_number, 7);
        assert_eq!(t.reason, "auto");
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
            "comment": { "body": "/review", "author_association": "OWNER" }
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
    fn review_command_on_pr_triggers_for_write_user() {
        let body = json!({
            "action": "created",
            "repository": { "full_name": "canyugs/ocp" },
            "issue": { "number": 12, "pull_request": { "url": "https://api.github.com/repos/canyugs/ocp/pulls/12" } },
            "comment": { "body": "/review please", "author_association": "MEMBER" }
        });
        let t = parse_trigger("issue_comment", &body).expect("should trigger");
        assert_eq!(t.pr_number, 12);
        assert_eq!(t.reason, "/review");
    }

    #[test]
    fn review_command_is_gated_to_write_user() {
        let body = |assoc: &str| {
            json!({
                "action": "created",
                "repository": { "full_name": "canyugs/ocp" },
                "issue": { "number": 12, "pull_request": { "url": "u" } },
                "comment": { "body": "/review please", "author_association": assoc }
            })
        };
        assert!(parse_trigger("issue_comment", &body("COLLABORATOR")).is_some());
        assert!(parse_trigger("issue_comment", &body("OWNER")).is_some());
        assert!(parse_trigger("issue_comment", &body("CONTRIBUTOR")).is_none());
        assert!(parse_trigger("issue_comment", &body("NONE")).is_none());

        let missing_assoc = json!({
            "action": "created",
            "repository": { "full_name": "canyugs/ocp" },
            "issue": { "number": 12, "pull_request": { "url": "u" } },
            "comment": { "body": "/review please" }
        });
        assert!(parse_trigger("issue_comment", &missing_assoc).is_none());

        let different_command = json!({
            "action": "created",
            "repository": { "full_name": "canyugs/ocp" },
            "issue": { "number": 12, "pull_request": { "url": "u" } },
            "comment": { "body": "/reviewer please", "author_association": "OWNER" }
        });
        assert!(parse_trigger("issue_comment", &different_command).is_none());
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

    // --- conversational follow-up (ADR 006) ---

    #[test]
    fn parse_ask_comment_matches_slash_ask_and_mention() {
        // /ask with text → question is the remainder
        assert_eq!(parse_ask_comment("/ask why is this a P1?", None).as_deref(), Some("why is this a P1?"));
        // bare /ask → empty question (a ping = "look at this PR")
        assert_eq!(parse_ask_comment("/ask", None).as_deref(), Some(""));
        // @mention (handle configured) → question stripped of the mention
        assert_eq!(
            parse_ask_comment("@zeabur-council can you suggest a fix?", Some("zeabur-council")).as_deref(),
            Some("can you suggest a fix?"),
        );
        // tolerate the [bot] suffix
        assert_eq!(parse_ask_comment("@zeabur-council[bot] ping", Some("zeabur-council")).as_deref(), Some("ping"));
        // a mention with no configured handle is NOT an ask
        assert!(parse_ask_comment("@zeabur-council hi", None).is_none());
        // ordinary chatter is not an ask
        assert!(parse_ask_comment("lgtm thanks", Some("zeabur-council")).is_none());
        // word-boundary: `/asked`/`/asking` must NOT match `/ask`
        assert!(parse_ask_comment("/asked for another review", None).is_none());
        assert!(parse_ask_comment("/asking", None).is_none());
        // word-boundary: handle `council` must NOT match the different user `@council-admin`
        assert!(parse_ask_comment("@council-admin said yes", Some("council")).is_none());
    }

    #[test]
    fn can_command_is_write_ish_only() {
        for a in ["OWNER", "MEMBER", "COLLABORATOR"] {
            assert!(can_command(a), "{a} should be allowed");
        }
        for a in ["CONTRIBUTOR", "FIRST_TIME_CONTRIBUTOR", "NONE", ""] {
            assert!(!can_command(a), "{a} should be denied");
        }
    }

    #[test]
    fn repo_allowed_opt_in() {
        assert!(repo_allowed("o/r", None), "unset = allow all");
        assert!(repo_allowed("o/r", Some("")), "empty = allow all");
        assert!(repo_allowed("o/r", Some("a/b, o/r ,c/d")));
        assert!(!repo_allowed("x/y", Some("a/b,o/r")));
    }

    #[test]
    fn ask_command_triggers_for_write_user_and_is_gated() {
        let body = |assoc: &str| {
            json!({
                "action": "created",
                "repository": { "full_name": "canyugs/ocp" },
                "issue": { "number": 12, "pull_request": { "url": "u" } },
                "comment": { "id": 555, "body": "/ask why P1?", "author_association": assoc }
            })
        };
        // a collaborator's /ask → an ask trigger carrying the question + comment id
        let t = parse_trigger("issue_comment", &body("COLLABORATOR")).expect("write user asks");
        assert_eq!(t.reason, "ask");
        assert_eq!(t.question.as_deref(), Some("why P1?"));
        assert_eq!(t.comment_id, Some(555));
        assert_eq!(t.pr_number, 12);
        // a non-write commenter's /ask is ignored (token-spend gate)
        assert!(parse_trigger("issue_comment", &body("NONE")).is_none());
    }
}
