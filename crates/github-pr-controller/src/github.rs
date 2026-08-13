//! The controller's own hands on GitHub.
//!
//! Four calls, no more: create a comment, patch that comment, set the
//! `openab/council` commit status, and submit a **formal pull-request review**
//! (APPROVE or REQUEST_CHANGES). The last one is the whole reason this module
//! exists — org branch protection asks for review approvals, and nothing has
//! produced one since the chair stopped being able to run `gh` (see
//! `docs/HANDOFF-adr031-closing-half.md` in the ops repo).
//!
//! Everything fails closed. A write is only successful when GitHub says so in
//! the exact shape we asked for; anything else is an error the outbox retries.
//! Constructing this client at all requires `external_canary` **and**
//! `GITHUB_CONTROLLER_ENABLE_WRITES=1` (invariant #4 — one side-effect owner).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{Config, OperatingMode};

/// Re-mint this long before GitHub's stated expiry so a drain never starts a
/// call with a token about to die. Installation tokens live an hour.
const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
const USER_AGENT: &str = "openab-github-controller";
/// Reconcile reads page until exhausted, capped here. GitHub's reviews list is
/// oldest-first with no sort parameter, so "read one newest page" is not a
/// thing (council F2, #307) — scan everything, bounded: a thousand entries on
/// one pull request is beyond any real council history.
const RECONCILE_MAX_PAGES: usize = 10;

/// The only two formal reviews this controller submits. A closed enum, so a
/// parsed decision string can never reach GitHub as, say, `COMMENT` or
/// `DISMISS` — the API accepts more values than we ever want to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }

    /// The review state GitHub reports back once it has recorded the event.
    fn expected_state(self) -> &'static str {
        match self {
            Self::Approve => "APPROVED",
            Self::RequestChanges => "CHANGES_REQUESTED",
        }
    }
}

/// Commit-status state. Same reasoning as `ReviewEvent`: closed on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Success,
    Failure,
    Error,
}

impl StatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Error => "error",
        }
    }
}

pub struct GitHubClient {
    /// `Some` in canary mode — writes are pinned to exactly that repository
    /// (invariant #5's canary-phase guard). `None` in `external` mode: the App
    /// installation is the scope boundary, enforced by GitHub itself on every
    /// call the installation token makes.
    repository: Option<String>,
    /// The App's numeric id and its bot login (`<slug>[bot]`), used to verify
    /// that an object the reconcile found is OURS. The marker alone proves
    /// nothing: it is published in every comment we post, so anyone can copy
    /// it into a decoy (council F1, #307). No identity → never adopt.
    app_numeric_id: Option<i64>,
    expected_login: Option<String>,
    api_base: String,
    app_id: String,
    installation_id: String,
    private_key: String,
    http: reqwest::Client,
    token: Mutex<Option<CachedToken>>,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: SystemTime,
}

impl GitHubClient {
    /// Build the client, or `None` when this runtime is not allowed to write.
    /// The gate lives in config so `/readyz` and this constructor cannot drift.
    pub fn from_config(config: &Config) -> Option<Self> {
        if !config.github_readiness().ready {
            return None;
        }
        let app = &config.github_app;
        // Canary mode pins writes to exactly one repository (invariant #5's
        // canary-phase guard): the installation token can reach every repo the
        // App is installed on, and a mis-parsed `trigger_ref` must not write to
        // one of them. External mode drops the pin — the installation is the
        // boundary, and GitHub enforces it on every call.
        let repository = match config.mode {
            OperatingMode::External => None,
            _ => Some(config.canary_repository.clone()?),
        };
        Some(Self {
            repository,
            app_numeric_id: app.app_id.as_deref().and_then(|id| id.parse().ok()),
            expected_login: config
                .bot_handle
                .as_deref()
                .map(|handle| format!("{handle}[bot]")),
            api_base: config.github_api_base.trim_end_matches('/').to_string(),
            app_id: app.app_id.clone()?,
            installation_id: app.installation_id.clone()?,
            private_key: app.private_key.clone()?,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .ok()?,
            token: Mutex::new(None),
        })
    }

    /// Post the round's verdict comment. Returns its id, which the next round
    /// patches instead of posting again.
    pub async fn create_comment(&self, repo: &str, issue: i64, body: &str) -> Result<i64> {
        self.check_repository(repo)?;
        let response = self
            .send(
                reqwest::Method::POST,
                &format!("repos/{repo}/issues/{issue}/comments"),
                Some(json!({ "body": body })),
            )
            .await?;
        response["id"]
            .as_i64()
            .context("comment response carried no id")
    }

