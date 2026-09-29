//! Lifecycle of a repository's personal adapter.
//!
//! `None --Fit--> Fitting --FitDone--> Active`. A refit from `Active` keeps serving the old
//! weights (`Refitting`) and falls back to them if it fails. When the embedding model changes,
//! the weights no longer apply (`Invalidated`: queries use the identity map) until a new fit
//! finishes. A fit that was running when the model changed is discarded.

use crate::machine::{step, Illegal};

/// States of the personal adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterState {
    /// No weights; queries use the identity map.
    None,
    /// First fit in progress.
    Fitting,
    /// Weights fitted for the current model are applied.
    Active,
    /// A refit is in progress; the previous weights keep being applied.
    Refitting,
    /// Weights exist but were fitted for a different model; not applied.
    Invalidated,
}

impl AdapterState {
    /// Every state, for exhaustive tests.
    pub const ALL: [AdapterState; 5] = [
        AdapterState::None,
        AdapterState::Fitting,
        AdapterState::Active,
        AdapterState::Refitting,
        AdapterState::Invalidated,
    ];

    /// Whether queries are mapped through the adapter in this state.
    pub fn applies(self) -> bool {
        matches!(self, AdapterState::Active | AdapterState::Refitting)
    }
}

/// Events that drive the adapter lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterEvent {
    /// Start fitting (first fit, refit after new commits, or after a model change).
    Fit,
    /// The fit produced weights.
    FitDone,
    /// The fit failed (too few usable examples, or an error).
    FitFailed,
    /// The embedding model changed.
    ModelChanged,
    /// The user removed the adapter (`wn rollback` past the first fit, or `--no-adapter` reset).
    Discard,
}

impl AdapterEvent {
    /// Every event, for exhaustive tests.
    pub const ALL: [AdapterEvent; 5] = [
        AdapterEvent::Fit,
        AdapterEvent::FitDone,
        AdapterEvent::FitFailed,
        AdapterEvent::ModelChanged,
        AdapterEvent::Discard,
    ];
}

use AdapterEvent as E;
use AdapterState as S;

/// The complete transition table.
pub const TRANSITIONS: &[(AdapterState, AdapterEvent, AdapterState)] = &[
    (S::None, E::Fit, S::Fitting),
    (S::None, E::ModelChanged, S::None),
    (S::Fitting, E::FitDone, S::Active),
    (S::Fitting, E::FitFailed, S::None),
    (S::Fitting, E::ModelChanged, S::None),
    (S::Active, E::Fit, S::Refitting),
    (S::Active, E::ModelChanged, S::Invalidated),
    (S::Active, E::Discard, S::None),
    (S::Refitting, E::FitDone, S::Active),
    (S::Refitting, E::FitFailed, S::Active),
    (S::Refitting, E::ModelChanged, S::Invalidated),
    (S::Invalidated, E::Fit, S::Fitting),
    (S::Invalidated, E::ModelChanged, S::Invalidated),
    (S::Invalidated, E::Discard, S::None),
];

/// The adapter lifecycle. The state only changes through [`AdapterLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterLifecycle {
    state: AdapterState,
}

impl Default for AdapterLifecycle {
    fn default() -> Self {
        Self { state: S::None }
    }
}

impl AdapterLifecycle {
    /// Starts in a given state (e.g. `Active` when weights for the current model are on disk).
    pub fn starting_in(state: AdapterState) -> Self {
        Self { state }
    }

    /// Current state.
    pub fn state(&self) -> AdapterState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(
        &mut self,
        event: AdapterEvent,
    ) -> Result<AdapterState, Illegal<AdapterState, AdapterEvent>> {
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
