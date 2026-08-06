//! The controller's durable state, behind one boundary (ADR 033).
//!
//! `ProductStore` is the trait the rest of the controller programs against.
//! Two backends implement it: [`sqlite::SqliteStore`] (the original, still the
//! default — tests and local development stay on it) and
//! [`postgres::PostgresStore`] (production, per ADR 033). The backend is
//! chosen by the shape of `GITHUB_CONTROLLER_DB`: a `postgres://` /
//! `postgresql://` URL opens Postgres, anything else is a SQLite path.

pub mod postgres;
pub mod sqlite;

/// `wvr_` + 128 random bits, hex. The id doubles as the bump capability, so
/// it must be unguessable; matches the kernel's format so migrated ids and
/// new ones are indistinguishable to the chair.
pub(crate) fn new_waiver_id() -> String {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).expect("os rng");
    format!("wvr_{}", hex::encode(raw))
}

use controller_protocol::audit::{
    AuditCorrelation, AuditError, AuditEvent, AuditEventPage, AuditEventQuery, AuditEventRecord,
    AuditOutcome, AUDIT_EVENT_VERSION,
};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub use postgres::PostgresStore;
pub use sqlite::SqliteStore;

pub(crate) const PROCESSING_LEASE_SECS: i64 = 5 * 60;
pub(crate) const COMPLETED_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
/// A write that keeps failing stops being retried. The drain is not the place
/// to decide a GitHub outage is permanent; a human reading `last_error` is.
pub(crate) const WRITE_MAX_ATTEMPTS: i64 = 5;
/// How long a claimed write may stay in flight before another drain may take
/// it. Only reached when the process dies mid-send; the same lease idiom the
/// delivery table already uses.
pub(crate) const WRITE_CLAIM_LEASE_SECS: i64 = 5 * 60;

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// The controller's existing product tables use Unix seconds, while the
/// shared investigation contract uses Unix milliseconds. Keep the conversion
/// at this boundary so legacy store timestamps cannot leak into the journal.
pub(crate) fn audit_timestamp(value: i64) -> i64 {
    if value < 10_000_000_000 {
        value.saturating_mul(1_000)
    } else {
        value
    }
}

pub(crate) fn new_audit_event(
    event_key: impl Into<String>,
    kind: &str,
    outcome: AuditOutcome,
    occurred_at: i64,
    correlation: AuditCorrelation,
    detail: Value,
    error: Option<AuditError>,
) -> AuditEvent {
    let event_key = event_key.into();
    AuditEvent {
        version: AUDIT_EVENT_VERSION,
        event_id: format!("aud:github-pr-controller:{event_key}"),
        event_key,
        occurred_at: audit_timestamp(occurred_at),
        recorded_at: now_ms(),
        service: "github-pr-controller".into(),
        kind: kind.into(),
        outcome,
        caused_by: None,
        correlation,
        actor: None,
        target: None,
        detail,
        error,
    }
}

/// One error type across backends so callers stay driver-agnostic.
#[derive(Debug)]
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Postgres(tokio_postgres::Error),
    /// Pool checkout / configuration failures that are not a query error.
    Pool(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(error) => write!(f, "sqlite: {error}"),
            StoreError::Postgres(error) => write!(f, "postgres: {error}"),
            StoreError::Pool(error) => write!(f, "postgres pool: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Sqlite(error)
    }
}

impl From<tokio_postgres::Error> for StoreError {
    fn from(error: tokio_postgres::Error) -> Self {
        StoreError::Postgres(error)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

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

/// The provider object a session belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTarget {
    pub repo: String,
    pub pr_number: i64,
    pub head_sha: Option<String>,
}

/// One council round as the controller knows it: the verdict it parsed out of
/// a terminal event, keyed to the session that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRound {
    pub repo: String,
    pub pr_number: i64,
    pub session_id: String,
    pub head_sha: Option<String>,
    pub decision: String,
    pub red: i64,
    pub yellow: i64,
    pub green: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedRound {
    pub id: i64,
    pub round: i64,
    /// False when a redelivered terminal event found the round already there —
    /// the caller must not re-enqueue side effects on the strength of it.
    pub first_time: bool,
}

/// A stored finding as an operator reads it back: the write struct plus the
/// identity the controller supplies at insert time. Field names and types
/// mirror the kernel's `ReviewFinding` exactly, because the ops reporting
/// scripts were written against that JSON and this is where they move to
/// (SEI-895 — the kernel's copy has NULL repo/pr for every controller-opened
/// session, since only the deleted webhook produced a parseable trigger_ref).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewFindingRow {
    pub id: i64,
    pub session_id: String,
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    pub stable_id: String,
    pub severity: String,
    pub status: String,
    pub title: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub raised_by: Option<String>,
    pub angle: Option<String>,
    pub head_sha: Option<String>,
    pub created_at: i64,
    /// ADR 038 — who judged this finding, why, and when. None for a finding
    /// the council alone has touched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<i64>,
}

