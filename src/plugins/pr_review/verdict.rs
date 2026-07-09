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
}
