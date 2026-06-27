//! GitHub App identity — the external (GitHub-facing) half of the identity model
//! (ROADMAP "GitHub App identity" + "Per-role scoped tokens"; Principle: Agent
//! Identity). The internal half — bot↔plane tokens — lives in `identity.rs`; these
//! are "two faces of the same model" (ROADMAP §Agent Identity).
//!
//! Option A (plane = identity registry, pod executes): the **plane** holds the App
//! private key in one place, mints a short-lived App JWT, and exchanges it for a
//! *per-role scoped* installation access token — chair gets `pull_requests:write`,
//! reviewers get read-only. The scoped token is handed down to a pod (via the
//! `/v1/sessions/:id/github-token` endpoint); the pod calls the GitHub API with it
//! instead of the old broad shared PAT (`api.rs` `inherit_env` GH_TOKEN). The pod
//! never sees the App private key — only a short-lived, role-scoped, session-bound
//! token. Closing a session purges the cached tokens (central revoke).
//!
//! pr-agent reference: `github_provider.py:_get_github_client` does the App-vs-PAT
//! split via PyGithub's `AppAuthentication(app_id, private_key, installation_id)`,
//! which hides the JWT→installation-token exchange. We do it explicitly because the
//! per-role `permissions` scoping (pr-agent has none — one all-permissions token) is
//! the whole point here.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

/// Refresh a cached token this long before its real expiry, so a pod never receives
/// a token about to die mid-review. GitHub caps installation tokens at 1h.
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;
/// Conservative TTL we record for a freshly minted token (GitHub's 1h minus margin).
/// We honor this rather than parsing GitHub's RFC3339 `expires_at` — refreshing
/// early is always safe and avoids a date-parsing dependency.
const TOKEN_TTL_MS: i64 = 55 * 60 * 1000;

/// External (GitHub-facing) role → permission scope. Derived from the internal bot
/// role string in `store::Bot.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Source of truth for the verdict — may write to PRs (approve / request-changes
    /// / comment). Exactly one per council.
    Chair,
    /// Deliberates only — read-only. Can't post, so it can't produce the duplicate
    /// ×N inline comments that motivated per-role scoping (ROADMAP Phase 1).
    Reviewer,
}

impl Role {
    pub fn from_bot_role(role: &str) -> Role {
        match role {
            "chair" => Role::Chair,
            _ => Role::Reviewer,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Chair => "chair",
            Role::Reviewer => "reviewer",
        }
    }

    /// GitHub installation-token permission scope for this role. The point of
    /// per-role scoping: a leaked reviewer token physically cannot write to a PR.
    /// Matches the App perms in ROADMAP: `pull_requests:write`, `contents:read`.
    pub fn permissions(&self) -> Value {
        match self {
            Role::Chair => json!({ "pull_requests": "write", "contents": "read" }),
            Role::Reviewer => json!({ "pull_requests": "read", "contents": "read" }),
        }
    }
}

/// A minted, role-scoped installation access token.
#[derive(Debug, Clone)]
pub struct MintedToken {
    pub token: String,
    /// Unix epoch **milliseconds** (matches `store::now_ms`), so cache-expiry math
    /// is uniform with the rest of the store.
    pub expires_at: i64,
    pub role: Role,
}

#[derive(Serialize)]
struct Claims {
    iat: u64,
    exp: u64,
    iss: String,
}

/// The plane's GitHub App credential. One per deployment; held only here.
#[derive(Clone)]
pub struct GitHubApp {
    pub app_id: String,
    private_key_pem: String,
    pub installation_id: u64,
    api_base: String,
}

impl GitHubApp {
    pub fn from_parts(
        app_id: impl Into<String>,
        private_key_pem: impl Into<String>,
        installation_id: u64,
        api_base: impl Into<String>,
    ) -> GitHubApp {
        GitHubApp {
            app_id: app_id.into(),
            private_key_pem: private_key_pem.into(),
            installation_id,
            api_base: api_base.into(),
        }
    }

