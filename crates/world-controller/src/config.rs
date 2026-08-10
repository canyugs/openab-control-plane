//! Environment configuration, `WORLD_CONTROLLER_*` prefix. Registration and
//! grants flow: `docs/controller-action-api.md`.

const DEFAULT_ADDR: &str = "0.0.0.0:8092";
const DEFAULT_DB: &str = "world-controller.db";

#[derive(Debug, Clone, Default)]
pub struct OcpActionConfig {
    pub base_url: Option<String>,
    pub action_token: Option<String>,
    pub scope: Option<String>,
    pub controller_id: Option<String>,
}

impl OcpActionConfig {
    pub fn is_complete(&self) -> bool {
        self.base_url.is_some()
            && self.action_token.is_some()
            && self.scope.is_some()
            && self.controller_id.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub addr: String,
    pub db_path: String,
    pub ocp_action: OcpActionConfig,
    pub event_signing_secret: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_values(|name| std::env::var(name).ok())
    }

    pub fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Self {
        Self {
            addr: nonempty(value("WORLD_CONTROLLER_ADDR")).unwrap_or_else(|| DEFAULT_ADDR.into()),
            db_path: nonempty(value("WORLD_CONTROLLER_DB")).unwrap_or_else(|| DEFAULT_DB.into()),
            ocp_action: OcpActionConfig {
                base_url: nonempty(value("WORLD_CONTROLLER_OCP_URL")),
                action_token: nonempty(value("WORLD_CONTROLLER_OCP_ACTION_TOKEN")),
                scope: nonempty(value("WORLD_CONTROLLER_OCP_SCOPE")),
                controller_id: nonempty(value("WORLD_CONTROLLER_ID")),
            },
            event_signing_secret: nonempty(value("WORLD_CONTROLLER_EVENT_SIGNING_SECRET")),
        }
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}
