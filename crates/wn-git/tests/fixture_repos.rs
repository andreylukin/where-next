//! wn-git against real repositories built inside each test.

use std::fs;
use std::path::Path;
use std::process::Command;

use wn_git::{co_change, commits_since, head, history, repo_root, scan, ContentId};
use wn_sources::Kind;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn repo() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q", "-b", "main"]);
    git(t.path(), &["config", "commit.gpgsign", "false"]);
    t
}

#[test]
fn scan_classifies_tracked_modified_untracked_and_deleted_files() {
    let t = repo();
    let d = t.path();
    write(d, "src/auth.py", "def login():\n    pass\n");
    write(d, "src/gone.go", "package x\n");
    write(d, "src/edit.rs", "fn a() {}\n");
    write(d, "vendor/lib/x.go", "package v\n");
    write(d, "Cargo.lock", "lock");
    write(d, "Dockerfile", "FROM x\n");
    write(d, "README.md", "# hi\n");
    commit(d, "initial import of the project");
    fs::remove_file(d.join("src/gone.go")).unwrap();
    write(d, "src/edit.rs", "fn a() {}\nfn b() {}\n");
    write(d, "src/new.ts", "export function n() {}\n");
    write(d, "notes.txt", "x");

    let (files, cov) = scan(d);
    assert!(cov.git);
    let kinds: Vec<(&str, Kind)> = files.iter().map(|(p, f)| (p.as_str(), f.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            ("Dockerfile", Kind::Config),
            ("src/auth.py", Kind::Source),
            ("src/edit.rs", Kind::Source),
            ("src/new.ts", Kind::Source),
        ]
    );
    assert!(matches!(files["src/auth.py"].id, ContentId::Blob(ref s) if s.len() == 40));
    assert!(
        matches!(files["src/edit.rs"].id, ContentId::Mtime(_)),
        "modified files use mtime"
    );
    assert!(
        matches!(files["src/new.ts"].id, ContentId::Mtime(_)),
        "untracked files use mtime"
    );
    assert_eq!(cov.deleted_dropped, 1);
    assert_eq!(cov.modified, 1);
    assert_eq!(cov.untracked, 1);
    assert_eq!(cov.unsupported, 2, "README.md and notes.txt");
    assert_eq!(cov.unsupported_ext.get(".md"), Some(&1));
}

#[test]
fn scanning_a_subdirectory_still_sees_modified_files() {
    let t = repo();
    let d = t.path();
    write(d, "project/src/edit.rs", "fn a() {}\n");
    write(d, "other/x.rs", "fn x() {}\n");
    commit(d, "initial");
    write(d, "project/src/edit.rs", "fn a() {}\nfn b() {}\n");
    let (files, cov) = scan(&d.join("project"));
    assert_eq!(files.keys().collect::<Vec<_>>(), vec!["src/edit.rs"]);
    assert!(
        matches!(files["src/edit.rs"].id, ContentId::Mtime(_)),
        "modified files use mtime below the repository root too"
    );
    assert_eq!(cov.modified, 1);
}

#[test]
fn scan_walks_plain_directories() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "a/b.py", "x = 1\n");
    write(t.path(), "node_modules/p/i.js", "x");
    write(t.path(), "c.txt", "x");
    let (files, cov) = scan(t.path());
    assert!(!cov.git);
    assert_eq!(files.keys().collect::<Vec<_>>(), vec!["a/b.py"]);
    assert_eq!(cov.unsupported, 1);
}

#[cfg(unix)]
#[test]
fn scan_skips_tracked_and_untracked_symlinks() {
    use std::os::unix::fs::symlink;

    let t = repo();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "private.py", "def private_symbol(): pass\n");
    write(t.path(), "safe.py", "def safe_symbol(): pass\n");
    symlink(
        outside.path().join("private.py"),
        t.path().join("linked.py"),
    )
    .unwrap();
    git(t.path(), &["add", "safe.py", "linked.py"]);
    let (files, _) = scan(t.path());
    assert!(files.contains_key("safe.py"));
    assert!(!files.contains_key("linked.py"));

    symlink(
        outside.path().join("private.py"),
        t.path().join("untracked.py"),
    )
    .unwrap();
    let (files, _) = scan(t.path());
    assert!(!files.contains_key("untracked.py"));
}

#[test]
fn history_lists_non_merge_commits_newest_first_with_paths() {
    let t = repo();
    let d = t.path();
    write(d, "a.py", "1\n");
    commit(d, "first commit\n\nBody line one\nmore");
    write(d, "b.py", "1\n");
    write(d, "a.py", "2\n");
    commit(d, "second: touch a and b");
    git(d, &["checkout", "-q", "-b", "side"]);
    write(d, "c.py", "1\n");
    commit(d, "side work");
    git(d, &["checkout", "-q", "main"]);
    write(d, "d.py", "1\n");
    commit(d, "main work");
    git(d, &["merge", "-q", "--no-ff", "side", "-m", "merge side"]);

    let h = history(d, 200);
    let subjects: Vec<&str> = h.iter().map(|c| c.subject.as_str()).collect();
    assert!(!subjects.contains(&"merge side"), "merges are excluded");
    assert_eq!(subjects.len(), 4);
    assert_eq!(subjects.last(), Some(&"first commit"));
    let first = h.last().unwrap();
    assert_eq!(first.body.trim(), "Body line one\nmore");
    assert_eq!(first.paths, vec!["a.py"]);
    let second = h.iter().find(|c| c.subject.starts_with("second")).unwrap();
    assert_eq!(second.paths, vec!["a.py", "b.py"]);
    assert_eq!(second.sha.len(), 40);
    assert!(!second.date.is_empty());

    assert_eq!(
        history(d, 1).len().min(3),
        history(d, 1).len(),
        "limit bounds the read"
    );
    let tip = head(d).unwrap();
    assert_eq!(tip.len(), 40);
    assert_eq!(commits_since(d, &tip), Some(0));
    assert_eq!(
        commits_since(d, &first.sha),
        Some(3),
        "non-merge commits after the first"
    );
    assert_eq!(commits_since(d, "not-a-sha"), None);
    assert_eq!(
        repo_root(&d.join(".")).canonicalize().unwrap(),
        d.canonicalize().unwrap()
    );
}

#[test]
fn co_change_counts_pairs_changed_together() {
    let t = repo();
    let d = t.path();
    write(d, "a.py", "1\n");
    write(d, "b.py", "1\n");
    commit(d, "a and b together");
    write(d, "a.py", "2\n");
    write(d, "b.py", "2\n");
    write(d, "c.py", "2\n");
    commit(d, "a b c");
    let pairs = co_change(&history(d, 200), 10);
    assert_eq!(
        pairs.get(&("a.py".to_string(), "b.py".to_string())),
        Some(&2)
    );
    assert_eq!(
        pairs.get(&("a.py".to_string(), "c.py".to_string())),
        Some(&1)
    );
    assert!(
        !pairs.contains_key(&("b.py".to_string(), "a.py".to_string())),
        "pairs are ordered"
    );
}
