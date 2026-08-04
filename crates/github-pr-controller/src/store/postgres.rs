//! The Postgres backend (ADR 033): same tables, same semantics, network
//! reachable — the operational walls SQLite hit (exec-and-copy access, volume
//! durability, single-instance coupling) are what this removes.
//!
//! Semantics are the SQLite implementation's, mapped to Postgres idioms:
//!
//! - `BEGIN IMMEDIATE` serialized every read-decide-write; here each such
//!   method takes `pg_advisory_xact_lock(hashtext(key))` on its idempotency
//!   key inside the transaction — same serialization, but per key instead of
//!   per database.
//! - The outbox claim loop becomes `SELECT … FOR UPDATE SKIP LOCKED`, the
//!   canonical Postgres outbox pattern the ADR names.
//! - `PRAGMA user_version` becomes a `schema_migrations` table; the migration
//!   list mirrors the SQLite one step for step so the histories stay aligned.

use super::{
    now_unix, CanarySummary, DeliveryAdmission, PendingWrite, ProductStore, RecordedRound,
    ReviewFinding, ReviewFindingQuery, ReviewFindingRow, ReviewRound, RuntimeEventAdmission, SessionTarget, ShadowAdmission,
    ShadowSummary, StoreError, StoreResult, COMPLETED_RETENTION_SECS, PROCESSING_LEASE_SECS,
    WRITE_CLAIM_LEASE_SECS, WRITE_MAX_ATTEMPTS,
};
use controller_protocol::audit::{
    AuditActor, AuditCorrelation, AuditCursor, AuditError, AuditEvent, AuditEventPage,
    AuditEventQuery, AuditEventRecord, AuditOutcome, AuditTarget,
};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio_postgres::config::SslMode;
use tokio_postgres::NoTls;

