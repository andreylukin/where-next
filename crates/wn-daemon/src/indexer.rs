//! One indexer per repository, as an explicit state machine plus a cross-process lock.
//!
//! ```text
//! Idle --Acquired--> Indexing --Done--> Idle
//!   |                  |
//!  Busy              Failed --> Failed --Acquired--> Indexing
//!   v
//! Waiting --Acquired--> Indexing
//!   |
//!  GaveUp --> Idle
//! ```
//!
//! Whoever builds or refreshes an index (a `wn` command, the background daemon, `wn mcp`) first
//! takes an exclusive lock on `index.lock` in that repository's cache directory. `Busy` means
//! another process or thread holds it: a blocking caller waits (and says so), a background
//! rescan gives up and tries again later. Only `Indexing` holds the lock, so at most one indexer
//! per repository embeds at a time, and the waiter reuses what the first one stored.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// States of one indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexerState {
    /// Not indexing; the lock is not held.
    Idle,
    /// Another indexer holds the repository's lock.
    Waiting,
    /// This indexer holds the lock and is scanning, embedding or fitting.
    Indexing,
    /// The last run failed; the lock is released.
    Failed,
}

impl IndexerState {
    pub const ALL: [IndexerState; 4] = [
        IndexerState::Idle,
        IndexerState::Waiting,
        IndexerState::Indexing,
        IndexerState::Failed,
    ];

    /// Whether this indexer holds the repository's lock.
    pub fn holds_lock(self) -> bool {
        matches!(self, IndexerState::Indexing)
    }
}

/// Events of an indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexerEvent {
    /// The repository's lock was taken.
    Acquired,
    /// Another indexer holds the lock.
    Busy,
    /// A non-blocking caller stopped waiting.
    GaveUp,
    /// The work finished; the lock is released.
    Done,
    /// The work failed; the lock is released.
    Failed,
}

impl IndexerEvent {
    pub const ALL: [IndexerEvent; 5] = [
        IndexerEvent::Acquired,
        IndexerEvent::Busy,
        IndexerEvent::GaveUp,
        IndexerEvent::Done,
        IndexerEvent::Failed,
    ];
}

use IndexerEvent as E;
use IndexerState as S;

/// The complete transition table. Pairs not listed are illegal.
pub const TRANSITIONS: &[(IndexerState, IndexerEvent, IndexerState)] = &[
    (S::Idle, E::Acquired, S::Indexing),
    (S::Idle, E::Busy, S::Waiting),
    (S::Failed, E::Acquired, S::Indexing),
    (S::Failed, E::Busy, S::Waiting),
    (S::Waiting, E::Acquired, S::Indexing),
    (S::Waiting, E::GaveUp, S::Idle),
    (S::Indexing, E::Done, S::Idle),
    (S::Indexing, E::Failed, S::Failed),
];

