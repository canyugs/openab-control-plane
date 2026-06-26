//! Per-bot token issuance + verification (design §10).
//! We own the gateway side, so we replace OpenAB's single-shared-token model:
//! each bot gets its own token; the connection maps to a bot_id by token hash.

use crate::store::{Bot, Store};
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
}
