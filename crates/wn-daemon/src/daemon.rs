//! A resident session: the [`SessionLifecycle`] around a [`Workspace`].
//!
//! Until warm-up succeeds, and after failures, queries fail open with a machine-readable state
//! instead of an empty hint list.

use std::time::Instant;

use serde::Serialize;
use wn_core::rank::{AnswerState, Outcome};

use crate::session::{SessionEvent, SessionLifecycle, SessionState};
use crate::workspace::{Provenance, Refreshed, Scan, Workspace};

/// One answer as sent to agents: the `wn-core` outcome plus session provenance and timing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reply {
    #[serde(flatten)]
    pub outcome: Outcome,
    pub session: String,
    pub provenance: Provenance,
    pub ms: u128,
}

/// Session status.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub session: String,
    pub last_error: Option<String>,
    pub provenance: Provenance,
}

pub struct Daemon {
    session: SessionLifecycle,
    workspace: Workspace,
    last_error: Option<String>,
}

impl Daemon {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            session: SessionLifecycle::default(),
            workspace,
            last_error: None,
        }
    }

    pub fn state(&self) -> SessionState {
        self.session.state()
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    fn event(&mut self, event: SessionEvent) {
        if let Err(err) = self.session.handle(event) {
            self.last_error = Some(err.to_string());
        }
    }

    /// Scans, indexes and fits the adapter. Legal from `Starting` or `Degraded`.
    pub fn warm(&mut self) -> Result<Refreshed, String> {
        self.session
            .handle(SessionEvent::Warm)
            .map_err(|e| e.to_string())?;
        match self.workspace.warm() {
            Ok(stats) => {
                self.last_error = None;
                self.event(SessionEvent::WarmDone);
                Ok(stats)
            }
            Err(err) => {
                self.last_error = Some(err.clone());
                self.event(SessionEvent::WarmFailed);
                Err(err)
            }
        }
    }

    /// Answers a query, or fails open.
    pub fn ask(&self, query: &str, context: &str) -> Reply {
        let start = Instant::now();
        let outcome = if self.session.state().can_answer() {
            self.workspace.ask(query, context)
        } else {
            Outcome {
                state: AnswerState::Error,
                error: Some(match &self.last_error {
                    Some(e) => format!("session {:?}: {e}", self.session.state()),
                    None => format!("session {:?}: not serving yet", self.session.state()),
                }),
                ..Outcome::default()
            }
        };
        Reply {
            outcome,
            session: format!("{:?}", self.session.state()),
            provenance: self.workspace.provenance(),
            ms: start.elapsed().as_millis(),
        }
    }

    /// Applies a scan taken outside the lock. Success clears a `Degraded` session; failure while
    /// serving degrades it.
    pub fn apply(&mut self, scan: Scan) -> Result<Refreshed, String> {
        match self.session.state() {
            SessionState::Serving | SessionState::Degraded => {}
            other => return Err(format!("cannot refresh in session state {other:?}")),
        }
        let start = Instant::now();
        match self.workspace.apply(scan) {
            Ok(stats) => {
                if self.session.state() == SessionState::Degraded {
                    self.last_error = None;
                    self.event(SessionEvent::Recovered);
                }
                Ok(Refreshed {
                    encoded: stats.encoded,
                    removed: stats.removed,
                    files_indexed: self.workspace.provenance().files_indexed,
                    adapter: None,
                    ms: start.elapsed().as_millis(),
                })
            }
            Err(err) => {
                self.last_error = Some(err.clone());
                if self.session.state() == SessionState::Serving {
                    self.event(SessionEvent::Failure);
                }
                Err(err)
            }
        }
    }

    /// Scans and applies synchronously.
    pub fn refresh(&mut self) -> Result<Refreshed, String> {
        let scan = crate::workspace::scan(self.workspace.root());
        self.apply(scan)
    }

    pub fn status(&self) -> Status {
        Status {
            session: format!("{:?}", self.session.state()),
            last_error: self.last_error.clone(),
            provenance: self.workspace.provenance(),
        }
    }

    pub fn shutdown(&mut self) {
        self.event(SessionEvent::Shutdown);
        self.event(SessionEvent::Closed);
    }
}

/// Object-safe interface for transports (MCP, CLI).
pub trait Service: Send {
    fn ask(&self, query: &str, context: &str) -> Reply;
    fn refresh(&mut self) -> Result<Refreshed, String>;
    fn status(&self) -> Status;
}

impl Service for Daemon {
    fn ask(&self, query: &str, context: &str) -> Reply {
        Daemon::ask(self, query, context)
    }

    fn refresh(&mut self) -> Result<Refreshed, String> {
        Daemon::refresh(self)
    }

    fn status(&self) -> Status {
        Daemon::status(self)
    }
}

/// Background warm-up and periodic refresh for a shared daemon. Scans run without the lock;
/// only a changed scan is applied under it.
pub mod background {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use super::Daemon;
    use crate::session::SessionState;
    use crate::workspace::scan;

    pub struct Refresher {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Refresher {
        pub fn stop(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    /// Warms the daemon, then rescans every `interval` until stopped.
    pub fn spawn(daemon: Arc<Mutex<Daemon>>, interval: Duration) -> Refresher {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            if let Ok(mut d) = daemon.lock() {
                let _ = d.warm();
            }
            while !flag.load(Ordering::SeqCst) {
                sleep_unless_stopped(interval, &flag);
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let (root, known, state) = match daemon.lock() {
                    Ok(d) => (
                        d.workspace().root().to_path_buf(),
                        d.workspace().last_versions(),
                        d.state(),
                    ),
                    Err(_) => break,
                };
                if state == SessionState::Degraded && known.is_empty() {
                    if let Ok(mut d) = daemon.lock() {
                        let _ = d.warm();
                    }
                    continue;
                }
                if !matches!(state, SessionState::Serving | SessionState::Degraded) {
                    continue;
                }
                let fresh = scan(&root);
                let unchanged = fresh.files.len() == known.len()
                    && fresh
                        .files
                        .iter()
                        .all(|f| known.get(&f.path) == Some(&f.cid));
                if unchanged {
                    continue;
                }
                if let Ok(mut d) = daemon.lock() {
                    let _ = d.apply(fresh);
                }
            }
        });
        Refresher {
            stop,
            handle: Some(handle),
        }
    }

    fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) {
        let step = Duration::from_millis(50);
        let mut waited = Duration::ZERO;
        while waited < total && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(step);
            waited += step;
        }
    }
}
