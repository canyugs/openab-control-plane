use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
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
               ON shadow_comparisons(created_at);",
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
        Ok(deliveries + comparisons)
    }

    fn prune_completed_deliveries_at(
        &self,
        now: i64,
        retention_secs: i64,
    ) -> rusqlite::Result<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "DELETE FROM webhook_deliveries
              WHERE (state IN ('planned', 'ignored') AND completed_at < ?1)
                 OR (state = 'processing' AND received_at < ?1)",
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
    fn delivery_retention_prunes_completed_and_abandoned_processing_rows() {
        let store = ProductStore::memory().unwrap();
        store
            .begin_delivery_at("completed", "pull_request", None, "abc", 1_000)
            .unwrap();
        store
            .finish_delivery_at("completed", "planned", &json!({"ok": true}), 1_000)
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
            2
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
}
