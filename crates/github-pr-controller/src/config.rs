use std::collections::BTreeSet;

const DEFAULT_ADDR: &str = "0.0.0.0:8091";
const DEFAULT_DB: &str = "github-controller.db";
const DEFAULT_GITHUB_API: &str = "https://api.github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatingMode {
    PlanOnly,
    ExternalCanary,
    /// The end state ADR 031 aims at: the controller owns every repository the
    /// GitHub App installation covers. The installation token is the scope
    /// boundary — GitHub refuses writes outside it regardless of what this
    /// process believes — and `allowed_repos` remains available as an optional
    /// narrower brake.
    External,
    Invalid(String),
}

impl OperatingMode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::PlanOnly => "plan_only",
            Self::ExternalCanary => "external_canary",
            Self::External => "external",
            Self::Invalid(value) => value,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OcpActionConfig {
    pub base_url: Option<String>,
    pub action_token: Option<String>,
    pub scope: Option<String>,
    pub controller_id: Option<String>,
}

impl OcpActionConfig {
    pub fn is_empty(&self) -> bool {
        self.base_url.is_none()
            && self.action_token.is_none()
            && self.scope.is_none()
            && self.controller_id.is_none()
    }

    pub fn is_complete(&self) -> bool {
        self.base_url.is_some()
            && self.action_token.is_some()
            && self.scope.is_some()
            && self.controller_id.is_some()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitHubAppConfig {
    pub app_id: Option<String>,
    pub installation_id: Option<String>,
    pub private_key: Option<String>,
}

impl GitHubAppConfig {
    pub fn is_empty(&self) -> bool {
        self.app_id.is_none() && self.installation_id.is_none() && self.private_key.is_none()
    }

    pub fn is_complete(&self) -> bool {
        self.app_id.is_some() && self.installation_id.is_some() && self.private_key.is_some()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub addr: String,
    pub db_path: String,
    pub mode: OperatingMode,
    pub webhook_secret: Option<String>,
    pub shadow_secret: Option<String>,
    pub observer_secret: Option<String>,
    pub canary_repository: Option<String>,
    pub allowed_repos: BTreeSet<String>,
    pub bot_handle: Option<String>,
    pub roster: Vec<String>,
    pub council_preset: Option<String>,
    pub review_mode: String,
    pub ocp_action: OcpActionConfig,
    pub event_signing_secret: Option<String>,
    pub github_app: GitHubAppConfig,
    /// Second lock on the write client, independent of credentials being
    /// present. ADR 031 invariant #4 allows exactly one side-effect owner per
    /// provider object, and until the P7 routing switch that owner is still the
    /// embedded plugin — so credentials alone must never start writing.
    pub enable_writes: bool,
    /// GitHub REST base, overridable so tests can point at a local server.
    pub github_api_base: String,
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
            mode: match nonempty(value("GITHUB_CONTROLLER_MODE")).as_deref() {
                None | Some("plan_only") => OperatingMode::PlanOnly,
                Some("external_canary") => OperatingMode::ExternalCanary,
                Some("external") => OperatingMode::External,
                Some(value) => OperatingMode::Invalid(value.into()),
            },
            webhook_secret: nonempty(value("GITHUB_CONTROLLER_WEBHOOK_SECRET")),
            shadow_secret: nonempty(value("GITHUB_CONTROLLER_SHADOW_SECRET")),
            observer_secret: nonempty(value("GITHUB_CONTROLLER_OBSERVER_SECRET")),
            canary_repository: nonempty(value("GITHUB_CONTROLLER_CANARY_REPOSITORY")),
            allowed_repos,
            bot_handle: nonempty(value("GITHUB_CONTROLLER_BOT_HANDLE"))
                .map(|handle| handle.trim_start_matches('@').to_string()),
            roster,
            council_preset: nonempty(value("GITHUB_CONTROLLER_COUNCIL_PRESET"))
                .filter(|preset| matches!(preset.as_str(), "lite" | "quick" | "standard" | "full")),
            review_mode: nonempty(value("GITHUB_CONTROLLER_REVIEW_MODE"))
                .filter(|mode| matches!(mode.as_str(), "status" | "approve" | "enforce"))
                .unwrap_or_else(|| "approve".into()),
            ocp_action: OcpActionConfig {
                base_url: nonempty(value("GITHUB_CONTROLLER_OCP_URL")),
                action_token: nonempty(value("GITHUB_CONTROLLER_OCP_ACTION_TOKEN")),
                scope: nonempty(value("GITHUB_CONTROLLER_OCP_SCOPE")),
                controller_id: nonempty(value("GITHUB_CONTROLLER_ID")),
            },
            event_signing_secret: nonempty(value("GITHUB_CONTROLLER_EVENT_SIGNING_SECRET")),
            github_app: GitHubAppConfig {
                app_id: nonempty(value("GITHUB_CONTROLLER_GITHUB_APP_ID")),
                installation_id: nonempty(value("GITHUB_CONTROLLER_GITHUB_APP_INSTALLATION_ID")),
                private_key: nonempty(value("GITHUB_CONTROLLER_GITHUB_APP_PRIVATE_KEY")),
            },
            enable_writes: nonempty(value("GITHUB_CONTROLLER_ENABLE_WRITES")).as_deref()
                == Some("1"),
            github_api_base: nonempty(value("GITHUB_CONTROLLER_GITHUB_API_BASE"))
                .unwrap_or_else(|| DEFAULT_GITHUB_API.into()),
        }
    }

    /// The write client's readiness. Credentials are refused unless the
    /// controller is both in `external_canary` **and** explicitly switched on,
    /// so a half-configured deploy fails loudly instead of writing.
    pub fn github_readiness(&self) -> ComponentReadiness {
        match self.mode {
            _ if self.github_app.is_empty() && !self.enable_writes => {
                ComponentReadiness::disabled("not configured; write client disabled")
            }
            OperatingMode::ExternalCanary | OperatingMode::External
                if self.enable_writes && self.github_app.is_complete() =>
            {
                ComponentReadiness::ready("GitHub write client configured")
            }
            OperatingMode::ExternalCanary | OperatingMode::External if !self.enable_writes => {
                ComponentReadiness::not_ready(
                    "GitHub App credentials present but writes are not enabled",
                )
            }
            OperatingMode::ExternalCanary | OperatingMode::External
                if self.github_app.is_empty() =>
            {
                ComponentReadiness::not_ready("writes enabled but no GitHub App configured")
            }
            OperatingMode::ExternalCanary | OperatingMode::External => {
                ComponentReadiness::not_ready("partial GitHub App configuration")
            }
            _ => ComponentReadiness::not_ready(
                "GitHub App credentials and writes forbidden outside external_canary mode",
            ),
        }
    }

    pub fn ingress_readiness(&self) -> ComponentReadiness {
        if self.webhook_secret.is_some() {
            ComponentReadiness::ready("webhook HMAC configured")
        } else {
            ComponentReadiness::not_ready("webhook HMAC missing")
        }
    }

    pub fn ownership_readiness(&self) -> ComponentReadiness {
        match &self.mode {
            OperatingMode::PlanOnly if self.canary_repository.is_none() => {
                ComponentReadiness::disabled("external canary ownership disabled")
            }
            OperatingMode::PlanOnly => ComponentReadiness::not_ready(
                "canary repository configured while mode is plan_only",
            ),
            OperatingMode::Invalid(value) => {
                ComponentReadiness::not_ready(format!("unsupported controller mode: {value}"))
            }
            OperatingMode::External => {
                if self.canary_repository.is_some() {
                    return ComponentReadiness::not_ready(
                        "canary repository is set but the mode is external — unset one of them",
                    );
                }
                if self.allowed_repos.is_empty() {
                    ComponentReadiness::ready(
                        "external ingress owned for the whole App installation",
                    )
                } else {
                    ComponentReadiness::ready(format!(
                        "external ingress owned for the App installation, filtered to {} repos",
                        self.allowed_repos.len()
                    ))
                }
            }
            OperatingMode::ExternalCanary => {
                let Some(repository) = self.canary_repository.as_deref() else {
                    return ComponentReadiness::not_ready("external canary repository missing");
                };
                if !valid_repository(repository) {
                    return ComponentReadiness::not_ready(
                        "external canary repository must be owner/name",
                    );
                }
                if !self.allowed_repos.is_empty()
                    && (self.allowed_repos.len() != 1 || !self.allowed_repos.contains(repository))
                {
                    return ComponentReadiness::not_ready(
                        "allowed repositories must be empty or exactly the canary repository",
                    );
                }
                ComponentReadiness::ready(format!(
                    "external ingress owned for exactly {repository}"
                ))
            }
        }
    }

    pub fn ocp_readiness(&self) -> ComponentReadiness {
        match self.mode {
            OperatingMode::PlanOnly if self.ocp_action.is_empty() => {
                ComponentReadiness::disabled("action client disabled in plan-only mode")
            }
            OperatingMode::PlanOnly | OperatingMode::Invalid(_) => ComponentReadiness::not_ready(
                "OCP action credentials forbidden outside external_canary mode",
            ),
            OperatingMode::ExternalCanary | OperatingMode::External
                if self.ocp_action.is_complete() =>
            {
                ComponentReadiness::ready("scoped OCP action client configured")
            }
            OperatingMode::ExternalCanary | OperatingMode::External => {
                ComponentReadiness::not_ready("scoped OCP action client configuration incomplete")
            }
        }
    }

    pub fn event_readiness(&self) -> ComponentReadiness {
        match self.mode {
            OperatingMode::PlanOnly
                if self.event_signing_secret.is_none() && self.observer_secret.is_none() =>
            {
                ComponentReadiness::disabled("runtime-event receiver disabled in plan-only mode")
            }
            OperatingMode::PlanOnly | OperatingMode::Invalid(_) => ComponentReadiness::not_ready(
                "runtime-event and observer credentials forbidden outside external_canary mode",
            ),
            OperatingMode::ExternalCanary | OperatingMode::External
                if self.event_signing_secret.is_some()
                    && self.ocp_action.controller_id.is_some()
                    && self.observer_secret.is_some() =>
            {
                ComponentReadiness::ready("signed runtime-event receiver configured")
            }
            OperatingMode::ExternalCanary | OperatingMode::External => {
                ComponentReadiness::not_ready(
                    "runtime-event signing and observation secrets are required",
                )
            }
        }
    }
}

fn valid_repository(repository: &str) -> bool {
    let mut parts = repository.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(name), None)
        if !owner.is_empty() && !name.is_empty())
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
            ("GITHUB_CONTROLLER_COUNCIL_PRESET", "standard"),
            ("GITHUB_CONTROLLER_REVIEW_MODE", "enforce"),
            ("OABCP_DB", "/tmp/plane.db"),
        ]);
        let config = Config::from_values(|name| values.get(name).map(ToString::to_string));
        assert_eq!(config.db_path, "/tmp/controller.db");
        assert_eq!(config.bot_handle.as_deref(), Some("review-bot"));
        assert_eq!(config.roster, ["chair", "reviewer"]);
        assert_eq!(config.council_preset.as_deref(), Some("standard"));
        assert_eq!(config.review_mode, "enforce");
        assert_eq!(config.allowed_repos.len(), 2);
        assert_ne!(config.db_path, values["OABCP_DB"]);
        assert_eq!(config.mode, OperatingMode::PlanOnly);
        assert!(config.ocp_action.is_empty());

        let mut wrong = config;
        wrong.observer_secret = Some("canary-only".into());
        assert!(!wrong.event_readiness().ready);
    }

    #[test]
    fn writes_need_external_canary_and_an_explicit_switch() {
        let mut config = Config::from_values(|_| None);
        assert!(!config.enable_writes);
        assert!(!config.github_readiness().enabled, "off by default");

        // Credentials in plan_only stay forbidden — that is the pre-P4 rule and
        // it has not moved.
        config.github_app = GitHubAppConfig {
            app_id: Some("1".into()),
            installation_id: Some("2".into()),
            private_key: Some("pem".into()),
        };
        assert!(!config.github_readiness().ready);
        config.enable_writes = true;
        assert!(!config.github_readiness().ready, "still plan_only");

        // Canary + switch + complete credentials: the only ready combination.
        config.mode = OperatingMode::ExternalCanary;
        assert!(config.github_readiness().ready);

        // Each leg alone is not enough.
        let mut no_switch = config.clone();
        no_switch.enable_writes = false;
        assert!(!no_switch.github_readiness().ready);
        let mut partial = config.clone();
        partial.github_app.private_key = None;
        assert!(!partial.github_readiness().ready);
        // The switch on its own, with no credentials, is a misconfiguration
        // rather than a quiet no-op.
        let mut switch_only = Config::from_values(|_| None);
        switch_only.mode = OperatingMode::ExternalCanary;
        switch_only.enable_writes = true;
        assert!(!switch_only.github_readiness().ready);
    }

    #[test]
    fn external_canary_requires_exact_ownership_and_complete_credentials() {
        let values = BTreeMap::from([
            ("GITHUB_CONTROLLER_MODE", "external_canary"),
            ("GITHUB_CONTROLLER_CANARY_REPOSITORY", "example/repo"),
            ("GITHUB_CONTROLLER_ALLOWED_REPOS", "example/repo"),
            ("GITHUB_CONTROLLER_OCP_URL", "https://ocp.example.test"),
            ("GITHUB_CONTROLLER_OCP_ACTION_TOKEN", "token"),
            ("GITHUB_CONTROLLER_OCP_SCOPE", "tenant:dev/resource:canary"),
            ("GITHUB_CONTROLLER_ID", "github-canary"),
            ("GITHUB_CONTROLLER_EVENT_SIGNING_SECRET", "event-secret"),
            ("GITHUB_CONTROLLER_OBSERVER_SECRET", "observation-secret"),
        ]);
        let config = Config::from_values(|name| values.get(name).map(ToString::to_string));
        assert_eq!(config.mode, OperatingMode::ExternalCanary);
        assert!(config.ownership_readiness().ready);
        assert!(config.ocp_readiness().ready);
        assert!(config.event_readiness().ready);

        let mut wrong = config;
        wrong.allowed_repos.insert("other/repo".into());
        assert!(!wrong.ownership_readiness().ready);
    }

    #[test]
    fn external_mode_owns_the_installation_and_refuses_a_canary_pin() {
        let values = BTreeMap::from([
            ("GITHUB_CONTROLLER_MODE", "external"),
            ("GITHUB_CONTROLLER_OCP_URL", "https://ocp.example.test"),
            ("GITHUB_CONTROLLER_OCP_ACTION_TOKEN", "token"),
            ("GITHUB_CONTROLLER_OCP_SCOPE", "tenant:prod/resource:canary"),
            ("GITHUB_CONTROLLER_ID", "github-prod"),
            ("GITHUB_CONTROLLER_EVENT_SIGNING_SECRET", "event-secret"),
            ("GITHUB_CONTROLLER_OBSERVER_SECRET", "observation-secret"),
        ]);
        let config = Config::from_values(|name| values.get(name).map(ToString::to_string));
        assert_eq!(config.mode, OperatingMode::External);
        assert!(config.ownership_readiness().ready);
        assert!(config.ocp_readiness().ready);
        assert!(config.event_readiness().ready);

        // A leftover canary pin means the operator has not decided which mode
        // this deployment is in — fail loudly rather than guess.
        let mut pinned = config.clone();
        pinned.canary_repository = Some("example/repo".into());
        assert!(!pinned.ownership_readiness().ready);

        // allowed_repos stays available as an optional narrower brake.
        let mut braked = config;
        braked.allowed_repos = BTreeSet::from(["example/repo".into()]);
        assert!(braked.ownership_readiness().ready);
    }
}
