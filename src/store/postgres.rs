//! The Postgres backend for the kernel store (ADR 033 phase 2).
//!
//! The [`Store`] trait is sync and stays sync — 86 methods with callers in
//! every kernel module. This backend owns a dedicated tokio runtime thread;
//! each sync method enters that runtime's context and drives its future with
//! `futures::executor::block_on` (the `reqwest::blocking` pattern).
//! `tokio::task::block_in_place` is NOT used (panics on the current-thread
//! runtimes `#[tokio::test]` defaults to), and `Handle::block_on` is NOT used
//! (panics when the caller is already inside a runtime). Blocking the caller
//! on a lane-internal ~1ms query matches the SQLite backend's cost, where
//! every call already serializes behind one `Mutex<Connection>`.
//!
//! Dialect mapping is the one the controller backend shipped and soaked:
//! BIGSERIAL ids, BIGINT integers, BYTEA blobs, `$N` params,
//! `ON CONFLICT DO NOTHING`, `RETURNING`, advisory xact locks where SQLite
//! relied on IMMEDIATE-transaction serialization, `FOR UPDATE SKIP LOCKED`
//! for outbox/dispatcher claims.

#![allow(dead_code)] // unreachable until `open_store` routes postgres:// here
#![allow(unused_variables)] // stub params; remove with the last `unimplemented!` before merge

use super::*;
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::config::SslMode;
use tokio_postgres::NoTls;

/// Verified TLS against the platform root store; `sslmode=disable` is the
/// explicit lane-internal opt-out. Same posture as the controller backend
/// (council F1 on #318): no encrypt-without-verify mode exists.
fn tls_connector() -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for cert in loaded.certs {
        roots
            .add(cert)
            .map_err(|error| anyhow::anyhow!("root certificate rejected: {error}"))?;
    }
    if roots.is_empty() {
        let detail = loaded
            .errors
            .first()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no certificates found".into());
        anyhow::bail!("platform root certificate store unavailable: {detail}");
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

pub struct PostgresStore {
    pool: Pool,
    /// Dedicated runtime driving connections and timers; kept alive for the
    /// store's lifetime on its own thread.
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
}

impl PostgresStore {
    pub fn open(url: &str) -> Result<Self> {
        Self::open_with_options(url, None)
    }

    /// `search_path` carves out a schema — the tests' isolation mechanism.
    pub(crate) fn open_with_options(url: &str, search_path: Option<&str>) -> Result<Self> {
        let runtime = std::sync::Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("pg-store")
                .enable_all()
                .build()?,
        );
        let mut config: tokio_postgres::Config = url.parse()?;
        if let Some(schema) = search_path {
            config.options(format!("-c search_path={schema}"));
        }
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
            .map_err(|error| anyhow::anyhow!("postgres pool: {error}"))?;
        let store = Self { pool, runtime };
        store.block(async { store.migrate_pg().await })?;
        Ok(store)
    }

    /// Run a future to completion from a sync method, regardless of whether
    /// the caller is inside a tokio runtime. See module docs for why this
    /// exact combination and not the alternatives.
    fn block<F: std::future::Future>(&self, future: F) -> F::Output {
        let _guard = self.runtime.handle().enter();
        futures::executor::block_on(future)
    }

    async fn client(&self) -> Result<deadpool_postgres::Client> {
        self.pool
            .get()
            .await
            .map_err(|error| anyhow::anyhow!("postgres pool: {error}"))
    }

    async fn migrate_pg(&self) -> Result<()> {
        let mut client = self.client().await?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                   version BIGINT PRIMARY KEY, applied_at BIGINT NOT NULL);",
            )
            .await?;
        let transaction = client.transaction().await?;
        transaction
            .query("SELECT pg_advisory_xact_lock($1)", &[&0x0CB_0334_i64])
            .await?;
        let applied: i64 = transaction
            .query_one("SELECT COUNT(*) FROM schema_migrations", &[])
            .await?
            .get(0);
        for (index, sql) in PG_MIGRATIONS
            .iter()
            .enumerate()
            .skip(applied.max(0) as usize)
        {
            transaction.batch_execute(sql).await?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)",
                    &[&((index + 1) as i64), &now_ms()],
                )
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }
}

/// The kernel schema in Postgres dialect — the SQLite `SCHEMA` batch translated
/// (BIGSERIAL / BIGINT / BYTEA) with the `migrate()` fixup indexes folded in,
/// since a fresh Postgres database never needs the legacy ALTER path.
/// Versioned via `schema_migrations` from day one.
const PG_MIGRATIONS: &[&str] = &[r#"
CREATE TABLE IF NOT EXISTS bots (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
    token_hash TEXT NOT NULL, token_plain TEXT,
    last_seen BIGINT,
    provider TEXT, capabilities TEXT NOT NULL DEFAULT '[]',
    enabled BIGINT NOT NULL DEFAULT 1,
    health TEXT NOT NULL DEFAULT 'ok',
    consecutive_errors BIGINT NOT NULL DEFAULT 0, last_error_at BIGINT,
    note TEXT, version TEXT, runtime TEXT,
    source TEXT NOT NULL DEFAULT 'registered'
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
    trigger_ref TEXT, trigger_fingerprint TEXT, quorum_n BIGINT NOT NULL, chair_bot TEXT,
    created_at BIGINT NOT NULL, closed_at BIGINT,
    mode TEXT NOT NULL DEFAULT 'council',
    decision TEXT, findings_red BIGINT, findings_yellow BIGINT, findings_green BIGINT,
    result_author_id TEXT, result_message_ids TEXT
);
CREATE TABLE IF NOT EXISTS session_bots (
    session_id TEXT NOT NULL, bot_id TEXT NOT NULL,
    -- Materializes SQLite's implicit rowid: roster order = insertion order,
    -- and pipeline stage order rides on it.
    ord BIGSERIAL,
    PRIMARY KEY (session_id, bot_id)
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL UNIQUE, root_message_id TEXT
);
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, thread_id TEXT,
    author_kind TEXT NOT NULL, author_id TEXT, audience TEXT, content TEXT NOT NULL,
    reply_to TEXT, created_at BIGINT NOT NULL,
    -- rowid replacement: same-millisecond messages keep insertion order.
    ord BIGSERIAL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, created_at);
