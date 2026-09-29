//! Lifecycle of a repository index, as an explicit state machine.
//!
//! Every legal transition is listed in [`TRANSITIONS`]. Anything not listed is rejected
//! with [`IllegalTransition`], so the index can never drift into an undefined state.
//!
//! Happy path: `Uninitialized --Init--> Indexing --IndexDone--> Ready --FilesChanged--> Stale
//! --Refresh--> Refreshing --RefreshDone--> Ready`. Failures go to `Error`, which can retry
//! (`Init`) or `Reset`. `ModelChanged` from any state returns to `Uninitialized`, because stored
//! vectors are only comparable with the model that produced them.

use std::fmt;

/// States of a repository index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexState {
    /// No usable vectors (never built, or invalidated by a model change).
    Uninitialized,
    /// First full build in progress.
    Indexing,
    /// Vectors match the working tree.
    Ready,
    /// Files changed since the last build; results are still served, flagged as stale.
    Stale,
    /// Re-embedding changed files; the previous snapshot keeps serving.
    Refreshing,
    /// The last build or refresh failed.
    Error,
}

impl IndexState {
    /// Every state, for exhaustive tests and property strategies.
    pub const ALL: [IndexState; 6] = [
        IndexState::Uninitialized,
        IndexState::Indexing,
        IndexState::Ready,
        IndexState::Stale,
        IndexState::Refreshing,
        IndexState::Error,
    ];

    /// Whether a query can be answered from this state. Other states fail open: the caller
    /// falls back to ordinary search instead of receiving an empty hint.
    pub fn can_serve(self) -> bool {
        matches!(
            self,
            IndexState::Ready | IndexState::Stale | IndexState::Refreshing
        )
    }
}

/// Events that drive the index lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexEvent {
    /// Start a full build.
    Init,
    /// The full build finished.
    IndexDone,
    /// The full build failed.
    IndexFailed,
    /// The file watcher saw changes in the working tree.
    FilesChanged,
    /// Start re-embedding changed files.
    Refresh,
    /// The refresh finished.
    RefreshDone,
    /// The refresh failed.
    RefreshFailed,
    /// Discard a failed index.
    Reset,
    /// The embedding model changed; stored vectors are no longer comparable.
    ModelChanged,
}

impl IndexEvent {
    /// Every event, for exhaustive tests and property strategies.
    pub const ALL: [IndexEvent; 9] = [
        IndexEvent::Init,
        IndexEvent::IndexDone,
        IndexEvent::IndexFailed,
        IndexEvent::FilesChanged,
        IndexEvent::Refresh,
        IndexEvent::RefreshDone,
        IndexEvent::RefreshFailed,
        IndexEvent::Reset,
        IndexEvent::ModelChanged,
    ];
}

use IndexEvent as E;
use IndexState as S;

/// The complete transition table: `(from, event, to)`. Pairs not listed are illegal.
pub const TRANSITIONS: &[(IndexState, IndexEvent, IndexState)] = &[
    (S::Uninitialized, E::Init, S::Indexing),
    (S::Uninitialized, E::ModelChanged, S::Uninitialized),
    (S::Indexing, E::IndexDone, S::Ready),
    (S::Indexing, E::IndexFailed, S::Error),
    // Changes during a build are picked up by the watcher's next event after it finishes.
    (S::Indexing, E::FilesChanged, S::Indexing),
    (S::Indexing, E::ModelChanged, S::Uninitialized),
    (S::Ready, E::FilesChanged, S::Stale),
    (S::Ready, E::ModelChanged, S::Uninitialized),
    (S::Stale, E::FilesChanged, S::Stale),
    (S::Stale, E::Refresh, S::Refreshing),
    (S::Stale, E::ModelChanged, S::Uninitialized),
    (S::Refreshing, E::RefreshDone, S::Ready),
    (S::Refreshing, E::RefreshFailed, S::Error),
    (S::Refreshing, E::FilesChanged, S::Refreshing),
    (S::Refreshing, E::ModelChanged, S::Uninitialized),
    (S::Error, E::Init, S::Indexing),
    (S::Error, E::Reset, S::Uninitialized),
    (S::Error, E::ModelChanged, S::Uninitialized),
];

/// Returns the next state for a legal `(state, event)` pair.
pub fn next(state: IndexState, event: IndexEvent) -> Option<IndexState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: IndexState,
    pub event: IndexEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The index lifecycle. The state only changes through [`IndexLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLifecycle {
    state: IndexState,
}

impl Default for IndexLifecycle {
    fn default() -> Self {
        Self {
            state: IndexState::Uninitialized,
        }
    }
}

impl IndexLifecycle {
    /// Current state.
    pub fn state(&self) -> IndexState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(&mut self, event: IndexEvent) -> Result<IndexState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_duplicate_pairs() {
        for (i, (a, e, _)) in TRANSITIONS.iter().enumerate() {
            for (b, f, _) in &TRANSITIONS[i + 1..] {
                assert!(
                    !(a == b && e == f),
                    "duplicate transition for {a:?} on {e:?}"
                );
            }
        }
    }

    #[test]
    fn illegal_event_leaves_state_unchanged() {
        let mut lc = IndexLifecycle::default();
        let err = lc.handle(IndexEvent::RefreshDone).unwrap_err();
        assert_eq!(err.state, IndexState::Uninitialized);
        assert_eq!(lc.state(), IndexState::Uninitialized);
    }
}
