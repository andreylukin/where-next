//! The `wn` binary against a real repository built in the test, with snapshot-tested output.

use std::fs;
use std::path::Path;
use std::process::Command;

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

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A small project with enough history for the adapter to fit.
fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "commit.gpgsign", "false"]);
    write(d, "src/auth.py", "\"\"\"Login sessions and token verification.\"\"\"\ndef login(user):\n    pass\n\ndef verify_token(token):\n    pass\n");
    write(d, "src/upload.go", "// Package upload retries storage uploads on timeout.\npackage upload\n\nfunc RetryUpload() {}\n");
    write(
        d,
        "src/picker.ts",
        "/** Model picker listbox with keyboard support. */\nexport function renderPicker() {}\n",
    );
    write(d, "Dockerfile", "FROM python:3.12\nRUN pip install app\n");
    write(d, "README.md", "# demo\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "initial project layout"]);
    let work = [
        ("src/auth.py", "fix login session expiry"),
        ("src/upload.go", "retry uploads when storage times out"),
        (
            "src/picker.ts",
            "picker: keyboard navigation in the listbox",
        ),
    ];
    for i in 0..24 {
        let (path, msg) = work[i % 3];
        let p = d.join(path);
        let mut text = fs::read_to_string(&p).unwrap();
        text.push_str(&format!(
            "{} change {i}\n",
            if path.ends_with(".py") { "#" } else { "//" }
        ));
        fs::write(p, text).unwrap();
        git(d, &["commit", "-q", "-am", &format!("{msg} ({i})")]);
    }
    t
}

fn wn(repo: &Path, home: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_wn"))
        .args(args)
        .arg("--path")
        .arg(repo)
        .env("WHERE_NEXT_HOME", home)
        .output()
        .unwrap();
    let text = if out.status.success() {
        out.stdout
    } else {
        out.stderr
    };
    (
        String::from_utf8_lossy(&text).trim_end().to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn redact(text: &str, repo: &Path) -> String {
    let name = repo.file_name().unwrap().to_string_lossy();
    let mut s = text.replace(&*name, "[repo]");
    // Adapter revisions depend on float rounding across platforms.
    while let Some(i) = s.find("revision ") {
        let end = s[i + 9..]
            .find(|c: char| !c.is_ascii_hexdigit())
            .map_or(s.len(), |e| i + 9 + e);
        s.replace_range(i + 9..end, "[rev]");
        if !s[end.min(s.len())..].contains("revision ") {
            break;
        }
    }
    s
}

#[test]
fn init_status_ask_train_rollback() {
    let repo = project();
    let home = tempfile::tempdir().unwrap();
    let r = repo.path();

    let (init, code) = wn(r, home.path(), &["init"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!("init", redact(&init, r));

    let (status, _) = wn(r, home.path(), &["status"]);
    insta::assert_snapshot!("status", redact(&status, r));

    let (ask, code) = wn(
        r,
        home.path(),
        &["ask", "the login session expires too early"],
    );
    assert_eq!(code, 0);
    assert!(ask.contains("adapter on"), "{ask}");
    let first = ask.lines().nth(1).unwrap();
    assert!(
        first.ends_with("src/auth.py"),
        "top hint should be auth.py:\n{ask}"
    );

    let (json, _) = wn(
        r,
        home.path(),
        &["ask", "storage upload timeout", "--json", "--functions"],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["state"], "ok");
    assert_eq!(v["files"][0]["path"], "src/upload.go");
    assert_eq!(v["adapter"]["applied"], true);

    let (plain, _) = wn(
        r,
        home.path(),
        &["ask", "storage upload timeout", "--no-adapter"],
    );
    assert!(plain.contains("adapter off"));

    let (train, _) = wn(r, home.path(), &["train"]);
    assert!(train.contains("adapter: active"), "{train}");
    let (rb, _) = wn(r, home.path(), &["rollback"]);
    assert_eq!(rb, "adapter: restored the previous adapter");
    let (rb2, _) = wn(r, home.path(), &["rollback"]);
    assert!(rb2.starts_with("adapter: removed"), "{rb2}");
    let (rb3, _) = wn(r, home.path(), &["rollback"]);
    assert_eq!(rb3, "adapter: nothing to roll back");

    let (mcp, code) = wn(r, home.path(), &["mcp"]);
    assert_eq!(code, 2);
    assert!(mcp.contains("wn-mcp"));
}

#[test]
fn empty_and_unsupported_repositories_fail_open() {
    let home = tempfile::tempdir().unwrap();
    let docs = tempfile::tempdir().unwrap();
    write(docs.path(), "notes.md", "# notes\n");
    let (out, code) = wn(docs.path(), home.path(), &["ask", "anything"]);
    assert_eq!(code, 0);
    assert_eq!(out, "where-next: unsupported_scope; use normal search.");

    let empty = tempfile::tempdir().unwrap();
    let (out, _) = wn(empty.path(), home.path(), &["ask", "anything", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["state"], "empty_index");
}
