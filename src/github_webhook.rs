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
    /// conversational follow-up (`@mention` / `/ask`, ADR 011).
    pub reason: String,
    /// App-opaque supersede fingerprint. Equal non-NULL values dedupe; different
    /// or NULL values supersede on the review path.
    pub trigger_fingerprint: Option<String>,
    /// True only for pull_request.synchronize, where the hourly auto-cap applies.
    pub is_synchronize: bool,
    /// Review preset from a `review:<preset>` label on the PR, read straight from the
    /// webhook payload (no GitHub call). `None` → convene falls back to the global
    /// env / default. Lets a PR pick its own review depth (lite|quick|standard|full).
    pub preset: Option<String>,
    /// The follow-up question text — set only when `reason == "ask"` (ADR 011).
    pub question: Option<String>,
    /// Triggering comment id — the idempotency key for an `ask` (a re-delivered
    /// `issue_comment` webhook must not double-answer). `None` for PR-event triggers.
    pub comment_id: Option<u64>,
    /// Author-provided fix notes from `@handle review [notes]`. `Some("")` is a
    /// deliberate bare mention-review command; `None` means no carried re-review context.
    pub review_notes: Option<String>,
    /// True for `@handle full review`, which asks the successor round to omit the
    /// delta header and review from scratch.
    pub review_from_scratch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionReviewCommand {
    notes: String,
    from_scratch: bool,
}

/// Users allowed to command the bot via a comment (`/review`, `/ask`, `@mention`).
/// Read from the webhook payload's `author_association` — no GitHub call. Write-ish
/// roles only: anyone else's command is ignored (matters most for `/ask`, which spends
/// tokens on demand — ADR 011).
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

/// Extract a follow-up question from a PR comment (ADR 011). A comment is an "ask" if
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

fn strip_leading_mention<'a>(comment: &'a str, handle: Option<&str>) -> Option<&'a str> {
    let c = comment.trim();
    let h = handle?;
    let rest = c.strip_prefix('@')?;
    let name = rest.get(..h.len())?;
    if !name.eq_ignore_ascii_case(h) {
        return None;
    }
    let tail = &rest[h.len()..];
    let boundary =
        tail.is_empty() || !tail.starts_with(|ch: char| ch.is_alphanumeric() || ch == '-');
    if !boundary {
        return None;
    }
    Some(tail.trim_start_matches("[bot]").trim_start())
}

fn strip_ci_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let prefix = text.get(..word.len())?;
    if !prefix.eq_ignore_ascii_case(word) {
        return None;
    }
    let rest = &text[word.len()..];
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start_matches(char::is_whitespace))
    } else {
        None
    }
}

