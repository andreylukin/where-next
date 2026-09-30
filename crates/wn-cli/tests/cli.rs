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

/// An empty git repository (wn refuses directories outside one).
fn git_dir() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q", "-b", "main"]);
    t
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

/// `wn` with an isolated cache and no installed models (the lexical fallback answers), so
/// results do not depend on what the developer has downloaded.
fn wn_command(repo: &Path, home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wn"));
    c.arg("--path")
        .arg(repo)
        .env("WHERE_NEXT_HOME", home)
        .env("WN_MODELS_HOME", home.join("no-models"))
        .env("WN_NO_DAEMON", "1")
        .env_remove("WN_MODEL_DIR");
    c
}

fn wn(repo: &Path, home: &Path, args: &[&str]) -> (String, i32) {
    let out = wn_command(repo, home).args(args).output().unwrap();
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

    let (skip, _) = wn(
        r,
        home.path(),
        &["ask", "the login session expires", "--start"],
    );
    assert!(
        skip.contains("start hints help in large repositories"),
        "{skip}"
    );
    let (small, _) = wn(
        r,
        home.path(),
        &[
            "ask",
            "the login session expires",
            "--start",
            "--start-min-files",
            "1",
        ],
    );
    assert!(small.contains("src/auth.py"), "{small}");

    let (train, _) = wn(r, home.path(), &["train"]);
    assert!(train.contains("adapter: active"), "{train}");
    let (rb, _) = wn(r, home.path(), &["rollback"]);
    assert_eq!(rb, "adapter: restored the previous adapter");
    let (rb2, _) = wn(r, home.path(), &["rollback"]);
    assert!(rb2.starts_with("adapter: removed"), "{rb2}");
    let (rb3, _) = wn(r, home.path(), &["rollback"]);
    assert_eq!(rb3, "adapter: nothing to roll back");
}

