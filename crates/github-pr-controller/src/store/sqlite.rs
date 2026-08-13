//! The original SQLite backend — the default, unchanged in semantics by the
//! ADR 033 trait extraction. Pod-local file, IMMEDIATE-transaction claims,
//! `PRAGMA user_version` migrations.

use super::{
    now_unix, CanarySummary, DeliveryAdmission, PendingWrite, ProductStore, RecordedRound,
    ReviewFinding, ReviewFindingQuery, ReviewFindingRow, ReviewRound, ReviewWaiver,
    RuntimeEventAdmission, SessionTarget, ShadowAdmission, ShadowSummary, StoreError, StoreResult,
    COMPLETED_RETENTION_SECS, PROCESSING_LEASE_SECS, WRITE_CLAIM_LEASE_SECS, WRITE_MAX_ATTEMPTS,
};
use controller_protocol::audit::{
    AuditActor, AuditCorrelation, AuditCursor, AuditError, AuditEvent, AuditEventPage,
    AuditEventQuery, AuditEventRecord, AuditOutcome, AuditTarget,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Schema versions, applied in order, `PRAGMA user_version` holding the count
/// already applied. Migration 0 is the original `IF NOT EXISTS` batch, so a
/// database written before this framework existed migrates by re-running it as
/// a no-op and then taking 1.
const MIGRATIONS: &[&str] = &[
    // 0 — delivery-side tables (pre-existing)
    "CREATE TABLE IF NOT EXISTS webhook_deliveries (
       delivery_id TEXT PRIMARY KEY,
       event_type TEXT NOT NULL,
       repository TEXT,
       payload_sha256 TEXT NOT NULL,
       state TEXT NOT NULL,
       result_json TEXT,
       received_at INTEGER NOT NULL,
       completed_at INTEGER
     );
     CREATE TABLE IF NOT EXISTS shadow_comparisons (
       comparison_id TEXT PRIMARY KEY,
       request_sha256 TEXT NOT NULL,
       repository TEXT,
       exact_match INTEGER NOT NULL,
       identity_mismatches INTEGER NOT NULL,
       presentation_mismatches INTEGER NOT NULL,
       created_at INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_shadow_comparisons_created
       ON shadow_comparisons(created_at);
     CREATE TABLE IF NOT EXISTS runtime_event_receipts (
       event_id TEXT PRIMARY KEY,
       body_sha256 TEXT NOT NULL,
       event_type TEXT NOT NULL,
       session_id TEXT,
       occurred_at INTEGER NOT NULL,
       received_at INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_runtime_event_receipts_received
       ON runtime_event_receipts(received_at);",
    // 1 — product tables for the closing half (SEI-852 step 3)
    "CREATE TABLE IF NOT EXISTS review_rounds (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       repo TEXT NOT NULL,
       pr_number INTEGER NOT NULL,
       round INTEGER NOT NULL,
       -- one round per session: at-least-once terminal events reprocess, and
       -- this is what makes the second delivery a no-op instead of a round 2.
       session_id TEXT NOT NULL UNIQUE,
       head_sha TEXT,
       -- the upsert anchor for the verdict comment, learned after the first
       -- write; NULL until then.
       comment_id INTEGER,
       decision TEXT NOT NULL,
       red INTEGER NOT NULL,
       yellow INTEGER NOT NULL,
       green INTEGER NOT NULL,
       created_at INTEGER NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_review_rounds_pr
       ON review_rounds(repo, pr_number, round);
     -- Port of the kernel's pr_review_findings, same columns. Append-only.
     CREATE TABLE IF NOT EXISTS review_findings (
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
     CREATE INDEX IF NOT EXISTS idx_review_findings_pr
       ON review_findings(repo, pr_number, id);
     CREATE INDEX IF NOT EXISTS idx_review_findings_session
       ON review_findings(session_id);
     -- Idempotent outbox. One row per (session, kind): re-enqueueing the same
     -- write after a redelivery is ignored, so a verdict is posted once.
     CREATE TABLE IF NOT EXISTS github_writes (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       session_id TEXT NOT NULL,
       kind TEXT NOT NULL,
       payload_json TEXT NOT NULL,
       state TEXT NOT NULL,
       attempts INTEGER NOT NULL,
       last_error TEXT,
       created_at INTEGER NOT NULL,
       done_at INTEGER,
       UNIQUE(session_id, kind)
     );
     CREATE INDEX IF NOT EXISTS idx_github_writes_pending
       ON github_writes(state, id);",
    // 2 — what a session this controller opened is *about*. The terminal event
    // names only a session id; the provider object it belongs to is knowledge
    // the kernel does not hold and must not (ADR 031), so the controller keeps
    // it. A session with no row here was not opened by us — we ignore its
    // terminal event rather than guess.
    "CREATE TABLE IF NOT EXISTS session_targets (
       session_id TEXT PRIMARY KEY,
       repo TEXT NOT NULL,
       pr_number INTEGER NOT NULL,
       head_sha TEXT,
       created_at INTEGER NOT NULL
     );",
    // 3 — when a drain took ownership of a write. Two drains that both read
    // the same pending row would both post, and a comment is not idempotent.
    "ALTER TABLE github_writes ADD COLUMN claimed_at INTEGER;",
    // 4 — ADR 036 first-party investigation journal.
    "CREATE TABLE IF NOT EXISTS audit_events (
       seq INTEGER PRIMARY KEY AUTOINCREMENT,
       event_id TEXT NOT NULL UNIQUE,
       event_key TEXT NOT NULL,
       version INTEGER NOT NULL,
       occurred_at INTEGER NOT NULL,
       recorded_at INTEGER NOT NULL,
       service TEXT NOT NULL,
       kind TEXT NOT NULL,
       outcome TEXT NOT NULL,
       caused_by TEXT,
       delivery_id TEXT,
       controller_id TEXT,
       action_id TEXT,
       scope TEXT,
       trigger_ref TEXT,
       trigger_fingerprint TEXT,
       session_id TEXT,
       message_id TEXT,
       runtime_event_id TEXT,
       write_id TEXT,
       actor_kind TEXT,
       actor_id TEXT,
       actor_display TEXT,
       actor_association TEXT,
       target_kind TEXT,
       target_ref TEXT,
       target_revision TEXT,
       detail_json TEXT NOT NULL,
       error_json TEXT,
       UNIQUE(service, event_key)
     );
     CREATE INDEX IF NOT EXISTS idx_audit_events_session ON audit_events(session_id);
     CREATE INDEX IF NOT EXISTS idx_audit_events_delivery ON audit_events(delivery_id);
     CREATE INDEX IF NOT EXISTS idx_audit_events_action ON audit_events(controller_id, action_id);
     CREATE INDEX IF NOT EXISTS idx_audit_events_runtime ON audit_events(runtime_event_id);
     CREATE INDEX IF NOT EXISTS idx_audit_events_write ON audit_events(write_id);
     CREATE INDEX IF NOT EXISTS idx_audit_events_trigger ON audit_events(trigger_ref);
     CREATE INDEX IF NOT EXISTS idx_audit_events_recorded ON audit_events(recorded_at, seq);",
    // 5 — ADR 035 waiver ledger, ported from the kernel (SEI-895 item 5).
    // Timestamps in unix seconds like every other table here (the kernel's
    // copy used ms; the data migration converts).
    "CREATE TABLE IF NOT EXISTS review_waivers (
       id TEXT PRIMARY KEY,
       repo TEXT NOT NULL,
       path_class TEXT,
       text TEXT NOT NULL,
       origin_pr TEXT,
       created_by TEXT NOT NULL,
       created_at INTEGER NOT NULL,
       expires_at INTEGER NOT NULL,
       revoked_at INTEGER,
       fired_count INTEGER NOT NULL DEFAULT 0,
       last_fired_at INTEGER
     );
     CREATE INDEX IF NOT EXISTS idx_review_waivers_repo ON review_waivers(repo, expires_at);",
    // 6 — ADR 038: an author's judgement on a finding. The status column
    // already carried `dismissed`; what was missing is who said so and why,
    // without which the record cannot be audited and the decision cannot be
    // told apart from the chair's own bookkeeping.
    "ALTER TABLE review_findings ADD COLUMN decided_by TEXT;
     ALTER TABLE review_findings ADD COLUMN decided_reason TEXT;
     ALTER TABLE review_findings ADD COLUMN decided_at INTEGER;",
    // 7 — ADR 038 v2: `waive` mints a repo-scoped waiver from the finding it
    // accepts. The finding remembers which waiver it minted (so `reopen` can
    // revoke it), and the ledger remembers whose words each entry carries —
    // every pre-existing row is the operator's (ADR 035's only write path
    // until now), so the default is honest, not merely convenient.
    "ALTER TABLE review_findings ADD COLUMN waiver_id TEXT;
     ALTER TABLE review_waivers ADD COLUMN source TEXT NOT NULL DEFAULT 'operator';",
];

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> rusqlite::Result<Self> {
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn begin_delivery(
        &self,
        delivery_id: &str,
        event_type: &str,
        repository: Option<&str>,
        payload_sha256: &str,
    ) -> rusqlite::Result<DeliveryAdmission> {
        self.begin_delivery_at(
            delivery_id,
            event_type,
            repository,
            payload_sha256,
            now_unix(),
        )
    }

    fn begin_delivery_at(
        &self,
        delivery_id: &str,
        event_type: &str,
        repository: Option<&str>,
        payload_sha256: &str,
        now: i64,
    ) -> rusqlite::Result<DeliveryAdmission> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT payload_sha256, state, result_json, received_at
                   FROM webhook_deliveries WHERE delivery_id = ?1",
                [delivery_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;

        let admission = match existing {
            Some((existing_hash, _, _, _)) if existing_hash != payload_sha256 => {
                DeliveryAdmission::Conflict
            }
            Some((_, state, _, _)) if state == "retryable" => {
                transaction.execute(
                    "UPDATE webhook_deliveries
                        SET event_type = ?2, repository = ?3, state = 'processing',
                            result_json = NULL, received_at = ?4, completed_at = NULL
                      WHERE delivery_id = ?1",
                    params![delivery_id, event_type, repository, now],
                )?;
                DeliveryAdmission::New
            }
            Some((_, state, _, received_at))
                if state == "processing"
                    && received_at <= now.saturating_sub(PROCESSING_LEASE_SECS) =>
            {
                transaction.execute(
                    "UPDATE webhook_deliveries
                        SET event_type = ?2, repository = ?3, received_at = ?4
                      WHERE delivery_id = ?1",
                    params![delivery_id, event_type, repository, now],
                )?;
                DeliveryAdmission::New
            }
            Some((_, state, result_json, _)) => DeliveryAdmission::Duplicate {
                state,
                result: result_json.and_then(|value| serde_json::from_str(&value).ok()),
            },
            None => {
                transaction.execute(
                    "INSERT INTO webhook_deliveries
                       (delivery_id, event_type, repository, payload_sha256, state, received_at)
                     VALUES (?1, ?2, ?3, ?4, 'processing', ?5)",
                    params![delivery_id, event_type, repository, payload_sha256, now],
                )?;
                DeliveryAdmission::New
            }
        };
        transaction.commit()?;
        Ok(admission)
    }

    pub fn finish_delivery(
        &self,
        delivery_id: &str,
        state: &str,
        result: &Value,
    ) -> rusqlite::Result<()> {
        self.finish_delivery_at(delivery_id, state, result, now_unix())
    }

    pub fn release_delivery_for_retry(
        &self,
        delivery_id: &str,
        result: &Value,
    ) -> rusqlite::Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "UPDATE webhook_deliveries
                SET state = 'retryable', result_json = ?2, completed_at = NULL
              WHERE delivery_id = ?1 AND state = 'processing'",
            params![delivery_id, result.to_string()],
        )?;
        Ok(())
    }

    fn finish_delivery_at(
        &self,
        delivery_id: &str,
        state: &str,
        result: &Value,
        now: i64,
    ) -> rusqlite::Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "UPDATE webhook_deliveries
                SET state = ?2, result_json = ?3, completed_at = ?4
              WHERE delivery_id = ?1",
            params![delivery_id, state, result.to_string(), now],
        )?;
        Ok(())
    }

    pub fn prune_completed_deliveries(&self) -> rusqlite::Result<usize> {
        let now = now_unix();
        let deliveries = self.prune_completed_deliveries_at(now, COMPLETED_RETENTION_SECS)?;
        let comparisons = self.prune_shadow_comparisons_at(now, COMPLETED_RETENTION_SECS)?;
        let events = self.prune_runtime_events_at(now, COMPLETED_RETENTION_SECS)?;
        Ok(deliveries + comparisons + events)
    }

    fn prune_completed_deliveries_at(
        &self,
        now: i64,
        retention_secs: i64,
    ) -> rusqlite::Result<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "DELETE FROM webhook_deliveries
              WHERE (state IN ('planned', 'ignored', 'acted') AND completed_at < ?1)
                 OR (state IN ('processing', 'retryable') AND received_at < ?1)",
            [now.saturating_sub(retention_secs)],
        )
    }

    pub fn record_shadow_comparison(
        &self,
        request_sha256: &str,
        repository: Option<&str>,
        report: &crate::shadow::ShadowReport,
    ) -> rusqlite::Result<ShadowAdmission> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT request_sha256 FROM shadow_comparisons WHERE comparison_id = ?1",
                [&report.comparison_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let admission = match existing {
            Some(existing) if existing == request_sha256 => ShadowAdmission::Duplicate,
            Some(_) => ShadowAdmission::Conflict,
            None => {
                transaction.execute(
                    "INSERT INTO shadow_comparisons
               (comparison_id, request_sha256, repository, exact_match,
                identity_mismatches, presentation_mismatches, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        report.comparison_id,
                        request_sha256,
                        repository,
                        report.exact_match,
                        report.identity_or_ownership_mismatches as i64,
                        report.presentation_mismatches as i64,
                        now_unix(),
                    ],
                )?;
                ShadowAdmission::New
            }
        };
        transaction.commit()?;
        Ok(admission)
    }

    pub fn shadow_summary(&self) -> rusqlite::Result<ShadowSummary> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN exact_match = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN identity_mismatches > 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN presentation_mismatches > 0 THEN 1 ELSE 0 END), 0)
               FROM shadow_comparisons",
            [],
            |row| {
                Ok(ShadowSummary {
                    total: row.get(0)?,
                    exact_matches: row.get(1)?,
                    identity_or_ownership_mismatch_reports: row.get(2)?,
                    presentation_mismatch_reports: row.get(3)?,
                })
            },
        )
    }

    pub fn record_runtime_event(
        &self,
        body_sha256: &str,
        event: &crate::runtime_events::RuntimeEventEnvelope,
    ) -> rusqlite::Result<RuntimeEventAdmission> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT body_sha256 FROM runtime_event_receipts WHERE event_id = ?1",
                [&event.event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let admission = match existing {
            Some(existing) if existing == body_sha256 => RuntimeEventAdmission::Duplicate,
            Some(_) => RuntimeEventAdmission::Conflict,
            None => {
                transaction.execute(
                    "INSERT INTO runtime_event_receipts
                       (event_id, body_sha256, event_type, session_id, occurred_at, received_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        event.event_id,
                        body_sha256,
                        event.event_type,
                        event.session_id,
                        event.occurred_at,
                        now_unix(),
                    ],
                )?;
                RuntimeEventAdmission::New
            }
        };
        transaction.commit()?;
        Ok(admission)
    }

    pub fn canary_summary(&self) -> rusqlite::Result<CanarySummary> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let (acted_deliveries, processing_deliveries, retryable_deliveries) = connection
            .query_row(
                "SELECT
               COALESCE(SUM(CASE WHEN state = 'acted' THEN 1 ELSE 0 END), 0),
               COALESCE(SUM(CASE WHEN state = 'processing' THEN 1 ELSE 0 END), 0),
               COALESCE(SUM(CASE WHEN state = 'retryable' THEN 1 ELSE 0 END), 0)
             FROM webhook_deliveries",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let (runtime_events, latest_event_occurred_at) = connection.query_row(
            "SELECT COUNT(*), MAX(occurred_at) FROM runtime_event_receipts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut statement = connection.prepare(
            "SELECT event_type, COUNT(*) FROM runtime_event_receipts
             GROUP BY event_type ORDER BY event_type",
        )?;
        let runtime_event_types = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
        Ok(CanarySummary {
            acted_deliveries,
            processing_deliveries,
            retryable_deliveries,
            runtime_events,
            runtime_event_types,
            latest_event_occurred_at,
        })
    }

    /// The round a session opened now would be: one past the highest this
    /// controller has closed for the pull request.
    pub fn next_round(&self, repo: &str, pr_number: i64) -> rusqlite::Result<i64> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.query_row(
            "SELECT COALESCE(MAX(round), 0) + 1 FROM review_rounds
              WHERE repo = ?1 AND pr_number = ?2",
            params![repo, pr_number],
            |row| row.get(0),
        )
    }

    /// Remember what a session we just opened is about. Idempotent: a
    /// redelivered webhook that re-opens the same session must not conflict.
    pub fn record_session_target(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
    ) -> rusqlite::Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "INSERT INTO session_targets
               (session_id, repo, pr_number, head_sha, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
               repo = excluded.repo,
               pr_number = excluded.pr_number,
               head_sha = COALESCE(excluded.head_sha, session_targets.head_sha)",
            params![session_id, repo, pr_number, head_sha, now_unix()],
        )?;
        Ok(())
    }

    /// `None` means this controller did not open the session — the terminal
    /// event belongs to someone else and must not be acted on.
    pub fn session_target(&self, session_id: &str) -> rusqlite::Result<Option<SessionTarget>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .query_row(
                "SELECT repo, pr_number, head_sha FROM session_targets WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok(SessionTarget {
                        repo: row.get(0)?,
                        pr_number: row.get(1)?,
                        head_sha: row.get(2)?,
                    })
                },
            )
            .optional()
    }

    /// Open (or recover) the round for a closed session. Idempotent: a
    /// redelivered terminal event returns the round already recorded rather
    /// than opening a second one.
    pub fn record_review_round(&self, round: &ReviewRound) -> rusqlite::Result<RecordedRound> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT id, round FROM review_rounds WHERE session_id = ?1",
                [&round.session_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let recorded = match existing {
            Some((id, number)) => RecordedRound {
                id,
                round: number,
                first_time: false,
            },
            None => {
                let number: i64 = transaction.query_row(
                    "SELECT COALESCE(MAX(round), 0) + 1 FROM review_rounds
                      WHERE repo = ?1 AND pr_number = ?2",
                    params![round.repo, round.pr_number],
                    |row| row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO review_rounds
                       (repo, pr_number, round, session_id, head_sha, decision,
                        red, yellow, green, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        round.repo,
                        round.pr_number,
                        number,
                        round.session_id,
                        round.head_sha,
                        round.decision,
                        round.red,
                        round.yellow,
                        round.green,
                        now_unix(),
                    ],
                )?;
                RecordedRound {
                    id: transaction.last_insert_rowid(),
                    round: number,
                    first_time: true,
                }
            }
        };
        transaction.commit()?;
        Ok(recorded)
    }

    /// The comment id to PATCH on the next round of the same pull request.
    /// Carried forward from the newest round that has one.
    pub fn last_comment_id(&self, repo: &str, pr_number: i64) -> rusqlite::Result<Option<i64>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .query_row(
                "SELECT comment_id FROM review_rounds
                  WHERE repo = ?1 AND pr_number = ?2 AND comment_id IS NOT NULL
                  ORDER BY round DESC LIMIT 1",
                params![repo, pr_number],
                |row| row.get(0),
            )
            .optional()
    }

    /// The comment this session's round posted — the review timeline entry
    /// links to it.
    pub fn round_comment_id(&self, session_id: &str) -> rusqlite::Result<Option<i64>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection
            .query_row(
                "SELECT comment_id FROM review_rounds
                  WHERE session_id = ?1 AND comment_id IS NOT NULL",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
    }

    pub fn set_round_comment_id(&self, session_id: &str, comment_id: i64) -> rusqlite::Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "UPDATE review_rounds SET comment_id = ?2 WHERE session_id = ?1",
            params![session_id, comment_id],
        )?;
        Ok(())
    }

    /// Append a session's findings. Idempotent by session: a redelivery adds
    /// nothing, since the ledger is append-only and would otherwise double.
    /// Read the ledger back, newest first. Filters are ANDed; an unset filter
    /// does not constrain. Bound parameters throughout — `limit` is the only
    /// interpolation and it is a `usize` the caller has already clamped.
    pub fn review_findings(
        &self,
        query: &ReviewFindingQuery,
    ) -> rusqlite::Result<Vec<ReviewFindingRow>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection.prepare(
            "SELECT id, session_id, repo, pr_number, stable_id, severity, status,
                    title, path, line, raised_by, angle, head_sha, created_at,
                    decided_by, decided_reason, decided_at, waiver_id
               FROM review_findings
              WHERE (?1 IS NULL OR repo = ?1)
                AND (?2 IS NULL OR pr_number = ?2)
                AND (?3 IS NULL OR status = ?3)
                AND (?4 IS NULL OR severity = ?4)
              ORDER BY id DESC
              LIMIT ?5",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                query.repo,
                query.pr_number,
                query.status,
                query.severity,
                query.limit as i64,
            ],
            |row| {
                Ok(ReviewFindingRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    repo: row.get(2)?,
                    pr_number: row.get(3)?,
                    stable_id: row.get(4)?,
                    severity: row.get(5)?,
                    status: row.get(6)?,
                    title: row.get(7)?,
                    path: row.get(8)?,
                    line: row.get(9)?,
                    raised_by: row.get(10)?,
                    angle: row.get(11)?,
                    head_sha: row.get(12)?,
                    created_at: row.get(13)?,
                    decided_by: row.get(14)?,
                    decided_reason: row.get(15)?,
                    decided_at: row.get(16)?,
                    waiver_id: row.get(17)?,
                })
            },
        )?;
        rows.collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_review_finding(
        &self,
        repo: &str,
        pr_number: i64,
        stable_id: &str,
        head_sha: &str,
        status: &str,
        decided_by: &str,
        reason: Option<&str>,
    ) -> rusqlite::Result<Option<ReviewFindingRow>> {
        let connection = self.connection.lock().unwrap();
        // The head_sha predicate is the compare-and-swap (ADR 038 point 4): a
        // decision taken against a head that has since moved matches nothing,
        // so it cannot land on a newer round's finding of the same name.
        let changed = connection.execute(
            "UPDATE review_findings
                SET status = ?5, decided_by = ?6, decided_reason = ?7, decided_at = ?8
              WHERE repo = ?1 AND pr_number = ?2 AND stable_id = ?3 AND head_sha = ?4",
            rusqlite::params![
                repo,
                pr_number,
                stable_id,
                head_sha,
                status,
                decided_by,
                reason,
                now_unix(),
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let row = Self::finding_row(&connection, repo, pr_number, stable_id, head_sha)?;
        Ok(row)
    }

    fn finding_row(
        connection: &Connection,
        repo: &str,
        pr_number: i64,
        stable_id: &str,
        head_sha: &str,
    ) -> rusqlite::Result<Option<ReviewFindingRow>> {
        let mut statement = connection.prepare(
            "SELECT id, session_id, repo, pr_number, stable_id, severity, status,
                    title, path, line, raised_by, angle, head_sha, created_at,
                    decided_by, decided_reason, decided_at, waiver_id
               FROM review_findings
              WHERE repo = ?1 AND pr_number = ?2 AND stable_id = ?3 AND head_sha = ?4
              ORDER BY id DESC LIMIT 1",
        )?;
        statement
            .query_row(
                rusqlite::params![repo, pr_number, stable_id, head_sha],
                |row| {
                    Ok(ReviewFindingRow {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        repo: row.get(2)?,
                        pr_number: row.get(3)?,
                        stable_id: row.get(4)?,
                        severity: row.get(5)?,
                        status: row.get(6)?,
                        title: row.get(7)?,
                        path: row.get(8)?,
                        line: row.get(9)?,
                        raised_by: row.get(10)?,
                        angle: row.get(11)?,
                        head_sha: row.get(12)?,
                        created_at: row.get(13)?,
                        decided_by: row.get(14)?,
                        decided_reason: row.get(15)?,
                        decided_at: row.get(16)?,
                        waiver_id: row.get(17)?,
                    })
                },
            )
            .optional()
    }

    /// ADR 038 v2 in one transaction: flip the finding to `waived` under the
    /// head-sha compare-and-swap, mint the repo-scoped waiver from the
    /// finding's own council-authored title and path, link the two. The
    /// author's reason lands on the finding row only — the waiver's text is
    /// what future chairs are shown, and it must never be the author's prose
    /// (ADR 038 point 7).
    #[allow(clippy::too_many_arguments)]
    pub fn waive_review_finding(
        &self,
        repo: &str,
        pr_number: i64,
        stable_id: &str,
        head_sha: &str,
        decided_by: &str,
        reason: &str,
        expires_at: i64,
    ) -> rusqlite::Result<Option<(ReviewFindingRow, ReviewWaiver)>> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_unix();
        let changed = transaction.execute(
            "UPDATE review_findings
                SET status = 'waived', decided_by = ?5, decided_reason = ?6, decided_at = ?7
              WHERE repo = ?1 AND pr_number = ?2 AND stable_id = ?3 AND head_sha = ?4",
            rusqlite::params![repo, pr_number, stable_id, head_sha, decided_by, reason, now,],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let Some(row) = Self::finding_row(&transaction, repo, pr_number, stable_id, head_sha)?
        else {
            return Ok(None);
        };
        let waiver = ReviewWaiver {
            id: super::new_waiver_id(),
            repo: repo.into(),
            path_class: row.path.clone(),
            text: row.title.clone(),
            origin_pr: Some(format!("{repo}#{pr_number}")),
            created_by: decided_by.into(),
            created_at: now,
            expires_at,
            revoked_at: None,
            fired_count: 0,
            last_fired_at: None,
            source: super::WAIVER_SOURCE_AUTHOR.into(),
        };
        transaction.execute(
            "INSERT INTO review_waivers
                (id, repo, path_class, text, origin_pr, created_by, created_at,
                 expires_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                waiver.id,
                waiver.repo,
                waiver.path_class,
                waiver.text,
                waiver.origin_pr,
                waiver.created_by,
                waiver.created_at,
                waiver.expires_at,
                waiver.source,
            ],
        )?;
        transaction.execute(
            "UPDATE review_findings SET waiver_id = ?2 WHERE id = ?1",
            rusqlite::params![row.id, waiver.id],
        )?;
        transaction.commit()?;
        let row = ReviewFindingRow {
            waiver_id: Some(waiver.id.clone()),
            ..row
        };
        Ok(Some((row, waiver)))
    }

    pub fn record_review_findings(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
        findings: &[ReviewFinding],
    ) -> rusqlite::Result<usize> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM review_findings WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        if already > 0 {
            return Ok(0);
        }
        let now = now_unix();
        for finding in findings {
            transaction.execute(
                "INSERT INTO review_findings
                   (session_id, repo, pr_number, stable_id, severity, status,
                    title, path, line, raised_by, angle, head_sha, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    session_id,
                    repo,
                    pr_number,
                    finding.stable_id,
                    finding.severity,
                    finding.status,
                    finding.title,
                    finding.path,
                    finding.line,
                    finding.raised_by,
                    finding.angle,
                    head_sha,
                    now,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(findings.len())
    }

    /// Queue a GitHub write. Returns false when this (session, kind) is
    /// already queued — the guarantee that at-least-once delivery cannot
    /// produce a second comment, status or review.
    pub fn enqueue_write(
        &self,
        session_id: &str,
        kind: &str,
        payload: &Value,
    ) -> rusqlite::Result<bool> {
        let created_at = now_unix();
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO github_writes
               (session_id, kind, payload_json, state, attempts, created_at)
             VALUES (?1, ?2, ?3, 'pending', 0, ?4)",
            params![session_id, kind, payload.to_string(), created_at],
        )?;
        let write_id = transaction.query_row(
            "SELECT id FROM github_writes WHERE session_id = ?1 AND kind = ?2",
            params![session_id, kind],
            |row| row.get::<_, i64>(0),
        )?;
        if inserted == 1 {
            let event = super::new_audit_event(
                format!("github.write.enqueued:{write_id}"),
                "github.write.enqueued",
                super::AuditOutcome::Pending,
                created_at,
                super::AuditCorrelation {
                    session_id: Some(session_id.into()),
                    write_id: Some(write_id.to_string()),
                    ..Default::default()
                },
                serde_json::json!({"operation": kind}),
                None,
            );
            Self::append_audit_event_locked(&transaction, &event)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        transaction.commit()?;
        Ok(inserted == 1)
    }

    /// Take ownership of up to `limit` queued writes.
    ///
    /// Claiming is what stops two concurrent drains from both sending the same
    /// comment — `create_comment` and `submit_review` are not idempotent, so a
    /// double send is a double post. The read and the state change happen in
    /// one IMMEDIATE transaction; a claim abandoned by a dying process becomes
    /// available again after `WRITE_CLAIM_LEASE_SECS`, the same lease idiom the
    /// delivery table already uses.
    pub fn claim_writes(&self, limit: i64) -> rusqlite::Result<Vec<PendingWrite>> {
        self.claim_writes_at(limit, now_unix())
    }

    fn claim_writes_at(&self, limit: i64, now: i64) -> rusqlite::Result<Vec<PendingWrite>> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claimed: Vec<PendingWrite>;
        {
            let mut statement = transaction.prepare(
                "SELECT id, session_id, kind, payload_json, attempts, state
                   FROM github_writes
                  WHERE state = 'pending'
                     OR (state = 'in_flight' AND claimed_at <= ?2)
                  ORDER BY id LIMIT ?1",
            )?;
            claimed = statement
                .query_map(
                    params![limit, now.saturating_sub(WRITE_CLAIM_LEASE_SECS)],
                    |row| {
                        Ok(PendingWrite {
                            id: row.get(0)?,
                            session_id: row.get(1)?,
                            kind: row.get(2)?,
                            payload: serde_json::from_str(&row.get::<_, String>(3)?)
                                .unwrap_or(Value::Null),
                            attempts: row.get(4)?,
                            was_reclaimed: row.get::<_, String>(5)? == "in_flight",
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
        }
        for write in &claimed {
            transaction.execute(
                "UPDATE github_writes SET state = 'in_flight', claimed_at = ?2 WHERE id = ?1",
                params![write.id, now],
            )?;
        }
        transaction.commit()?;
        Ok(claimed)
    }

    /// Claim rows as if every lease had lapsed — the crash-replay test's way
    /// of fast-forwarding time.
    #[cfg(test)]
    pub fn claim_writes_for_test_after_lease(
        &self,
        limit: i64,
    ) -> rusqlite::Result<Vec<PendingWrite>> {
        self.claim_writes_at(limit, now_unix() + WRITE_CLAIM_LEASE_SECS + 1)
    }

    /// Queued-but-unsent writes, claimed or not. For inspection and tests —
    /// sending goes through `claim_writes`.
    pub fn pending_writes(&self, limit: i64) -> rusqlite::Result<Vec<PendingWrite>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection.prepare(
            "SELECT id, session_id, kind, payload_json, attempts
               FROM github_writes WHERE state IN ('pending', 'in_flight')
               ORDER BY id LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit], |row| {
                Ok(PendingWrite {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or(Value::Null),
                    attempts: row.get(4)?,
                    was_reclaimed: false,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn mark_write_done(&self, id: i64) -> rusqlite::Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "UPDATE github_writes
                SET state = 'done', last_error = NULL, claimed_at = NULL, done_at = ?2
              WHERE id = ?1",
            params![id, now_unix()],
        )?;
        Ok(())
    }

    /// Count the attempt and keep the row retryable until it has burned
    /// through `WRITE_MAX_ATTEMPTS`, then park it as `failed`.
    pub fn mark_write_failed(&self, id: i64, error: &str) -> rusqlite::Result<()> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "UPDATE github_writes
                SET attempts = attempts + 1,
                    last_error = ?2,
                    claimed_at = NULL,
                    state = CASE WHEN attempts + 1 >= ?3 THEN 'failed' ELSE 'pending' END
              WHERE id = ?1",
            params![id, error, WRITE_MAX_ATTEMPTS],
        )?;
        Ok(())
    }

    fn append_audit_event_locked(
        transaction: &rusqlite::Transaction<'_>,
        event: &AuditEvent,
    ) -> StoreResult<()> {
        event
            .validate()
            .map_err(|error| StoreError::Pool(format!("invalid audit event: {error}")))?;
        let detail_json = serde_json::to_string(&event.detail)
            .map_err(|error| StoreError::Pool(format!("serialize audit detail: {error}")))?;
        let error_json = event
            .error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| StoreError::Pool(format!("serialize audit error: {error}")))?;
        let actor = event.actor.as_ref();
        let target = event.target.as_ref();
        transaction.execute(
            "INSERT OR IGNORE INTO audit_events
                (event_id, event_key, version, occurred_at, recorded_at, service, kind, outcome,
                 caused_by, delivery_id, controller_id, action_id, scope, trigger_ref,
                 trigger_fingerprint, session_id, message_id, runtime_event_id, write_id,
                 actor_kind, actor_id, actor_display, actor_association, target_kind, target_ref,
                 target_revision, detail_json, error_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
            rusqlite::params![
                event.event_id,
                event.event_key,
                i64::from(event.version),
                event.occurred_at,
                event.recorded_at,
                event.service,
                event.kind,
                event.outcome.as_str(),
                event.caused_by,
                event.correlation.delivery_id,
                event.correlation.controller_id,
                event.correlation.action_id,
                event.correlation.scope,
                event.correlation.trigger_ref,
                event.correlation.trigger_fingerprint,
                event.correlation.session_id,
                event.correlation.message_id,
                event.correlation.runtime_event_id,
                event.correlation.write_id,
                actor.map(|value| value.kind.as_str()),
                actor.and_then(|value| value.id.as_deref()),
                actor.and_then(|value| value.display.as_deref()),
                actor.and_then(|value| value.association.as_deref()),
                target.map(|value| value.kind.as_str()),
                target.and_then(|value| value.reference.as_deref()),
                target.and_then(|value| value.revision.as_deref()),
                detail_json,
                error_json,
            ],
        )?;
        Ok(())
    }

    pub fn append_audit_event(&self, event: &AuditEvent) -> StoreResult<AuditEventRecord> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::append_audit_event_locked(&transaction, event)?;
        let record = transaction.query_row(
            "SELECT seq, event_id, event_key, version, occurred_at, recorded_at, service, kind,
                    outcome, caused_by, delivery_id, controller_id, action_id, scope, trigger_ref,
                    trigger_fingerprint, session_id, message_id, runtime_event_id, write_id,
                    actor_kind, actor_id, actor_display, actor_association, target_kind, target_ref,
                    target_revision, detail_json, error_json
             FROM audit_events WHERE service = ?1 AND event_key = ?2",
            rusqlite::params![event.service, event.event_key],
            audit_event_from_sqlite_row,
        )?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn audit_events(&self, query: &AuditEventQuery) -> StoreResult<AuditEventPage> {
        let limit = query.bounded_limit();
        let cursor_recorded_at = query.cursor.map(|cursor| cursor.recorded_at);
        let cursor_seq = query.cursor.map(|cursor| cursor.seq);
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection.prepare(
            "SELECT seq, event_id, event_key, version, occurred_at, recorded_at, service, kind,
                    outcome, caused_by, delivery_id, controller_id, action_id, scope, trigger_ref,
                    trigger_fingerprint, session_id, message_id, runtime_event_id, write_id,
                    actor_kind, actor_id, actor_display, actor_association, target_kind, target_ref,
                    target_revision, detail_json, error_json
             FROM audit_events
             WHERE (?1 IS NULL OR delivery_id = ?1)
               AND (?2 IS NULL OR controller_id = ?2)
               AND (?3 IS NULL OR action_id = ?3)
               AND (?4 IS NULL OR runtime_event_id = ?4)
               AND (?5 IS NULL OR session_id = ?5)
               AND (?6 IS NULL OR message_id = ?6)
               AND (?7 IS NULL OR write_id = ?7)
               AND (?8 IS NULL OR trigger_ref = ?8)
               AND (?9 IS NULL OR kind = ?9)
               AND (?10 IS NULL OR recorded_at >= ?10)
               AND (?11 IS NULL OR recorded_at <= ?11)
               AND (?12 IS NULL OR recorded_at < ?12
                    OR (recorded_at = ?12 AND seq < ?13))
             ORDER BY recorded_at DESC, seq DESC
             LIMIT ?14",
        )?;
        let mut events = statement
            .query_map(
                rusqlite::params![
                    query.delivery_id.as_deref(),
                    query.controller_id.as_deref(),
                    query.action_id.as_deref(),
                    query.runtime_event_id.as_deref(),
                    query.session_id.as_deref(),
                    query.message_id.as_deref(),
                    query.write_id.as_deref(),
                    query.trigger_ref.as_deref(),
                    query.kind.as_deref(),
                    query.since,
                    query.until,
                    cursor_recorded_at,
                    cursor_seq,
                    (limit + 1) as i64,
                ],
                audit_event_from_sqlite_row,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = events.len() > limit;
        if has_more {
            events.truncate(limit);
        }
        let next_cursor = has_more
            .then(|| {
                events.last().map(|event| {
                    AuditCursor {
                        recorded_at: event.event.recorded_at,
                        seq: event.seq,
                    }
                    .encode()
                })
            })
            .flatten();
        Ok(AuditEventPage {
            events,
            next_cursor,
        })
    }

    pub fn prune_audit_events(&self, before: i64, extended_before: i64) -> StoreResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        Ok(connection.execute(
            "DELETE FROM audit_events
             WHERE (recorded_at < ?1 AND NOT (
                       outcome IN ('failed', 'outcome_unknown', 'reconciled')
                       OR kind LIKE 'security.%'
                       OR kind LIKE 'config.%'
                       OR kind LIKE 'operator.%'
                       OR kind LIKE '%dead_lettered'
                       OR kind = 'audit.retention_pruned'
                   ))
                OR (recorded_at < ?2 AND (
                       outcome IN ('failed', 'outcome_unknown', 'reconciled')
                       OR kind LIKE 'security.%'
                       OR kind LIKE 'config.%'
                       OR kind LIKE 'operator.%'
                       OR kind LIKE '%dead_lettered'
                       OR kind = 'audit.retention_pruned'
                   ))",
            rusqlite::params![before, extended_before.min(before)],
        )?)
    }

    fn prune_shadow_comparisons_at(
        &self,
        now: i64,
        retention_secs: i64,
    ) -> rusqlite::Result<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "DELETE FROM shadow_comparisons WHERE created_at < ?1",
            [now.saturating_sub(retention_secs)],
        )
    }

    fn prune_runtime_events_at(&self, now: i64, retention_secs: i64) -> rusqlite::Result<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "DELETE FROM runtime_event_receipts WHERE received_at < ?1",
            [now.saturating_sub(retention_secs)],
        )
    }
}

#[async_trait::async_trait]
impl ProductStore for SqliteStore {
    async fn begin_delivery(
        &self,
        delivery_id: &str,
        event_type: &str,
        repository: Option<&str>,
        payload_sha256: &str,
    ) -> StoreResult<DeliveryAdmission> {
        Ok(SqliteStore::begin_delivery(
            self,
            delivery_id,
            event_type,
            repository,
            payload_sha256,
        )?)
    }

    async fn finish_delivery(
        &self,
        delivery_id: &str,
        state: &str,
        result: &Value,
    ) -> StoreResult<()> {
        Ok(SqliteStore::finish_delivery(
            self,
            delivery_id,
            state,
            result,
        )?)
    }

    async fn release_delivery_for_retry(
        &self,
        delivery_id: &str,
        result: &Value,
    ) -> StoreResult<()> {
        Ok(SqliteStore::release_delivery_for_retry(
            self,
            delivery_id,
            result,
        )?)
    }

    async fn prune_completed_deliveries(&self) -> StoreResult<usize> {
        Ok(SqliteStore::prune_completed_deliveries(self)?)
    }

    async fn record_shadow_comparison(
        &self,
        request_sha256: &str,
        repository: Option<&str>,
        report: &crate::shadow::ShadowReport,
    ) -> StoreResult<ShadowAdmission> {
        Ok(SqliteStore::record_shadow_comparison(
            self,
            request_sha256,
            repository,
            report,
        )?)
    }

    async fn shadow_summary(&self) -> StoreResult<ShadowSummary> {
        Ok(SqliteStore::shadow_summary(self)?)
    }

    async fn record_runtime_event(
        &self,
        body_sha256: &str,
        event: &crate::runtime_events::RuntimeEventEnvelope,
    ) -> StoreResult<RuntimeEventAdmission> {
        Ok(SqliteStore::record_runtime_event(self, body_sha256, event)?)
    }

    async fn canary_summary(&self) -> StoreResult<CanarySummary> {
        Ok(SqliteStore::canary_summary(self)?)
    }

    async fn next_round(&self, repo: &str, pr_number: i64) -> StoreResult<i64> {
        Ok(SqliteStore::next_round(self, repo, pr_number)?)
    }

    async fn record_session_target(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
    ) -> StoreResult<()> {
        Ok(SqliteStore::record_session_target(
            self, session_id, repo, pr_number, head_sha,
        )?)
    }

    async fn session_target(&self, session_id: &str) -> StoreResult<Option<SessionTarget>> {
        Ok(SqliteStore::session_target(self, session_id)?)
    }

    async fn record_review_round(&self, round: &ReviewRound) -> StoreResult<RecordedRound> {
        Ok(SqliteStore::record_review_round(self, round)?)
    }

    async fn last_comment_id(&self, repo: &str, pr_number: i64) -> StoreResult<Option<i64>> {
        Ok(SqliteStore::last_comment_id(self, repo, pr_number)?)
    }

    async fn round_comment_id(&self, session_id: &str) -> StoreResult<Option<i64>> {
        Ok(SqliteStore::round_comment_id(self, session_id)?)
    }

    async fn set_round_comment_id(&self, session_id: &str, comment_id: i64) -> StoreResult<()> {
        Ok(SqliteStore::set_round_comment_id(
            self, session_id, comment_id,
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn decide_review_finding(
        &self,
        repo: &str,
        pr_number: i64,
        stable_id: &str,
        head_sha: &str,
        status: &str,
        decided_by: &str,
        reason: Option<&str>,
    ) -> StoreResult<Option<ReviewFindingRow>> {
        Ok(SqliteStore::decide_review_finding(
            self, repo, pr_number, stable_id, head_sha, status, decided_by, reason,
        )?)
    }

    async fn review_findings(
        &self,
        query: &ReviewFindingQuery,
    ) -> StoreResult<Vec<ReviewFindingRow>> {
        Ok(SqliteStore::review_findings(self, query)?)
    }

    async fn create_review_waiver(
        &self,
        repo: &str,
        path_class: Option<&str>,
        text: &str,
        origin_pr: Option<&str>,
        created_by: &str,
        expires_at: i64,
        source: &str,
    ) -> StoreResult<ReviewWaiver> {
        let waiver = ReviewWaiver {
            id: super::new_waiver_id(),
            repo: repo.into(),
            path_class: path_class.map(Into::into),
            text: text.into(),
            origin_pr: origin_pr.map(Into::into),
            created_by: created_by.into(),
            created_at: now_unix(),
            expires_at,
            revoked_at: None,
            fired_count: 0,
            last_fired_at: None,
            source: source.into(),
        };
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "INSERT INTO review_waivers
                (id, repo, path_class, text, origin_pr, created_by, created_at,
                 expires_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                waiver.id,
                waiver.repo,
                waiver.path_class,
                waiver.text,
                waiver.origin_pr,
                waiver.created_by,
                waiver.created_at,
                waiver.expires_at,
                waiver.source,
            ],
        )?;
        Ok(waiver)
    }

    #[allow(clippy::too_many_arguments)]
    async fn waive_review_finding(
        &self,
        repo: &str,
        pr_number: i64,
        stable_id: &str,
        head_sha: &str,
        decided_by: &str,
        reason: &str,
        expires_at: i64,
    ) -> StoreResult<Option<(ReviewFindingRow, ReviewWaiver)>> {
        Ok(SqliteStore::waive_review_finding(
            self, repo, pr_number, stable_id, head_sha, decided_by, reason, expires_at,
        )?)
    }

    async fn list_review_waivers(
        &self,
        repo: Option<&str>,
        include_inactive: bool,
        now: i64,
    ) -> StoreResult<Vec<ReviewWaiver>> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = connection.prepare(
            "SELECT id, repo, path_class, text, origin_pr, created_by,
                    created_at, expires_at, revoked_at, fired_count, last_fired_at, source
               FROM review_waivers
              WHERE (?1 IS NULL OR repo = ?1)
                AND (?2 OR (revoked_at IS NULL AND expires_at > ?3))
              ORDER BY created_at",
        )?;
        let rows = statement.query_map(rusqlite::params![repo, include_inactive, now], |row| {
            Ok(ReviewWaiver {
                id: row.get(0)?,
                repo: row.get(1)?,
                path_class: row.get(2)?,
                text: row.get(3)?,
                origin_pr: row.get(4)?,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
                expires_at: row.get(7)?,
                revoked_at: row.get(8)?,
                fired_count: row.get(9)?,
                last_fired_at: row.get(10)?,
                source: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    async fn update_review_waiver(
        &self,
        id: &str,
        expires_at: Option<i64>,
        revoke: bool,
    ) -> StoreResult<bool> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let updated = connection.execute(
            "UPDATE review_waivers SET
                expires_at = COALESCE(?2, expires_at),
                revoked_at = CASE WHEN ?3 THEN COALESCE(revoked_at, ?4) ELSE revoked_at END
             WHERE id = ?1",
            rusqlite::params![id, expires_at, revoke, now_unix()],
        )?;
        Ok(updated == 1)
    }

    async fn record_waiver_fired(&self, repo: &str, ids: &[String]) -> StoreResult<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
        let mut bumped = 0;
        for id in unique {
            bumped += connection.execute(
                "UPDATE review_waivers
                    SET fired_count = fired_count + 1, last_fired_at = ?3
                  WHERE id = ?1 AND repo = ?2",
                rusqlite::params![id, repo, now],
            )?;
        }
        Ok(bumped)
    }

    async fn record_review_findings(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
        findings: &[ReviewFinding],
    ) -> StoreResult<usize> {
        Ok(SqliteStore::record_review_findings(
            self, session_id, repo, pr_number, head_sha, findings,
        )?)
    }

    async fn enqueue_write(
        &self,
        session_id: &str,
        kind: &str,
        payload: &Value,
    ) -> StoreResult<bool> {
        Ok(SqliteStore::enqueue_write(self, session_id, kind, payload)?)
    }

    async fn claim_writes(&self, limit: i64) -> StoreResult<Vec<PendingWrite>> {
        Ok(SqliteStore::claim_writes(self, limit)?)
    }

    async fn claim_writes_for_test_after_lease(
        &self,
        limit: i64,
    ) -> StoreResult<Vec<PendingWrite>> {
        Ok(self.claim_writes_at(limit, now_unix() + WRITE_CLAIM_LEASE_SECS + 1)?)
    }

    async fn pending_writes(&self, limit: i64) -> StoreResult<Vec<PendingWrite>> {
        Ok(SqliteStore::pending_writes(self, limit)?)
    }

    async fn mark_write_done(&self, id: i64) -> StoreResult<()> {
        Ok(SqliteStore::mark_write_done(self, id)?)
    }

    async fn mark_write_failed(&self, id: i64, error: &str) -> StoreResult<()> {
        Ok(SqliteStore::mark_write_failed(self, id, error)?)
    }

    async fn append_audit_event(&self, event: &AuditEvent) -> StoreResult<AuditEventRecord> {
        SqliteStore::append_audit_event(self, event)
    }

    async fn audit_events(&self, query: &AuditEventQuery) -> StoreResult<AuditEventPage> {
        SqliteStore::audit_events(self, query)
    }

    async fn prune_audit_events(&self, before: i64, extended_before: i64) -> StoreResult<usize> {
        SqliteStore::prune_audit_events(self, before, extended_before)
    }
}

fn migrate(connection: &Connection) -> rusqlite::Result<()> {
    let applied: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied.max(0) as usize) {
        // user_version cannot be bound, hence the format!; `index` is an
        // enumerate counter, never user input.
        connection.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {}; COMMIT;",
            index + 1
        ))?;
    }
    Ok(())
}

fn audit_event_from_sqlite_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEventRecord> {
    let outcome_raw: String = row.get(8)?;
    let outcome = AuditOutcome::parse(&outcome_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown audit outcome {outcome_raw}"),
            )),
        )
    })?;
    let detail_raw: String = row.get(27)?;
    let detail = serde_json::from_str(&detail_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(27, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let error = row
        .get::<_, Option<String>>(28)?
        .map(|raw| {
            serde_json::from_str::<AuditError>(&raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    28,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    let actor = row.get::<_, Option<String>>(20)?.map(|kind| AuditActor {
        kind,
        id: row.get(21).ok().flatten(),
        display: row.get(22).ok().flatten(),
        association: row.get(23).ok().flatten(),
    });
    let target = row.get::<_, Option<String>>(24)?.map(|kind| AuditTarget {
        kind,
        reference: row.get(25).ok().flatten(),
        revision: row.get(26).ok().flatten(),
    });
    Ok(AuditEventRecord {
        seq: row.get(0)?,
        event: AuditEvent {
            version: row.get::<_, i64>(3)? as u16,
            event_id: row.get(1)?,
            event_key: row.get(2)?,
            occurred_at: row.get(4)?,
            recorded_at: row.get(5)?,
            service: row.get(6)?,
            kind: row.get(7)?,
            outcome,
            caused_by: row.get(9)?,
            correlation: AuditCorrelation {
                delivery_id: row.get(10)?,
                controller_id: row.get(11)?,
                action_id: row.get(12)?,
                scope: row.get(13)?,
                trigger_ref: row.get(14)?,
                trigger_fingerprint: row.get(15)?,
                session_id: row.get(16)?,
                message_id: row.get(17)?,
                runtime_event_id: row.get(18)?,
                write_id: row.get(19)?,
            },
            actor,
            target,
            detail,
            error,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn finding(stable_id: &str, severity: &str) -> crate::store::ReviewFinding {
        crate::store::ReviewFinding {
            stable_id: stable_id.into(),
            severity: severity.into(),
            status: "open".into(),
            title: format!("{stable_id} title"),
            path: Some("internal/services/cicd/main.go".into()),
            line: Some(389),
            raised_by: Some("rev-codex".into()),
            angle: Some("security".into()),
        }
    }

    #[test]
    fn a_decisions_writes_survive_the_rounds_own_outbox_rows() {
        // Regression, council review of #370 F1: the outbox is
        // UNIQUE(session_id, kind) with INSERT OR IGNORE, so decision writes
        // that reused the round's kinds were silently dropped — the author was
        // told the pull request had been unblocked while nothing on it moved.
        let store = SqliteStore::memory().unwrap();
        let round = json!({"repo": "zeabur/backend", "sha": "f9caff5d"});
        assert!(store
            .enqueue_write("ses_1", crate::closing::KIND_STATUS, &round)
            .unwrap());
        assert!(store
            .enqueue_write("ses_1", crate::closing::KIND_REVIEW, &round)
            .unwrap());
        // Same session, same operations, different event: these must land.
        let status = crate::deciding::decision_kind(crate::deciding::KIND_DECISION_STATUS, 42);
        let review = crate::deciding::decision_kind(crate::deciding::KIND_DECISION_REVIEW, 42);
        assert!(store.enqueue_write("ses_1", &status, &round).unwrap());
        assert!(store.enqueue_write("ses_1", &review, &round).unwrap());
        // A redelivered command lands on the same row and posts once.
        assert!(!store.enqueue_write("ses_1", &status, &round).unwrap());
        // A later decision on the same session gets rows of its own.
        let later = crate::deciding::decision_kind(crate::deciding::KIND_DECISION_STATUS, 43);
        assert!(store.enqueue_write("ses_1", &later, &round).unwrap());

        let pending = store.claim_writes(10).unwrap();
        let kinds: Vec<&str> = pending.iter().map(|w| w.kind.as_str()).collect();
        assert!(kinds.contains(&status.as_str()), "got {kinds:?}");
        assert!(kinds.contains(&review.as_str()), "got {kinds:?}");
        assert_eq!(kinds.len(), 5);
    }

    #[test]
    fn a_decision_lands_on_the_reviewed_head_and_nowhere_else() {
        let store = SqliteStore::memory().unwrap();
        store
            .record_review_findings(
                "ses_1",
                "zeabur/backend",
                2382,
                Some("head-old"),
                &[finding("F1", "red"), finding("F2", "green")],
            )
            .unwrap();

        // Wrong head: the compare-and-swap misses, and nothing is written —
        // a stale decision must never unblock code nobody reviewed.
        assert!(SqliteStore::decide_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F1",
            "head-new",
            "dismissed",
            "yuaanlin",
            Some("no")
        )
        .unwrap()
        .is_none());
        let untouched = SqliteStore::review_findings(
            &store,
            &crate::store::ReviewFindingQuery {
                repo: Some("zeabur/backend".into()),
                pr_number: Some(2382),
                status: None,
                severity: None,
                limit: 10,
            },
        )
        .unwrap();
        assert!(untouched.iter().all(|row| row.status == "open"));

        // Right head: the row carries who, why and when.
        let decided = SqliteStore::decide_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F1",
            "head-old",
            "dismissed",
            "yuaanlin",
            Some("validator pins the IP first"),
        )
        .unwrap()
        .expect("the reviewed head matches");
        assert_eq!(decided.status, "dismissed");
        assert_eq!(decided.decided_by.as_deref(), Some("yuaanlin"));
        assert_eq!(
            decided.decided_reason.as_deref(),
            Some("validator pins the IP first")
        );
        assert!(decided.decided_at.is_some());
        assert_eq!(decided.title, "F1 title", "the council's own words survive");
    }

    #[test]
    fn an_unknown_finding_id_reports_a_miss_rather_than_inventing_a_row() {
        let store = SqliteStore::memory().unwrap();
        store
            .record_review_findings(
                "ses_1",
                "zeabur/backend",
                2382,
                Some("head"),
                &[finding("F1", "red")],
            )
            .unwrap();
        assert!(SqliteStore::decide_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F9",
            "head",
            "dismissed",
            "yuaanlin",
            Some("x")
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn reopen_restores_the_finding_and_keeps_the_trail() {
        let store = SqliteStore::memory().unwrap();
        store
            .record_review_findings(
                "ses_1",
                "zeabur/backend",
                2382,
                Some("h"),
                &[finding("F1", "red")],
            )
            .unwrap();
        SqliteStore::decide_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F1",
            "h",
            "dismissed",
            "yuaanlin",
            Some("mistake"),
        )
        .unwrap()
        .unwrap();
        let reopened = SqliteStore::decide_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F1",
            "h",
            "open",
            "yuaanlin",
            Some("undo"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(reopened.status, "open");
        // Undo is itself a decision: the record says who undid it and why.
        assert_eq!(reopened.decided_reason.as_deref(), Some("undo"));
    }

    #[test]
    fn a_waive_flips_the_finding_and_mints_a_linked_repo_scoped_waiver() {
        let store = SqliteStore::memory().unwrap();
        store
            .record_review_findings(
                "ses_1",
                "zeabur/backend",
                2382,
                Some("h"),
                &[finding("F1", "red")],
            )
            .unwrap();
        let expires = now_unix() + 90 * 86_400;
        let (row, waiver) = SqliteStore::waive_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F1",
            "h",
            "yuaanlin",
            "eval traffic only, capped upstream",
            expires,
        )
        .unwrap()
        .expect("the reviewed head matches");
        assert_eq!(row.status, "waived");
        assert_eq!(row.decided_by.as_deref(), Some("yuaanlin"));
        assert_eq!(
            row.decided_reason.as_deref(),
            Some("eval traffic only, capped upstream")
        );
        assert_eq!(row.waiver_id.as_deref(), Some(waiver.id.as_str()));
        // The waiver carries the council's words, never the author's prose
        // (ADR 038 point 7) — and it is repo-scoped and expiring.
        assert_eq!(waiver.repo, "zeabur/backend");
        assert_eq!(waiver.text, "F1 title");
        assert_eq!(
            waiver.path_class.as_deref(),
            Some("internal/services/cicd/main.go")
        );
        assert_eq!(waiver.origin_pr.as_deref(), Some("zeabur/backend#2382"));
        assert_eq!(waiver.created_by, "yuaanlin");
        assert_eq!(waiver.expires_at, expires);
        assert_eq!(waiver.source, crate::store::WAIVER_SOURCE_AUTHOR);
        assert!(!waiver.text.contains("eval traffic"), "no author prose");

        // The row reads back with the link, and the waiver is active — the
        // exact rows the dispatch-time chair injection lists for the repo.
        let rows = SqliteStore::review_findings(
            &store,
            &crate::store::ReviewFindingQuery {
                repo: Some("zeabur/backend".into()),
                pr_number: Some(2382),
                status: None,
                severity: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(rows[0].waiver_id.as_deref(), Some(waiver.id.as_str()));
        let active =
            futures_block_on(store.list_review_waivers(Some("zeabur/backend"), false, now_unix()))
                .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, waiver.id);
        assert_eq!(active[0].source, crate::store::WAIVER_SOURCE_AUTHOR);
    }

    #[test]
    fn a_waive_on_a_moved_head_writes_nothing_at_all() {
        // The compare-and-swap must cover the waiver too: a stale waive that
        // still minted a repo-scoped suppression would silence rounds on code
        // nobody reviewed — worse than the stale dismiss it guards against.
        let store = SqliteStore::memory().unwrap();
        store
            .record_review_findings(
                "ses_1",
                "zeabur/backend",
                2382,
                Some("head-old"),
                &[finding("F1", "red")],
            )
            .unwrap();
        assert!(SqliteStore::waive_review_finding(
            &store,
            "zeabur/backend",
            2382,
            "F1",
            "head-new",
            "yuaanlin",
            "reason",
            now_unix() + 86_400,
        )
        .unwrap()
        .is_none());
        let waivers =
            futures_block_on(store.list_review_waivers(Some("zeabur/backend"), true, now_unix()))
                .unwrap();
        assert!(waivers.is_empty(), "no waiver may survive a missed CAS");
        let rows = SqliteStore::review_findings(
            &store,
            &crate::store::ReviewFindingQuery {
                repo: Some("zeabur/backend".into()),
                pr_number: Some(2382),
                status: None,
                severity: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(rows[0].status, "open");
    }

    /// The store trait is async but SQLite's methods are sync underneath;
    /// tests that need a trait-only method borrow a tiny executor.
    fn futures_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn delivery_ids_are_durable_idempotency_keys() {
        let store = SqliteStore::memory().unwrap();
        assert_eq!(
            store
                .begin_delivery("delivery-1", "pull_request", Some("example/repo"), "abc")
                .unwrap(),
            DeliveryAdmission::New
        );
        store
            .finish_delivery("delivery-1", "planned", &json!({"planned": true}))
            .unwrap();
        assert_eq!(
            store
                .begin_delivery("delivery-1", "pull_request", Some("example/repo"), "abc")
                .unwrap(),
            DeliveryAdmission::Duplicate {
                state: "planned".into(),
                result: Some(json!({"planned": true})),
            }
        );
        assert_eq!(
            store
                .begin_delivery("delivery-1", "pull_request", Some("example/repo"), "other")
                .unwrap(),
            DeliveryAdmission::Conflict
        );
    }

    #[test]
    fn processing_delivery_is_retriable_then_reclaimed_after_lease() {
        let store = SqliteStore::memory().unwrap();
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_000)
                .unwrap(),
            DeliveryAdmission::New
        );
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_100)
                .unwrap(),
            DeliveryAdmission::Duplicate {
                state: "processing".into(),
                result: None,
            }
        );
        assert_eq!(
            store
                .begin_delivery_at(
                    "delivery-1",
                    "pull_request",
                    Some("example/repo"),
                    "abc",
                    1_300,
                )
                .unwrap(),
            DeliveryAdmission::New
        );
    }

    #[test]
    fn retryable_delivery_is_immediately_readmitted_with_the_same_body() {
        let store = SqliteStore::memory().unwrap();
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_000)
                .unwrap(),
            DeliveryAdmission::New
        );
        store
            .release_delivery_for_retry("delivery-1", &json!({"error": "outage"}))
            .unwrap();
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_001)
                .unwrap(),
            DeliveryAdmission::New
        );
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "changed", 1_002)
                .unwrap(),
            DeliveryAdmission::Conflict
        );
    }

    #[test]
    fn delivery_retention_prunes_completed_and_abandoned_processing_rows() {
        let store = SqliteStore::memory().unwrap();
        store
            .begin_delivery_at("completed", "pull_request", None, "abc", 1_000)
            .unwrap();
        store
            .finish_delivery_at("completed", "planned", &json!({"ok": true}), 1_000)
            .unwrap();
        store
            .begin_delivery_at("acted", "pull_request", None, "acted-hash", 1_000)
            .unwrap();
        store
            .finish_delivery_at("acted", "acted", &json!({"ok": true}), 1_000)
            .unwrap();
        store
            .begin_delivery_at("abandoned", "pull_request", None, "def", 1_000)
            .unwrap();
        store
            .begin_delivery_at("recent", "pull_request", None, "ghi", 1_002)
            .unwrap();

        assert_eq!(
            store
                .prune_completed_deliveries_at(
                    1_000 + COMPLETED_RETENTION_SECS + 1,
                    COMPLETED_RETENTION_SECS,
                )
                .unwrap(),
            3
        );
        assert_eq!(
            store
                .begin_delivery_at("abandoned", "pull_request", None, "def", 2_000_000)
                .unwrap(),
            DeliveryAdmission::New,
            "processing rows older than retention are pruned"
        );
        assert_eq!(
            store
                .begin_delivery_at("recent", "pull_request", None, "changed", 1_003)
                .unwrap(),
            DeliveryAdmission::Conflict,
            "processing rows inside retention remain available for lease recovery"
        );
    }

    #[test]
    fn shadow_summary_records_counts_without_persisting_payloads() {
        let store = SqliteStore::memory().unwrap();
        let exact = crate::shadow::ShadowReport {
            comparison_id: "comparison-1".into(),
            exact_match: true,
            promotion_blocked: false,
            identity_or_ownership_mismatches: 0,
            presentation_mismatches: 0,
            mismatches: Vec::new(),
            controller: None,
        };
        let mut mismatch = exact.clone();
        mismatch.comparison_id = "comparison-2".into();
        mismatch.exact_match = false;
        mismatch.promotion_blocked = true;
        mismatch.identity_or_ownership_mismatches = 1;
        store
            .record_shadow_comparison("hash-1", Some("example/repo"), &exact)
            .unwrap();
        store
            .record_shadow_comparison("hash-2", Some("example/repo"), &mismatch)
            .unwrap();

        assert_eq!(
            store.shadow_summary().unwrap(),
            ShadowSummary {
                total: 2,
                exact_matches: 1,
                identity_or_ownership_mismatch_reports: 1,
                presentation_mismatch_reports: 0,
            }
        );

        assert_eq!(
            store
                .record_shadow_comparison("hash-1", Some("example/repo"), &exact)
                .unwrap(),
            ShadowAdmission::Duplicate
        );
        assert_eq!(
            store
                .record_shadow_comparison("changed", Some("example/repo"), &exact)
                .unwrap(),
            ShadowAdmission::Conflict
        );
    }

    #[test]
    fn runtime_event_receipts_dedupe_and_expose_aggregate_canary_state() {
        let store = SqliteStore::memory().unwrap();
        let event = crate::runtime_events::RuntimeEventEnvelope {
            version: "1".into(),
            event_id: "cev_1".into(),
            controller_id: "github-canary".into(),
            event_type: "session.timeout".into(),
            session_id: Some("ses_1".into()),
            occurred_at: 1_000,
            payload: json!({"reason": "timeout", "private": "not persisted"}),
        };
        assert_eq!(
            store.record_runtime_event("hash-1", &event).unwrap(),
            RuntimeEventAdmission::New
        );
        assert_eq!(
            store.record_runtime_event("hash-1", &event).unwrap(),
            RuntimeEventAdmission::Duplicate
        );
        assert_eq!(
            store.record_runtime_event("changed", &event).unwrap(),
            RuntimeEventAdmission::Conflict
        );
        assert_eq!(
            store.canary_summary().unwrap(),
            CanarySummary {
                acted_deliveries: 0,
                processing_deliveries: 0,
                retryable_deliveries: 0,
                runtime_events: 1,
                runtime_event_types: BTreeMap::from([("session.timeout".into(), 1)]),
                latest_event_occurred_at: Some(1_000),
            }
        );

        let connection = store.connection.lock().unwrap();
        let schema: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'runtime_event_receipts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!schema.contains("payload"));
        assert!(!schema.contains("body_json"));
    }

    fn round(session: &str, decision: &str) -> ReviewRound {
        ReviewRound {
            repo: "example/repo".into(),
            pr_number: 7,
            session_id: session.into(),
            head_sha: Some("deadbeef".into()),
            decision: decision.into(),
            red: 0,
            yellow: 0,
            green: 2,
        }
    }

    #[test]
    fn migrations_are_idempotent_and_adopt_a_pre_framework_database() {
        // A database written before `user_version` existed: the delivery tables
        // are there, the counter is 0. Migrating must not fail on the existing
        // tables, and must still apply everything after migration 0.
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0]).unwrap();
        migrate(&connection).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
                   AND name IN ('review_rounds', 'review_findings', 'github_writes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 3);

        // Re-running is a no-op, not a re-apply.
        migrate(&connection).unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[test]
    fn rounds_number_per_pull_request_and_redelivery_is_a_no_op() {
        let store = SqliteStore::memory().unwrap();
        let first = store
            .record_review_round(&round("ses_1", "request_changes"))
            .unwrap();
        assert_eq!((first.round, first.first_time), (1, true));

        // Same session again — an at-least-once terminal event redelivered.
        let again = store
            .record_review_round(&round("ses_1", "request_changes"))
            .unwrap();
        assert_eq!(
            (again.id, again.round, again.first_time),
            (first.id, 1, false)
        );

        let second = store
            .record_review_round(&round("ses_2", "approve"))
            .unwrap();
        assert_eq!((second.round, second.first_time), (2, true));

        // A different pull request starts its own numbering.
        let mut other = round("ses_3", "approve");
        other.pr_number = 8;
        assert_eq!(store.record_review_round(&other).unwrap().round, 1);
    }

    #[test]
    fn the_comment_anchor_carries_to_the_next_round() {
        let store = SqliteStore::memory().unwrap();
        store
            .record_review_round(&round("ses_1", "request_changes"))
            .unwrap();
        assert_eq!(store.last_comment_id("example/repo", 7).unwrap(), None);
        store.set_round_comment_id("ses_1", 9_001).unwrap();
        store
            .record_review_round(&round("ses_2", "approve"))
            .unwrap();
        assert_eq!(
            store.last_comment_id("example/repo", 7).unwrap(),
            Some(9_001),
            "round 2 patches round 1's comment instead of posting a new one"
        );
        assert_eq!(store.last_comment_id("example/repo", 8).unwrap(), None);
    }

    #[test]
    fn findings_are_written_once_per_session() {
        let store = SqliteStore::memory().unwrap();
        let findings = vec![ReviewFinding {
            stable_id: "f1".into(),
            severity: "red".into(),
            status: "open".into(),
            title: "unbounded read".into(),
            path: Some("src/lib.rs".into()),
            line: Some(12),
            raised_by: Some("rev-claude".into()),
            angle: Some("correctness".into()),
        }];
        assert_eq!(
            store
                .record_review_findings("ses_1", "example/repo", 7, Some("deadbeef"), &findings)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .record_review_findings("ses_1", "example/repo", 7, Some("deadbeef"), &findings)
                .unwrap(),
            0,
            "a redelivery must not double the ledger"
        );
        let connection = store.connection.lock().unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM review_findings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn a_claimed_write_is_invisible_to_a_second_drain_until_its_lease_lapses() {
        // Two terminal events closing in the same window spawn two drains.
        // Without claiming, both would read the same row and both would POST —
        // and a comment is not idempotent (council F2, #305).
        let store = SqliteStore::memory().unwrap();
        store
            .enqueue_write("ses_1", "comment", &json!({"body": "verdict"}))
            .unwrap();

        let first = store.claim_writes_at(10, 1_000).unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            store.claim_writes_at(10, 1_010).unwrap().is_empty(),
            "a concurrent drain must not take a claimed write"
        );

        // A process that died mid-send leaves the claim behind; the lease is
        // what stops the write being stranded forever.
        assert_eq!(
            store
                .claim_writes_at(10, 1_000 + WRITE_CLAIM_LEASE_SECS)
                .unwrap()
                .len(),
            1
        );

        // Finishing releases it either way.
        store.mark_write_failed(first[0].id, "502").unwrap();
        assert_eq!(store.claim_writes_at(10, 1_020).unwrap().len(), 1);
        store.mark_write_done(first[0].id).unwrap();
        assert!(store.claim_writes_at(10, 1_030).unwrap().is_empty());
    }

    #[test]
    fn audit_journal_is_idempotent_and_filters_by_delivery() {
        let store = SqliteStore::memory().unwrap();
        let event = AuditEvent {
            version: controller_protocol::audit::AUDIT_EVENT_VERSION,
            event_id: "aud:controller:delivery".into(),
            event_key: "ingress.received:d-1".into(),
            occurred_at: 10,
            recorded_at: 11,
            service: "github-pr-controller".into(),
            kind: "ingress.received".into(),
            outcome: AuditOutcome::Pending,
            caused_by: None,
            correlation: AuditCorrelation {
                delivery_id: Some("d-1".into()),
                ..Default::default()
            },
            actor: None,
            target: Some(AuditTarget {
                kind: "github_pull_request".into(),
                reference: Some("example/repo#1".into()),
                ..Default::default()
            }),
            detail: serde_json::json!({"payload_sha256": "hash"}),
            error: None,
        };
        let first = store.append_audit_event(&event).unwrap();
        assert_eq!(store.append_audit_event(&event).unwrap(), first);
        let page = store
            .audit_events(&AuditEventQuery {
                delivery_id: Some("d-1".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(
            page.events[0].event.target.as_ref().unwrap().kind,
            "github_pull_request"
        );
    }

    #[test]
    fn audit_retention_keeps_extended_events_until_the_second_cutoff() {
        let store = SqliteStore::memory().unwrap();
        let base = AuditEvent {
            version: controller_protocol::audit::AUDIT_EVENT_VERSION,
            event_id: String::new(),
            event_key: String::new(),
            occurred_at: 0,
            recorded_at: 0,
            service: "github-pr-controller".into(),
            kind: "ingress.received".into(),
            outcome: AuditOutcome::Accepted,
            caused_by: None,
            correlation: AuditCorrelation::default(),
            actor: None,
            target: None,
            detail: json!({}),
            error: None,
        };
        let append = |store: &SqliteStore, mut event: AuditEvent, key: &str, recorded_at: i64| {
            event.event_id = format!("aud:test:{key}");
            event.event_key = key.into();
            event.recorded_at = recorded_at;
            event.occurred_at = recorded_at;
            store.append_audit_event(&event).unwrap();
        };

        append(&store, base.clone(), "normal-old", 900);
        let mut extended = base.clone();
        extended.kind = "github.write.failed".into();
        extended.outcome = AuditOutcome::Failed;
        append(&store, extended.clone(), "failure-between-windows", 900);
        append(&store, extended, "failure-too-old", 100);
        append(&store, base, "recent", 1_100);

        assert_eq!(store.prune_audit_events(1_000, 200).unwrap(), 2);
        let page = store.audit_events(&AuditEventQuery::default()).unwrap();
        let keys: Vec<_> = page
            .events
            .iter()
            .map(|event| event.event.event_key.as_str())
            .collect();
        assert!(keys.contains(&"failure-between-windows"));
        assert!(keys.contains(&"recent"));
        assert!(!keys.contains(&"normal-old"));
        assert!(!keys.contains(&"failure-too-old"));
    }

    #[test]
    fn the_outbox_queues_once_and_parks_a_write_that_keeps_failing() {
        let store = SqliteStore::memory().unwrap();
        assert!(store
            .enqueue_write("ses_1", "review", &json!({"event": "APPROVE"}))
            .unwrap());
        assert!(
            !store
                .enqueue_write("ses_1", "review", &json!({"event": "APPROVE"}))
                .unwrap(),
            "the same write must never be queued twice"
        );
        assert!(store
            .enqueue_write("ses_1", "status", &json!({"state": "success"}))
            .unwrap());

        let pending = store.pending_writes(10).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].kind, "review");
        assert_eq!(pending[0].payload, json!({"event": "APPROVE"}));

        store.mark_write_done(pending[1].id).unwrap();
        let review = pending[0].id;
        for attempt in 1..WRITE_MAX_ATTEMPTS {
            store.mark_write_failed(review, "502").unwrap();
            assert_eq!(
                store.pending_writes(10).unwrap()[0].attempts,
                attempt,
                "still retryable"
            );
        }
        store.mark_write_failed(review, "502").unwrap();
        assert!(
            store.pending_writes(10).unwrap().is_empty(),
            "a write out of attempts is parked, not retried forever"
        );
    }
}
