//! Store trait + SQLite impl + domain types (design §8, §6c).
//!
//! The `Store` trait is the backing-service seam: SQLite today (spike), a
//! networked DB (Postgres/libSQL) for production — callers depend on the trait,
//! so the swap touches only this file. ponytail: one impl for now, but the seam
//! is deliberate (see design §6c "12-factor posture").

use anyhow::{Context, Result};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotInventory {
    pub id: String,
    pub name: String,
    pub role: String,
    pub provider: Option<String>,
    pub capabilities: Vec<String>,
    pub connected: bool,
    pub enabled: bool,
    pub health: String,
    pub note: Option<String>,
    pub version: Option<String>,
    pub runtime: Option<Value>,
    pub last_seen_ms: Option<i64>,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct BotMetadata {
    pub provider: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub version: Option<String>,
    pub runtime: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct BotMetadataPatch {
    pub provider: Option<Option<String>>,
    pub capabilities: Option<Vec<String>>,
    pub enabled: Option<bool>,
    pub health: Option<String>,
    pub note: Option<Option<String>>,
    pub version: Option<Option<String>>,
    pub runtime: Option<Option<Value>>,
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
    /// Structured verdict (ADR 013), parsed from the chair's `[[verdict:…]]`
    /// trailer at normal close. All NULL on timeout or missing trailer.
    pub decision: Option<String>,
    pub findings_red: Option<i64>,
    pub findings_yellow: Option<i64>,
    pub findings_green: Option<i64>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub message_id: String,
    pub bot_id: String,
    pub emoji: String,
}

/// Backing-service seam (design §6c). All callers depend on this, not on SQLite.
pub trait Store: Send + Sync {
    fn register_bot(
        &self,
        name: &str,
        role: &str,
        token_hash: &str,
        token_plain: &str,
    ) -> Result<Bot>;
    /// Idempotent insert with a caller-chosen id (= name for a seeded roster, so
    /// pods can fetch /bot-config/<name>). Returns true if newly inserted.
    fn seed_bot(
        &self,
        id: &str,
        name: &str,
        role: &str,
        token_hash: &str,
        token_plain: &str,
    ) -> Result<bool>;
    fn bot_by_token_hash(&self, token_hash: &str) -> Result<Option<Bot>>;
    fn bot(&self, id: &str) -> Result<Option<Bot>>;
    /// Plaintext token, for serving /bot-config to a stock OAB pod (spike
    /// convenience; production injects the token via pre_seed/env, §6c).
    fn bot_token_plain(&self, id: &str) -> Result<Option<String>>;
    fn set_connected(&self, bot_id: &str, connected: bool) -> Result<()>;
    fn list_bots(&self) -> Result<Vec<BotInventory>>;
    fn bot_inventory(&self, id: &str) -> Result<Option<BotInventory>>;
    fn discover_bot(
        &self,
        id: &str,
        name: Option<&str>,
        role: &str,
        metadata: &BotMetadata,
    ) -> Result<(Bot, bool)>;
    fn update_bot_metadata(&self, id: &str, patch: &BotMetadataPatch) -> Result<bool>;

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
    /// Idempotent session create for controller actions. If a non-terminal session
    /// already owns `trigger_ref`, returns it with `deduped = true` and does not
    /// mutate its prompt or roster.
    #[allow(clippy::too_many_arguments)]
    fn create_session_deduped(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<(Session, bool)>;
    fn session(&self, id: &str) -> Result<Option<Session>>;
    fn list_sessions(
        &self,
        trigger_ref: Option<&str>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Session>>;
    /// Add a bot to a session roster. Returns true if newly added (false if it
    /// was already a member) — the caller backfills history only on a fresh join.
    fn add_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool>;
    /// Replace one session roster member with another, preserving the roster row
    /// position. The caller validates both ids and handles backfill.
    fn replace_session_bot(
        &self,
        session_id: &str,
        old_bot_id: &str,
        new_bot_id: &str,
    ) -> Result<bool>;
    /// Remove one bot from a session roster (liveness trim). The caller shrinks
    /// the quorum and purges the bot's pending outbox.
    fn remove_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool>;
    /// Adjust a session's quorum (liveness trim after a roster drop).
    fn set_session_quorum(&self, session_id: &str, quorum_n: i64) -> Result<()>;
    /// Update the authoritative chair identity for a session. Used when replacing
    /// the current chair with another chair-capable bot.
    fn set_session_chair(&self, session_id: &str, chair_bot: &str) -> Result<()>;
    fn set_state(&self, session_id: &str, state: SessionState) -> Result<()>;
    fn advance_state(&self, session_id: &str, from: SessionState, to: SessionState)
        -> Result<bool>;
    /// Close from *any* non-terminal state (the liveness watchdog — the current
    /// state is unknown when a timeout fires). CAS so only one caller wins;
    /// returns true if this call performed the close.
    fn close_if_active(&self, session_id: &str) -> Result<bool>;
    /// Record the structured verdict (ADR 013) parsed from the chair's
    /// `[[verdict:…]]` trailer. Called once at normal close; never on timeout.
    fn set_session_verdict(
        &self,
        session_id: &str,
        decision: &str,
        red: Option<i64>,
        yellow: Option<i64>,
        green: Option<i64>,
    ) -> Result<()>;
    /// Non-terminal session ids created before `cutoff_ms` — watchdog candidates.
    fn active_sessions_before(&self, cutoff_ms: i64) -> Result<Vec<String>>;
    fn roster(&self, session_id: &str) -> Result<Vec<String>>;
    /// A non-terminal session carrying this `trigger_ref`, if any. Makes
    /// webhook-driven creation idempotent: GitHub re-delivers on 5xx, so a retried
    /// PR event must not open a second council for the same PR.
    fn active_session_for_trigger(&self, trigger_ref: &str) -> Result<Option<String>>;

    /// Runtime standing council override. None means use `OABCP_COUNCIL_ROSTER`
    /// or the built-in default.
    fn standing_roster(&self) -> Result<Option<Vec<String>>>;
    fn set_standing_roster(&self, roster: &[String]) -> Result<()>;

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
    fn reactions(&self, session_id: &str) -> Result<Vec<Reaction>>;
    fn reactors_in_session(&self, session_id: &str, emoji: &str) -> Result<Vec<String>>;

    /// Durable per-bot outbox (offline delivery). Frames queued here are flushed
    /// in `seq` order when the bot is connected; a disconnected bot keeps them
    /// and gets them on reconnect. `ack_outbox` removes a delivered frame.
    fn enqueue_outbox(
        &self,
        bot_id: &str,
        session_id: &str,
        idem_key: &str,
        frame: &str,
    ) -> Result<()>;
    fn pending_outbox(&self, bot_id: &str) -> Result<Vec<(i64, String)>>;
    fn ack_outbox(&self, seq: i64) -> Result<()>;
    fn purge_outbox_for_session_bot(&self, session_id: &str, bot_id: &str) -> Result<()>;

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
    /// Every cached token string for a session (any role). Read before `purge_*` so
    /// the close path can revoke each one GitHub-side.
    fn session_installation_tokens(&self, session_id: &str) -> Result<Vec<String>>;
    /// Central revoke: drop every scoped token for a session. Called when the session
    /// closes so a pod can't keep acting on GitHub after the verdict.
    fn purge_installation_tokens(&self, session_id: &str) -> Result<()>;
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS bots (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
    token_hash TEXT NOT NULL, token_plain TEXT,
    connected INTEGER NOT NULL DEFAULT 0, last_seen INTEGER,
    provider TEXT, capabilities TEXT NOT NULL DEFAULT '[]',
    enabled INTEGER NOT NULL DEFAULT 1,
    health TEXT NOT NULL DEFAULT 'ok',
    note TEXT, version TEXT, runtime TEXT,
    source TEXT NOT NULL DEFAULT 'registered'
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
    trigger_ref TEXT, quorum_n INTEGER NOT NULL, chair_bot TEXT,
    created_at INTEGER NOT NULL, closed_at INTEGER,
    mode TEXT NOT NULL DEFAULT 'council',
    decision TEXT, findings_red INTEGER, findings_yellow INTEGER, findings_green INTEGER
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
    bot_id TEXT NOT NULL, session_id TEXT, idem_key TEXT, frame TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbox_bot ON outbox(bot_id, seq);
CREATE INDEX IF NOT EXISTS idx_outbox_session_bot ON outbox(session_id, bot_id);
-- Idempotency key = "{bot_id}:{message_id}". UNIQUE makes enqueue idempotent while a
-- frame is still pending, so a retry / backfill re-enqueue of the same logical message
-- to the same bot can't queue a duplicate (C2). NULLs (legacy rows) are distinct in
-- SQLite, so old frames are unaffected. Cleared on ack, so legit re-delivery still works.
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_idem ON outbox(idem_key);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY, value TEXT NOT NULL
);
-- KNOWN GAP (#4): `token` is stored in plaintext. GitHub installation tokens are
-- short-lived (≤1h) bearer credentials; until encryption-at-rest lands (AES-GCM with
-- a KMS-derived key) the DB file itself must be access-controlled. Fast-follow.
CREATE TABLE IF NOT EXISTS installation_tokens (
    session_id TEXT NOT NULL, role TEXT NOT NULL,
    token TEXT NOT NULL, expires_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, role)
);
"#;

/// Additive migrations for DBs created before a column or index existed. Each
/// `ALTER` errors with "duplicate column" once the column is present — ignored.
/// ponytail: no migration framework; one guarded ALTER per added column.
fn migrate(conn: &Connection) -> Result<()> {
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN mode TEXT NOT NULL DEFAULT 'council'",
        [],
    );
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN decision TEXT", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN findings_red INTEGER", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN findings_yellow INTEGER", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN findings_green INTEGER", []);
    let _ = conn.execute("ALTER TABLE outbox ADD COLUMN session_id TEXT", []);
    let _ = conn.execute("ALTER TABLE outbox ADD COLUMN idem_key TEXT", []);
    let _ = conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_idem ON outbox(idem_key)",
        [],
    );
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN provider TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE bots ADD COLUMN capabilities TEXT NOT NULL DEFAULT '[]'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE bots ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE bots ADD COLUMN health TEXT NOT NULL DEFAULT 'ok'",
        [],
    );
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN note TEXT", []);
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN version TEXT", []);
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN runtime TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE bots ADD COLUMN source TEXT NOT NULL DEFAULT 'registered'",
        [],
    );
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_outbox_session_bot ON outbox(session_id, bot_id)",
        [],
    )?;
    ensure_no_duplicate_active_trigger_refs(conn)?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_active_trigger_ref
         ON sessions(trigger_ref)
         WHERE trigger_ref IS NOT NULL AND state NOT IN ('closed', 'aborted')",
        [],
    )?;
    Ok(())
}

