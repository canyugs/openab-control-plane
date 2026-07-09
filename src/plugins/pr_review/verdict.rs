use crate::orchestrator::unfenced_lines;

/// Parsed `[[verdict:…]]` trailer (ADR 013): chair decision + optional 🔴/🟡/🟢 counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictTrailer {
    pub decision: String, // "approve" | "request_changes"
    pub red: Option<i64>,
    pub yellow: Option<i64>,
    pub green: Option<i64>,
}

pub(crate) fn parse_verdict_trailer_line(line: &str) -> Option<VerdictTrailer> {
    let start = line.rfind("[[verdict:")?;
    let rest = &line[start + "[[verdict:".len()..];
    let inner = &rest[..rest.find("]]")?];
    let mut parts = inner.split_whitespace();
    let decision = parts.next()?;
    if decision != "approve" && decision != "request_changes" {
        return None;
    }
    let (mut red, mut yellow, mut green) = (None, None, None);
    for part in parts {
        let (key, value) = part.split_once('=')?;
        let n: i64 = value.parse().ok().filter(|n| *n >= 0)?;
        match key {
            "r" => red = Some(n),
            "y" => yellow = Some(n),
            "g" => green = Some(n),
            _ => return None,
        }
    }
    Some(VerdictTrailer {
        decision: decision.to_string(),
        red,
        yellow,
        green,
    })
}

/// Parse `[[verdict:approve|request_changes r=N y=N g=N]]` only from the final
/// non-empty unfenced line of the chair's final message (ADR 013). Counts are
/// optional. If multiple trailers occur on that final line, the last one wins.
/// An unknown decision or any malformed part rejects the whole trailer (None) —
/// the session then closes with NULLs, today's prose-only behavior.
pub fn parse_verdict_trailer(text: &str) -> Option<VerdictTrailer> {
    let line = unfenced_lines(text).into_iter().next_back()?;
    parse_verdict_trailer_line(line)
}

pub(crate) fn trailer(text: &str) -> Option<VerdictTrailer> {
    parse_verdict_trailer(text)
}

#[cfg(test)]
mod tests {
    use crate::store::Store as _;
    use super::*;

    #[test]
    fn verdict_trailer_parsing() {
        // Full form, embedded in a real chair final.
        let t = parse_verdict_trailer(
            "Report…\n\nVerdict: request changes\n[[verdict:request_changes r=1 y=3 g=5]] [done]",
        )
        .unwrap();
        assert_eq!(t.decision, "request_changes");
        assert_eq!((t.red, t.yellow, t.green), (Some(1), Some(3), Some(5)));

        // Decision only — counts optional.
        let t = parse_verdict_trailer("LGTM [[verdict:approve]] [done]").unwrap();
        assert_eq!(t.decision, "approve");
        assert_eq!((t.red, t.yellow, t.green), (None, None, None));

        // Last trailer wins (chair quoted an earlier draft).
        let t =
            parse_verdict_trailer("[[verdict:approve]] … [[verdict:request_changes r=2]]").unwrap();
        assert_eq!(t.decision, "request_changes");
        assert_eq!(t.red, Some(2));

        let t = parse_verdict_trailer(
            "quoted bad draft:\n> [[verdict:maybe r=1]]\n\n[[verdict:approve r=0 y=1 g=2]] [done]",
        )
        .unwrap();
        assert_eq!(t.decision, "approve");
        assert_eq!((t.red, t.yellow, t.green), (Some(0), Some(1), Some(2)));

        assert!(parse_verdict_trailer("[[verdict:approve]]\nfinal prose after trailer").is_none());
        assert!(parse_verdict_trailer("```\n[[verdict:approve]] [done]\n```").is_none());
        assert_eq!(
            parse_verdict_trailer("[[verdict:approve]] [done]")
                .unwrap()
                .decision,
            "approve"
        );

        // Malformed → None, never a partial parse.
        assert!(parse_verdict_trailer("no trailer here [done]").is_none());
        assert!(parse_verdict_trailer("[[verdict:maybe r=1]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve r=lots]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve r=-1]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve x=1]]").is_none());
        assert!(parse_verdict_trailer("[[verdict:approve r=1").is_none()); // unclosed
    }

    #[tokio::test]
    async fn review_close_webhook_reads_structured_verdict_columns() {
        let (webhook_url, mut webhook_rx) = crate::orchestrator::test_support::spawn_close_webhook_listener().await;
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new_with_options(
            store.clone(),
            None,
            None,
            None,
            None,
            "http://control-plane.zeabur.internal:8090".to_string(),
            Some(webhook_url),
        );
        let chair = store.register_bot("chair", "chair", "h1", "t1").unwrap();
        let session = store
            .create_session(
                "review",
                None,
                0,
                Some(&chair.id),
                std::slice::from_ref(&chair.id),
                "review_council",
            )
            .unwrap();
        store
            .advance_state(&session.id, crate::store::SessionState::Open, crate::store::SessionState::Quorum)
            .unwrap();

        crate::orchestrator::handle_reply(
            &state,
            &chair.id,
            crate::orchestrator::test_support::msg_reply(
                &session.id,
                "VERDICT: approve [[verdict:approve r=1 y=0 g=2]] [done]",
            ),
        )
        .unwrap();

        let payload = tokio::time::timeout(std::time::Duration::from_secs(5), webhook_rx.recv())
            .await
            .expect("timed out waiting for close webhook")
            .expect("close webhook listener stopped");
        assert_eq!(payload["decision"], "approve");
        assert_eq!(payload["findings_red"], 1);
        assert_eq!(payload["findings_yellow"], 0);
        assert_eq!(payload["findings_green"], 2);
    }

    #[test]
    fn solo_close_keeps_structured_verdict_null_even_with_trailer() {
        let store = std::sync::Arc::new(crate::store::SqliteStore::memory().unwrap());
        let state = crate::state::AppState::new(store.clone());
        let bot = store.register_bot("solo", "reviewer", "h1", "t1").unwrap();
        let session = store
            .create_session("solo", None, 0, None, std::slice::from_ref(&bot.id), "solo")
            .unwrap();
        store
            .advance_state(&session.id, crate::store::SessionState::Open, crate::store::SessionState::Deliberating)
            .unwrap();
        let mut north = state.north_tx.subscribe();

        crate::orchestrator::handle_reply(
            &state,
            &bot.id,
            crate::orchestrator::test_support::msg_reply(
                &session.id,
                "solo final [[verdict:approve r=1 y=0 g=2]] [done]",
            ),
        )
        .unwrap();

        let closed = store.session(&session.id).unwrap().unwrap();
        assert_eq!(
            crate::store::SessionState::from_db_str(&closed.state),
            crate::store::SessionState::Closed
        );
        assert!(closed.decision.is_none());
        assert!(closed.findings_red.is_none());
        assert!(closed.findings_yellow.is_none());
        assert!(closed.findings_green.is_none());

        let mut verdict_event = None;
        while let Ok(raw) = north.try_recv() {
            let event: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if event["type"] == "verdict" {
                verdict_event = Some(event);
                break;
            }
        }
        let event = verdict_event.expect("solo close should emit a north verdict event");
        assert!(event["payload"]["decision"].is_null());
        assert!(event["payload"]["findings_red"].is_null());
        assert!(event["payload"]["findings_yellow"].is_null());
        assert!(event["payload"]["findings_green"].is_null());
    }

}
