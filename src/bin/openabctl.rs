//! First-party, read-only investigation bundle builder (ADR 036).
//!
//! The command intentionally talks to the two service-local audit APIs. It
//! does not open either database and it treats an unavailable service or a
//! missing cross-service correlation as data in the result, not as something
//! to hide behind a partial success message.

use anyhow::{Context, Result};
use controller_protocol::audit::{AuditEventPage, AuditEventRecord};
use hmac::{Hmac, Mac};
use reqwest::{Client, Url};
use serde::Serialize;
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;
const INVESTIGATION_BUNDLE_VERSION: u16 = 1;
const PAGE_LIMIT: usize = 500;
const MAX_PAGES_PER_QUERY: usize = 100;
const MAX_CORRELATION_QUERIES: usize = 128;
const MAX_BUNDLE_EVENTS: usize = 20_000;

#[derive(Debug, Clone)]
enum Selector {
    Session(String),
    Delivery(String),
    TriggerRef(String),
}

impl Selector {
    fn filter(&self) -> CorrelationLink {
        match self {
            Self::Session(value) => CorrelationLink {
                field: "session_id".into(),
                value: value.clone(),
            },
            Self::Delivery(value) => CorrelationLink {
                field: "delivery_id".into(),
                value: value.clone(),
            },
            Self::TriggerRef(value) => CorrelationLink {
                field: "trigger_ref".into(),
                value: value.clone(),
            },
        }
    }

