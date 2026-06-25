//! Session lifecycle helpers (design §9). The state machine transitions live in
//! the orchestrator; the deterministic *quorum* rule lives here, pure + tested.
//!
//! Quorum = distinct reviewers who signalled "done" (🆗 reaction), per design.
//! The chair is never counted as a reviewer vote.

pub const DONE_EMOJI: &str = "🆗";

/// Reviewers that count toward quorum = roster minus chair.
pub fn reviewers(roster: &[String], chair: Option<&str>) -> Vec<String> {
    roster
        .iter()
        .filter(|b| Some(b.as_str()) != chair)
        .cloned()
        .collect()
}

/// Has quorum been reached? `done` = bot ids that posted the done signal.
/// Only reviewers (roster minus chair) count, deduped, compared to quorum_n.
pub fn quorum_reached(
    roster: &[String],
    chair: Option<&str>,
    done: &[String],
    quorum_n: i64,
) -> bool {
    let revs = reviewers(roster, chair);
    let count = revs.iter().filter(|r| done.contains(r)).count() as i64;
    count >= quorum_n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<String> {
        vec!["chair".into(), "rev1".into(), "rev2".into(), "rev3".into()]
    }

    #[test]
    fn chair_excluded_from_reviewers() {
        let revs = reviewers(&roster(), Some("chair"));
        assert_eq!(revs.len(), 3);
        assert!(!revs.contains(&"chair".to_string()));
    }

    #[test]
    fn quorum_not_reached_below_threshold() {
        let done = vec!["rev1".to_string()];
        assert!(!quorum_reached(&roster(), Some("chair"), &done, 2));
    }

    #[test]
    fn quorum_reached_at_threshold() {
        let done = vec!["rev1".to_string(), "rev2".to_string()];
        assert!(quorum_reached(&roster(), Some("chair"), &done, 2));
    }

    #[test]
    fn chair_done_signal_does_not_count() {
        // chair reacting 🆗 must not satisfy quorum
        let done = vec!["chair".to_string(), "rev1".to_string()];
        assert!(!quorum_reached(&roster(), Some("chair"), &done, 2));
    }

    #[test]
    fn duplicate_done_signals_count_once() {
        // dedup is the store's job (DISTINCT), but the rule must be idempotent
        let done = vec!["rev1".to_string(), "rev1".to_string()];
        assert!(!quorum_reached(&roster(), Some("chair"), &done, 2));
    }
}
