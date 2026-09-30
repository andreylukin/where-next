//! `wn hook …`: payload fixtures for every agent and event (shapes from the Claude Code, Codex and
//! Cursor hook docs), search detection, per-session dedupe, silence rules, the time budget with a
//! slow daemon, and the output JSON each agent reads.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wn_cli::hooks::{self, Asker, Deps, HookKind, Moment};
use wn_core::rank::{AnswerState, Hint, Hints, Outcome};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn claude_prompt(cwd: &str, session: &str, prompt: &str) -> String {
    json!({
        "session_id": session,
        "transcript_path": "/Users/me/.claude/projects/x/00893aaf.jsonl",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": "UserPromptSubmit",
        "prompt": prompt
    })
    .to_string()
}

fn claude_grep(cwd: &str, pattern: &str, num_files: usize) -> String {
    let files: Vec<String> = (0..num_files).map(|i| format!("{cwd}/f{i}.rs")).collect();
    json!({
        "session_id": "s1",
        "cwd": cwd,
        "hook_event_name": "PostToolUse",
        "tool_name": "Grep",
        "tool_input": { "pattern": pattern, "output_mode": "files_with_matches" },
        "tool_response": { "mode": "files_with_matches", "filenames": files, "numFiles": num_files },
        "tool_use_id": "toolu_01",
        "duration_ms": 12
    })
    .to_string()
}

fn claude_bash(cwd: &str, command: &str, stdout: &str) -> String {
    json!({
        "session_id": "s1",
        "cwd": cwd,
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "search" },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false },
        "tool_use_id": "toolu_02"
    })
    .to_string()
}

fn claude_bash_failure(cwd: &str, command: &str, error: &str) -> String {
    json!({
        "session_id": "s1",
        "cwd": cwd,
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "tool_use_id": "toolu_03",
        "error": error,
        "is_interrupt": false
    })
    .to_string()
}

fn codex_prompt(cwd: &str) -> String {
    json!({
        "session_id": "thr_123",
        "transcript_path": null,
        "cwd": cwd,
        "hook_event_name": "UserPromptSubmit",
        "model": "gpt-5.5-codex",
        "turn_id": "turn_1",
        "permission_mode": "default",
        "prompt": "where is the retry policy for uploads"
    })
    .to_string()
}

fn codex_bash(cwd: &str, command: Value, response: Value) -> String {
    json!({
        "session_id": "thr_123",
        "cwd": cwd,
        "hook_event_name": "PostToolUse",
        "model": "gpt-5.5-codex",
        "turn_id": "turn_1",
        "tool_name": "Bash",
        "tool_use_id": "call_1",
        "tool_input": { "command": command },
        "tool_response": response
    })
    .to_string()
}

fn cursor_shell(cwd: &str, command: &str, output: Value) -> String {
    json!({
        "conversation_id": "conv-1",
        "generation_id": "gen-1",
        "model": "claude-opus",
        "hook_event_name": "postToolUse",
        "cursor_version": "1.7.2",
        "workspace_roots": [cwd],
        "user_email": null,
        "transcript_path": null,
        "tool_name": "Shell",
        "tool_input": { "command": command, "working_directory": cwd },
        "tool_output": output.to_string(),
        "tool_use_id": "abc123",
        "cwd": cwd,
        "duration": 5432
    })
    .to_string()
}