CREATE TABLE IF NOT EXISTS reactions (
    message_id TEXT NOT NULL, bot_id TEXT NOT NULL, emoji TEXT NOT NULL,
    PRIMARY KEY (message_id, bot_id, emoji)
);
CREATE TABLE IF NOT EXISTS outbox (
    seq BIGSERIAL PRIMARY KEY,
    bot_id TEXT NOT NULL, session_id TEXT, idem_key TEXT, frame TEXT NOT NULL, created_at BIGINT NOT NULL,
    delivered_at BIGINT
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
    token TEXT NOT NULL, expires_at BIGINT NOT NULL,
    PRIMARY KEY (session_id, role)
);
-- ADR 020 findings ledger (PR-review plugin-owned). One row per finding per
-- review round; a session IS a round, so history across rounds = rows across
-- sessions of the same repo/pr. Append-only.
-- ponytail: no rounds/events tables yet — sessions already carries round
-- metadata; add pr_review_finding_events when resolve/dismiss commands land.
CREATE TABLE IF NOT EXISTS pr_review_findings (
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
CREATE INDEX IF NOT EXISTS idx_prf_repo_pr ON pr_review_findings(repo, pr_number, id);
-- Reviews the hourly cap dropped (SEI-819). One row per trigger_ref; a later
-- drop overwrites so the newest dropped head wins. The catch-up sweep convenes
-- and deletes once the cap window clears.
CREATE TABLE IF NOT EXISTS pending_reviews (
    trigger_ref TEXT PRIMARY KEY,
    repo TEXT NOT NULL, pr_number BIGINT NOT NULL,
    fingerprint TEXT, preset TEXT,
    requested_at BIGINT NOT NULL
);
-- Durable removal evidence for staged compatibility surfaces. Generic by
-- design: the store treats `surface` as an opaque key and owns no provider
-- vocabulary. Counters survive process and image restarts across a release.
CREATE TABLE IF NOT EXISTS compatibility_usage (
    surface TEXT PRIMARY KEY,
    uses BIGINT NOT NULL,
    first_used_at BIGINT NOT NULL,
    last_used_at BIGINT NOT NULL
);
-- Provider-neutral external-controller boundary (ADR 008 / migration P4).
-- These are additive typed tables so the previous OCP image ignores them and
-- still starts against the same database.
CREATE TABLE IF NOT EXISTS controllers (
    id TEXT PRIMARY KEY,
    enabled BIGINT NOT NULL DEFAULT 1,
    max_concurrent_sessions BIGINT NOT NULL DEFAULT 5,
    max_actions_per_minute BIGINT NOT NULL DEFAULT 60,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS controller_action_tokens (
    id TEXT PRIMARY KEY,
    controller_id TEXT NOT NULL,
    token_hash BYTEA NOT NULL,
    pepper_version BIGINT NOT NULL,
    not_before BIGINT NOT NULL,
    expires_at BIGINT,
    revoked_at BIGINT,
    created_at BIGINT NOT NULL,
    UNIQUE(controller_id, token_hash, pepper_version)
);
CREATE INDEX IF NOT EXISTS idx_controller_action_tokens_active
    ON controller_action_tokens(controller_id, revoked_at, expires_at);
CREATE TABLE IF NOT EXISTS controller_action_grants (
    controller_id TEXT NOT NULL,
    action_kind TEXT NOT NULL,
    granted BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (controller_id, action_kind)
);
CREATE TABLE IF NOT EXISTS controller_bindings (
    controller_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    enabled BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (controller_id, scope)
);
CREATE TABLE IF NOT EXISTS controller_action_idempotency (
    controller_id TEXT NOT NULL,
    action_id TEXT NOT NULL,
    request_hash BYTEA NOT NULL,
    action_kind TEXT NOT NULL,
    scope TEXT NOT NULL,
    state TEXT NOT NULL,
    http_status BIGINT,
    response_json TEXT,
    received_at BIGINT NOT NULL,
    completed_at BIGINT,
    PRIMARY KEY (controller_id, action_id)
);
CREATE INDEX IF NOT EXISTS idx_controller_actions_rate
    ON controller_action_idempotency(controller_id, received_at);
CREATE TABLE IF NOT EXISTS controller_sessions (
    controller_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    trigger_ref TEXT NOT NULL,
    trigger_fingerprint TEXT,
    session_id TEXT NOT NULL UNIQUE,
    current BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (controller_id, session_id)
);
CREATE INDEX IF NOT EXISTS idx_controller_sessions_scope
    ON controller_sessions(controller_id, scope, session_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_controller_current_trigger
    ON controller_sessions(controller_id, trigger_ref) WHERE current = 1;
CREATE TABLE IF NOT EXISTS controller_event_grants (
    controller_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    granted BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (controller_id, event_type)
);
CREATE TABLE IF NOT EXISTS controller_events (
    id TEXT PRIMARY KEY,
    controller_id TEXT NOT NULL,
    session_id TEXT,
    event_type TEXT NOT NULL,
    event_endpoint TEXT NOT NULL,
    event_key_version BIGINT NOT NULL,
    body_json TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    attempts BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    next_attempt_at BIGINT NOT NULL,
    lease_until BIGINT,
    delivered_at BIGINT,
    last_error TEXT,
    -- rowid replacement: stable claim ordering under equal timestamps.
    ord BIGSERIAL,
    UNIQUE(controller_id, idempotency_key)
);
CREATE INDEX IF NOT EXISTS idx_controller_events_due
    ON controller_events(state, next_attempt_at, created_at);
CREATE TABLE IF NOT EXISTS controller_event_audit (
    id BIGSERIAL PRIMARY KEY,
    controller_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT NOT NULL,
    created_at BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_controller_event_audit_controller
    ON controller_event_audit(controller_id, created_at);

-- Folded in from the SQLite migrate() fixups (fresh-DB schema needs no ALTERs):
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_idem ON outbox(idem_key);
CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(bot_id, seq) WHERE delivered_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_outbox_session_bot ON outbox(session_id, bot_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_active_trigger_ref
    ON sessions(trigger_ref)
    WHERE trigger_ref IS NOT NULL AND state NOT IN ('closed', 'aborted');
"#];

impl Store for PostgresStore {
    fn upsert_controller_installation(
        &self,
        controller_id: &str,
        max_concurrent_sessions: i64,
        max_actions_per_minute: i64,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    #[allow(clippy::too_many_arguments)]
    fn provision_controller_installation(
        &self,
        controller_id: &str,
        max_concurrent_sessions: i64,
        max_actions_per_minute: i64,
        actions: &[String],
        scopes: &[String],
        token: &NewControllerActionToken,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn controller_installation(
        &self,
        controller_id: &str,
    ) -> Result<Option<ControllerInstallation>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_controller_installation_enabled(
        &self,
        controller_id: &str,
        enabled: bool,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    #[allow(clippy::too_many_arguments)]
    fn put_controller_action_token(
        &self,
        token_id: &str,
        controller_id: &str,
        token_hash: &[u8],
        pepper_version: i64,
        not_before: i64,
        expires_at: Option<i64>,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn expire_controller_action_tokens(
        &self,
        controller_id: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<usize> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn rotate_controller_action_token(
        &self,
        controller_id: &str,
        token: &NewControllerActionToken,
        old_tokens_expire_at: i64,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn revoke_controller_action_token(
        &self,
        controller_id: &str,
        token_id: &str,
        revoked_at: i64,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn active_controller_action_tokens(&self, now: i64) -> Result<Vec<ControllerActionToken>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_controller_action_grant(
        &self,
        controller_id: &str,
        action_kind: &str,
        granted: bool,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_controller_scope_binding(
        &self,
        controller_id: &str,
        scope: &str,
        enabled: bool,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_controller_action(
        &self,
        controller_id: &str,
        credential_hashes: &[ControllerCredentialHash],
        action_id: &str,
        request_hash: &[u8],
        action_kind: &str,
        scope: &str,
        session_id: Option<&str>,
        open_intent: Option<&ControllerOpenIntent>,
        now: i64,
    ) -> Result<ControllerActionStart> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn finish_controller_action(
        &self,
        controller_id: &str,
        action_id: &str,
        http_status: i64,
        response_json: &str,
        session_binding: Option<&ControllerSessionBinding>,
        completed_at: i64,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn controller_session_for_trigger(
        &self,
        controller_id: &str,
        trigger_ref: &str,
    ) -> Result<Option<ControllerSessionBinding>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn configure_controller_events(
        &self,
        controller_id: &str,
        endpoint: &str,
        key_version: i64,
        event_types: &[String],
        now: i64,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn claim_controller_events(
        &self,
        now: i64,
        limit: usize,
        lease_ms: i64,
    ) -> Result<Vec<ControllerEventDelivery>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn complete_controller_event(&self, event_id: &str, delivered_at: i64) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn fail_controller_event(
        &self,
        event_id: &str,
        error: &str,
        next_attempt_at: Option<i64>,
        now: i64,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn prune_delivered_controller_events(&self, before: i64) -> Result<usize> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn controller_event_audit(&self, controller_id: &str) -> Result<Vec<ControllerEventAudit>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn register_bot(
        &self,
        name: &str,
        role: &str,
        token_hash: &str,
        token_plain: &str,
    ) -> Result<Bot> {
        let id = new_id("bot");
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "INSERT INTO bots (id, name, role, token_hash, token_plain, source)
                     VALUES ($1, $2, $3, $4, $5, 'registered')",
                    &[&id, &name, &role, &token_hash, &token_plain],
                )
                .await?;
            Ok(Bot {
                id: id.clone(),
                name: name.to_string(),
                role: role.to_string(),
            })
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
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "INSERT INTO bots (id, name, role, token_hash, token_plain, source)
                     VALUES ($1, $2, $3, $4, $5, 'seeded')
                     ON CONFLICT (id) DO NOTHING",
                    &[&id, &name, &role, &token_hash, &token_plain],
                )
                .await?;
            Ok(n > 0)
        })
    }

    fn bot_by_token_hash(&self, token_hash: &str) -> Result<Option<Bot>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt(
                    "SELECT id, name, role FROM bots WHERE token_hash = $1",
                    &[&token_hash],
                )
                .await?
                .map(map_bot_pg))
        })
    }

    fn bot(&self, id: &str) -> Result<Option<Bot>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt("SELECT id, name, role FROM bots WHERE id = $1", &[&id])
                .await?
                .map(map_bot_pg))
        })
    }

    fn bot_token_plain(&self, id: &str) -> Result<Option<String>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt("SELECT token_plain FROM bots WHERE id = $1", &[&id])
                .await?
                .and_then(|r| r.get::<_, Option<String>>(0)))
        })
    }

    fn bots_with_plaintext_token(&self) -> Result<Vec<String>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query(
                    "SELECT name FROM bots
                     WHERE token_plain IS NOT NULL AND token_plain != ''
                     ORDER BY name",
                    &[],
                )
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect())
        })
    }

    fn touch_last_seen(&self, bot_id: &str) -> Result<()> {
        self.touch_last_seen_at(bot_id, now_ms())
    }

    fn touch_last_seen_at(&self, bot_id: &str, ts: i64) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            // Monotonic: never move last_seen backwards (council #274 F1).
            client
                .execute(
                    "UPDATE bots SET last_seen = $2
                     WHERE id = $1 AND (last_seen IS NULL OR $2 > last_seen)",
                    &[&bot_id, &ts],
                )
                .await?;
            Ok(())
        })
    }

    fn list_bots(&self) -> Result<Vec<BotInventory>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query(
                    "SELECT id, name, role, provider, capabilities, enabled,
                            health, note, version, runtime, last_seen, source
                     FROM bots
                     ORDER BY id ASC",
                    &[],
                )
                .await?
                .iter()
                .map(map_bot_inventory_pg)
                .collect())
        })
    }

    fn bot_inventory(&self, id: &str) -> Result<Option<BotInventory>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt(
                    "SELECT id, name, role, provider, capabilities, enabled,
                            health, note, version, runtime, last_seen, source
                     FROM bots
                     WHERE id = $1",
                    &[&id],
                )
                .await?
                .as_ref()
                .map(map_bot_inventory_pg))
        })
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
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            tx.query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&id])
                .await?;
            let source: Option<String> = tx
                .query_opt("SELECT source FROM bots WHERE id = $1", &[&id])
                .await?
                .map(|r| r.get(0));
            let inserted = if source.is_some() {
                tx.execute(
                    "UPDATE bots
                     SET provider = COALESCE($2, provider),
                         capabilities = CASE WHEN $3 THEN $4 ELSE capabilities END,
                         version = COALESCE($5, version),
                         runtime = COALESCE($6, runtime)
                     WHERE id = $1",
                    &[
                        &id,
                        &metadata.provider.as_deref(),
                        &metadata.capabilities.is_some(),
                        &capabilities.as_deref(),
                        &metadata.version.as_deref(),
                        &runtime.as_deref(),
                    ],
                )
                .await?;
                false
            } else {
                let token = format!("oabct_{}", uuid::Uuid::new_v4().simple());
                let display_name = name.unwrap_or(id);
                tx.execute(
                    "INSERT INTO bots
                        (id, name, role, token_hash, token_plain, provider, capabilities,
                         version, runtime, source)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'discovered')",
                    &[
                        &id,
                        &display_name,
                        &role,
                        &crate::identity::hash_token(&token),
                        &token.as_str(),
                        &metadata.provider.as_deref(),
                        &capabilities.as_deref().unwrap_or("[]"),
                        &metadata.version.as_deref(),
                        &runtime.as_deref(),
                    ],
                )
                .await?;
                true
            };
            let bot = tx
                .query_one("SELECT id, name, role FROM bots WHERE id = $1", &[&id])
                .await
                .map(map_bot_pg)?;
            tx.commit().await?;
            Ok((bot, inserted))
        })
    }

    fn update_bot_metadata(&self, id: &str, patch: &BotMetadataPatch) -> Result<bool> {
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            let exists = tx
                .query_opt("SELECT 1 FROM bots WHERE id = $1", &[&id])
                .await?
                .is_some();
            if !exists {
                tx.rollback().await?;
                return Ok(false);
            }
            if let Some(provider) = &patch.provider {
                tx.execute(
                    "UPDATE bots SET provider = $2 WHERE id = $1",
                    &[&id, &provider.as_deref()],
                )
                .await?;
            }
            if let Some(capabilities) = &patch.capabilities {
                tx.execute(
                    "UPDATE bots SET capabilities = $2 WHERE id = $1",
                    &[&id, &capabilities_json(capabilities)],
                )
                .await?;
            }
            if let Some(enabled) = patch.enabled {
                tx.execute(
                    "UPDATE bots SET enabled = $2 WHERE id = $1",
                    &[&id, &(enabled as i64)],
                )
                .await?;
            }
            if let Some(health) = &patch.health {
                tx.execute("UPDATE bots SET health = $2 WHERE id = $1", &[&id, &health])
                    .await?;
            }
            if let Some(note) = &patch.note {
                tx.execute(
                    "UPDATE bots SET note = $2 WHERE id = $1",
                    &[&id, &note.as_deref()],
                )
                .await?;
            }
            if let Some(version) = &patch.version {
                tx.execute(
                    "UPDATE bots SET version = $2 WHERE id = $1",
                    &[&id, &version.as_deref()],
                )
                .await?;
            }
            if let Some(runtime) = &patch.runtime {
                let runtime = runtime.as_ref().map(serde_json::to_string).transpose()?;
                tx.execute(
                    "UPDATE bots SET runtime = $2 WHERE id = $1",
                    &[&id, &runtime.as_deref()],
                )
                .await?;
            }
            tx.commit().await?;
            Ok(true)
        })
    }

    fn delete_bot(&self, bot_id: &str) -> Result<DeleteBotOutcome> {
        self.block(async {
            let client = self.client().await?;
            let has_active_session = client
                .query_opt(
                    "SELECT 1
                     FROM session_bots sb
                     JOIN sessions s ON s.id = sb.session_id
                     WHERE sb.bot_id = $1
                       AND s.state NOT IN ('closed', 'aborted')
                     LIMIT 1",
                    &[&bot_id],
                )
                .await?
                .is_some();
            if has_active_session {
                return Ok(DeleteBotOutcome::ActiveSession);
            }
            let deleted = client
                .execute("DELETE FROM bots WHERE id = $1", &[&bot_id])
                .await?;
            if deleted == 0 {
                Ok(DeleteBotOutcome::NotFound)
            } else {
                Ok(DeleteBotOutcome::Deleted)
            }
        })
    }

    fn record_bot_frame(
        &self,
        bot_id: &str,
        is_error: bool,
        threshold: i64,
    ) -> Result<BotHealthTransition> {
        self.block(async {
            let mut client = self.client().await?;
            // Serialize per bot: SQLite ran this read-decide-write under the
            // global connection lock; two concurrent frames must not race the
            // consecutive_errors counter past the degraded threshold.
            let tx = client.transaction().await?;
            tx.query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&bot_id])
                .await?;
            let Some(row) = tx
                .query_opt(
                    "SELECT consecutive_errors, health FROM bots WHERE id = $1",
                    &[&bot_id],
                )
                .await?
            else {
                tx.rollback().await?;
                return Ok(BotHealthTransition::None); // unknown bot
            };
            let (errors, health): (i64, String) = (row.get(0), row.get(1));
            let outcome = if is_error {
                let next = errors + 1;
                if next >= threshold && health != "degraded" {
                    tx.execute(
                        "UPDATE bots SET consecutive_errors = $2, last_error_at = $3, health = 'degraded' WHERE id = $1",
                        &[&bot_id, &next, &now_ms()],
                    )
                    .await?;
                    BotHealthTransition::Degraded
                } else {
                    tx.execute(
                        "UPDATE bots SET consecutive_errors = $2, last_error_at = $3 WHERE id = $1",
                        &[&bot_id, &next, &now_ms()],
                    )
                    .await?;
                    BotHealthTransition::None
                }
            } else if health == "degraded" {
                tx.execute(
                    "UPDATE bots SET consecutive_errors = 0, health = 'ok' WHERE id = $1",
                    &[&bot_id],
                )
                .await?;
                BotHealthTransition::Recovered
            } else if errors != 0 {
                tx.execute(
                    "UPDATE bots SET consecutive_errors = 0 WHERE id = $1",
                    &[&bot_id],
                )
                .await?;
                BotHealthTransition::None
            } else {
                BotHealthTransition::None
            };
            tx.commit().await?;
            Ok(outcome)
        })
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
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            tx.execute(
                "INSERT INTO sessions (id, title, state, trigger_ref, quorum_n, chair_bot, created_at, mode)
                 VALUES ($1, $2, 'open', $3, $4, $5, $6, $7)",
                &[&id, &title, &trigger_ref, &quorum_n, &chair_bot, &created_at, &mode],
            )
            .await?;
            for bot_id in roster {
                tx.execute(
                    "INSERT INTO session_bots (session_id, bot_id) VALUES ($1, $2)
                     ON CONFLICT (session_id, bot_id) DO NOTHING",
                    &[&id, &bot_id],
                )
                .await?;
            }
            tx.commit().await?;
            Ok(Session {
                id: id.clone(),
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
            let existing = self.block(async {
                let client = self.client().await?;
                active_session_for_trigger_pg(&**client, trigger_ref).await
            })?;
            if let Some(existing) = existing {
                return Ok((existing, true));
            }
        }
        match self.create_session(title, trigger_ref, quorum_n, chair_bot, roster, mode) {
            Ok(session) => Ok((session, false)),
            Err(err) if trigger_ref.is_some() && is_pg_unique_violation(&err) => {
                let trigger_ref = trigger_ref.expect("checked by is_some guard");
                let existing = self.block(async {
                    let client = self.client().await?;
                    active_session_for_trigger_pg(&**client, trigger_ref).await
                })?;
                if let Some(existing) = existing {
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
        opening_inputs: &[OpeningInput],
    ) -> Result<(Session, SessionCreateOutcome)> {
        let id = new_id("ses");
        let created_at = now_ms();
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            let mut outcome = SessionCreateOutcome::Created;
            if let Some(trigger_ref) = trigger_ref {
                // Serialize per trigger: SQLite ran the whole check-close-insert
                // under the global mutex; two racing superseders must not both
                // close-and-insert (the second would trip the partial unique
                // index mid-flight instead of deduping cleanly).
                tx.query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&trigger_ref])
                    .await?;
                if let Some(existing) = active_session_for_trigger_pg(&*tx, trigger_ref).await? {
                    if matches!(
                        (existing.trigger_fingerprint.as_deref(), trigger_fingerprint),
                        (Some(existing), Some(incoming)) if existing == incoming
                    ) {
                        tx.commit().await?;
                        return Ok((existing, SessionCreateOutcome::Deduped));
                    }
                    tx.execute(
                        "UPDATE sessions SET state = 'closed', closed_at = $2
                         WHERE id = $1 AND state NOT IN ('closed', 'aborted')",
                        &[&existing.id.as_str(), &created_at],
                    )
                    .await?;
                    enqueue_controller_session_event_pg(
                        &*tx,
                        &existing.id,
                        "session.superseded",
                        json!({ "reason": "superseded" }),
                        &format!("session.superseded:{}", existing.id),
                        created_at,
                    )
                    .await?;
                    outcome = SessionCreateOutcome::Superseded {
                        old_id: existing.id,
                    };
                }
            }
            let initial_state = if opening_inputs.is_empty() {
                "open"
            } else {
                "deliberating"
            };
            tx.execute(
                "INSERT INTO sessions
                    (id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, mode)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[&id, &title, &initial_state, &trigger_ref, &trigger_fingerprint, &quorum_n, &chair_bot, &created_at, &mode],
            )
            .await?;
            for bot_id in roster {
                tx.execute(
                    "INSERT INTO session_bots (session_id, bot_id) VALUES ($1, $2)
                     ON CONFLICT (session_id, bot_id) DO NOTHING",
                    &[&id, &bot_id],
                )
                .await?;
            }
            for input in opening_inputs {
                insert_opening_input_pg(&*tx, &id, input, created_at).await?;
            }
            tx.commit().await?;
            let session = Session {
                id: id.clone(),
                title: title.to_string(),
                state: initial_state.into(),
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
        })
    }

    fn session(&self, id: &str) -> Result<Option<Session>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt(
                    &format!("SELECT {SESSION_COLS} FROM sessions WHERE id = $1"),
                    &[&id],
                )
                .await?
                .as_ref()
                .map(map_session_pg))
        })
    }

    fn list_sessions(
        &self,
        trigger_ref: Option<&str>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Session>> {
        let limit = limit as i64;
        self.block(async {
            let client = self.client().await?;
            // id DESC replaces SQLite's rowid DESC tiebreak; ids are random so
            // this only stabilizes equal-timestamp ordering deterministically.
            let rows = match (trigger_ref, state) {
                (Some(trigger_ref), Some(state)) => {
                    client
                        .query(
                            &format!(
                                "SELECT {SESSION_COLS} FROM sessions
                                 WHERE trigger_ref = $1 AND state = $2
                                 ORDER BY created_at DESC, id DESC LIMIT $3"
                            ),
                            &[&trigger_ref, &state, &limit],
                        )
                        .await?
                }
                (Some(trigger_ref), None) => {
                    client
                        .query(
                            &format!(
                                "SELECT {SESSION_COLS} FROM sessions
                                 WHERE trigger_ref = $1
                                 ORDER BY created_at DESC, id DESC LIMIT $2"
                            ),
                            &[&trigger_ref, &limit],
                        )
                        .await?
                }
                (None, Some(state)) => {
                    client
                        .query(
                            &format!(
                                "SELECT {SESSION_COLS} FROM sessions
                                 WHERE state = $1
                                 ORDER BY created_at DESC, id DESC LIMIT $2"
                            ),
                            &[&state, &limit],
                        )
                        .await?
                }
                (None, None) => {
                    client
                        .query(
                            &format!(
                                "SELECT {SESSION_COLS} FROM sessions
                                 ORDER BY created_at DESC, id DESC LIMIT $1"
                            ),
                            &[&limit],
                        )
                        .await?
                }
            };
            Ok(rows.iter().map(map_session_pg).collect())
        })
    }

    fn add_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "INSERT INTO session_bots (session_id, bot_id) VALUES ($1, $2)
                     ON CONFLICT (session_id, bot_id) DO NOTHING",
                    &[&session_id, &bot_id],
                )
                .await?;
            Ok(n == 1)
        })
    }

    fn add_session_bots_if_capacity(
        &self,
        session_id: &str,
        bot_ids: &[String],
        max_roster: usize,
        opening_inputs: &[OpeningInput],
    ) -> Result<RosterAddOutcome> {
        let mut unique = Vec::new();
        for bot_id in bot_ids {
            if !unique.iter().any(|known| known == bot_id) {
                unique.push(bot_id.clone());
            }
        }
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            // Serialize per session: the capacity check-then-insert raced only
            // behind SQLite's global mutex.
            tx.query("SELECT pg_advisory_xact_lock(hashtext($1))", &[&session_id])
                .await?;
            let roster_len: i64 = tx
                .query_one(
                    "SELECT COUNT(*) FROM session_bots WHERE session_id = $1",
                    &[&session_id],
                )
                .await?
                .get(0);
            let mut added = Vec::new();
            let mut already_members = Vec::new();
            for bot_id in unique {
                let exists: bool = tx
                    .query_one(
                        "SELECT EXISTS(
                            SELECT 1 FROM session_bots WHERE session_id = $1 AND bot_id = $2
                        )",
                        &[&session_id, &bot_id],
                    )
                    .await?
                    .get(0);
                if exists {
                    already_members.push(bot_id);
                } else {
                    added.push(bot_id);
                }
            }
            if roster_len as usize + added.len() > max_roster {
                tx.rollback().await?;
                return Ok(RosterAddOutcome::Full);
            }
            for bot_id in &added {
                tx.execute(
                    "INSERT INTO session_bots (session_id, bot_id) VALUES ($1, $2)",
                    &[&session_id, &bot_id],
                )
                .await?;
            }
            let created_at = now_ms();
            for input in opening_inputs
                .iter()
                .filter(|input| added.iter().any(|bot_id| bot_id == &input.recipient))
            {
                insert_opening_input_pg(&*tx, session_id, input, created_at).await?;
            }
            tx.commit().await?;
            Ok(RosterAddOutcome::Added {
                added,
                already_members,
            })
        })
    }

    fn replace_session_bot(
        &self,
        session_id: &str,
        old_bot_id: &str,
        new_bot_id: &str,
    ) -> Result<bool> {
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "UPDATE session_bots SET bot_id = $3
                     WHERE session_id = $1 AND bot_id = $2",
                    &[&session_id, &old_bot_id, &new_bot_id],
                )
                .await?;
            Ok(n == 1)
        })
    }

    fn remove_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "DELETE FROM session_bots WHERE session_id = $1 AND bot_id = $2",
                    &[&session_id, &bot_id],
                )
                .await?;
            Ok(n == 1)
        })
    }

    fn set_session_quorum(&self, session_id: &str, quorum_n: i64) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "UPDATE sessions SET quorum_n = $2 WHERE id = $1",
                    &[&session_id, &quorum_n],
                )
                .await?;
            Ok(())
        })
    }

    fn set_session_chair(&self, session_id: &str, chair_bot: &str) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "UPDATE sessions SET chair_bot = $2 WHERE id = $1",
                    &[&session_id, &chair_bot],
                )
                .await?;
            Ok(())
        })
    }

    fn set_state(&self, session_id: &str, state: SessionState) -> Result<()> {
        let closed_at = if matches!(state, SessionState::Closed | SessionState::Aborted) {
            Some(now_ms())
        } else {
            None
        };
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "UPDATE sessions SET state = $2, closed_at = COALESCE($3, closed_at) WHERE id = $1",
                    &[&session_id, &state.as_str(), &closed_at],
                )
                .await?;
            Ok(())
        })
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
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "UPDATE sessions SET state = $3, closed_at = COALESCE($4, closed_at)
                     WHERE id = $1 AND state = $2",
                    &[&session_id, &from.as_str(), &to.as_str(), &closed_at],
                )
                .await?;
            Ok(n == 1)
        })
    }

    fn close_if_active(&self, session_id: &str, event_type: &str, reason: &str) -> Result<bool> {
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            let closed_at = now_ms();
            let n = tx
                .execute(
                    "UPDATE sessions SET state = 'closed', closed_at = $2
                     WHERE id = $1 AND state NOT IN ('closed', 'aborted')",
                    &[&session_id, &closed_at],
                )
                .await?;
            if n == 1 {
                enqueue_controller_session_event_pg(
                    &*tx,
                    session_id,
                    event_type,
                    json!({ "state": "closed", "reason": reason }),
                    &format!("{event_type}:{session_id}:{closed_at}"),
                    closed_at,
                )
                .await?;
            }
            tx.commit().await?;
            Ok(n == 1)
        })
    }

    fn set_session_verdict(
        &self,
        session_id: &str,
        decision: &str,
        red: Option<i64>,
        yellow: Option<i64>,
        green: Option<i64>,
    ) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "UPDATE sessions SET decision = $2, findings_red = $3,
                            findings_yellow = $4, findings_green = $5
                     WHERE id = $1",
                    &[&session_id, &decision, &red, &yellow, &green],
                )
                .await?;
            Ok(())
        })
    }

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
        final_messages: &[String],
    ) -> Result<bool> {
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            let closed_at = now_ms();
            let n = tx
                .execute(
                    "UPDATE sessions SET state = 'closed', closed_at = $3,
                            decision = $4, findings_red = $5, findings_yellow = $6, findings_green = $7,
                            result_author_id = $8, result_message_ids = $9
                     WHERE id = $1 AND state = $2",
                    &[
                        &session_id,
                        &from.as_str(),
                        &closed_at,
                        &decision,
                        &red,
                        &yellow,
                        &green,
                        &result_author_id,
                        &result_message_ids_json,
                    ],
                )
                .await?;
            if n == 1 {
                // ADR 031:159 — cap final_messages, dropping the OLDEST parts;
                // the verdict trailer and findings block sit at the end.
                let mut kept: Vec<&str> = final_messages.iter().map(String::as_str).collect();
                let mut total: usize = kept.iter().map(|m| m.len()).sum();
                let mut truncated = false;
                while total > FINAL_MESSAGES_MAX_BYTES && kept.len() > 1 {
                    total -= kept.remove(0).len();
                    truncated = true;
                }
                if let Some(last) = kept.last_mut() {
                    if last.len() > FINAL_MESSAGES_MAX_BYTES {
                        let want = last.len() - FINAL_MESSAGES_MAX_BYTES;
                        let cut = (want..=last.len())
                            .find(|i| last.is_char_boundary(*i))
                            .unwrap_or(last.len());
                        *last = &last[cut..];
                        truncated = true;
                    }
                }
                let mut payload = json!({
                    "state": "closed",
                    "reason": "normal",
                    "decision": decision,
                    "findings_red": red,
                    "findings_yellow": yellow,
                    "findings_green": green,
                    "final_messages": kept,
                });
                if truncated {
                    payload["final_messages_truncated"] = json!(true);
                }
                enqueue_controller_session_event_pg(
                    &*tx,
                    session_id,
                    "session.terminal",
                    payload,
                    &format!("session.terminal:{session_id}:{closed_at}"),
                    closed_at,
                )
                .await?;
            }
            tx.commit().await?;
            Ok(n == 1)
        })
    }

    fn reopen_session(&self, session_id: &str, from: SessionState) -> Result<bool> {
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "UPDATE sessions SET state = 'deliberating',
                            result_author_id = NULL, result_message_ids = NULL
                     WHERE id = $1 AND state = $2",
                    &[&session_id, &from.as_str()],
                )
                .await?;
            Ok(n == 1)
        })
    }

    fn insert_review_findings(
        &self,
        session_id: &str,
        repo: Option<&str>,
        pr_number: Option<i64>,
        head_sha: Option<&str>,
        findings: &[NewReviewFinding],
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn review_findings(
        &self,
        repo: Option<&str>,
        pr_number: Option<i64>,
        status: Option<&str>,
        severity: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ReviewFinding>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn mark_once(&self, key: &str) -> Result<bool> {
        self.block(async {
            let client = self.client().await?;
            let n = client
                .execute(
                    "INSERT INTO settings (key, value) VALUES ($1, '1')
                     ON CONFLICT (key) DO NOTHING",
                    &[&key],
                )
                .await?;
            Ok(n == 1)
        })
    }

    fn upsert_pending_review(
        &self,
        trigger_ref: &str,
        repo: &str,
        pr_number: i64,
        fingerprint: Option<&str>,
        preset: Option<&str>,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn pending_reviews(&self) -> Result<Vec<PendingReview>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn delete_pending_review(&self, trigger_ref: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn latest_session_created_at(&self, trigger_ref: &str) -> Result<Option<i64>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_one(
                    "SELECT MAX(created_at) FROM sessions WHERE trigger_ref = $1",
                    &[&trigger_ref],
                )
                .await?
                .get(0))
        })
    }

    fn active_sessions_before(&self, cutoff_ms: i64) -> Result<Vec<String>> {
        self.block(async {
            let client = self.client().await?;
            // Anchor on last activity, not created_at (see SQLite impl notes).
            Ok(client
                .query(
                    "SELECT s.id FROM sessions s
                     WHERE s.state NOT IN ('closed', 'aborted')
                       AND COALESCE(
                             (SELECT MAX(m.created_at) FROM messages m WHERE m.session_id = s.id),
                             s.created_at
                           ) < $1",
                    &[&cutoff_ms],
                )
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect())
        })
    }

    fn roster(&self, session_id: &str) -> Result<Vec<String>> {
        self.block(async {
            let client = self.client().await?;
            // ord materializes SQLite's rowid: insertion order = the order the
            // roster was passed at create_session; pipeline stages ride on it.
            Ok(client
                .query(
                    "SELECT bot_id FROM session_bots WHERE session_id = $1 ORDER BY ord",
                    &[&session_id],
                )
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect())
        })
    }

    fn active_session_for_trigger(&self, trigger_ref: &str) -> Result<Option<String>> {
        self.block(async {
            let client = self.client().await?;
            Ok(active_session_for_trigger_pg(&**client, trigger_ref)
                .await?
                .map(|session| session.id))
        })
    }

    fn standing_roster(&self) -> Result<Option<Vec<String>>> {
        self.block(async {
            let client = self.client().await?;
            let value: Option<String> = client
                .query_opt(
                    "SELECT value FROM settings WHERE key = 'council_roster'",
                    &[],
                )
                .await?
                .map(|r| r.get(0));
            value
                .map(|raw| serde_json::from_str(&raw).context("decode council_roster setting"))
                .transpose()
        })
    }

    fn set_standing_roster(&self, roster: &[String]) -> Result<()> {
        let value = serde_json::to_string(roster)?;
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "INSERT INTO settings (key, value)
                     VALUES ('council_roster', $1)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    &[&value],
                )
                .await?;
            Ok(())
        })
    }

    fn upsert_thread(&self, session_id: &str, root_message_id: Option<&str>) -> Result<String> {
        self.block(async {
            let client = self.client().await?;
            if let Some(existing) = client
                .query_opt(
                    "SELECT id FROM threads WHERE session_id = $1",
                    &[&session_id],
                )
                .await?
                .map(|r| r.get::<_, String>(0))
            {
                return Ok(existing);
            }
            let id = new_id("thr");
            client
                .execute(
                    "INSERT INTO threads (id, session_id, root_message_id) VALUES ($1, $2, $3)",
                    &[&id, &session_id, &root_message_id],
                )
                .await?;
            Ok(id)
        })
    }

    fn thread_for_session(&self, session_id: &str) -> Result<Option<String>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt(
                    "SELECT id FROM threads WHERE session_id = $1",
                    &[&session_id],
                )
                .await?
                .map(|r| r.get(0)))
        })
    }

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
    ) -> Result<Message> {
        let id = new_id("msg");
        let created_at = now_ms();
        self.block(async {
            let mut client = self.client().await?;
            let tx = client.transaction().await?;
            tx.execute(
                "INSERT INTO messages (id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                &[&id, &session_id, &thread_id, &author_kind, &author_id, &audience, &content, &reply_to, &created_at],
            )
            .await?;
            enqueue_controller_session_event_pg(
                &*tx,
                session_id,
                "session.progress",
                json!({
                    "message_id": id,
                    "author_kind": author_kind,
                    "author_id": author_id,
                    "audience": audience,
                }),
                &format!("session.progress:{id}"),
                created_at,
            )
            .await?;
            tx.commit().await?;
            Ok(Message {
                id: id.clone(),
                session_id: session_id.to_string(),
                thread_id: thread_id.map(String::from),
                author_kind: author_kind.to_string(),
                author_id: author_id.map(String::from),
                audience: audience.map(String::from),
                content: content.to_string(),
                reply_to: reply_to.map(String::from),
                created_at,
            })
        })
    }

    fn edit_message(&self, message_id: &str, content: &str) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "UPDATE messages SET content = $2 WHERE id = $1",
                    &[&message_id, &content],
                )
                .await?;
            Ok(())
        })
    }

    fn message(&self, id: &str) -> Result<Option<Message>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query_opt(
                    "SELECT id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at
                     FROM messages WHERE id = $1",
                    &[&id],
                )
                .await?
                .as_ref()
                .map(map_message_pg))
        })
    }

    fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        self.block(async {
            let client = self.client().await?;
            // ord tiebreak replaces rowid: equal-timestamp chunks keep insertion
            // order regardless of plan (ADR 028 span integrity, council #241 F1).
            Ok(client
                .query(
                    "SELECT id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at
                     FROM messages WHERE session_id = $1 ORDER BY created_at ASC, ord ASC",
                    &[&session_id],
                )
                .await?
                .iter()
                .map(map_message_pg)
                .collect())
        })
    }

    fn add_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "INSERT INTO reactions (message_id, bot_id, emoji) VALUES ($1, $2, $3)
                     ON CONFLICT (message_id, bot_id, emoji) DO NOTHING",
                    &[&message_id, &bot_id, &emoji],
                )
                .await?;
            Ok(())
        })
    }

    fn remove_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        self.block(async {
            let client = self.client().await?;
            client
                .execute(
                    "DELETE FROM reactions WHERE message_id = $1 AND bot_id = $2 AND emoji = $3",
                    &[&message_id, &bot_id, &emoji],
                )
                .await?;
            Ok(())
        })
    }

    fn reactions(&self, session_id: &str) -> Result<Vec<Reaction>> {
        self.block(async {
            let client = self.client().await?;
            Ok(client
                .query(
                    "SELECT r.message_id, r.bot_id, r.emoji
                     FROM reactions r
                     JOIN messages m ON m.id = r.message_id
                     WHERE m.session_id = $1
                     ORDER BY m.created_at, r.message_id, r.bot_id, r.emoji",
                    &[&session_id],
                )
                .await?
                .iter()
                .map(|r| Reaction {
                    message_id: r.get(0),
                    bot_id: r.get(1),
                    emoji: r.get(2),
                })
                .collect())
        })
    }

    fn done_voters(&self, session_id: &str) -> Result<Vec<String>> {
        self.block(async {
            let client = self.client().await?;
            // Done-vote invariant: a done reaction counts only on the opening
            // trigger / system prompt, or the voting bot's own message.
            Ok(client
                .query(
                    "SELECT DISTINCT r.bot_id FROM reactions r
                     JOIN messages m ON m.id = r.message_id
                     WHERE m.session_id = $1
                       AND r.emoji = '🆗'
                       AND (m.author_kind IN ('client', 'system') OR m.author_id = r.bot_id)",
                    &[&session_id],
                )
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect())
        })
    }

    fn enqueue_outbox(
        &self,
        bot_id: &str,
        session_id: &str,
        idem_key: &str,
        frame: &str,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn pending_outbox(&self, bot_id: &str) -> Result<Vec<(i64, String)>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn ack_outbox(&self, seq: i64) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn purge_outbox_for_session_bot(&self, session_id: &str, bot_id: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn purge_outbox_for_session(&self, session_id: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn purge_terminal_outbox(&self) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn cache_installation_token(
        &self,
        session_id: &str,
        role: &str,
        token: &str,
        expires_at: i64,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn installation_token(&self, session_id: &str, role: &str) -> Result<Option<(String, i64)>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn session_installation_tokens(&self, session_id: &str) -> Result<Vec<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn purge_installation_tokens(&self, session_id: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn record_compatibility_use(&self, surface: &str, amount: i64) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn compatibility_usage(&self) -> Result<Vec<CompatibilityUsage>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn stats(&self, now: i64) -> Result<Value> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }
}

fn map_bot_pg(r: tokio_postgres::Row) -> Bot {
    Bot {
        id: r.get(0),
        name: r.get(1),
        role: r.get(2),
    }
}

fn map_bot_inventory_pg(r: &tokio_postgres::Row) -> BotInventory {
    BotInventory {
        id: r.get(0),
        name: r.get(1),
        role: r.get(2),
        provider: r.get(3),
        capabilities: parse_capabilities(r.get(4)),
        enabled: r.get::<_, i64>(5) != 0,
        health: r.get(6),
        note: r.get(7),
        version: r.get(8),
        runtime: parse_runtime(r.get(9)),
        last_seen_ms: r.get(10),
        source: r.get(11),
    }
}

/// Skips (loudly) without `TEST_POSTGRES_URL`; each test owns a throwaway
/// schema, dropped and recreated on entry.
#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> Option<PostgresStore> {
        let Ok(url) = std::env::var("TEST_POSTGRES_URL") else {
            eprintln!("TEST_POSTGRES_URL not set; skipping kernel postgres test");
            return None;
        };
        let schema = format!("oab_kernel_{tag}");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
            tokio::spawn(connection);
            client
                .batch_execute(&format!(
                    "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema};"
                ))
                .await
                .unwrap();
        });
        Some(PostgresStore::open_with_options(&url, Some(&schema)).unwrap())
    }

    #[test]
    fn session_dedupe_and_supersede_semantics() {
        let Some(s) = store("sessions_dedupe") else {
            return;
        };
        let roster = vec!["chair".to_string(), "rev".to_string()];
        let (a, deduped) = s
            .create_session_deduped("t", Some("pr#1"), 2, Some("chair"), &roster, "council")
            .unwrap();
        assert!(!deduped);
        let (b, deduped) = s
            .create_session_deduped("t", Some("pr#1"), 2, Some("chair"), &roster, "council")
            .unwrap();
        assert!(deduped, "same active trigger dedupes");
        assert_eq!(a.id, b.id);
        // same fingerprint → dedupe; new fingerprint → supersede
        let (c, outcome) = s
            .create_session_superseding(
                "t",
                Some("pr#1"),
                Some("sha:aaa"),
                2,
                Some("chair"),
                &roster,
                "council",
                &[],
            )
            .unwrap();
        assert!(
            matches!(outcome, SessionCreateOutcome::Superseded { ref old_id } if *old_id == a.id)
        );
        let old = s.session(&a.id).unwrap().unwrap();
        assert_eq!(old.state, "closed");
        let (d, outcome) = s
            .create_session_superseding(
                "t",
                Some("pr#1"),
                Some("sha:aaa"),
                2,
                Some("chair"),
                &roster,
                "council",
                &[],
            )
            .unwrap();
        assert!(matches!(outcome, SessionCreateOutcome::Deduped));
        assert_eq!(c.id, d.id);
    }

    #[test]
    fn roster_keeps_insertion_order_via_ord() {
        let Some(s) = store("roster_order") else {
            return;
        };
        let roster: Vec<String> = ["z-bot", "a-bot", "m-bot"]
            .iter()
            .map(|b| b.to_string())
            .collect();
        let ses = s
            .create_session("t", None, 2, Some("z-bot"), &roster, "council")
            .unwrap();
        assert_eq!(
            s.roster(&ses.id).unwrap(),
            roster,
            "insertion order, not lexical order"
        );
        assert!(s.replace_session_bot(&ses.id, "a-bot", "b-bot").unwrap());
        assert!(s.remove_session_bot(&ses.id, "m-bot").unwrap());
        assert_eq!(s.roster(&ses.id).unwrap(), vec!["z-bot", "b-bot"]);
    }

    #[test]
    fn messages_keep_equal_timestamp_insertion_order() {
        let Some(s) = store("msg_order") else { return };
        let ses = s.create_session("t", None, 1, None, &[], "solo").unwrap();
        for i in 0..5 {
            s.add_message(
                &ses.id,
                None,
                "bot",
                Some("b"),
                None,
                &format!("m{i}"),
                None,
            )
            .unwrap();
        }
        let contents: Vec<String> = s
            .messages(&ses.id)
            .unwrap()
            .into_iter()
            .map(|m| m.content)
            .collect();
        assert_eq!(contents, vec!["m0", "m1", "m2", "m3", "m4"]);
        let first = s.messages(&ses.id).unwrap().remove(0);
        s.edit_message(&first.id, "edited").unwrap();
        assert_eq!(s.message(&first.id).unwrap().unwrap().content, "edited");
    }

    #[test]
    fn state_machine_and_close_with_result() {
        let Some(s) = store("state_machine") else {
            return;
        };
        let ses = s
            .create_session("t", None, 1, None, &[], "council")
            .unwrap();
        assert!(s
            .advance_state(&ses.id, SessionState::Open, SessionState::Deliberating)
            .unwrap());
        assert!(
            !s.advance_state(&ses.id, SessionState::Open, SessionState::Deliberating)
                .unwrap(),
            "stale from-state must not transition"
        );
        assert!(s
            .close_session_with_result(
                &ses.id,
                SessionState::Deliberating,
                Some("approve"),
                Some(0),
                Some(1),
                Some(2),
                Some("chair"),
                None,
                &["report".to_string()],
            )
            .unwrap());
        let closed = s.session(&ses.id).unwrap().unwrap();
        assert_eq!(closed.state, "closed");
        assert_eq!(closed.decision.as_deref(), Some("approve"));
        assert!(closed.closed_at.is_some());
        assert!(s.reopen_session(&ses.id, SessionState::Closed).unwrap());
        assert_eq!(s.session(&ses.id).unwrap().unwrap().state, "deliberating");
    }

    #[test]
    fn reactions_and_done_voters_invariant() {
        let Some(s) = store("done_voters") else {
            return;
        };
        let ses = s
            .create_session("t", None, 2, None, &[], "council")
            .unwrap();
        let opening = s
            .add_message(&ses.id, None, "client", None, None, "prompt", None)
            .unwrap();
        let peer = s
            .add_message(&ses.id, None, "bot", Some("rev-a"), None, "peer msg", None)
            .unwrap();
        s.add_reaction(&opening.id, "rev-a", "🆗").unwrap();
        s.add_reaction(&opening.id, "rev-a", "🆗").unwrap(); // idempotent
        s.add_reaction(&peer.id, "rev-b", "🆗").unwrap(); // peer's message — not a vote
        s.add_reaction(&peer.id, "rev-a", "🆗").unwrap(); // own message — counts
        let mut voters = s.done_voters(&ses.id).unwrap();
        voters.sort();
        assert_eq!(voters, vec!["rev-a"]);
        assert_eq!(s.reactions(&ses.id).unwrap().len(), 3);
        s.remove_reaction(&opening.id, "rev-a", "🆗").unwrap();
        assert_eq!(s.reactions(&ses.id).unwrap().len(), 2);
    }

    #[test]
    fn capacity_gate_and_watchdog_scan() {
        let Some(s) = store("capacity") else { return };
        let ses = s
            .create_session("t", Some("pr#9"), 2, None, &["a".into()], "council")
            .unwrap();
        let outcome = s
            .add_session_bots_if_capacity(&ses.id, &["b".into(), "c".into()], 2, &[])
            .unwrap();
        assert!(matches!(outcome, RosterAddOutcome::Full));
        let outcome = s
            .add_session_bots_if_capacity(&ses.id, &["b".into(), "a".into()], 3, &[])
            .unwrap();
        match outcome {
            RosterAddOutcome::Added {
                added,
                already_members,
            } => {
                assert_eq!(added, vec!["b"]);
                assert_eq!(already_members, vec!["a"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(
            s.latest_session_created_at("pr#9").unwrap(),
            Some(ses.created_at)
        );
        // no messages yet → activity anchor = created_at → stale
        let stale = s.active_sessions_before(now_ms() + 1).unwrap();
        assert!(stale.contains(&ses.id));
        s.add_message(&ses.id, None, "bot", Some("a"), None, "alive", None)
            .unwrap();
        let stale = s.active_sessions_before(ses.created_at + 1).unwrap();
        assert!(
            !stale.contains(&ses.id),
            "fresh message defers the deadline"
        );
    }

    #[test]
    fn bot_identity_seed_register_and_token_lookup() {
        let Some(s) = store("bots_identity") else {
            return;
        };
        assert!(s
            .seed_bot("chair", "Chair", "chair", "hash-1", "tok-1")
            .unwrap());
        assert!(
            !s.seed_bot("chair", "Chair", "chair", "other", "tok-x")
                .unwrap(),
            "seeding an existing id is a no-op"
        );
        let bot = s
            .register_bot("rev", "reviewer", "hash-2", "tok-2")
            .unwrap();
        assert!(bot.id.starts_with("bot"));
        assert_eq!(s.bot_by_token_hash("hash-1").unwrap().unwrap().id, "chair");
        assert_eq!(s.bot("chair").unwrap().unwrap().role, "chair");
        assert_eq!(
            s.bot_token_plain("chair").unwrap().as_deref(),
            Some("tok-1")
        );
        let names = s.bots_with_plaintext_token().unwrap();
        assert!(names.contains(&"Chair".to_string()));
    }

    #[test]
    fn last_seen_is_monotonic() {
        let Some(s) = store("last_seen") else { return };
        s.seed_bot("b", "B", "reviewer", "h", "t").unwrap();
        s.touch_last_seen_at("b", 2_000).unwrap();
        s.touch_last_seen_at("b", 1_000).unwrap(); // stale clock must not clobber
        let inv = s.bot_inventory("b").unwrap().unwrap();
        assert_eq!(inv.last_seen_ms, Some(2_000));
    }

    #[test]
    fn record_bot_frame_degrades_once_and_recovers() {
        let Some(s) = store("bot_frames") else { return };
        s.seed_bot("b", "B", "reviewer", "h", "t").unwrap();
        assert!(matches!(
            s.record_bot_frame("b", true, 2).unwrap(),
            BotHealthTransition::None
        ));
        assert!(matches!(
            s.record_bot_frame("b", true, 2).unwrap(),
            BotHealthTransition::Degraded
        ));
        assert!(
            matches!(
                s.record_bot_frame("b", true, 2).unwrap(),
                BotHealthTransition::None,
            ),
            "already degraded does not re-fire"
        );
        assert!(matches!(
            s.record_bot_frame("b", false, 2).unwrap(),
            BotHealthTransition::Recovered
        ));
        assert!(matches!(
            s.record_bot_frame("missing", true, 2).unwrap(),
            BotHealthTransition::None
        ));
    }

    #[test]
    fn standing_roster_and_mark_once_round_trip() {
        let Some(s) = store("settings") else { return };
        assert_eq!(s.standing_roster().unwrap(), None);
        s.set_standing_roster(&["chair".into(), "rev".into()])
            .unwrap();
        assert_eq!(
            s.standing_roster().unwrap(),
            Some(vec!["chair".to_string(), "rev".to_string()])
        );
        assert!(s.mark_once("migration-x").unwrap());
        assert!(
            !s.mark_once("migration-x").unwrap(),
            "second mark is a no-op"
        );
    }

    #[test]
    fn discover_bot_inserts_then_upserts_metadata() {
        let Some(s) = store("discover") else { return };
        let meta = BotMetadata {
            provider: Some("claude".into()),
            capabilities: Some(vec!["review".into()]),
            version: Some("1.0".into()),
            runtime: None,
        };
        let (bot, inserted) = s.discover_bot("d1", Some("D1"), "reviewer", &meta).unwrap();
        assert!(inserted);
        assert_eq!(bot.name, "D1");
        let meta2 = BotMetadata {
            provider: None,
            capabilities: None,
            version: Some("1.1".into()),
            runtime: None,
        };
        let (_, inserted) = s.discover_bot("d1", None, "reviewer", &meta2).unwrap();
        assert!(!inserted);
        let inv = s.bot_inventory("d1").unwrap().unwrap();
        assert_eq!(
            inv.provider.as_deref(),
            Some("claude"),
            "None must not clobber"
        );
        assert_eq!(inv.version.as_deref(), Some("1.1"));
        assert_eq!(inv.source, "discovered");
    }
}

fn is_pg_unique_violation(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(|e| e.code())
            .is_some_and(|c| *c == tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    })
}

fn map_session_pg(r: &tokio_postgres::Row) -> Session {
    Session {
        id: r.get(0),
        title: r.get(1),
        state: r.get(2),
        trigger_ref: r.get(3),
        trigger_fingerprint: r.get(4),
        quorum_n: r.get(5),
        chair_bot: r.get(6),
        created_at: r.get(7),
        closed_at: r.get(8),
        mode: r.get(9),
        decision: r.get(10),
        findings_red: r.get(11),
        findings_yellow: r.get(12),
        findings_green: r.get(13),
        result_author_id: r.get(14),
        result_message_ids: r.get(15),
    }
}

const SESSION_COLS: &str = "id, title, state, trigger_ref, trigger_fingerprint, quorum_n, chair_bot, created_at, closed_at, mode, decision, findings_red, findings_yellow, findings_green, result_author_id, result_message_ids";

async fn active_session_for_trigger_pg<G: tokio_postgres::GenericClient>(
    g: &G,
    trigger_ref: &str,
) -> Result<Option<Session>> {
    Ok(g.query_opt(
        &format!(
            "SELECT {SESSION_COLS} FROM sessions
              WHERE trigger_ref = $1 AND state NOT IN ('closed', 'aborted')
              ORDER BY created_at DESC, id DESC LIMIT 1"
        ),
        &[&trigger_ref],
    )
    .await?
    .as_ref()
    .map(map_session_pg))
}

async fn enqueue_controller_event_pg<G: tokio_postgres::GenericClient>(
    g: &G,
    controller_id: &str,
    session_id: Option<&str>,
    event_type: &str,
    payload: Value,
    idempotency_key: &str,
    occurred_at: i64,
) -> Result<()> {
    let destination = g
        .query_opt(
            "SELECT c.event_endpoint, c.event_key_version
             FROM controllers c
             JOIN controller_event_grants g ON g.controller_id = c.id
             WHERE c.id = $1 AND c.enabled = 1
               AND c.event_endpoint IS NOT NULL AND c.event_key_version IS NOT NULL
               AND g.event_type = $2 AND g.granted = 1",
            &[&controller_id, &event_type],
        )
        .await?
        .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)));
    let Some((event_endpoint, event_key_version)) = destination else {
        return Ok(());
    };
    let event_id = new_id("cev");
    let body_json = serde_json::to_string(&json!({
        "version": "1",
        "event_id": event_id,
        "controller_id": controller_id,
        "event_type": event_type,
        "session_id": session_id,
        "occurred_at": occurred_at,
        "payload": payload,
    }))?;
    g.execute(
        "INSERT INTO controller_events
            (id, controller_id, session_id, event_type, event_endpoint, event_key_version,
             body_json, idempotency_key, state, attempts, created_at, next_attempt_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending', 0, $9, $9)
         ON CONFLICT (controller_id, idempotency_key) DO NOTHING",
        &[
            &event_id,
            &controller_id,
            &session_id,
            &event_type,
            &event_endpoint,
            &event_key_version,
            &body_json,
            &idempotency_key,
            &occurred_at,
        ],
    )
    .await?;
    Ok(())
}

