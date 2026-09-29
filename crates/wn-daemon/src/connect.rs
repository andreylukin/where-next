//! How a `wn` command reaches the background daemon, as an explicit state machine.
//!
//! ```text
//! Idle --Connect--> Connecting --Connected--> Handshaking --Accepted--> Ready
//!                       |                          |
//!                    Refused                    Mismatch (daemon from another binary/version)
//!                       v                          v
//!                   Spawning --Spawned--> Waiting  Restarting --Stopped--> Respawning --Spawned-->
//!                                                  Rewaiting --Connected--> Rehandshaking --Accepted--> Ready
//! ```
//!
//! The daemon is spawned at most once and replaced at most once per command. Every failure
//! (spawn error, timeout, a second version mismatch, a broken connection after `Ready`) ends in
//! `InProcess`: the command answers without a daemon, exactly as if none existed. `Disabled`
//! (`WN_NO_DAEMON`, `--no-daemon`) goes straight there.

use std::fmt;

/// States of one command's connection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectState {
    /// Nothing tried yet.
    Idle,
    /// Connecting to an existing daemon socket.
    Connecting,
    /// Connected; comparing binary and protocol versions.
    Handshaking,
    /// No daemon answered; starting one.
    Spawning,
    /// Waiting for the freshly started daemon to bind its socket.
    Waiting,
    /// The running daemon is from another build; asking it to stop.
    Restarting,
    /// The old daemon stopped; starting its replacement.
    Respawning,
    /// Waiting for the replacement to bind its socket.
    Rewaiting,
    /// Handshaking with the replacement.
    Rehandshaking,
    /// Terminal: requests go to the daemon.
    Ready,
    /// Terminal: the command answers in-process.
    InProcess,
}

impl ConnectState {
    pub const ALL: [ConnectState; 11] = [
        ConnectState::Idle,
        ConnectState::Connecting,
        ConnectState::Handshaking,
        ConnectState::Spawning,
        ConnectState::Waiting,
        ConnectState::Restarting,
        ConnectState::Respawning,
        ConnectState::Rewaiting,
        ConnectState::Rehandshaking,
        ConnectState::Ready,
        ConnectState::InProcess,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(self, ConnectState::Ready | ConnectState::InProcess)
    }
}

/// Events of a connection attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectEvent {
    /// Start: try the socket.
    Connect,
    /// The daemon is disabled for this command.
    Disabled,
    /// The socket accepted the connection.
    Connected,
    /// Nothing listens on the socket.
    Refused,
    /// A daemon process was started.
    Spawned,
    /// Starting a daemon process failed.
    SpawnFailed,
    /// A wait (bind, handshake reply, stop) took too long.
    TimedOut,
    /// Versions match.
    Accepted,
    /// The daemon runs another binary or protocol version.
    Mismatch,
    /// The old daemon acknowledged the stop and its socket is gone.
    Stopped,
    /// The connection broke after `Ready` (the daemon died mid-request).
    Broken,
}

impl ConnectEvent {
    pub const ALL: [ConnectEvent; 11] = [
        ConnectEvent::Connect,
        ConnectEvent::Disabled,
        ConnectEvent::Connected,
        ConnectEvent::Refused,
        ConnectEvent::Spawned,
        ConnectEvent::SpawnFailed,
        ConnectEvent::TimedOut,
        ConnectEvent::Accepted,
        ConnectEvent::Mismatch,
        ConnectEvent::Stopped,
        ConnectEvent::Broken,
    ];
}

use ConnectEvent as E;
use ConnectState as S;

/// The complete transition table. Pairs not listed are illegal.
pub const TRANSITIONS: &[(ConnectState, ConnectEvent, ConnectState)] = &[
    (S::Idle, E::Connect, S::Connecting),
    (S::Idle, E::Disabled, S::InProcess),
    (S::Connecting, E::Connected, S::Handshaking),
    (S::Connecting, E::Refused, S::Spawning),
    (S::Connecting, E::TimedOut, S::InProcess),
    (S::Spawning, E::Spawned, S::Waiting),
    (S::Spawning, E::SpawnFailed, S::InProcess),
    (S::Waiting, E::Connected, S::Handshaking),
    (S::Waiting, E::TimedOut, S::InProcess),
    (S::Handshaking, E::Accepted, S::Ready),
    (S::Handshaking, E::Mismatch, S::Restarting),
    (S::Handshaking, E::TimedOut, S::InProcess),
    (S::Restarting, E::Stopped, S::Respawning),
    (S::Restarting, E::TimedOut, S::InProcess),
    (S::Respawning, E::Spawned, S::Rewaiting),
    (S::Respawning, E::SpawnFailed, S::InProcess),
    (S::Rewaiting, E::Connected, S::Rehandshaking),
    (S::Rewaiting, E::TimedOut, S::InProcess),
    (S::Rehandshaking, E::Accepted, S::Ready),
    (S::Rehandshaking, E::Mismatch, S::InProcess),
    (S::Rehandshaking, E::TimedOut, S::InProcess),
    (S::Ready, E::Broken, S::InProcess),
];

pub fn next(state: ConnectState, event: ConnectEvent) -> Option<ConnectState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: ConnectState,
    pub event: ConnectEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in connection state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// One command's connection attempt. The state only changes through [`Connection::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    state: ConnectState,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            state: ConnectState::Idle,
        }
    }
}

impl Connection {
    pub fn state(&self) -> ConnectState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(&mut self, event: ConnectEvent) -> Result<ConnectState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}
