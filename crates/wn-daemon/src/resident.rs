//! Lifecycle of the per-user background daemon process (the socket server behind `wn ask`), as an
//! explicit state machine.
//!
//! `Starting --Bound--> Listening`. Only `Listening` accepts requests; each request keeps it
//! there. An idle timeout or a shutdown request moves it to `Draining`, which finishes in-flight
//! work, removes the socket and ends in `Stopped`. A bind failure (another daemon owns the socket,
//! or the path is unusable) ends in `Failed`; clients then answer in-process.

use std::fmt;

/// States of the daemon process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResidentState {
    /// Process started; socket not bound yet.
    Starting,
    /// Socket bound; requests are accepted.
    Listening,
    /// Idle timeout or shutdown: no new requests, finishing in-flight ones.
    Draining,
    /// Terminal: socket removed, process exits.
    Stopped,
    /// Terminal: the socket could not be bound.
    Failed,
}

impl ResidentState {
    pub const ALL: [ResidentState; 5] = [
        ResidentState::Starting,
        ResidentState::Listening,
        ResidentState::Draining,
        ResidentState::Stopped,
        ResidentState::Failed,
    ];

    /// Whether new requests are accepted.
    pub fn accepts(self) -> bool {
        matches!(self, ResidentState::Listening)
    }

    /// Whether the process is done.
    pub fn is_terminal(self) -> bool {
        matches!(self, ResidentState::Stopped | ResidentState::Failed)
    }
}

/// Events that drive the daemon process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResidentEvent {
    /// The socket is bound.
    Bound,
    /// Binding failed.
    BindFailed,
    /// A request arrived (resets the idle clock).
    Request,
    /// No request for the idle timeout.
    IdleTimeout,
    /// A client asked the daemon to stop (for example after an upgrade).
    Shutdown,
    /// In-flight requests finished and the socket is removed.
    Drained,
}

impl ResidentEvent {
    pub const ALL: [ResidentEvent; 6] = [
        ResidentEvent::Bound,
        ResidentEvent::BindFailed,
        ResidentEvent::Request,
        ResidentEvent::IdleTimeout,
        ResidentEvent::Shutdown,
        ResidentEvent::Drained,
    ];
}

use ResidentEvent as E;
use ResidentState as S;

/// The complete transition table. Pairs not listed are illegal.
pub const TRANSITIONS: &[(ResidentState, ResidentEvent, ResidentState)] = &[
    (S::Starting, E::Bound, S::Listening),
    (S::Starting, E::BindFailed, S::Failed),
    (S::Starting, E::Shutdown, S::Stopped),
    (S::Listening, E::Request, S::Listening),
    (S::Listening, E::IdleTimeout, S::Draining),
    (S::Listening, E::Shutdown, S::Draining),
    (S::Draining, E::Drained, S::Stopped),
];

pub fn next(state: ResidentState, event: ResidentEvent) -> Option<ResidentState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: ResidentState,
    pub event: ResidentEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in daemon state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The daemon lifecycle. The state only changes through [`ResidentLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentLifecycle {
    state: ResidentState,
}

impl Default for ResidentLifecycle {
    fn default() -> Self {
        Self {
            state: ResidentState::Starting,
        }
    }
}

impl ResidentLifecycle {
    pub fn state(&self) -> ResidentState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(&mut self, event: ResidentEvent) -> Result<ResidentState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}