fn search(pattern: &str, results: Option<usize>) -> Moment {
    Moment::Search {
        pattern: pattern.into(),
        results,
    }
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

#[test]
fn parses_every_agent_payload() {
    let t = hooks::parse(
        HookKind::ClaudePrompt,
        &claude_prompt("/r", "s1", "fix login"),
    )
    .unwrap();
    assert_eq!(t.moment, Moment::Prompt("fix login".into()));
    assert_eq!(
        (t.session.as_str(), t.event.as_str()),
        ("s1", "UserPromptSubmit")
    );
    assert_eq!(t.cwd, PathBuf::from("/r"));

    let t = hooks::parse(HookKind::ClaudeSearch, &claude_grep("/r", "RetryPolicy", 0)).unwrap();
    assert_eq!(t.moment, search("RetryPolicy", Some(0)));
    let t = hooks::parse(HookKind::ClaudeSearch, &claude_grep("/r", "retry", 45)).unwrap();
    assert_eq!(t.moment, search("retry", Some(45)));

    let glob = json!({
        "session_id": "s1", "cwd": "/r", "hook_event_name": "PostToolUse", "tool_name": "Glob",
        "tool_input": { "pattern": "**/*upload*" },
        "tool_response": { "filenames": [], "numFiles": 0, "truncated": false, "durationMs": 3 }
    });
    let t = hooks::parse(HookKind::ClaudeSearch, &glob.to_string()).unwrap();
    assert_eq!(t.moment, search("**/*upload*", Some(0)));

    let many = (0..40)
        .map(|i| format!("src/a{i}.rs:1:retry"))
        .collect::<Vec<_>>()
        .join("\n");
    let t = hooks::parse(
        HookKind::ClaudeSearch,
        &claude_bash("/r", "rg -n retry src", &many),
    )
    .unwrap();
    assert_eq!(t.moment, search("retry", Some(40)));

    let t = hooks::parse(
        HookKind::ClaudeSearch,
        &claude_bash_failure("/r", "rg 'fn upload_part'", "Exit code 1\n"),
    )
    .unwrap();
    assert_eq!(t.moment, search("fn upload_part", Some(0)));
    assert_eq!(t.event, "PostToolUseFailure");
    // A real failure (exit 2: bad regex) is not "found nothing".
    let t = hooks::parse(
        HookKind::ClaudeSearch,
        &claude_bash_failure("/r", "rg '('", "Exit code 2\nregex parse error"),
    )
    .unwrap();
    assert_eq!(t.moment, search("(", None));

    let t = hooks::parse(HookKind::CodexPrompt, &codex_prompt("/r")).unwrap();
    assert_eq!(
        t.moment,
        Moment::Prompt("where is the retry policy for uploads".into())
    );
    assert_eq!(t.session, "thr_123");
    let t = hooks::parse(
        HookKind::CodexSearch,
        &codex_bash("/r", json!("grep -rn UploadRetry ."), json!("")),
    )
    .unwrap();
    assert_eq!(t.moment, search("UploadRetry", Some(0)));
    let t = hooks::parse(
        HookKind::CodexSearch,
        &codex_bash(
            "/r",
            json!(["bash", "-lc", "fd upload"]),
            json!({ "exit_code": 0, "stdout": "a\nb\n" }),
        ),
    )
    .unwrap();
    assert_eq!(t.moment, search("upload", Some(2)));

    let t = hooks::parse(
        HookKind::CursorSearch,
        &cursor_shell(
            "/w",
            "rg UploadRetry",
            json!({ "exitCode": 1, "stdout": "" }),
        ),
    )
    .unwrap();
    assert_eq!(t.moment, search("UploadRetry", Some(0)));
    assert_eq!(
        (t.session.as_str(), t.event.as_str()),
        ("conv-1", "postToolUse")
    );
}

#[test]
fn ignores_what_it_does_not_answer() {
    for (kind, input) in [
        (HookKind::ClaudePrompt, "garbage".to_string()),
        (HookKind::ClaudePrompt, claude_prompt("/r", "s1", "   ")),
        (HookKind::ClaudeSearch, claude_bash("/r", "cargo test", "ok")),
        (HookKind::ClaudeSearch, claude_bash("/r", "ls src", "")),
        (
            HookKind::ClaudeSearch,
            json!({ "session_id": "s", "cwd": "/r", "tool_name": "Read", "tool_input": { "file_path": "/r/a" } }).to_string(),
        ),
        (HookKind::CodexSearch, codex_bash("/r", json!("git status"), json!(""))),
    ] {
        assert_eq!(hooks::parse(kind, &input), None, "{input}");
    }
}

#[test]
fn finds_the_pattern_of_search_commands() {
    let cases = [
        ("rg -n 'fn parse_config' src", Some("fn parse_config")),
        ("rg -g '*.rs' -t rust UploadRetry", Some("UploadRetry")),
        ("rg -e retry -e backoff", Some("retry")),
        ("grep -rn \"S3Client\" .", Some("S3Client")),
        ("grep -E 'a|b' file", Some("a|b")),
        ("cd sub && git grep -n upload_part", Some("upload_part")),
        (
            "RUST_LOG=1 rg --max-count 5 retry | head -20",
            Some("retry"),
        ),
        ("find . -name '*retry*' -type f", Some("*retry*")),
        ("fd -e py upload", Some("upload")),
        ("ag --python retry", Some("retry")),
        ("rg foo 2>&1", Some("foo")),
        ("cargo test retry", None),
        ("echo rg", None),
        ("ls", None),
    ];
    for (cmd, want) in cases {
        assert_eq!(hooks::search_pattern(cmd).as_deref(), want, "{cmd}");
    }
}

#[test]
fn turns_patterns_into_query_words() {
    assert_eq!(
        hooks::intent(r"fn\s+parse_(config|args)").as_deref(),
        Some("fn parse_ config args")
    );
    assert_eq!(hooks::intent("**/*upload*").as_deref(), Some("upload"));
    assert_eq!(hooks::intent("S3Client").as_deref(), Some("S3Client"));
    assert_eq!(hooks::intent(r"\.rs$"), None, "too little to ask");
    assert_eq!(hooks::intent("(").as_deref(), None);
}

// ---------------------------------------------------------------------------------------------
// Running a hook with a stub daemon
// ---------------------------------------------------------------------------------------------

struct Stub {
    answer: Mutex<Vec<&'static str>>,
    state: AnswerState,
    delay: Duration,
    asked: Mutex<Vec<(String, String)>>,
}

impl Stub {
    fn new(paths: &[&'static str]) -> Arc<Stub> {
        Stub::with(paths, AnswerState::Ok, Duration::ZERO)
    }

    fn with(paths: &[&'static str], state: AnswerState, delay: Duration) -> Arc<Stub> {
        Arc::new(Stub {
            answer: Mutex::new(paths.to_vec()),
            state,
            delay,
            asked: Mutex::new(Vec::new()),
        })
    }
}

impl Asker for Stub {
    fn ask(&self, _root: &Path, query: &str, context: &str) -> Option<Outcome> {
        std::thread::sleep(self.delay);
        self.asked
            .lock()
            .unwrap()
            .push((query.to_string(), context.to_string()));
        let files = self
            .answer
            .lock()
            .unwrap()
            .iter()
            .map(|p| Hint {
                path: p.to_string(),
                similarity: 0.61,
                evidence: None,
                name: None,
                line: None,
            })
            .collect();
        Some(Outcome {
            state: self.state,
            hints: Hints {
                files,
                ..Hints::default()
            },
            ..Outcome::default()
        })
    }
}

/// A git repository with an index recorded under a wn home.
struct World {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl World {
    fn new(indexed: bool) -> World {
        let w = World {
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(w.repo.path().join(".git")).unwrap();
        fs::create_dir_all(w.repo.path().join("src")).unwrap();
        if indexed {
            w.index(1234);
        }
        w
    }

    fn index(&self, files: usize) {
        let root = self.repo.path().canonicalize().unwrap();
        let dir = wn_daemon::workspace::repo_cache_dir(self.home.path(), &root);
        fs::create_dir_all(dir.join("gemma-xl1-model/index")).unwrap();
        let ev = json!({ "ts": 1, "ms": 1, "files": files, "configs": 0, "history_commits": 0, "model": "gemma-xl1-abc" });
        fs::write(dir.join("usage-index.json"), ev.to_string()).unwrap();
    }

    fn cwd(&self) -> String {
        self.repo.path().join("src").to_string_lossy().into_owned()
    }

    fn deps(&self, asker: Arc<dyn Asker>) -> Deps {
        Deps {
            home: self.home.path().to_path_buf(),
            now: 1_790_000_000,
            budget: Duration::from_millis(1500),
            min_files: 0,
            model: Some("gemma-xl1".into()),
            asker,
        }
    }
}

fn context_of(out: &str) -> String {
    let v: Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out}"));
    v.pointer("/hookSpecificOutput/additionalContext")
        .or_else(|| v.get("additional_context"))
        .and_then(Value::as_str)
        .unwrap()
        .to_string()
}

#[test]
fn every_prompt_gets_new_hints_once_per_session() {
    let w = World::new(true);
    let stub = Stub::new(&["src/upload.rs", "src/retry.rs"]);
    let deps = w.deps(stub.clone());
    let out = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s1", "fix upload retries"),
        &deps,
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let text = context_of(&out);
    assert!(text.starts_with(wn_cli::agents::HOOK_MARKER), "{text}");
    assert!(text.contains("- src/upload.rs\n- src/retry.rs"), "{text}");
    assert!(text.len() < 1000, "≤ ~250 tokens: {text}");

    // Second prompt, same files: nothing new, nothing printed.
    let again = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s1", "and the tests?"),
        &deps,
    );
    assert_eq!(again, "");
    // A new file comes through alone.
    stub.answer.lock().unwrap().push("src/client.rs");
    let third = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s1", "client?"),
        &deps,
    );
    assert!(
        context_of(&third).ends_with(":\n- src/client.rs"),
        "{third}"
    );
    // Another session starts fresh.
    let other = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s2", "fix upload"),
        &deps,
    );
    assert!(context_of(&other).contains("src/upload.rs"));
    // Every prompt was asked (no first-prompt-only rule).
    assert_eq!(stub.asked.lock().unwrap().len(), 4);
}

#[test]
fn at_most_three_paths() {
    let w = World::new(true);
    let stub = Stub::new(&["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
    let out = hooks::run(
        HookKind::CodexPrompt,
        &codex_prompt(&w.cwd()),
        &w.deps(stub),
    );
    assert_eq!(context_of(&out).matches("\n- ").count(), 3, "{out}");
}

#[test]
fn search_hook_asks_the_pattern_with_the_latest_prompt_as_context() {
    let w = World::new(true);
    let stub = Stub::new(&["src/upload.rs"]);
    let deps = w.deps(stub.clone());
    // The prompt hook injects upload.rs.
    let _ = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s1", "fix upload retries"),
        &deps,
    );
    *stub.answer.lock().unwrap() = vec!["src/backoff.rs"];
    // A search with a handful of results: wn stays out of it.
    let few = claude_bash(&w.cwd(), "rg -n RetryPolicy", "a.rs:1:x\nb.rs:2:y");
    assert_eq!(hooks::run(HookKind::ClaudeSearch, &few, &deps), "");
    // Nothing found: hints for the search intent, with the latest prompt from the transcript.
    let transcript = w.home.path().join("transcript.jsonl");
    let lines = [
        json!({ "type": "user", "message": { "role": "user", "content": "an older prompt" } }),
        json!({ "type": "user", "message": { "role": "user", "content": "fix upload retries" } }),
        json!({ "type": "attachment", "attachment": { "type": "hook_additional_context", "content": ["x"] } }),
        json!({ "type": "assistant", "message": { "content": [ { "type": "tool_use", "name": "Bash", "input": { "command": "rg" } } ] } }),
        json!({ "type": "user", "message": { "role": "user", "content": [ { "type": "tool_result", "content": "no matches" } ] } }),
        json!({ "type": "user", "isMeta": true, "message": { "role": "user", "content": "meta" } }),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    fs::write(&transcript, text.join("\n") + "\n").unwrap();
    let mut none: Value = serde_json::from_str(&claude_bash_failure(
        &w.cwd(),
        "rg -n 'RetryPolicy'",
        "Exit code 1\n",
    ))
    .unwrap();
    none["transcript_path"] = json!(transcript);
    let out = hooks::run(HookKind::ClaudeSearch, &none.to_string(), &deps);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        "PostToolUseFailure"
    );
    let text = context_of(&out);
    assert!(
        text.contains("the search `RetryPolicy`") && text.contains("- src/backoff.rs"),
        "{text}"
    );
    let asked = stub.asked.lock().unwrap().clone();
    assert_eq!(
        asked.last().unwrap(),
        &("RetryPolicy".to_string(), "fix upload retries".to_string())
    );
}

#[test]
fn reads_the_latest_prompt_from_claude_and_codex_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("rollout.jsonl");
    let lines = [
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user", "content": [ { "type": "input_text", "text": "<environment_context>cwd</environment_context>" } ] } }),
        json!({ "type": "response_item", "payload": { "type": "message", "role": "user", "content": [ { "type": "input_text", "text": "where is the retry policy" } ] } }),
        json!({ "type": "response_item", "payload": { "type": "function_call", "name": "shell", "arguments": "{}" } }),
    ];
    let text: Vec<String> = lines.iter().map(Value::to_string).collect();
    fs::write(&codex, text.join("\n")).unwrap();
    assert_eq!(
        hooks::latest_prompt(&codex).as_deref(),
        Some("where is the retry policy")
    );
    assert_eq!(
        hooks::latest_prompt(&dir.path().join("missing.jsonl")),
        None
    );
}

#[test]
fn cursor_gets_its_own_output_shape() {
    let w = World::new(true);
    let stub = Stub::new(&["src/upload.rs"]);
    let input = cursor_shell(
        &w.cwd(),
        "rg UploadRetry",
        json!({ "exitCode": 1, "stdout": "" }),
    );
    let out = hooks::run(HookKind::CursorSearch, &input, &w.deps(stub));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("hookSpecificOutput").is_none());
    assert!(v["additional_context"]
        .as_str()
        .unwrap()
        .contains("src/upload.rs"));
}

