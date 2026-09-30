//! Session behaviour around a workspace (wn-core runtime, hashing encoder, generated git repos):
//! fail-open answers, degradation and recovery, background refresh, snapshots.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wn_core::encoder::HashEncoder;
use wn_core::rank::AnswerState;
use wn_daemon::daemon::{background, Daemon};
use wn_daemon::session::SessionState;
use wn_daemon::workspace::{SharedEncoder, Workspace};

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
    fs::write(
        dir.path().join("auth.py"),
        "\"\"\"Tokens.\"\"\"\n\ndef verify_token(t):\n    pass\n",
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

fn encoder() -> SharedEncoder {
    Arc::new(HashEncoder { dim: 128 })
}

fn daemon(root: &Path, cache: &Path) -> Daemon {
    let mut ws = Workspace::open(root, cache, encoder());
    ws.options.no_abstain = true; // the hashing encoder is not calibrated
    Daemon::new(ws)
}

#[test]
fn fails_open_before_warm_then_serves() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut d = daemon(dir.path(), cache.path());
    let cold = d.ask("enqueue a job", "");
    assert_eq!(cold.outcome.state, AnswerState::Error);
    assert_eq!(cold.session, "Starting");
    d.warm().unwrap();
    assert_eq!(d.state(), SessionState::Serving);
    let warm = d.ask("enqueue a job", "");
    assert_eq!(warm.outcome.state, AnswerState::Ok);
    assert_eq!(warm.outcome.hints.files[0].path, "queue.rs");
    assert!(warm.outcome.hints.files.len() <= 3);
}

#[test]
fn failed_warm_degrades_and_retry_recovers() {
    let dir = tempfile::tempdir().unwrap(); // empty, not a repo
    let cache = tempfile::tempdir().unwrap();
    let mut d = daemon(dir.path(), cache.path());
    let _ = d.warm();
    // An empty directory warms successfully but has nothing to index: answers fail open.
    let reply = d.ask("x", "");
    assert_ne!(reply.outcome.state, AnswerState::Ok);
    git(dir.path(), &["init", "-q"]);
    fs::write(dir.path().join("a.py"), "def alpha():\n    pass\n").unwrap();
    d.refresh().unwrap();
    assert_eq!(d.ask("alpha", "").outcome.state, AnswerState::Ok);
}

#[test]
fn reply_serializes_flat_with_state_and_provenance() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut d = daemon(dir.path(), cache.path());
    d.warm().unwrap();
    let json = serde_json::to_value(d.ask("verify the token", "")).unwrap();
    assert_eq!(json["state"], "ok");
    assert_eq!(json["files"][0]["path"], "auth.py");
    assert_eq!(json["session"], "Serving");
    assert!(json["provenance"]["model"].as_str().unwrap().len() > 3);
}

#[test]
fn snapshot_survives_restart_without_reembedding() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut first = daemon(dir.path(), cache.path());
    assert!(first.warm().unwrap().encoded > 0);
    let mut second = daemon(dir.path(), cache.path());
    assert_eq!(second.warm().unwrap().encoded, 0);
}

#[test]
fn shutdown_stops_answering() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut d = daemon(dir.path(), cache.path());
    d.warm().unwrap();
    d.shutdown();
    assert_eq!(d.state(), SessionState::Stopped);
    assert_eq!(d.ask("enqueue", "").outcome.state, AnswerState::Error);
}

#[test]
fn background_refresher_warms_and_picks_up_new_files() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let shared = Arc::new(Mutex::new(daemon(dir.path(), cache.path())));
    let refresher = background::spawn(shared.clone(), Duration::from_millis(100));
    let deadline = Instant::now() + Duration::from_secs(15);
    while shared.lock().unwrap().state() != SessionState::Serving {
        assert!(Instant::now() < deadline, "never warmed");
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::write(
        dir.path().join("mailer.py"),
        "def send_welcome_email():\n    pass\n",
    )
    .unwrap();
    loop {
        let reply = shared.lock().unwrap().ask("send the welcome email", "");
        if reply.outcome.hints.files.first().map(|h| h.path.as_str()) == Some("mailer.py") {
            break;
        }
        assert!(Instant::now() < deadline, "new file never indexed");
        std::thread::sleep(Duration::from_millis(50));
    }
    refresher.stop();
}

#[test]
fn an_empty_query_is_an_error_like_the_cli() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut d = daemon(dir.path(), cache.path());
    d.warm().unwrap();
    let reply = d.ask("  ", "");
    assert_eq!(reply.outcome.state, AnswerState::Error);
    assert_eq!(
        reply.outcome.error.as_deref(),
        Some(wn_core::runtime::EMPTY_QUERY)
    );
    assert!(reply.outcome.hints.files.is_empty());
}

/// An MCP workspace opened before another indexer stored a newer index must not keep (and later
/// overwrite with) its stale copy, even when it did not have to wait for the lock.
#[test]
fn a_workspace_reloads_an_index_another_indexer_stored_meanwhile() {
    use wn_daemon::workspace::scan;

    let repo = tempfile::tempdir().unwrap();
    for i in 0..12 {
        std::fs::write(
            repo.path().join(format!("f{i}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let enc = || Arc::new(HashEncoder::default()) as SharedEncoder;
    let mut stale = Workspace::open(repo.path(), cache.path(), enc());
    let mut other = Workspace::open(repo.path(), cache.path(), enc());
    assert_eq!(other.apply(scan(repo.path())).unwrap().encoded, 12);
    let stats = stale.apply(scan(repo.path())).unwrap();
    assert_eq!(
        stats.encoded, 0,
        "re-embedded what the other indexer stored"
    );
    assert_eq!(stale.provenance().files_indexed, 12);
}
