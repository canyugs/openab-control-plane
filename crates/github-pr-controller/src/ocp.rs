use crate::config::OcpActionConfig;
use controller_protocol::{
    ActionEnvelope, ActionResultEnvelope, ControllerAction, ErrorCode, ErrorEnvelope,
    OpenSessionAction, CURRENT_VERSION,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub type ActionFuture =
    Pin<Box<dyn Future<Output = Result<ActionResultEnvelope, ActionFailure>> + Send + 'static>>;

pub trait OcpActionClient: Send + Sync {
    fn open_session(&self, action_id: String, action: OpenSessionAction) -> ActionFuture;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionFailure {
    Unavailable,
    InvalidResponse,
    Protocol {
        status: u16,
        code: ErrorCode,
        retryable: bool,
    },
}

impl ActionFailure {
    pub fn public_code(&self) -> &'static str {
        match self {
            Self::Unavailable => "ocp_action_unavailable",
            Self::InvalidResponse => "ocp_action_invalid_response",
            Self::Protocol {
                retryable: true, ..
            } => "ocp_action_retryable_error",
            Self::Protocol {
                retryable: false, ..
            } => "ocp_action_rejected",
        }
    }
}

pub struct ReqwestOcpActionClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    action_token: String,
    scope: String,
}

impl ReqwestOcpActionClient {
    pub fn new(config: &OcpActionConfig) -> anyhow::Result<Self> {
        if !config.is_complete() {
            anyhow::bail!("OCP action client configuration is incomplete");
        }
        let base_url = config.base_url.as_deref().expect("complete config");
        let mut endpoint = reqwest::Url::parse(base_url)?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
            || endpoint.query().is_some()
        {
            anyhow::bail!(
                "OCP action URL must be an HTTPS origin without userinfo, query, or fragment"
            );
        }
        endpoint.set_path("/v1/controller/actions");
        let client = reqwest::Client::builder()
            .timeout(ACTION_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            endpoint,
            action_token: config.action_token.clone().expect("complete config"),
            scope: config.scope.clone().expect("complete config"),
        })
    }
}

impl OcpActionClient for ReqwestOcpActionClient {
    fn open_session(&self, action_id: String, action: OpenSessionAction) -> ActionFuture {
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let action_token = self.action_token.clone();
        let scope = self.scope.clone();
        Box::pin(async move {
            let envelope = ActionEnvelope {
                version: CURRENT_VERSION,
                action_id: action_id.clone(),
                action: ControllerAction::OpenSession(action),
            };
            let response = client
                .post(endpoint)
                .header(AUTHORIZATION, format!("Bearer {action_token}"))
                .header(CONTENT_TYPE, "application/json")
                .header("X-OAB-Action-ID", &action_id)
                .header("X-OAB-Scope", scope)
                .json(&envelope)
                .send()
                .await
                .map_err(|_| ActionFailure::Unavailable)?;
            let status = response.status();
            if response
                .content_length()
                .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
            {
                return Err(ActionFailure::InvalidResponse);
            }
            let body = response
                .bytes()
                .await
                .map_err(|_| ActionFailure::Unavailable)?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(ActionFailure::InvalidResponse);
            }
            if status.is_success() {
                let result: ActionResultEnvelope =
                    serde_json::from_slice(&body).map_err(|_| ActionFailure::InvalidResponse)?;
                if result.version != CURRENT_VERSION || result.action_id != action_id {
                    return Err(ActionFailure::InvalidResponse);
                }
                return Ok(result);
            }
            let error: ErrorEnvelope =
                serde_json::from_slice(&body).map_err(|_| ActionFailure::InvalidResponse)?;
            if error.version != CURRENT_VERSION
                || error.action_id.as_deref().is_some_and(|id| id != action_id)
            {
                return Err(ActionFailure::InvalidResponse);
            }
            Err(ActionFailure::Protocol {
                status: status.as_u16(),
                code: error.error.code,
                retryable: error.error.retryable,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_client_requires_a_safe_complete_origin() {
        let complete = |url: &str| OcpActionConfig {
            base_url: Some(url.into()),
            action_token: Some("token".into()),
            scope: Some("tenant:dev/resource:canary".into()),
            controller_id: Some("github-canary".into()),
        };
        assert!(ReqwestOcpActionClient::new(&complete("https://ocp.example.test/base")).is_ok());
        assert!(ReqwestOcpActionClient::new(&complete("http://ocp.example.test")).is_err());
        assert!(ReqwestOcpActionClient::new(&complete("https://user@ocp.example.test")).is_err());
        assert!(ReqwestOcpActionClient::new(&complete("https://ocp.example.test?q=1")).is_err());
    }
}
