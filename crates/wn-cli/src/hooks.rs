//! `wn hook …`: the entry points agents run (installed by `wn setup`).
//!
//! Two moments where navigation matters:
//! - **prompt** (Claude Code and Codex `UserPromptSubmit`): every prompt is asked as a query;
//! - **search** (Claude Code `PostToolUse`/`PostToolUseFailure` on Grep, Glob and Bash; Codex
//!   `PostToolUse` on Bash; Cursor `postToolUse` on Shell and Grep): after a search (`rg`,
//!   `grep`, `find`, `fd`, `git grep`, the Grep/Glob tools) that found nothing or more than
//!   [`SEARCH_MANY`] results, the pattern is asked, with the session's latest prompt (read from
//!   the transcript named in the payload) as context.
//!
//! Confident hints (answer state `ok`) are added to the agent's context: at most [`MAX_HINTS`]
//! paths, each file at most once per session. Every hook fails open: it always exits 0, answers
//! within [`DEFAULT_BUDGET_MS`] or prints nothing, and is silent when wn abstains, no model is
//! installed (the lexical fallback has no calibrated abstain threshold), the directory is not a
//! git repository, the repository is not indexed with the installed model, or `WN_HOOKS=0`.
//!
//! A hook only asks the background daemon (starting it when needed); it never loads a model or
//! builds an index itself.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wn_core::rank::{AnswerState, Outcome};

use crate::agents::HOOK_MARKER;

/// Kill switch: `WN_HOOKS=0` turns every hook into a no-op.
pub const KILL_SWITCH_ENV: &str = "WN_HOOKS";
/// Minimum indexed source files for a hook to answer (default 0: every indexed repository).
pub const MIN_FILES_ENV: &str = "WN_HOOK_MIN_FILES";
/// Time budget of one hook in milliseconds (default [`DEFAULT_BUDGET_MS`]).
pub const BUDGET_ENV: &str = "WN_HOOK_TIMEOUT_MS";
/// Default time budget of one hook.
pub const DEFAULT_BUDGET_MS: u64 = 1500;
/// Most paths one injection carries.
pub const MAX_HINTS: usize = 3;
/// A search with more results than this is "a lot".
pub const SEARCH_MANY: usize = 30;
/// Per-session state (injected file paths) expires after this long.
pub const SESSION_TTL_S: u64 = 86_400;
/// Hook runs kept in the hook log.
const LOG_RETENTION_S: u64 = wn_daemon::usage::RETENTION_DAYS * 86_400;
/// Hook log in `$WHERE_NEXT_HOME`.
pub const HOOK_LOG: &str = "hook-log.jsonl";
/// Per-session state directory in `$WHERE_NEXT_HOME`.
pub const SESSIONS_DIR: &str = "hook-sessions";

/// `wn hook <name>`: which agent and moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Subcommand)]
pub enum HookKind {
    /// Claude Code `UserPromptSubmit`.
    ClaudePrompt,
    /// Claude Code `PostToolUse` / `PostToolUseFailure` on Grep, Glob and Bash.
    ClaudeSearch,
    /// Codex `UserPromptSubmit`.
    CodexPrompt,
    /// Codex `PostToolUse` on Bash.
    CodexSearch,
    /// Cursor `postToolUse` on Shell and Grep.
    CursorSearch,
    /// Claude Code `SessionStart`: starts the daemon and loads the model in the background.
    ClaudeStart,
    /// Codex `SessionStart`: the same warm-up.
    CodexStart,
    /// Cursor `sessionStart`: the same warm-up.
    CursorStart,
}

impl HookKind {
    pub const ALL: [HookKind; 8] = [
        HookKind::ClaudePrompt,
        HookKind::ClaudeSearch,
        HookKind::CodexPrompt,
        HookKind::CodexSearch,
        HookKind::CursorSearch,
        HookKind::ClaudeStart,
        HookKind::CodexStart,
        HookKind::CursorStart,
    ];

