//! Progress for long index builds, on stderr only (stdout and `--json` stay clean).
//!
//! A [`Tracker`] counts what an index build has done; a [`Ticker`] thread turns it into a line
//! every second once the build has run for [`FIRST_AFTER`], and hands each line to a [`Sink`]. The
//! in-process sink is [`Render`] on stderr: on a terminal it redraws one line, otherwise it prints
//! plain lines (the first at once, then every [`PLAIN_EVERY`]). The daemon's sink forwards the lines
//! to the waiting client, which renders them the same way.

use std::io::{IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wn_core::encoder::{EncodeError, Encoder, QueryInput};

/// Quiet period before the first progress line (short builds print nothing).
pub const FIRST_AFTER: Duration = Duration::from_secs(1);

/// Interval between progress updates.
pub const TICK: Duration = Duration::from_secs(1);

/// Interval between plain (non-terminal) progress lines after the first.
pub const PLAIN_EVERY: Duration = Duration::from_secs(10);

/// Receives progress lines.
pub trait Sink: Send + Sync {
    /// A new progress line.
    fn update(&self, line: &str);
    /// The operation ended (clear or finish the line).
    fn done(&self) {}
}

/// Phases of an index build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Waiting = 0,
    Scanning = 1,
    Reading = 2,
    Embedding = 3,
    Fitting = 4,
}

impl Phase {
    fn from_u8(v: u8) -> Phase {
        match v {
            0 => Phase::Waiting,
            1 => Phase::Scanning,
            2 => Phase::Reading,
            3 => Phase::Embedding,
            _ => Phase::Fitting,
        }
    }
}

/// Counters of one build, shared with the [`Ticker`].
#[derive(Debug)]
pub struct Tracker {
    repo: String,
    started: Instant,
    phase: AtomicU8,
    read: AtomicUsize,
    embedded: AtomicUsize,
}

impl Tracker {
    pub fn new(repo: &str) -> Arc<Tracker> {
        Arc::new(Tracker {
            repo: repo.to_string(),
            started: Instant::now(),
            phase: AtomicU8::new(Phase::Scanning as u8),
            read: AtomicUsize::new(0),
            embedded: AtomicUsize::new(0),
        })
    }

    pub fn phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::SeqCst);
    }

    /// One file read for embedding.
    pub fn read_one(&self) {
        self.read.fetch_add(1, Ordering::SeqCst);
        self.phase(Phase::Reading);
    }

    /// `n` documents embedded.
    pub fn embedded(&self, n: usize) {
        self.embedded.fetch_add(n, Ordering::SeqCst);
    }

    /// The current progress line.
    pub fn line(&self) -> String {
        let secs = self.started.elapsed().as_secs();
        let read = self.read.load(Ordering::SeqCst);
        let embedded = self.embedded.load(Ordering::SeqCst);
        let what = match Phase::from_u8(self.phase.load(Ordering::SeqCst)) {
            Phase::Waiting => "waiting for another wn process that is indexing it".to_string(),
            Phase::Scanning => "listing files".to_string(),
            Phase::Reading => format!("reading files ({read})"),
            Phase::Embedding if embedded <= read => {
                format!("embedding {embedded} of {read} files")
            }
            Phase::Embedding => format!("embedded {embedded} documents"),
            Phase::Fitting => "fitting the adapter from git history".to_string(),
        };
        format!("wn: indexing {}: {what} ({secs}s)", self.repo)
    }
}

/// Documents per call to the wrapped encoder. A model encoder embeds one call at a time, so
/// small calls let a query for another repository (in the daemon) run between them instead of
/// waiting for a whole index checkpoint, and make progress counts move.
pub const SUB_BATCH: usize = 64;

/// Wraps an encoder to count embedded documents, embedding in calls of at most [`SUB_BATCH`].
pub struct Counting<'a> {
    pub inner: &'a dyn Encoder,
    pub tracker: &'a Tracker,
}

impl Encoder for Counting<'_> {
    fn fingerprint(&self) -> String {
        self.inner.fingerprint()
    }

    fn calibration(&self) -> Option<wn_core::rank::Calibration> {
        self.inner.calibration()
    }

    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        self.tracker.phase(Phase::Embedding);
        let mut out = Vec::with_capacity(texts.len());
        for (i, chunk) in texts.chunks(SUB_BATCH).enumerate() {
            if i > 0 {
                // Model encoders hold an unfair lock per call: pause so a waiting query gets it.
                std::thread::sleep(Duration::from_millis(2));
            }
            out.extend(self.inner.documents(chunk)?);
            self.tracker.embedded(chunk.len());
        }
        Ok(out)
    }

    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        self.inner.queries(items)
    }
}

/// Sends the tracker's line to a sink every [`TICK`] after [`FIRST_AFTER`], until dropped.
pub struct Ticker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    sink: Arc<dyn Sink>,
    spoke: Arc<AtomicBool>,
}

impl Ticker {
    pub fn start(tracker: Arc<Tracker>, sink: Arc<dyn Sink>) -> Ticker {
        Ticker::lines(move || tracker.line(), sink)
    }

    /// A ticker for a step without counters: `wn: <what> (<seconds>s)`.
    pub fn step(what: &str, sink: Arc<dyn Sink>) -> Ticker {
        let (what, started) = (what.to_string(), Instant::now());
        Ticker::lines(
            move || format!("wn: {what} ({}s)", started.elapsed().as_secs()),
            sink,
        )
    }