pub fn next(state: IndexerState, event: IndexerEvent) -> Option<IndexerState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: IndexerState,
    pub event: IndexerEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in indexer state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The indexer lifecycle. The state only changes through [`IndexerLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerLifecycle {
    state: IndexerState,
}

impl Default for IndexerLifecycle {
    fn default() -> Self {
        Self {
            state: IndexerState::Idle,
        }
    }
}

impl IndexerLifecycle {
    pub fn state(&self) -> IndexerState {
        self.state
    }

    /// Applies an event. Illegal events leave the state unchanged and return an error.
    pub fn handle(&mut self, event: IndexerEvent) -> Result<IndexerState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}

/// Name of the lock file inside a repository's cache directory.
pub const LOCK_FILE: &str = "index.lock";

/// Name of the revision marker: rewritten (under the lock) whenever an indexer changed what is
/// stored, so others know to reload instead of trusting file modification times.
pub const REVISION_FILE: &str = "index.rev";

/// An indexer for one repository cache directory: the [`IndexerLifecycle`] driven by an
/// exclusive advisory lock on [`LOCK_FILE`] (released when the work ends or the process dies).
#[derive(Debug)]
pub struct Indexer {
    life: IndexerLifecycle,
    path: PathBuf,
    held: Option<File>,
}

impl Indexer {
    /// An idle indexer for the cache directory `dir` (nothing is created yet).
    pub fn new(dir: &Path) -> Self {
        Self {
            life: IndexerLifecycle::default(),
            path: dir.join(LOCK_FILE),
            held: None,
        }
    }

    pub fn state(&self) -> IndexerState {
        self.life.state()
    }

    fn revision_path(&self) -> PathBuf {
        self.path.with_file_name(REVISION_FILE)
    }

    /// The stored revision (empty before any change was marked). Read it before loading the
    /// index, then compare after taking the lock: a difference means another indexer stored
    /// something newer since.
    pub fn revision(&self) -> String {
        std::fs::read_to_string(self.revision_path()).unwrap_or_default()
    }

    /// Records that this indexer changed what is stored (only while `Indexing`).
    pub fn mark_changed(&mut self) -> io::Result<()> {
        if !self.state().holds_lock() {
            return Err(io::Error::other(
                "mark_changed without holding the index lock",
            ));
        }
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let token = format!(
            "{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        let path = self.revision_path();
        let tmp = path.with_extension(format!("rev.{token}"));
        std::fs::write(&tmp, token)?;
        std::fs::rename(&tmp, &path)
    }

    fn event(&mut self, event: IndexerEvent) {
        // Every call site only sends events legal in the current state.
        let _ = self.life.handle(event);
    }

    fn open(&self) -> io::Result<File> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)
    }

    /// Takes the lock without waiting. `Ok(false)`: another indexer holds it (state `Waiting`;
    /// call [`Indexer::begin`] to wait or [`Indexer::give_up`]). Errors mean the cache directory
    /// is unusable (for example not writable).
    pub fn try_begin(&mut self) -> io::Result<bool> {
        if self.state().holds_lock() {
            return Ok(true);
        }
        let file = self.open()?;
        if lock::try_exclusive(&file)? {
            self.held = Some(file);
            self.event(E::Acquired);
            Ok(true)
        } else {
            if self.state() != S::Waiting {
                self.event(E::Busy);
            }
            Ok(false)
        }
    }

    /// Takes the lock, waiting for another indexer if needed; `on_wait` runs once before waiting.
    /// Returns whether it had to wait (the index on disk may have changed meanwhile).
    pub fn begin(&mut self, on_wait: impl FnOnce()) -> io::Result<bool> {
        if self.try_begin()? {
            return Ok(false);
        }
        on_wait();
        let file = self.open()?;
        lock::exclusive(&file)?;
        self.held = Some(file);
        self.event(E::Acquired);
        Ok(true)
    }

    /// Stops waiting (only from `Waiting`).
    pub fn give_up(&mut self) {
        if self.state() == S::Waiting {
            self.event(E::GaveUp);
        }
    }

    /// Ends the work and releases the lock (only from `Indexing`).
    pub fn finish(&mut self, ok: bool) {
        if self.state() == S::Indexing {
            self.held = None;
            self.event(if ok { E::Done } else { E::Failed });
        }
    }
}

#[cfg(unix)]
mod lock {
    use std::fs::File;
    use std::io;

    use rustix::fs::{flock, FlockOperation};

    pub fn try_exclusive(file: &File) -> io::Result<bool> {
        match flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(true),
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub fn exclusive(file: &File) -> io::Result<()> {
        loop {
            match flock(file, FlockOperation::LockExclusive) {
                Err(e) if e == rustix::io::Errno::INTR => continue,
                other => return other.map_err(Into::into),
            }
        }
    }
}

#[cfg(not(unix))]
mod lock {
    //! No cross-process lock on this platform yet (there is no daemon either).
    use std::fs::File;
    use std::io;

    pub fn try_exclusive(_file: &File) -> io::Result<bool> {
        Ok(true)
    }

    pub fn exclusive(_file: &File) -> io::Result<()> {
        // TODO(windows): take a real exclusive lock (LockFileEx); Windows is untested.
        Ok(())
    }
}