/// Verified TLS against the platform's root store. Certificate validation is
/// on by default (council F1, round 1) — `sslmode=disable` in the URL is the
/// only way to skip encryption, and there is deliberately no
/// "encrypt but do not verify" mode.
fn tls_connector() -> StoreResult<tokio_postgres_rustls::MakeRustlsConnect> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for cert in loaded.certs {
        roots
            .add(cert)
            .map_err(|error| StoreError::Pool(format!("root certificate rejected: {error}")))?;
    }
    if roots.is_empty() {
        let detail = loaded
            .errors
            .first()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no certificates found".into());
        return Err(StoreError::Pool(format!(
            "platform root certificate store unavailable: {detail}"
        )));
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// Mirrors the SQLite `MIGRATIONS` list step for step: same four versions,
/// same tables, dialect differences only (BIGSERIAL ids, `$N` params live in
/// the queries, not here). Recorded in `schema_migrations` instead of
/// `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[
    // 0 — delivery-side tables (pre-existing)
    "CREATE TABLE IF NOT EXISTS webhook_deliveries (
       delivery_id TEXT PRIMARY KEY,
       event_type TEXT NOT NULL,
       repository TEXT,
       payload_sha256 TEXT NOT NULL,
       state TEXT NOT NULL,
       result_json TEXT,
       received_at BIGINT NOT NULL,
       completed_at BIGINT
     );
     CREATE TABLE IF NOT EXISTS shadow_comparisons (
       comparison_id TEXT PRIMARY KEY,
       request_sha256 TEXT NOT NULL,
       repository TEXT,
       exact_match BIGINT NOT NULL,
       identity_mismatches BIGINT NOT NULL,
       presentation_mismatches BIGINT NOT NULL,
       created_at BIGINT NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_shadow_comparisons_created
       ON shadow_comparisons(created_at);
     CREATE TABLE IF NOT EXISTS runtime_event_receipts (
       event_id TEXT PRIMARY KEY,
       body_sha256 TEXT NOT NULL,
       event_type TEXT NOT NULL,
       session_id TEXT,
       occurred_at BIGINT NOT NULL,
       received_at BIGINT NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_runtime_event_receipts_received
       ON runtime_event_receipts(received_at);",
    // 1 — product tables for the closing half
    "CREATE TABLE IF NOT EXISTS review_rounds (
       id BIGSERIAL PRIMARY KEY,
       repo TEXT NOT NULL,
       pr_number BIGINT NOT NULL,
       round BIGINT NOT NULL,
       session_id TEXT NOT NULL UNIQUE,
       head_sha TEXT,
       comment_id BIGINT,
       decision TEXT NOT NULL,
       red BIGINT NOT NULL,
       yellow BIGINT NOT NULL,
       green BIGINT NOT NULL,
       created_at BIGINT NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_review_rounds_pr
       ON review_rounds(repo, pr_number, round);
     CREATE TABLE IF NOT EXISTS review_findings (
       id BIGSERIAL PRIMARY KEY,
       session_id TEXT NOT NULL,
       repo TEXT, pr_number BIGINT,
       stable_id TEXT NOT NULL,
       severity TEXT NOT NULL,
       status TEXT NOT NULL,
       title TEXT NOT NULL,
       path TEXT, line BIGINT,
       raised_by TEXT, angle TEXT,
       head_sha TEXT,
       created_at BIGINT NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_review_findings_pr
       ON review_findings(repo, pr_number, id);
     CREATE INDEX IF NOT EXISTS idx_review_findings_session
       ON review_findings(session_id);
     CREATE TABLE IF NOT EXISTS github_writes (
       id BIGSERIAL PRIMARY KEY,
       session_id TEXT NOT NULL,
       kind TEXT NOT NULL,
       payload_json TEXT NOT NULL,
       state TEXT NOT NULL,
       attempts BIGINT NOT NULL,
       last_error TEXT,
       created_at BIGINT NOT NULL,
       done_at BIGINT,
       UNIQUE(session_id, kind)
     );
     CREATE INDEX IF NOT EXISTS idx_github_writes_pending
       ON github_writes(state, id);",
    // 2 — session targets
    "CREATE TABLE IF NOT EXISTS session_targets (
       session_id TEXT PRIMARY KEY,
       repo TEXT NOT NULL,
       pr_number BIGINT NOT NULL,
       head_sha TEXT,
       created_at BIGINT NOT NULL
     );",
    // 3 — when a drain took ownership of a write
    "ALTER TABLE github_writes ADD COLUMN IF NOT EXISTS claimed_at BIGINT;",
    // 4 — ADR 036 first-party investigation journal.
    r#"
CREATE TABLE IF NOT EXISTS audit_events (
    seq BIGSERIAL PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    event_key TEXT NOT NULL,
    version BIGINT NOT NULL,
    occurred_at BIGINT NOT NULL,
    recorded_at BIGINT NOT NULL,
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
CREATE INDEX IF NOT EXISTS idx_audit_events_recorded ON audit_events(recorded_at, seq);
"#,
];

/// Serializes concurrent boots racing the migration list; any constant that
/// is ours alone works.
const MIGRATION_LOCK_KEY: i64 = 0x0CB_0333;

pub struct PostgresStore {
    pool: Pool,
}

impl PostgresStore {
    pub async fn open(url: &str) -> StoreResult<Self> {
        Self::open_with_options(url, None).await
    }

    /// `search_path` carves out a schema — the tests' isolation mechanism,
    /// and available to ops if the lane's instance is shared.
    pub(crate) async fn open_with_options(
        url: &str,
        search_path: Option<&str>,
    ) -> StoreResult<Self> {
        let mut config: tokio_postgres::Config = url.parse()?;
        if let Some(schema) = search_path {
            config.options(format!("-c search_path={schema}"));
        }
        // TLS with certificate verification against the platform trust store
        // is the default; the URL carries a password and ADR 033's whole point
        // is a network-reachable database. `sslmode=disable` is the explicit
        // opt-out for same-project / local-Docker plaintext. The default
        // (`prefer`) negotiates: TLS where the server offers it, plaintext
        // where it does not — so a lane-internal instance without TLS still
        // works without any URL surgery.
        let manager_config = ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        };
        let manager = if matches!(config.get_ssl_mode(), SslMode::Disable) {
            Manager::from_config(config, NoTls, manager_config)
        } else {
            Manager::from_config(config, tls_connector()?, manager_config)
        };
        let pool = Pool::builder(manager)
            .max_size(8)
            .build()
            .map_err(|error| StoreError::Pool(error.to_string()))?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn client(&self) -> StoreResult<deadpool_postgres::Client> {
        self.pool
            .get()
            .await
            .map_err(|error| StoreError::Pool(error.to_string()))
    }

    async fn migrate(&self) -> StoreResult<()> {
        let mut client = self.client().await?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                   version BIGINT PRIMARY KEY,
                   applied_at BIGINT NOT NULL
                 );",
            )
            .await?;
        let transaction = client.transaction().await?;
        transaction
            .query("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK_KEY])
            .await?;
        let applied: i64 = transaction
            .query_one("SELECT COUNT(*) FROM schema_migrations", &[])
            .await?
            .get(0);
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied.max(0) as usize) {
            transaction.batch_execute(sql).await?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)",
                    &[&((index + 1) as i64), &now_unix()],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn begin_delivery_at(
        &self,
        delivery_id: &str,
        event_type: &str,
        repository: Option<&str>,
        payload_sha256: &str,
        now: i64,
    ) -> StoreResult<DeliveryAdmission> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction
            .query(
                "SELECT pg_advisory_xact_lock(hashtext($1))",
                &[&delivery_id],
            )
            .await?;
        let existing = transaction
            .query_opt(
                "SELECT payload_sha256, state, result_json, received_at
                   FROM webhook_deliveries WHERE delivery_id = $1",
                &[&delivery_id],
            )
            .await?
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    row.get::<_, String>(1),
                    row.get::<_, Option<String>>(2),
                    row.get::<_, i64>(3),
                )
            });

        let admission = match existing {
            Some((existing_hash, _, _, _)) if existing_hash != payload_sha256 => {
                DeliveryAdmission::Conflict
            }
            Some((_, state, _, _)) if state == "retryable" => {
                transaction
                    .execute(
                        "UPDATE webhook_deliveries
                            SET event_type = $2, repository = $3, state = 'processing',
                                result_json = NULL, received_at = $4, completed_at = NULL
                          WHERE delivery_id = $1",
                        &[&delivery_id, &event_type, &repository, &now],
                    )
                    .await?;
                DeliveryAdmission::New
            }
            Some((_, state, _, received_at))
                if state == "processing"
                    && received_at <= now.saturating_sub(PROCESSING_LEASE_SECS) =>
            {
                transaction
                    .execute(
                        "UPDATE webhook_deliveries
                            SET event_type = $2, repository = $3, received_at = $4
                          WHERE delivery_id = $1",
                        &[&delivery_id, &event_type, &repository, &now],
                    )
                    .await?;
                DeliveryAdmission::New
            }
            Some((_, state, result_json, _)) => DeliveryAdmission::Duplicate {
                state,
                result: result_json.and_then(|value| serde_json::from_str(&value).ok()),
            },
            None => {
                transaction
                    .execute(
                        "INSERT INTO webhook_deliveries
                           (delivery_id, event_type, repository, payload_sha256, state, received_at)
                         VALUES ($1, $2, $3, $4, 'processing', $5)",
                        &[
                            &delivery_id,
                            &event_type,
                            &repository,
                            &payload_sha256,
                            &now,
                        ],
                    )
                    .await?;
                DeliveryAdmission::New
            }
        };
        transaction.commit().await?;
        Ok(admission)
    }

    async fn finish_delivery_at(
        &self,
        delivery_id: &str,
        state: &str,
        result: &Value,
        now: i64,
    ) -> StoreResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE webhook_deliveries
                    SET state = $2, result_json = $3, completed_at = $4
                  WHERE delivery_id = $1",
                &[&delivery_id, &state, &result.to_string(), &now],
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn prune_at(&self, now: i64, retention_secs: i64) -> StoreResult<usize> {
        let client = self.client().await?;
        let cutoff = now.saturating_sub(retention_secs);
        let deliveries = client
            .execute(
                "DELETE FROM webhook_deliveries
                  WHERE (state IN ('planned', 'ignored', 'acted') AND completed_at < $1)
                     OR (state IN ('processing', 'retryable') AND received_at < $1)",
                &[&cutoff],
            )
            .await?;
        let comparisons = client
            .execute(
                "DELETE FROM shadow_comparisons WHERE created_at < $1",
                &[&cutoff],
            )
            .await?;
        let events = client
            .execute(
                "DELETE FROM runtime_event_receipts WHERE received_at < $1",
                &[&cutoff],
            )
            .await?;
        Ok((deliveries + comparisons + events) as usize)
    }

    pub(crate) async fn claim_writes_at(
        &self,
        limit: i64,
        now: i64,
    ) -> StoreResult<Vec<PendingWrite>> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        // FOR UPDATE SKIP LOCKED: two concurrent drains partition the queue
        // instead of colliding — the claim contention the SQLite IMMEDIATE
        // loop serialized is gone outright (ADR 033).
        let rows = transaction
            .query(
                "SELECT id, session_id, kind, payload_json, attempts, state
                   FROM github_writes
                  WHERE state = 'pending'
                     OR (state = 'in_flight' AND claimed_at <= $2)
                  ORDER BY id LIMIT $1
                    FOR UPDATE SKIP LOCKED",
                &[&limit, &now.saturating_sub(WRITE_CLAIM_LEASE_SECS)],
            )
            .await?;
        let claimed: Vec<PendingWrite> = rows
            .iter()
            .map(|row| PendingWrite {
                id: row.get(0),
                session_id: row.get(1),
                kind: row.get(2),
                payload: serde_json::from_str(&row.get::<_, String>(3)).unwrap_or(Value::Null),
                attempts: row.get(4),
                was_reclaimed: row.get::<_, String>(5) == "in_flight",
            })
            .collect();
        for write in &claimed {
            transaction
                .execute(
                    "UPDATE github_writes SET state = 'in_flight', claimed_at = $2 WHERE id = $1",
                    &[&write.id, &now],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(claimed)
    }
}