    /// The subcommand name (`claude-prompt`, …).
    pub fn name(self) -> &'static str {
        match self {
            HookKind::ClaudePrompt => "claude-prompt",
            HookKind::ClaudeSearch => "claude-search",
            HookKind::CodexPrompt => "codex-prompt",
            HookKind::CodexSearch => "codex-search",
            HookKind::CursorSearch => "cursor-search",
            HookKind::ClaudeStart => "claude-start",
            HookKind::CodexStart => "codex-start",
            HookKind::CursorStart => "cursor-start",
        }
    }

    /// `claude`, `codex` or `cursor`.
    pub fn agent(self) -> &'static str {
        match self {
            HookKind::ClaudePrompt | HookKind::ClaudeSearch | HookKind::ClaudeStart => "claude",
            HookKind::CodexPrompt | HookKind::CodexSearch | HookKind::CodexStart => "codex",
            HookKind::CursorSearch | HookKind::CursorStart => "cursor",
        }
    }

    pub fn is_prompt(self) -> bool {
        matches!(self, HookKind::ClaudePrompt | HookKind::CodexPrompt)
    }

    /// A session-start warm-up (prints nothing, never waits).
    pub fn is_start(self) -> bool {
        matches!(
            self,
            HookKind::ClaudeStart | HookKind::CodexStart | HookKind::CursorStart
        )
    }

    fn default_event(self) -> &'static str {
        match self {
            HookKind::ClaudePrompt | HookKind::CodexPrompt => "UserPromptSubmit",
            HookKind::ClaudeSearch | HookKind::CodexSearch => "PostToolUse",
            HookKind::CursorSearch => "postToolUse",
            HookKind::ClaudeStart | HookKind::CodexStart => "SessionStart",
            HookKind::CursorStart => "sessionStart",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------------------------

/// What a hook payload asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moment {
    /// The user's prompt.
    Prompt(String),
    /// A search and how many results it returned (`None`: unknown).
    Search {
        pattern: String,
        results: Option<usize>,
    },
    /// A session started: warm the daemon up.
    Start,
}

/// A parsed hook payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub session: String,
    pub cwd: PathBuf,
    /// The agent's event name, echoed in the output (`UserPromptSubmit`, `PostToolUse`, …).
    pub event: String,
    pub moment: Moment,
    /// The session transcript, when the agent names one.
    pub transcript: Option<PathBuf>,
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Non-empty lines in a text.
fn lines(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty()).count()
}

/// Result count of a shell command's output: stdout lines, or 0 when a search exited 1 with no
/// output (`rg`/`grep` found nothing).
fn shell_results(stdout: &str, exit: Option<i64>) -> Option<usize> {
    match (exit, lines(stdout)) {
        (Some(1), 0) | (None | Some(0), 0) => Some(0),
        (None | Some(0) | Some(1), n) => Some(n),
        _ => None,
    }
}

/// `Exit code N\n<output>` (Claude Code's Bash failure text).
fn exit_and_output(error: &str) -> (Option<i64>, &str) {
    let (first, rest) = error.split_once('\n').unwrap_or((error, ""));
    let code = first
        .strip_prefix("Exit code ")
        .and_then(|c| c.trim().parse().ok());
    match code {
        Some(c) => (Some(c), rest),
        None => (None, error),
    }
}

fn command_of(input: &Value) -> Option<String> {
    match input.get("command")? {
        Value::String(c) => Some(c.clone()),
        Value::Array(argv) => {
            let parts: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
            match parts.iter().position(|p| *p == "-lc" || *p == "-c") {
                Some(i) if i + 1 < parts.len() => Some(parts[i + 1].to_string()),
                _ => Some(parts.join(" ")),
            }
        }
        _ => None,
    }
}

/// Text output of a tool response that may be a string, `{stdout}`, `{output}` or a JSON string.
fn response_text(v: &Value) -> (Option<i64>, String) {
    match v {
        Value::String(t) => match serde_json::from_str::<Value>(t) {
            Ok(inner @ Value::Object(_)) => response_text(&inner),
            _ => (None, t.clone()),
        },
        Value::Object(_) => {
            let exit = ["exitCode", "exit_code"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_i64));
            let text = ["stdout", "output", "aggregated_output", "formatted_output"]
                .iter()
                .find_map(|k| s(v, k))
                .unwrap_or("")
                .to_string();
            (exit, text)
        }
        _ => (None, String::new()),
    }
}

