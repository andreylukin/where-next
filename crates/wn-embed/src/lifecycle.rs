//! Lifecycle of the embedding model on disk, as an explicit state machine.
//!
//! Every legal transition is listed in [`TRANSITIONS`]; anything else is rejected with
//! [`IllegalTransition`] and leaves the state unchanged.
//!
//! Happy path from nothing: `Missing --Fetch--> Downloading --DownloadDone--> Verifying
//! --ChecksumOk--> Loaded`. Files already on disk skip the download: `Missing --LocalFound-->
//! Verifying`. A failed checksum lands in `Corrupt`, which can re-fetch or be reset. A model
//! whose files change on disk (for example an update) is re-verified before it serves again.

use std::fmt;

/// States of the model on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelState {
    /// No model files (or they were evicted).
    Missing,
    /// Files are being fetched from the configured source.
    Downloading,
    /// Files are present and being checked against the manifest.
    Verifying,
    /// Files match the manifest and the model is ready to embed.
    Loaded,
    /// Files are present but do not match the manifest; the model must not be used.
    Corrupt,
}

impl ModelState {
    /// Every state, for exhaustive tests and property strategies.
    pub const ALL: [ModelState; 5] = [
        ModelState::Missing,
        ModelState::Downloading,
        ModelState::Verifying,
        ModelState::Loaded,
        ModelState::Corrupt,
    ];

    /// Only a verified model may produce embeddings. Every other state fails open.
    pub fn can_embed(self) -> bool {
        matches!(self, ModelState::Loaded)
    }
}

/// Events that drive the model lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelEvent {
    /// Start fetching the model from its source.
    Fetch,
    /// Model files were found on disk; verify them without downloading.
    LocalFound,
    /// The download finished.
    DownloadDone,
    /// The download failed.
    DownloadFailed,
    /// Every file matched its expected checksum.
    ChecksumOk,
    /// At least one file was missing or did not match.
    ChecksumMismatch,
    /// Model files changed on disk after loading.
    FilesChanged,
    /// Drop the loaded model and its files from consideration.
    Evict,
    /// Discard corrupt files.
    Reset,
}

impl ModelEvent {
    /// Every event, for exhaustive tests and property strategies.
    pub const ALL: [ModelEvent; 9] = [
        ModelEvent::Fetch,
        ModelEvent::LocalFound,
        ModelEvent::DownloadDone,
        ModelEvent::DownloadFailed,
        ModelEvent::ChecksumOk,
        ModelEvent::ChecksumMismatch,
        ModelEvent::FilesChanged,
        ModelEvent::Evict,
        ModelEvent::Reset,
    ];
}

use ModelEvent as E;
use ModelState as S;

/// The complete transition table: `(from, event, to)`. Pairs not listed are illegal.
pub const TRANSITIONS: &[(ModelState, ModelEvent, ModelState)] = &[
    (S::Missing, E::Fetch, S::Downloading),
    (S::Missing, E::LocalFound, S::Verifying),
    (S::Downloading, E::DownloadDone, S::Verifying),
    (S::Downloading, E::DownloadFailed, S::Missing),
    (S::Verifying, E::ChecksumOk, S::Loaded),
    (S::Verifying, E::ChecksumMismatch, S::Corrupt),
    (S::Loaded, E::FilesChanged, S::Verifying),
    (S::Loaded, E::Evict, S::Missing),
    (S::Corrupt, E::Fetch, S::Downloading),
    (S::Corrupt, E::Reset, S::Missing),
];

/// Returns the next state for a legal `(state, event)` pair.
pub fn next(state: ModelState, event: ModelEvent) -> Option<ModelState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: ModelState,
    pub event: ModelEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in model state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The model lifecycle. The state only changes through [`ModelLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLifecycle {
    state: ModelState,
}

impl Default for ModelLifecycle {
    fn default() -> Self {
        Self {
            state: ModelState::Missing,
        }
    }
}

impl ModelLifecycle {
    /// Current state.
    pub fn state(&self) -> ModelState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(&mut self, event: ModelEvent) -> Result<ModelState, IllegalTransition> {
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
                assert!(!(a == b && e == f), "duplicate transition {a:?} on {e:?}");
            }
        }
    }

    #[test]
    fn illegal_event_leaves_state_unchanged() {
        let mut lc = ModelLifecycle::default();
        let err = lc.handle(ModelEvent::ChecksumOk).unwrap_err();
        assert_eq!(err.state, ModelState::Missing);
        assert_eq!(lc.state(), ModelState::Missing);
    }
}
