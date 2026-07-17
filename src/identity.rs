//! Per-bot token issuance + verification (design §10).
//! We own the gateway side, so we replace OpenAB's single-shared-token model:
//! each bot gets its own token; the connection maps to a bot_id by token hash.

use crate::github_app::{GitHubApp, Role, REFRESH_MARGIN_MS};
use crate::store::{now_ms, Bot, Store};
use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

static EXTERNALIZE_DEFAULT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

/// Register a bot and return (bot, plaintext token). The token is shown once to
/// the API caller; plaintext-at-rest is controlled by ADR 016 compatibility.
pub fn issue(
    store: &dyn Store,
    name: &str,
    role: &str,
    provided: Option<String>,
) -> Result<(Bot, String)> {
    let (token, plaintext) = issue_token(provided);
    let bot = store.register_bot(name, role, &hash_token(&token), &plaintext)?;
    Ok((bot, token))
}

/// Whether gateway tokens are delivered through the pod's env
/// (`token = "${OABCP_BOT_TOKEN}"` in `/bot-config`, OpenAB env-expands at boot)
/// instead of rendered plaintext (ADR 016).
pub fn externalize_tokens() -> bool {
    match std::env::var("OABCP_EXTERNALIZE_TOKENS") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => *EXTERNALIZE_DEFAULT.get().unwrap_or(&true),
    }
}

/// Decide the externalize default from DB state (ADR 016 S15). Returns
/// (default_on, legacy_bot_names). Only meaningful when the env is unset;
/// callers with an explicit env never invoke this. Emits the deprecation
/// warning when falling back to legacy.
pub fn decide_externalize_default(store: &dyn Store) -> anyhow::Result<(bool, Vec<String>)> {
    let legacy = store.bots_with_plaintext_token()?;
    if legacy.is_empty() {
        return Ok((true, legacy));
    }
    let vars: Vec<String> = legacy.iter().map(|n| bot_token_env_var(n)).collect();
    tracing::warn!(
        "OABCP_EXTERNALIZE_TOKENS is unset and {} bot(s) still hold plaintext \
         tokens in the DB; staying in LEGACY plaintext mode so this in-place \
         upgrade keeps booting. To migrate to externalized tokens, set these \
         plane env vars to each bot's token and OABCP_EXTERNALIZE_TOKENS=1: {} \
         — and set OABCP_BOT_TOKEN on each bot pod to its own token, since \
         /bot-config will then serve the env reference instead of plaintext",
        legacy.len(),
        vars.join(", ")
    );
    Ok((false, legacy))
}

/// Resolve + install the boot decision into the process global. Called once at
/// startup BEFORE roster seeding. Explicit env → no-op (explicit always wins).
pub fn resolve_externalize_default(store: &dyn Store) -> anyhow::Result<()> {
    if std::env::var("OABCP_EXTERNALIZE_TOKENS").is_ok() {
        return Ok(());
    }
    let (default_on, _) = decide_externalize_default(store)?;
    let _ = EXTERNALIZE_DEFAULT.set(default_on);
    Ok(())
}

#[cfg(test)]
pub(crate) fn token_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The plane env var an operator sets to supply bot `<name>`'s gateway token in
/// externalized mode. Non-alphanumerics fold to `_` so a name like `rev-codex-1`
/// still yields a valid env var (`OABCP_BOT_TOKEN_REV_CODEX_1`).
pub fn bot_token_env_var(name: &str) -> String {
    let suffix: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("OABCP_BOT_TOKEN_{suffix}")
}

/// Pick a seeded bot's `(token, plaintext_to_store)`. Externalized: the operator
/// provided the token, so store the hash and **no plaintext** (nothing for
/// `/bot-config` to leak). Legacy: generate a random token and keep the plaintext
/// so the endpoint can serve it.
fn seed_token(provided: Option<String>) -> (String, String) {
    match provided {
        Some(token) => (token, String::new()),
        None => {
            let token = format!("oabct_{}", uuid::Uuid::new_v4().simple());
            (token.clone(), token)
        }
    }
}

/// Pick an API-registered bot's `(token, plaintext_to_store)`. Supplied tokens
/// are operator-owned, so the plane stores only the hash. Generated tokens remain
/// plaintext-at-rest only in legacy mode for `/bot-config` compatibility.
fn issue_token(provided: Option<String>) -> (String, String) {
    match provided {
        Some(token) => (token, String::new()),
        None => {
            let token = format!("oabct_{}", uuid::Uuid::new_v4().simple());
            let plaintext = if externalize_tokens() {
                String::new()
            } else {
                token.clone()
            };
            (token, plaintext)
        }
    }
}