/// Results of Claude Code's Grep/Glob tools (`numFiles`, `numLines`, `filenames`, `content`).
fn tool_results(resp: &Value) -> Option<usize> {
    if let Some(Value::String(content)) = resp.get("content") {
        if resp.get("mode").and_then(Value::as_str) == Some("content") {
            return Some(lines(content));
        }
    }
    ["numLines", "numFiles", "numMatches"]
        .iter()
        .find_map(|k| resp.get(*k).and_then(Value::as_u64).map(|n| n as usize))
        .or_else(|| {
            resp.get("filenames")
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .or_else(|| resp.as_str().map(lines))
}

fn search_moment(
    tool: &str,
    input: &Value,
    response: Option<&Value>,
    error: Option<&str>,
) -> Option<Moment> {
    match tool {
        "Grep" | "Glob" => {
            let pattern = s(input, "pattern")
                .or_else(|| s(input, "query"))?
                .to_string();
            let results = match (response, error) {
                (Some(r), _) => tool_results(r),
                _ => None,
            };
            Some(Moment::Search { pattern, results })
        }
        "Bash" | "Shell" => {
            let command = command_of(input)?;
            let pattern = search_pattern(&command)?;
            let results = match (response, error) {
                (Some(r), _) => {
                    let (exit, text) = response_text(r);
                    shell_results(&text, exit)
                }
                (None, Some(e)) => {
                    let (exit, out) = exit_and_output(e);
                    shell_results(out, exit)
                }
                (None, None) => None,
            };
            Some(Moment::Search { pattern, results })
        }
        _ => None,
    }
}

/// Parses one hook payload. `None` when it is not something this hook answers.
pub fn parse(kind: HookKind, input: &str) -> Option<Trigger> {
    let v: Value = serde_json::from_str(input).ok()?;
    let session = s(&v, "session_id")
        .or_else(|| s(&v, "conversation_id"))
        .unwrap_or("")
        .to_string();
    let cwd = s(&v, "cwd")
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            v.get("workspace_roots")
                .and_then(Value::as_array)
                .and_then(|r| r.first())
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })?;
    let event = s(&v, "hook_event_name")
        .unwrap_or(kind.default_event())
        .to_string();
    let moment = if kind.is_start() {
        Moment::Start
    } else if kind.is_prompt() {
        let prompt = s(&v, "prompt")?.trim();
        if prompt.is_empty() {
            return None;
        }
        Moment::Prompt(prompt.to_string())
    } else {
        let tool = s(&v, "tool_name")?;
        let input = v.get("tool_input").unwrap_or(&Value::Null);
        let response = v.get("tool_response").or_else(|| v.get("tool_output"));
        let error = s(&v, "error").or_else(|| s(&v, "error_message"));
        search_moment(tool, input, response, error)?
    };
    let transcript = s(&v, "transcript_path")
        .filter(|t| !t.is_empty())
        .map(PathBuf::from);
    Some(Trigger {
        session,
        cwd,
        event,
        moment,
        transcript,
    })
}

/// Bytes read from the end of a transcript to find the latest prompt.
const TRANSCRIPT_TAIL: u64 = 256 * 1024;

/// The text of a user message line in a Claude Code transcript or a Codex rollout, if it is one
/// (tool results, meta lines and injected context are not prompts).
fn user_text(line: &str) -> Option<String> {
    if !line.contains("\"user\"") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let texts = |content: &Value| -> Option<String> {
        match content {
            Value::String(t) => Some(t.clone()),
            Value::Array(blocks) => {
                if blocks.iter().any(|b| s(b, "type") == Some("tool_result")) {
                    return None;
                }
                let t: Vec<&str> = blocks
                    .iter()
                    .filter(|b| matches!(s(b, "type"), Some("text" | "input_text")))
                    .filter_map(|b| s(b, "text"))
                    .collect();
                (!t.is_empty()).then(|| t.join("\n"))
            }
            _ => None,
        }
    };
    let text = if s(&v, "type") == Some("user") {
        if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        texts(v.pointer("/message/content")?)?
    } else {
        let p = v.get("payload")?;
        if s(p, "type") != Some("message") || s(p, "role") != Some("user") {
            return None;
        }
        texts(p.get("content")?)?
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<') && !text.contains(HOOK_MARKER))
        .then(|| truncate(text, 500))
}

/// The latest user prompt in a transcript (read from its last [`TRANSCRIPT_TAIL`] bytes; never
/// stored).
pub fn latest_prompt(transcript: &Path) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL)))
        .ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines().rev().find_map(user_text)
}

