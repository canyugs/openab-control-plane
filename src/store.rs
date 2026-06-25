//! SQLite store + domain types (design §8).
//! ponytail: one process-wide Mutex<Connection>. Fine at council scale (router +
//! light writes). Swap to a pool if write contention ever shows up.

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
    pub fn from_str(s: &str) -> SessionState {
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

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS bots (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
    token_hash TEXT NOT NULL, connected INTEGER NOT NULL DEFAULT 0, last_seen INTEGER
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
    trigger_ref TEXT, quorum_n INTEGER NOT NULL, chair_bot TEXT,
    created_at INTEGER NOT NULL, closed_at INTEGER
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
CREATE TABLE IF NOT EXISTS outputs (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, kind TEXT NOT NULL,
    target TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL
);
"#;

impl Store {
    pub fn open(path: &str) -> Result<Store> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    pub fn memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn: Mutex::new(conn) })
    }

    // --- bots / identity ---

    pub fn register_bot(&self, name: &str, role: &str, token_hash: &str) -> Result<Bot> {
        let id = new_id("bot");
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO bots (id, name, role, token_hash) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, role, token_hash],
        )?;
        Ok(Bot { id, name: name.to_string(), role: role.to_string() })
    }

    pub fn bot_by_token_hash(&self, token_hash: &str) -> Result<Option<Bot>> {
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

    pub fn bot(&self, id: &str) -> Result<Option<Bot>> {
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

    pub fn set_connected(&self, bot_id: &str, connected: bool) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE bots SET connected = ?2, last_seen = ?3 WHERE id = ?1",
            params![bot_id, connected as i64, now_ms()],
        )?;
        Ok(())
    }

    // --- sessions ---

    pub fn create_session(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
    ) -> Result<Session> {
        let id = new_id("ses");
        let created_at = now_ms();
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO sessions (id, title, state, trigger_ref, quorum_n, chair_bot, created_at)
             VALUES (?1, ?2, 'open', ?3, ?4, ?5, ?6)",
            params![id, title, trigger_ref, quorum_n, chair_bot, created_at],
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
        })
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        let c = self.conn.lock().unwrap();
        let s = c
            .query_row(
                "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at
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
                    })
                },
            )
            .optional()?;
        Ok(s)
    }

    pub fn set_state(&self, session_id: &str, state: SessionState) -> Result<()> {
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

    /// Atomic guarded transition. Returns true iff this call performed it
    /// (state was `from`). Lets concurrent repliers fire one-shot transitions
    /// (deliberating→quorum, quorum→closed) exactly once.
    pub fn advance_state(
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

    pub fn roster(&self, session_id: &str) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT bot_id FROM session_bots WHERE session_id = ?1")?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // --- threads (convergence invariant §6a) ---

    /// One thread per session (convergence invariant §6a). First create wins;
    /// concurrent callers get the same thread_id.
    pub fn upsert_thread(&self, session_id: &str, root_message_id: Option<&str>) -> Result<String> {
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

    pub fn thread_for_session(&self, session_id: &str) -> Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        self.thread_locked(&c, session_id)
    }

    fn thread_locked(&self, c: &Connection, session_id: &str) -> Result<Option<String>> {
        Ok(c.query_row(
            "SELECT id FROM threads WHERE session_id = ?1",
            params![session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    }

    // --- messages ---

    #[allow(clippy::too_many_arguments)]
    pub fn add_message(
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

    pub fn edit_message(&self, message_id: &str, content: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE messages SET content = ?2 WHERE id = ?1",
            params![message_id, content],
        )?;
        Ok(())
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
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

    // --- reactions (quorum signal) ---

    pub fn add_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT OR IGNORE INTO reactions (message_id, bot_id, emoji) VALUES (?1, ?2, ?3)",
            params![message_id, bot_id, emoji],
        )?;
        Ok(())
    }

    pub fn remove_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM reactions WHERE message_id = ?1 AND bot_id = ?2 AND emoji = ?3",
            params![message_id, bot_id, emoji],
        )?;
        Ok(())
    }

    /// Distinct bots that have reacted with `emoji` anywhere in this session.
    pub fn reactors_in_session(&self, session_id: &str, emoji: &str) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT DISTINCT r.bot_id FROM reactions r
             JOIN messages m ON m.id = r.message_id
             WHERE m.session_id = ?1 AND r.emoji = ?2",
        )?;
        let rows = stmt.query_map(params![session_id, emoji], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // --- outputs ---

    pub fn add_output(&self, session_id: &str, kind: &str, target: &str) -> Result<String> {
        let id = new_id("out");
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO outputs (id, session_id, kind, target, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
            params![id, session_id, kind, target, now_ms()],
        )?;
        Ok(id)
    }

    pub fn set_output_status(&self, output_id: &str, status: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE outputs SET status = ?2 WHERE id = ?1",
            params![output_id, status],
        )?;
        Ok(())
    }
}
