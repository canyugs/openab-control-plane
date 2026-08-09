//! Argument grammar for `session.intent` payloads. The kernel transports
//! `{verb, args}` raw on purpose (ADR 039); the grammar — `key=value` pairs,
//! values bare words or double-quoted strings — is this controller's to own.

use std::collections::BTreeMap;

pub const DEFAULT_NS: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Delegate {
        ns: String,
        /// Task id; caller-chosen via `id=` so later intents can reference it
        /// in `deps=`, else the controller mints one.
        id: Option<String>,
        to: String,
        spec: String,
        deps: Vec<String>,
    },
    Status {
        ns: String,
        task: Option<String>,
    },
}

/// `Ok(None)` is an unknown verb — the kernel transports any verb, unknown
/// ones are logged and ignored. `Err` is a known verb with bad arguments,
/// which gets a rejection reply into the asking session.
pub fn parse(verb: &str, args: &str) -> Result<Option<Intent>, String> {
    let pairs = match verb {
        "delegate" | "status" => parse_args(args)?,
        _ => return Ok(None),
    };
    let ns = pairs
        .get("ns")
        .cloned()
        .unwrap_or_else(|| DEFAULT_NS.to_string());
    if !valid_name(&ns) {
        return Err(format!("invalid ns '{ns}'"));
    }
    match verb {
        "delegate" => {
            let to = pairs
                .get("to")
                .cloned()
                .ok_or_else(|| "delegate needs to=<bot_id|role>".to_string())?;
            let spec = pairs
                .get("task")
                .cloned()
                .ok_or_else(|| "delegate needs task=\"…\"".to_string())?;
            if spec.trim().is_empty() {
                return Err("delegate task must not be empty".into());
            }
            let deps: Vec<String> = pairs
                .get("deps")
                .map(|raw| {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let id = pairs.get("id").cloned();
            for name in [&id, &Some(to.clone())].into_iter().flatten() {
                if !valid_name(name) {
                    return Err(format!("invalid identifier '{name}'"));
                }
            }
            for dep in &deps {
                if !valid_name(dep) {
                    return Err(format!("invalid dep id '{dep}'"));
                }
            }
            Ok(Some(Intent::Delegate {
                ns,
                id,
                to,
                spec,
                deps,
            }))
        }
        "status" => Ok(Some(Intent::Status {
            ns,
            task: pairs.get("task").cloned(),
        })),
        _ => unreachable!(),
    }
}

/// `key=value` pairs separated by whitespace. Values are bare words or
/// double-quoted strings.
// ponytail: no escape sequences inside quotes — a task spec containing a
// literal `"` needs a richer grammar (JSON body per ADR 039's open question).
fn parse_args(args: &str) -> Result<BTreeMap<String, String>, String> {
    let mut pairs = BTreeMap::new();
    let mut rest = args.trim_start();
    while !rest.is_empty() {
        let Some((key, after_eq)) = rest.split_once('=') else {
            return Err(format!("expected key=value at '{rest}'"));
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(format!("invalid key before '={after_eq}'"));
        }
        let (value, tail) = if let Some(quoted) = after_eq.strip_prefix('"') {
            let Some(end) = quoted.find('"') else {
                return Err("unterminated quoted value".into());
            };
            (&quoted[..end], &quoted[end + 1..])
        } else {
            match after_eq.find(char::is_whitespace) {
                Some(end) => (&after_eq[..end], &after_eq[end..]),
                None => (after_eq, ""),
            }
        };
        if pairs.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("duplicate key '{key}'"));
        }
        rest = tail.trim_start();
    }
    Ok(pairs)
}

/// Task/bot identifiers share the wire identifier alphabet so they can ride
/// scope strings and trigger refs unquoted.
pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegate_parses_quoted_spec_deps_and_defaults() {
        let intent = parse(
            "delegate",
            "to=b2 task=\"summarize the findings, then stop\" deps=b1a,b1b id=summary",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            intent,
            Intent::Delegate {
                ns: DEFAULT_NS.into(),
                id: Some("summary".into()),
                to: "b2".into(),
                spec: "summarize the findings, then stop".into(),
                deps: vec!["b1a".into(), "b1b".into()],
            }
        );

        let minimal = parse("delegate", "to=worker task=\"do it\"")
            .unwrap()
            .unwrap();
        assert_eq!(
            minimal,
            Intent::Delegate {
                ns: DEFAULT_NS.into(),
                id: None,
                to: "worker".into(),
                spec: "do it".into(),
                deps: vec![],
            }
        );
    }

    #[test]
    fn status_parses_with_and_without_task() {
        assert_eq!(
            parse("status", "").unwrap().unwrap(),
            Intent::Status {
                ns: DEFAULT_NS.into(),
                task: None
            }
        );
        assert_eq!(
            parse("status", "task=summary ns=demo").unwrap().unwrap(),
            Intent::Status {
                ns: "demo".into(),
                task: Some("summary".into())
            }
        );
    }

    #[test]
    fn unknown_verbs_are_none_and_bad_args_are_errors() {
        assert_eq!(parse("teleport", "to=mars").unwrap(), None);
        assert!(parse("delegate", "task=\"no assignee\"").is_err());
        assert!(parse("delegate", "to=b2").is_err());
        assert!(parse("delegate", "to=b2 task=\"unterminated").is_err());
        assert!(parse("delegate", "to=b2 task=\"x\" deps=ok,../evil").is_err());
        assert!(parse("delegate", "stray to=b2 task=\"x\"").is_err());
    }
}
