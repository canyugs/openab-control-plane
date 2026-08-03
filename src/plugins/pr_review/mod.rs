pub mod findings;
pub mod verdict;

/// The v1 report gate only rejects the two shapes that are unambiguously not a
/// report: a very short settled turn and a bare tool/error echo. It deliberately
/// does not impose a report grammar; a concise human finding remains valid.
pub const MIN_REPORT_CHARS: usize = 20;

pub fn report_delivered(text: &str) -> bool {
    let text = text.trim();
    text.chars().count() >= MIN_REPORT_CHARS && !is_bare_tool_echo(text)
}

fn is_bare_tool_echo(text: &str) -> bool {
    let first_line = text.lines().next().unwrap_or_default().trim();
    let lower = text.to_ascii_lowercase();
    let first_lower = lower.lines().next().unwrap_or_default().trim();
    let tool_marker = first_lower.starts_with("tool result")
        || first_lower.starts_with("tool output")
        || first_lower.starts_with("tool_result")
        || first_lower.starts_with("<tool_result")
        || first_lower.starts_with("{\"tool_result\"")
        || first_lower.starts_with("{\"type\":\"tool_result\"");
    let json_error = first_line.starts_with('{')
        && lower.contains("\"error\"")
        && (lower.contains("jsonrpc") || lower.contains("\"code\""));
    (tool_marker || json_error) && text.lines().count() <= 4
}

/// Immutable PR-review policy assembled once at the process boundary.
///
/// The plugin consumes this value and never reaches into process environment
/// state itself. That keeps webhook parsing, admission, and prompt rendering
/// deterministic and makes the review plugin extractable from the OCP binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReviewConfig {
    pub bot_handle: Option<String>,
    pub allowed_repos: Vec<String>,
    pub review_round_budget: usize,
    pub review_hourly_cap: usize,
    pub council_preset: Option<String>,
    pub council_roster: Vec<String>,
    pub plane_status_notice: bool,
}

impl Default for PrReviewConfig {
    fn default() -> Self {
        Self {
            bot_handle: None,
            allowed_repos: Vec::new(),
            review_round_budget: 10,
            review_hourly_cap: 3,
            council_preset: None,
            council_roster: vec!["chair".into(), "rev1".into(), "rev2".into()],
            plane_status_notice: false,
        }
    }
}

impl PrReviewConfig {
    /// Build from an explicit key/value source. The composition root owns the
    /// actual environment lookup; the plugin only owns normalization/defaults.
    pub(crate) fn from_values(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let mut config = Self::default();
        config.bot_handle = lookup("OABCP_BOT_HANDLE").and_then(|raw| normalize_bot_handle(&raw));
        config.allowed_repos = csv_value(lookup("OABCP_ALLOWED_REPOS")).unwrap_or_default();
        config.review_round_budget = usize_value(lookup("OABCP_REVIEW_ROUND_BUDGET"), 10);
        config.review_hourly_cap = usize_value(lookup("OABCP_REVIEW_HOURLY_CAP"), 3);
        config.council_roster = csv_value(lookup("OABCP_COUNCIL_ROSTER"))
            .filter(|roster| !roster.is_empty())
            .unwrap_or_else(|| config.council_roster.clone());
        config.council_preset = lookup("OABCP_COUNCIL_PRESET")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .and_then(|value| {
                if matches!(value.as_str(), "lite" | "quick" | "standard" | "full") {
                    Some(value)
                } else {
                    tracing::warn!(preset = %value, "unknown OABCP_COUNCIL_PRESET (want lite|quick|standard|full); using default");
                    None
                }
            });
        config.plane_status_notice = matches!(
            lookup("OABCP_PLANE_STATUS_NOTICE").as_deref(),
            Some("1") | Some("true")
        );
        config
    }

    pub fn repo_allowed(&self, repo: &str) -> bool {
        self.allowed_repos.is_empty() || self.allowed_repos.iter().any(|allowed| allowed == repo)
    }
}

fn csv_value(value: Option<String>) -> Option<Vec<String>> {
    value.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn usize_value(value: Option<String>, default: usize) -> usize {
    value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn normalize_bot_handle(raw: &str) -> Option<String> {
    let handle = raw.trim().trim_start_matches('@').trim();
    if handle.is_empty() {
        None
    } else {
        Some(handle.to_string())
    }
}

/// Effective standing roster. A DB override lets operators replace bots
/// without restarting the control-plane; injected process configuration
/// remains the fallback and bootstrap source.
pub fn runtime_council_roster(
    state: &std::sync::Arc<crate::state::AppState>,
) -> anyhow::Result<(Vec<String>, &'static str)> {
    match state.store.standing_roster()? {
        Some(roster) => Ok((roster, "override")),
        None => Ok((state.pr_review_config.council_roster.clone(), "config")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn report_delivery_rejects_short_echoes_but_keeps_concise_findings() {
        assert!(!report_delivered("PONG [done]"));
        assert!(report_delivered("Nil input can panic."));
        assert!(report_delivered(
            "Risk: nil input can panic; add a guard before dereferencing."
        ));
        assert!(report_delivered(
            "One short finding: the retry path drops the request id."
        ));
    }

    #[test]
    fn report_delivery_rejects_bare_tool_echo_shapes() {
        assert!(!report_delivered(
            "tool_result: command completed successfully but returned no report"
        ));
        assert!(!report_delivered(
            r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"Internal Error"}}"#
        ));
        assert!(!report_delivered(
            "{\n  \"jsonrpc\": \"2.0\",\n  \"error\": {\"code\": -32603}\n}"
        ));
    }

    #[test]
    fn explicit_config_source_normalizes_every_review_policy_value() {
        let values = HashMap::from([
            ("OABCP_BOT_HANDLE", " @nellen "),
            ("OABCP_ALLOWED_REPOS", "nuphos/core, nuphos/ops"),
            ("OABCP_REVIEW_ROUND_BUDGET", "7"),
            ("OABCP_REVIEW_HOURLY_CAP", "2"),
            ("OABCP_COUNCIL_PRESET", "standard"),
            ("OABCP_COUNCIL_ROSTER", "chair, security, tests"),
            ("OABCP_COUNCIL_REVIEW_MODE", "enforce"),
            ("OABCP_PLANE_STATUS_NOTICE", "true"),
        ]);
        let config = PrReviewConfig::from_values(|name| values.get(name).map(|v| v.to_string()));

        assert_eq!(config.bot_handle.as_deref(), Some("nellen"));
        assert_eq!(config.allowed_repos, vec!["nuphos/core", "nuphos/ops"]);
        assert_eq!(config.review_round_budget, 7);
        assert_eq!(config.review_hourly_cap, 2);
        assert_eq!(config.council_preset.as_deref(), Some("standard"));
        assert_eq!(config.council_roster, vec!["chair", "security", "tests"]);
        assert!(config.plane_status_notice);
    }

    #[test]
    fn invalid_explicit_config_preserves_safe_defaults() {
        let values = HashMap::from([
            ("OABCP_REVIEW_ROUND_BUDGET", "not-a-number"),
            ("OABCP_REVIEW_HOURLY_CAP", ""),
            ("OABCP_COUNCIL_PRESET", "unknown"),
            ("OABCP_COUNCIL_ROSTER", " , "),
            ("OABCP_PLANE_STATUS_NOTICE", "yes"),
        ]);
        let config = PrReviewConfig::from_values(|name| values.get(name).map(|v| v.to_string()));

        assert_eq!(config, PrReviewConfig::default());
    }
}
