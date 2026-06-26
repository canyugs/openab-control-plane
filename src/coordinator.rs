//! Coordination policy (the pluggable lifecycle seam). The orchestrator owns the
//! *mechanics* (fanout, state transitions, delivery, emitting events); a
//! `Coordinator` owns the *policy* — who synthesizes, when the group has
//! converged, and what to tell the synthesizer. v1 ships one mode
//! (`QuorumCouncil`); a second mode is a new impl, no orchestrator change.

use crate::session::quorum_reached;
use crate::store::Session;

pub trait Coordinator: Send + Sync {
    fn kind(&self) -> &'static str;

    /// The bot that synthesizes the outcome and whose done-signal closes the
    /// session. `None` = no designated synthesizer (the session won't self-close
    /// on a synthesizer signal).
    fn synthesizer<'a>(&self, session: &'a Session) -> Option<&'a str>;

    /// Have the participants converged enough to prompt the synthesizer?
    /// `done` = bot ids that posted the done-signal; `roster` is the session's
    /// members (the orchestrator supplies both so the policy stays store-free).
    fn converged(&self, session: &Session, roster: &[String], done: &[String]) -> bool;

    /// The message the plane delivers to the synthesizer on convergence.
    fn converge_prompt(&self) -> &str;
}

/// v1 lifecycle: reviewers (roster minus chair) signal done; once `quorum_n` of
/// them have, the chair synthesizes and the chair's own done closes the session.
/// Encodes exactly the behaviour the orchestrator had inline.
pub struct QuorumCouncil;

impl Coordinator for QuorumCouncil {
    fn kind(&self) -> &'static str {
        "quorum_council"
    }

    fn synthesizer<'a>(&self, session: &'a Session) -> Option<&'a str> {
        session.chair_bot.as_deref()
    }

    fn converged(&self, session: &Session, roster: &[String], done: &[String]) -> bool {
        quorum_reached(roster, session.chair_bot.as_deref(), done, session.quorum_n)
    }

    fn converge_prompt(&self) -> &str {
        "Quorum reached. Chair, please render the verdict."
    }
}

/// Pick the coordinator for a session. Today there is one mode; a second mode
/// selects here (e.g. on a `session.mode` field) — the only place that changes.
pub fn for_session(_session: &Session) -> Box<dyn Coordinator> {
    Box::new(QuorumCouncil)
}
