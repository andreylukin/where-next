//! History replay plumbing against small repositories built in the test.

use std::fs;
use std::path::Path;
use std::process::Command;

use wn_git::replay::{ancestors_among, is_test_path, read_blobs, replay_commits, source_tree};

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn commit(dir: &Path, msg: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn init() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q", "-b", "main"]);
    git(t.path(), &["config", "commit.gpgsign", "false"]);
    t
}

#[test]
fn commits_come_oldest_first_with_parents_and_changed_paths() {
    let t = init();
    let d = t.path();
    write(d, "src/a.py", "def a():\n    pass\n");
    write(d, "README.md", "# demo\n");
    let root = commit(d, "initial");
    write(d, "src/a.py", "def a():\n    return 1\n");
    write(d, "src/b.go", "package b\n");
    let second = commit(d, "second: add b\n\nlonger body");
    write(d, "src/b.go", "package b\n\nfunc B() {}\n");
    let third = commit(d, "third");

    let commits = replay_commits(d, 10);
    // The root commit has no parent to rank against and is skipped.
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, second);
    assert_eq!(commits[0].parent, root);
    assert_eq!(commits[0].subject, "second: add b");
    assert_eq!(commits[0].body.trim(), "longer body");
    assert_eq!(commits[0].changed, vec!["src/a.py", "src/b.go"]);
    assert_eq!(commits[1].sha, third);
    assert_eq!(commits[1].changed, vec!["src/b.go"]);
    // A limit keeps the newest ones.
    let newest = replay_commits(d, 1);
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].sha, third);
}

#[test]
fn merges_are_skipped() {
    let t = init();
    let d = t.path();
    write(d, "src/a.py", "a = 1\n");
    commit(d, "initial");
    git(d, &["checkout", "-q", "-b", "side"]);
    write(d, "src/side.py", "s = 1\n");
    let side = commit(d, "side work");
    git(d, &["checkout", "-q", "main"]);
    write(d, "src/a.py", "a = 2\n");
    let main = commit(d, "main work");
    git(d, &["merge", "-q", "--no-ff", "side", "-m", "merge side"]);
    let shas: Vec<String> = replay_commits(d, 10).into_iter().map(|c| c.sha).collect();
    assert_eq!(shas.len(), 2);
    assert!(shas.contains(&side) && shas.contains(&main));
}

#[test]
fn source_tree_lists_source_files_with_their_blobs() {
    let t = init();
    let d = t.path();
    write(d, "src/a.py", "def a():\n    pass\n");
    write(d, "web/app.ts", "export const x = 1\n");
    write(d, "README.md", "# demo\n");
    write(d, "package-lock.json", "{}\n");
    let rev = commit(d, "initial");
    let tree = source_tree(d, &rev);
    let paths: Vec<&str> = tree.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, vec!["src/a.py", "web/app.ts"]);
    assert_eq!(
        tree[0].1,
        git(d, &["rev-parse", &format!("{rev}:src/a.py")])
    );
    assert!(source_tree(d, "not-a-rev").is_empty());
}

#[test]
fn blobs_are_read_truncated_and_missing_ones_skipped() {
    let t = init();
    let d = t.path();
    write(d, "src/a.py", "hello world\n");
    write(d, "src/b.py", "second file\n");
    let rev = commit(d, "initial");
    let a = git(d, &["rev-parse", &format!("{rev}:src/a.py")]);
    let b = git(d, &["rev-parse", &format!("{rev}:src/b.py")]);
    let missing = "0".repeat(40);
    let got = read_blobs(d, &[a.clone(), missing.clone(), b.clone()], 5);
    assert_eq!(got.get(&a).map(String::as_str), Some("hello"));
    assert_eq!(got.get(&b).map(String::as_str), Some("secon"));
    assert!(!got.contains_key(&missing));
    let full = read_blobs(d, std::slice::from_ref(&a), 1 << 20);
    assert_eq!(full[&a], "hello world\n");
    assert!(read_blobs(d, &[], 10).is_empty());
}

#[test]
fn test_paths_follow_the_reference_rule() {
    for p in [
        "pkg/tests/test_x.py",
        "a/test/b.rs",
        "src/__tests__/x.ts",
        "__tests__/x.js",
        "test_top.py",
        "pkg/test_util.py",
        "server/handler_test.go",
        "pkg/x_test.py",
        "web/app.spec.ts",
        "web/app.test.tsx",
        "web/app.spec.jsx",
        "pkg/dist/bundle.js",
    ] {
        assert!(is_test_path(p), "{p} should be a test path");
    }
    for p in [
        "tests/top_level_dir_needs_a_slash_before.py",
        "src/testing.py",
        "src/contest.py",
        "web/app.spec.css",
        "web/app.tests.ts",
        "distance/metric.py",
        "src/latest_version.go",
    ] {
        assert!(!is_test_path(p), "{p} should not be a test path");
    }
}

#[test]
fn ancestry_follows_the_commit_graph_including_merges() {
    let t = init();
    let d = t.path();
    write(d, "src/a.py", "a = 1\n");
    let a = commit(d, "a");
    git(d, &["checkout", "-q", "-b", "side"]);
    write(d, "src/side.py", "s = 1\n");
    let s = commit(d, "side");
    git(d, &["checkout", "-q", "main"]);
    write(d, "src/a.py", "a = 2\n");
    let b = commit(d, "b");
    git(d, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    write(d, "src/a.py", "a = 3\n");
    let c = commit(d, "c after merge");
    let wanted = vec![a.clone(), s.clone(), b.clone(), c.clone()];
    let anc = ancestors_among(d, &wanted, 100);
    // a is an ancestor of everything else, and of nothing is itself.
    assert!(!anc[0][0] && !anc[0][1] && !anc[0][2] && !anc[0][3]);
    assert!(anc[1][0] && !anc[1][2], "side sees a, not main's b");
    assert!(anc[2][0] && !anc[2][1], "b sees a, not side");
    assert!(
        anc[3][0] && anc[3][1] && anc[3][2],
        "after the merge, both lines are ancestors"
    );
    // Too shallow a walk forgets distant ancestors (conservative).
    let shallow = ancestors_among(d, &wanted, 2);
    assert!(!shallow[3][0]);
}
