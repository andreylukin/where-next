//! A tiny helper for table-driven state machines.
//!
//! Each machine lists its legal `(from, event, to)` transitions in a `const` table; anything not
//! listed is rejected with [`Illegal`] and leaves the state unchanged.

use std::fmt;

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Illegal<S, E> {
    /// State the machine was in (and is still in).
    pub state: S,
    /// The rejected event.
    pub event: E,
}

impl<S: fmt::Debug, E: fmt::Debug> fmt::Display for Illegal<S, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in state {:?}",
            self.event, self.state
        )
    }
}

impl<S: fmt::Debug, E: fmt::Debug> std::error::Error for Illegal<S, E> {}

/// Looks up the next state in a transition table.
pub fn step<S: Copy + PartialEq, E: Copy + PartialEq>(
    table: &[(S, E, S)],
    state: S,
    event: E,
) -> Result<S, Illegal<S, E>> {
    table
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
        .ok_or(Illegal { state, event })
}

/// Asserts a table has at most one entry per `(state, event)` pair.
#[cfg(test)]
pub fn assert_unique<S: PartialEq + fmt::Debug, E: PartialEq + fmt::Debug>(table: &[(S, E, S)]) {
    for (i, (a, e, _)) in table.iter().enumerate() {
        for (b, f, _) in &table[i + 1..] {
            assert!(
                !(a == b && e == f),
                "duplicate transition for {a:?} on {e:?}"
            );
        }
    }
}