fn ensure_no_duplicate_active_trigger_refs(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT trigger_ref
         FROM sessions
         WHERE trigger_ref IS NOT NULL AND state NOT IN ('closed', 'aborted')
         GROUP BY trigger_ref
         HAVING COUNT(*) > 1
         ORDER BY trigger_ref",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let duplicates = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    for trigger_ref in duplicates {
        let mut sessions = conn.prepare(
            "SELECT id
             FROM sessions
             WHERE trigger_ref = ?1 AND state NOT IN ('closed', 'aborted')
             ORDER BY created_at DESC, id DESC",
        )?;
        let ids = sessions
            .query_map(params![trigger_ref], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for stale in ids.iter().skip(1) {
            conn.execute(
                "UPDATE sessions SET state = 'aborted', closed_at = ?2 WHERE id = ?1",
                params![stale, now_ms()],
            )?;
        }
    }
    Ok(())
}

fn is_constraint_violation(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|err| match err {
                rusqlite::Error::SqliteFailure(code, _) => {
                    code.code == ErrorCode::ConstraintViolation
                }
                _ => false,
            })
    })
}

fn capabilities_json(capabilities: &[String]) -> String {
    serde_json::to_string(capabilities).unwrap_or_else(|_| "[]".to_string())
}

fn runtime_json(runtime: &Option<Value>) -> Option<String> {
    runtime
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
}