    /// Sends `line()` to `sink` every [`TICK`] after [`FIRST_AFTER`], until dropped.
    pub fn lines(line: impl Fn() -> String + Send + 'static, sink: Arc<dyn Sink>) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let spoke = Arc::new(AtomicBool::new(false));
        let (flag, said, out) = (stop.clone(), spoke.clone(), sink.clone());
        let handle = std::thread::spawn(move || {
            let mut next = FIRST_AFTER;
            loop {
                let step = Duration::from_millis(50);
                let mut waited = Duration::ZERO;
                while waited < next {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(step);
                    waited += step;
                }
                out.update(&line());
                said.store(true, Ordering::SeqCst);
                next = TICK;
            }
        });
        Ticker {
            stop,
            handle: Some(handle),
            sink,
            spoke,
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        if self.spoke.load(Ordering::SeqCst) {
            self.sink.done();
        }
    }
}

/// Renders progress lines on stderr: one redrawn line on a terminal, plain periodic lines
/// otherwise.
pub struct Render {
    tty: bool,
    state: Mutex<RenderState>,
}

#[derive(Default)]
struct RenderState {
    last: Option<Instant>,
    drawn: bool,
}

impl Render {
    pub fn stderr() -> Render {
        Render::new(std::io::stderr().is_terminal())
    }

    pub fn new(tty: bool) -> Render {
        Render {
            tty,
            state: Mutex::new(RenderState::default()),
        }
    }

    /// What to write for `line` now (`None`: skip it), updating the render state.
    pub fn frame(&self, line: &str) -> Option<String> {
        let mut s = self.state.lock().ok()?;
        if self.tty {
            s.drawn = true;
            return Some(format!("\r\x1b[2K{line}"));
        }
        let due = s.last.map_or(true, |t| t.elapsed() >= PLAIN_EVERY);
        if !due {
            return None;
        }
        s.last = Some(Instant::now());
        Some(format!("{line}\n"))
    }

    /// What to write when the operation ends (the next operation's first line prints at once).
    pub fn end(&self) -> Option<String> {
        let mut s = self.state.lock().ok()?;
        s.last = None;
        if self.tty && s.drawn {
            s.drawn = false;
            return Some("\r\x1b[2K".to_string());
        }
        None
    }
}

impl Sink for Render {
    fn update(&self, line: &str) {
        if let Some(text) = self.frame(line) {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(text.as_bytes());
            let _ = err.flush();
        }
    }

    fn done(&self) {
        if let Some(text) = self.end() {
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_embeds_in_small_calls_and_keeps_the_order() {
        use wn_core::encoder::HashEncoder;
        let inner = HashEncoder::default();
        let tracker = Tracker::new("r");
        let counting = Counting {
            inner: &inner,
            tracker: &tracker,
        };
        let texts: Vec<String> = (0..SUB_BATCH * 2 + 5)
            .map(|i| format!("handler {i}"))
            .collect();
        assert_eq!(
            counting.documents(&texts).unwrap(),
            inner.documents(&texts).unwrap()
        );
        assert_eq!(tracker.embedded.load(Ordering::SeqCst), texts.len());
    }

    #[test]
    fn plain_output_prints_the_first_line_at_once_then_throttles() {
        let r = Render::new(false);
        assert_eq!(r.frame("a").as_deref(), Some("a\n"));
        assert_eq!(r.frame("b"), None);
        assert_eq!(r.end(), None);
        assert_eq!(
            r.frame("c").as_deref(),
            Some("c\n"),
            "a new step prints at once"
        );
    }

    #[test]
    fn a_terminal_redraws_one_line_and_clears_it_at_the_end() {
        let r = Render::new(true);
        assert_eq!(r.frame("a").as_deref(), Some("\r\x1b[2Ka"));
        assert_eq!(r.frame("b").as_deref(), Some("\r\x1b[2Kb"));
        assert_eq!(r.end().as_deref(), Some("\r\x1b[2K"));
        assert_eq!(r.end(), None);
    }

    #[test]
    fn lines_name_the_repository_and_the_phase() {
        let t = Tracker::new("big");
        assert!(t.line().starts_with("wn: indexing big: listing files"));
        t.read_one();
        t.read_one();
        t.phase(Phase::Embedding);
        t.embedded(1);
        assert!(t.line().contains("embedding 1 of 2 files"), "{}", t.line());
        t.phase(Phase::Fitting);
        assert!(t.line().contains("fitting the adapter"));
    }

    struct Collect(Mutex<Vec<String>>);

    impl Sink for Collect {
        fn update(&self, line: &str) {
            self.0.lock().unwrap().push(line.to_string());
        }
        fn done(&self) {
            self.0.lock().unwrap().push("done".into());
        }
    }

    #[test]
    fn a_ticker_is_silent_for_short_work_and_speaks_for_long_work() {
        let sink = Arc::new(Collect(Mutex::new(Vec::new())));
        drop(Ticker::start(Tracker::new("r"), sink.clone()));
        assert!(sink.0.lock().unwrap().is_empty());
        let ticker = Ticker::start(Tracker::new("r"), sink.clone());
        std::thread::sleep(FIRST_AFTER + Duration::from_millis(300));
        drop(ticker);
        let got = sink.0.lock().unwrap().clone();
        assert!(got.len() >= 2 && got.last().unwrap() == "done", "{got:?}");
    }
}
