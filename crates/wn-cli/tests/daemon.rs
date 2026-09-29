//! The background daemon end to end with the real `wn` binary and the lexical encoder: answers are
//! identical to in-process ones, a daemon from another build is replaced, the idle timeout and a
//! stop request end it, `WN_NO_DAEMON` never starts one, and a stale socket file is replaced.
//! Every test uses its own `WHERE_NEXT_HOME`, so it never touches a user's daemon.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "commit.gpgsign", "false"]);
    for (path, text) in [
        ("src/auth.py", "\"\"\"Login sessions and token verification.\"\"\"\ndef verify_token(token):\n    pass\n"),
        ("src/upload.go", "// Package upload retries storage uploads on timeout.\npackage upload\n\nfunc RetryUpload() {}\n"),
        ("src/picker.ts", "/** Model picker listbox. */\nexport function renderPicker() {}\n"),
    ] {
        let p = d.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "initial layout"]);
    t
}

/// A cache home with the daemon stopped on drop.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        // Short path: Unix socket paths are length-limited.
        let dir = tempfile::Builder::new()
            .prefix("wnd")
            .tempdir_in("/tmp")
            .unwrap();
        Self { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = wn_env(&self.path(), &self.path(), &[], &["daemon", "stop"]);
    }
}

fn wn_env(repo: &Path, home: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, i32) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wn"));
    c.args(args)
        .current_dir(repo)
        .env("WHERE_NEXT_HOME", home)
        .env("WN_MODELS_HOME", home.join("no-models"))
        .env("WN_DAEMON_IDLE_SECS", "60")
        .env_remove("WN_NO_DAEMON")
        .env_remove("WN_NO_LOG")
        .env_remove("WN_MODEL_DIR")
        .env_remove("WN_DAEMON_BINARY_ID");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn wn(repo: &Path, home: &Path, args: &[&str]) -> (String, i32) {
    wn_env(repo, home, &[], args)
}

fn daemon_status(home: &Path, env: &[(&str, &str)]) -> serde_json::Value {
    let (out, code) = wn_env(home, home, env, &["daemon", "status", "--json"]);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

/// Lines across every repository's `usage.jsonl` under `home`.
fn logged_queries(home: &Path) -> usize {
    fs::read_dir(home)
        .unwrap()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path().join("usage.jsonl")).ok())
        .map(|s| s.lines().count())
        .sum()
}

const QUERY: &str = "retry uploads when storage times out";

#[test]
fn daemon_answers_exactly_like_the_in_process_path() {
    let repo = project();
    let home = Home::new();
    let (r, h) = (repo.path(), home.path());
    assert_eq!(wn(r, &h, &["init", "--no-daemon"]).1, 0);
    assert_eq!(daemon_status(&h, &[])["running"], false);

    let first = wn(r, &h, &["ask", "--json", "--no-abstain", QUERY]);
    assert_eq!(first.1, 0);
    let status = daemon_status(&h, &[]);
    assert_eq!(status["running"], true, "first ask starts the daemon");
    assert_eq!(status["stats"]["requests"], 1);

    let second = wn(r, &h, &["ask", "--json", "--no-abstain", QUERY]);
    let local = wn(
        r,
        &h,
        &["ask", "--json", "--no-abstain", "--no-daemon", QUERY],
    );
    assert_eq!(first, second);
    assert_eq!(first, local, "daemon and in-process answers differ");
    assert!(first.0.contains("src/upload.go"), "{}", first.0);
    assert_eq!(daemon_status(&h, &[])["stats"]["requests"], 2);
    assert_eq!(
        logged_queries(&h),
        3,
        "daemon answers are logged like local ones"
    );

    let text_daemon = wn(r, &h, &["status"]);
    let text_local = wn(r, &h, &["status", "--no-daemon"]);
    assert_eq!(text_daemon, text_local);
    assert_eq!(daemon_status(&h, &[])["stats"]["requests"], 3);

    let (out, code) = wn(&h, &h, &["daemon", "stop"]);
    assert_eq!(code, 0);
    assert!(out.contains("stopped"), "{out}");
    assert_eq!(daemon_status(&h, &[])["running"], false);
}

#[test]
fn a_daemon_from_another_build_is_replaced() {
    let repo = project();
    let home = Home::new();
    let (r, h) = (repo.path(), home.path());
    let old = [("WN_DAEMON_BINARY_ID", "build-old")];
    let new = [("WN_DAEMON_BINARY_ID", "build-new")];
    assert_eq!(wn_env(&h, &h, &old, &["daemon", "start"]).1, 0);
    let before = daemon_status(&h, &old);
    assert_eq!(before["stats"]["binary"], "build-old");

    let (out, code) = wn_env(r, &h, &new, &["ask", "--json", "--no-abstain", QUERY]);
    assert_eq!(code, 0);
    assert!(out.contains("src/upload.go"), "{out}");
    let after = daemon_status(&h, &new);
    assert_eq!(after["stats"]["binary"], "build-new");
    assert_ne!(before["stats"]["pid"], after["stats"]["pid"]);
}

#[test]
fn idle_timeout_ends_the_daemon() {
    let home = Home::new();
    let h = home.path();
    let quick = [("WN_DAEMON_IDLE_SECS", "1")];
    assert_eq!(wn_env(&h, &h, &quick, &["daemon", "start"]).1, 0);
    assert_eq!(daemon_status(&h, &[])["running"], true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon_status(&h, &[])["running"] == true {
        assert!(
            Instant::now() < deadline,
            "daemon outlived its idle timeout"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn no_daemon_env_never_starts_one() {
    let repo = project();
    let home = Home::new();
    let (r, h) = (repo.path(), home.path());
    let off = [("WN_NO_DAEMON", "1")];
    assert_eq!(wn_env(r, &h, &off, &["ask", "--no-abstain", QUERY]).1, 0);
    assert_eq!(daemon_status(&h, &[])["running"], false);
}

#[test]
fn a_stale_socket_file_is_replaced() {
    let repo = project();
    let home = Home::new();
    let (r, h) = (repo.path(), home.path());
    fs::write(h.join("daemon.sock"), "stale").unwrap();
    let (out, code) = wn(r, &h, &["ask", "--json", "--no-abstain", QUERY]);
    assert_eq!(code, 0);
    assert!(out.contains("src/upload.go"), "{out}");
    assert_eq!(daemon_status(&h, &[])["running"], true);
}
