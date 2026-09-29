//! Lifecycle of a resident where-next session (the MCP server or daemon process), as an
//! explicit state machine.
//!
//! `Starting --Warm--> Warming --WarmDone--> Serving`. Warming loads and verifies the model and
//! builds or loads the index. Any failure lands in `Degraded`, where queries fail open (the caller
//! is told to use ordinary search) until a retry or recovery. Shutdown is always allowed and ends
//! in `Stopped`.

use std::fmt;

/// States of a resident session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// Process started; nothing loaded.
    Starting,
    /// Loading the model and building or loading the index.
    Warming,
    /// Model and index ready; queries are answered.
    Serving,
    /// Something failed; queries fail open with a machine-readable reason.
    Degraded,
    /// Shutdown requested; finishing in-flight work.
    ShuttingDown,
    /// Terminal state.
    Stopped,
}

impl SessionState {
    pub const ALL: [SessionState; 6] = [
        SessionState::Starting,
        SessionState::Warming,
        SessionState::Serving,
        SessionState::Degraded,
        SessionState::ShuttingDown,
        SessionState::Stopped,
    ];

    /// Whether queries get real hints. Every other state fails open.
    pub fn can_answer(self) -> bool {
        matches!(self, SessionState::Serving)
    }
}

/// Events that drive a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionEvent {
    /// Start (or retry) loading the model and index.
    Warm,
    /// Model and index are ready.
    WarmDone,
    /// Loading failed.
    WarmFailed,
    /// A runtime failure while serving (model error, index corruption).
    Failure,
    /// The failure cleared without a full re-warm (for example a refresh succeeded).
    Recovered,
    /// Stop requested.
    Shutdown,
    /// In-flight work finished after a shutdown.
    Closed,
}

impl SessionEvent {
    pub const ALL: [SessionEvent; 7] = [
        SessionEvent::Warm,
        SessionEvent::WarmDone,
        SessionEvent::WarmFailed,
        SessionEvent::Failure,
        SessionEvent::Recovered,
        SessionEvent::Shutdown,
        SessionEvent::Closed,
    ];
}

use SessionEvent as E;
use SessionState as S;

/// The complete transition table. Pairs not listed are illegal.
pub const TRANSITIONS: &[(SessionState, SessionEvent, SessionState)] = &[
    (S::Starting, E::Warm, S::Warming),
    (S::Starting, E::Shutdown, S::ShuttingDown),
    (S::Warming, E::WarmDone, S::Serving),
    (S::Warming, E::WarmFailed, S::Degraded),
    (S::Warming, E::Shutdown, S::ShuttingDown),
    (S::Serving, E::Failure, S::Degraded),
    (S::Serving, E::Shutdown, S::ShuttingDown),
    (S::Degraded, E::Recovered, S::Serving),
    (S::Degraded, E::Warm, S::Warming),
    (S::Degraded, E::Shutdown, S::ShuttingDown),
    (S::ShuttingDown, E::Closed, S::Stopped),
];

pub fn next(state: SessionState, event: SessionEvent) -> Option<SessionState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: SessionState,
    pub event: SessionEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in session state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The session lifecycle. The state only changes through [`SessionLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLifecycle {
    state: SessionState,
}

impl Default for SessionLifecycle {
    fn default() -> Self {
        Self {
            state: SessionState::Starting,
        }
    }
}

impl SessionLifecycle {
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(&mut self, event: SessionEvent) -> Result<SessionState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}