async fn append_audit_event_pg_locked<G: tokio_postgres::GenericClient>(
    g: &G,
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
    let actor_kind = event.actor.as_ref().map(|value| value.kind.clone());
    let actor_id = event.actor.as_ref().and_then(|value| value.id.clone());
    let actor_display = event.actor.as_ref().and_then(|value| value.display.clone());
    let actor_association = event
        .actor
        .as_ref()
        .and_then(|value| value.association.clone());
    let target_kind = event.target.as_ref().map(|value| value.kind.clone());
    let target_ref = event
        .target
        .as_ref()
        .and_then(|value| value.reference.clone());
    let target_revision = event
        .target
        .as_ref()
        .and_then(|value| value.revision.clone());
    let version = i64::from(event.version);
    let outcome = event.outcome.as_str().to_string();
    g.execute(
        "INSERT INTO audit_events
            (event_id, event_key, version, occurred_at, recorded_at, service, kind, outcome,
             caused_by, delivery_id, controller_id, action_id, scope, trigger_ref,
             trigger_fingerprint, session_id, message_id, runtime_event_id, write_id,
             actor_kind, actor_id, actor_display, actor_association, target_kind, target_ref,
             target_revision, detail_json, error_json)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                 $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28)
         ON CONFLICT (service, event_key) DO NOTHING",
        &[
            &event.event_id,
            &event.event_key,
            &version,
            &event.occurred_at,
            &event.recorded_at,
            &event.service,
            &event.kind,
            &outcome,
            &event.caused_by,
            &event.correlation.delivery_id,
            &event.correlation.controller_id,
            &event.correlation.action_id,
            &event.correlation.scope,
            &event.correlation.trigger_ref,
            &event.correlation.trigger_fingerprint,
            &event.correlation.session_id,
            &event.correlation.message_id,
            &event.correlation.runtime_event_id,
            &event.correlation.write_id,
            &actor_kind,
            &actor_id,
            &actor_display,
            &actor_association,
            &target_kind,
            &target_ref,
            &target_revision,
            &detail_json,
            &error_json,
        ],
    )
    .await?;
    Ok(())
}

#[async_trait::async_trait]
impl ProductStore for PostgresStore {
    async fn begin_delivery(
        &self,
        delivery_id: &str,
        event_type: &str,
        repository: Option<&str>,
        payload_sha256: &str,
    ) -> StoreResult<DeliveryAdmission> {
        self.begin_delivery_at(
            delivery_id,
            event_type,
            repository,
            payload_sha256,
            now_unix(),
        )
        .await
    }

    async fn finish_delivery(
        &self,
        delivery_id: &str,
        state: &str,
        result: &Value,
    ) -> StoreResult<()> {
        self.finish_delivery_at(delivery_id, state, result, now_unix())
            .await
    }

