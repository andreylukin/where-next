//! Locking in the daemon and between indexers: a request for one repository never waits for
//! another repository's indexing, statistics name the busy repository, and two indexers of the
//! same repository (the daemon and a `wn init`, or two commands) never embed the same files twice.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use wn_cli::daemon::handler::Warm;
use wn_cli::daemon::{Op, Request, PROTOCOL};
use wn_cli::progress::Sink;
use wn_cli::{AskArgs, EncoderInfo, SharedEncoder, Workspace};
use wn_core::encoder::{EncodeError, Encoder, HashEncoder, QueryInput};

/// Every test in this binary shares one cache home (the daemon handler reads it from the
/// environment).
fn cache_home() -> &'static Path {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = HOME.get_or_init(|| tempfile::tempdir().unwrap()).path();
    std::env::set_var("WHERE_NEXT_HOME", dir);
    dir
}

fn repo(files: usize) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    for i in 0..files {
        let p = d.join(format!("src/m{}/f{i}.rs", i % 5));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            format!("/// Handler {i} parses request {i}.\nfn handle_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(d)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "commit.gpgsign", "false"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);
    t
}

fn root(t: &tempfile::TempDir) -> PathBuf {
    t.path().canonicalize().unwrap()
}

/// Wraps the lexical encoder: counts embedded documents and, while the gate is closed, blocks
/// every embedding call.
struct Gated {
    inner: HashEncoder,
    tag: &'static str,
    gate: Arc<(Mutex<bool>, Condvar)>,
    embedded: Arc<AtomicUsize>,
    delay: Duration,
}

impl Encoder for Gated {
    fn fingerprint(&self) -> String {
        format!("{}-{}", self.tag, self.inner.fingerprint())
    }
    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let (open, cv) = &*self.gate;
        let mut o = open.lock().unwrap();
        while !*o {
            o = cv.wait(o).unwrap();
        }
        drop(o);
        std::thread::sleep(self.delay);
        self.embedded.fetch_add(texts.len(), Ordering::SeqCst);
        self.inner.documents(texts)
    }
    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        self.inner.queries(items)
    }
}

fn gate(open: bool) -> Arc<(Mutex<bool>, Condvar)> {
    Arc::new((Mutex::new(open), Condvar::new()))
}

fn open_gate(g: &(Mutex<bool>, Condvar)) {
    *g.0.lock().unwrap() = true;
    g.1.notify_all();
}

fn shared(e: Gated) -> (SharedEncoder, EncoderInfo) {
    let info = EncoderInfo {
        fingerprint: e.fingerprint(),
        fallback: true,
        model: None,
        reason: Some("test".into()),
    };
    (Arc::new(e) as SharedEncoder, info)
}

/// A daemon whose model `/blocked` embeds only once `gate` opens; every other request uses an
/// ungated lexical encoder.
fn warm(gate: Arc<(Mutex<bool>, Condvar)>) -> Arc<Warm> {
    Arc::new(Warm::with_loader(Box::new(move |model: Option<&Path>| {
        let blocked = model == Some(Path::new("/blocked"));
        shared(Gated {
            inner: HashEncoder::default(),
            tag: if blocked { "blocked" } else { "free" },
            gate: if blocked {
                gate.clone()
            } else {
                self::gate(true)
            },
            embedded: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
        })
    })))
}

fn request(repo: &Path, model: Option<&str>, op: Op) -> Request {
    Request {
        proto: PROTOCOL,
        client: "test".into(),
        repo: Some(repo.to_string_lossy().into_owned()),
        model: model.map(String::from),
        json: true,
        op,
    }
}

fn ask(query: &str) -> Op {
    Op::Ask {
        args: AskArgs {
            query: query.into(),
            functions: false,
            k: 3,
            no_adapter: true,
            strict: false,
            no_abstain: true,
            start: false,
            start_min_files: 0,
            no_log: true,
        },
        context: String::new(),
    }
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_request_for_one_repository_never_waits_for_another_repositorys_indexing() {
    cache_home();
    let (a, b) = (repo(30), repo(10));
    let (ra, rb) = (root(&a), root(&b));
    let g = gate(false);
    let warm = warm(g.clone());
    let indexing = {
        let (warm, ra) = (warm.clone(), ra.clone());
        std::thread::spawn(move || warm.serve(&request(&ra, Some("/blocked"), Op::Status), None))
    };
    let a_name = ra.file_name().unwrap().to_string_lossy().into_owned();
    wait_until("repository A to be busy", || {
        warm.busy() == vec![a_name.clone()]
    });

    let start = Instant::now();
    let answer = warm.serve(
        &request(&rb, None, ask("where is the request handler")),
        None,
    );
    let status = warm.serve(&request(&rb, None, Op::Status), None);
    let took = start.elapsed();
    assert!(answer.ok && status.ok, "{answer:?} {status:?}");
    assert!(took < Duration::from_secs(3), "B waited {took:?} for A");
    assert!(status.text.contains("\"files\": 10"), "{}", status.text);
    // Statistics never wait either, and name what is busy.
    assert_eq!(warm.busy(), vec![a_name]);
    assert_eq!(warm.repos(), 2);

    open_gate(&g);
    let done = indexing.join().unwrap();
    assert!(done.ok && done.text.contains("\"files\": 30"), "{done:?}");
    assert!(warm.busy().is_empty());
}

struct Lines(Mutex<Vec<String>>);

impl Sink for Lines {
    fn update(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_string());
    }
}