/// An ADR 035 waiver: an operator-recorded accepted trade-off, injected into
/// the chair's opening input for its repo and bumped when a round waives a
/// matching finding. Ported from the kernel (SEI-895 item 5) — the controller
/// owns the repo, which restores repo scoping the kernel had lost.
/// Timestamps are unix SECONDS (controller convention; the kernel used ms).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewWaiver {
    pub id: String,
    pub repo: String,
    pub path_class: Option<String>,
    pub text: String,
    pub origin_pr: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub fired_count: i64,
    pub last_fired_at: Option<i64>,
}

/// Filters for [`ProductStore::review_findings`]. Every field is optional and
/// ANDed; `limit` is clamped by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewFindingQuery {
    pub repo: Option<String>,
    pub pr_number: Option<i64>,
    pub status: Option<String>,
    pub severity: Option<String>,
    pub limit: usize,
}

/// Port of the kernel's `pr_review_findings` row, same columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFinding {
    pub stable_id: String,
    pub severity: String,
    pub status: String,
    pub title: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub raised_by: Option<String>,
    pub angle: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingWrite {
    pub id: i64,
    pub session_id: String,
    pub kind: String,
    pub payload: Value,
    pub attempts: i64,
    /// True when a previous process claimed the write and its lease expired
    /// before the local completion state was recorded.
    pub was_reclaimed: bool,
}

/// The store boundary (ADR 033 step zero). Semantics are the SQLite
/// implementation's, which carried production to the cutover; the Postgres
/// backend reproduces them (documented per method there where the idiom
/// differs).
#[async_trait::async_trait]
pub trait ProductStore: Send + Sync {
    async fn begin_delivery(
        &self,
        delivery_id: &str,
        event_type: &str,
        repository: Option<&str>,
        payload_sha256: &str,
    ) -> StoreResult<DeliveryAdmission>;

    async fn finish_delivery(
        &self,
        delivery_id: &str,
        state: &str,
        result: &Value,
    ) -> StoreResult<()>;

    async fn release_delivery_for_retry(
        &self,
        delivery_id: &str,
        result: &Value,
    ) -> StoreResult<()>;

    async fn prune_completed_deliveries(&self) -> StoreResult<usize>;

    async fn record_shadow_comparison(
        &self,
        request_sha256: &str,
        repository: Option<&str>,
        report: &crate::shadow::ShadowReport,
    ) -> StoreResult<ShadowAdmission>;

    async fn shadow_summary(&self) -> StoreResult<ShadowSummary>;

    async fn record_runtime_event(
        &self,
        body_sha256: &str,
        event: &crate::runtime_events::RuntimeEventEnvelope,
    ) -> StoreResult<RuntimeEventAdmission>;

    async fn canary_summary(&self) -> StoreResult<CanarySummary>;

    /// The round a session opened now would be: one past the highest this
    /// controller has closed for the pull request.
    async fn next_round(&self, repo: &str, pr_number: i64) -> StoreResult<i64>;

    /// Remember what a session we just opened is about. Idempotent: a
    /// redelivered webhook that re-opens the same session must not conflict.
    async fn record_session_target(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
    ) -> StoreResult<()>;

    /// `None` means this controller did not open the session — the terminal
    /// event belongs to someone else and must not be acted on.
    async fn session_target(&self, session_id: &str) -> StoreResult<Option<SessionTarget>>;

    /// Open (or recover) the round for a closed session. Idempotent: a
    /// redelivered terminal event returns the round already recorded rather
    /// than opening a second one.
    async fn record_review_round(&self, round: &ReviewRound) -> StoreResult<RecordedRound>;

    /// The comment id to PATCH on the next round of the same pull request.
    /// Carried forward from the newest round that has one.
    async fn last_comment_id(&self, repo: &str, pr_number: i64) -> StoreResult<Option<i64>>;

    /// The comment this session's round posted — the review timeline entry
    /// links to it.
    async fn round_comment_id(&self, session_id: &str) -> StoreResult<Option<i64>>;

    async fn set_round_comment_id(&self, session_id: &str, comment_id: i64) -> StoreResult<()>;

    /// Append a session's findings. Idempotent by session: a redelivery adds
    /// nothing, since the ledger is append-only and would otherwise double.
    async fn record_review_findings(
        &self,
        session_id: &str,
        repo: &str,
        pr_number: i64,
        head_sha: Option<&str>,
        findings: &[ReviewFinding],
    ) -> StoreResult<usize>;