/// `wn mcp` speaks MCP over stdio: initialize, list tools, call where_next, exit on EOF.
#[test]
fn mcp_serves_tools_over_stdio() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let repo = project();
    let home = tempfile::tempdir().unwrap();
    let mut child = wn_command(repo.path(), home.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut send = |v: serde_json::Value| writeln!(stdin, "{v}").unwrap();
    let mut read_id = |id: i64| loop {
        let line = lines.next().expect("server closed stdout").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v["id"] == id {
            return v;
        }
    };
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}}}),
    );
    let init = read_id(1);
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = read_id(2);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"where_next"), "{names:?}");
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "where_next", "arguments": {"query": "storage upload timeout"}}}),
    );
    let call = read_id(3);
    let body: serde_json::Value =
        serde_json::from_str(call["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(body["state"].is_string(), "{body}");
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
        "name": "where_next", "arguments": {"query": "storage upload timeout", "start": true}}}),
    );
    let start = read_id(4);
    let text = start["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("start hints help in large repositories") || text.contains("not serving"),
        "{text}"
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn empty_and_unsupported_repositories_fail_open() {
    let home = tempfile::tempdir().unwrap();
    let docs = git_dir();
    write(docs.path(), "notes.md", "# notes\n");
    let (out, code) = wn(docs.path(), home.path(), &["ask", "anything"]);
    assert_eq!(code, 0);
    assert_eq!(out, "where-next: unsupported_scope; use normal search.");

    let empty = git_dir();
    let (out, _) = wn(empty.path(), home.path(), &["ask", "anything", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["state"], "empty_index");
}

#[test]
fn ask_without_a_model_says_how_to_install_one() {
    let home = tempfile::tempdir().unwrap();
    let repo = project();
    let (out, code) = wn(
        repo.path(),
        home.path(),
        &["ask", "where is the upload retried"],
    );
    assert_eq!(code, 0);
    assert!(out.contains("run `wn model pull`"), "{out}");
    // JSON stays machine-readable: no note appended.
    let (json, _) = wn(
        repo.path(),
        home.path(),
        &["ask", "where is the upload retried", "--json"],
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&json).is_ok(),
        "{json}"
    );
}

#[test]
fn an_empty_query_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let repo = project();
    for q in ["", "   "] {
        let (out, code) = wn(repo.path(), home.path(), &["ask", q]);
        assert_eq!(code, 2, "{out}");
        assert!(out.contains("empty query"), "{out}");
        let (json, code) = wn(repo.path(), home.path(), &["ask", q, "--json"]);
        assert_eq!(code, 2, "{json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["state"], "error");
        assert!(
            v["error"].as_str().unwrap().contains("empty query"),
            "{json}"
        );
        assert!(v["files"].as_array().is_none_or(|f| f.is_empty()));
    }
    // A context file alone (say, a pasted stack trace) is still a query.
    let ctx = home.path().join("trace.txt");
    fs::write(&ctx, "upload retry timed out\n").unwrap();
    let (out, code) = wn(
        repo.path(),
        home.path(),
        &["ask", "", "--context-file", ctx.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{out}");
}

#[test]
fn model_list_offers_the_default_model() {
    let home = tempfile::tempdir().unwrap();
    let repo = project();
    let (out, code) = wn(repo.path(), home.path(), &["model", "list"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("wn model pull gemma-xl1") && out.contains("(default)"),
        "{out}"
    );
}

#[test]
fn a_path_that_does_not_exist_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("no-such-dir");
    for args in [&["status"][..], &["init"], &["ask", "anything"]] {
        let (out, code) = wn(&missing, home.path(), args);
        assert_eq!(code, 2, "{args:?}: {out}");
        assert!(out.contains("does not exist"), "{args:?}: {out}");
    }
    assert!(!home.path().join("repos").exists());
}

#[test]
fn an_unusable_model_directory_is_reported_not_silent() {
    let home = tempfile::tempdir().unwrap();
    let repo = project();
    let empty_model = tempfile::tempdir().unwrap();
    let model = empty_model.path().to_str().unwrap();
    let (out, code) = wn(
        repo.path(),
        home.path(),
        &["--model", model, "ask", "where is the upload retried"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("note: model unavailable"), "{out}");
    let (out, _) = wn(repo.path(), home.path(), &["--model", model, "status"]);
    assert!(
        out.contains("model: lexical fallback (model unavailable"),
        "{out}"
    );
}

#[test]
fn nothing_to_rank_says_what_to_do_next() {
    let home = tempfile::tempdir().unwrap();
    let empty = git_dir();
    let (out, code) = wn(empty.path(), home.path(), &["ask", "anything"]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("where-next: empty_index; use normal search."),
        "{out}"
    );
    assert!(out.contains("note: no source files found here"), "{out}");
}

#[test]
fn status_reads_naturally_for_single_files() {
    let home = tempfile::tempdir().unwrap();
    let t = git_dir();
    write(t.path(), "main.rs", "fn main() {}\n");
    write(t.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
    let (out, _) = wn(t.path(), home.path(), &["status"]);
    assert!(
        out.contains("1 source file, 1 config file indexed"),
        "{out}"
    );
    assert!(
        out.contains("model: lexical fallback (no model installed; run `wn model pull`)"),
        "{out}"
    );
}

#[test]
fn a_directory_outside_git_is_refused_quickly_unless_any_dir() {
    let home = tempfile::tempdir().unwrap();
    let plain = tempfile::tempdir().unwrap();
    for i in 0..50 {
        write(plain.path(), &format!("a/b/f{i}.rs"), "fn f() {}\n");
    }
    for args in [
        &["status"][..],
        &["init"],
        &["ask", "where is f"],
        &["train"],
    ] {
        let start = std::time::Instant::now();
        let (out, code) = wn(plain.path(), home.path(), args);
        assert_eq!(code, 2, "{args:?}: {out}");
        assert!(
            out.contains("not inside a git repository"),
            "{args:?}: {out}"
        );
        assert!(out.contains("--any-dir"), "{out}");
        assert!(
            start.elapsed().as_secs() < 5,
            "{args:?} took {:?}",
            start.elapsed()
        );
    }
    let (out, code) = wn(plain.path(), home.path(), &["--any-dir", "status"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("50 source files"), "{out}");
}

#[test]
fn the_home_directory_is_refused_even_when_it_is_a_git_repository() {
    let cache = tempfile::tempdir().unwrap();
    let home = git_dir();
    write(home.path(), "dotfiles/x.sh", "echo hi\n");
    let run = |args: &[&str]| {
        let out = wn_command(home.path(), cache.path())
            .env("HOME", home.path())
            .args(args)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stderr).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    };
    let (err, code) = run(&["ask", "where is the config"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("home directory"), "{err}");
    let (err, code) = run(&["--any-dir", "status"]);
    assert_eq!(code, 0, "{err}");
}

#[cfg(unix)]
#[test]
fn an_unwritable_cache_directory_fails_init_with_a_plain_message() {
    use std::os::unix::fs::PermissionsExt;
    let repo = project();
    let parent = tempfile::tempdir().unwrap();
    let cache = parent.path().join("cache");
    fs::create_dir(&cache).unwrap();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o555)).unwrap();
    let (out, code) = wn(repo.path(), &cache, &["init"]);
    let (json_out, json_code) = wn(repo.path(), &cache, &["init", "--json"]);
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, 1, "{out}");
    assert!(out.starts_with("wn: could not index"), "{out}");
    assert!(out.contains("cache directory"), "{out}");
    assert_eq!(json_code, 1, "{json_out}");
}

/// `wn mcp` outside a git repository (clients often start it in `~`) still starts and fails open
/// on every call, saying why and what to do, instead of exiting or indexing the directory.
#[test]
fn mcp_outside_a_git_repository_starts_and_fails_open_per_call() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let home = tempfile::tempdir().unwrap();
    let plain = tempfile::tempdir().unwrap();
    write(plain.path(), "a/f.rs", "fn f() {}\n");
    let mut child = wn_command(plain.path(), home.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut send = |v: serde_json::Value| writeln!(stdin, "{v}").unwrap();
    let mut read_id = |id: i64| loop {
        let line = lines.next().expect("server closed stdout").unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v["id"] == id {
            return v;
        }
    };
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}}}),
    );
    assert!(read_id(1)["result"]["serverInfo"].is_object());
    send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let body = |v: serde_json::Value| -> serde_json::Value {
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "where_next", "arguments": {"query": "where is f"}}}),
    );
    let answer = body(read_id(2));
    assert_eq!(answer["state"], "error", "{answer}");
    let why = answer["error"].as_str().unwrap();
    assert!(
        why.contains("not inside a git repository") && why.contains("--any-dir"),
        "{why}"
    );
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "status", "arguments": {}}}),
    );
    let status = body(read_id(3));
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("git repository"),
        "{status}"
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
    // Nothing was indexed.
    assert!(fs::read_dir(home.path())
        .unwrap()
        .flatten()
        .all(|e| !e.path().join("index").exists()));
}