#[test]
fn a_second_request_for_a_repository_being_indexed_says_it_is_waiting() {
    cache_home();
    let a = repo(20);
    let ra = root(&a);
    let g = gate(false);
    let warm = warm(g.clone());
    let first = {
        let (warm, ra) = (warm.clone(), ra.clone());
        std::thread::spawn(move || warm.serve(&request(&ra, Some("/blocked"), Op::Status), None))
    };
    wait_until("the first request to start", || !warm.busy().is_empty());
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    let second = {
        let (warm, ra, lines) = (warm.clone(), ra.clone(), lines.clone());
        std::thread::spawn(move || {
            warm.serve(
                &request(&ra, Some("/blocked"), Op::Status),
                Some(lines as Arc<dyn Sink>),
            )
        })
    };
    wait_until("the waiting line", || !lines.0.lock().unwrap().is_empty());
    assert!(
        lines.0.lock().unwrap()[0].contains("waiting for the daemon to finish indexing"),
        "{:?}",
        lines.0.lock().unwrap()
    );
    open_gate(&g);
    assert!(first.join().unwrap().ok);
    assert!(second.join().unwrap().ok);
}

#[test]
fn two_indexers_of_one_repository_embed_each_file_once() {
    let home = cache_home();
    let r = repo(40);
    let rr = root(&r);
    let embedded = Arc::new(AtomicUsize::new(0));
    let open = |embedded: &Arc<AtomicUsize>| {
        let (enc, info) = shared(Gated {
            inner: HashEncoder::default(),
            tag: "twice",
            gate: gate(true),
            embedded: embedded.clone(),
            delay: Duration::from_millis(20),
        });
        let mut ws = Workspace::open_in(&rr, home, enc, info);
        // Small chunks, so the first indexer is still busy when the second starts.
        ws.index.set_checkpoint_every(4);
        ws
    };
    let mut first = open(&embedded);
    let mut second = open(&embedded);
    let t1 = std::thread::spawn(move || first.refresh(false).map(|s| s.encoded));
    wait_until("the first indexer to embed", || {
        embedded.load(Ordering::SeqCst) > 0
    });
    let t2 = std::thread::spawn(move || {
        let stats = second.refresh(false)?;
        Ok::<_, String>((
            stats.encoded,
            second.index.count(wn_core::index::EntryKind::File),
        ))
    });
    let one = t1.join().unwrap().unwrap();
    let (two, files_seen) = t2.join().unwrap().unwrap();
    assert_eq!(one, 40);
    assert_eq!(
        two, 0,
        "the second indexer re-embedded what the first stored"
    );
    assert_eq!(
        files_seen, 40,
        "the second indexer reloaded the stored index"
    );
    assert_eq!(embedded.load(Ordering::SeqCst), 40);
}

#[test]
fn rollback_waits_for_a_running_indexer_of_the_repository() {
    let home = cache_home();
    let r = repo(5);
    let (enc, info) = shared(Gated {
        inner: HashEncoder::default(),
        tag: "rollback",
        gate: gate(true),
        embedded: Arc::new(AtomicUsize::new(0)),
        delay: Duration::ZERO,
    });
    let mut ws = Workspace::open_in(&root(&r), home, enc, info);
    std::fs::create_dir_all(ws.dir.join("adapter")).unwrap();
    // Another indexer (say a `wn init` fitting the adapter) holds the lock.
    let mut other = wn_daemon::indexer::Indexer::new(&ws.dir);
    assert!(other.try_begin().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        let msg = ws.rollback();
        tx.send(()).unwrap();
        (msg, ws)
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "rollback ran while another indexer held the lock"
    );
    other.finish(true);
    let (msg, ws) = t.join().unwrap();
    assert!(msg.unwrap().starts_with("adapter: removed"));
    assert!(!ws.dir.join("adapter").exists());
}
