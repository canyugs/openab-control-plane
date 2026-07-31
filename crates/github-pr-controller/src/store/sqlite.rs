//! The original SQLite backend — the default, unchanged in semantics by the
//! ADR 033 trait extraction. Pod-local file, IMMEDIATE-transaction claims,
//! `PRAGMA user_version` migrations.

use super::{
    now_unix, CanarySummary, DeliveryAdmission, PendingWrite, ProductStore, RecordedRound,
    ReviewFinding, ReviewRound, RuntimeEventAdmission, SessionTarget, ShadowAdmission,
    ShadowSummary, StoreResult, COMPLETED_RETENTION_SECS, PROCESSING_LEASE_SECS,
    WRITE_CLAIM_LEASE_SECS, WRITE_MAX_ATTEMPTS,
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
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        let inserted = connection.execute(
            "INSERT OR IGNORE INTO github_writes
               (session_id, kind, payload_json, state, attempts, created_at)
             VALUES (?1, ?2, ?3, 'pending', 0, ?4)",
            params![session_id, kind, payload.to_string(), now_unix()],
        )?;
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
                "SELECT id, session_id, kind, payload_json, attempts
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
