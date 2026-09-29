//! End-to-end engine tests on a generated git repository, with the hashing encoder (no weights).

use std::fs;
use std::path::Path;
use std::process::Command;

use wn_core::index_lifecycle::IndexState;
use wn_daemon::engine::Engine;
use wn_daemon::fake::HashEncoder;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/upload.py"),
        "\"\"\"Upload chunks to object storage.\"\"\"\n\ndef upload_part(chunk):\n    pass\n\ndef retry_backoff(n):\n    pass\n",
    )
    .unwrap();
    fs::write(
        root.join("src/auth.py"),
        "\"\"\"Session tokens.\"\"\"\n\ndef verify_token(t):\n    pass\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# demo\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "init"]);
    dir
}

fn engine(root: &Path, cache: Option<&Path>) -> Engine<HashEncoder> {
    Engine::new(root, cache.map(Path::to_path_buf), HashEncoder::new(256))
}

#[test]
fn build_then_ask_finds_the_relevant_file() {
    let dir = repo();
    let mut e = engine(dir.path(), None);
    let stats = e.build().unwrap();
    assert_eq!(stats.documents, 2, "only source files are indexed");
    assert_eq!(e.index_state(), IndexState::Ready);
    let hints = e.ask("retry the upload part", "", 3).unwrap();
    assert_eq!(hints[0].path, "src/upload.py");
    assert!(hints[0].reason.contains("upload_part"));
    assert!(hints.len() <= 3);
}

#[test]
fn refresh_picks_up_untracked_files_and_drops_deleted_ones() {
    let dir = repo();
    let root = dir.path();
    let mut e = engine(root, None);
    e.build().unwrap();
    fs::write(
        root.join("src/billing.py"),
        "def charge_invoice():\n    pass\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("ignored")).unwrap();
    fs::write(
        root.join("ignored/x.py"),
        "def charge_invoice():\n    pass\n",
    )
    .unwrap();
    fs::remove_file(root.join("src/auth.py")).unwrap();
    let stats = e.refresh().unwrap();
    assert_eq!((stats.embedded, stats.removed, stats.documents), (1, 1, 2));
    let hints = e.ask("charge the invoice", "", 3).unwrap();
    assert_eq!(hints[0].path, "src/billing.py");
    assert!(hints.iter().all(|h| h.path != "src/auth.py"));
    assert!(hints.iter().all(|h| !h.path.starts_with("ignored/")));
}

#[test]
fn unchanged_refresh_embeds_nothing() {
    let dir = repo();
    let mut e = engine(dir.path(), None);
    e.build().unwrap();
    let stats = e.refresh().unwrap();
    assert_eq!((stats.embedded, stats.removed), (0, 0));
}

#[test]
fn snapshot_is_reused_across_engines() {
    let dir = repo();
    let cache = tempfile::tempdir().unwrap();
    let mut first = engine(dir.path(), Some(cache.path()));
    assert!(!first.build().unwrap().from_snapshot);
    let mut second = engine(dir.path(), Some(cache.path()));
    let stats = second.build().unwrap();
    assert!(stats.from_snapshot);
    assert_eq!(
        stats.embedded, 0,
        "nothing re-embedded from a fresh snapshot"
    );
    assert_eq!(second.encoder_mut().calls, 0);
}

#[test]
fn asking_before_build_fails_open() {
    let dir = repo();
    let mut e = engine(dir.path(), None);
    assert!(e.ask("anything", "", 3).is_err());
}

#[test]
fn non_git_directory_fails_build_into_error_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = engine(dir.path(), None);
    assert!(e.build().is_err());
    assert_eq!(e.index_state(), IndexState::Error);
}
