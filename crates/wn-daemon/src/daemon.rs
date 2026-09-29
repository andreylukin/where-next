//! A resident session: the [`SessionLifecycle`] wrapped around an [`Engine`].
//!
//! Every answer is either hints (at most three) or an explicit fail-open answer telling the caller
//! to use ordinary search, with a machine-readable state. Errors never surface as an empty hint.

use std::time::Instant;

use serde::Serialize;

use crate::engine::{Encoder, Engine, Hint, Plan, Provenance, RefreshStats};
use crate::session::{SessionEvent, SessionLifecycle, SessionState};

/// The answer to one query.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Answer {
    /// Up to three hints. Scores are similarities, not probabilities.
    Hints {
        hints: Vec<Hint>,
        provenance: Provenance,
        /// The index may be behind the working tree (a refresh is pending or running).
        stale: bool,
        ms: u128,
    },
    /// No hint: fall back to ordinary search. `state` is the session state, `reason` says why.
    FailOpen { state: String, reason: String },
}

/// Session status for `status` requests.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub session: String,
    pub index: String,
    pub documents: usize,
    pub last_error: Option<String>,
    pub provenance: Provenance,
}

pub struct Daemon<E: Encoder> {
    session: SessionLifecycle,
    engine: Engine<E>,
    last_error: Option<String>,
}

impl<E: Encoder> Daemon<E> {
    pub fn new(engine: Engine<E>) -> Self {
        Self {
            session: SessionLifecycle::default(),
            engine,
            last_error: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.session.state()
    }

    pub fn engine(&self) -> &Engine<E> {
        &self.engine
    }

    fn event(&mut self, event: SessionEvent) {
        // Transitions requested here are always legal from the states we call them in; a failure
        // would be a bug, so keep the state and record it rather than panicking in a server.
        if let Err(err) = self.session.handle(event) {
            self.last_error = Some(err.to_string());
        }
    }

    /// Loads the index (snapshot or full build). Legal from `Starting` or `Degraded`.
    pub fn warm(&mut self) -> Result<RefreshStats, String> {
        self.session
            .handle(SessionEvent::Warm)
            .map_err(|e| e.to_string())?;
        match self.engine.build() {
            Ok(stats) => {
                self.last_error = None;
                self.event(SessionEvent::WarmDone);
                Ok(stats)
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                self.event(SessionEvent::WarmFailed);
                Err(err.to_string())
            }
        }
    }

    /// Answers a query, or fails open.
    pub fn ask(&mut self, query: &str, context: &str, k: usize) -> Answer {
        let start = Instant::now();
        if !self.session.state().can_answer() {
            return Answer::FailOpen {
                state: format!("{:?}", self.session.state()),
                reason: self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "session is not serving yet".into()),
            };
        }
        match self.engine.ask(query, context, k) {
            Ok(hints) => Answer::Hints {
                stale: !matches!(
                    self.engine.index_state(),
                    wn_core::index_lifecycle::IndexState::Ready
                ),
                provenance: self.engine.provenance(),
                hints,
                ms: start.elapsed().as_millis(),
            },
            Err(err) => {
                self.last_error = Some(err.to_string());
                self.event(SessionEvent::Failure);
                Answer::FailOpen {
                    state: format!("{:?}", self.session.state()),
                    reason: err.to_string(),
                }
            }
        }
    }

    /// Applies a refresh plan. A successful refresh clears a `Degraded` session.
    pub fn refresh_with(&mut self, plan: Plan) -> Result<RefreshStats, String> {
        match self.session.state() {
            SessionState::Serving | SessionState::Degraded => {}
            other => return Err(format!("cannot refresh in session state {other:?}")),
        }
        match self.engine.refresh_with(plan) {
            Ok(stats) => {
                if self.session.state() == SessionState::Degraded {
                    self.last_error = None;
                    self.event(SessionEvent::Recovered);
                }
                Ok(stats)
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                if self.session.state() == SessionState::Serving {
                    self.event(SessionEvent::Failure);
                }
                Err(err.to_string())
            }
        }
    }

    /// Scans and refreshes synchronously.
    pub fn refresh(&mut self) -> Result<RefreshStats, String> {
        let plan = crate::engine::plan_changes(self.engine.root(), &self.engine.known_keys())
            .map_err(|e| e.to_string())?;
        self.refresh_with(plan)
    }

    pub fn status(&self) -> Status {
        Status {
            session: format!("{:?}", self.session.state()),
            index: format!("{:?}", self.engine.index_state()),
            documents: self.engine.documents(),
            last_error: self.last_error.clone(),
            provenance: self.engine.provenance(),
        }
    }

    /// Stops the session: `Shutdown` then `Closed`.
    pub fn shutdown(&mut self) {
        self.event(SessionEvent::Shutdown);
        self.event(SessionEvent::Closed);
    }
}
