use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const PROCESSING_LEASE_SECS: i64 = 5 * 60;
const COMPLETED_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;

pub struct ProductStore {
    connection: Mutex<Connection>,
}

#[derive(Debug, PartialEq)]
pub enum DeliveryAdmission {
    New,
    Duplicate {
        state: String,
        result: Option<Value>,
    },
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShadowSummary {
    pub total: i64,
    pub exact_matches: i64,
    pub identity_or_ownership_mismatch_reports: i64,
    pub presentation_mismatch_reports: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowAdmission {
    New,
    Duplicate,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEventAdmission {
    New,
    Duplicate,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanarySummary {
    pub acted_deliveries: i64,
    pub processing_deliveries: i64,
    pub retryable_deliveries: i64,
    pub runtime_events: i64,
    pub runtime_event_types: BTreeMap<String, i64>,
    pub latest_event_occurred_at: Option<i64>,
}

impl ProductStore {
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
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS webhook_deliveries (
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
        )?;
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delivery_ids_are_durable_idempotency_keys() {
        let store = ProductStore::memory().unwrap();
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
        let store = ProductStore::memory().unwrap();
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
        let store = ProductStore::memory().unwrap();
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
        let store = ProductStore::memory().unwrap();
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
        let store = ProductStore::memory().unwrap();
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
        let store = ProductStore::memory().unwrap();
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
}