/// Idempotently register a roster bot whose `id == name`, so pods can fetch
/// `/bot-config/<name>` with a name known ahead of time (template-wired).
/// Externalized (ADR 016): the token comes from the plane env
/// `OABCP_BOT_TOKEN_<NAME>`, hash-only at rest. Legacy: a random token is stored
/// and served plaintext. Returns true if newly created, false if it existed.
pub fn seed(store: &dyn Store, name: &str, role: &str) -> Result<bool> {
    let provided = if externalize_tokens() {
        let var = bot_token_env_var(name);
        Some(std::env::var(&var).map_err(|_| {
            anyhow!(
                "token externalization is on (OABCP_EXTERNALIZE_TOKENS set, or \
                 defaulting on since S15) but {var} is unset for bot '{name}'; \
                 set {var}, or OABCP_EXTERNALIZE_TOKENS=0 for legacy plaintext mode"
            )
        })?)
    } else {
        None
    };
    let (token, plaintext) = seed_token(provided);
    store.seed_bot(name, name, role, &hash_token(&token), &plaintext)
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
    // Bot-level fetches (`bot:*` keys) come from the pod's pre_boot refresh
    // loop, which re-asks only every ~50 minutes — a cached token with less
    // remaining life than that dies in the pod's hands and every GitHub call
    // 401s until the next refresh (SEI-810, live on prod: a pod restarted
    // mid-token-life got a ~20-minutes-left token and ran two blind rounds).
    // These fetches are rare (per pod per ~50min), so always mint fresh.
    // Session-level fetches are frequent and short-lived; they keep the cache.
    let cacheable = !session_id.starts_with("bot:");
    if cacheable {
        if let Some(token) = fresh_cached(store, session_id, role)? {
            return Ok(token); // cache hit, comfortably fresh — no GitHub round-trip
        }
    }
    // Serialize mints, then re-check: another task may have minted while we waited.
    let _guard = mint_lock.lock().await;
    if cacheable {
        if let Some(token) = fresh_cached(store, session_id, role)? {
            return Ok(token);
        }
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
/// Two layers: (1) purge the plane's cache so no future call hands the token out;
/// (2) revoke it GitHub-side via `DELETE /installation/token` so a token already in
/// a pod dies immediately instead of living out its ≤1h TTL. Layer 2 is async and
/// best-effort — the close paths are sync, so each revoke is spawned and the TTL
/// stays the backstop if the call fails. Fires only in App mode (`app` = Some);
/// PAT mode has no per-role installation tokens to revoke.
pub fn revoke_session_github_tokens(
    store: &dyn Store,
    app: Option<&GitHubApp>,
    session_id: &str,
) -> Result<()> {
    // Read live tokens before the purge wipes them.
    let tokens = store.session_installation_tokens(session_id)?;
    store.purge_installation_tokens(session_id)?;
    if let Some(app) = app {
        for token in tokens {
            let app = app.clone(); // clones the PEM string; fine at 1–2 tokens per close
            tokio::spawn(async move {
                if let Err(e) = app.revoke_installation_token(&token).await {
                    tracing::warn!("server-side github token revoke failed: {e}");
                }
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, Store};

    #[test]
    fn issued_token_verifies_to_same_bot() {
        let store = SqliteStore::memory().unwrap();
        let (bot, token) = issue(&store, "gandalf", "chair", None).unwrap();
        let resolved = verify(&store, &token).unwrap();
        assert_eq!(resolved.id, bot.id);
        assert_eq!(resolved.role, "chair");
    }

    #[test]
    fn wrong_token_rejected() {
        let store = SqliteStore::memory().unwrap();
        issue(&store, "gandalf", "chair", None).unwrap();
        assert!(verify(&store, "oabct_nope").is_err());
    }

    #[tokio::test]
    async fn seed_is_idempotent_and_id_equals_name() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "0");

        let store = SqliteStore::memory().unwrap();
        assert!(seed(&store, "rev1", "reviewer").unwrap()); // first → inserted
        assert!(!seed(&store, "rev1", "reviewer").unwrap()); // again → skipped
        let bot = store.bot("rev1").unwrap().unwrap();
        assert_eq!(bot.id, "rev1"); // id == name, so /bot-config/rev1 resolves
        assert_eq!(bot.role, "reviewer");
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    #[test]
    fn bot_token_env_var_folds_non_alnum() {
        assert_eq!(bot_token_env_var("chair"), "OABCP_BOT_TOKEN_CHAIR");
        assert_eq!(bot_token_env_var("rev1"), "OABCP_BOT_TOKEN_REV1");
        // hyphens/dots → underscore so the var name stays valid
        assert_eq!(
            bot_token_env_var("rev-codex-1"),
            "OABCP_BOT_TOKEN_REV_CODEX_1"
        );
    }

    #[test]
    fn seed_token_externalized_stores_no_plaintext() {
        // operator-provided → hash-only, nothing for /bot-config to leak
        let (token, plaintext) = seed_token(Some("oabct_provided".into()));
        assert_eq!(token, "oabct_provided");
        assert!(plaintext.is_empty());
        // legacy → random token kept as plaintext to serve
        let (token, plaintext) = seed_token(None);
        assert_eq!(token, plaintext);
        assert!(token.starts_with("oabct_"));
    }

    #[tokio::test]
    async fn issue_generated_externalized_stores_no_plaintext_and_verifies() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "1");

        let store = SqliteStore::memory().unwrap();
        let (bot, token) = issue(&store, "rev-ext", "reviewer", None).unwrap();

        assert_eq!(store.bot_token_plain(&bot.id).unwrap().as_deref(), Some(""));
        assert_eq!(verify(&store, &token).unwrap().id, bot.id);
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    #[tokio::test]
    async fn issue_generated_legacy_stores_plaintext() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "0");

        let store = SqliteStore::memory().unwrap();
        let (bot, token) = issue(&store, "rev-legacy", "reviewer", None).unwrap();

        assert_eq!(
            store.bot_token_plain(&bot.id).unwrap().as_deref(),
            Some(token.as_str())
        );
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    #[tokio::test]
    async fn issue_provided_legacy_stores_no_plaintext_and_verifies() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "0");

        let store = SqliteStore::memory().unwrap();
        let provided = "operator_token_123".to_string();
        let (bot, token) =
            issue(&store, "rev-provided", "reviewer", Some(provided.clone())).unwrap();

        assert_eq!(token, provided);
        assert_eq!(store.bot_token_plain(&bot.id).unwrap().as_deref(), Some(""));
        assert_eq!(verify(&store, &token).unwrap().id, bot.id);
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    #[tokio::test]
    async fn decide_default_fresh_db_externalizes() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::remove_var("OABCP_EXTERNALIZE_TOKENS");

        let store = SqliteStore::memory().unwrap();
        let decision = decide_externalize_default(&store).unwrap();

        assert_eq!(decision, (true, vec![]));
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    #[tokio::test]
    async fn decide_default_legacy_rows_stays_legacy() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::set_var("OABCP_EXTERNALIZE_TOKENS", "0");

        let store = SqliteStore::memory().unwrap();
        seed(&store, "rev1", "reviewer").unwrap();
        std::env::remove_var("OABCP_EXTERNALIZE_TOKENS");

        let decision = decide_externalize_default(&store).unwrap();

        assert_eq!(decision, (false, vec!["rev1".to_string()]));
        assert_eq!(bot_token_env_var(&decision.1[0]), "OABCP_BOT_TOKEN_REV1");
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    /// Pins the boot wiring, not just the pure decision: with the env unset,
    /// resolve_externalize_default on a fresh DB must leave externalize_tokens()
    /// reading true. Safe against OnceLock cross-test pollution — the fresh-DB
    /// direction latches `true`, which matches the unresolved fallback.
    #[tokio::test]
    async fn resolve_fresh_db_installs_externalized_default() {
        let _guard = token_env_lock().lock().await;
        let old = std::env::var("OABCP_EXTERNALIZE_TOKENS").ok();
        std::env::remove_var("OABCP_EXTERNALIZE_TOKENS");

        let store = SqliteStore::memory().unwrap();
        resolve_externalize_default(&store).unwrap();

        assert!(externalize_tokens());
        restore_env("OABCP_EXTERNALIZE_TOKENS", old);
    }

    #[test]
    fn tokens_are_distinct_per_bot() {
        let store = SqliteStore::memory().unwrap();
        let (_, t1) = issue(&store, "aragorn", "reviewer", None).unwrap();
        let (_, t2) = issue(&store, "gimli", "reviewer", None).unwrap();
        assert_ne!(t1, t2);
        assert_ne!(
            verify(&store, &t1).unwrap().id,
            verify(&store, &t2).unwrap().id
        );
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
    async fn bot_level_key_always_mints_fresh_even_with_fresh_cache() {
        // SEI-810: the pod's refresh loop re-asks only every ~50min, so handing
        // back a cached token (however "fresh" by the 5-minute margin) can leave
        // the pod holding a token that dies mid-cycle. bot:* keys must mint.
        let store = SqliteStore::memory().unwrap();
        let far_future = now_ms() + 60 * 60 * 1000;
        store
            .cache_installation_token("bot:chair", "chair", "ghs_cached", far_future)
            .unwrap();
        let lock = tokio::sync::Mutex::new(());
        // dummy_app errors on mint — reaching the mint IS the assertion: the
        // fresh cached row above must NOT short-circuit a bot-level fetch.
        let err = github_token_for(&store, &dummy_app(), &lock, "bot:chair", Role::Chair).await;
        assert!(err.is_err(), "bot:* fetch must attempt a fresh mint");
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
        // revoke reads the live tokens (for server-side DELETE) before purging.
        let live = store.session_installation_tokens("ses_1").unwrap();
        assert_eq!(live.len(), 2);
        assert!(live.contains(&"ghs_c".to_string()) && live.contains(&"ghs_r".to_string()));
        // app=None → cache-only revoke, no network (PAT-mode path); server-side
        // DELETE needs a live GitHub and is covered by the plane integration run.
        revoke_session_github_tokens(&store, None, "ses_1").unwrap();
        assert!(store
            .session_installation_tokens("ses_1")
            .unwrap()
            .is_empty());
        assert!(store
            .installation_token("ses_1", "chair")
            .unwrap()
            .is_none());
        assert!(store
            .installation_token("ses_1", "reviewer")
            .unwrap()
            .is_none());
    }

    fn restore_env(key: &str, old: Option<String>) {
        match old {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