fn parse_capabilities(raw: String) -> Vec<String> {
    serde_json::from_str(&raw).unwrap_or_default()
}

fn parse_runtime(raw: Option<String>) -> Option<Value> {
    raw.and_then(|value| serde_json::from_str(&value).ok())
}

fn map_bot_inventory(r: &rusqlite::Row<'_>) -> rusqlite::Result<BotInventory> {
    Ok(BotInventory {
        id: r.get(0)?,
        name: r.get(1)?,
        role: r.get(2)?,
        provider: r.get(3)?,
        capabilities: parse_capabilities(r.get(4)?),
        connected: r.get::<_, i64>(5)? != 0,
        enabled: r.get::<_, i64>(6)? != 0,
        health: r.get(7)?,
        note: r.get(8)?,
        version: r.get(9)?,
        runtime: parse_runtime(r.get(10)?),
        last_seen_ms: r.get(11)?,
        source: r.get(12)?,
    })
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
        migrate(&conn)?;
        Ok(SqliteStore {
            conn: Mutex::new(conn),
        })
    }

    pub fn memory() -> Result<SqliteStore> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(SqliteStore {
            conn: Mutex::new(conn),
        })
    }

    fn thread_locked(&self, c: &Connection, session_id: &str) -> Result<Option<String>> {
        Ok(c.query_row(
            "SELECT id FROM threads WHERE session_id = ?1",
            params![session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    }

    fn active_session_for_trigger_locked(
        c: &Connection,
        trigger_ref: &str,
    ) -> Result<Option<Session>> {
        Ok(c.query_row(
            "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green
             FROM sessions
             WHERE trigger_ref = ?1 AND state NOT IN ('closed', 'aborted')
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            params![trigger_ref],
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
                    decision: r.get(9)?,
                    findings_red: r.get(10)?,
                    findings_yellow: r.get(11)?,
                    findings_green: r.get(12)?,
                })
            },
        )
        .optional()?)
    }
}