#[test]
fn silent_when_abstaining_without_model_or_index_outside_git_or_below_the_minimum() {
    // Abstain.
    let w = World::new(true);
    let abstain = Stub::with(&["src/a.rs"], AnswerState::Abstain, Duration::ZERO);
    let input = claude_prompt(&w.cwd(), "s1", "fix upload");
    assert_eq!(
        hooks::run(HookKind::ClaudePrompt, &input, &w.deps(abstain)),
        ""
    );
    // Not indexed: the daemon is never asked.
    let w = World::new(false);
    let stub = Stub::new(&["src/a.rs"]);
    assert_eq!(
        hooks::run(
            HookKind::ClaudePrompt,
            &claude_prompt(&w.cwd(), "s1", "x y z"),
            &w.deps(stub.clone())
        ),
        ""
    );
    assert!(stub.asked.lock().unwrap().is_empty());
    // Not a git repository.
    let plain = tempfile::tempdir().unwrap();
    let out = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(plain.path().to_str().unwrap(), "s1", "x y z"),
        &w.deps(stub.clone()),
    );
    assert_eq!(out, "");
    // No model installed (lexical fallback), or an index built with another model.
    let w = World::new(true);
    let mut deps = w.deps(stub.clone());
    deps.model = None;
    assert_eq!(
        hooks::run(
            HookKind::ClaudePrompt,
            &claude_prompt(&w.cwd(), "s1", "x y z"),
            &deps
        ),
        ""
    );
    deps.model = Some("gemma-g2r".into());
    assert_eq!(
        hooks::run(
            HookKind::ClaudePrompt,
            &claude_prompt(&w.cwd(), "s1", "x y z"),
            &deps
        ),
        ""
    );
    // Below WN_HOOK_MIN_FILES.
    let w = World::new(true);
    let mut deps = w.deps(stub.clone());
    deps.min_files = 5000;
    assert_eq!(
        hooks::run(
            HookKind::ClaudePrompt,
            &claude_prompt(&w.cwd(), "s1", "x y z"),
            &deps
        ),
        ""
    );
    assert!(stub.asked.lock().unwrap().is_empty());
}