/// A git repository at `$HOME` (dotfiles) never stands in for a project below it: a plain command
/// in a non-repository subdirectory is refused like `$HOME` itself, and `--any-dir` indexes only
/// that subdirectory, not the whole home directory.
#[test]
fn a_git_home_directory_never_stands_in_for_a_subdirectory() {
    let cache = tempfile::tempdir().unwrap();
    let home = git_dir();
    write(home.path(), "dotfiles/x.rs", "fn x() {}\n");
    write(home.path(), "other/y.rs", "fn y() {}\n");
    let project = home.path().join("project");
    write(&project, "a.rs", "fn a() {}\n");
    write(&project, "b.rs", "fn b() {}\n");
    let run = |args: &[&str]| {
        let out = wn_command(&project, cache.path())
            .env("HOME", home.path())
            .args(args)
            .output()
            .unwrap();
        let text = if out.status.success() {
            out.stdout
        } else {
            out.stderr
        };
        (
            String::from_utf8_lossy(&text).into_owned(),
            out.status.code().unwrap_or(-1),
        )
    };
    let (err, code) = run(&["ask", "where is a"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("home directory"), "{err}");
    let (out, code) = run(&["--any-dir", "status", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files"], 2, "{out}");
    assert_eq!(
        Path::new(v["repo"].as_str().unwrap()),
        project.canonicalize().unwrap()
    );
}
