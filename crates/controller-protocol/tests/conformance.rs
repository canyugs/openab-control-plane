use controller_protocol::{
    ActionEnvelope, ActionResultEnvelope, ControllerAction, ControllerActionResult, ErrorCode,
    ErrorEnvelope, OpenSessionAction, PostMessageAction, ProtocolError, VersionOffer,
    CURRENT_VERSION,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;

fn assert_golden_round_trip<T>(value: &T, golden: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let actual = serde_json::to_string_pretty(value).expect("fixture serializes");
    assert_eq!(actual, golden.trim_end());
    let decoded: T = serde_json::from_str(golden).expect("golden deserializes");
    assert_eq!(&decoded, value);
}

#[test]
fn version_offer_is_golden_pinned() {
    assert_golden_round_trip(
        &VersionOffer::default(),
        include_str!("golden/version_offer.json"),
    );
}

#[test]
fn action_envelopes_are_golden_pinned() {
    let open = ActionEnvelope {
        version: CURRENT_VERSION,
        action_id: "act_open_001".into(),
        action: ControllerAction::OpenSession(OpenSessionAction {
            title: "incident council".into(),
            trigger_ref: Some("source:item/42".into()),
            trigger_fingerprint: Some("revision:abc123".into()),
            roster: vec!["chair".into(), "analyst".into()],
            quorum_n: 1,
            chair_bot: Some("chair".into()),
            mode: "council".into(),
            prompt: "Investigate item 42.".into(),
            recipient_inputs: BTreeMap::from([
                ("analyst".into(), "Focus on failure evidence.".into()),
                ("chair".into(), "Synthesize the terminal report.".into()),
            ]),
        }),
    };
    assert_golden_round_trip(&open, include_str!("golden/action_open_session.json"));

    let post = ActionEnvelope {
        version: CURRENT_VERSION,
        action_id: "act_post_001".into(),
        action: ControllerAction::PostMessage(PostMessageAction {
            session_id: "ses_001".into(),
            content: "Additional source context.".into(),
        }),
    };
    assert_golden_round_trip(&post, include_str!("golden/action_post_message.json"));
}

#[test]
fn action_results_are_golden_pinned() {
    let results = vec![
        ActionResultEnvelope {
            version: CURRENT_VERSION,
            action_id: "act_open_001".into(),
            result: ControllerActionResult::SessionOpened {
                session_id: "ses_001".into(),
                deduped: false,
            },
        },
        ActionResultEnvelope {
            version: CURRENT_VERSION,
            action_id: "act_open_002".into(),
            result: ControllerActionResult::Superseded {
                session_id: "ses_002".into(),
                old_id: "ses_001".into(),
            },
        },
        ActionResultEnvelope {
            version: CURRENT_VERSION,
            action_id: "act_post_001".into(),
            result: ControllerActionResult::MessagePosted {
                message_id: "msg_001".into(),
            },
        },
    ];
    assert_golden_round_trip(&results, include_str!("golden/action_results.json"));
}

#[test]
fn stable_error_envelopes_are_golden_pinned() {
    let codes = [
        ErrorCode::InvalidRequest,
        ErrorCode::UnsupportedVersion,
        ErrorCode::Unauthorized,
        ErrorCode::Forbidden,
        ErrorCode::NotFound,
        ErrorCode::Gone,
        ErrorCode::Conflict,
        ErrorCode::RateLimited,
        ErrorCode::Internal,
    ];
    let errors = codes
        .into_iter()
        .enumerate()
        .map(|(index, code)| ErrorEnvelope {
            version: CURRENT_VERSION,
            action_id: Some(format!("act_error_{index:02}")),
            error: ProtocolError {
                code,
                message: "stable public message".into(),
                retryable: matches!(code, ErrorCode::RateLimited | ErrorCode::Internal),
            },
        })
        .collect::<Vec<_>>();
    assert_golden_round_trip(&errors, include_str!("golden/errors.json"));
}

#[test]
fn unsupported_version_error_is_golden_pinned() {
    assert_golden_round_trip(
        &ErrorEnvelope::unsupported_version(Some("act_new".into()), 99),
        include_str!("golden/error_unsupported_version.json"),
    );
}

#[test]
fn open_session_defaults_recipient_inputs_for_older_payloads() {
    let action: ControllerAction = serde_json::from_value(serde_json::json!({
        "type": "open_session",
        "params": {
            "title": "compatibility session",
            "roster": ["chair"],
            "quorum_n": 1,
            "mode": "solo",
            "prompt": "Inspect the item."
        }
    }))
    .expect("payload without recipient_inputs remains valid");

    let ControllerAction::OpenSession(action) = action else {
        panic!("expected open_session action");
    };
    assert!(action.recipient_inputs.is_empty());
}
