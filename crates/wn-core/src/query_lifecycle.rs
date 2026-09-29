//! Lifecycle of one query: it either answers, abstains, or fails open to normal search.
//!
//! `Received --Ranked--> Ranked --Confident--> Answer` or `--NotConfident--> Abstain`. Any
//! operational problem (no index, model missing, unsupported repository) goes to `FailOpen`,
//! which tells the caller to use its ordinary search instead of trusting an empty hint.

use crate::machine::{step, Illegal};

/// States of a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryState {
    /// The query arrived.
    Received,
    /// Candidates were ranked; confidence not yet decided.
    Ranked,
    /// Confident hints are returned.
    Answer,
    /// Not confident enough; no hints.
    Abstain,
    /// Could not rank; the caller should use normal search.
    FailOpen,
}

impl QueryState {
    /// Every state, for exhaustive tests.
    pub const ALL: [QueryState; 5] = [
        QueryState::Received,
        QueryState::Ranked,
        QueryState::Answer,
        QueryState::Abstain,
        QueryState::FailOpen,
    ];

    /// Whether the query is finished.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            QueryState::Answer | QueryState::Abstain | QueryState::FailOpen
        )
    }

    /// Whether hints are shown to the caller.
    pub fn shows_hints(self) -> bool {
        self == QueryState::Answer
    }
}

/// Events of a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryEvent {
    /// Candidates were ranked.
    Ranked,
    /// The top result passed the abstain thresholds.
    Confident,
    /// The top result did not pass the abstain thresholds.
    NotConfident,
    /// Ranking is impossible (no index, model missing, error).
    Unavailable,
}

impl QueryEvent {
    /// Every event, for exhaustive tests.
    pub const ALL: [QueryEvent; 4] = [
        QueryEvent::Ranked,
        QueryEvent::Confident,
        QueryEvent::NotConfident,
        QueryEvent::Unavailable,
    ];
}

use QueryEvent as E;
use QueryState as S;

/// The complete transition table.
pub const TRANSITIONS: &[(QueryState, QueryEvent, QueryState)] = &[
    (S::Received, E::Ranked, S::Ranked),
    (S::Received, E::Unavailable, S::FailOpen),
    (S::Ranked, E::Confident, S::Answer),
    (S::Ranked, E::NotConfident, S::Abstain),
    (S::Ranked, E::Unavailable, S::FailOpen),
];

/// One query's lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryLifecycle {
    state: QueryState,
}

impl Default for QueryLifecycle {
    fn default() -> Self {
        Self { state: S::Received }
    }
}

impl QueryLifecycle {
    /// Current state.
    pub fn state(&self) -> QueryState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(
        &mut self,
        event: QueryEvent,
    ) -> Result<QueryState, Illegal<QueryState, QueryEvent>> {
        self.state = step(TRANSITIONS, self.state, event)?;
        Ok(self.state)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_has_no_duplicate_pairs() {
        crate::machine::assert_unique(super::TRANSITIONS);
    }
}
