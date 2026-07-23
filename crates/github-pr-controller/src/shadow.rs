use crate::planner::ParitySnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowCompareRequest {
    pub comparison_id: String,
    pub delivery_id: String,
    pub event_type: String,
    pub payload: Value,
    pub embedded: Option<ParityOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ParityOutcome {
    Planned { snapshot: Box<ParitySnapshot> },
    Ignored { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowMismatch {
    pub path: String,
    pub class: MismatchClass,
    pub expected: Value,
    pub actual: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MismatchClass {
    IdentityOrOwnership,
    Presentation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowReport {
    pub comparison_id: String,
    pub exact_match: bool,
    pub promotion_blocked: bool,
    pub identity_or_ownership_mismatches: usize,
    pub presentation_mismatches: usize,
    pub mismatches: Vec<ShadowMismatch>,
    pub controller: Option<ParityOutcome>,
}

pub fn compare(
    comparison_id: String,
    embedded: Option<ParityOutcome>,
    controller: Option<ParityOutcome>,
) -> ShadowReport {
    let expected = serde_json::to_value(embedded).expect("parity snapshot serializes");
    let actual = serde_json::to_value(&controller).expect("parity snapshot serializes");
    let mut mismatches = Vec::new();
    compare_value("$", &expected, &actual, &mut mismatches);
    let identity_or_ownership_mismatches = mismatches
        .iter()
        .filter(|mismatch| mismatch.class == MismatchClass::IdentityOrOwnership)
        .count();
    let presentation_mismatches = mismatches.len() - identity_or_ownership_mismatches;
    ShadowReport {
        comparison_id,
        exact_match: mismatches.is_empty(),
        promotion_blocked: identity_or_ownership_mismatches > 0,
        identity_or_ownership_mismatches,
        presentation_mismatches,
        mismatches,
        controller,
    }
}

fn compare_value(path: &str, expected: &Value, actual: &Value, out: &mut Vec<ShadowMismatch>) {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            let keys = expected
                .keys()
                .chain(actual.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                compare_value(
                    &format!("{path}.{key}"),
                    expected.get(&key).unwrap_or(&Value::Null),
                    actual.get(&key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ if expected == actual => {}
        _ => out.push(ShadowMismatch {
            path: path.to_string(),
            class: classify(path),
            expected: expected.clone(),
            actual: actual.clone(),
        }),
    }
}

fn classify(path: &str) -> MismatchClass {
    if path == "$.snapshot.open_session.prompt" {
        MismatchClass::Presentation
    } else {
        MismatchClass::IdentityOrOwnership
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use controller_protocol::OpenSessionAction;
    use std::collections::BTreeMap;

    fn snapshot() -> ParitySnapshot {
        ParitySnapshot {
            repository: "example/repo".into(),
            pr_number: 42,
            reason: "auto".into(),
            preset: Some("full".into()),
            open_session: OpenSessionAction {
                title: "council".into(),
                trigger_ref: Some("github:pr/example/repo#42".into()),
                trigger_fingerprint: Some("sha:abc123".into()),
                roster: vec!["chair".into(), "rev1".into(), "rev2".into()],
                quorum_n: 2,
                chair_bot: Some("chair".into()),
                mode: "review_council".into(),
                prompt: "prompt".into(),
                recipient_inputs: BTreeMap::new(),
            },
            dedupe: crate::planner::DedupeProjection {
                object_key: "github:pr/example/repo#42".into(),
                fingerprint: Some("sha:abc123".into()),
                same_active_fingerprint: "dedupe".into(),
                different_active_fingerprint: "supersede".into(),
            },
            terminal_projection: crate::planner::TerminalProjection {
                result_identity: "github:pr/example/repo#42".into(),
                verdict_source: "terminal_final_message".into(),
                findings_source: Some("openab_findings_block".into()),
            },
            proposed_writes: Vec::new(),
        }
    }

    #[test]
    fn exact_identity_and_presentation_are_distinguished() {
        let expected = ParityOutcome::Planned {
            snapshot: Box::new(snapshot()),
        };
        let exact = compare(
            "exact".into(),
            Some(expected.clone()),
            Some(expected.clone()),
        );
        assert!(exact.exact_match);
        assert!(!exact.promotion_blocked);

        let mut prompt_drift = expected.clone();
        let ParityOutcome::Planned { snapshot } = &mut prompt_drift else {
            unreachable!();
        };
        snapshot.open_session.prompt = "different copy".into();
        let report = compare("prompt".into(), Some(expected.clone()), Some(prompt_drift));
        assert_eq!(report.presentation_mismatches, 1);
        assert_eq!(report.identity_or_ownership_mismatches, 0);
        assert!(!report.promotion_blocked);

        let mut roster_drift = expected.clone();
        let ParityOutcome::Planned { snapshot } = &mut roster_drift else {
            unreachable!();
        };
        snapshot.open_session.roster.pop();
        let report = compare("roster".into(), Some(expected), Some(roster_drift));
        assert_eq!(report.identity_or_ownership_mismatches, 1);
        assert!(report.promotion_blocked);
    }

    #[test]
    fn trigger_presence_mismatch_is_blocking() {
        let report = compare(
            "missing".into(),
            Some(ParityOutcome::Planned {
                snapshot: Box::new(snapshot()),
            }),
            None,
        );
        assert!(report.promotion_blocked);
        assert_eq!(report.identity_or_ownership_mismatches, 1);
    }

    #[test]
    fn ignored_reason_drift_is_blocking() {
        let report = compare(
            "ignored".into(),
            Some(ParityOutcome::Ignored {
                reason: "not_a_trigger".into(),
            }),
            Some(ParityOutcome::Ignored {
                reason: "author_not_trusted".into(),
            }),
        );
        assert!(report.promotion_blocked);
        assert_eq!(report.identity_or_ownership_mismatches, 1);
        assert_eq!(report.mismatches[0].path, "$.reason");
    }
}
