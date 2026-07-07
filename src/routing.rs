//! Client/system fanout policy (design §10): trust = session roster. Client- or
//! system-authored messages can fan to the full roster; bot-authored messages
//! are stored + emitted north and only reach peers through Relay or backfill.
//! Pure + testable.

/// Recipients of a message authored by `author` (None = client/system author).
pub fn fanout_targets(roster: &[String], author: Option<&str>) -> Vec<String> {
    roster
        .iter()
        .filter(|b| Some(b.as_str()) != author)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<String> {
        vec!["chair".into(), "rev1".into(), "rev2".into()]
    }

    #[test]
    fn client_message_reaches_all_bots() {
        assert_eq!(fanout_targets(&roster(), None).len(), 3);
    }

    #[test]
    fn author_never_receives_own_message() {
        let t = fanout_targets(&roster(), Some("rev1"));
        assert_eq!(t, vec!["chair".to_string(), "rev2".to_string()]);
        assert!(!t.contains(&"rev1".to_string()));
    }

    #[test]
    fn non_member_author_still_fans_to_all() {
        // a sender not in the roster (e.g. system) → everyone gets it
        assert_eq!(fanout_targets(&roster(), Some("stranger")).len(), 3);
    }
}