    async fn release_delivery_for_retry(
        &self,
        delivery_id: &str,
        result: &Value,
    ) -> StoreResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE webhook_deliveries
                    SET state = 'retryable', result_json = $2, completed_at = NULL
                  WHERE delivery_id = $1 AND state = 'processing'",
                &[&delivery_id, &result.to_string()],
            )
            .await?;
        Ok(())
    }

    async fn prune_completed_deliveries(&self) -> StoreResult<usize> {
        self.prune_at(now_unix(), COMPLETED_RETENTION_SECS).await
    }

    async fn record_shadow_comparison(
        &self,
        request_sha256: &str,
        repository: Option<&str>,
        report: &crate::shadow::ShadowReport,
    ) -> StoreResult<ShadowAdmission> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction
            .query(
                "SELECT pg_advisory_xact_lock(hashtext($1))",
                &[&report.comparison_id],
            )
            .await?;
        let existing = transaction
            .query_opt(
                "SELECT request_sha256 FROM shadow_comparisons WHERE comparison_id = $1",
                &[&report.comparison_id],
            )
            .await?
            .map(|row| row.get::<_, String>(0));
        let admission = match existing {
            Some(existing) if existing == request_sha256 => ShadowAdmission::Duplicate,
            Some(_) => ShadowAdmission::Conflict,
            None => {
                transaction
                    .execute(
                        "INSERT INTO shadow_comparisons
                           (comparison_id, request_sha256, repository, exact_match,
                            identity_mismatches, presentation_mismatches, created_at)
                         VALUES ($1, $2, $3, $4, $5, $6, $7)",
                        &[
                            &report.comparison_id,
                            &request_sha256,
                            &repository,
                            &(report.exact_match as i64),
                            &(report.identity_or_ownership_mismatches as i64),
                            &(report.presentation_mismatches as i64),
                            &now_unix(),
                        ],
                    )
                    .await?;
                ShadowAdmission::New
            }
        };
        transaction.commit().await?;
        Ok(admission)
    }

    async fn shadow_summary(&self) -> StoreResult<ShadowSummary> {
        let client = self.client().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN exact_match = 1 THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN identity_mismatches > 0 THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN presentation_mismatches > 0 THEN 1 ELSE 0 END), 0)
                   FROM shadow_comparisons",
                &[],
            )
            .await?;
        Ok(ShadowSummary {
            total: row.get(0),
            exact_matches: row.get(1),
            identity_or_ownership_mismatch_reports: row.get(2),
            presentation_mismatch_reports: row.get(3),
        })
    }

    async fn record_runtime_event(
        &self,
        body_sha256: &str,
        event: &crate::runtime_events::RuntimeEventEnvelope,
    ) -> StoreResult<RuntimeEventAdmission> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction
            .query(
                "SELECT pg_advisory_xact_lock(hashtext($1))",
                &[&event.event_id],
            )
            .await?;
        let existing = transaction
            .query_opt(
                "SELECT body_sha256 FROM runtime_event_receipts WHERE event_id = $1",
                &[&event.event_id],
            )
            .await?
            .map(|row| row.get::<_, String>(0));
        let admission = match existing {
            Some(existing) if existing == body_sha256 => RuntimeEventAdmission::Duplicate,
            Some(_) => RuntimeEventAdmission::Conflict,
            None => {
                transaction
                    .execute(
                        "INSERT INTO runtime_event_receipts
                           (event_id, body_sha256, event_type, session_id, occurred_at, received_at)
                         VALUES ($1, $2, $3, $4, $5, $6)",
                        &[
                            &event.event_id,
                            &body_sha256,
                            &event.event_type,
                            &event.session_id,
                            &event.occurred_at,
                            &now_unix(),
                        ],
                    )
                    .await?;
                RuntimeEventAdmission::New
            }
        };
        transaction.commit().await?;
        Ok(admission)
    }

    async fn canary_summary(&self) -> StoreResult<CanarySummary> {
        let client = self.client().await?;
        let deliveries = client
            .query_one(
                "SELECT
                   COALESCE(SUM(CASE WHEN state = 'acted' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN state = 'processing' THEN 1 ELSE 0 END), 0),
                   COALESCE(SUM(CASE WHEN state = 'retryable' THEN 1 ELSE 0 END), 0)
                 FROM webhook_deliveries",
                &[],
            )
            .await?;
        let events = client
            .query_one(
                "SELECT COUNT(*), MAX(occurred_at) FROM runtime_event_receipts",
                &[],
            )
            .await?;
        let types = client
            .query(
                "SELECT event_type, COUNT(*) FROM runtime_event_receipts
                 GROUP BY event_type ORDER BY event_type",
                &[],
            )
            .await?;
        Ok(CanarySummary {
            acted_deliveries: deliveries.get(0),
            processing_deliveries: deliveries.get(1),
            retryable_deliveries: deliveries.get(2),
            runtime_events: events.get(0),
            runtime_event_types: types
                .iter()
                .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)))
                .collect::<BTreeMap<_, _>>(),
            latest_event_occurred_at: events.get(1),
        })
    }

    async fn next_round(&self, repo: &str, pr_number: i64) -> StoreResult<i64> {
        let client = self.client().await?;
        let row = client
            .query_one(
                "SELECT COALESCE(MAX(round), 0) + 1 FROM review_rounds
                  WHERE repo = $1 AND pr_number = $2",
                &[&repo, &pr_number],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn record_session_target(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
    ) -> StoreResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO session_targets
                   (session_id, repo, pr_number, head_sha, created_at)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (session_id) DO UPDATE SET
                   repo = excluded.repo,
                   pr_number = excluded.pr_number,
                   head_sha = COALESCE(excluded.head_sha, session_targets.head_sha)",
                &[&session_id, &repo, &pr_number, &head_sha, &now_unix()],
            )
            .await?;
        Ok(())
    }

    async fn session_target(&self, session_id: &str) -> StoreResult<Option<SessionTarget>> {
        let client = self.client().await?;
        Ok(client
            .query_opt(
                "SELECT repo, pr_number, head_sha FROM session_targets WHERE session_id = $1",
                &[&session_id],
            )
            .await?
            .map(|row| SessionTarget {
                repo: row.get(0),
                pr_number: row.get(1),
                head_sha: row.get(2),
            }))
    }

    async fn record_review_round(&self, round: &ReviewRound) -> StoreResult<RecordedRound> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        // Round numbering is MAX+1 per pull request: serialize on the PR, not
        // the session, so two sessions closing against the same PR cannot
        // compute the same round number.
        let pr_key = format!("{}#{}", round.repo, round.pr_number);
        transaction
            .query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&pr_key])
            .await?;
        let existing = transaction
            .query_opt(
                "SELECT id, round FROM review_rounds WHERE session_id = $1",
                &[&round.session_id],
            )
            .await?
            .map(|row| (row.get::<_, i64>(0), row.get::<_, i64>(1)));
        let recorded = match existing {
            Some((id, number)) => RecordedRound {
                id,
                round: number,
                first_time: false,
            },
            None => {
                let number: i64 = transaction
                    .query_one(
                        "SELECT COALESCE(MAX(round), 0) + 1 FROM review_rounds
                          WHERE repo = $1 AND pr_number = $2",
                        &[&round.repo, &round.pr_number],
                    )
                    .await?
                    .get(0);
                let id: i64 = transaction
                    .query_one(
                        "INSERT INTO review_rounds
                           (repo, pr_number, round, session_id, head_sha, decision,
                            red, yellow, green, created_at)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                         RETURNING id",
                        &[
                            &round.repo,
                            &round.pr_number,
                            &number,
                            &round.session_id,
                            &round.head_sha,
                            &round.decision,
                            &round.red,
                            &round.yellow,
                            &round.green,
                            &now_unix(),
                        ],
                    )
                    .await?
                    .get(0);
                RecordedRound {
                    id,
                    round: number,
                    first_time: true,
                }
            }
        };
        transaction.commit().await?;
        Ok(recorded)
    }

    async fn last_comment_id(&self, repo: &str, pr_number: i64) -> StoreResult<Option<i64>> {
        let client = self.client().await?;
        Ok(client
            .query_opt(
                "SELECT comment_id FROM review_rounds
                  WHERE repo = $1 AND pr_number = $2 AND comment_id IS NOT NULL
                  ORDER BY round DESC LIMIT 1",
                &[&repo, &pr_number],
            )
            .await?
            .map(|row| row.get(0)))
    }

    async fn round_comment_id(&self, session_id: &str) -> StoreResult<Option<i64>> {
        let client = self.client().await?;
        Ok(client
            .query_opt(
                "SELECT comment_id FROM review_rounds
                  WHERE session_id = $1 AND comment_id IS NOT NULL",
                &[&session_id],
            )
            .await?
            .map(|row| row.get(0)))
    }

    async fn set_round_comment_id(&self, session_id: &str, comment_id: i64) -> StoreResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE review_rounds SET comment_id = $2 WHERE session_id = $1",
                &[&session_id, &comment_id],
            )
            .await?;
        Ok(())
    }

    /// Same predicate shape as SQLite: NULL filters do not constrain. Postgres
    /// needs the explicit casts because a NULL parameter is otherwise of
    /// unknown type at plan time.
    async fn review_findings(
        &self,
        query: &ReviewFindingQuery,
    ) -> StoreResult<Vec<ReviewFindingRow>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT id, session_id, repo, pr_number, stable_id, severity, status,
                        title, path, line, raised_by, angle, head_sha, created_at
                   FROM review_findings
                  WHERE ($1::TEXT IS NULL OR repo = $1)
                    AND ($2::BIGINT IS NULL OR pr_number = $2)
                    AND ($3::TEXT IS NULL OR status = $3)
                    AND ($4::TEXT IS NULL OR severity = $4)
                  ORDER BY id DESC
                  LIMIT $5",
                &[
                    &query.repo,
                    &query.pr_number,
                    &query.status,
                    &query.severity,
                    &(query.limit as i64),
                ],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| ReviewFindingRow {
                id: row.get(0),
                session_id: row.get(1),
                repo: row.get(2),
                pr_number: row.get(3),
                stable_id: row.get(4),
                severity: row.get(5),
                status: row.get(6),
                title: row.get(7),
                path: row.get(8),
                line: row.get(9),
                raised_by: row.get(10),
                angle: row.get(11),
                head_sha: row.get(12),
                created_at: row.get(13),
            })
            .collect())
    }

    async fn record_review_findings(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
        findings: &[ReviewFinding],
    ) -> StoreResult<usize> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction
            .query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&session_id])
            .await?;
        let already: i64 = transaction
            .query_one(
                "SELECT COUNT(*) FROM review_findings WHERE session_id = $1",
                &[&session_id],
            )
            .await?
            .get(0);
        if already > 0 {
            // Nothing was written; roll back explicitly so the advisory lock
            // and the transaction end now, not when the connection recycles.
            transaction.rollback().await?;
            return Ok(0);
        }
        let now = now_unix();
        for finding in findings {
            transaction
                .execute(
                    "INSERT INTO review_findings
                       (session_id, repo, pr_number, stable_id, severity, status,
                        title, path, line, raised_by, angle, head_sha, created_at)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                    &[
                        &session_id,
                        &repo,
                        &pr_number,
                        &finding.stable_id,
                        &finding.severity,
                        &finding.status,
                        &finding.title,
                        &finding.path,
                        &finding.line,
                        &finding.raised_by,
                        &finding.angle,
                        &head_sha,
                        &now,
                    ],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(findings.len())
    }

    async fn enqueue_write(
        &self,
        session_id: &str,
        kind: &str,
        payload: &Value,
    ) -> StoreResult<bool> {
        let now = now_unix();
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        let inserted = transaction
            .execute(
                "INSERT INTO github_writes
                   (session_id, kind, payload_json, state, attempts, created_at)
                 VALUES ($1, $2, $3, 'pending', 0, $4)
                 ON CONFLICT (session_id, kind) DO NOTHING",
                &[&session_id, &kind, &payload.to_string(), &now],
            )
            .await?;
        let write_id: i64 = transaction
            .query_one(
                "SELECT id FROM github_writes WHERE session_id = $1 AND kind = $2",
                &[&session_id, &kind],
            )
            .await?
            .get(0);
        if inserted == 1 {
            let event = super::new_audit_event(
                format!("github.write.enqueued:{write_id}"),
                "github.write.enqueued",
                super::AuditOutcome::Pending,
                now,
                super::AuditCorrelation {
                    session_id: Some(session_id.into()),
                    write_id: Some(write_id.to_string()),
                    ..Default::default()
                },
                serde_json::json!({"operation": kind}),
                None,
            );
            append_audit_event_pg_locked(&*transaction, &event).await?;
        }
        transaction.commit().await?;
        Ok(inserted == 1)
    }

    async fn claim_writes(&self, limit: i64) -> StoreResult<Vec<PendingWrite>> {
        self.claim_writes_at(limit, now_unix()).await
    }

    async fn claim_writes_for_test_after_lease(
        &self,
        limit: i64,
    ) -> StoreResult<Vec<PendingWrite>> {
        self.claim_writes_at(limit, now_unix() + WRITE_CLAIM_LEASE_SECS + 1)
            .await
    }

    async fn pending_writes(&self, limit: i64) -> StoreResult<Vec<PendingWrite>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT id, session_id, kind, payload_json, attempts
                   FROM github_writes WHERE state IN ('pending', 'in_flight')
                   ORDER BY id LIMIT $1",
                &[&limit],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| PendingWrite {
                id: row.get(0),
                session_id: row.get(1),
                kind: row.get(2),
                payload: serde_json::from_str(&row.get::<_, String>(3)).unwrap_or(Value::Null),
                attempts: row.get(4),
                was_reclaimed: false,
            })
            .collect())
    }

    async fn mark_write_done(&self, id: i64) -> StoreResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE github_writes
                    SET state = 'done', last_error = NULL, claimed_at = NULL, done_at = $2
                  WHERE id = $1",
                &[&id, &now_unix()],
            )
            .await?;
        Ok(())
    }

    async fn mark_write_failed(&self, id: i64, error: &str) -> StoreResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE github_writes
                    SET attempts = attempts + 1,
                        last_error = $2,
                        claimed_at = NULL,
                        state = CASE WHEN attempts + 1 >= $3 THEN 'failed' ELSE 'pending' END
                  WHERE id = $1",
                &[&id, &error, &WRITE_MAX_ATTEMPTS],
            )
            .await?;
        Ok(())
    }

    async fn append_audit_event(&self, event: &AuditEvent) -> StoreResult<AuditEventRecord> {
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
        let actor_kind = event.actor.as_ref().map(|value| value.kind.clone());
        let actor_id = event.actor.as_ref().and_then(|value| value.id.clone());
        let actor_display = event.actor.as_ref().and_then(|value| value.display.clone());
        let actor_association = event
            .actor
            .as_ref()
            .and_then(|value| value.association.clone());
        let target_kind = event.target.as_ref().map(|value| value.kind.clone());
        let target_ref = event
            .target
            .as_ref()
            .and_then(|value| value.reference.clone());
        let target_revision = event
            .target
            .as_ref()
            .and_then(|value| value.revision.clone());
        let outcome = event.outcome.as_str().to_string();
        let version = i64::from(event.version);
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        let row = transaction
            .query_opt(
                "INSERT INTO audit_events
                    (event_id, event_key, version, occurred_at, recorded_at, service, kind,
                     outcome, caused_by, delivery_id, controller_id, action_id, scope, trigger_ref,
                     trigger_fingerprint, session_id, message_id, runtime_event_id, write_id,
                     actor_kind, actor_id, actor_display, actor_association, target_kind, target_ref,
                     target_revision, detail_json, error_json)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                         $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28)
                 ON CONFLICT (service, event_key) DO NOTHING
                 RETURNING seq, event_id, event_key, version, occurred_at, recorded_at, service,
                           kind, outcome, caused_by, delivery_id, controller_id, action_id,
                           scope, trigger_ref, trigger_fingerprint, session_id, message_id,
                           runtime_event_id, write_id, actor_kind, actor_id, actor_display,
                           actor_association, target_kind, target_ref, target_revision,
                           detail_json, error_json",
                &[
                    &event.event_id,
                    &event.event_key,
                    &version,
                    &event.occurred_at,
                    &event.recorded_at,
                    &event.service,
                    &event.kind,
                    &outcome,
                    &event.caused_by,
                    &event.correlation.delivery_id,
                    &event.correlation.controller_id,
                    &event.correlation.action_id,
                    &event.correlation.scope,
                    &event.correlation.trigger_ref,
                    &event.correlation.trigger_fingerprint,
                    &event.correlation.session_id,
                    &event.correlation.message_id,
                    &event.correlation.runtime_event_id,
                    &event.correlation.write_id,
                    &actor_kind,
                    &actor_id,
                    &actor_display,
                    &actor_association,
                    &target_kind,
                    &target_ref,
                    &target_revision,
                    &detail_json,
                    &error_json,
                ],
            )
            .await?;
        let row = match row {
            Some(row) => row,
            None => transaction
                .query_one(
                    "SELECT seq, event_id, event_key, version, occurred_at, recorded_at, service,
                            kind, outcome, caused_by, delivery_id, controller_id, action_id, scope,
                            trigger_ref, trigger_fingerprint, session_id, message_id,
                            runtime_event_id, write_id, actor_kind, actor_id, actor_display,
                            actor_association, target_kind, target_ref, target_revision,
                            detail_json, error_json
                     FROM audit_events WHERE service = $1 AND event_key = $2",
                    &[&event.service, &event.event_key],
                )
                .await?,
        };
        let record = audit_event_from_pg_row(&row)?;
        transaction.commit().await?;
        Ok(record)
    }

    async fn audit_events(&self, query: &AuditEventQuery) -> StoreResult<AuditEventPage> {
        let limit = query.bounded_limit();
        let cursor_recorded_at = query.cursor.map(|cursor| cursor.recorded_at);
        let cursor_seq = query.cursor.map(|cursor| cursor.seq);
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT seq, event_id, event_key, version, occurred_at, recorded_at, service,
                        kind, outcome, caused_by, delivery_id, controller_id, action_id, scope,
                        trigger_ref, trigger_fingerprint, session_id, message_id,
                        runtime_event_id, write_id, actor_kind, actor_id, actor_display,
                        actor_association, target_kind, target_ref, target_revision,
                        detail_json, error_json
                 FROM audit_events
                 WHERE ($1::text IS NULL OR delivery_id = $1)
                   AND ($2::text IS NULL OR controller_id = $2)
                   AND ($3::text IS NULL OR action_id = $3)
                   AND ($4::text IS NULL OR runtime_event_id = $4)
                   AND ($5::text IS NULL OR session_id = $5)
                   AND ($6::text IS NULL OR message_id = $6)
                   AND ($7::text IS NULL OR write_id = $7)
                   AND ($8::text IS NULL OR trigger_ref = $8)
                   AND ($9::text IS NULL OR kind = $9)
                   AND ($10::bigint IS NULL OR recorded_at >= $10)
                   AND ($11::bigint IS NULL OR recorded_at <= $11)
                   AND ($12::bigint IS NULL OR recorded_at < $12
                        OR (recorded_at = $12 AND seq < $13))
                 ORDER BY recorded_at DESC, seq DESC
                 LIMIT $14",
                &[
                    &query.delivery_id,
                    &query.controller_id,
                    &query.action_id,
                    &query.runtime_event_id,
                    &query.session_id,
                    &query.message_id,
                    &query.write_id,
                    &query.trigger_ref,
                    &query.kind,
                    &query.since,
                    &query.until,
                    &cursor_recorded_at,
                    &cursor_seq,
                    &((limit + 1) as i64),
                ],
            )
            .await?;
        let mut events = rows
            .iter()
            .map(audit_event_from_pg_row)
            .collect::<StoreResult<Vec<_>>>()?;
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

    async fn prune_audit_events(&self, before: i64, extended_before: i64) -> StoreResult<usize> {
        let client = self.client().await?;
        Ok(client
            .execute(
                "DELETE FROM audit_events
                 WHERE (recorded_at < $1 AND NOT (
                           outcome IN ('failed', 'outcome_unknown', 'reconciled')
                           OR kind LIKE 'security.%'
                           OR kind LIKE 'config.%'
                           OR kind LIKE 'operator.%'
                           OR kind LIKE '%dead_lettered'
                           OR kind = 'audit.retention_pruned'
                       ))
                    OR (recorded_at < $2 AND (
                           outcome IN ('failed', 'outcome_unknown', 'reconciled')
                           OR kind LIKE 'security.%'
                           OR kind LIKE 'config.%'
                           OR kind LIKE 'operator.%'
                           OR kind LIKE '%dead_lettered'
                           OR kind = 'audit.retention_pruned'
                       ))",
                &[&before, &extended_before.min(before)],
            )
            .await? as usize)
    }
}

