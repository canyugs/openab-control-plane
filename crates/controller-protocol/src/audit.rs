//! Shared first-party investigation-journal contract (ADR 036).
//!
//! This module deliberately contains only serialized data and query cursors.
//! It has no storage, transport, provider, or runtime dependencies, so the OCP
//! kernel and external controllers can use the same envelope without sharing a
//! database or importing one another's product schemas.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const AUDIT_EVENT_VERSION: u16 = 1;
pub const DEFAULT_PAGE_LIMIT: usize = 100;
pub const MAX_PAGE_LIMIT: usize = 500;
pub const DEFAULT_RETENTION_DAYS: i64 = 90;
pub const EXTENDED_RETENTION_DAYS: i64 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Pending,
    Accepted,
    Ignored,
    Denied,
    Succeeded,
    Failed,
    RetryScheduled,
    OutcomeUnknown,
    Reconciled,
}

impl AuditOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Ignored => "ignored",
            Self::Denied => "denied",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::RetryScheduled => "retry_scheduled",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Reconciled => "reconciled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "accepted" => Self::Accepted,
            "ignored" => Self::Ignored,
            "denied" => Self::Denied,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "retry_scheduled" => Self::RetryScheduled,
            "outcome_unknown" => Self::OutcomeUnknown,
            "reconciled" => Self::Reconciled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditCorrelation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditActor {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditTarget {
    pub kind: String,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditError {
    pub class: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub version: u16,
    pub event_id: String,
    pub event_key: String,
    pub occurred_at: i64,
    pub recorded_at: i64,
    pub service: String,
    pub kind: String,
    pub outcome: AuditOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<String>,
    #[serde(default)]
    pub correlation: AuditCorrelation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<AuditActor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<AuditTarget>,
    #[serde(default)]
    pub detail: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AuditError>,
}

impl AuditEvent {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != AUDIT_EVENT_VERSION {
            return Err(format!("unsupported audit event version {}", self.version));
        }
        for (name, value) in [
            ("event_id", self.event_id.as_str()),
            ("event_key", self.event_key.as_str()),
            ("service", self.service.as_str()),
            ("kind", self.kind.as_str()),
        ] {
            if value.is_empty() || value.len() > 256 {
                return Err(format!("audit {name} must be 1..=256 bytes"));
            }
        }
        if self.occurred_at < 0 || self.recorded_at < 0 {
            return Err("audit timestamps must be non-negative unix milliseconds".into());
        }
        if self.kind.trim() != self.kind || self.event_key.trim() != self.event_key {
            return Err("audit kind and event_key must not have surrounding whitespace".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEventRecord {
    pub seq: i64,
    #[serde(flatten)]
    pub event: AuditEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditCursor {
    pub recorded_at: i64,
    pub seq: i64,
}

impl AuditCursor {
    pub fn encode(self) -> String {
        format!("{}:{}", self.recorded_at, self.seq)
    }

    pub fn decode(value: &str) -> Option<Self> {
        let (recorded_at, seq) = value.split_once(':')?;
        Some(Self {
            recorded_at: recorded_at.parse().ok()?,
            seq: seq.parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditEventQuery {
    pub delivery_id: Option<String>,
    pub controller_id: Option<String>,
    pub action_id: Option<String>,
    pub runtime_event_id: Option<String>,
    pub session_id: Option<String>,
    pub message_id: Option<String>,
    pub write_id: Option<String>,
    pub trigger_ref: Option<String>,
    pub kind: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub cursor: Option<AuditCursor>,
    pub limit: usize,
}

impl AuditEventQuery {
    pub fn bounded_limit(&self) -> usize {
        match self.limit {
            0 => DEFAULT_PAGE_LIMIT,
            limit => limit.min(MAX_PAGE_LIMIT),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEventPage {
    pub events: Vec<AuditEventRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
