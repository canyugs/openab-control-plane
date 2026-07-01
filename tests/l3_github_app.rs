//! L3 — real-GitHub-App validation. The one layer unit + local tests can't reach:
//! the actual JWT → installation-token exchange and per-role permission scoping, which
//! only GitHub can confirm. Ignored by default; run against a real App:
//!
//! ```text
//! GITHUB_APP_ID=...  GITHUB_APP_INSTALLATION_ID=...  GITHUB_APP_PRIVATE_KEY="$(cat key.pem)" \
//! GITHUB_TEST_REPO=owner/repo  GITHUB_TEST_PR=123 \
//!   cargo test --test l3_github_app -- --ignored --nocapture
//! ```
//!
//! See docs/archive/github-app-validation-l3.md and issue #9.

use openab_control_plane::github_app::{GitHubApp, Role};

fn app_or_panic() -> GitHubApp {
    GitHubApp::from_env().expect(
        "L3 needs GITHUB_APP_ID + GITHUB_APP_INSTALLATION_ID + GITHUB_APP_PRIVATE_KEY \
         (see docs/archive/github-app-validation-l3.md)",
    )
}

/// Try to create a PR review with the given token. Returns the HTTP status.
/// Creating a review requires `pull_requests:write` — the exact permission that
/// separates chair from reviewer.
async fn try_create_review(token: &str, repo: &str, pr: &str) -> reqwest::StatusCode {
    let url = format!("https://api.github.com/repos/{repo}/pulls/{pr}/reviews");
    reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "openab-control-plane-l3")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .json(&serde_json::json!({ "event": "COMMENT", "body": "L3 scope check (openab-control-plane)" }))
        .send()
        .await
        .expect("review request sent")
        .status()
}

/// Proves: the App JWT (RS256) is accepted by GitHub and the installation-token
/// exchange works for both roles, returning distinct tokens. App env only.
#[tokio::test]
#[ignore = "L3: needs a real GitHub App — see docs/archive/github-app-validation-l3.md (#9)"]
async fn l3_mints_chair_and_reviewer_tokens() {
    let app = app_or_panic();
    let chair = app
        .mint_installation_token(Role::Chair)
        .await
        .expect("mint chair token");
    let reviewer = app
        .mint_installation_token(Role::Reviewer)
        .await
        .expect("mint reviewer token");
    assert!(!chair.token.is_empty(), "chair token minted");
    assert!(!reviewer.token.is_empty(), "reviewer token minted");
    assert_ne!(chair.token, reviewer.token, "a distinct token per role");
}

/// Proves the security invariant for real: a chair token can write to a PR
/// (create a review) and a reviewer token cannot (403). Needs GITHUB_TEST_REPO +
/// GITHUB_TEST_PR pointing at an open PR the installation can access.
#[tokio::test]
#[ignore = "L3: needs a real GitHub App + test PR — see docs/archive/github-app-validation-l3.md (#9)"]
async fn l3_role_scoping_chair_writes_reviewer_blocked() {
    let app = app_or_panic();
    let repo = std::env::var("GITHUB_TEST_REPO").expect("set GITHUB_TEST_REPO=owner/repo");
    let pr = std::env::var("GITHUB_TEST_PR").expect("set GITHUB_TEST_PR=<number>");

    let chair = app
        .mint_installation_token(Role::Chair)
        .await
        .expect("mint chair");
    let reviewer = app
        .mint_installation_token(Role::Reviewer)
        .await
        .expect("mint reviewer");

    let chair_status = try_create_review(&chair.token, &repo, &pr).await;
    let reviewer_status = try_create_review(&reviewer.token, &repo, &pr).await;

    assert!(
        chair_status.is_success(),
        "chair (pull_requests:write) should create a review, got {chair_status}"
    );
    assert_eq!(
        reviewer_status,
        reqwest::StatusCode::FORBIDDEN,
        "reviewer (read-only) must be blocked from writing, got {reviewer_status}"
    );
}