impl Store for SqliteStore {
    fn register_bot(
        &self,
        name: &str,
        role: &str,
        token_hash: &str,
        token_plain: &str,
    ) -> Result<Bot> {
        let id = new_id("bot");
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO bots (id, name, role, token_hash, token_plain, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 'registered')",
            params![id, name, role, token_hash, token_plain],
        )?;
        Ok(Bot {
            id,
            name: name.to_string(),
            role: role.to_string(),
        })
    }

    fn seed_bot(
        &self,
        id: &str,
        name: &str,
        role: &str,
        token_hash: &str,
        token_plain: &str,
    ) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO bots (id, name, role, token_hash, token_plain, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 'seeded')",
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
                |r| {
                    Ok(Bot {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        role: r.get(2)?,
                    })
                },
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
                |r| {
                    Ok(Bot {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        role: r.get(2)?,
                    })
                },
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

    fn list_bots(&self) -> Result<Vec<BotInventory>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id, name, role, provider, capabilities, connected, enabled,
                    health, note, version, runtime, last_seen, source
             FROM bots
             ORDER BY id ASC",
        )?;
        let bots = stmt
            .query_map([], map_bot_inventory)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(bots)
    }

    fn bot_inventory(&self, id: &str) -> Result<Option<BotInventory>> {
        let c = self.conn.lock().unwrap();
        let bot = c
            .query_row(
                "SELECT id, name, role, provider, capabilities, connected, enabled,
                        health, note, version, runtime, last_seen, source
                 FROM bots
                 WHERE id = ?1",
                params![id],
                map_bot_inventory,
            )
            .optional()?;
        Ok(bot)
    }

    fn discover_bot(
        &self,
        id: &str,
        name: Option<&str>,
        role: &str,
        metadata: &BotMetadata,
    ) -> Result<(Bot, bool)> {
        let capabilities = metadata.capabilities.as_deref().map(capabilities_json);
        let runtime = runtime_json(&metadata.runtime);
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let source = tx
            .query_row("SELECT source FROM bots WHERE id = ?1", params![id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        let inserted = if source.is_some() {
            tx.execute(
                "UPDATE bots
                 SET provider = COALESCE(?2, provider),
                     capabilities = CASE WHEN ?3 THEN ?4 ELSE capabilities END,
                     version = COALESCE(?5, version),
                     runtime = COALESCE(?6, runtime)
                 WHERE id = ?1",
                params![
                    id,
                    metadata.provider.as_deref(),
                    metadata.capabilities.is_some(),
                    capabilities.as_deref(),
                    metadata.version.as_deref(),
                    runtime.as_deref()
                ],
            )?;
            false
        } else {
            let token = format!("oabct_{}", uuid::Uuid::new_v4().simple());
            let display_name = name.unwrap_or(id);
            tx.execute(
                "INSERT INTO bots
                (id, name, role, token_hash, token_plain, provider, capabilities,
                 version, runtime, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'discovered')",
                params![
                    id,
                    display_name,
                    role,
                    crate::identity::hash_token(&token),
                    token.as_str(),
                    metadata.provider.as_deref(),
                    capabilities.as_deref().unwrap_or("[]"),
                    metadata.version.as_deref(),
                    runtime.as_deref()
                ],
            )?;
            true
        };
        let bot = tx.query_row(
            "SELECT id, name, role FROM bots WHERE id = ?1",
            params![id],
            |r| {
                Ok(Bot {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    role: r.get(2)?,
                })
            },
        )?;
        tx.commit()?;
        Ok((bot, inserted))
    }

    fn update_bot_metadata(&self, id: &str, patch: &BotMetadataPatch) -> Result<bool> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let exists = tx
            .query_row("SELECT 1 FROM bots WHERE id = ?1", params![id], |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        if let Some(provider) = &patch.provider {
            tx.execute(
                "UPDATE bots SET provider = ?2 WHERE id = ?1",
                params![id, provider.as_deref()],
            )?;
        }
        if let Some(capabilities) = &patch.capabilities {
            tx.execute(
                "UPDATE bots SET capabilities = ?2 WHERE id = ?1",
                params![id, capabilities_json(capabilities)],
            )?;
        }
        if let Some(enabled) = patch.enabled {
            tx.execute(
                "UPDATE bots SET enabled = ?2 WHERE id = ?1",
                params![id, enabled as i64],
            )?;
        }
        if let Some(health) = &patch.health {
            tx.execute(
                "UPDATE bots SET health = ?2 WHERE id = ?1",
                params![id, health],
            )?;
        }
        if let Some(note) = &patch.note {
            tx.execute(
                "UPDATE bots SET note = ?2 WHERE id = ?1",
                params![id, note.as_deref()],
            )?;
        }
        if let Some(version) = &patch.version {
            tx.execute(
                "UPDATE bots SET version = ?2 WHERE id = ?1",
                params![id, version.as_deref()],
            )?;
        }
        if let Some(runtime) = &patch.runtime {
            let runtime = runtime.as_ref().map(serde_json::to_string).transpose()?;
            tx.execute(
                "UPDATE bots SET runtime = ?2 WHERE id = ?1",
                params![id, runtime.as_deref()],
            )?;
        }
        tx.commit()?;
        Ok(true)
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
            decision: None,
            findings_red: None,
            findings_yellow: None,
            findings_green: None,
        })
    }

    fn create_session_deduped(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<(Session, bool)> {
        if let Some(trigger_ref) = trigger_ref {
            let c = self.conn.lock().unwrap();
            if let Some(existing) = Self::active_session_for_trigger_locked(&c, trigger_ref)? {
                return Ok((existing, true));
            }
        }

        match self.create_session(title, trigger_ref, quorum_n, chair_bot, roster, mode) {
            Ok(session) => Ok((session, false)),
            Err(err) if trigger_ref.is_some() && is_constraint_violation(&err) => {
                let c = self.conn.lock().unwrap();
                let trigger_ref = trigger_ref.expect("checked by is_some guard");
                if let Some(existing) = Self::active_session_for_trigger_locked(&c, trigger_ref)? {
                    return Ok((existing, true));
                }
                Err(err).with_context(|| {
                    format!(
                        "active trigger_ref conflict for '{trigger_ref}' but no active session was found"
                    )
                })
            }
            Err(err) => Err(err),
        }
    }

    fn session(&self, id: &str) -> Result<Option<Session>> {
        let c = self.conn.lock().unwrap();
        let s = c
            .query_row(
                "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green
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
                        decision: r.get(9)?,
                        findings_red: r.get(10)?,
                        findings_yellow: r.get(11)?,
                        findings_green: r.get(12)?,
                    })
                },
            )
            .optional()?;
        Ok(s)
    }

    fn list_sessions(
        &self,
        trigger_ref: Option<&str>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Session>> {
        fn map_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
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
                decision: r.get(9)?,
                findings_red: r.get(10)?,
                findings_yellow: r.get(11)?,
                findings_green: r.get(12)?,
            })
        }

        let c = self.conn.lock().unwrap();
        let limit = limit as i64;
        let sessions = match (trigger_ref, state) {
            (Some(trigger_ref), Some(state)) => {
                let mut stmt = c.prepare(
                    "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green
                     FROM sessions
                     WHERE trigger_ref = ?1 AND state = ?2
                     ORDER BY created_at DESC, rowid DESC
                     LIMIT ?3",
                )?;
                let rows: Vec<Session> = stmt
                    .query_map(params![trigger_ref, state, limit], map_session)?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            }
            (Some(trigger_ref), None) => {
                let mut stmt = c.prepare(
                    "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green
                     FROM sessions
                     WHERE trigger_ref = ?1
                     ORDER BY created_at DESC, rowid DESC
                     LIMIT ?2",
                )?;
                let rows: Vec<Session> = stmt
                    .query_map(params![trigger_ref, limit], map_session)?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            }
            (None, Some(state)) => {
                let mut stmt = c.prepare(
                    "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green
                     FROM sessions
                     WHERE state = ?1
                     ORDER BY created_at DESC, rowid DESC
                     LIMIT ?2",
                )?;
                let rows: Vec<Session> = stmt
                    .query_map(params![state, limit], map_session)?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            }
            (None, None) => {
                let mut stmt = c.prepare(
                    "SELECT id, title, state, trigger_ref, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green
                     FROM sessions
                     ORDER BY created_at DESC, rowid DESC
                     LIMIT ?1",
                )?;
                let rows: Vec<Session> = stmt
                    .query_map(params![limit], map_session)?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            }
        };
        Ok(sessions)
    }

    fn add_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO session_bots (session_id, bot_id) VALUES (?1, ?2)",
            params![session_id, bot_id],
        )?;
        Ok(n == 1)
    }

    fn remove_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "DELETE FROM session_bots WHERE session_id = ?1 AND bot_id = ?2",
            params![session_id, bot_id],
        )?;
        Ok(n == 1)
    }

    fn set_session_quorum(&self, session_id: &str, quorum_n: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE sessions SET quorum_n = ?2 WHERE id = ?1",
            params![session_id, quorum_n],
        )?;
        Ok(())
    }

    fn replace_session_bot(
        &self,
        session_id: &str,
        old_bot_id: &str,
        new_bot_id: &str,
    ) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "UPDATE session_bots
             SET bot_id = ?3
             WHERE session_id = ?1 AND bot_id = ?2",
            params![session_id, old_bot_id, new_bot_id],
        )?;
        Ok(n == 1)
    }

    fn set_session_chair(&self, session_id: &str, chair_bot: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE sessions SET chair_bot = ?2 WHERE id = ?1",
            params![session_id, chair_bot],
        )?;
        Ok(())
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

    fn set_session_verdict(
        &self,
        session_id: &str,
        decision: &str,
        red: Option<i64>,
        yellow: Option<i64>,
        green: Option<i64>,
    ) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE sessions SET decision = ?2, findings_red = ?3,
                    findings_yellow = ?4, findings_green = ?5
             WHERE id = ?1",
            params![session_id, decision, red, yellow, green],
        )?;
        Ok(())
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
        // create_session. Pipeline stage order rides on this; council uses it
        // only for stable fanout, while chair identity comes from `chair_bot`.
        let mut stmt =
            c.prepare("SELECT bot_id FROM session_bots WHERE session_id = ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn active_session_for_trigger(&self, trigger_ref: &str) -> Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        Ok(Self::active_session_for_trigger_locked(&c, trigger_ref)?.map(|session| session.id))
    }

    fn standing_roster(&self) -> Result<Option<Vec<String>>> {
        let c = self.conn.lock().unwrap();
        let value = c
            .query_row(
                "SELECT value FROM settings WHERE key = 'council_roster'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|raw| serde_json::from_str(&raw).context("decode council_roster setting"))
            .transpose()
    }

    fn set_standing_roster(&self, roster: &[String]) -> Result<()> {
        let value = serde_json::to_string(roster)?;
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO settings (key, value)
             VALUES ('council_roster', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![value],
        )?;
        Ok(())
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

    fn reactions(&self, session_id: &str) -> Result<Vec<Reaction>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT r.message_id, r.bot_id, r.emoji
             FROM reactions r
             JOIN messages m ON m.id = r.message_id
             WHERE m.session_id = ?1
             ORDER BY m.created_at, r.message_id, r.bot_id, r.emoji",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(Reaction {
                message_id: r.get(0)?,
                bot_id: r.get(1)?,
                emoji: r.get(2)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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

    fn enqueue_outbox(
        &self,
        bot_id: &str,
        session_id: &str,
        idem_key: &str,
        frame: &str,
    ) -> Result<()> {
        let c = self.conn.lock().unwrap();
        // OR IGNORE: a duplicate idem_key means the same frame is already pending —
        // dropping the second insert is the whole point (idempotent enqueue).
        c.execute(
            "INSERT OR IGNORE INTO outbox (bot_id, session_id, idem_key, frame, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![bot_id, session_id, idem_key, frame, now_ms()],
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

    fn purge_outbox_for_session_bot(&self, session_id: &str, bot_id: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM outbox WHERE session_id = ?1 AND bot_id = ?2",
            params![session_id, bot_id],
        )?;
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

    fn session_installation_tokens(&self, session_id: &str) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT token FROM installation_tokens WHERE session_id = ?1")?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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

        let s = store
            .create_session("Review o/r#1", Some(pr), 0, None, &[], "council")
            .unwrap();
        // an open session with this trigger is found → webhook retry is idempotent
        assert_eq!(
            store.active_session_for_trigger(pr).unwrap().as_deref(),
            Some(s.id.as_str())
        );

        // once closed, the same PR can open a fresh council (e.g. a later push)
        store.set_state(&s.id, SessionState::Closed).unwrap();
        assert!(store.active_session_for_trigger(pr).unwrap().is_none());
    }

    #[test]
    fn enqueue_outbox_is_idempotent_per_idem_key() {
        let store = SqliteStore::memory().unwrap();
        let key = "bot1:msg1";
        store.enqueue_outbox("bot1", "s1", key, "frameA").unwrap();
        // Same key (retry / backfill re-enqueue) is dropped — no duplicate frame.
        store.enqueue_outbox("bot1", "s1", key, "frameA").unwrap();
        assert_eq!(store.pending_outbox("bot1").unwrap().len(), 1);

        // A different message to the same bot still queues.
        store
            .enqueue_outbox("bot1", "s1", "bot1:msg2", "frameB")
            .unwrap();
        assert_eq!(store.pending_outbox("bot1").unwrap().len(), 2);

        // After ack the key frees up, so a legit re-delivery works again.
        let seq = store.pending_outbox("bot1").unwrap()[0].0;
        store.ack_outbox(seq).unwrap();
        store.enqueue_outbox("bot1", "s1", key, "frameA").unwrap();
        assert_eq!(store.pending_outbox("bot1").unwrap().len(), 2);
    }

    #[test]
    fn active_trigger_ref_is_unique_until_terminal() {
        let store = SqliteStore::memory().unwrap();
        let trigger = "github:pr/o/r#1";
        let first = store
            .create_session("Review o/r#1", Some(trigger), 0, None, &[], "council")
            .unwrap();

        assert!(store
            .create_session("Review o/r#1 retry", Some(trigger), 0, None, &[], "council")
            .is_err());

        store.set_state(&first.id, SessionState::Closed).unwrap();
        assert!(store
            .create_session("Review o/r#1 again", Some(trigger), 0, None, &[], "council")
            .is_ok());
    }

    #[test]
    fn create_session_deduped_returns_existing_active_trigger() {
        let store = SqliteStore::memory().unwrap();
        let trigger = "github:pr/o/r#2";

        let (first, deduped) = store
            .create_session_deduped("Review o/r#2", Some(trigger), 0, None, &[], "council")
            .unwrap();
        assert!(!deduped);

        let (second, deduped) = store
            .create_session_deduped("Review o/r#2 retry", Some(trigger), 0, None, &[], "council")
            .unwrap();
        assert!(deduped);
        assert_eq!(second.id, first.id);
        assert_eq!(second.title, "Review o/r#2");
    }

    #[test]
    fn create_session_deduped_without_trigger_ref_creates_distinct_sessions() {
        let store = SqliteStore::memory().unwrap();

        let (first, deduped) = store
            .create_session_deduped("manual", None, 0, None, &[], "council")
            .unwrap();
        assert!(!deduped);

        let (second, deduped) = store
            .create_session_deduped("manual", None, 0, None, &[], "council")
            .unwrap();
        assert!(!deduped);
        assert_ne!(second.id, first.id);
    }

    #[test]
    fn list_sessions_filters_by_trigger_state_and_limit() {
        let store = SqliteStore::memory().unwrap();
        let trigger = "github:pr/o/r#3";
        let first = store
            .create_session("Review o/r#3", Some(trigger), 0, None, &[], "council")
            .unwrap();
        store.set_state(&first.id, SessionState::Closed).unwrap();
        let second = store
            .create_session("Review o/r#3 again", Some(trigger), 0, None, &[], "council")
            .unwrap();
        let other = store
            .create_session("manual", None, 0, None, &[], "solo")
            .unwrap();

        let by_trigger = store.list_sessions(Some(trigger), None, 10).unwrap();
        assert_eq!(by_trigger.len(), 2);
        assert_eq!(by_trigger[0].id, second.id);
        assert_eq!(by_trigger[1].id, first.id);

        let closed = store
            .list_sessions(Some(trigger), Some("closed"), 10)
            .unwrap();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id, first.id);

        let latest = store.list_sessions(None, None, 1).unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].id, other.id);
    }

    #[test]
    fn reactions_returns_only_the_requested_session() {
        let store = SqliteStore::memory().unwrap();
        let s1 = store
            .create_session("one", None, 0, None, &[], "council")
            .unwrap();
        let s2 = store
            .create_session("two", None, 0, None, &[], "council")
            .unwrap();
        let m1 = store
            .add_message(&s1.id, None, "bot", Some("rev1"), "done", None)
            .unwrap();
        let m2 = store
            .add_message(&s2.id, None, "bot", Some("rev2"), "done", None)
            .unwrap();
        store.add_reaction(&m1.id, "rev1", "🆗").unwrap();
        store.add_reaction(&m2.id, "rev2", "🆗").unwrap();

        let reactions = store.reactions(&s1.id).unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].message_id, m1.id);
        assert_eq!(reactions[0].bot_id, "rev1");
    }

    #[test]
    fn duplicate_active_trigger_ref_preflight_aborts_stale_duplicates() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (id, title, state, trigger_ref, quorum_n, created_at, mode)
             VALUES ('s1', 't', 'open', 'dup', 0, 1, 'council');
             INSERT INTO sessions (id, title, state, trigger_ref, quorum_n, created_at, mode)
             VALUES ('s2', 't', 'deliberating', 'dup', 0, 2, 'council');",
        )
        .unwrap();

        ensure_no_duplicate_active_trigger_refs(&conn).unwrap();
        let active: Vec<String> = conn
            .prepare(
                "SELECT id FROM sessions
                 WHERE trigger_ref = 'dup' AND state NOT IN ('closed', 'aborted')
                 ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(active, vec!["s2"]);
        let stale_state: String = conn
            .query_row("SELECT state FROM sessions WHERE id = 's1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stale_state, "aborted");
    }
}
