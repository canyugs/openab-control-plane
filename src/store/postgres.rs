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
        let runtime = std::sync::Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("pg-store")
                .enable_all()
                .build()?,
        );
        let mut config: tokio_postgres::Config = url.parse()?;
        let _unused = &mut config;
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
    PRIMARY KEY (session_id, bot_id)
);
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL UNIQUE, root_message_id TEXT
);
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY, session_id TEXT NOT NULL, thread_id TEXT,
    author_kind TEXT NOT NULL, author_id TEXT, audience TEXT, content TEXT NOT NULL,
    reply_to TEXT, created_at BIGINT NOT NULL
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
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn seed_bot(
        &self,
        id: &str,
        name: &str,
        role: &str,
        token_hash: &str,
        token_plain: &str,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn bot_by_token_hash(&self, token_hash: &str) -> Result<Option<Bot>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn bot(&self, id: &str) -> Result<Option<Bot>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn bot_token_plain(&self, id: &str) -> Result<Option<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn bots_with_plaintext_token(&self) -> Result<Vec<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn touch_last_seen(&self, bot_id: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn touch_last_seen_at(&self, bot_id: &str, ts: i64) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn list_bots(&self) -> Result<Vec<BotInventory>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn bot_inventory(&self, id: &str) -> Result<Option<BotInventory>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn discover_bot(
        &self,
        id: &str,
        name: Option<&str>,
        role: &str,
        metadata: &BotMetadata,
    ) -> Result<(Bot, bool)> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn update_bot_metadata(&self, id: &str, patch: &BotMetadataPatch) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn delete_bot(&self, bot_id: &str) -> Result<DeleteBotOutcome> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn record_bot_frame(
        &self,
        bot_id: &str,
        is_error: bool,
        threshold: i64,
    ) -> Result<BotHealthTransition> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    #[allow(clippy::too_many_arguments)]
    fn create_session(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<Session> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    #[allow(clippy::too_many_arguments)]
    fn create_session_deduped(
        &self,
        title: &str,
        trigger_ref: Option<&str>,
        quorum_n: i64,
        chair_bot: Option<&str>,
        roster: &[String],
        mode: &str,
    ) -> Result<(Session, bool)> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

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
        opening_inputs: &[OpeningInput],
    ) -> Result<(Session, SessionCreateOutcome)> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn session(&self, id: &str) -> Result<Option<Session>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn list_sessions(
        &self,
        trigger_ref: Option<&str>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Session>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn add_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn add_session_bots_if_capacity(
        &self,
        session_id: &str,
        bot_ids: &[String],
        max_roster: usize,
        opening_inputs: &[OpeningInput],
    ) -> Result<RosterAddOutcome> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn replace_session_bot(
        &self,
        session_id: &str,
        old_bot_id: &str,
        new_bot_id: &str,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn remove_session_bot(&self, session_id: &str, bot_id: &str) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_session_quorum(&self, session_id: &str, quorum_n: i64) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_session_chair(&self, session_id: &str, chair_bot: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_state(&self, session_id: &str, state: SessionState) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn advance_state(
        &self,
        session_id: &str,
        from: SessionState,
        to: SessionState,
    ) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn close_if_active(&self, session_id: &str, event_type: &str, reason: &str) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_session_verdict(
        &self,
        session_id: &str,
        decision: &str,
        red: Option<i64>,
        yellow: Option<i64>,
        green: Option<i64>,
    ) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
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
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn reopen_session(&self, session_id: &str, from: SessionState) -> Result<bool> {
        unimplemented!("ADR 033 phase 2: not yet ported")
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
        unimplemented!("ADR 033 phase 2: not yet ported")
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
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn active_sessions_before(&self, cutoff_ms: i64) -> Result<Vec<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn roster(&self, session_id: &str) -> Result<Vec<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn active_session_for_trigger(&self, trigger_ref: &str) -> Result<Option<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn standing_roster(&self) -> Result<Option<Vec<String>>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn set_standing_roster(&self, roster: &[String]) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn upsert_thread(&self, session_id: &str, root_message_id: Option<&str>) -> Result<String> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn thread_for_session(&self, session_id: &str) -> Result<Option<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
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
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn edit_message(&self, message_id: &str, content: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn message(&self, id: &str) -> Result<Option<Message>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn messages(&self, session_id: &str) -> Result<Vec<Message>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn add_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn remove_reaction(&self, message_id: &str, bot_id: &str, emoji: &str) -> Result<()> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn reactions(&self, session_id: &str) -> Result<Vec<Reaction>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
    }

    fn done_voters(&self, session_id: &str) -> Result<Vec<String>> {
        unimplemented!("ADR 033 phase 2: not yet ported")
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