// ---------------------------------------------------------------------------------------------
// Shell searches
// ---------------------------------------------------------------------------------------------

/// Splits a shell command into words (quotes and backslashes handled; no expansion).
fn words(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = segment.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Splits a command line on `|`, `&&`, `||`, `;` and newlines outside quotes.
fn segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, '|' | '&' | ';' | '\n') => {
                if !cur.trim().is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Short flags that take a value, per tool family.
const GREP_VALUE_FLAGS: &[&str] = &[
    "-A",
    "-B",
    "-C",
    "-m",
    "-g",
    "-t",
    "-T",
    "-j",
    "-M",
    "-f",
    "-d",
    "-D",
    "--glob",
    "--iglob",
    "--type",
    "--type-not",
    "--max-count",
    "--context",
    "--after-context",
    "--before-context",
    "--max-depth",
    "--threads",
    "--max-columns",
    "--include",
    "--exclude",
    "--file",
    "--encoding",
    "--sort",
    "--sortr",
    "--color",
    "--colors",
    "--path-separator",
];
const FD_VALUE_FLAGS: &[&str] = &[
    "-e",
    "-t",
    "-E",
    "-d",
    "-x",
    "-X",
    "-c",
    "-j",
    "-S",
    "-o",
    "--extension",
    "--type",
    "--exclude",
    "--max-depth",
    "--min-depth",
    "--exec",
    "--exec-batch",
    "--color",
    "--threads",
    "--size",
    "--owner",
    "--changed-within",
    "--changed-before",
    "--max-results",
];
const FIND_PATTERN_FLAGS: &[&str] = &[
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
];
const WRAPPERS: &[&str] = &["sudo", "time", "nice", "command", "exec", "env", "xargs"];

/// The first positional argument after the flags (`-e PATTERN` wins).
fn positional(args: &[String], value_flags: &[&str], explicit: &[&str]) -> Option<String> {
    let mut i = 0;
    let mut first = None;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return first.or_else(|| args.get(i + 1).cloned());
        }
        if explicit.contains(&a.as_str()) {
            return args.get(i + 1).cloned();
        }
        if let Some((flag, value)) = a.split_once('=') {
            if explicit.contains(&flag) {
                return Some(value.to_string());
            }
        }
        if a.starts_with('-') && a.len() > 1 {
            if value_flags.contains(&a.as_str()) {
                i += 1;
            }
        } else if first.is_none() {
            first = Some(a.clone());
        }
        i += 1;
    }
    first
}

/// The pattern of a search command (`rg`, `grep`, `ag`, `ack`, `git grep`, `fd`, `find`), when
/// the command line runs one.
pub fn search_pattern(command: &str) -> Option<String> {
    for segment in segments(command) {
        let mut w = words(&segment);
        // Leading `VAR=value` assignments and wrappers.
        while let Some(first) = w.first() {
            let assignment = first.contains('=') && !first.starts_with('-') && !first.contains('/');
            if assignment || WRAPPERS.contains(&first.as_str()) {
                w.remove(0);
            } else {
                break;
            }
        }
        let Some(program) = w.first() else { continue };
        let name = program.rsplit('/').next().unwrap_or(program).to_string();
        let args = &w[1..];
        let pattern = match name.as_str() {
            "rg" | "grep" | "egrep" | "fgrep" | "ag" | "ack" => {
                positional(args, GREP_VALUE_FLAGS, &["-e", "--regexp"])
            }
            "git" if args.first().map(String::as_str) == Some("grep") => {
                positional(&args[1..], GREP_VALUE_FLAGS, &["-e"])
            }
            "fd" | "fdfind" => positional(args, FD_VALUE_FLAGS, &[]),
            "find" => args
                .iter()
                .position(|a| FIND_PATTERN_FLAGS.contains(&a.as_str()))
                .and_then(|i| args.get(i + 1).cloned()),
            _ => None,
        };
        if let Some(p) = pattern {
            return Some(p);
        }
    }
    None
}

