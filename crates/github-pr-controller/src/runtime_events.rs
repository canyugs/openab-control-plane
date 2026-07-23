use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;
const MAX_TIMESTAMP_SKEW_SECS: u64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEventEnvelope {
    pub version: String,
    pub event_id: String,
    pub controller_id: String,
    pub event_type: String,
    pub session_id: Option<String>,
    pub occurred_at: i64,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationError {
    MissingHeader,
    InvalidIdentity,
    StaleTimestamp,
    InvalidSignature,
    InvalidBody,
}

impl VerificationError {
    pub fn public_code(self) -> &'static str {
        match self {
            Self::MissingHeader => "missing_runtime_event_header",
            Self::InvalidIdentity => "invalid_runtime_event_identity",
            Self::StaleTimestamp => "stale_runtime_event_timestamp",
            Self::InvalidSignature => "invalid_runtime_event_signature",
            Self::InvalidBody => "invalid_runtime_event_body",
        }
    }
}

pub struct RuntimeEventVerifier {
    controller_id: String,
    secret: Vec<u8>,
}

impl RuntimeEventVerifier {
    pub fn new(controller_id: &str, encoded_secret: &str) -> anyhow::Result<Self> {
        let secret = URL_SAFE_NO_PAD.decode(encoded_secret.as_bytes())?;
        if controller_id.is_empty() || secret.len() < 32 {
            anyhow::bail!("runtime-event controller id and 32-byte signing secret are required");
        }
        Ok(Self {
            controller_id: controller_id.into(),
            secret,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn verify(
        &self,
        header_controller_id: Option<&str>,
        header_event_id: Option<&str>,
        header_timestamp: Option<&str>,
        header_signature: Option<&str>,
        request_target: &str,
        body: &[u8],
        now_secs: i64,
    ) -> Result<RuntimeEventEnvelope, VerificationError> {
        let (Some(controller_id), Some(event_id), Some(timestamp), Some(signature)) = (
            header_controller_id,
            header_event_id,
            header_timestamp,
            header_signature,
        ) else {
            return Err(VerificationError::MissingHeader);
        };
        if controller_id != self.controller_id || !valid_identifier(event_id) {
            return Err(VerificationError::InvalidIdentity);
        }
        let timestamp = timestamp
            .parse::<i64>()
            .map_err(|_| VerificationError::StaleTimestamp)?;
        if now_secs.abs_diff(timestamp) > MAX_TIMESTAMP_SKEW_SECS {
            return Err(VerificationError::StaleTimestamp);
        }
        let signature = signature
            .strip_prefix("sha256=")
            .and_then(|value| hex::decode(value).ok())
            .ok_or(VerificationError::InvalidSignature)?;
        let canonical = canonical_bytes(controller_id, event_id, timestamp, request_target, body);
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC accepts signing keys of any length");
        mac.update(&canonical);
        mac.verify_slice(&signature)
            .map_err(|_| VerificationError::InvalidSignature)?;

        let envelope: RuntimeEventEnvelope =
            serde_json::from_slice(body).map_err(|_| VerificationError::InvalidBody)?;
        if envelope.version != "1"
            || envelope.controller_id != controller_id
            || envelope.event_id != event_id
            || !supported_event_type(&envelope.event_type)
            || envelope
                .session_id
                .as_deref()
                .is_some_and(|id| !valid_identifier(id))
        {
            return Err(VerificationError::InvalidBody);
        }
        Ok(envelope)
    }
}

fn canonical_bytes(
    controller_id: &str,
    event_id: &str,
    timestamp: i64,
    request_target: &str,
    body: &[u8],
) -> Vec<u8> {
    let body_hash = hex::encode(Sha256::digest(body));
    format!("v1\n{controller_id}\n{event_id}\n{timestamp}\nPOST\n{request_target}\n{body_hash}")
        .into_bytes()
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

fn supported_event_type(value: &str) -> bool {
    matches!(
        value,
        "session.opened"
            | "session.progress"
            | "session.terminal"
            | "session.timeout"
            | "session.superseded"
            | "action.failed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_headers(
        secret: &[u8],
        event_id: &str,
        timestamp: i64,
        target: &str,
        body: &[u8],
    ) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(&canonical_bytes(
            "github-canary",
            event_id,
            timestamp,
            target,
            body,
        ));
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn exact_target_body_timestamp_and_identity_are_verified() {
        let secret = vec![9; 32];
        let verifier =
            RuntimeEventVerifier::new("github-canary", &URL_SAFE_NO_PAD.encode(&secret)).unwrap();
        let target = "/api/v1/openab/events?version=1";
        let body = br#"{"version":"1","event_id":"cev_1","controller_id":"github-canary","event_type":"session.timeout","session_id":"ses_1","occurred_at":1000000,"payload":{"reason":"timeout"}}"#;
        let signature = signed_headers(&secret, "cev_1", 1_000, target, body);
        let event = verifier
            .verify(
                Some("github-canary"),
                Some("cev_1"),
                Some("1000"),
                Some(&signature),
                target,
                body,
                1_001,
            )
            .unwrap();
        assert_eq!(event.event_type, "session.timeout");

        assert_eq!(
            verifier.verify(
                Some("github-canary"),
                Some("cev_1"),
                Some("1000"),
                Some(&signature),
                "/different",
                body,
                1_001,
            ),
            Err(VerificationError::InvalidSignature)
        );
        assert_eq!(
            verifier.verify(
                Some("github-canary"),
                Some("cev_1"),
                Some("1000"),
                Some(&signature),
                target,
                body,
                2_000,
            ),
            Err(VerificationError::StaleTimestamp)
        );
    }
}