#[test]
fn a_slow_answer_is_dropped_at_the_budget_and_logged() {
    let w = World::new(true);
    let slow = Stub::with(&["src/a.rs"], AnswerState::Ok, Duration::from_millis(3000));
    let mut deps = w.deps(slow);
    deps.budget = Duration::from_millis(300);
    let started = Instant::now();
    let out = hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s1", "fix upload"),
        &deps,
    );
    assert_eq!(out, "");
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "{:?}",
        started.elapsed()
    );
    let runs = hooks::load_runs(w.home.path(), 0);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, "timeout");
}

#[test]
fn injections_are_logged_for_stats() {
    let w = World::new(true);
    let deps = w.deps(Stub::new(&["src/upload.rs"]));
    hooks::run(
        HookKind::ClaudePrompt,
        &claude_prompt(&w.cwd(), "s9", "fix upload"),
        &deps,
    );
    let runs = hooks::load_runs(w.home.path(), 0);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (
            runs[0].agent.as_str(),
            runs[0].moment.as_str(),
            runs[0].outcome.as_str(),
            runs[0].session.as_str()
        ),
        ("claude", "prompt", "injected", "s9")
    );
    assert_eq!(runs[0].injected, vec!["src/upload.rs".to_string()]);
    // The injected answer is in the repository's usage log, so transcripts can be matched to it.
    let usage = wn_daemon::usage::load_all(w.home.path(), wn_daemon::usage::now());
    let q = &usage[0].queries[0];
    assert_eq!(
        (q.state.as_str(), q.hinted.clone()),
        ("ok", vec!["src/upload.rs".to_string()])
    );
    assert_eq!(q.files, 1234);
}