/// Every test needs a reachable Postgres and skips (loudly) without one:
/// `TEST_POSTGRES_URL=postgres://user:pass@localhost:5432/postgres`.
/// Each test owns a throwaway schema, dropped and recreated on entry, so
/// reruns are clean and a failed run leaves its state behind for inspection.
fn audit_event_from_pg_row(row: &tokio_postgres::Row) -> StoreResult<AuditEventRecord> {
    let outcome_raw: String = row.get(8);
    let outcome = AuditOutcome::parse(&outcome_raw)
        .ok_or_else(|| StoreError::Pool(format!("unknown audit outcome {outcome_raw}")))?;
    let actor = row.get::<_, Option<String>>(20).map(|kind| AuditActor {
        kind,
        id: row.get(21),
        display: row.get(22),
        association: row.get(23),
    });
    let target = row.get::<_, Option<String>>(24).map(|kind| AuditTarget {
        kind,
        reference: row.get(25),
        revision: row.get(26),
    });
    let detail_raw: String = row.get(27);
    let detail = serde_json::from_str(&detail_raw)
        .map_err(|error| StoreError::Pool(format!("parse audit detail: {error}")))?;
    let error = row
        .get::<_, Option<String>>(28)
        .map(|raw| {
            serde_json::from_str::<AuditError>(&raw)
                .map_err(|error| StoreError::Pool(format!("parse audit error: {error}")))
        })
        .transpose()?;
    Ok(AuditEventRecord {
        seq: row.get(0),
        event: AuditEvent {
            version: row.get::<_, i64>(3) as u16,
            event_id: row.get(1),
            event_key: row.get(2),
            occurred_at: row.get(4),
            recorded_at: row.get(5),
            service: row.get(6),
            kind: row.get(7),
            outcome,
            caused_by: row.get(9),
            correlation: AuditCorrelation {
                delivery_id: row.get(10),
                controller_id: row.get(11),
                action_id: row.get(12),
                scope: row.get(13),
                trigger_ref: row.get(14),
                trigger_fingerprint: row.get(15),
                session_id: row.get(16),
                message_id: row.get(17),
                runtime_event_id: row.get(18),
                write_id: row.get(19),
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

    async fn store(tag: &str) -> Option<PostgresStore> {
        let Ok(url) = std::env::var("TEST_POSTGRES_URL") else {
            eprintln!("TEST_POSTGRES_URL not set; skipping postgres store test");
            return None;
        };
        let schema = format!("ocp_test_{tag}");
        let (client, connection) = tokio_postgres::connect(&url, NoTls)
            .await
            .expect("connect to TEST_POSTGRES_URL");
        tokio::spawn(connection);
        client
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema};"
            ))
            .await
            .expect("reset test schema");
        Some(
            PostgresStore::open_with_options(&url, Some(&schema))
                .await
                .expect("open postgres store"),
        )
    }

    #[tokio::test]
    async fn delivery_ids_are_durable_idempotency_keys() {
        let Some(store) = store("delivery_idempotency").await else {
            return;
        };
        assert_eq!(
            store
                .begin_delivery_at(
                    "delivery-1",
                    "pull_request",
                    Some("example/repo"),
                    "abc",
                    1_000
                )
                .await
                .unwrap(),
            DeliveryAdmission::New
        );
        ProductStore::finish_delivery(&store, "delivery-1", "planned", &json!({"planned": true}))
            .await
            .unwrap();
        assert_eq!(
            store
                .begin_delivery_at(
                    "delivery-1",
                    "pull_request",
                    Some("example/repo"),
                    "abc",
                    1_100
                )
                .await
                .unwrap(),
            DeliveryAdmission::Duplicate {
                state: "planned".into(),
                result: Some(json!({"planned": true})),
            }
        );
        assert_eq!(
            store
                .begin_delivery_at(
                    "delivery-1",
                    "pull_request",
                    Some("example/repo"),
                    "other",
                    1_200
                )
                .await
                .unwrap(),
            DeliveryAdmission::Conflict
        );
    }

    #[tokio::test]
    async fn processing_delivery_is_readmitted_only_after_its_lease_lapses() {
        let Some(store) = store("delivery_lease").await else {
            return;
        };
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_000)
                .await
                .unwrap(),
            DeliveryAdmission::New
        );
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_100)
                .await
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
                    None,
                    "abc",
                    1_000 + PROCESSING_LEASE_SECS + 1
                )
                .await
                .unwrap(),
            DeliveryAdmission::New
        );
    }

    #[tokio::test]
    async fn retryable_delivery_is_immediately_readmitted_with_the_same_body() {
        let Some(store) = store("delivery_retryable").await else {
            return;
        };
        store
            .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_000)
            .await
            .unwrap();
        ProductStore::release_delivery_for_retry(&store, "delivery-1", &json!({"ok": false}))
            .await
            .unwrap();
        assert_eq!(
            store
                .begin_delivery_at("delivery-1", "pull_request", None, "abc", 1_001)
                .await
                .unwrap(),
            DeliveryAdmission::New
        );
    }

    #[tokio::test]
    async fn retention_prunes_completed_and_abandoned_rows() {
        let Some(store) = store("retention").await else {
            return;
        };
        store
            .begin_delivery_at("old-acted", "pull_request", None, "abc", 1_000)
            .await
            .unwrap();
        store
            .finish_delivery_at("old-acted", "acted", &json!({"ok": true}), 1_001)
            .await
            .unwrap();
        store
            .begin_delivery_at("old-processing", "pull_request", None, "abc", 1_000)
            .await
            .unwrap();
        store
            .begin_delivery_at("fresh", "pull_request", None, "abc", 10_000)
            .await
            .unwrap();
        // Cutoff lands at 5_000: both 1_000-era rows expire, `fresh` survives.
        let pruned = store
            .prune_at(COMPLETED_RETENTION_SECS + 5_000, COMPLETED_RETENTION_SECS)
            .await
            .unwrap();
        assert_eq!(pruned, 2);
        assert_eq!(
            store
                .begin_delivery_at("old-acted", "pull_request", None, "other", 20_000)
                .await
                .unwrap(),
            DeliveryAdmission::New,
            "a pruned delivery id is admitted fresh even with a new body"
        );
    }

    #[tokio::test]
    async fn shadow_summary_counts_and_dedupes() {
        let Some(store) = store("shadow").await else {
            return;
        };
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
        mismatch.identity_or_ownership_mismatches = 1;
        store
            .record_shadow_comparison("hash-1", Some("example/repo"), &exact)
            .await
            .unwrap();
        store
            .record_shadow_comparison("hash-2", Some("example/repo"), &mismatch)
            .await
            .unwrap();
        assert_eq!(
            store.shadow_summary().await.unwrap(),
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
                .await
                .unwrap(),
            ShadowAdmission::Duplicate
        );
        assert_eq!(
            store
                .record_shadow_comparison("changed", Some("example/repo"), &exact)
                .await
                .unwrap(),
            ShadowAdmission::Conflict
        );
    }

    #[tokio::test]
    async fn runtime_event_receipts_dedupe_and_expose_canary_state() {
        let Some(store) = store("runtime_events").await else {
            return;
        };
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
            store.record_runtime_event("hash-1", &event).await.unwrap(),
            RuntimeEventAdmission::New
        );
        assert_eq!(
            store.record_runtime_event("hash-1", &event).await.unwrap(),
            RuntimeEventAdmission::Duplicate
        );
        assert_eq!(
            store.record_runtime_event("changed", &event).await.unwrap(),
            RuntimeEventAdmission::Conflict
        );
        assert_eq!(
            store.canary_summary().await.unwrap(),
            CanarySummary {
                acted_deliveries: 0,
                processing_deliveries: 0,
                retryable_deliveries: 0,
                runtime_events: 1,
                runtime_event_types: BTreeMap::from([("session.timeout".into(), 1)]),
                latest_event_occurred_at: Some(1_000),
            }
        );
    }

    #[tokio::test]
    async fn migrations_are_idempotent_across_reopens() {
        let Some(store) = store("migrations").await else {
            return;
        };
        drop(store);
        let url = std::env::var("TEST_POSTGRES_URL").unwrap();
        // Reopen the same schema twice more: the migration list must be a
        // no-op, not a duplicate-table error.
        for _ in 0..2 {
            PostgresStore::open_with_options(&url, Some("ocp_test_migrations"))
                .await
                .expect("reopen against an already-migrated schema");
        }
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

    #[tokio::test]
    async fn rounds_number_per_pull_request_and_redelivery_is_a_no_op() {
        let Some(store) = store("rounds").await else {
            return;
        };
        let first = store
            .record_review_round(&round("ses_1", "approve"))
            .await
            .unwrap();
        assert_eq!(first.round, 1);
        assert!(first.first_time);
        let second = store
            .record_review_round(&round("ses_2", "comment"))
            .await
            .unwrap();
        assert_eq!(second.round, 2);
        assert!(second.first_time);
        let redelivered = store
            .record_review_round(&round("ses_1", "approve"))
            .await
            .unwrap();
        assert_eq!(redelivered.id, first.id);
        assert_eq!(redelivered.round, 1);
        assert!(!redelivered.first_time);
        assert_eq!(store.next_round("example/repo", 7).await.unwrap(), 3);
        assert_eq!(store.next_round("example/repo", 8).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn the_comment_anchor_carries_to_the_next_round() {
        let Some(store) = store("comment_anchor").await else {
            return;
        };
        store
            .record_review_round(&round("ses_1", "approve"))
            .await
            .unwrap();
        assert_eq!(
            store.last_comment_id("example/repo", 7).await.unwrap(),
            None
        );
        store.set_round_comment_id("ses_1", 4242).await.unwrap();
        assert_eq!(
            store.last_comment_id("example/repo", 7).await.unwrap(),
            Some(4242)
        );
        store
            .record_review_round(&round("ses_2", "comment"))
            .await
            .unwrap();
        assert_eq!(
            store.last_comment_id("example/repo", 7).await.unwrap(),
            Some(4242),
            "a newer round without a comment id must not hide the anchor"
        );
    }

    #[tokio::test]
    async fn findings_are_written_once_per_session() {
        let Some(store) = store("findings").await else {
            return;
        };
        let finding = ReviewFinding {
            stable_id: "finding-1".into(),
            severity: "red".into(),
            status: "open".into(),
            title: "SQL injection".into(),
            path: Some("src/db.rs".into()),
            line: Some(42),
            raised_by: Some("bot-a".into()),
            angle: Some("security".into()),
        };
        assert_eq!(
            store
                .record_review_findings(
                    "ses_1",
                    "example/repo",
                    7,
                    Some("deadbeef"),
                    std::slice::from_ref(&finding)
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .record_review_findings("ses_1", "example/repo", 7, Some("deadbeef"), &[finding])
                .await
                .unwrap(),
            0,
            "a redelivered session appends nothing to the append-only ledger"
        );
    }

    #[tokio::test]
    async fn a_claimed_write_is_invisible_until_its_lease_lapses() {
        let Some(store) = store("claim_lease").await else {
            return;
        };
        assert!(store
            .enqueue_write("ses_1", "comment", &json!({"body": "verdict"}))
            .await
            .unwrap());
        assert!(
            !store
                .enqueue_write("ses_1", "comment", &json!({"body": "verdict again"}))
                .await
                .unwrap(),
            "one row per (session, kind): at-least-once delivery cannot double-post"
        );
        let claimed = ProductStore::claim_writes(&store, 10).await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert!(
            ProductStore::claim_writes(&store, 10)
                .await
                .unwrap()
                .is_empty(),
            "a second drain inside the lease window sees nothing"
        );
        let replayed = store.claim_writes_for_test_after_lease(10).await.unwrap();
        assert_eq!(
            replayed.len(),
            1,
            "a lapsed lease makes the write claimable again"
        );
        store.mark_write_done(replayed[0].id).await.unwrap();
        assert!(store.pending_writes(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_write_that_keeps_failing_is_parked_as_failed() {
        let Some(store) = store("write_parking").await else {
            return;
        };
        store
            .enqueue_write("ses_1", "status", &json!({"state": "success"}))
            .await
            .unwrap();
        for attempt in 0..WRITE_MAX_ATTEMPTS {
            let claimed = store.claim_writes_for_test_after_lease(10).await.unwrap();
            assert_eq!(
                claimed.len(),
                1,
                "attempt {attempt} should still be claimable"
            );
            store
                .mark_write_failed(claimed[0].id, "github said no")
                .await
                .unwrap();
        }
        assert!(
            store
                .claim_writes_for_test_after_lease(10)
                .await
                .unwrap()
                .is_empty(),
            "a write past WRITE_MAX_ATTEMPTS is parked, not retried"
        );
        assert!(store.pending_writes(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn concurrent_drains_partition_the_queue_instead_of_double_claiming() {
        let Some(store) = store("skip_locked").await else {
            return;
        };
        for i in 0..10 {
            store
                .enqueue_write(&format!("ses_{i}"), "comment", &json!({"n": i}))
                .await
                .unwrap();
        }
        let store = std::sync::Arc::new(store);
        let (a, b) = tokio::join!(
            {
                let store = store.clone();
                async move { ProductStore::claim_writes(&*store, 10).await.unwrap() }
            },
            {
                let store = store.clone();
                async move { ProductStore::claim_writes(&*store, 10).await.unwrap() }
            }
        );
        let mut ids: Vec<i64> = a.iter().chain(b.iter()).map(|w| w.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "no write may be claimed by both drains");
        assert_eq!(total, 10, "every write is claimed by exactly one drain");
    }
}
