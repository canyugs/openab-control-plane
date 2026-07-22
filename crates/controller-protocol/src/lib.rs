#![forbid(unsafe_code)]

//! Provider-neutral data contract between an external controller and the
//! OpenAB control-plane runtime.
//!
//! This crate contains serialized data only. It intentionally has no runtime,
//! storage, transport, or product integration dependencies.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    SUPPORTED_VERSIONS
        .iter()
        .rev()
        .copied()
        .find(|version| peer.supported_versions.contains(version))
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
pub struct ActionResultEnvelope {
    pub version: u16,
    pub action_id: String,
    pub result: ControllerActionResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ControllerActionResult {
    SessionOpened { session_id: String, deduped: bool },
    Superseded { session_id: String, old_id: String },
    MessagePosted { message_id: String },
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
}
