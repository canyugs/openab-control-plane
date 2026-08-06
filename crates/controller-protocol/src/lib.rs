#![forbid(unsafe_code)]

//! Provider-neutral data contract between an external controller and the
//! OpenAB control-plane runtime.
//!
//! This crate contains serialized data only. It intentionally has no runtime,
//! storage, transport, or product integration dependencies.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod audit;

pub const CURRENT_VERSION: u16 = 1;
pub const SUPPORTED_VERSIONS: &[u16] = &[CURRENT_VERSION];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionOffer {
    pub supported_versions: Vec<u16>,
}

impl Default for VersionOffer {
    fn default() -> Self {
        Self {
            supported_versions: SUPPORTED_VERSIONS.to_vec(),
        }
    }
}

/// Select the highest mutually supported version.
pub fn negotiate_version(peer: &VersionOffer) -> Option<u16> {
    highest_mutual_version(SUPPORTED_VERSIONS, peer)
}

fn highest_mutual_version(supported: &[u16], peer: &VersionOffer) -> Option<u16> {
    supported
        .iter()
        .copied()
        .filter(|version| peer.supported_versions.contains(version))
        .max()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionEnvelope {
    pub version: u16,
    pub action_id: String,
    pub action: ControllerAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "params", rename_all = "snake_case")]
pub enum ControllerAction {
    OpenSession(OpenSessionAction),
    PostMessage(PostMessageAction),
    AddRoster(AddRosterAction),
    CloseSession(CloseSessionAction),
    EmitStatus(EmitStatusAction),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSessionAction {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_fingerprint: Option<String>,
    pub roster: Vec<String>,
    pub quorum_n: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chair_bot: Option<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// Target-specific opening inputs. P2 defines the contract; the runtime
    /// adapter rejects non-empty values until P3 can land them atomically.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipient_inputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMessageAction {
    pub session_id: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddRosterAction {
    pub session_id: String,
    pub bots: Vec<String>,
    /// Target-specific opening inputs for newly added members. A controller
    /// must supply these when extending a session whose client context is
    /// entirely audience-scoped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipient_inputs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseSessionAction {
    pub session_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmitStatusAction {
    pub session_id: String,
    pub target: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResultEnvelope {
    pub version: u16,
    pub action_id: String,
    pub result: ControllerActionResult,
}

/// Durable execution state returned by the controller reconciliation surface.
/// `outcome_unknown` is intentionally terminal for the original action id: a
/// controller must inspect the attached session projection before deciding
/// whether a new action is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerActionExecutionState {
    Processing,
    Completed,
    OutcomeUnknown,
}

/// The kernel-owned settled result. This is deliberately narrower than the
/// root north session detail: controllers can recover their own result without
/// gaining transcript, roster, or operator visibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerSessionResult {
    pub author_id: String,
    pub message_ids: Vec<String>,
    pub text: String,
}

/// Minimal provider-neutral session projection needed to reconcile an action
/// response or a lost runtime event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerSessionReconciliation {
    pub session_id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ControllerSessionResult>,
}

/// Read-only outcome for one action owned by the authenticated installation
/// and exact scope. `response` is the original stored action response when the
/// action completed; `session` is the current kernel projection when one can be
/// identified, including after an indeterminate `open_session` crash window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerActionReconciliation {
    pub version: u16,
    pub action_id: String,
    pub action_kind: String,
    pub scope: String,
    pub state: ControllerActionExecutionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ControllerSessionReconciliation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ControllerActionResult {
    SessionOpened {
        session_id: String,
        deduped: bool,
    },
    Superseded {
        session_id: String,
        old_id: String,
    },
    MessagePosted {
        message_id: String,
    },
    RosterAdded {
        session_id: String,
        added: Vec<String>,
        already_members: Vec<String>,
    },
    SessionClosed {
        session_id: String,
        closed: bool,
    },
    StatusEmitted {
        session_id: String,
        status_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    pub error: ProtocolError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    Unauthorized,
    Forbidden,
    NotFound,
    Gone,
    Conflict,
    RateLimited,
    Internal,
}

impl ErrorEnvelope {
    pub fn unsupported_version(action_id: Option<String>, requested: u16) -> Self {
        Self {
            version: CURRENT_VERSION,
            action_id,
            error: ProtocolError {
                code: ErrorCode::UnsupportedVersion,
                message: format!(
                    "unsupported protocol version {requested}; supported versions: {}",
                    SUPPORTED_VERSIONS
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                retryable: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_selects_highest_mutual_version() {
        assert_eq!(
            negotiate_version(&VersionOffer {
                supported_versions: vec![0, 1, 9],
            }),
            Some(1)
        );
        assert_eq!(
            negotiate_version(&VersionOffer {
                supported_versions: vec![2, 3],
            }),
            None
        );
    }

    #[test]
    fn negotiation_does_not_depend_on_supported_version_order() {
        let supported = [3, 1, 2];
        let peer = VersionOffer {
            supported_versions: vec![1, 2, 4],
        };
        assert_eq!(highest_mutual_version(&supported, &peer), Some(2));
    }

    #[test]
    fn reconciliation_wire_shape_is_provider_neutral_and_omits_absent_fields() {
        let envelope = ControllerActionReconciliation {
            version: CURRENT_VERSION,
            action_id: "act_42".into(),
            action_kind: "open_session".into(),
            scope: "tenant:example/resource:42".into(),
            state: ControllerActionExecutionState::OutcomeUnknown,
            http_status: None,
            response: None,
            session: Some(ControllerSessionReconciliation {
                session_id: "ses_42".into(),
                state: "open".into(),
                trigger_ref: Some("object:example/42".into()),
                trigger_fingerprint: Some("revision:abc".into()),
                closed_at: None,
                decision: None,
                result: None,
            }),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["state"], "outcome_unknown");
        assert_eq!(value["session"]["trigger_ref"], "object:example/42");
        assert!(value.get("http_status").is_none());
        assert!(value.get("response").is_none());
        assert!(value["session"].get("result").is_none());
    }
}
