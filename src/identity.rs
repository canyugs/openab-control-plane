//! Per-bot token issuance + verification (design §10).
//! We own the gateway side, so we replace OpenAB's single-shared-token model:
//! each bot gets its own token; the connection maps to a bot_id by token hash.

use crate::github_app::{GitHubApp, Role, REFRESH_MARGIN_MS};
use crate::store::{now_ms, Bot, Store};
use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

/// Register a bot and return (bot, plaintext token). The token is shown once;
/// only its hash is stored.
pub fn issue(store: &dyn Store, name: &str, role: &str) -> Result<(Bot, String)> {
    let token = format!("oabct_{}", uuid::Uuid::new_v4().simple());
    let bot = store.register_bot(name, role, &hash_token(&token), &token)?;
    Ok((bot, token))
}

/// Idempotently register a roster bot whose `id == name`, so pods can fetch
/// `/bot-config/<name>` with a name known ahead of time (template-wired). The
/// token is random, stored once, and served via `/bot-config` — no human copies
/// it. Returns true if newly created, false if it already existed.
pub fn seed(store: &dyn Store, name: &str, role: &str) -> Result<bool> {
    let token = format!("oabct_{}", uuid::Uuid::new_v4().simple());
    store.seed_bot(name, name, role, &hash_token(&token), &token)
}

/// Resolve a connection token to its bot, or error.
pub fn verify(store: &dyn Store, token: &str) -> Result<Bot> {
    store
        .bot_by_token_hash(&hash_token(token))?
        .ok_or_else(|| anyhow!("invalid bot token"))
}

// ---------------------------------------------------------------------------
// External identity (GitHub App) — the other "face" of the identity model
// (ROADMAP §Agent Identity). The plane mints per-`session × role` GitHub tokens
// and owns their lifecycle, mirroring how it owns the internal bot tokens above.
// ---------------------------------------------------------------------------

/// Return a valid, non-expired, **role-scoped** GitHub installation token for this
/// session — the "plane mints `session × role` tokens" path. Reuses the cached
/// token unless it's absent or within the refresh margin, otherwise mints a fresh
/// one (chair → write, reviewer → read-only) and caches it. The pod calls GitHub
/// with the returned token instead of the old broad shared PAT.
///
/// `mint_lock` serializes the mint section: without it, concurrent callers for the
/// same (session, role) each pass the freshness check and each mint a distinct live
/// token (check-then-act race).
pub async fn github_token_for(
    store: &dyn Store,
    app: &GitHubApp,
    mint_lock: &tokio::sync::Mutex<()>,
    session_id: &str,
    role: Role,
) -> Result<String> {
    if let Some(token) = fresh_cached(store, session_id, role)? {
        return Ok(token); // cache hit, comfortably fresh — no GitHub round-trip
    }
    // Serialize mints, then re-check: another task may have minted while we waited.
    let _guard = mint_lock.lock().await;
    if let Some(token) = fresh_cached(store, session_id, role)? {
        return Ok(token);
    }
    let minted = app.mint_installation_token(role).await?;
    store.cache_installation_token(session_id, role.as_str(), &minted.token, minted.expires_at)?;
    Ok(minted.token)
}

/// Cached token for (session, role) iff it stays valid past the refresh margin.
/// Written as `expires_at > now + margin` (not `expires_at - now > margin`) so an
/// already-expired token reads as a plain miss.
fn fresh_cached(store: &dyn Store, session_id: &str, role: Role) -> Result<Option<String>> {
    Ok(store
        .installation_token(session_id, role.as_str())?
        .filter(|(_, expires_at)| *expires_at > now_ms() + REFRESH_MARGIN_MS)
        .map(|(token, _)| token))
}

/// Central revoke (ROADMAP): when a session closes, drop its scoped GitHub tokens so
/// a pod can't keep acting on the PR after the verdict. Call from every close path.
///
/// KNOWN GAP (#3): this purges only the plane's cache — it does not call
/// `DELETE /installation/token` on GitHub, so a token already handed to a pod stays
/// valid at GitHub until its TTL (≤1h) elapses. The close paths are sync; a real
/// server-side revoke needs an async call and is tracked as a fast-follow. TTL is
/// the backstop until then.
pub fn revoke_session_github_tokens(store: &dyn Store, session_id: &str) -> Result<()> {
    store.purge_installation_tokens(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;

    #[test]
    fn issued_token_verifies_to_same_bot() {
        let store = SqliteStore::memory().unwrap();
        let (bot, token) = issue(&store, "gandalf", "chair").unwrap();
        let resolved = verify(&store, &token).unwrap();
        assert_eq!(resolved.id, bot.id);
        assert_eq!(resolved.role, "chair");
    }

    #[test]
    fn wrong_token_rejected() {
        let store = SqliteStore::memory().unwrap();
        issue(&store, "gandalf", "chair").unwrap();
        assert!(verify(&store, "oabct_nope").is_err());
    }

    #[test]
    fn seed_is_idempotent_and_id_equals_name() {
        let store = SqliteStore::memory().unwrap();
        assert!(seed(&store, "rev1", "reviewer").unwrap()); // first → inserted
        assert!(!seed(&store, "rev1", "reviewer").unwrap()); // again → skipped
        let bot = store.bot("rev1").unwrap().unwrap();
        assert_eq!(bot.id, "rev1"); // id == name, so /bot-config/rev1 resolves
        assert_eq!(bot.role, "reviewer");
    }

    #[test]
    fn tokens_are_distinct_per_bot() {
        let store = SqliteStore::memory().unwrap();
        let (_, t1) = issue(&store, "aragorn", "reviewer").unwrap();
        let (_, t2) = issue(&store, "gimli", "reviewer").unwrap();
        assert_ne!(t1, t2);
        assert_ne!(verify(&store, &t1).unwrap().id, verify(&store, &t2).unwrap().id);
    }

    // A throwaway App whose private key is invalid — fine for cache-hit tests where
    // minting is never reached. (Network minting can't run in unit tests.)
    fn dummy_app() -> GitHubApp {
        GitHubApp::from_parts("1", "not-a-key", 42, "https://api.github.com")
    }

    #[tokio::test]
    async fn cached_token_is_reused_without_minting() {
        let store = SqliteStore::memory().unwrap();
        // Seed a token that expires far in the future → comfortably fresh.
        let far_future = now_ms() + 60 * 60 * 1000;
        store
            .cache_installation_token("ses_1", "chair", "ghs_cached", far_future)
            .unwrap();
        // dummy_app would err if minting were attempted (bad key) — the cache hit
        // must short-circuit before that.
        let lock = tokio::sync::Mutex::new(());
        let got = github_token_for(&store, &dummy_app(), &lock, "ses_1", Role::Chair)
            .await
            .unwrap();
        assert_eq!(got, "ghs_cached");
    }

    #[tokio::test]
    async fn revoke_purges_session_tokens() {
        let store = SqliteStore::memory().unwrap();
        let far_future = now_ms() + 60 * 60 * 1000;
        store
            .cache_installation_token("ses_1", "chair", "ghs_c", far_future)
            .unwrap();
        store
            .cache_installation_token("ses_1", "reviewer", "ghs_r", far_future)
            .unwrap();
        revoke_session_github_tokens(&store, "ses_1").unwrap();
        assert!(store.installation_token("ses_1", "chair").unwrap().is_none());
        assert!(store.installation_token("ses_1", "reviewer").unwrap().is_none());
    }
}