/// Turns a regex or glob into words for a query: `fn\s+parse_(config|args)` → `fn parse config
/// args`. `None` when fewer than 3 letters or digits remain.
pub fn intent(pattern: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // `\s`, `\w`, `\b`, `\d`, … are classes; `\.` is a literal.
            match chars.next() {
                Some(n) if n.is_ascii_alphabetic() => out.push(' '),
                Some(n) => out.push(n),
                None => {}
            }
        } else if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' || c == ':' {
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    let words: Vec<&str> = out
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c == '.' || c == '-' || c == ':'))
        .filter(|w| !w.is_empty())
        .collect();
    let text = words.join(" ");
    (text.chars().filter(|c| c.is_alphanumeric()).count() >= 3).then_some(text)
}

// ---------------------------------------------------------------------------------------------
// Session state (dedupe)
// ---------------------------------------------------------------------------------------------

/// What one session has been shown (file paths only; no prompt or query text).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Files already injected.
    #[serde(default)]
    pub injected: Vec<String>,
    /// Last change (Unix seconds).
    #[serde(default)]
    pub updated: u64,
}

fn session_file(home: &Path, session: &str) -> Option<PathBuf> {
    if session.is_empty() {
        return None;
    }
    let safe: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    Some(home.join(SESSIONS_DIR).join(format!("{safe}.json")))
}

fn mtime_s(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Loads a session's state (empty when missing or older than [`SESSION_TTL_S`]).
pub fn load_session(home: &Path, session: &str, now: u64) -> Session {
    let Some(path) = session_file(home, session) else {
        return Session::default();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Session>(&t).ok())
        .filter(|s| s.updated + SESSION_TTL_S >= now)
        .unwrap_or_default()
}

/// Saves a session's state and removes expired ones.
pub fn save_session(home: &Path, session: &str, state: &Session, now: u64) {
    let Some(path) = session_file(home, session) else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                if mtime_s(&e.path()).is_some_and(|m| m + SESSION_TTL_S < now) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let state = Session {
        updated: now,
        ..state.clone()
    };
    let tmp = path.with_extension("tmp");
    if let Ok(text) = serde_json::to_string(&state) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Log
// ---------------------------------------------------------------------------------------------

/// One hook run that reached the daemon (`$WHERE_NEXT_HOME/hook-log.jsonl`, local only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRun {
    pub ts: u64,
    /// `claude`, `codex` or `cursor`.
    pub agent: String,
    /// `prompt` or `search`.
    pub moment: String,
    pub root: PathBuf,
    pub session: String,
    /// Wall time of the hook, milliseconds.
    pub ms: u64,
    /// `injected`, `quiet` (abstained, or every hint already shown) or `timeout`.
    pub outcome: String,
    /// Files injected.
    #[serde(default)]
    pub injected: Vec<String>,
}

/// Appends a run to the hook log (pruned to the usage retention). Off with `WN_NO_LOG`.
pub fn log_run(home: &Path, run: &HookRun) {
    if !wn_daemon::usage::enabled() {
        return;
    }
    let path = home.join(HOOK_LOG);
    let cutoff = run.ts.saturating_sub(LOG_RETENTION_S);
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = false;
    for l in old.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<HookRun>(l) {
            Ok(r) if r.ts >= cutoff => kept.push(l),
            _ => dropped = true,
        }
    }
    let Ok(line) = serde_json::to_string(run) else {
        return;
    };
    if dropped {
        kept.push(&line);
        let tmp = path.with_extension("jsonl.tmp");
        if std::fs::write(&tmp, kept.join("\n") + "\n").is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    } else {
        use std::io::Write as _;
        let _ = std::fs::create_dir_all(home);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }
}

/// Hook runs logged at or after `since`.
pub fn load_runs(home: &Path, since: u64) -> Vec<HookRun> {
    std::fs::read_to_string(home.join(HOOK_LOG))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<HookRun>(l).ok())
        .filter(|r| r.ts >= since)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Running a hook
// ---------------------------------------------------------------------------------------------

/// Answers a query for a repository (the daemon in production, a stub in tests).
pub trait Asker: Send + Sync + 'static {
    fn ask(&self, root: &Path, query: &str, context: &str) -> Option<Outcome>;
    /// Starts loading the model and index for `root` in the background; returns at once.
    fn warm(&self, root: &Path);
}

/// Asks the background daemon (starting it when needed); never answers in-process.
pub struct DaemonAsker;