    /// Build from env. Returns `None` if unset, so the plane still boots in PAT mode
    /// (pr-agent's `deployment_type = "user"`) for parity testing before the App is
    /// provisioned. Env:
    ///   - `GITHUB_APP_ID`
    ///   - `GITHUB_APP_INSTALLATION_ID`
    ///   - `GITHUB_APP_PRIVATE_KEY` — PEM (literal newlines or `\n`-escaped), or a
    ///     base64-wrapped PEM (convenient for single-line env stores).
    ///   - `GITHUB_API_BASE` (optional; defaults to public GitHub, override for GHES).
    pub fn from_env() -> Option<GitHubApp> {
        let app_id = std::env::var("GITHUB_APP_ID").ok()?;
        let installation_id = std::env::var("GITHUB_APP_INSTALLATION_ID").ok()?.parse().ok()?;
        let private_key_pem = normalize_pem(&std::env::var("GITHUB_APP_PRIVATE_KEY").ok()?);
        let api_base =
            std::env::var("GITHUB_API_BASE").unwrap_or_else(|_| "https://api.github.com".into());
        Some(GitHubApp::from_parts(app_id, private_key_pem, installation_id, api_base))
    }

    /// Mint a short-lived App JWT (RS256, `iss = app_id`, ≤10 min) — the auth used to
    /// exchange for an installation token. `iat` is backdated 60s to tolerate clock
    /// skew (GitHub's own guidance).
    pub fn app_jwt(&self) -> Result<String> {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let now = unix_secs();
        let claims = Claims { iat: now - 60, exp: now + 9 * 60, iss: self.app_id.clone() };
        let key = EncodingKey::from_rsa_pem(self.private_key_pem.as_bytes())
            .context("GITHUB_APP_PRIVATE_KEY is not a valid RSA PEM")?;
        encode(&Header::new(Algorithm::RS256), &claims, &key)
            .context("signing App JWT failed")
    }

    /// Exchange the App JWT for a **per-role scoped** installation access token:
    /// `POST /app/installations/{id}/access_tokens` with role-scoped `permissions`.
    /// This is the line pr-agent doesn't have — it mints one all-permissions token;
    /// we scope per role so reviewers are read-only.
    pub async fn mint_installation_token(&self, role: Role) -> Result<MintedToken> {
        let jwt = self.app_jwt()?;
        let url = format!(
            "{}/app/installations/{}/access_tokens",
            self.api_base.trim_end_matches('/'),
            self.installation_id
        );
        let resp = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "openab-control-plane")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&json!({ "permissions": role.permissions() }))
            .send()
            .await
            .context("installation access_tokens request failed")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("parse access_tokens response")?;
        if !status.is_success() {
            return Err(anyhow!("GitHub access_tokens returned {status}: {body}"));
        }
        let token = body["token"]
            .as_str()
            .ok_or_else(|| anyhow!("access_tokens response had no `token`: {body}"))?
            .to_string();
        Ok(MintedToken { token, expires_at: now_ms_local() + TOKEN_TTL_MS, role })
    }
}

/// Accept a PEM with real newlines, a `\n`-escaped single line, or a base64-wrapped
/// PEM — env stores mangle multi-line secrets in all three ways.
fn normalize_pem(raw: &str) -> String {
    let s = raw.trim();
    if s.contains("-----BEGIN") {
        s.replace("\\n", "\n")
    } else {
        use base64::{engine::general_purpose::STANDARD, Engine};
        STANDARD
            .decode(s)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_else(|| s.to_string())
    }
}

fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn now_ms_local() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_maps_from_bot_role_string() {
        assert_eq!(Role::from_bot_role("chair"), Role::Chair);
        assert_eq!(Role::from_bot_role("reviewer"), Role::Reviewer);
        // unknown roles default to the least-privileged role
        assert_eq!(Role::from_bot_role("observer"), Role::Reviewer);
    }

    #[test]
    fn chair_can_write_reviewer_is_read_only() {
        // The security invariant: only the chair gets pull_requests:write.
        assert_eq!(Role::Chair.permissions()["pull_requests"], "write");
        assert_eq!(Role::Reviewer.permissions()["pull_requests"], "read");
        // neither role gets contents:write — we never push code.
        assert_eq!(Role::Chair.permissions()["contents"], "read");
        assert_eq!(Role::Reviewer.permissions()["contents"], "read");
    }

    #[test]
    fn normalize_pem_handles_escaped_and_base64() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nABC\n-----END RSA PRIVATE KEY-----";
        // already-real newlines pass through
        assert_eq!(normalize_pem(pem), pem);
        // \n-escaped single line is unescaped
        let escaped = pem.replace('\n', "\\n");
        assert_eq!(normalize_pem(&escaped), pem);
        // base64-wrapped PEM is decoded back to the PEM
        use base64::{engine::general_purpose::STANDARD, Engine};
        let b64 = STANDARD.encode(pem);
        assert_eq!(normalize_pem(&b64), pem);
    }
}
