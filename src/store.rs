//! Store trait + SQLite impl + domain types (design §8, §6c).
//!
//! The `Store` trait is the backing-service seam: SQLite today (spike), a
//! networked DB (Postgres/libSQL) for production — callers depend on the trait,
//! so the swap touches only this file. ponytail: one impl for now, but the seam
//! is deliberate (see design §6c "12-factor posture").

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bot {
    pub id: String,
    pub name: String,
    pub role: String, // "chair" | "reviewer"
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionState {
    Open,
    Deliberating,
    Quorum,
    Closed,
    Aborted,
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Open => "open",
            SessionState::Deliberating => "deliberating",
            SessionState::Quorum => "quorum",
            SessionState::Closed => "closed",
            SessionState::Aborted => "aborted",
        }
    }
    pub fn from_db_str(s: &str) -> SessionState {
        match s {
            "deliberating" => SessionState::Deliberating,
            "quorum" => SessionState::Quorum,
            "closed" => SessionState::Closed,
            "aborted" => SessionState::Aborted,
            _ => SessionState::Open,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub state: String,
    pub trigger_ref: Option<String>,
    pub quorum_n: i64,
    pub chair_bot: Option<String>,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    /// Coordination mode → picks the `Coordinator` (default "council").
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub author_kind: String, // "bot" | "client" | "system"
    pub author_id: Option<String>,
    pub content: String,
    pub reply_to: Option<String>,
    pub created_at: i64,
}

/// Backing-service seam (design §6c). All callers depend on this, not on SQLite.
pub trait Store: Send + Sync {
    fn register_bot(&self, name: &str, role: &str, token_hash: &str, token_plain: &str) -> Result<Bot>;
    /// Idempotent insert with a caller-chosen id (= name for a seeded roster, so
    /// pods can fetch /bot-config/<name>). Returns true if newly inserted.
    fn seed_bot(&self, id: &str, name: &str, role: &str, token_hash: &str, token_plain: &str) -> Result<bool>;
    fn bot_by_token_hash(&self, token_hash: &str) -> Result<Option<Bot>>;
    fn bot(&self, id: &str) -> Result<Option<Bot>>;
    /// Plaintext token, for serving /bot-config to a stock OAB pod (spike
    /// convenience; production injects the token via pre_seed/env, §6c).
    fn bot_token_plain(&self, id: &str) -> Result<Option<String>>;
    fn set_connected(&self, bot_id: &str, connected: bool) -> Result<()>;

    #[allow(clippy::too_many_arguments)]
    fn create_session(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<Session>;
    fn session(&self, id: &str) -> Result<Option<Session>>;
    /// Add a bot to a session roster. Returns true if newly added (false if it
    /// was already a member) — the caller backfills history only on a fresh join.
    fn add_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool>;
    fn set_state(&self, session_id: &str, state: SessionState) -> Result<()>;
    fn advance_state(&self, session_id: &str, from: SessionState, to: SessionState) -> Result<bool>;
    /// Close from *any* non-terminal state (the liveness watchdog — the current
    /// state is unknown when a timeout fires). CAS so only one caller wins;
    /// returns true if this call performed the close.
    fn close_if_active(&self, session_id: &str) -> Result<bool>;
    /// Non-terminal session ids created before `cutoff_ms` — watchdog candidates.
    fn active_sessions_before(&self, cutoff_ms: i64) -> Result<Vec<String>>;
    fn roster(&self, session_id: &str) -> Result<Vec<String>>;
    /// A non-terminal session carrying this `trigger_ref`, if any. Makes
    /// webhook-driven creation idempotent: GitHub re-delivers on 5xx, so a retried
    /// PR event must not open a second council for the same PR.
    fn active_session_for_trigger(&self, trigger_ref: &str) -> Result<Option<String>>;

    fn upsert_thread(&self, session_id: &str, root_message_id: Option<&str>) -> Result<String>;
    fn thread_for_session(&self, session_id: &str) -> Result<Option<String>>;

    #[allow(clippy::too_many_arguments)]
    fn add_message(
        &self,
        session_id: &str,
        thread_id: Option<&str>,
        author_kind: &str,
        author_id: Option<&str>,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<Message>;
    fn edit_message(&self, message_id: &str, content: &str) -> Result<()>;
    fn messages(&self, session_id: &str) -> Result<Vec<Message>>;

    fn add_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()>;
    fn remove_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()>;
    fn reactors_in_session(&self, session_id: &str, emoji: &str) -> Result<Vec<String>>;

    /// Durable per-bot outbox (offline delivery). Frames queued here are flushed
    /// in `seq` order when the bot is connected; a disconnected bot keeps them
    /// and gets them on reconnect. `ack_outbox` removes a delivered frame.
    fn enqueue_outbox(&self, bot_id: &str, frame: &str) -> Result<()>;
    fn pending_outbox(&self, bot_id: &str) -> Result<Vec<(i64, String)>>;
    fn ack_outbox(&self, seq: i64) -> Result<()>;

    /// Per-`session × role` GitHub installation-token cache (Principle: Agent
    /// Identity). The plane mints a scoped token once per (session, role) and reuses
    /// it until near expiry, so a council doesn't hit GitHub's token endpoint on
    /// every post. `expires_at` is unix-ms (`now_ms`). Upsert overwrites on refresh.
    fn cache_installation_token(
        &self,
        session_id: &str,
        role: &str,
        token: &str,
        expires_at: i64,
    ) -> Result<()>;
    /// Cached `(token, expires_at_ms)` for a (session, role), or None. The caller
    /// decides freshness (refresh margin lives in `github_app`).
    fn installation_token(&self, session_id: &str, role: &str) -> Result<Option<(String, i64)>>;
    /// Central revoke: drop every scoped token for a session. Called when the session
    /// closes so a pod can't keep acting on GitHub after the verdict.
    fn purge_installation_tokens(&self, session_id: &str) -> Result<()>;
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS bots (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
    token_hash TEXT NOT NULL, token_plain TEXT,
    connected INTEGER NOT NULL DEFAULT 0, last_seen INTEGER
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
    trigger_ref TEXT, quorum_n INTEGER NOT NULL, chair_bot TEXT,
    created_at INTEGER NOT NULL, closed_at INTEGER,
    mode TEXT NOT NULL DEFAULT 'council'
);
CREATE TABLE IF NOT EXISTS session_bots (
    session_id TEXT NOT NULL, bot_id TEXT NOT NULL,
    PRIMARY KEY (session_id, bot_id)
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL UNIQUE, root_message_id TEXT
);
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, thread_id TEXT,
    author_kind TEXT NOT NULL, author_id TEXT, content TEXT NOT NULL,
    reply_to TEXT, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reactions (
    message_id TEXT NOT NULL, bot_id TEXT NOT NULL, emoji TEXT NOT NULL,
    PRIMARY KEY (message_id, bot_id, emoji)
);
CREATE TABLE IF NOT EXISTS outbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    bot_id TEXT NOT NULL, frame TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbox_bot ON outbox(bot_id, seq);
-- KNOWN GAP (#4): `token` is stored in plaintext. GitHub installation tokens are
-- short-lived (≤1h) bearer credentials; until encryption-at-rest lands (AES-GCM with
-- a KMS-derived key) the DB file itself must be access-controlled. Fast-follow.
CREATE TABLE IF NOT EXISTS installation_tokens (
    session_id TEXT NOT NULL, role TEXT NOT NULL,
    token TEXT NOT NULL, expires_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, role)
);
"#;

/// Additive column migrations for DBs created before a column existed. Each
/// `ALTER` errors with "duplicate column" once the column is present — ignored.
/// ponytail: no migration framework; one guarded ALTER per added column.
fn migrate(conn: &Connection) {
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN mode TEXT NOT NULL DEFAULT 'council'", []);
}

/// SQLite-backed `Store`. ponytail: one process-wide Mutex<Connection>. Fine at
/// council scale (router + light writes). Swap the whole type for a networked
/// `Store` impl in production (design §6c).
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: &str) -> Result<SqliteStore> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn);
        Ok(SqliteStore { conn: Mutex::new(conn) })
    }

    pub fn memory() -> Result<SqliteStore> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn);
        Ok(SqliteStore { conn: Mutex::new(conn) })
    }

    fn thread_locked(&self, c: &Connection, session_id: &str) -> Result<Option<String>> {
        Ok(c.query_row(
            "SELECT id FROM threads WHERE session_id = ?1",
            params![session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    }
}

impl Store for SqliteStore {
    fn register_bot(&self, name: &str, role: &str, token_hash: &str, token_plain: &str) -> Result<Bot> {
        let id = new_id("bot");
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO bots (id, name, role, token_hash, token_plain) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, role, token_hash, token_plain],
        )?;
        Ok(Bot { id, name: name.to_string(), role: role.to_string() })
    }

    fn seed_bot(&self, id: &str, name: &str, role: &str, token_hash: &str, token_plain: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO bots (id, name, role, token_hash, token_plain) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, role, token_hash, token_plain],
        )?;
        Ok(n > 0)
    }

    fn bot_token_plain(&self, id: &str) -> Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row(
            "SELECT token_plain FROM bots WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
    }

    fn bot_by_token_hash(&self, token_hash: &str) -> Result<Option<Bot>> {
        let c = self.conn.lock().unwrap();
        let bot = c
            .query_row(
                "SELECT id, name, role FROM bots WHERE token_hash = ?1",
                params![token_hash],
                |r| Ok(Bot { id: r.get(0)?, name: r.get(1)?, role: r.get(2)? }),
            )
            .optional()?;
        Ok(bot)
    }

    fn bot(&self, id: &str) -> Result<Option<Bot>> {
        let c = self.conn.lock().unwrap();
        let bot = c
            .query_row(
                "SELECT id, name, role FROM bots WHERE id = ?1",
                params![id],
                |r| Ok(Bot { id: r.get(0)?, name: r.get(1)?, role: r.get(2)? }),
            )
            .optional()?;
        Ok(bot)
    }

    fn set_connected(&self, bot_id: &str, connected: bool) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE bots SET connected = ?2, last_seen = ?3 WHERE id = ?1",
            params![bot_id, connected as i64, now_ms()],
        )?;
        Ok(())
    }

    fn create_session(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<Session> {
        let id = new_id("ses");
        let created_at = now_ms();
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO sessions (id, title, state, trigger_ref, quorum_n, chair_bot, created_at, mode)
             VALUES (?1, ?2, 'open', ?3, ?4, ?5, ?6, ?7)",
            params![id, title, trigger_ref, quorum_n, chair_bot, created_at, mode],
        )?;
        for bot_id in roster {
            tx.execute(
                "INSERT OR IGNORE INTO session_bots (session_id, bot_id) VALUES (?1, ?2)",
                params![id, bot_id],
            )?;
        }
        tx.commit()?;
        Ok(Session {
            id,
            title: title.to_string(),
            state: "open".into(),
            trigger_ref: trigger_ref.map(String::from),
            quorum_n,
            chair_bot: chair_bot.map(String::from),
            created_at,
            closed_at: None,
            mode: mode.to_string(),
        })
    }

    fn session(&self, id: &str) -> Result<Option<Session>> {
        let c = self.conn.lock().unwrap();
        let s = c
            .query_row(
                "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode
                 FROM sessions WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        state: r.get(2)?,
                        trigger_ref: r.get(3)?,
                        quorum_n: r.get(4)?,
                        chair_bot: r.get(5)?,
                        created_at: r.get(6)?,
                        closed_at: r.get(7)?,
                        mode: r.get(8)?,
                    })
                },
            )
            .optional()?;
        Ok(s)
    }

    fn add_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO session_bots (session_id, bot_id) VALUES (?1, ?2)",
            params![session_id, bot_id],
        )?;
        Ok(n == 1)
    }

    fn set_state(&self, session_id: &str, state: SessionState) -> Result<()> {
        let closed_at = if matches!(state, SessionState::Closed | SessionState::Aborted) {
            Some(now_ms())
        } else {
            None
        };
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE sessions SET state = ?2, closed_at = COALESCE(?3, closed_at) WHERE id = ?1",
            params![session_id, state.as_str(), closed_at],
        )?;
        Ok(())
    }

    fn advance_state(
        &self,
        session_id: &str,
        from: SessionState,
        to: SessionState,
    ) -> Result<bool> {
        let closed_at = if matches!(to, SessionState::Closed | SessionState::Aborted) {
            Some(now_ms())
        } else {
            None
        };
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "UPDATE sessions SET state = ?3, closed_at = COALESCE(?4, closed_at)
             WHERE id = ?1 AND state = ?2",
            params![session_id, from.as_str(), to.as_str(), closed_at],
        )?;
        Ok(n == 1)
    }

    fn close_if_active(&self, session_id: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "UPDATE sessions SET state = 'closed', closed_at = ?2
             WHERE id = ?1 AND state NOT IN ('closed', 'aborted')",
            params![session_id, now_ms()],
        )?;
        Ok(n == 1)
    }

    fn active_sessions_before(&self, cutoff_ms: i64) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id FROM sessions
             WHERE created_at < ?1 AND state NOT IN ('closed', 'aborted')",
        )?;
        let rows = stmt.query_map(params![cutoff_ms], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn roster(&self, session_id: &str) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        // ORDER BY rowid = insertion order = the order roster was passed at
        // create_session. Pipeline stage order rides on this; council ignores it.
        let mut stmt =
            c.prepare("SELECT bot_id FROM session_bots WHERE session_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn active_session_for_trigger(&self, trigger_ref: &str) -> Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row(
            "SELECT id FROM sessions
             WHERE trigger_ref = ?1 AND state NOT IN ('closed', 'aborted')
             ORDER BY created_at DESC LIMIT 1",
            params![trigger_ref],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    }

    fn upsert_thread(&self, session_id: &str, root_message_id: Option<&str>) -> Result<String> {
        let c = self.conn.lock().unwrap();
        if let Some(existing) = self.thread_locked(&c, session_id)? {
            return Ok(existing);
        }
        let id = new_id("thr");
        c.execute(
            "INSERT INTO threads (id, session_id, root_message_id) VALUES (?1, ?2, ?3)",
            params![id, session_id, root_message_id],
        )?;
        Ok(id)
    }

    fn thread_for_session(&self, session_id: &str) -> Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        self.thread_locked(&c, session_id)
    }

    fn add_message(
        &self,
        session_id: &str,
        thread_id: Option<&str>,
        author_kind: &str,
        author_id: Option<&str>,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<Message> {
        let id = new_id("msg");
        let created_at = now_ms();
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO messages (id, session_id, thread_id, author_kind, author_id, content, reply_to, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, session_id, thread_id, author_kind, author_id, content, reply_to, created_at],
        )?;
        Ok(Message {
            id,
            session_id: session_id.to_string(),
            thread_id: thread_id.map(String::from),
            author_kind: author_kind.to_string(),
            author_id: author_id.map(String::from),
            content: content.to_string(),
            reply_to: reply_to.map(String::from),
            created_at,
        })
    }

    fn edit_message(&self, message_id: &str, content: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE messages SET content = ?2 WHERE id = ?1",
            params![message_id, content],
        )?;
        Ok(())
    }

    fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id, session_id, thread_id, author_kind, author_id, content, reply_to, created_at
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(Message {
                id: r.get(0)?,
                session_id: r.get(1)?,
                thread_id: r.get(2)?,
                author_kind: r.get(3)?,
                author_id: r.get(4)?,
                content: r.get(5)?,
                reply_to: r.get(6)?,
                created_at: r.get(7)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn add_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT OR IGNORE INTO reactions (message_id, bot_id, emoji) VALUES (?1, ?2, ?3)",
            params![message_id, bot_id, emoji],
        )?;
        Ok(())
    }

    fn remove_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM reactions WHERE message_id = ?1 AND bot_id = ?2 AND emoji = ?3",
            params![message_id, bot_id, emoji],
        )?;
        Ok(())
    }

    fn reactors_in_session(&self, session_id: &str, emoji: &str) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT DISTINCT r.bot_id FROM reactions r
             JOIN messages m ON m.id = r.message_id
             WHERE m.session_id = ?1 AND r.emoji = ?2",
        )?;
        let rows = stmt.query_map(params![session_id, emoji], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn enqueue_outbox(&self, bot_id: &str, frame: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO outbox (bot_id, frame, created_at) VALUES (?1, ?2, ?3)",
            params![bot_id, frame, now_ms()],
        )?;
        Ok(())
    }

    fn pending_outbox(&self, bot_id: &str) -> Result<Vec<(i64, String)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT seq, frame FROM outbox WHERE bot_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![bot_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn ack_outbox(&self, seq: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM outbox WHERE seq = ?1", params![seq])?;
        Ok(())
    }

    fn cache_installation_token(
        &self,
        session_id: &str,
        role: &str,
        token: &str,
        expires_at: i64,
    ) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO installation_tokens (session_id, role, token, expires_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, role)
             DO UPDATE SET token = excluded.token, expires_at = excluded.expires_at",
            params![session_id, role, token, expires_at],
        )?;
        Ok(())
    }

    fn installation_token(&self, session_id: &str, role: &str) -> Result<Option<(String, i64)>> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row(
            "SELECT token, expires_at FROM installation_tokens
             WHERE session_id = ?1 AND role = ?2",
            params![session_id, role],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
    }

    fn purge_installation_tokens(&self, session_id: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM installation_tokens WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_session_for_trigger_dedups_only_while_open() {
        let store = SqliteStore::memory().unwrap();
        let pr = "https://api.github.com/repos/o/r/pulls/1";
        assert!(store.active_session_for_trigger(pr).unwrap().is_none());

        let s = store.create_session("Review o/r#1", Some(pr), 0, None, &[], "council").unwrap();
        // an open session with this trigger is found → webhook retry is idempotent
        assert_eq!(store.active_session_for_trigger(pr).unwrap().as_deref(), Some(s.id.as_str()));

        // once closed, the same PR can open a fresh council (e.g. a later push)
        store.set_state(&s.id, SessionState::Closed).unwrap();
        assert!(store.active_session_for_trigger(pr).unwrap().is_none());
    }
}