impl Asker for DaemonAsker {
    fn ask(&self, root: &Path, query: &str, context: &str) -> Option<Outcome> {
        let root = root.to_str()?;
        let cli = <crate::Cli as clap::Parser>::try_parse_from([
            "wn", "--path", root, "--json", "ask", "--no-log", "--", query,
        ])
        .ok()?;
        if crate::daemon::disabled(&cli) {
            return None;
        }
        let kind = crate::daemon::op_for(&cli.command)?;
        let (json, code) = crate::daemon::client::call(&cli, kind, context)?;
        if code != 0 {
            return None;
        }
        serde_json::from_str(&json).ok()
    }

    fn warm(&self, root: &Path) {
        if std::env::var("WN_NO_DAEMON").is_ok_and(|v| !v.is_empty() && v != "0") {
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        // A detached `wn ask` through the daemon: starts it, loads the model and the index.
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--path")
            .arg(root)
            .args(["ask", "--no-log", "--json", "where is the entry point"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        let _ = cmd.spawn();
    }
}

/// The repository root of `cwd` when it is inside a git repository (walks up to `.git`).
pub fn git_root(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.canonicalize().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The installed model's name (`gemma-xl1`), which prefixes its index directories. `None` without
/// a model: the lexical fallback has no calibrated threshold, so hooks stay silent.
pub fn model_name() -> Option<String> {
    let dir = crate::resolve_model(None)?;
    let name = std::fs::read(dir.join("wn-model.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_string));
    name.or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
}

/// Indexed source files of a repository, when wn has an index for it built with `model`.
pub fn indexed_files(home: &Path, root: &Path, model: &str) -> Option<usize> {
    let dir = wn_daemon::workspace::repo_cache_dir(home, root);
    let prefix = format!("{model}-");
    let has_index = std::fs::read_dir(&dir).ok()?.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with(&prefix) && e.path().join("index").exists()
    });
    if !has_index {
        return None;
    }
    let files = std::fs::read(dir.join(wn_daemon::usage::INDEX_STATS))
        .ok()
        .and_then(|b| serde_json::from_slice::<wn_daemon::usage::IndexEvent>(&b).ok())
        .map_or(0, |e| e.files);
    Some(files)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Whether hooks are switched off (`WN_HOOKS=0`, `false`, `off` or `no`).
pub fn killed() -> bool {
    std::env::var(KILL_SWITCH_ENV).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// The context text for new hints.
pub fn render(moment: &Moment, paths: &[String]) -> String {
    let head = match moment {
        Moment::Prompt(_) | Moment::Start => {
            format!("{HOOK_MARKER} starting with these files for this prompt")
        }
        Moment::Search { pattern, .. } => format!(
            "{HOOK_MARKER} these files for the search `{}`",
            truncate(pattern, 60)
        ),
    };
    let mut lines = vec![format!("{head}; verify before relying on them:")];
    lines.extend(paths.iter().map(|p| format!("- {p}")));
    lines.join("\n")
}

/// The JSON an agent reads to add `text` to its context.
pub fn output(kind: HookKind, event: &str, text: &str) -> String {
    let v = match kind.agent() {
        "cursor" => serde_json::json!({ "additional_context": text }),
        _ => serde_json::json!({
            "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
        }),
    };
    v.to_string()
}

/// Everything a hook run depends on besides its input.
pub struct Deps {
    pub home: PathBuf,
    pub now: u64,
    pub budget: Duration,
    pub min_files: usize,
    /// Name of the installed model (see [`model_name`]); `None` keeps every hook silent.
    pub model: Option<String>,
    pub asker: std::sync::Arc<dyn Asker>,
}

impl Deps {
    /// Production dependencies: `$WHERE_NEXT_HOME`, the daemon, `WN_HOOK_TIMEOUT_MS`,
    /// `WN_HOOK_MIN_FILES`.
    pub fn from_env() -> Deps {
        Deps {
            home: crate::home(),
            now: wn_daemon::usage::now(),
            budget: Duration::from_millis(env_u64(BUDGET_ENV, DEFAULT_BUDGET_MS)),
            min_files: env_u64(MIN_FILES_ENV, 0) as usize,
            model: model_name(),
            asker: std::sync::Arc::new(DaemonAsker),
        }
    }
}

/// Runs one hook: the JSON to print (empty for nothing). Never fails.
pub fn run(kind: HookKind, input: &str, deps: &Deps) -> String {
    let started = Instant::now();
    if killed() {
        return String::new();
    }
    let Some(trigger) = parse(kind, input) else {
        return String::new();
    };
    let Some(root) = git_root(&trigger.cwd) else {
        return String::new();
    };
    let Some(model) = deps.model.as_deref() else {
        return String::new();
    };
    let Some(files) = indexed_files(&deps.home, &root, model) else {
        return String::new();
    };
    if files < deps.min_files {
        return String::new();
    }
    if trigger.moment == Moment::Start {
        deps.asker.warm(&root);
        return String::new();
    }
    let mut session = load_session(&deps.home, &trigger.session, deps.now);
    let (query, context) = match &trigger.moment {
        Moment::Prompt(p) => (p.clone(), String::new()),
        Moment::Start => return String::new(),
        Moment::Search { pattern, results } => {
            let wanted = results.is_some_and(|n| n == 0 || n > SEARCH_MANY);
            let Some(words) = intent(pattern).filter(|_| wanted) else {
                return String::new();
            };
            let prompt = trigger.transcript.as_deref().and_then(latest_prompt);
            (words, prompt.unwrap_or_default())
        }
    };

    let (tx, rx) = mpsc::channel();
    let asker = deps.asker.clone();
    let ask_root = root.clone();
    let (q, c) = (query.clone(), context.clone());
    std::thread::spawn(move || {
        let _ = tx.send(asker.ask(&ask_root, &q, &c));
    });
    let left = deps.budget.saturating_sub(started.elapsed());
    let answer = rx.recv_timeout(left);
    let moment_name = if kind.is_prompt() { "prompt" } else { "search" };
    let mut run = HookRun {
        ts: deps.now,
        agent: kind.agent().into(),
        moment: moment_name.into(),
        root: root.clone(),
        session: trigger.session.clone(),
        ms: 0,
        outcome: "quiet".into(),
        injected: Vec::new(),
    };
    let outcome = match answer {
        Ok(Some(o)) => o,
        Ok(None) => return String::new(),
        Err(_) => {
            run.outcome = "timeout".into();
            run.ms = started.elapsed().as_millis() as u64;
            log_run(&deps.home, &run);
            return String::new();
        }
    };
    let fresh: Vec<String> = if outcome.state == AnswerState::Ok {
        outcome
            .hints
            .files
            .iter()
            .map(|h| h.path.clone())
            .filter(|p| p.chars().count() <= 200 && !session.injected.contains(p))
            .take(MAX_HINTS)
            .collect()
    } else {
        Vec::new()
    };
    run.ms = started.elapsed().as_millis() as u64;
    if fresh.is_empty() {
        log_run(&deps.home, &run);
        return String::new();
    }
    session.injected.extend(fresh.iter().cloned());
    save_session(&deps.home, &trigger.session, &session, deps.now);
    run.outcome = "injected".into();
    run.injected = fresh.clone();
    log_run(&deps.home, &run);
    record_answer(
        &deps.home, &root, &query, &context, &outcome, &fresh, run.ms, files,
    );
    output(kind, &trigger.event, &render(&trigger.moment, &fresh))
}

/// Logs the injected answer in the repository's usage log, like a `wn ask`, so `wn stats` can
/// match it to what the agent did next.
#[allow(clippy::too_many_arguments)]
fn record_answer(
    home: &Path,
    root: &Path,
    query: &str,
    context: &str,
    outcome: &Outcome,
    injected: &[String],
    ms: u64,
    files: usize,
) {
    let dir = wn_daemon::workspace::repo_cache_dir(home, root);
    let model = std::fs::read(dir.join(wn_daemon::usage::INDEX_STATS))
        .ok()
        .and_then(|b| serde_json::from_slice::<wn_daemon::usage::IndexEvent>(&b).ok())
        .map(|e| e.model)
        .unwrap_or_default();
    let event = wn_daemon::usage::QueryEvent {
        ts: wn_daemon::usage::now(),
        kind: wn_core::rank::QueryKind::classify(query, context)
            .as_str()
            .to_string(),
        state: "ok".into(),
        ms,
        model,
        adapter: outcome.adapter.applied,
        files,
        hinted: injected.to_vec(),
    };
    let _ = wn_daemon::usage::record_query(&dir, root, &event);
}
