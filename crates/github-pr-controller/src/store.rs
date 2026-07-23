use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
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
             );",
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
        self.prune_completed_deliveries_at(now_unix(), COMPLETED_RETENTION_SECS)
    }

    fn prune_completed_deliveries_at(
        &self,
        now: i64,
        retention_secs: i64,
    ) -> rusqlite::Result<usize> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        connection.execute(
            "DELETE FROM webhook_deliveries
              WHERE state IN ('planned', 'ignored')
                AND completed_at < ?1",
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
    fn completed_delivery_retention_is_bounded_without_pruning_processing() {
        let store = ProductStore::memory().unwrap();
        store
            .begin_delivery_at("completed", "pull_request", None, "abc", 1_000)
            .unwrap();
        store
            .finish_delivery_at("completed", "planned", &json!({"ok": true}), 1_000)
            .unwrap();
        store
            .begin_delivery_at("processing", "pull_request", None, "def", 1_000)
            .unwrap();

        assert_eq!(
            store
                .prune_completed_deliveries_at(
                    1_000 + COMPLETED_RETENTION_SECS + 1,
                    COMPLETED_RETENTION_SECS,
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .begin_delivery_at("processing", "pull_request", None, "def", 2_000_000)
                .unwrap(),
            DeliveryAdmission::New,
            "processing rows are reclaimed, never pruned"
        );
    }
}
