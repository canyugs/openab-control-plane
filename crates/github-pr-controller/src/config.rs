use std::collections::BTreeSet;

const DEFAULT_ADDR: &str = "0.0.0.0:8091";
const DEFAULT_DB: &str = "github-controller.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubAppConfig {
    pub app_id: Option<String>,
    pub installation_id: Option<String>,
    pub private_key: Option<String>,
}

impl GitHubAppConfig {
    pub fn readiness(&self) -> ComponentReadiness {
        let present = [
            self.app_id.is_some(),
            self.installation_id.is_some(),
            self.private_key.is_some(),
        ];
        if present.iter().all(|value| *value) {
            ComponentReadiness::ready("configured; write client disabled in plan-only mode")
        } else if present.iter().all(|value| !*value) {
            ComponentReadiness::disabled("not configured; write client disabled")
        } else {
            ComponentReadiness::not_ready("partial GitHub App configuration")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub addr: String,
    pub db_path: String,
    pub webhook_secret: Option<String>,
    pub allowed_repos: BTreeSet<String>,
    pub bot_handle: Option<String>,
    pub roster: Vec<String>,
    pub github_app: GitHubAppConfig,
}

impl Config {
    pub fn from_env() -> Self {
        Self::from_values(|name| std::env::var(name).ok())
    }

    pub fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Self {
        let allowed_repos = csv(value("GITHUB_CONTROLLER_ALLOWED_REPOS"))
            .into_iter()
            .collect();
        let roster = {
            let configured = csv(value("GITHUB_CONTROLLER_ROSTER"));
            if configured.is_empty() {
                vec!["chair".into(), "rev1".into(), "rev2".into()]
            } else {
                configured
            }
        };
        Self {
            addr: nonempty(value("GITHUB_CONTROLLER_ADDR")).unwrap_or_else(|| DEFAULT_ADDR.into()),
            db_path: nonempty(value("GITHUB_CONTROLLER_DB")).unwrap_or_else(|| DEFAULT_DB.into()),
            webhook_secret: nonempty(value("GITHUB_CONTROLLER_WEBHOOK_SECRET")),
            allowed_repos,
            bot_handle: nonempty(value("GITHUB_CONTROLLER_BOT_HANDLE"))
                .map(|handle| handle.trim_start_matches('@').to_string()),
            roster,
            github_app: GitHubAppConfig {
                app_id: nonempty(value("GITHUB_CONTROLLER_GITHUB_APP_ID")),
                installation_id: nonempty(value("GITHUB_CONTROLLER_GITHUB_APP_INSTALLATION_ID")),
                private_key: nonempty(value("GITHUB_CONTROLLER_GITHUB_APP_PRIVATE_KEY")),
            },
        }
    }

    pub fn ingress_readiness(&self) -> ComponentReadiness {
        if self.webhook_secret.is_some() {
            ComponentReadiness::ready("webhook HMAC configured")
        } else {
            ComponentReadiness::not_ready("webhook HMAC missing")
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ComponentReadiness {
    pub enabled: bool,
    pub ready: bool,
    pub detail: String,
}

impl ComponentReadiness {
    pub fn ready(detail: impl Into<String>) -> Self {
        Self {
            enabled: true,
            ready: true,
            detail: detail.into(),
        }
    }

    pub fn not_ready(detail: impl Into<String>) -> Self {
        Self {
            enabled: true,
            ready: false,
            detail: detail.into(),
        }
    }

    pub fn disabled(detail: impl Into<String>) -> Self {
        Self {
            enabled: false,
            ready: false,
            detail: detail.into(),
        }
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn csv(value: Option<String>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn config_reads_only_controller_owned_names() {
        let values = BTreeMap::from([
            ("GITHUB_CONTROLLER_DB", "/tmp/controller.db"),
            (
                "GITHUB_CONTROLLER_ALLOWED_REPOS",
                "example/repo, other/repo",
            ),
            ("GITHUB_CONTROLLER_BOT_HANDLE", "@review-bot"),
            ("GITHUB_CONTROLLER_ROSTER", "chair,reviewer"),
            ("OABCP_DB", "/tmp/plane.db"),
        ]);
        let config = Config::from_values(|name| values.get(name).map(ToString::to_string));
        assert_eq!(config.db_path, "/tmp/controller.db");
        assert_eq!(config.bot_handle.as_deref(), Some("review-bot"));
        assert_eq!(config.roster, ["chair", "reviewer"]);
        assert_eq!(config.allowed_repos.len(), 2);
        assert_ne!(config.db_path, values["OABCP_DB"]);
    }

    #[test]
    fn github_readiness_distinguishes_disabled_partial_and_ready() {
        let mut config = GitHubAppConfig {
            app_id: None,
            installation_id: None,
            private_key: None,
        };
        assert!(!config.readiness().enabled);
        config.app_id = Some("1".into());
        assert!(!config.readiness().ready);
        config.installation_id = Some("2".into());
        config.private_key = Some("pem".into());
        assert!(config.readiness().ready);
    }
}
