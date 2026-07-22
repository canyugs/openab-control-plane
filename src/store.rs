//! Store trait + SQLite impl + domain types (design §8, §6c).
//!
//! The `Store` trait is the backing-service seam: SQLite today (spike), a
//! networked DB (Postgres/libSQL) for production — callers depend on the trait,
//! so the swap touches only this file. ponytail: one impl for now, but the seam
//! is deliberate (see design §6c "12-factor posture").

use anyhow::{Context, Result};
use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
    pub enabled: bool,
    pub health: String,
    pub note: Option<String>,
    pub version: Option<String>,
    pub runtime: Option<Value>,
    pub last_seen_ms: Option<i64>,
    pub source: String,
}

/// Result of one passive agent-liveness accounting step (ADR 023 Phase 1). Lets
/// the ingest caller WARN exactly once per health crossing instead of on every
/// error frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotHealthTransition {
    /// No health change this frame (still `ok`, or still `degraded`).
    None,
    /// The consecutive-error count just crossed the threshold: `ok -> degraded`.
    Degraded,
    /// A content frame cleared a prior `degraded`: `degraded -> ok` (observed
    /// recovery — the agent is producing output again).
    Recovered,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteBotOutcome {
    Deleted,
    NotFound,
    ActiveSession,
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
    pub trigger_fingerprint: Option<String>,
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
    /// Settled result identity (ADR 028), recorded at normal close: the author
    /// whose settlement closed the session and the ordered JSON array of its
    /// message ids. NULL on timeout close and for legacy rows. Skipped from the
    /// flat serialization — the read surface exposes them as the `result`
    /// object on the session-detail response (the LIST wire shape is frozen).
    #[serde(skip)]
    pub result_author_id: Option<String>,
    #[serde(skip)]
    pub result_message_ids: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCreateOutcome {
    Created,
    Deduped,
    Superseded { old_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub author_kind: String, // "bot" | "client" | "system"
    pub author_id: Option<String>,
    /// NULL = broadcast; Some(bot_id) = scoped to one bot/seat owner.
    pub audience: Option<String>,
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

/// One findings-ledger row (ADR 020): a finding as reported by one review
/// round. Written once at session close from the chair's hidden
/// `<!-- openab-findings … -->` block; the kernel stores plain rows and knows
/// nothing about the block format (that parse lives in the pr_review plugin).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFinding {
    pub id: i64,
    pub session_id: String,
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    /// PR-scoped stable id, e.g. `F1` (monotonic across rounds per steering).
    pub stable_id: String,
    pub severity: String, // red | yellow | green
    pub status: String,   // open | resolved | dismissed
    pub title: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub raised_by: Option<String>,
    pub angle: Option<String>,
    pub head_sha: Option<String>,
    pub created_at: i64,
}

/// A review the hourly cap dropped, awaiting catch-up (SEI-819).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReview {
    pub trigger_ref: String,
    pub repo: String,
    pub pr_number: i64,
    pub fingerprint: Option<String>,
    pub preset: Option<String>,
    pub requested_at: i64,
}

/// Insert shape for `insert_review_findings` — everything but the row id and
/// the shared per-call fields (session, repo/pr, head_sha, timestamp).
#[derive(Debug, Clone)]
pub struct NewReviewFinding {
    pub stable_id: String,
    pub severity: String,
    pub status: String,
    pub title: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub raised_by: Option<String>,
    pub angle: Option<String>,
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
    /// Names of bots whose plaintext token is still stored (legacy mode). Used at
    /// boot to decide whether an unset OABCP_EXTERNALIZE_TOKENS may safely default on.
    fn bots_with_plaintext_token(&self) -> Result<Vec<String>>;
    fn touch_last_seen(&self, bot_id: &str) -> Result<()>;
    /// Persist a specific last-seen timestamp (e.g. the in-memory ws activity
    /// clock). Lets the gateway advance `last_seen` on ongoing traffic, not just
    /// at connect/disconnect, so `/v1/bots` reports true liveness. (SEI-785)
    fn touch_last_seen_at(&self, bot_id: &str, ts: i64) -> Result<()>;
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
    fn delete_bot(&self, bot_id: &str) -> Result<DeleteBotOutcome>;

    /// Passive agent-liveness accounting for one bot turn (ADR 023 Phase 1).
    /// `is_error` is true when the turn produced an error frame (e.g. JSON-RPC
    /// `-32603`) instead of model output. An error increments the per-bot
    /// consecutive-error counter and stamps `last_error_at`; crossing `threshold`
    /// flips `health` to `degraded`. A content frame resets the counter and clears
    /// a prior `degraded` back to `ok` (observed recovery — not auto-remediation).
    /// Returns the health transition so the caller WARNs once per crossing.
    /// A no-op (returns `None`) for an unknown bot id.
    fn record_bot_frame(
        &self,
        bot_id: &str,
        is_error: bool,
        threshold: i64,
    ) -> Result<BotHealthTransition>;

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
    /// Open a session for `trigger_ref` using fingerprint-aware idempotency.
    /// Equal non-NULL fingerprints dedupe to the active row. A different or NULL
    /// fingerprint closes the active row and inserts the successor in one txn.
    #[allow(clippy::too_many_arguments)]
    fn create_session_superseding(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        trigger_fingerprint: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<(Session, SessionCreateOutcome)>;
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
    /// Atomic normal close (ADR 028): one guarded UPDATE (one implicit SQLite
    /// transaction) that performs the close CAS (`from`→closed, same guard as
    /// `advance_state`) AND records the structured verdict (ADR 013) AND the
    /// settled-result identity. Either everything lands or nothing does: a
    /// failure leaves the session in `from` (the watchdog timeout path remains
    /// the termination backstop) — a normal close can never be observed
    /// closed-without-result. Returns true iff this call performed the close.
    #[allow(clippy::too_many_arguments)]
    fn close_session_with_result(
        &self,
        session_id: &str,
        from: SessionState,
        decision: Option<&str>,
        red: Option<i64>,
        yellow: Option<i64>,
        green: Option<i64>,
        result_author_id: Option<&str>,
        result_message_ids_json: Option<&str>,
    ) -> Result<bool>;
    /// Reopen a terminal session for a follow-up turn (ADR 011 solo pattern),
    /// clearing the recorded result identity in the same guarded UPDATE — the
    /// previous turn's span must not survive as the reopened session's result
    /// (ADR 028). Returns true iff the CAS won.
    fn reopen_session(&self, session_id: &str, from: SessionState) -> Result<bool>;
    /// Append findings-ledger rows for a closed review round (ADR 020). Called
    /// at most once per session (guarded by the close CAS upstream).
    fn insert_review_findings(
        &self,
        session_id: &str,
        repo: Option<&str>,
        pr_number: Option<i64>,
        head_sha: Option<&str>,
        findings: &[NewReviewFinding],
    ) -> Result<()>;
    /// Read the findings ledger, newest rows first. All filters optional.
    fn review_findings(
        &self,
        repo: Option<&str>,
        pr_number: Option<i64>,
        status: Option<&str>,
        severity: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ReviewFinding>>;
    /// Atomic once-marker on the settings table: true exactly once per key
    /// (INSERT OR IGNORE). Used for once-per-PR notices (SEI-820).
    fn mark_once(&self, key: &str) -> Result<bool>;
    /// Record a synchronize the hourly cap dropped (SEI-819); newest drop wins.
    fn upsert_pending_review(
        &self,
        trigger_ref: &str,
        repo: &str,
        pr_number: i64,
        fingerprint: Option<&str>,
        preset: Option<&str>,
    ) -> Result<()>;
    /// All cap-dropped reviews awaiting catch-up.
    fn pending_reviews(&self) -> Result<Vec<PendingReview>>;
    fn delete_pending_review(&self, trigger_ref: &str) -> Result<()>;
    /// Newest session created_at for this trigger_ref (any state), if any.
    fn latest_session_created_at(&self, trigger_ref: &str) -> Result<Option<i64>>;
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
        audience: Option<&str>,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<Message>;
    fn edit_message(&self, message_id: &str, content: &str) -> Result<()>;
    fn message(&self, id: &str) -> Result<Option<Message>>;
    fn messages(&self, session_id: &str) -> Result<Vec<Message>>;

    fn add_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()>;
    fn remove_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()>;
    fn reactions(&self, session_id: &str) -> Result<Vec<Reaction>>;
    fn done_voters(&self, session_id: &str) -> Result<Vec<String>>;

    /// Durable per-bot outbox (offline delivery). Frames queued here are flushed
    /// in `seq` order when the bot is connected; a disconnected bot keeps them
    /// and gets them on reconnect. `ack_outbox` marks a frame delivered; the row
    /// is retained so `idem_key` dedup survives ack until session outbox purge.
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
    /// Drop every durable frame for a terminal/superseded session, across all bots.
    /// pr-mention-plan P1 supersede should use this same primitive after commit.
    fn purge_outbox_for_session(&self, session_id: &str) -> Result<()>;
    /// Boot backstop for crash-between-close-and-purge plus legacy NULL-session rows.
    fn purge_terminal_outbox(&self) -> Result<()>;

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

    /// Read-only observability snapshot: session outcome aggregates (state split,
    /// 24h throughput, time-to-verdict p50/p95, mode/decision split, findings
    /// totals) + outbox backlog. Distribution only — NOT a quality signal (see C6).
    /// `now` is unix-ms so the caller controls the 24h window (testable).
    fn stats(&self, now: i64) -> Result<Value>;
}

/// p50/p95-style percentile over an unsorted slice (nearest-rank on the sorted
/// values). Empty → None. ponytail: exact enough for an eyeball metric; swap for
/// interpolation only if someone charts these.
fn percentile(values: &mut [i64], p: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    // clamp so a caller passing p>100 (or fp rounding at the top) can't index OOB.
    let idx = (((p / 100.0) * (values.len() - 1) as f64).round() as usize).min(values.len() - 1);
    Some(values[idx])
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS bots (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
    token_hash TEXT NOT NULL, token_plain TEXT,
    last_seen INTEGER,
    provider TEXT, capabilities TEXT NOT NULL DEFAULT '[]',
    enabled INTEGER NOT NULL DEFAULT 1,
    health TEXT NOT NULL DEFAULT 'ok',
    consecutive_errors INTEGER NOT NULL DEFAULT 0, last_error_at INTEGER,
    note TEXT, version TEXT, runtime TEXT,
    source TEXT NOT NULL DEFAULT 'registered'
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
    trigger_ref TEXT, trigger_fingerprint TEXT, quorum_n INTEGER NOT NULL, chair_bot TEXT,
    created_at INTEGER NOT NULL, closed_at INTEGER,
    mode TEXT NOT NULL DEFAULT 'council',
    decision TEXT, findings_red INTEGER, findings_yellow INTEGER, findings_green INTEGER,
    result_author_id TEXT, result_message_ids TEXT
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
    author_kind TEXT NOT NULL, author_id TEXT, audience TEXT, content TEXT NOT NULL,
    reply_to TEXT, created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, created_at);
CREATE TABLE IF NOT EXISTS reactions (
    message_id TEXT NOT NULL, bot_id TEXT NOT NULL, emoji TEXT NOT NULL,
    PRIMARY KEY (message_id, bot_id, emoji)
);
CREATE TABLE IF NOT EXISTS outbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    bot_id TEXT NOT NULL, session_id TEXT, idem_key TEXT, frame TEXT NOT NULL, created_at INTEGER NOT NULL,
    delivered_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_outbox_bot ON outbox(bot_id, seq);
-- Indexes on migrated-in columns (session_id, idem_key, delivered_at) live in
-- migrate(), AFTER the ALTERs that add those columns: on a legacy DB this table
-- already exists without them, and an index referencing a missing column aborts
-- the whole schema batch at boot (the 0.1.13 → delivered_at upgrade crash).
-- Idempotency key = "{bot_id}:{message_id}". A row per (bot_id, message_id)
-- persists from first enqueue until the session's outbox is purged (A5/trim/replace);
-- delivered_at NULL = pending. This is what makes idem_key dedup survive ack (A2).
-- NULLs (legacy rows) are distinct in SQLite, so old frames are unaffected.
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
-- ADR 020 findings ledger (PR-review plugin-owned). One row per finding per
-- review round; a session IS a round, so history across rounds = rows across
-- sessions of the same repo/pr. Append-only.
-- ponytail: no rounds/events tables yet — sessions already carries round
-- metadata; add pr_review_finding_events when resolve/dismiss commands land.
CREATE TABLE IF NOT EXISTS pr_review_findings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    repo TEXT, pr_number INTEGER,
    stable_id TEXT NOT NULL,
    severity TEXT NOT NULL,
    status TEXT NOT NULL,
    title TEXT NOT NULL,
    path TEXT, line INTEGER,
    raised_by TEXT, angle TEXT,
    head_sha TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_prf_repo_pr ON pr_review_findings(repo, pr_number, id);
-- Reviews the hourly cap dropped (SEI-819). One row per trigger_ref; a later
-- drop overwrites so the newest dropped head wins. The catch-up sweep convenes
-- and deletes once the cap window clears.
CREATE TABLE IF NOT EXISTS pending_reviews (
    trigger_ref TEXT PRIMARY KEY,
    repo TEXT NOT NULL, pr_number INTEGER NOT NULL,
    fingerprint TEXT, preset TEXT,
    requested_at INTEGER NOT NULL
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
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN findings_yellow INTEGER",
        [],
    );
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN findings_green INTEGER", []);
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN trigger_fingerprint TEXT",
        [],
    );
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN result_author_id TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE sessions ADD COLUMN result_message_ids TEXT",
        [],
    );
    let _ = conn.execute("ALTER TABLE messages ADD COLUMN audience TEXT", []);
    let _ = conn.execute("ALTER TABLE outbox ADD COLUMN session_id TEXT", []);
    let _ = conn.execute("ALTER TABLE outbox ADD COLUMN idem_key TEXT", []);
    let _ = conn.execute("ALTER TABLE outbox ADD COLUMN delivered_at INTEGER", []);
    // These reference the columns added just above, so they run here and not in
    // SCHEMA. Real errors must surface: a swallowed failure here means silent
    // full-table outbox scans and broken idem dedup, not a cosmetic miss.
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_idem ON outbox(idem_key)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(bot_id, seq) WHERE delivered_at IS NULL",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_outbox_session_bot ON outbox(session_id, bot_id)",
        [],
    )?;
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
    // ADR 023 Phase 1 — passive agent-liveness accounting.
    let _ = conn.execute(
        "ALTER TABLE bots ADD COLUMN consecutive_errors INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN last_error_at INTEGER", []);
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN note TEXT", []);
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN version TEXT", []);
    let _ = conn.execute("ALTER TABLE bots ADD COLUMN runtime TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE bots ADD COLUMN source TEXT NOT NULL DEFAULT 'registered'",
        [],
    );
    let _ = conn.execute("UPDATE bots SET connected = 0", []);
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_outbox_session_bot ON outbox(session_id, bot_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, created_at)",
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
        enabled: r.get::<_, i64>(5)? != 0,
        health: r.get(6)?,
        note: r.get(7)?,
        version: r.get(8)?,
        runtime: parse_runtime(r.get(9)?),
        last_seen_ms: r.get(10)?,
        source: r.get(11)?,
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
        // Boundary-review C1 measured a 10x delivery-path win with WAL + NORMAL.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
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
            "SELECT id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green,
                            result_author_id, result_message_ids
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
                    trigger_fingerprint: r.get(4)?,
                    quorum_n: r.get(5)?,
                    chair_bot: r.get(6)?,
                    created_at: r.get(7)?,
                    closed_at: r.get(8)?,
                    mode: r.get(9)?,
                    decision: r.get(10)?,
                    findings_red: r.get(11)?,
                    findings_yellow: r.get(12)?,
                    findings_green: r.get(13)?,
                    result_author_id: r.get(14)?,
                    result_message_ids: r.get(15)?,
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

    fn bots_with_plaintext_token(&self) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT name FROM bots
             WHERE token_plain IS NOT NULL AND token_plain != ''
             ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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

    fn touch_last_seen(&self, bot_id: &str) -> Result<()> {
        self.touch_last_seen_at(bot_id, now_ms())
    }

    fn touch_last_seen_at(&self, bot_id: &str, ts: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE bots SET last_seen = ?2 WHERE id = ?1",
            params![bot_id, ts],
        )?;
        Ok(())
    }

    fn list_bots(&self) -> Result<Vec<BotInventory>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT id, name, role, provider, capabilities, enabled,
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
                "SELECT id, name, role, provider, capabilities, enabled,
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

    fn delete_bot(&self, bot_id: &str) -> Result<DeleteBotOutcome> {
        let c = self.conn.lock().unwrap();
        let has_active_session = c
            .query_row(
                "SELECT 1
                 FROM session_bots sb
                 JOIN sessions s ON s.id = sb.session_id
                 WHERE sb.bot_id = ?1
                   AND s.state NOT IN ('closed', 'aborted')
                 LIMIT 1",
                params![bot_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if has_active_session {
            return Ok(DeleteBotOutcome::ActiveSession);
        }
        let deleted = c.execute("DELETE FROM bots WHERE id = ?1", params![bot_id])?;
        if deleted == 0 {
            Ok(DeleteBotOutcome::NotFound)
        } else {
            Ok(DeleteBotOutcome::Deleted)
        }
    }

    fn record_bot_frame(
        &self,
        bot_id: &str,
        is_error: bool,
        threshold: i64,
    ) -> Result<BotHealthTransition> {
        let c = self.conn.lock().unwrap();
        let Some((errors, health)) = c
            .query_row(
                "SELECT consecutive_errors, health FROM bots WHERE id = ?1",
                params![bot_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(BotHealthTransition::None); // unknown bot — nothing to record
        };

        if is_error {
            let next = errors + 1;
            // Cross into `degraded` exactly once — only when a prior-ok bot reaches
            // the threshold. Already-degraded bots keep counting but don't re-fire.
            if next >= threshold && health != "degraded" {
                c.execute(
                    "UPDATE bots SET consecutive_errors = ?2, last_error_at = ?3, health = 'degraded' WHERE id = ?1",
                    params![bot_id, next, now_ms()],
                )?;
                Ok(BotHealthTransition::Degraded)
            } else {
                c.execute(
                    "UPDATE bots SET consecutive_errors = ?2, last_error_at = ?3 WHERE id = ?1",
                    params![bot_id, next, now_ms()],
                )?;
                Ok(BotHealthTransition::None)
            }
        } else if health == "degraded" {
            // A content frame after `degraded` is evidence the agent recovered.
            c.execute(
                "UPDATE bots SET consecutive_errors = 0, health = 'ok' WHERE id = ?1",
                params![bot_id],
            )?;
            Ok(BotHealthTransition::Recovered)
        } else if errors != 0 {
            c.execute(
                "UPDATE bots SET consecutive_errors = 0 WHERE id = ?1",
                params![bot_id],
            )?;
            Ok(BotHealthTransition::None)
        } else {
            Ok(BotHealthTransition::None)
        }
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
            trigger_fingerprint: None,
            quorum_n,
            chair_bot: chair_bot.map(String::from),
            created_at,
            closed_at: None,
            mode: mode.to_string(),
            decision: None,
            findings_red: None,
            findings_yellow: None,
            findings_green: None,
            result_author_id: None,
            result_message_ids: None,
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

    fn create_session_superseding(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        trigger_fingerprint: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<(Session, SessionCreateOutcome)> {
        let id = new_id("ses");
        let created_at = now_ms();
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let mut outcome = SessionCreateOutcome::Created;

        if let Some(trigger_ref) = trigger_ref {
            if let Some(existing) = Self::active_session_for_trigger_locked(&tx, trigger_ref)? {
                if matches!(
                    (
                        existing.trigger_fingerprint.as_deref(),
                        trigger_fingerprint
                    ),
                    (Some(existing), Some(incoming)) if existing == incoming
                ) {
                    tx.commit()?;
                    return Ok((existing, SessionCreateOutcome::Deduped));
                }
                tx.execute(
                    "UPDATE sessions SET state = 'closed', closed_at = ?2
                     WHERE id = ?1 AND state NOT IN ('closed', 'aborted')",
                    params![existing.id.as_str(), created_at],
                )?;
                outcome = SessionCreateOutcome::Superseded {
                    old_id: existing.id,
                };
            }
        }

        tx.execute(
            "INSERT INTO sessions
                (id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, mode)
             VALUES (?1, ?2, 'open', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                title,
                trigger_ref,
                trigger_fingerprint,
                quorum_n,
                chair_bot,
                created_at,
                mode
            ],
        )?;
        for bot_id in roster {
            tx.execute(
                "INSERT OR IGNORE INTO session_bots (session_id, bot_id) VALUES (?1, ?2)",
                params![id, bot_id],
            )?;
        }
        tx.commit()?;
        let session = Session {
            id,
            title: title.to_string(),
            state: "open".into(),
            trigger_ref: trigger_ref.map(String::from),
            trigger_fingerprint: trigger_fingerprint.map(String::from),
            quorum_n,
            chair_bot: chair_bot.map(String::from),
            created_at,
            closed_at: None,
            mode: mode.to_string(),
            decision: None,
            findings_red: None,
            findings_yellow: None,
            findings_green: None,
            result_author_id: None,
            result_message_ids: None,
        };
        Ok((session, outcome))
    }

    fn session(&self, id: &str) -> Result<Option<Session>> {
        let c = self.conn.lock().unwrap();
        let s = c
            .query_row(
                "SELECT id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green,
                            result_author_id, result_message_ids
                 FROM sessions WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        state: r.get(2)?,
                        trigger_ref: r.get(3)?,
                        trigger_fingerprint: r.get(4)?,
                        quorum_n: r.get(5)?,
                        chair_bot: r.get(6)?,
                        created_at: r.get(7)?,
                        closed_at: r.get(8)?,
                        mode: r.get(9)?,
                        decision: r.get(10)?,
                        findings_red: r.get(11)?,
                        findings_yellow: r.get(12)?,
                        findings_green: r.get(13)?,
                        result_author_id: r.get(14)?,
                        result_message_ids: r.get(15)?,
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
                trigger_fingerprint: r.get(4)?,
                quorum_n: r.get(5)?,
                chair_bot: r.get(6)?,
                created_at: r.get(7)?,
                closed_at: r.get(8)?,
                mode: r.get(9)?,
                decision: r.get(10)?,
                findings_red: r.get(11)?,
                findings_yellow: r.get(12)?,
                findings_green: r.get(13)?,
                result_author_id: r.get(14)?,
                result_message_ids: r.get(15)?,
            })
        }

        let c = self.conn.lock().unwrap();
        let limit = limit as i64;
        let sessions = match (trigger_ref, state) {
            (Some(trigger_ref), Some(state)) => {
                let mut stmt = c.prepare(
                    "SELECT id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green,
                            result_author_id, result_message_ids
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
                    "SELECT id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green,
                            result_author_id, result_message_ids
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
                    "SELECT id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green,
                            result_author_id, result_message_ids
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
                    "SELECT id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode,
                            decision, findings_red, findings_yellow, findings_green,
                            result_author_id, result_message_ids
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

    fn close_session_with_result(
        &self,
        session_id: &str,
        from: SessionState,
        decision: Option<&str>,
        red: Option<i64>,
        yellow: Option<i64>,
        green: Option<i64>,
        result_author_id: Option<&str>,
        result_message_ids_json: Option<&str>,
    ) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "UPDATE sessions SET state = 'closed', closed_at = ?3,
                    decision = ?4, findings_red = ?5, findings_yellow = ?6, findings_green = ?7,
                    result_author_id = ?8, result_message_ids = ?9
             WHERE id = ?1 AND state = ?2",
            params![
                session_id,
                from.as_str(),
                now_ms(),
                decision,
                red,
                yellow,
                green,
                result_author_id,
                result_message_ids_json
            ],
        )?;
        Ok(n == 1)
    }

    fn reopen_session(&self, session_id: &str, from: SessionState) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "UPDATE sessions SET state = 'deliberating',
                    result_author_id = NULL, result_message_ids = NULL
             WHERE id = ?1 AND state = ?2",
            params![session_id, from.as_str()],
        )?;
        Ok(n == 1)
    }

    fn insert_review_findings(
        &self,
        session_id: &str,
        repo: Option<&str>,
        pr_number: Option<i64>,
        head_sha: Option<&str>,
        findings: &[NewReviewFinding],
    ) -> Result<()> {
        let mut c = self.conn.lock().unwrap();
        let now = now_ms();
        let tx = c.transaction()?;
        for f in findings {
            tx.execute(
                "INSERT INTO pr_review_findings
                    (session_id, repo, pr_number, stable_id, severity, status,
                     title, path, line, raised_by, angle, head_sha, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    session_id, repo, pr_number, f.stable_id, f.severity, f.status,
                    f.title, f.path, f.line, f.raised_by, f.angle, head_sha, now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn mark_once(&self, key: &str) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let n = c.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, '1')",
            params![key],
        )?;
        Ok(n == 1)
    }

    fn upsert_pending_review(
        &self,
        trigger_ref: &str,
        repo: &str,
        pr_number: i64,
        fingerprint: Option<&str>,
        preset: Option<&str>,
    ) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO pending_reviews
                (trigger_ref, repo, pr_number, fingerprint, preset, requested_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(trigger_ref) DO UPDATE SET
                fingerprint = excluded.fingerprint,
                preset = excluded.preset,
                requested_at = excluded.requested_at",
            params![trigger_ref, repo, pr_number, fingerprint, preset, now_ms()],
        )?;
        Ok(())
    }

    fn pending_reviews(&self) -> Result<Vec<PendingReview>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT trigger_ref, repo, pr_number, fingerprint, preset, requested_at
             FROM pending_reviews ORDER BY requested_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingReview {
                trigger_ref: r.get(0)?,
                repo: r.get(1)?,
                pr_number: r.get(2)?,
                fingerprint: r.get(3)?,
                preset: r.get(4)?,
                requested_at: r.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn delete_pending_review(&self, trigger_ref: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM pending_reviews WHERE trigger_ref = ?1",
            params![trigger_ref],
        )?;
        Ok(())
    }

    fn latest_session_created_at(&self, trigger_ref: &str) -> Result<Option<i64>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT MAX(created_at) FROM sessions WHERE trigger_ref = ?1")?;
        let v: Option<i64> = stmt.query_row(params![trigger_ref], |r| r.get(0))?;
        Ok(v)
    }

    fn review_findings(
        &self,
        repo: Option<&str>,
        pr_number: Option<i64>,
        status: Option<&str>,
        severity: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ReviewFinding>> {
        let c = self.conn.lock().unwrap();
        // ponytail: fixed ?-slots with "IS NULL OR" per filter beats dynamic SQL.
        let mut stmt = c.prepare(
            "SELECT id, session_id, repo, pr_number, stable_id, severity, status,
                    title, path, line, raised_by, angle, head_sha, created_at
             FROM pr_review_findings
             WHERE (?1 IS NULL OR repo = ?1)
               AND (?2 IS NULL OR pr_number = ?2)
               AND (?3 IS NULL OR status = ?3)
               AND (?4 IS NULL OR severity = ?4)
             ORDER BY id DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![repo, pr_number, status, severity, limit],
            |r| {
                Ok(ReviewFinding {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    repo: r.get(2)?,
                    pr_number: r.get(3)?,
                    stable_id: r.get(4)?,
                    severity: r.get(5)?,
                    status: r.get(6)?,
                    title: r.get(7)?,
                    path: r.get(8)?,
                    line: r.get(9)?,
                    raised_by: r.get(10)?,
                    angle: r.get(11)?,
                    head_sha: r.get(12)?,
                    created_at: r.get(13)?,
                })
            },
        )?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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
        audience: Option<&str>,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<Message> {
        let id = new_id("msg");
        let created_at = now_ms();
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO messages (id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at],
        )?;
        Ok(Message {
            id,
            session_id: session_id.to_string(),
            thread_id: thread_id.map(String::from),
            author_kind: author_kind.to_string(),
            author_id: author_id.map(String::from),
            audience: audience.map(String::from),
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

    fn message(&self, id: &str) -> Result<Option<Message>> {
        let c = self.conn.lock().unwrap();
        let msg = c
            .query_row(
                "SELECT id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at
                 FROM messages WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Message {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        thread_id: r.get(2)?,
                        author_kind: r.get(3)?,
                        author_id: r.get(4)?,
                        audience: r.get(5)?,
                        content: r.get(6)?,
                        reply_to: r.get(7)?,
                        created_at: r.get(8)?,
                    })
                },
            )
            .optional()?;
        Ok(msg)
    }

    fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            // rowid tiebreak: equal-timestamp chunks must keep insertion order
            // regardless of query plan (ADR 028 span integrity, council #241 F1)
            "SELECT id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(Message {
                id: r.get(0)?,
                session_id: r.get(1)?,
                thread_id: r.get(2)?,
                author_kind: r.get(3)?,
                author_id: r.get(4)?,
                audience: r.get(5)?,
                content: r.get(6)?,
                reply_to: r.get(7)?,
                created_at: r.get(8)?,
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

    fn done_voters(&self, session_id: &str) -> Result<Vec<String>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            // Done-vote invariant: a 🆗 counts only when it targets the opening
            // trigger / system prompt, or the voting bot's own message. OAB's
            // per-turn status 🆗 on a peer bot message is not a quorum vote.
            "SELECT DISTINCT r.bot_id FROM reactions r
             JOIN messages m ON m.id = r.message_id
             WHERE m.session_id = ?1
               AND r.emoji = '🆗'
               AND (m.author_kind IN ('client', 'system') OR m.author_id = r.bot_id)",
        )?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
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
        // OR IGNORE: a duplicate idem_key means this logical frame is already
        // pending or delivered for the session — dropping the second insert is
        // the whole point (idempotent enqueue).
        c.execute(
            "INSERT OR IGNORE INTO outbox (bot_id, session_id, idem_key, frame, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![bot_id, session_id, idem_key, frame, now_ms()],
        )?;
        Ok(())
    }

    fn pending_outbox(&self, bot_id: &str) -> Result<Vec<(i64, String)>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT seq, frame FROM outbox
             WHERE bot_id = ?1 AND delivered_at IS NULL
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![bot_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn ack_outbox(&self, seq: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE outbox SET delivered_at = ?2 WHERE seq = ?1",
            params![seq, now_ms()],
        )?;
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

    fn purge_outbox_for_session(&self, session_id: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM outbox WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    fn purge_terminal_outbox(&self) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "DELETE FROM outbox
             WHERE session_id IN (
                 SELECT id FROM sessions WHERE state IN ('closed', 'aborted')
             )
             OR session_id IS NULL",
            [],
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
        let mut stmt = c.prepare("SELECT token FROM installation_tokens WHERE session_id = ?1")?;
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

    fn stats(&self, now: i64) -> Result<Value> {
        let c = self.conn.lock().unwrap();

        // GROUP BY helper: collect a (key,count) query into a JSON object.
        let group = |sql: &str| -> Result<Value> {
            let mut stmt = c.prepare(sql)?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?
                        .unwrap_or_else(|| "unknown".into()),
                    r.get::<_, i64>(1)?,
                ))
            })?;
            let mut map = serde_json::Map::new();
            for row in rows {
                let (k, n) = row?;
                map.insert(k, json!(n));
            }
            Ok(Value::Object(map))
        };

        let by_state = group("SELECT state, COUNT(*) FROM sessions GROUP BY state")?;
        let by_mode = group("SELECT mode, COUNT(*) FROM sessions GROUP BY mode")?;
        let by_decision = group(
            "SELECT decision, COUNT(*) FROM sessions WHERE decision IS NOT NULL GROUP BY decision",
        )?;

        // "Reached a verdict" = `decision IS NOT NULL`. Aborted sessions stamp
        // `closed_at` too (see the abort path), so filtering on `closed_at` alone
        // would fold their non-review elapsed time into throughput + percentiles.
        let closed_24h: i64 = c.query_row(
            "SELECT COUNT(*) FROM sessions
             WHERE decision IS NOT NULL AND closed_at IS NOT NULL AND closed_at >= ?1",
            params![now - 24 * 3600 * 1000],
            |r| r.get(0),
        )?;

        // Time-to-verdict: durations of sessions that actually reached a verdict,
        // percentile in Rust (SQLite has no percentile_cont).
        let mut durations: Vec<i64> = {
            let mut stmt = c.prepare(
                // closed_at >= created_at drops any negative duration from wall-clock
                // skew (the timestamps are separate now_ms() reads, not monotonic).
                "SELECT closed_at - created_at FROM sessions
                 WHERE decision IS NOT NULL AND closed_at IS NOT NULL
                   AND closed_at >= created_at",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let ttv_count = durations.len() as i64;

        // Findings aggregated over verdict sessions only, so numerator and
        // denominator share the same population (no avg skew).
        let (red, yellow, green, findings_sessions): (i64, i64, i64, i64) = c.query_row(
            "SELECT COALESCE(SUM(findings_red),0), COALESCE(SUM(findings_yellow),0),
                    COALESCE(SUM(findings_green),0), COUNT(*)
             FROM sessions WHERE decision IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let avg_findings = if findings_sessions > 0 {
            (red + yellow + green) as f64 / findings_sessions as f64
        } else {
            0.0
        };

        let outbox_pending: i64 = c.query_row(
            "SELECT COUNT(*) FROM outbox WHERE delivered_at IS NULL",
            [],
            |r| r.get(0),
        )?;

        Ok(json!({
            "sessions": {
                "by_state": by_state,
                "closed_24h": closed_24h,
                "time_to_verdict_ms": {
                    "p50": percentile(&mut durations, 50.0),
                    "p95": percentile(&mut durations, 95.0),
                    "count": ttv_count,
                },
                "by_mode": by_mode,
                "by_decision": by_decision,
                "findings": {
                    "red": red, "yellow": yellow, "green": green,
                    "avg_per_session": avg_findings,
                },
            },
            "outbox": { "pending": outbox_pending },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db_path(test_name: &str) -> String {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ocp-{test_name}-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        remove_db_files(&path);
        path.to_string_lossy().into_owned()
    }

    fn remove_db_files(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    #[test]
    fn open_applies_wal_and_normal_synchronous() {
        let path = temp_db_path("open-applies-wal-and-normal-synchronous");
        let store = SqliteStore::open(&path).unwrap();
        let c = store.conn.lock().unwrap();

        let journal_mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        let synchronous: i64 = c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();

        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 1);

        drop(c);
        drop(store);
        remove_db_files(&PathBuf::from(path));
    }

    #[test]
    fn messages_query_uses_session_index() {
        let store = SqliteStore::memory().unwrap();
        let c = store.conn.lock().unwrap();
        let mut stmt = c
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT id, session_id, thread_id, author_kind, author_id, content, reply_to, created_at
                 FROM messages WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
            )
            .unwrap();
        let rows = stmt
            .query_map(params!["session-1"], |r| r.get::<_, String>(3))
            .unwrap();
        let plan = rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n");

        assert!(
            plan.contains("USING INDEX idx_messages_session"),
            "unexpected query plan:\n{plan}"
        );
        assert!(
            !plan.contains("USE TEMP B-TREE FOR ORDER BY"),
            "unexpected query plan:\n{plan}"
        );
    }

    #[test]
    fn bots_with_plaintext_token_returns_only_legacy_names() {
        let store = SqliteStore::memory().unwrap();
        store
            .seed_bot(
                "rev_legacy",
                "rev-legacy",
                "reviewer",
                "legacy-hash",
                "legacy-token",
            )
            .unwrap();
        store
            .seed_bot("rev_ext", "rev-ext", "reviewer", "ext-hash", "")
            .unwrap();

        assert_eq!(
            store.bots_with_plaintext_token().unwrap(),
            vec!["rev-legacy".to_string()]
        );
    }

    #[test]
    fn migrate_adds_index_to_legacy_db() {
        let path = temp_db_path("migrate-adds-index-to-legacy-db");
        {
            let store = SqliteStore::open(&path).unwrap();
            let c = store.conn.lock().unwrap();
            c.execute("DROP INDEX IF EXISTS idx_messages_session", [])
                .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        let c = store.conn.lock().unwrap();
        let index_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_messages_session'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(index_count, 1);

        drop(c);
        drop(store);
        remove_db_files(&PathBuf::from(path));
    }

    #[test]
    fn legacy_outbox_without_delivered_at_opens_and_migrates() {
        // The 0.1.13 → delivered_at upgrade crash: a legacy outbox table (no
        // session_id/idem_key/delivered_at) must not abort the boot-time schema
        // batch via an index that references a not-yet-migrated column.
        let path = temp_db_path("legacy-outbox-opens-and-migrates");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE outbox (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    bot_id TEXT NOT NULL, frame TEXT NOT NULL, created_at INTEGER NOT NULL
                );
                CREATE INDEX idx_outbox_bot ON outbox(bot_id, seq);
                INSERT INTO outbox (bot_id, frame, created_at) VALUES ('bot1', 'f', 1);",
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        let c = store.conn.lock().unwrap();
        for idx in [
            "idx_outbox_idem",
            "idx_outbox_pending",
            "idx_outbox_session_bot",
        ] {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing index {idx} after migrate");
        }
        // The legacy row survives as pending (delivered_at NULL).
        let pending: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE delivered_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);

        drop(c);
        drop(store);
        remove_db_files(&PathBuf::from(path));
    }

    #[test]
    fn migrate_zeroes_legacy_connected_flag() {
        let path = temp_db_path("migrate-zeroes-legacy-connected-flag");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE bots (
                    id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
                    token_hash TEXT NOT NULL, token_plain TEXT,
                    connected INTEGER NOT NULL DEFAULT 0, last_seen INTEGER
                );
                INSERT INTO bots (id, name, role, token_hash, token_plain, connected, last_seen)
                VALUES ('bot_legacy', 'legacy', 'reviewer', 'h', 't', 1, 123);",
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        let c = store.conn.lock().unwrap();
        let connected: i64 = c
            .query_row(
                "SELECT connected FROM bots WHERE id = 'bot_legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(connected, 0);

        drop(c);
        drop(store);
        remove_db_files(&PathBuf::from(path));
    }

    #[test]
    fn record_bot_frame_drives_degraded_and_recovery() {
        let store = SqliteStore::memory().unwrap();
        store.seed_bot("kiro", "kiro", "chair", "h", "t").unwrap();
        let health = |s: &SqliteStore| -> String {
            s.conn
                .lock()
                .unwrap()
                .query_row("SELECT health FROM bots WHERE id = 'kiro'", [], |r| r.get(0))
                .unwrap()
        };

        // Below threshold: counts but stays ok.
        assert_eq!(
            store.record_bot_frame("kiro", true, 3).unwrap(),
            BotHealthTransition::None
        );
        assert_eq!(
            store.record_bot_frame("kiro", true, 3).unwrap(),
            BotHealthTransition::None
        );
        assert_eq!(health(&store), "ok");
        // Third consecutive error crosses — fires exactly once, stamps last_error_at.
        assert_eq!(
            store.record_bot_frame("kiro", true, 3).unwrap(),
            BotHealthTransition::Degraded
        );
        assert_eq!(health(&store), "degraded");
        // A further error while already degraded does not re-fire.
        assert_eq!(
            store.record_bot_frame("kiro", true, 3).unwrap(),
            BotHealthTransition::None
        );
        // A content frame is observed recovery: clears the counter and health.
        assert_eq!(
            store.record_bot_frame("kiro", false, 3).unwrap(),
            BotHealthTransition::Recovered
        );
        assert_eq!(health(&store), "ok");
        // Counter reset — one error no longer sits near the threshold.
        assert_eq!(
            store.record_bot_frame("kiro", true, 3).unwrap(),
            BotHealthTransition::None
        );
        let last_error_at: Option<i64> = store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT last_error_at FROM bots WHERE id = 'kiro'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(last_error_at.is_some(), "error frames stamp last_error_at");

        // Unknown bot is a silent no-op, never an error.
        assert_eq!(
            store.record_bot_frame("ghost", true, 3).unwrap(),
            BotHealthTransition::None
        );
    }

    #[test]
    fn trigger_fingerprint_migration_is_guarded() {
        let path = temp_db_path("trigger-fingerprint-migration-is-guarded");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
                    trigger_ref TEXT, quorum_n INTEGER NOT NULL, chair_bot TEXT,
                    created_at INTEGER NOT NULL, closed_at INTEGER,
                    mode TEXT NOT NULL DEFAULT 'council',
                    decision TEXT, findings_red INTEGER, findings_yellow INTEGER,
                    findings_green INTEGER
                );
                CREATE TABLE messages (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, thread_id TEXT,
                    author_kind TEXT NOT NULL, author_id TEXT, content TEXT NOT NULL,
                    reply_to TEXT, created_at INTEGER NOT NULL
                );
                CREATE TABLE outbox (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    bot_id TEXT NOT NULL, frame TEXT NOT NULL, created_at INTEGER NOT NULL
                );",
            )
            .unwrap();
            migrate(&c).unwrap();
            migrate(&c).unwrap();
            let has_column: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('sessions')
                     WHERE name = 'trigger_fingerprint'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has_column, 1);
        }
        remove_db_files(&PathBuf::from(path));
    }

    /// ADR 028: the normal close lands state, verdict columns, and result
    /// identity in one guarded UPDATE — no closed-without-result window — and a
    /// reopen clears the result identity in its own guarded UPDATE.
    #[test]
    fn atomic_close_records_result_and_reopen_clears_it() {
        let store = SqliteStore::memory().unwrap();
        let s = store
            .create_session("t", None, 0, None, &[], "solo")
            .unwrap();
        store.set_state(&s.id, SessionState::Deliberating).unwrap();

        // Wrong `from` → CAS refused, nothing recorded.
        assert!(!store
            .close_session_with_result(
                &s.id,
                SessionState::Quorum,
                None,
                None,
                None,
                None,
                Some("bot-a"),
                Some(r#"["msg_1"]"#),
            )
            .unwrap());
        let row = store.session(&s.id).unwrap().unwrap();
        assert_eq!(row.state, "deliberating");
        assert!(row.result_author_id.is_none());

        // Right `from` → close + verdict + result land together.
        assert!(store
            .close_session_with_result(
                &s.id,
                SessionState::Deliberating,
                Some("approve"),
                Some(0),
                Some(1),
                Some(2),
                Some("bot-a"),
                Some(r#"["msg_1","msg_2"]"#),
            )
            .unwrap());
        let row = store.session(&s.id).unwrap().unwrap();
        assert_eq!(row.state, "closed");
        assert!(row.closed_at.is_some());
        assert_eq!(row.decision.as_deref(), Some("approve"));
        assert_eq!(row.result_author_id.as_deref(), Some("bot-a"));
        assert_eq!(
            row.result_message_ids.as_deref(),
            Some(r#"["msg_1","msg_2"]"#)
        );

        // Reopen (follow-up turn) clears the result identity in the same breath.
        assert!(store.reopen_session(&s.id, SessionState::Closed).unwrap());
        let row = store.session(&s.id).unwrap().unwrap();
        assert_eq!(row.state, "deliberating");
        assert!(row.result_author_id.is_none());
        assert!(row.result_message_ids.is_none());
        // Reopen CAS is once-only too.
        assert!(!store.reopen_session(&s.id, SessionState::Closed).unwrap());
    }

    #[test]
    fn percentile_nearest_rank() {
        assert_eq!(percentile(&mut [], 50.0), None);
        assert_eq!(percentile(&mut [42], 95.0), Some(42));
        // 1..=10 → p50 idx round(0.5*9)=5 → value 6; p95 idx round(0.95*9)=9 → 10.
        let mut v: Vec<i64> = (1..=10).collect();
        assert_eq!(percentile(&mut v, 50.0), Some(6));
        assert_eq!(percentile(&mut v, 95.0), Some(10));
        // unsorted input is handled (sorts in place)
        assert_eq!(percentile(&mut [30, 10, 20], 50.0), Some(20));
        // p>100 clamps to the max instead of panicking on an OOB index.
        assert_eq!(percentile(&mut [1, 2, 3], 150.0), Some(3));
    }

    #[test]
    fn stats_aggregates_sessions_and_outbox() {
        let store = SqliteStore::memory().unwrap();
        // one open, one closed+approved with findings and a known duration.
        let open = store
            .create_session("open", Some("t:open"), 3, None, &[], "council")
            .unwrap();
        let done = store
            .create_session("done", Some("t:done"), 3, None, &[], "council")
            .unwrap();
        store
            .set_session_verdict(&done.id, "approve", Some(0), Some(2), Some(5))
            .unwrap();
        store.set_state(&done.id, SessionState::Closed).unwrap();
        store
            .enqueue_outbox("bot1", &open.id, "bot1:m1", "f")
            .unwrap();

        // An aborted session stamps closed_at but never reached a verdict — it
        // must NOT count toward throughput or time-to-verdict (council reds F1/F2).
        let aborted = store
            .create_session("aborted", Some("t:aborted"), 3, None, &[], "council")
            .unwrap();
        store.set_state(&aborted.id, SessionState::Aborted).unwrap();

        let now = now_ms();
        let s = store.stats(now).unwrap();
        assert_eq!(s["sessions"]["by_state"]["open"], json!(1));
        assert_eq!(s["sessions"]["by_state"]["closed"], json!(1));
        assert_eq!(s["sessions"]["by_state"]["aborted"], json!(1));
        // only the verdict session counts, not the aborted one.
        assert_eq!(s["sessions"]["closed_24h"], json!(1));
        assert_eq!(s["sessions"]["by_decision"]["approve"], json!(1));
        assert_eq!(s["sessions"]["findings"]["red"], json!(0));
        assert_eq!(s["sessions"]["findings"]["yellow"], json!(2));
        assert_eq!(s["sessions"]["findings"]["green"], json!(5));
        // one session has findings → avg = 7/1
        assert_eq!(s["sessions"]["findings"]["avg_per_session"], json!(7.0));
        // one closed session → p50/p95 both its (small, >=0) duration; count = 1.
        assert_eq!(s["sessions"]["time_to_verdict_ms"]["count"], json!(1));
        assert!(s["sessions"]["time_to_verdict_ms"]["p50"].is_i64());
        assert_eq!(s["outbox"]["pending"], json!(1));
    }

    #[test]
    fn stats_counts_only_pending_outbox_rows() {
        let store = SqliteStore::memory().unwrap();
        store
            .enqueue_outbox("bot1", "s1", "bot1:m1", "frameA")
            .unwrap();
        store
            .enqueue_outbox("bot1", "s1", "bot1:m2", "frameB")
            .unwrap();
        let seq = store.pending_outbox("bot1").unwrap()[0].0;
        store.ack_outbox(seq).unwrap();

        let s = store.stats(now_ms()).unwrap();
        assert_eq!(s["outbox"]["pending"], json!(1));
    }

    #[test]
    fn delete_bot_removes_identity_and_token() {
        let store = SqliteStore::memory().unwrap();
        let (bot, token) = crate::identity::issue(&store, "retire-me", "reviewer", None).unwrap();
        let token_hash = crate::identity::hash_token(&token);

        assert_eq!(
            store.delete_bot(&bot.id).unwrap(),
            DeleteBotOutcome::Deleted
        );

        assert!(store.bot(&bot.id).unwrap().is_none());
        assert!(store.bot_by_token_hash(&token_hash).unwrap().is_none());
    }

    #[test]
    fn delete_bot_refuses_active_session_member() {
        let store = SqliteStore::memory().unwrap();
        let (bot, _) = crate::identity::issue(&store, "active", "reviewer", None).unwrap();
        let session = store
            .create_session(
                "active review",
                Some("github:pr/o/r#active"),
                1,
                None,
                std::slice::from_ref(&bot.id),
                "council",
            )
            .unwrap();

        assert_eq!(
            store.delete_bot(&bot.id).unwrap(),
            DeleteBotOutcome::ActiveSession
        );
        assert!(store.bot(&bot.id).unwrap().is_some());

        store.set_state(&session.id, SessionState::Closed).unwrap();
        assert_eq!(
            store.delete_bot(&bot.id).unwrap(),
            DeleteBotOutcome::Deleted
        );
        assert!(store.bot(&bot.id).unwrap().is_none());
    }

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

        // After ack, the retained delivered marker keeps the idem_key occupied:
        // retrying the same logical message during this session is still a no-op.
        let seq = store.pending_outbox("bot1").unwrap()[0].0;
        store.ack_outbox(seq).unwrap();
        store.enqueue_outbox("bot1", "s1", key, "frameA").unwrap();
        assert!(store.pending_outbox("bot1").unwrap().is_empty());

        // A different message to the same bot still queues.
        store
            .enqueue_outbox("bot1", "s1", "bot1:msg2", "frameB")
            .unwrap();
        assert_eq!(store.pending_outbox("bot1").unwrap().len(), 1);
    }

    #[test]
    fn ack_outbox_marks_delivered_and_pending_excludes_it() {
        let store = SqliteStore::memory().unwrap();
        store
            .enqueue_outbox("bot1", "s1", "bot1:m1", "frameA")
            .unwrap();
        let seq = store.pending_outbox("bot1").unwrap()[0].0;

        store.ack_outbox(seq).unwrap();

        assert!(store.pending_outbox("bot1").unwrap().is_empty());
        let delivered_at: Option<i64> = {
            let c = store.conn.lock().unwrap();
            c.query_row(
                "SELECT delivered_at FROM outbox WHERE seq = ?1",
                params![seq],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(delivered_at.is_some());
    }

    #[test]
    fn purge_outbox_for_session_bot_deletes_delivered_rows_and_rearms_idem_key() {
        let store = SqliteStore::memory().unwrap();
        let key = "bot1:m1";
        store.enqueue_outbox("bot1", "s1", key, "frameA").unwrap();
        let seq = store.pending_outbox("bot1").unwrap()[0].0;
        store.ack_outbox(seq).unwrap();
        assert!(store.pending_outbox("bot1").unwrap().is_empty());

        store.purge_outbox_for_session_bot("s1", "bot1").unwrap();
        let rows: i64 = {
            let c = store.conn.lock().unwrap();
            c.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(rows, 0);

        store.enqueue_outbox("bot1", "s1", key, "frameA").unwrap();
        assert_eq!(store.pending_outbox("bot1").unwrap().len(), 1);
    }

    #[test]
    fn purge_outbox_for_session_removes_all_bots_rows() {
        let store = SqliteStore::memory().unwrap();
        store
            .enqueue_outbox("bot1", "session-s", "bot1:s1", "s-bot1")
            .unwrap();
        store
            .enqueue_outbox("bot2", "session-s", "bot2:s1", "s-bot2")
            .unwrap();
        store
            .enqueue_outbox("bot1", "session-t", "bot1:t1", "t-bot1")
            .unwrap();

        store.purge_outbox_for_session("session-s").unwrap();

        let bot1 = store.pending_outbox("bot1").unwrap();
        assert_eq!(bot1.len(), 1);
        assert_eq!(bot1[0].1, "t-bot1");
        assert!(store.pending_outbox("bot2").unwrap().is_empty());
    }

    #[test]
    fn purge_terminal_outbox_sweeps_closed_and_null_sessions() {
        let store = SqliteStore::memory().unwrap();
        let open = store
            .create_session("open", Some("t:open"), 1, None, &[], "council")
            .unwrap();
        let closed = store
            .create_session("closed", Some("t:closed"), 1, None, &[], "council")
            .unwrap();
        let aborted = store
            .create_session("aborted", Some("t:aborted"), 1, None, &[], "council")
            .unwrap();
        store.set_state(&closed.id, SessionState::Closed).unwrap();
        store.set_state(&aborted.id, SessionState::Aborted).unwrap();
        store
            .enqueue_outbox("bot", &open.id, "bot:open", "open-frame")
            .unwrap();
        store
            .enqueue_outbox("bot", &closed.id, "bot:closed", "closed-frame")
            .unwrap();
        store
            .enqueue_outbox("bot", &aborted.id, "bot:aborted", "aborted-frame")
            .unwrap();
        {
            let c = store.conn.lock().unwrap();
            c.execute(
                "INSERT INTO outbox (bot_id, session_id, idem_key, frame, created_at)
                 VALUES (?1, NULL, ?2, ?3, ?4)",
                params!["bot", "bot:null", "null-frame", now_ms()],
            )
            .unwrap();
        }

        store.purge_terminal_outbox().unwrap();

        let frames: Vec<_> = store
            .pending_outbox("bot")
            .unwrap()
            .into_iter()
            .map(|(_, frame)| frame)
            .collect();
        assert_eq!(frames, vec!["open-frame"]);
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
    fn create_session_superseding_dedupes_on_equal_fingerprint() {
        let store = SqliteStore::memory().unwrap();
        let trigger = "github:pr/o/r#4";

        let (first, outcome) = store
            .create_session_superseding(
                "Review o/r#4",
                Some(trigger),
                Some("sha:abc"),
                0,
                None,
                &[],
                "council",
            )
            .unwrap();
        assert_eq!(outcome, SessionCreateOutcome::Created);

        let (second, outcome) = store
            .create_session_superseding(
                "Review o/r#4 retry",
                Some(trigger),
                Some("sha:abc"),
                0,
                None,
                &[],
                "council",
            )
            .unwrap();
        assert_eq!(outcome, SessionCreateOutcome::Deduped);
        assert_eq!(second.id, first.id);
        assert_eq!(second.title, "Review o/r#4");
        assert_eq!(second.trigger_fingerprint.as_deref(), Some("sha:abc"));
    }

    #[test]
    fn create_session_superseding_closes_old_and_opens_successor_in_one_txn() {
        let store = SqliteStore::memory().unwrap();
        let trigger = "github:pr/o/r#5";
        let (old, _) = store
            .create_session_superseding(
                "old",
                Some(trigger),
                Some("sha:old"),
                0,
                None,
                &[],
                "council",
            )
            .unwrap();

        let (new, outcome) = store
            .create_session_superseding(
                "new",
                Some(trigger),
                Some("sha:new"),
                0,
                None,
                &[],
                "council",
            )
            .unwrap();

        assert_eq!(
            outcome,
            SessionCreateOutcome::Superseded {
                old_id: old.id.clone()
            }
        );
        assert_eq!(
            SessionState::from_db_str(&store.session(&old.id).unwrap().unwrap().state),
            SessionState::Closed
        );
        assert_ne!(old.id, new.id);
        assert_eq!(
            store
                .active_session_for_trigger(trigger)
                .unwrap()
                .as_deref(),
            Some(new.id.as_str())
        );
        let active_count: i64 = {
            let c = store.conn.lock().unwrap();
            c.query_row(
                "SELECT COUNT(*) FROM sessions
                 WHERE trigger_ref = ?1 AND state NOT IN ('closed', 'aborted')",
                params![trigger],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(active_count, 1);
    }

    #[test]
    fn create_session_superseding_null_fingerprints_supersede() {
        let store = SqliteStore::memory().unwrap();
        let trigger = "github:pr/o/r#6";
        let legacy = store
            .create_session("legacy", Some(trigger), 0, None, &[], "council")
            .unwrap();

        let (sha_round, outcome) = store
            .create_session_superseding(
                "sha round",
                Some(trigger),
                Some("sha:new"),
                0,
                None,
                &[],
                "council",
            )
            .unwrap();
        assert_eq!(
            outcome,
            SessionCreateOutcome::Superseded {
                old_id: legacy.id.clone()
            }
        );

        let (null_round, outcome) = store
            .create_session_superseding(
                "manual round",
                Some(trigger),
                None,
                0,
                None,
                &[],
                "council",
            )
            .unwrap();
        assert_eq!(
            outcome,
            SessionCreateOutcome::Superseded {
                old_id: sha_round.id.clone()
            }
        );
        assert_eq!(null_round.trigger_fingerprint, None);
    }

    #[test]
    fn create_session_superseding_concurrent_same_delivery_has_one_successor() {
        let store = std::sync::Arc::new(SqliteStore::memory().unwrap());
        let trigger = "github:pr/o/r#7";
        let (old, _) = store
            .create_session_superseding(
                "old",
                Some(trigger),
                Some("sha:old"),
                0,
                None,
                &[],
                "council",
            )
            .unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .create_session_superseding(
                            "new",
                            Some(trigger),
                            Some("sha:new"),
                            0,
                            None,
                            &[],
                            "council",
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>();

        assert!(results.iter().any(|(_, outcome)| {
            outcome
                == &SessionCreateOutcome::Superseded {
                    old_id: old.id.clone(),
                }
        }));
        assert!(results
            .iter()
            .any(|(_, outcome)| outcome == &SessionCreateOutcome::Deduped));
        let successor_ids = results
            .iter()
            .map(|(s, _)| s.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(successor_ids[0], successor_ids[1]);
        let active_count: i64 = {
            let c = store.conn.lock().unwrap();
            c.query_row(
                "SELECT COUNT(*) FROM sessions
                 WHERE trigger_ref = ?1 AND state NOT IN ('closed', 'aborted')",
                params![trigger],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(active_count, 1);
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
    fn message_audience_roundtrips() {
        let store = SqliteStore::memory().unwrap();
        let session = store
            .create_session("one", None, 0, None, &[], "council")
            .unwrap();
        let broadcast = store
            .add_message(&session.id, None, "client", None, None, "broadcast", None)
            .unwrap();
        let targeted = store
            .add_message(
                &session.id,
                None,
                "system",
                None,
                Some("chair"),
                "targeted",
                None,
            )
            .unwrap();
        {
            let c = store.conn.lock().unwrap();
            c.execute(
                "INSERT INTO messages (id, session_id, author_kind, author_id, content, reply_to, created_at)
                 VALUES ('msg_legacy', ?1, 'client', NULL, 'legacy', NULL, ?2)",
                params![session.id, now_ms()],
            )
            .unwrap();
        }

        assert_eq!(broadcast.audience, None);
        assert_eq!(targeted.audience.as_deref(), Some("chair"));
        let rows = store.messages(&session.id).unwrap();
        assert_eq!(
            rows.iter().find(|m| m.id == broadcast.id).unwrap().audience,
            None
        );
        assert_eq!(
            rows.iter()
                .find(|m| m.id == targeted.id)
                .unwrap()
                .audience
                .as_deref(),
            Some("chair")
        );
        assert_eq!(
            rows.iter().find(|m| m.id == "msg_legacy").unwrap().audience,
            None
        );
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
            .add_message(&s1.id, None, "bot", Some("rev1"), None, "done", None)
            .unwrap();
        let m2 = store
            .add_message(&s2.id, None, "bot", Some("rev2"), None, "done", None)
            .unwrap();
        store.add_reaction(&m1.id, "rev1", "🆗").unwrap();
        store.add_reaction(&m2.id, "rev2", "🆗").unwrap();

        let reactions = store.reactions(&s1.id).unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].message_id, m1.id);
        assert_eq!(reactions[0].bot_id, "rev1");
    }

    #[test]
    fn done_voters_counts_only_trigger_targeted_or_self_votes() {
        let store = SqliteStore::memory().unwrap();
        let s = store
            .create_session("one", None, 0, None, &[], "council")
            .unwrap();
        let client = store
            .add_message(&s.id, None, "client", None, None, "trigger", None)
            .unwrap();
        let system = store
            .add_message(&s.id, None, "system", None, None, "prompt", None)
            .unwrap();
        let own = store
            .add_message(&s.id, None, "bot", Some("rev1"), None, "final", None)
            .unwrap();
        let peer = store
            .add_message(&s.id, None, "bot", Some("rev2"), None, "peer", None)
            .unwrap();

        store.add_reaction(&client.id, "rev1", "🆗").unwrap();
        store.add_reaction(&system.id, "rev2", "🆗").unwrap();
        store.add_reaction(&own.id, "rev1", "🆗").unwrap();
        store.add_reaction(&peer.id, "rev1", "🆗").unwrap();

        let mut voters = store.done_voters(&s.id).unwrap();
        voters.sort();
        assert_eq!(voters, vec!["rev1".to_string(), "rev2".to_string()]);
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

    // SEI-785: last_seen must reflect the persisted timestamp, not just now(),
    // so the gateway can flush its in-memory activity clock and `/v1/bots`
    // reports true liveness instead of a value frozen at connect time.
    #[test]
    fn touch_last_seen_at_persists_given_timestamp() {
        let store = SqliteStore::memory().unwrap();
        store
            .discover_bot("b1", Some("Bot One"), "chair", &BotMetadata::default())
            .unwrap();

        // A timestamp deliberately unlike now() — the fix must persist THIS value.
        let ts = 1_000_000_000_000i64;
        store.touch_last_seen_at("b1", ts).unwrap();
        assert_eq!(store.bot_inventory("b1").unwrap().unwrap().last_seen_ms, Some(ts));

        // Plain touch_last_seen still advances to ~now (and past our fixed ts).
        store.touch_last_seen("b1").unwrap();
        let seen = store.bot_inventory("b1").unwrap().unwrap().last_seen_ms.unwrap();
        assert!(seen > ts, "touch_last_seen should advance last_seen to now()");
    }
}
