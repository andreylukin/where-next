//! Session behaviour around the engine: fail-open answers, degradation and recovery.

use std::fs;
use std::path::Path;
use std::process::Command;

use wn_daemon::daemon::{Answer, Daemon};
use wn_daemon::engine::Engine;
use wn_daemon::fake::HashEncoder;
use wn_daemon::session::SessionState;

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success());
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    fs::write(
        dir.path().join("queue.rs"),
        "/// Work queue.\nfn enqueue_job() {}\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

fn daemon(root: &Path) -> Daemon<HashEncoder> {
    Daemon::new(Engine::new(root, None, HashEncoder::new(128)))
}

#[test]
fn fails_open_before_warm_then_serves() {
    let dir = repo();
    let mut d = daemon(dir.path());
    match d.ask("enqueue a job", "", 3) {
        Answer::FailOpen { state, .. } => assert_eq!(state, "Starting"),
        other => panic!("expected fail-open, got {other:?}"),
    }
    d.warm().unwrap();
    assert_eq!(d.state(), SessionState::Serving);
    match d.ask("enqueue a job", "", 3) {
        Answer::Hints { hints, stale, .. } => {
            assert_eq!(hints[0].path, "queue.rs");
            assert!(!stale);
        }
        other => panic!("expected hints, got {other:?}"),
    }
}

#[test]
fn failed_warm_degrades_and_retry_recovers() {
    let dir = tempfile::tempdir().unwrap(); // not a git repo yet
    let mut d = daemon(dir.path());
    assert!(d.warm().is_err());
    assert_eq!(d.state(), SessionState::Degraded);
    assert!(matches!(d.ask("x", "", 3), Answer::FailOpen { .. }));
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("a.py"), "def alpha():\n    pass\n").unwrap();
    d.warm().unwrap();
    assert_eq!(d.state(), SessionState::Serving);
}

#[test]
fn answers_serialize_with_a_status_tag() {
    let dir = repo();
    let mut d = daemon(dir.path());
    let json = serde_json::to_value(d.ask("x", "", 3)).unwrap();
    assert_eq!(json["status"], "fail_open");
    d.warm().unwrap();
    let json = serde_json::to_value(d.ask("enqueue job", "", 3)).unwrap();
    assert_eq!(json["status"], "hints");
    assert_eq!(json["provenance"]["ranking"], "cosine");
}

#[test]
fn shutdown_stops_answering() {
    let dir = repo();
    let mut d = daemon(dir.path());
    d.warm().unwrap();
    d.shutdown();
    assert_eq!(d.state(), SessionState::Stopped);
    assert!(matches!(d.ask("enqueue", "", 3), Answer::FailOpen { .. }));
}