#[test]
fn session_state_expires_after_a_day() {
    let home = tempfile::tempdir().unwrap();
    let now = 1_790_000_000;
    let s = hooks::Session {
        injected: vec!["a.rs".into()],
        updated: now,
    };
    hooks::save_session(home.path(), "sess/../x", &s, now);
    assert_eq!(hooks::load_session(home.path(), "sess/../x", now), s);
    assert_eq!(
        hooks::load_session(home.path(), "sess/../x", now + hooks::SESSION_TTL_S * 2),
        hooks::Session::default()
    );
    let files: Vec<_> = fs::read_dir(home.path().join(hooks::SESSIONS_DIR))
        .unwrap()
        .collect();
    assert_eq!(files.len(), 1, "a session id cannot escape the directory");
}

// ---------------------------------------------------------------------------------------------
// Through the binary
// ---------------------------------------------------------------------------------------------

fn hook_bin(
    kind: &str,
    input: &str,
    env: &[(&str, &Path)],
    extra: &[(&str, &str)],
) -> (String, Option<i32>, Duration) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wn"));
    cmd.args(["hook", kind])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("WN_HOOKS")
        .env_remove("WN_NO_DAEMON");
    for (k, v) in env {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    // The first run of a fresh binary can be slow (macOS scans it); time a warm one.
    let _ = Command::new(env!("CARGO_BIN_EXE_wn"))
        .arg("--version")
        .output();
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code(),
        started.elapsed(),
    )
}