    /// Record an author's judgement on one finding (ADR 038).
    ///
    /// Scoped by `head_sha` as well as `(repo, pr, stable_id)`: a decision is
    /// about the revision the finding was raised on, so one taken against a
    /// head that has since moved must not land on the newer round's finding of
    /// the same name. Returns the row as it now stands, or None when nothing
    /// matched — the caller turns that into a reply, never a silent miss.
    async fn decide_review_finding(
        &self,
        repo: &str,
        pr_number: i64,
        stable_id: &str,
        head_sha: &str,
        status: &str,
        decided_by: &str,
        reason: Option<&str>,
    ) -> StoreResult<Option<ReviewFindingRow>>;

    /// Read the findings ledger back, newest first. The reporting surface the
    /// ops scripts join against.
    async fn review_findings(&self, query: &ReviewFindingQuery)
        -> StoreResult<Vec<ReviewFindingRow>>;

    /// ADR 035 P1: record an operator-accepted trade-off. The id is minted
    /// here (`wvr_` + 128-bit random) — it is a capability, never guessable.
    async fn create_review_waiver(
        &self,
        repo: &str,
        path_class: Option<&str>,
        text: &str,
        origin_pr: Option<&str>,
        created_by: &str,
        expires_at: i64,
    ) -> StoreResult<ReviewWaiver>;

    /// Active waivers for a repo (unexpired, unrevoked), or everything when
    /// `include_inactive`. Ordered oldest first, like the kernel did.
    async fn list_review_waivers(
        &self,
        repo: Option<&str>,
        include_inactive: bool,
        now: i64,
    ) -> StoreResult<Vec<ReviewWaiver>>;

    /// Extend or revoke. Returns false for an unknown id.
    async fn update_review_waiver(
        &self,
        id: &str,
        expires_at: Option<i64>,
        revoke: bool,
    ) -> StoreResult<bool>;

    /// ADR 035 P2: bump fired counters for waivers a closed round matched.
    /// Repo-scoped — a chair cannot bump another repo's ledger, the guarantee
    /// the kernel lost when trigger_refs went opaque.
    async fn record_waiver_fired(&self, repo: &str, ids: &[String]) -> StoreResult<usize>;

    /// Queue a GitHub write. Returns false when this (session, kind) is
    /// already queued — the guarantee that at-least-once delivery cannot
    /// produce a second comment, status or review.
    async fn enqueue_write(
        &self,
        session_id: &str,
        kind: &str,
        payload: &Value,
    ) -> StoreResult<bool>;

    /// Take ownership of up to `limit` queued writes.
    ///
    /// Claiming is what stops two concurrent drains from both sending the same
    /// comment — `create_comment` and `submit_review` are not idempotent, so a
    /// double send is a double post. A claim abandoned by a dying process
    /// becomes available again after `WRITE_CLAIM_LEASE_SECS`.
    async fn claim_writes(&self, limit: i64) -> StoreResult<Vec<PendingWrite>>;

    /// Claim rows as if every lease had lapsed — the crash-replay test's way
    /// of fast-forwarding time. Test-only; not part of the product surface.
    async fn claim_writes_for_test_after_lease(&self, limit: i64)
        -> StoreResult<Vec<PendingWrite>>;

    /// Queued-but-unsent writes, claimed or not. For inspection and tests —
    /// sending goes through `claim_writes`.
    async fn pending_writes(&self, limit: i64) -> StoreResult<Vec<PendingWrite>>;

    async fn mark_write_done(&self, id: i64) -> StoreResult<()>;

    /// Count the attempt and keep the row retryable until it has burned
    /// through `WRITE_MAX_ATTEMPTS`, then park it as `failed`.
    async fn mark_write_failed(&self, id: i64, error: &str) -> StoreResult<()>;

    /// Append/query the provider-controller half of the ADR 036 investigation
    /// journal. `(service, event_key)` is the retry-safe idempotency boundary.
    async fn append_audit_event(&self, event: &AuditEvent) -> StoreResult<AuditEventRecord>;
    async fn audit_events(&self, query: &AuditEventQuery) -> StoreResult<AuditEventPage>;
    async fn prune_audit_events(&self, before: i64, extended_before: i64) -> StoreResult<usize>;
}

/// True when the configured database location selects the Postgres backend.
pub fn is_postgres_url(db: &str) -> bool {
    db.starts_with("postgres://") || db.starts_with("postgresql://")
}

/// Open the backend `GITHUB_CONTROLLER_DB` selects: a `postgres://` URL opens
/// [`PostgresStore`], any other value is a SQLite path — the original,
/// unchanged default.
pub async fn open_product_store(db: &str) -> StoreResult<Arc<dyn ProductStore>> {
    if is_postgres_url(db) {
        Ok(Arc::new(PostgresStore::open(db).await?))
    } else {
        Ok(Arc::new(SqliteStore::open(db)?))
    }
}