    fn display(&self) -> String {
        let link = self.filter();
        format!("{}={}", link.field, link.value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CorrelationLink {
    field: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct BundleEvent {
    service: String,
    #[serde(flatten)]
    record: AuditEventRecord,
}

#[derive(Debug, Serialize)]
struct InvestigationGap {
    service: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation: Option<CorrelationLink>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct InvestigationBundle {
    version: u16,
    generated_at: i64,
    selector: String,
    events: Vec<BundleEvent>,
    gaps: Vec<InvestigationGap>,
}

struct ServiceClient {
    name: &'static str,
    base_url: String,
    path: &'static str,
    ocp_api_key: Option<String>,
    observer_secret: Option<String>,
}

struct ReadResult {
    events: Vec<AuditEventRecord>,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let selector = parse_args(env::args().skip(1).collect())?;
    let bundle = investigate(&selector).await?;
    println!("{}", serde_json::to_string_pretty(&bundle)?);
    Ok(())
}

async fn investigate(selector: &Selector) -> Result<InvestigationBundle> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .context("build investigation HTTP client")?;
    let services = [
        ServiceClient {
            name: "openab-control-plane",
            base_url: env_or("OPENABCTL_OCP_URL", "http://127.0.0.1:8090"),
            path: "/v1/audit/events",
            ocp_api_key: nonempty_env("OPENABCTL_OCP_API_KEY"),
            observer_secret: None,
        },
        ServiceClient {
            name: "github-pr-controller",
            base_url: env_or("OPENABCTL_CONTROLLER_URL", "http://127.0.0.1:8091"),
            path: "/api/v1/audit/events",
            ocp_api_key: None,
            observer_secret: nonempty_env("OPENABCTL_CONTROLLER_OBSERVER_SECRET"),
        },
    ];

    let initial = selector.filter();
    let mut gaps = Vec::new();
    let mut events = BTreeMap::<(String, String), BundleEvent>::new();
    let mut pending = VecDeque::new();
    let mut queried = BTreeSet::<(String, String)>::new();
    let mut event_limit_reached = false;

    for service in &services {
        let result = fetch_events(&client, service, &initial).await;
        if let Some(error) = result.error {
            gaps.push(InvestigationGap {
                service: service.name.into(),
                kind: "service_query_failed".into(),
                correlation: Some(initial.clone()),
                detail: error,
            });
        } else if result.events.is_empty() {
            gaps.push(InvestigationGap {
                service: service.name.into(),
                kind: "selector_no_events".into(),
                correlation: Some(initial.clone()),
                detail: "the service returned no matching audit events".into(),
            });
        }
        event_limit_reached |= add_events(service.name, result.events, &mut events, &mut pending);
    }
    queried.insert((initial.field.clone(), initial.value.clone()));

    let mut expansion_count = 0;
    while let Some(link) = pending.pop_front() {
        if !queried.insert((link.field.clone(), link.value.clone())) {
            continue;
        }
        if expansion_count >= MAX_CORRELATION_QUERIES {
            gaps.push(InvestigationGap {
                service: "openabctl".into(),
                kind: "correlation_expansion_limited".into(),
                correlation: Some(link),
                detail: format!("maximum of {MAX_CORRELATION_QUERIES} correlation queries reached"),
            });
            break;
        }
        expansion_count += 1;
        for service in &services {
            let result = fetch_events(&client, service, &link).await;
            if let Some(error) = result.error {
                gaps.push(InvestigationGap {
                    service: service.name.into(),
                    kind: "service_query_failed".into(),
                    correlation: Some(link.clone()),
                    detail: error,
                });
            } else if result.events.is_empty() {
                gaps.push(InvestigationGap {
                    service: service.name.into(),
                    kind: "correlation_not_found".into(),
                    correlation: Some(link.clone()),
                    detail: "no event in this service carried the discovered correlation".into(),
                });
            }
            event_limit_reached |=
                add_events(service.name, result.events, &mut events, &mut pending);
        }
    }

    if event_limit_reached {
        gaps.push(InvestigationGap {
            service: "openabctl".into(),
            kind: "bundle_event_limit_reached".into(),
            correlation: None,
            detail: format!("bundle contains at most {MAX_BUNDLE_EVENTS} events"),
        });
    }

    let mut events: Vec<_> = events.into_values().collect();
    events.sort_by(|left, right| {
        left.record
            .event
            .occurred_at
            .cmp(&right.record.event.occurred_at)
            .then_with(|| {
                left.record
                    .event
                    .recorded_at
                    .cmp(&right.record.event.recorded_at)
            })
            .then_with(|| left.service.cmp(&right.service))
            .then_with(|| left.record.seq.cmp(&right.record.seq))
    });

    Ok(InvestigationBundle {
        version: INVESTIGATION_BUNDLE_VERSION,
        generated_at: now_ms(),
        selector: selector.display(),
        events,
        gaps,
    })
}

fn add_events(
    service: &str,
    records: Vec<AuditEventRecord>,
    events: &mut BTreeMap<(String, String), BundleEvent>,
    pending: &mut VecDeque<CorrelationLink>,
) -> bool {
    let mut limit_reached = false;
    for record in records {
        let key = (service.to_string(), record.event.event_id.clone());
        if events.contains_key(&key) {
            continue;
        }
        if events.len() >= MAX_BUNDLE_EVENTS {
            limit_reached = true;
            continue;
        }
        for link in correlation_links(&record) {
            pending.push_back(link);
        }
        events.insert(
            key,
            BundleEvent {
                service: service.to_string(),
                record,
            },
        );
    }
    limit_reached
}

fn correlation_links(record: &AuditEventRecord) -> Vec<CorrelationLink> {
    let correlation = &record.event.correlation;
    [
        ("delivery_id", correlation.delivery_id.as_ref()),
        ("controller_id", correlation.controller_id.as_ref()),
        ("action_id", correlation.action_id.as_ref()),
        ("runtime_event_id", correlation.runtime_event_id.as_ref()),
        ("session_id", correlation.session_id.as_ref()),
        ("message_id", correlation.message_id.as_ref()),
        ("write_id", correlation.write_id.as_ref()),
        ("trigger_ref", correlation.trigger_ref.as_ref()),
    ]
    .into_iter()
    .filter_map(|(field, value)| {
        value.as_ref().map(|value| CorrelationLink {
            field: field.into(),
            value: (*value).clone(),
        })
    })
    .collect()
}

async fn fetch_events(
    client: &Client,
    service: &ServiceClient,
    filter: &CorrelationLink,
) -> ReadResult {
    let mut events = Vec::new();
    let mut cursor = None;
    for _ in 0..MAX_PAGES_PER_QUERY {
        match fetch_page(client, service, filter, cursor.as_deref()).await {
            Ok(page) => {
                events.extend(page.events);
                cursor = page.next_cursor;
                if cursor.is_none() {
                    return ReadResult {
                        events,
                        error: None,
                    };
                }
            }
            Err(error) => {
                return ReadResult {
                    events,
                    error: Some(error),
                };
            }
        }
    }
    ReadResult {
        events,
        error: Some(format!("pagination exceeded {MAX_PAGES_PER_QUERY} pages")),
    }
}

async fn fetch_page(
    client: &Client,
    service: &ServiceClient,
    filter: &CorrelationLink,
    cursor: Option<&str>,
) -> std::result::Result<AuditEventPage, String> {
    let url = build_url(service, filter, cursor).map_err(|_| "invalid service URL".to_string())?;
    let target = url_target(&url);
    let mut request = client.get(url);
    if let Some(api_key) = service.ocp_api_key.as_deref() {
        request = request.bearer_auth(api_key);
    }
    if let Some(secret) = service.observer_secret.as_deref() {
        let timestamp = now_unix();
        let mut mac =
            HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
        mac.update(format!("v1\n{timestamp}\nGET\n{target}").as_bytes());
        request = request.header(
            "x-canary-audit-signature-256",
            format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
        );
        request = request.header("x-canary-audit-timestamp", timestamp.to_string());
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "request timed out"
        } else {
            "request failed"
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    response
        .json::<AuditEventPage>()
        .await
        .map_err(|_| "invalid audit API response".into())
}

fn build_url(
    service: &ServiceClient,
    filter: &CorrelationLink,
    cursor: Option<&str>,
) -> Result<Url> {
    let base = service.base_url.trim_end_matches('/');
    let mut url = Url::parse(&format!("{base}{}", service.path))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair(&filter.field, &filter.value);
        query.append_pair("limit", &PAGE_LIMIT.to_string());
        if let Some(cursor) = cursor {
            query.append_pair("cursor", cursor);
        }
    }
    Ok(url)
}

fn url_target(url: &Url) -> String {
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    }
}

fn parse_args(args: Vec<String>) -> Result<Selector> {
    if args.first().map(String::as_str) != Some("investigate") {
        anyhow::bail!(
            "usage: openabctl investigate (--session ID | --delivery ID | --trigger-ref REF)"
        );
    }
    let mut selector = None;
    let mut index = 1;
    while index < args.len() {
        let argument = &args[index];
        let (flag, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(flag, value)| {
                (flag, Some(value))
            });
        let value = if let Some(value) = inline {
            value.to_string()
        } else {
            index += 1;
            args.get(index)
                .cloned()
                .context("investigate selector flag needs a value")?
        };
        if value.is_empty() {
            anyhow::bail!("investigate selector value must not be empty");
        }
        let next = match flag {
            "--session" => Selector::Session(value),
            "--delivery" => Selector::Delivery(value),
            "--trigger-ref" => Selector::TriggerRef(value),
            _ => anyhow::bail!(
                "unknown argument {argument}; usage: openabctl investigate (--session ID | --delivery ID | --trigger-ref REF)"
            ),
        };
        if selector.is_some() {
            anyhow::bail!("provide exactly one investigation selector");
        }
        selector = Some(next);
        index += 1;
    }
    selector.context("one investigation selector is required")
}

fn env_or(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.into())
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_investigation_selectors() {
        assert!(matches!(
            parse_args(vec!["investigate".into(), "--session".into(), "ses_1".into()])
                .unwrap(),
            Selector::Session(value) if value == "ses_1"
        ));
        assert!(parse_args(vec![
            "investigate".into(),
            "--session=ses_1".into(),
            "--delivery=d_1".into()
        ])
        .is_err());
    }

    #[test]
    fn signs_the_exact_query_target_shape() {
        let service = ServiceClient {
            name: "controller",
            base_url: "http://localhost:8091".into(),
            path: "/api/v1/audit/events",
            ocp_api_key: None,
            observer_secret: None,
        };
        let url = build_url(
            &service,
            &CorrelationLink {
                field: "session_id".into(),
                value: "ses_1".into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(
            url_target(&url),
            "/api/v1/audit/events?session_id=ses_1&limit=500"
        );
    }
}