    pub async fn update_comment(&self, repo: &str, comment_id: i64, body: &str) -> Result<()> {
        self.check_repository(repo)?;
        self.send(
            reqwest::Method::PATCH,
            &format!("repos/{repo}/issues/comments/{comment_id}"),
            Some(json!({ "body": body })),
        )
        .await
        .map(|_| ())
    }

    pub async fn set_status(
        &self,
        repo: &str,
        sha: &str,
        state: StatusState,
        context: &str,
        description: &str,
    ) -> Result<()> {
        self.check_repository(repo)?;
        self.send(
            reqwest::Method::POST,
            &format!("repos/{repo}/statuses/{sha}"),
            Some(json!({
                "state": state.as_str(),
                "context": context,
                // GitHub truncates past 140 and we would rather choose where.
                "description": truncate(description, 140),
            })),
        )
        .await
        .map(|_| ())
    }

    /// Submit the formal review. Success is GitHub echoing the state we asked
    /// for — a 2xx with anything else means the review did not land as an
    /// approval or a block, and we must not report it as one.
    pub async fn submit_review(
        &self,
        repo: &str,
        pr_number: i64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<i64> {
        self.check_repository(repo)?;
        let response = self
            .send(
                reqwest::Method::POST,
                &format!("repos/{repo}/pulls/{pr_number}/reviews"),
                Some(json!({ "event": event.as_str(), "body": body })),
            )
            .await?;
        let state = response["state"].as_str().unwrap_or_default();
        if state != event.expected_state() {
            bail!(
                "review submitted as {state:?}, expected {:?}",
                event.expected_state()
            );
        }
        response["id"].as_i64().context("review carried no id")
    }

    /// Find our own earlier comment carrying `marker` on an issue. The
    /// pre-send reconcile: a create retried after a crash must adopt the
    /// comment that already exists instead of posting a second one.
    /// The pull request's current head sha, read from GitHub itself. Needed
    /// when the trigger webhook carried no sha (issue_comment re-reviews): the
    /// status write must pin to a commit, and without one a comment-triggered
    /// round can never overwrite an earlier failure status — an approved
    /// re-review that leaves the merge blocked (#311). Server-read sha, same
    /// authority as a webhook sha; the never-trust rule is about the *chair's
    /// claimed* sha, which stays out of status writes.
    pub async fn pull_head_sha(&self, repo: &str, pr_number: i64) -> Result<String> {
        self.check_repository(repo)?;
        let pr = self
            .send(
                reqwest::Method::GET,
                &format!("repos/{repo}/pulls/{pr_number}"),
                None,
            )
            .await?;
        pr["head"]["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("pull request response carried no head sha"))
    }

    /// The opening comment's deterministic Baseline: change scope and CI
    /// state straight from the API, so the "review started" post carries
    /// substance the second the council convenes — no model in the loop.
    pub async fn pull_baseline(&self, repo: &str, pr_number: i64) -> Result<String> {
        self.check_repository(repo)?;
        let pr = self
            .send(
                reqwest::Method::GET,
                &format!("repos/{repo}/pulls/{pr_number}"),
                None,
            )
            .await?;
        let files = pr["changed_files"].as_i64().unwrap_or_default();
        let additions = pr["additions"].as_i64().unwrap_or_default();
        let deletions = pr["deletions"].as_i64().unwrap_or_default();
        let head = pr["head"]["sha"].as_str().unwrap_or_default();
        let short = &head[..head.len().min(7)];
        let checks = match self
            .send(
                reqwest::Method::GET,
                &format!("repos/{repo}/commits/{head}/status"),
                None,
            )
            .await
        {
            Ok(status) => format!(
                "{} ({} contexts)",
                status["state"].as_str().unwrap_or("unknown"),
                status["total_count"].as_i64().unwrap_or_default()
            ),
            Err(_) => "unavailable".into(),
        };
        Ok(format!(
            "Baseline:\n\
             - Scope: {files} files changed (+{additions}/\u{2212}{deletions})\n\
             - CI/checks: {checks} at `{short}`"
        ))
    }

    /// Is `login` a member of `org`, as far as this installation can see?
    /// Webhook payloads render `author_association` against PUBLIC membership
    /// only, so a private org member arrives as CONTRIBUTOR no matter what the
    /// App is allowed to see — but this endpoint does honor the App's
    /// `members: read` grant. 204 is a member, 404 is not (or the grant is
    /// missing, which must read as untrusted, not as an error).
    pub async fn is_org_member(&self, org: &str, login: &str) -> Result<bool> {
        let token = self.installation_token().await?;
        let response = self
            .http
            .get(format!("{}/orgs/{org}/members/{login}", self.api_base))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .context("github request failed")?;
        match response.status().as_u16() {
            204 => Ok(true),
            302 | 404 => Ok(false),
            status => bail!("github orgs/{org}/members/{login} returned {status}"),
        }
    }

    /// The App's own slug, as GitHub renders it — i.e. what a human types
    /// after `@` and what comments are authored by (`<slug>[bot]`).
    ///
    /// Exists so the controller can notice that its configured bot handle no
    /// longer matches the App. Renaming an App is done in GitHub's UI, far
    /// from this repository's install checklist, and every mention command
    /// stops parsing the moment the two drift — with no error anywhere.
    pub async fn app_slug(&self) -> Result<String> {
        let jwt = self.app_jwt()?;
        let response = self
            .http
            .get(format!("{}/app", self.api_base))
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .context("github request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("github /app returned {status}");
        }
        let body: serde_json::Value = response.json().await.context("bad /app json")?;
        body["slug"]
            .as_str()
            .map(str::to_string)
            .context("github /app carried no slug")
    }

    /// Whether `login` may write to `repo` — `admin`, `maintain` or `write`.
    ///
    /// Org membership is too coarse for a decision that unblocks a merge: a
    /// member with read-only access to one repository would otherwise be able
    /// to make the controller submit an APPROVE there, lending council
    /// authority to someone GitHub itself would refuse. Errors are the
    /// caller's to fail closed on.
    pub async fn can_write_repo(&self, repo: &str, login: &str) -> Result<bool> {
        self.check_repository(repo)?;
        let token = self.installation_token().await?;
        let response = self
            .http
            .get(format!(
                "{}/repos/{repo}/collaborators/{login}/permission",
                self.api_base
            ))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .context("github request failed")?;
        match response.status().as_u16() {
            200 => {
                let body: serde_json::Value =
                    response.json().await.context("bad permission json")?;
                Ok(matches!(
                    body["permission"].as_str(),
                    Some("admin") | Some("maintain") | Some("write")
                ))
            }
            403 | 404 => Ok(false),
            status => {
                bail!("github repos/{repo}/collaborators/{login}/permission returned {status}")
            }
        }
    }

    pub async fn find_marked_comment(
        &self,
        repo: &str,
        issue: i64,
        marker: &str,
    ) -> Result<Option<i64>> {
        self.check_repository(repo)?;
        self.find_marked(&format!("repos/{repo}/issues/{issue}/comments"), marker)
            .await
    }

    /// Same reconcile for formal reviews: `submit_review` retried after a
    /// crash must find the review it already submitted.
    pub async fn find_marked_review(
        &self,
        repo: &str,
        pr_number: i64,
        marker: &str,
    ) -> Result<Option<i64>> {
        self.check_repository(repo)?;
        self.find_marked(&format!("repos/{repo}/pulls/{pr_number}/reviews"), marker)
            .await
    }

    /// Scan a listing endpoint page by page until the marker is found or the
    /// listing ends. Ordering is deliberately not relied on: the reviews
    /// endpoint is oldest-first with no sort parameter (council F2, #307), so
    /// "read the newest page" is not a thing there.
    async fn find_marked(&self, path: &str, marker: &str) -> Result<Option<i64>> {
        for page in 1..=RECONCILE_MAX_PAGES {
            let entries = self
                .send(
                    reqwest::Method::GET,
                    &format!("{path}?per_page=100&page={page}"),
                    None,
                )
                .await?;
            if let Some(id) = self.scan_for_our_marker(&entries, marker) {
                return Ok(Some(id));
            }
            if entries.as_array().map(Vec::len).unwrap_or(0) < 100 {
                return Ok(None);
            }
        }
        // The cap is a backstop, not an expected path. Say so rather than
        // silently reporting "not found" and risking the very duplicate this
        // reconcile exists to prevent.
        bail!("reconcile scanned {RECONCILE_MAX_PAGES} pages of {path} without exhausting it")
    }

    /// The id of the first entry that carries `marker` AND is provably ours —
    /// authored by the App's bot login, or created via this App id. The marker
    /// is public the moment we post it, so a body match alone would let any
    /// repo participant plant a decoy: a comment we then fail to PATCH forever,
    /// or a fake review whose presence suppresses the real one (F1, #307).
    /// With no identity configured, nothing is ever adopted — the rare crash
    /// replay then risks a duplicate, which is strictly better than adopting
    /// an object we do not own.
    fn scan_for_our_marker(&self, entries: &Value, marker: &str) -> Option<i64> {
        entries.as_array()?.iter().find_map(|entry| {
            let marked = entry["body"]
                .as_str()
                .is_some_and(|body| body.contains(marker));
            if !marked || !self.is_ours(entry) {
                return None;
            }
            entry["id"].as_i64()
        })
    }

    fn is_ours(&self, entry: &Value) -> bool {
        let login_matches = match self.expected_login.as_deref() {
            Some(expected) => entry["user"]["login"].as_str() == Some(expected),
            None => false,
        };
        let app_matches = match self.app_numeric_id {
            Some(app_id) => entry["performed_via_github_app"]["id"].as_i64() == Some(app_id),
            None => false,
        };
        login_matches || app_matches
    }

    /// Every write names a repository; this is the one place that checks it.
    /// Canary mode: must be exactly the pinned repository. External mode: any
    /// well-formed `owner/name` — the installation token is the real boundary,
    /// so an out-of-installation repo fails at GitHub, and this check only
    /// rejects garbage before it becomes a request.
    fn check_repository(&self, repo: &str) -> Result<()> {
        match self.repository.as_deref() {
            Some(owned) if repo != owned => {
                bail!("refusing to write to {repo}: this controller owns {owned}")
            }
            Some(_) => Ok(()),
            None => {
                let mut parts = repo.split('/');
                let well_formed = matches!(
                    (parts.next(), parts.next(), parts.next()),
                    (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty()
                );
                if !well_formed {
                    bail!("refusing to write to malformed repository {repo:?}");
                }
                Ok(())
            }
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let token = self.installation_token().await?;
        let mut request = self
            .http
            .request(method, format!("{}/{path}", self.api_base))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", USER_AGENT);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("github request failed")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            // The body can carry the App's own identifiers; keep the head only.
            bail!("github {path} returned {status}: {}", truncate(&text, 300));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn installation_token(&self) -> Result<String> {
        if let Some(cached) = self.cached_token() {
            return Ok(cached);
        }
        let jwt = self.app_jwt()?;
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.api_base, self.installation_id
        );
        let response = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .context("installation access_tokens request failed")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("installation token request returned {status}");
        }
        let body: Value = serde_json::from_str(&text).context("access_tokens response not JSON")?;
        let value = body["token"]
            .as_str()
            .context("access_tokens response carried no token")?
            .to_string();
        // Trust the stated expiry when we can read it, otherwise assume the
        // documented hour. Either way the margin is subtracted.
        let ttl = body["expires_at"]
            .as_str()
            .and_then(parse_rfc3339_secs)
            .and_then(|at| at.duration_since(SystemTime::now()).ok())
            .unwrap_or(Duration::from_secs(3600));
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = Some(CachedToken {
            value: value.clone(),
            expires_at: SystemTime::now() + ttl.saturating_sub(TOKEN_REFRESH_MARGIN),
        });
        Ok(value)
    }

    /// Seed the token cache so tests can exercise the four calls without a
    /// private key. No PEM belongs in this repository, test-only or not.
    #[cfg(test)]
    pub(crate) fn seed_test_token(&self, value: &str) {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = Some(CachedToken {
            value: value.to_string(),
            expires_at: SystemTime::now() + Duration::from_secs(3600),
        });
    }

    fn cached_token(&self) -> Option<String> {
        let guard = self.token.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .filter(|token| token.expires_at > SystemTime::now())
            .map(|token| token.value.clone())
    }

    /// App JWT, `iat` backdated 60s so a slightly fast clock cannot make GitHub
    /// reject it as issued in the future.
    fn app_jwt(&self) -> Result<String> {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let claims = json!({
            "iat": now.saturating_sub(60),
            "exp": now + 9 * 60,
            "iss": self.app_id,
        });
        let key = EncodingKey::from_rsa_pem(self.private_key.as_bytes())
            .context("GitHub App private key is not an RSA PEM")?;
        encode(&Header::new(Algorithm::RS256), &claims, &key).context("sign app JWT")
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let cut = (0..=max)
        .rev()
        .find(|i| value.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}…", &value[..cut])
}

/// Minimal RFC 3339 reader for GitHub's `expires_at` (`2026-07-30T12:34:56Z`).
/// A full date-time crate is not worth carrying for one field, and an
/// unparseable value simply falls back to the documented one-hour TTL.
fn parse_rfc3339_secs(value: &str) -> Option<SystemTime> {
    let (date, rest) = value.split_once('T')?;
    let time = rest.trim_end_matches('Z');
    let mut date = date.split('-');
    let (y, m, d): (i64, i64, i64) = (
        date.next()?.parse().ok()?,
        date.next()?.parse().ok()?,
        date.next()?.parse().ok()?,
    );
    let mut time = time.split(':');
    let (hh, mm, ss): (i64, i64, f64) = (
        time.next()?.parse().ok()?,
        time.next()?.parse().ok()?,
        time.next()?.parse().ok()?,
    );
    // Days from civil (Howard Hinnant's algorithm), proleptic Gregorian.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss as i64;
    (secs >= 0).then(|| UNIX_EPOCH + Duration::from_secs(secs as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GitHubAppConfig, OperatingMode};
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{patch, post};
    use axum::{Json, Router};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    // Deliberately not a real PEM: no private key belongs in this repository,
    // test-only or not (ops `AGENTS.md`). Tests seed the token cache instead,
    // so the JWT exchange is the one leg proven at the P7 canary rather than
    // here — everything downstream of the token is covered below.
    const NOT_A_PEM: &str = "test-placeholder-not-a-key";

    #[derive(Clone)]
    struct Recorder {
        tx: mpsc::UnboundedSender<(String, Value)>,
        review_state: Arc<Mutex<String>>,
    }

    /// A GitHub-shaped server on localhost. Real HTTP, real JSON, so the client
    /// is exercised end to end rather than against a mock of itself.
    async fn fake_github(review_state: &str) -> (String, mpsc::UnboundedReceiver<(String, Value)>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let recorder = Recorder {
            tx,
            review_state: Arc::new(Mutex::new(review_state.to_string())),
        };
        let app = Router::new()
            .route(
                "/app/installations/:id/access_tokens",
                post(|| async {
                    Json(json!({"token": "ghs_installation", "expires_at": "2999-01-01T00:00:00Z"}))
                }),
            )
            .route(
                "/repos/:owner/:name/collaborators/:login/permission",
                axum::routing::get(
                    |Path((_owner, _name, login)): Path<(String, String, String)>| async move {
                        match login.as_str() {
                            "maintainer" => Json(json!({"permission": "write"})).into_response(),
                            "admin" => Json(json!({"permission": "admin"})).into_response(),
                            "reader" => Json(json!({"permission": "read"})).into_response(),
                            _ => StatusCode::NOT_FOUND.into_response(),
                        }
                    },
                ),
            )
            .route(
                "/repos/:owner/:name/issues/:number/comments",
                post(
                    |State(r): State<Recorder>, Json(body): Json<Value>| async move {
                        let _ = r.tx.send(("create_comment".into(), body));
                        Json(json!({"id": 4242}))
                    },
                ),
            )
            .route(
                "/repos/:owner/:name/issues/comments/:id",
                patch(
                    |State(r): State<Recorder>,
                     Path((_, _, id)): Path<(String, String, i64)>,
                     Json(body): Json<Value>| async move {
                        let _ = r.tx.send((format!("update_comment:{id}"), body));
                        Json(json!({"id": id}))
                    },
                ),
            )
            .route(
                "/repos/:owner/:name/statuses/:sha",
                post(
                    |State(r): State<Recorder>,
                     Path((_, _, sha)): Path<(String, String, String)>,
                     Json(body): Json<Value>| async move {
                        let _ = r.tx.send((format!("status:{sha}"), body));
                        Json(json!({"id": 1}))
                    },
                ),
            )
            .route(
                "/repos/:owner/:name/pulls/:number",
                axum::routing::get(|| async {
                    Json(json!({"head": {"sha": "livehead0000000000000000000000000000dead"}}))
                }),
            )
            .route(
                "/repos/:owner/:name/pulls/:number/reviews",
                post(
                    |State(r): State<Recorder>, Json(body): Json<Value>| async move {
                        let state = r.review_state.lock().unwrap().clone();
                        let _ = r.tx.send(("review".into(), body));
                        Json(json!({"id": 77, "state": state}))
                    },
                ),
            )
            .with_state(recorder);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), rx)
    }

    #[tokio::test]
    async fn write_access_is_what_authorises_judging_a_finding() {
        // ADR 038 / council #370 F2: a decision can unblock a merge, so org
        // membership is not enough — a read-only member would otherwise borrow
        // council authority GitHub itself would refuse them.
        let (base, _rx) = fake_github("APPROVED").await;
        let client = client(&base);
        assert!(client
            .can_write_repo("example/repo", "maintainer")
            .await
            .unwrap());
        assert!(client
            .can_write_repo("example/repo", "admin")
            .await
            .unwrap());
        assert!(!client
            .can_write_repo("example/repo", "reader")
            .await
            .unwrap());
        // Not a collaborator at all: GitHub answers 404, which is a refusal.
        assert!(!client
            .can_write_repo("example/repo", "stranger")
            .await
            .unwrap());
    }

    fn client(api_base: &str) -> GitHubClient {
        let mut config = Config::from_values(|_| None);
        config.mode = OperatingMode::ExternalCanary;
        config.enable_writes = true;
        config.canary_repository = Some("example/repo".into());
        config.github_api_base = api_base.to_string();
        config.github_app = GitHubAppConfig {
            app_id: Some("4235962".into()),
            installation_id: Some("144934354".into()),
            private_key: Some(NOT_A_PEM.to_string()),
        };
        let client = GitHubClient::from_config(&config)
            .expect("canary + switch + credentials builds a client");
        client.seed_test_token("ghs_installation");
        client
    }

    #[tokio::test]
    async fn the_pull_head_is_read_from_github_and_bound_to_the_canary_repo() {
        let (base, _rx) = fake_github("APPROVED").await;
        let client = client(&base);
        let sha = client.pull_head_sha("example/repo", 7).await.unwrap();
        assert_eq!(sha, "livehead0000000000000000000000000000dead");
        assert!(
            client.pull_head_sha("other/repo", 7).await.is_err(),
            "reads honour the canary binding too"
        );
    }

    #[tokio::test]
    async fn external_mode_writes_anywhere_wellformed_in_the_installation() {
        let (base, mut rx) = fake_github("APPROVED").await;
        let mut config = Config::from_values(|_| None);
        config.mode = OperatingMode::External;
        config.enable_writes = true;
        config.github_api_base = base;
        config.github_app = GitHubAppConfig {
            app_id: Some("4235962".into()),
            installation_id: Some("144934354".into()),
            private_key: Some(NOT_A_PEM.to_string()),
        };
        let client =
            GitHubClient::from_config(&config).expect("external mode needs no canary repository");
        client.seed_test_token("ghs_installation");

        client.create_comment("any/repo", 7, "body").await.unwrap();
        let (kind, _) = rx.recv().await.unwrap();
        assert_eq!(kind, "create_comment");

        for garbage in ["norepo", "a/b/c", "/name", "owner/"] {
            assert!(
                client.create_comment(garbage, 7, "body").await.is_err(),
                "malformed {garbage:?} must be refused before the request"
            );
        }
    }

    #[test]
    fn the_client_refuses_to_exist_without_the_write_switch() {
        let mut config = Config::from_values(|_| None);
        config.mode = OperatingMode::ExternalCanary;
        config.canary_repository = Some("example/repo".into());
        config.github_app = GitHubAppConfig {
            app_id: Some("1".into()),
            installation_id: Some("2".into()),
            private_key: Some(NOT_A_PEM.to_string()),
        };
        assert!(
            GitHubClient::from_config(&config).is_none(),
            "credentials alone must not produce a writer"
        );
        config.enable_writes = true;
        config.mode = OperatingMode::PlanOnly;
        assert!(
            GitHubClient::from_config(&config).is_none(),
            "plan_only must not produce a writer"
        );
    }

    #[tokio::test]
    async fn the_four_calls_go_out_in_github_shape() {
        let (base, mut rx) = fake_github("APPROVED").await;
        let client = client(&base);

        assert_eq!(
            client
                .create_comment("example/repo", 7, "round 1 verdict")
                .await
                .unwrap(),
            4242
        );
        client
            .update_comment("example/repo", 4242, "round 2 verdict")
            .await
            .unwrap();
        client
            .set_status(
                "example/repo",
                "deadbeef",
                StatusState::Success,
                "openab/council",
                "approve r=0 y=0 g=3",
            )
            .await
            .unwrap();
        assert_eq!(
            client
                .submit_review("example/repo", 7, ReviewEvent::Approve, "LGTM")
                .await
                .unwrap(),
            77
        );

        let (kind, body) = rx.recv().await.unwrap();
        assert_eq!(
            (kind.as_str(), body["body"].as_str()),
            ("create_comment", Some("round 1 verdict"))
        );
        let (kind, body) = rx.recv().await.unwrap();
        assert_eq!(kind, "update_comment:4242");
        assert_eq!(body["body"], "round 2 verdict");
        let (kind, body) = rx.recv().await.unwrap();
        assert_eq!(kind, "status:deadbeef");
        assert_eq!(body["state"], "success");
        assert_eq!(body["context"], "openab/council");
        let (kind, body) = rx.recv().await.unwrap();
        assert_eq!(kind, "review");
        assert_eq!(body["event"], "APPROVE");
    }

    #[tokio::test]
    async fn writes_outside_the_canary_repository_are_refused_before_the_request() {
        // The installation token reaches every repository the App is installed
        // on. Invariant #5 says the canary owns exactly one, so a repo the
        // controller was not given must fail here — not at GitHub, and not
        // silently succeed because the App happens to have access.
        let (base, mut rx) = fake_github("APPROVED").await;
        let client = client(&base);
        for error in [
            client
                .create_comment("other/repo", 7, "body")
                .await
                .unwrap_err()
                .to_string(),
            client
                .update_comment("other/repo", 1, "body")
                .await
                .unwrap_err()
                .to_string(),
            client
                .set_status("other/repo", "sha", StatusState::Success, "ctx", "d")
                .await
                .unwrap_err()
                .to_string(),
            client
                .submit_review("other/repo", 7, ReviewEvent::Approve, "LGTM")
                .await
                .unwrap_err()
                .to_string(),
        ] {
            assert!(error.contains("other/repo"), "{error}");
            assert!(error.contains("example/repo"), "{error}");
        }
        assert!(
            rx.try_recv().is_err(),
            "not one request should have left the process"
        );
    }

    #[tokio::test]
    async fn a_review_that_did_not_land_as_asked_is_an_error() {
        // GitHub answered 200 but recorded a plain comment, not an approval.
        // Reporting that as success would tell the outbox the branch is
        // unblocked when it is not.
        let (base, _rx) = fake_github("COMMENTED").await;
        let error = client(&base)
            .submit_review("example/repo", 7, ReviewEvent::Approve, "LGTM")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("COMMENTED"), "{error}");

        let (base, _rx) = fake_github("APPROVED").await;
        let error = client(&base)
            .submit_review("example/repo", 7, ReviewEvent::RequestChanges, "blocked")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("CHANGES_REQUESTED"), "{error}");
    }

    #[tokio::test]
    async fn a_failing_endpoint_is_an_error_not_a_silent_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/app/installations/:id/access_tokens",
                post(|| async { Json(json!({"token": "ghs_x"})) }),
            )
            .route(
                "/repos/:owner/:name/issues/:number/comments",
                post(|| async { (axum::http::StatusCode::FORBIDDEN, "no permission") }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let error = client(&format!("http://{addr}"))
            .create_comment("example/repo", 7, "body")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("403"), "{error}");
    }

    #[test]
    fn expiry_parsing_falls_back_rather_than_trusting_a_bad_value() {
        assert_eq!(
            parse_rfc3339_secs("1970-01-02T00:00:00Z"),
            Some(UNIX_EPOCH + Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_rfc3339_secs("2026-07-30T12:00:00Z"),
            Some(UNIX_EPOCH + Duration::from_secs(1_785_412_800))
        );
        assert!(parse_rfc3339_secs("not a timestamp").is_none());
        assert!(parse_rfc3339_secs("2026-07-30").is_none());
    }

    #[test]
    fn descriptions_are_clipped_on_a_char_boundary() {
        let wide = "界".repeat(100); // 300 bytes
        let clipped = truncate(&wide, 140);
        assert!(clipped.len() <= 141 + 3);
        assert!(clipped.ends_with('…'));
        assert_eq!(truncate("short", 140), "short");
    }
}