#[test]
fn a_daemon_that_never_answers_costs_at_most_the_budget() {
    let w = World::new(true);
    // Something listens on the daemon socket but never replies.
    let socket = wn_cli::daemon::socket_path(w.home.path());
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let held = Arc::new(Mutex::new(Vec::new()));
    let keep = held.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            keep.lock().unwrap().push(s);
        }
    });
    let home = w.home.path().to_path_buf();
    // A model "installed" by name only: the daemon that would load it never answers.
    let model = home.join("gemma-xl1");
    let (out, code, took) = hook_bin(
        "claude-prompt",
        &claude_prompt(&w.cwd(), "s1", "fix upload retries"),
        &[
            ("WHERE_NEXT_HOME", &home),
            ("HOME", &home),
            ("WN_MODEL_DIR", &model),
        ],
        &[],
    );
    assert_eq!((out.as_str(), code), ("", Some(0)));
    assert!(took < Duration::from_millis(2500), "hook took {took:?}");
    assert_eq!(hooks::load_runs(&home, 0)[0].outcome, "timeout");
    let _ = fs::remove_file(socket);
}

#[test]
fn the_kill_switch_and_garbage_input_are_silent() {
    let w = World::new(true);
    let home = w.home.path().to_path_buf();
    let model = home.join("gemma-xl1");
    let env = [
        ("WHERE_NEXT_HOME", home.as_path()),
        ("HOME", home.as_path()),
        ("WN_MODEL_DIR", model.as_path()),
    ];
    let input = claude_prompt(&w.cwd(), "s1", "fix upload retries");
    let (out, code, took) = hook_bin("claude-prompt", &input, &env, &[("WN_HOOKS", "0")]);
    assert_eq!((out.as_str(), code), ("", Some(0)));
    assert!(took < Duration::from_millis(1000));
    assert!(hooks::load_runs(&home, 0).is_empty(), "no daemon was asked");
    for kind in [
        "claude-prompt",
        "claude-search",
        "codex-prompt",
        "codex-search",
        "cursor-search",
    ] {
        let (out, code, _) = hook_bin(kind, "not json", &env, &[]);
        assert_eq!((out.as_str(), code), ("", Some(0)), "{kind}");
    }
    // WN_NO_DAEMON: hooks never answer in-process.
    let (out, code, _) = hook_bin("claude-prompt", &input, &env, &[("WN_NO_DAEMON", "1")]);
    assert_eq!((out.as_str(), code), ("", Some(0)));
}