async fn enqueue_controller_session_event_pg<G: tokio_postgres::GenericClient>(
    g: &G,
    session_id: &str,
    event_type: &str,
    payload: Value,
    idempotency_key: &str,
    occurred_at: i64,
) -> Result<()> {
    let controller_id: Option<String> = g
        .query_opt(
            "SELECT controller_id FROM controller_sessions WHERE session_id = $1",
            &[&session_id],
        )
        .await?
        .map(|r| r.get(0));
    if let Some(controller_id) = controller_id {
        enqueue_controller_event_pg(
            g,
            &controller_id,
            Some(session_id),
            event_type,
            payload,
            idempotency_key,
            occurred_at,
        )
        .await?;
    }
    Ok(())
}

async fn insert_opening_input_pg<G: tokio_postgres::GenericClient>(
    g: &G,
    session_id: &str,
    input: &OpeningInput,
    created_at: i64,
) -> Result<()> {
    g.execute(
        "INSERT INTO messages
            (id, session_id, thread_id, author_kind, author_id, audience, content, reply_to, created_at)
         VALUES ($1, $2, NULL, 'client', NULL, $3, $4, NULL, $5)",
        &[
            &new_id("msg"),
            &session_id,
            &input.recipient,
            &input.content,
            &created_at,
        ],
    )
    .await?;
    Ok(())
}

fn map_message_pg(r: &tokio_postgres::Row) -> Message {
    Message {
        id: r.get(0),
        session_id: r.get(1),
        thread_id: r.get(2),
        author_kind: r.get(3),
        author_id: r.get(4),
        audience: r.get(5),
        content: r.get(6),
        reply_to: r.get(7),
        created_at: r.get(8),
    }
}