/// Parse the paid re-review command tier. Unlike `parse_ask_comment`, this is
/// comment-leading only so quoted/code-span/mid-sentence copies cannot supersede
/// a live council.
fn parse_mention_review_comment(
    comment: &str,
    handle: Option<&str>,
) -> Option<MentionReviewCommand> {
    let rest = strip_leading_mention(comment, handle)?;
    if let Some(after_full) = strip_ci_word(rest, "full") {
        let notes = strip_ci_word(after_full, "review")?;
        return Some(MentionReviewCommand {
            notes: notes.to_string(),
            from_scratch: true,
        });
    }
    let notes = strip_ci_word(rest, "review")?;
    Some(MentionReviewCommand {
        notes: notes.to_string(),
        from_scratch: false,
    })
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
    let Some(sig) = signature_header else {
        return false;
    };
    let Some(hex_sig) = sig.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };
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
            if !matches!(
                action,
                "opened" | "reopened" | "ready_for_review" | "synchronize"
            ) {
                return None;
            }
            let pr = &body["pull_request"];
            if action != "ready_for_review" && pr["draft"].as_bool() == Some(true) {
                return None;
            }
            Some(WebhookTrigger {
                repo: body["repository"]["full_name"].as_str()?.to_string(),
                pr_number: pr["number"].as_u64()?,
                pr_url: pr["url"].as_str()?.to_string(),
                installation_id,
                reason: "auto".into(),
                trigger_fingerprint: pr["head"]["sha"].as_str().map(|sha| format!("sha:{sha}")),
                is_synchronize: action == "synchronize",
                preset: preset_from_labels(&pr["labels"]),
                question: None,
                comment_id: None,
                review_notes: None,
                review_from_scratch: false,
            })
        }
        "issue_comment" => {
            if body["action"].as_str()? != "created" {
                return None;
            }
            // An issue is a PR only when it carries a `pull_request` node — comments
            // on plain issues must not open a review session.
            let pr_url = body["issue"]["pull_request"]["url"].as_str()?;
            if body["comment"]["user"]["type"].as_str() == Some("Bot") {
                return None;
            }
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
                    trigger_fingerprint: body["comment"]["id"]
                        .as_u64()
                        .map(|id| format!("cmd:{id}")),
                    is_synchronize: false,
                    preset: preset_from_labels(&body["issue"]["labels"]),
                    question: None,
                    comment_id: body["comment"]["id"].as_u64(),
                    review_notes: None,
                    review_from_scratch: false,
                });
            }
            if let Some(review) = parse_mention_review_comment(comment, mention_handle().as_deref())
            {
                let assoc = body["comment"]["author_association"].as_str().unwrap_or("");
                if !can_command(assoc) {
                    return None;
                }
                return Some(WebhookTrigger {
                    repo,
                    pr_number,
                    pr_url: pr_url.to_string(),
                    installation_id,
                    reason: "/review".into(),
                    trigger_fingerprint: body["comment"]["id"]
                        .as_u64()
                        .map(|id| format!("cmd:{id}")),
                    is_synchronize: false,
                    preset: preset_from_labels(&body["issue"]["labels"]),
                    question: None,
                    comment_id: body["comment"]["id"].as_u64(),
                    review_notes: Some(review.notes),
                    review_from_scratch: review.from_scratch,
                });
            }
            // Conversational follow-up (ADR 011): `/ask` or an `@mention` of the bot,
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
                    trigger_fingerprint: body["comment"]["id"]
                        .as_u64()
                        .map(|id| format!("cmd:{id}")),
                    is_synchronize: false,
                    preset: None,
                    question: Some(question),
                    comment_id: body["comment"]["id"].as_u64(),
                    review_notes: None,
                    review_from_scratch: false,
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
    let sig = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok());
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
    if !repo_allowed(
        &trigger.repo,
        std::env::var("OABCP_ALLOWED_REPOS").ok().as_deref(),
    ) {
        tracing::warn!(repo = %trigger.repo, "repo not in OABCP_ALLOWED_REPOS — ignoring webhook");
        return Ok(
            Json(json!({ "ok": true, "triggered": false, "reason": "repo_not_allowed" }))
                .into_response(),
        );
    }

    // 5. Conversational follow-up (ADR 011) takes the ask path: a solo session answers
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
            return Ok(Json(
                json!({ "ok": true, "triggered": true, "session_id": existing, "deduped": true }),
            )
            .into_response());
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
                Ok(
                    Json(json!({ "ok": true, "triggered": true, "session_id": session_id }))
                        .into_response(),
                )
            }
            Err(e) => {
                tracing::error!(repo = %trigger.repo, pr = trigger.pr_number, "ask convene failed: {e:#}");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    // 6. Review path: convene a council for this PR (pointer trigger; bots self-fetch).
    let trigger_ref = crate::council::pr_trigger_ref(&trigger.repo, trigger.pr_number);
    match crate::council::check_review_admission(&state, &trigger_ref, trigger.is_synchronize)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        crate::council::ReviewAdmission::Allow => {}
        crate::council::ReviewAdmission::Deduped { session_id, reason } => {
            return Ok(Json(json!({
                "ok": true,
                "triggered": true,
                "session_id": session_id,
                "deduped": true,
                "reason": reason,
            }))
            .into_response());
        }
        crate::council::ReviewAdmission::Refused { session_id, reason } => {
            return Ok(Json(json!({
                "ok": true,
                "triggered": false,
                "session_id": session_id,
                "refused": true,
                "reason": reason,
            }))
            .into_response());
        }
    }

    // GitHub does not guarantee cross-delivery ordering and the plane deliberately
    // makes no ancestry call here. A late older synchronize can supersede a newer
    // round; the next trigger re-supersedes, and the reviewed-head label makes the
    // stale verdict visible until a payload-only recency rule exists.
    let rereview_context = if trigger.review_notes.is_some() || trigger.review_from_scratch {
        Some(crate::council::ReviewRereviewContext {
            base_sha: None,
            author_notes: trigger.review_notes.clone(),
            from_scratch: trigger.review_from_scratch,
        })
    } else {
        None
    };
    match crate::council::convene_for_pr(
        &state,
        &trigger.repo,
        trigger.pr_number,
        trigger.preset.clone(),
        trigger.trigger_fingerprint.clone(),
        rereview_context,
    )
    .await
    {
        Ok(crate::controller::ControllerActionResult::SessionOpened {
            session_id,
            deduped,
        }) => {
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
            Ok(Json(json!({
                "ok": true,
                "triggered": true,
                "session_id": session_id,
                "deduped": deduped,
            }))
            .into_response())
        }
        Ok(crate::controller::ControllerActionResult::Superseded { session_id, old_id }) => {
            state.emit_north(
                "github_trigger",
                &session_id,
                json!({
                    "repo": trigger.repo,
                    "pr_number": trigger.pr_number,
                    "pr_url": trigger.pr_url,
                    "installation_id": trigger.installation_id,
                    "reason": trigger.reason,
                    "superseded": old_id,
                }),
            );
            tracing::info!(
                session = %session_id, old_session = %old_id, repo = %trigger.repo,
                pr = trigger.pr_number, reason = %trigger.reason,
                "superseded council from GitHub webhook"
            );
            Ok(Json(json!({
                "ok": true,
                "triggered": true,
                "session_id": session_id,
                "deduped": false,
                "superseded": true,
                "old_session_id": old_id,
            }))
            .into_response())
        }
        Ok(crate::controller::ControllerActionResult::MessagePosted { .. }) => {
            Err(StatusCode::INTERNAL_SERVER_ERROR)
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
    use crate::state::AppState;
    use crate::store::{SessionState, SqliteStore, Store};
    use axum::body::{to_bytes, Bytes};
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use std::sync::Arc;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn restore_env(key: &str, old: Option<String>) {
        match old {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn webhook_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn with_bot_handle<T>(handle: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = webhook_env_lock().lock().unwrap();
        let old = std::env::var("OABCP_BOT_HANDLE").ok();
        match handle {
            Some(handle) => std::env::set_var("OABCP_BOT_HANDLE", handle),
            None => std::env::remove_var("OABCP_BOT_HANDLE"),
        }
        let result = f();
        restore_env("OABCP_BOT_HANDLE", old);
        result
    }

    fn state_with_review_bots() -> Arc<AppState> {
        let store = Arc::new(SqliteStore::memory().unwrap());
        store
            .seed_bot("chair", "chair", "chair", "h1", "t1")
            .unwrap();
        store
            .seed_bot("rev1", "rev1", "reviewer", "h2", "t2")
            .unwrap();
        store
            .seed_bot("rev2", "rev2", "reviewer", "h3", "t3")
            .unwrap();
        AppState::new_with_options(
            store,
            None,
            None,
            Some("secret".into()),
            None,
            "http://control-plane.test".into(),
            None,
        )
    }

    fn synchronize_payload(sha: &str) -> Value {
        json!({
            "action": "synchronize",
            "installation": { "id": 99 },
            "repository": { "full_name": "o/r" },
            "pull_request": {
                "number": 7,
                "url": "https://api.github.com/repos/o/r/pulls/7",
                "draft": false,
                "head": { "sha": sha },
                "labels": []
            }
        })
    }

    fn review_comment_payload(comment_id: u64) -> Value {
        json!({
            "action": "created",
            "installation": { "id": 99 },
            "repository": { "full_name": "o/r" },
            "issue": {
                "number": 7,
                "pull_request": { "url": "https://api.github.com/repos/o/r/pulls/7" },
                "labels": []
            },
            "comment": {
                "id": comment_id,
                "body": "/review please",
                "author_association": "MEMBER",
                "user": { "type": "User" }
            }
        })
    }

    fn issue_comment_payload(comment_id: u64, body: &str) -> Value {
        json!({
            "action": "created",
            "installation": { "id": 99 },
            "repository": { "full_name": "o/r" },
            "issue": {
                "number": 7,
                "pull_request": { "url": "https://api.github.com/repos/o/r/pulls/7" },
                "labels": []
            },
            "comment": {
                "id": comment_id,
                "body": body,
                "author_association": "MEMBER",
                "user": { "type": "User" }
            }
        })
    }

    async fn post_webhook(state: Arc<AppState>, event: &str, payload: Value) -> Value {
        let body = serde_json::to_vec(&payload).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", HeaderValue::from_str(event).unwrap());
        headers.insert(
            "x-hub-signature-256",
            HeaderValue::from_str(&sign("secret", &body)).unwrap(),
        );
        let response = handle_webhook(State(state), headers, Bytes::from(body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn signature_roundtrips_and_rejects_tampering() {
        let secret = "s3cr3t";
        let body = br#"{"action":"opened"}"#;
        let good = sign(secret, body);
        assert!(verify_signature(secret, body, Some(&good)));
        // wrong secret, tampered body, missing header, and bad prefix all fail
        assert!(!verify_signature("wrong", body, Some(&good)));
        assert!(!verify_signature(
            secret,
            br#"{"action":"closed"}"#,
            Some(&good)
        ));
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
        assert_eq!(t.trigger_fingerprint.as_deref(), None);
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
        assert_eq!(t.trigger_fingerprint.as_deref(), None);
    }

    #[test]
    fn pull_request_draft_auto_events_are_ignored_until_ready_for_review() {
        let draft_sync = json!({
            "action": "synchronize",
            "installation": { "id": 99 },
            "repository": { "full_name": "canyugs/ocp" },
            "pull_request": { "number": 7, "url": "https://api.github.com/repos/canyugs/ocp/pulls/7", "draft": true }
        });
        assert!(parse_trigger("pull_request", &draft_sync).is_none());

        let draft_opened = json!({
            "action": "opened",
            "installation": { "id": 99 },
            "repository": { "full_name": "canyugs/ocp" },
            "pull_request": { "number": 7, "url": "https://api.github.com/repos/canyugs/ocp/pulls/7", "draft": true }
        });
        assert!(parse_trigger("pull_request", &draft_opened).is_none());

        let ready = json!({
            "action": "ready_for_review",
            "installation": { "id": 99 },
            "repository": { "full_name": "canyugs/ocp" },
            "pull_request": { "number": 7, "url": "https://api.github.com/repos/canyugs/ocp/pulls/7", "draft": false }
        });
        assert_eq!(
            parse_trigger("pull_request", &ready)
                .expect("ready event should trigger")
                .reason,
            "auto",
        );
    }

    #[test]
    fn review_label_sets_preset_from_payload() {
        // pull_request labels
        let pr = json!({
            "action": "opened",
            "repository": { "full_name": "o/r" },
            "pull_request": { "number": 1, "url": "u", "labels": [{"name":"bug"},{"name":"review:full"}] }
        });
        assert_eq!(
            parse_trigger("pull_request", &pr)
                .unwrap()
                .preset
                .as_deref(),
            Some("full")
        );
        // issue_comment reads the issue's labels
        let ic = json!({
            "action": "created",
            "repository": { "full_name": "o/r" },
            "issue": { "number": 1, "pull_request": { "url": "u" }, "labels": [{"name":"review:quick"}] },
            "comment": { "body": "/review", "author_association": "OWNER" }
        });
        assert_eq!(
            parse_trigger("issue_comment", &ic)
                .unwrap()
                .preset
                .as_deref(),
            Some("quick")
        );
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
    fn review_arm_captures_comment_id_fingerprint() {
        let t = parse_trigger("issue_comment", &review_comment_payload(4242)).unwrap();
        assert_eq!(t.comment_id, Some(4242));
        assert_eq!(t.trigger_fingerprint.as_deref(), Some("cmd:4242"));
    }

    #[test]
    fn bot_author_comment_is_ignored() {
        let mut body = review_comment_payload(4243);
        body["comment"]["user"]["type"] = json!("Bot");

        assert!(parse_trigger("issue_comment", &body).is_none());
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

    // --- conversational follow-up (ADR 011) ---

    #[test]
    fn parse_ask_comment_matches_slash_ask_and_mention() {
        // /ask with text → question is the remainder
        assert_eq!(
            parse_ask_comment("/ask why is this a P1?", None).as_deref(),
            Some("why is this a P1?")
        );
        // bare /ask → empty question (a ping = "look at this PR")
        assert_eq!(parse_ask_comment("/ask", None).as_deref(), Some(""));
        // @mention (handle configured) → question stripped of the mention
        assert_eq!(
            parse_ask_comment(
                "@zeabur-council can you suggest a fix?",
                Some("zeabur-council")
            )
            .as_deref(),
            Some("can you suggest a fix?"),
        );
        // tolerate the [bot] suffix
        assert_eq!(
            parse_ask_comment("@zeabur-council[bot] ping", Some("zeabur-council")).as_deref(),
            Some("ping")
        );
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

    #[test]
    fn comment_leading_mention_review_convenes_with_notes() {
        with_bot_handle(Some("zeabur-council"), || {
            let body = issue_comment_payload(
                777,
                "@zeabur-council review fixed F1\n\nAdded a regression test.",
            );

            let t = parse_trigger("issue_comment", &body).expect("mention review should trigger");

            assert_eq!(t.reason, "/review");
            assert_eq!(t.trigger_fingerprint.as_deref(), Some("cmd:777"));
            assert_eq!(
                t.review_notes.as_deref(),
                Some("fixed F1\n\nAdded a regression test.")
            );
            assert!(!t.review_from_scratch);
        });
    }

    #[test]
    fn full_review_sets_from_scratch() {
        with_bot_handle(Some("zeabur-council"), || {
            let body = issue_comment_payload(778, "@zeabur-council FULL review start over");

            let t = parse_trigger("issue_comment", &body).expect("full review should trigger");

            assert_eq!(t.reason, "/review");
            assert_eq!(t.review_notes.as_deref(), Some("start over"));
            assert!(t.review_from_scratch);
        });
    }

    #[test]
    fn quoted_footer_mention_does_not_trigger() {
        with_bot_handle(Some("zeabur-council"), || {
            let body = issue_comment_payload(779, "> @zeabur-council review fixed F1");

            let t =
                parse_trigger("issue_comment", &body).expect("quote still falls through to ask");

            assert_eq!(t.reason, "ask");
            assert_eq!(t.question.as_deref(), Some("review fixed F1"));
            assert_eq!(t.review_notes, None);
        });
    }

    #[test]
    fn code_span_mention_does_not_trigger() {
        with_bot_handle(Some("zeabur-council"), || {
            let body = issue_comment_payload(780, "`@zeabur-council review fixed F1`");

            let t = parse_trigger("issue_comment", &body)
                .expect("code span still falls through to ask");

            assert_eq!(t.reason, "ask");
            assert_eq!(t.question.as_deref(), Some("review fixed F1`"));
            assert_eq!(t.review_notes, None);
        });
    }

    #[test]
    fn mid_sentence_mention_falls_through_to_ask() {
        with_bot_handle(Some("zeabur-council"), || {
            let body = issue_comment_payload(781, "Could @zeabur-council review the auth flow?");

            let t = parse_trigger("issue_comment", &body).expect("mid-sentence mention is ask");

            assert_eq!(t.reason, "ask");
            assert_eq!(t.question.as_deref(), Some("review the auth flow?"));
            assert_eq!(t.review_notes, None);
        });
    }

    #[test]
    fn handle_unset_disables_mention_grammar() {
        with_bot_handle(None, || {
            let body = issue_comment_payload(782, "@zeabur-council review fixed F1");

            assert!(parse_trigger("issue_comment", &body).is_none());
        });
    }

    #[test]
    fn bot_author_mention_is_ignored() {
        with_bot_handle(Some("zeabur-council"), || {
            let mut body = issue_comment_payload(783, "@zeabur-council review fixed F1");
            body["comment"]["user"]["type"] = json!("Bot");

            assert!(parse_trigger("issue_comment", &body).is_none());
        });
    }

    #[test]
    fn mention_review_reads_preset_from_issue_labels() {
        with_bot_handle(Some("zeabur-council"), || {
            let mut body = issue_comment_payload(784, "@zeabur-council review fixed F1");
            body["issue"]["labels"] = json!([{ "name": "review:full" }]);

            let t = parse_trigger("issue_comment", &body).expect("mention review should trigger");

            assert_eq!(t.preset.as_deref(), Some("full"));
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mention_review_delivers_prior_sha_delta_context_to_reviewer() {
        let _policy_guard = crate::council::review_policy_env_lock().lock().await;
        let old_cap = std::env::var("OABCP_REVIEW_HOURLY_CAP").ok();
        let old_budget = std::env::var("OABCP_REVIEW_ROUND_BUDGET").ok();
        std::env::remove_var("OABCP_REVIEW_HOURLY_CAP");
        std::env::remove_var("OABCP_REVIEW_ROUND_BUDGET");

        let state = state_with_review_bots();
        let first =
            crate::council::convene_for_pr(&state, "o/r", 7, None, Some("sha:abc123".into()), None)
                .await
                .unwrap();
        let crate::controller::ControllerActionResult::SessionOpened {
            session_id: first_id,
            ..
        } = first
        else {
            panic!("first review should open");
        };

        let second = crate::council::convene_for_pr(
            &state,
            "o/r",
            7,
            None,
            Some("cmd:9003".into()),
            Some(crate::council::ReviewRereviewContext {
                base_sha: None,
                author_notes: Some(
                    "Fixed F1 by guarding the empty diff.\n\nAdded coverage.".into(),
                ),
                from_scratch: false,
            }),
        )
        .await
        .unwrap();
        let crate::controller::ControllerActionResult::Superseded {
            session_id: second_id,
            ..
        } = second
        else {
            panic!("second review should supersede");
        };

        assert_ne!(second_id, first_id);
        let prompt = state
            .store
            .messages(&second_id)
            .unwrap()
            .into_iter()
            .find(|message| message.author_kind == "client")
            .unwrap()
            .content;
        assert!(prompt.contains("Delta: review the diff since `abc123`"));
        assert!(prompt.contains("Fixed F1 by guarding the empty diff.\n\nAdded coverage."));

        let reviewer_frames = state.store.pending_outbox("rev1").unwrap();
        assert!(reviewer_frames.iter().any(|(_, frame)| {
            frame.contains("review the diff since `abc123`")
                && frame.contains("Fixed F1 by guarding the empty diff.")
                && frame.contains("git merge-base --is-ancestor abc123 HEAD")
        }));

        restore_env("OABCP_REVIEW_HOURLY_CAP", old_cap);
        restore_env("OABCP_REVIEW_ROUND_BUDGET", old_budget);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synchronize_supersedes_active_review_session() {
        let _guard = crate::council::review_policy_env_lock().lock().await;
        let old_cap = std::env::var("OABCP_REVIEW_HOURLY_CAP").ok();
        let old_budget = std::env::var("OABCP_REVIEW_ROUND_BUDGET").ok();
        std::env::remove_var("OABCP_REVIEW_HOURLY_CAP");
        std::env::remove_var("OABCP_REVIEW_ROUND_BUDGET");

        let state = state_with_review_bots();
        let first = post_webhook(state.clone(), "pull_request", synchronize_payload("old")).await;
        let old_id = first["session_id"].as_str().unwrap().to_string();
        let mut north = state.north_tx.subscribe();

        let second = post_webhook(state.clone(), "pull_request", synchronize_payload("new")).await;
        let new_id = second["session_id"].as_str().unwrap().to_string();

        assert_ne!(new_id, old_id);
        assert_eq!(second["superseded"], json!(true));
        assert_eq!(
            SessionState::from_db_str(&state.store.session(&old_id).unwrap().unwrap().state),
            SessionState::Closed
        );
        assert_eq!(
            state
                .store
                .active_session_for_trigger("github:pr/o/r#7")
                .unwrap()
                .as_deref(),
            Some(new_id.as_str())
        );
        let events = std::iter::from_fn(|| north.try_recv().ok())
            .map(|raw| serde_json::from_str::<Value>(&raw).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            event["type"] == "state"
                && event["session_id"] == old_id
                && event["payload"]["state"] == "closed"
                && event["payload"]["reason"] == "superseded"
        }));

        restore_env("OABCP_REVIEW_HOURLY_CAP", old_cap);
        restore_env("OABCP_REVIEW_ROUND_BUDGET", old_budget);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn redelivered_same_head_sha_dedupes() {
        let _guard = crate::council::review_policy_env_lock().lock().await;
        let old_cap = std::env::var("OABCP_REVIEW_HOURLY_CAP").ok();
        let old_budget = std::env::var("OABCP_REVIEW_ROUND_BUDGET").ok();
        std::env::remove_var("OABCP_REVIEW_HOURLY_CAP");
        std::env::remove_var("OABCP_REVIEW_ROUND_BUDGET");

        let state = state_with_review_bots();
        let first = post_webhook(state.clone(), "pull_request", synchronize_payload("same")).await;
        let first_id = first["session_id"].as_str().unwrap().to_string();

        let second = post_webhook(state.clone(), "pull_request", synchronize_payload("same")).await;

        assert_eq!(second["session_id"].as_str(), Some(first_id.as_str()));
        assert_eq!(second["deduped"], json!(true));
        assert_eq!(state.store.messages(&first_id).unwrap().len(), 1);

        restore_env("OABCP_REVIEW_HOURLY_CAP", old_cap);
        restore_env("OABCP_REVIEW_ROUND_BUDGET", old_budget);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hourly_cap_dedupes_synchronize_but_explicit_review_bypasses() {
        let _guard = crate::council::review_policy_env_lock().lock().await;
        let old_cap = std::env::var("OABCP_REVIEW_HOURLY_CAP").ok();
        let old_budget = std::env::var("OABCP_REVIEW_ROUND_BUDGET").ok();
        std::env::set_var("OABCP_REVIEW_HOURLY_CAP", "1");
        std::env::remove_var("OABCP_REVIEW_ROUND_BUDGET");

        let state = state_with_review_bots();
        let first = post_webhook(state.clone(), "pull_request", synchronize_payload("one")).await;
        let first_id = first["session_id"].as_str().unwrap().to_string();

        let capped = post_webhook(state.clone(), "pull_request", synchronize_payload("two")).await;
        assert_eq!(capped["session_id"].as_str(), Some(first_id.as_str()));
        assert_eq!(capped["deduped"], json!(true));
        assert_eq!(capped["reason"], "hourly_cap");
        assert_ne!(
            SessionState::from_db_str(&state.store.session(&first_id).unwrap().unwrap().state),
            SessionState::Closed
        );

        let explicit =
            post_webhook(state.clone(), "issue_comment", review_comment_payload(9001)).await;
        let explicit_id = explicit["session_id"].as_str().unwrap().to_string();
        assert_ne!(explicit_id, first_id);
        assert_eq!(explicit["superseded"], json!(true));
        assert_eq!(
            SessionState::from_db_str(&state.store.session(&first_id).unwrap().unwrap().state),
            SessionState::Closed
        );

        restore_env("OABCP_REVIEW_HOURLY_CAP", old_cap);
        restore_env("OABCP_REVIEW_ROUND_BUDGET", old_budget);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn round_budget_refuses_all_paths_leaving_live_round_active() {
        let _guard = crate::council::review_policy_env_lock().lock().await;
        let old_cap = std::env::var("OABCP_REVIEW_HOURLY_CAP").ok();
        let old_budget = std::env::var("OABCP_REVIEW_ROUND_BUDGET").ok();
        std::env::remove_var("OABCP_REVIEW_HOURLY_CAP");
        std::env::set_var("OABCP_REVIEW_ROUND_BUDGET", "1");

        let state = state_with_review_bots();
        let first = post_webhook(state.clone(), "pull_request", synchronize_payload("one")).await;
        let first_id = first["session_id"].as_str().unwrap().to_string();
        let mut north = state.north_tx.subscribe();

        let auto_refused =
            post_webhook(state.clone(), "pull_request", synchronize_payload("two")).await;
        assert_eq!(auto_refused["triggered"], json!(false));
        assert_eq!(auto_refused["refused"], json!(true));
        assert_eq!(auto_refused["reason"], "round_budget");
        assert_eq!(
            state
                .store
                .active_session_for_trigger("github:pr/o/r#7")
                .unwrap()
                .as_deref(),
            Some(first_id.as_str())
        );

        let explicit_refused =
            post_webhook(state.clone(), "issue_comment", review_comment_payload(9002)).await;
        assert_eq!(explicit_refused["triggered"], json!(false));
        assert_eq!(explicit_refused["refused"], json!(true));
        assert_eq!(explicit_refused["reason"], "round_budget");
        assert_ne!(
            SessionState::from_db_str(&state.store.session(&first_id).unwrap().unwrap().state),
            SessionState::Closed
        );
        let events = std::iter::from_fn(|| north.try_recv().ok())
            .map(|raw| serde_json::from_str::<Value>(&raw).unwrap())
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            event["type"] == "github_review_refused"
                && event["session_id"] == first_id
                && event["payload"]["reason"] == "round_budget"
        }));

        restore_env("OABCP_REVIEW_HOURLY_CAP", old_cap);
        restore_env("OABCP_REVIEW_ROUND_BUDGET", old_budget);
    }
}
