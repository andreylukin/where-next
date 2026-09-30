//! Did the agent act on wn's hints? Reads Claude Code and Codex transcripts **locally and
//! read-only** to find each `wn ask` (or where-next MCP call, or hook injection), then looks at the
//! agent's next [`FOLLOW_CALLS`] tool calls, up to the next user turn, for the files it touched:
//! read (Read tool, `cat`, `rg`, …), ran (`pytest`, `cargo test`, `python`, …) or edited (Edit /
//! Write tools, `sed -i`, redirects, patches).
//!
//! Each answer is then [`Similarity::Exact`] (the agent touched a hinted file),
//! [`Similarity::Near`] (same directory as a hint, or its test ↔ source pair),
//! [`Similarity::Elsewhere`] (it touched other files) or [`Similarity::None`] (no files).
//!
//! Transcripts never leave the machine and nothing from them is stored: `wn stats` reduces them
//! to counts. `wn report` does not read them.
//!
//! Each transcript is walked by a small state machine ([`TRANSITIONS`]):
//! Seeking → (wn call) → Following → (N tool calls, user turn, next call, end) → Seeking, with
//! Armed for hook injections, which arrive with the prompt or a tool result, before the next
//! tool call.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use wn_core::machine::{step, Illegal};
use wn_daemon::usage::{QueryEvent, RepoUsage};

/// Tool calls after a wn answer that are checked for touched files.
pub const FOLLOW_CALLS: usize = 10;
/// A logged answer matches a call when it was recorded this long before the call…
pub const MATCH_BEFORE_S: u64 = 5;
/// …or up to this long after it (a cold daemon can take a while on the first answer).
pub const MATCH_AFTER_S: u64 = 600;
/// Opt-out: skip reading agent transcripts.
pub const OPT_OUT_ENV: &str = "WN_STATS_NO_AGENTS";
/// The first words of every hook injection (see [`crate::hooks::render`]).
pub const HOOK_MARKER: &str = "where-next (local index of this repository) suggests";

// ---------------------------------------------------------------------------------------------
// The per-transcript state machine
// ---------------------------------------------------------------------------------------------

/// Where the walk over one transcript is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackState {
    /// Looking for a wn call.
    Seeking,
    /// A hook injected hints; waiting for the agent's next tool call.
    Armed,
    /// Watching the tool calls after a wn answer.
    Following,
    /// End of the transcript.
    Done,
}

/// What the walk sees next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackEvent {
    /// A `wn ask` / where-next MCP call.
    Call,
    /// Hints injected by a hook (see `wn setup`).
    Hook,
    /// Any other tool call.
    Tool,
    /// The user spoke.
    UserTurn,
    /// [`FOLLOW_CALLS`] tool calls were seen.
    Limit,
    /// End of the transcript.
    Eof,
}

/// All states, for exhaustive tests.
pub const TRACK_STATES: [TrackState; 4] = [
    TrackState::Seeking,
    TrackState::Armed,
    TrackState::Following,
    TrackState::Done,
];

/// All events, for exhaustive tests.
pub const TRACK_EVENTS: [TrackEvent; 6] = [
    TrackEvent::Call,
    TrackEvent::Hook,
    TrackEvent::Tool,
    TrackEvent::UserTurn,
    TrackEvent::Limit,
    TrackEvent::Eof,
];

/// Legal transitions. Leaving Following (or Armed) emits the observation collected so far.
pub const TRANSITIONS: &[(TrackState, TrackEvent, TrackState)] = {
    use TrackEvent as E;
    use TrackState as S;
    &[
        (S::Seeking, E::Call, S::Following),
        (S::Seeking, E::Hook, S::Armed),
        (S::Seeking, E::Tool, S::Seeking),
        (S::Seeking, E::UserTurn, S::Seeking),
        (S::Seeking, E::Eof, S::Done),
        (S::Armed, E::Call, S::Following),
        (S::Armed, E::Hook, S::Armed),
        (S::Armed, E::Tool, S::Following),
        (S::Armed, E::UserTurn, S::Armed),
        (S::Armed, E::Eof, S::Done),
        (S::Following, E::Call, S::Following),
        (S::Following, E::Hook, S::Armed),
        (S::Following, E::Tool, S::Following),
        (S::Following, E::UserTurn, S::Seeking),
        (S::Following, E::Limit, S::Seeking),
        (S::Following, E::Eof, S::Done),
    ]
};

/// Which agent wrote a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
}

impl Agent {
    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

/// How the agent got the hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// `wn ask` from a shell tool.
    Cli,
    /// The where-next MCP tool.
    Mcp,
    /// A where-next hook (prompt or search; see `wn setup`).
    Hook,
}

/// A tool call after a wn answer, reduced to the files it names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Followed {
    /// Paths a read tool opened (Claude `Read`, `Grep` on a file).
    pub reads: Vec<String>,
    /// Paths an edit tool wrote (Claude `Edit`/`Write`, patch headers).
    pub edits: Vec<String>,
    /// Shell commands it ran.
    pub shell: Vec<String>,
}

/// One transcript line, reduced to what the walk needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A wn call at `ts` (Unix seconds).
    Call { ts: u64, source: Source },
    /// Any other tool call.
    Tool(Followed),
    /// The user spoke.
    UserTurn,
}

/// A wn answer and the tool calls that followed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub agent: Agent,
    pub session: String,
    pub ts: u64,
    pub source: Source,
    pub calls: Vec<Followed>,
}

pub struct Tracker {
    state: TrackState,
    agent: Agent,
    session: String,
    open: Option<Observation>,
    out: Vec<Observation>,
}

impl Tracker {
    pub fn new(agent: Agent, session: impl Into<String>) -> Tracker {
        Tracker {
            state: TrackState::Seeking,
            agent,
            session: session.into(),
            open: None,
            out: Vec::new(),
        }
    }

    pub fn state(&self) -> TrackState {
        self.state
    }

    /// Whether the next line matters even when it is not a wn call (tool calls, user turns).
    pub fn wants_everything(&self) -> bool {
        matches!(self.state, TrackState::Armed | TrackState::Following)
    }

    fn fire(&mut self, event: TrackEvent) -> Result<TrackState, Illegal<TrackState, TrackEvent>> {
        use TrackEvent as E;
        use TrackState as S;
        let next = step(TRANSITIONS, self.state, event)?;
        let emit = matches!(
            (self.state, event),
            (
                S::Following,
                E::Call | E::Hook | E::UserTurn | E::Limit | E::Eof
            ) | (S::Armed, E::Call | E::Hook | E::Eof)
        );
        if emit {
            if let Some(o) = self.open.take() {
                self.out.push(o);
            }
        }
        self.state = next;
        Ok(next)
    }

    /// Feeds one item.
    pub fn feed(&mut self, item: Item) -> Result<(), Illegal<TrackState, TrackEvent>> {
        match item {
            Item::Call { ts, source } => {
                let event = if source == Source::Hook {
                    TrackEvent::Hook
                } else {
                    TrackEvent::Call
                };
                self.fire(event)?;
                self.open = Some(Observation {
                    agent: self.agent,
                    session: self.session.clone(),
                    ts,
                    source,
                    calls: Vec::new(),
                });
            }
            Item::Tool(followed) => {
                self.fire(TrackEvent::Tool)?;
                if let Some(o) = self.open.as_mut() {
                    o.calls.push(followed);
                    if o.calls.len() >= FOLLOW_CALLS {
                        self.fire(TrackEvent::Limit)?;
                    }
                }
            }
            Item::UserTurn => {
                self.fire(TrackEvent::UserTurn)?;
            }
        }
        Ok(())
    }

    /// Ends the transcript and returns every observation.
    pub fn finish(mut self) -> Vec<Observation> {
        let _ = self.fire(TrackEvent::Eof);
        self.out
    }
}

// ---------------------------------------------------------------------------------------------
// Parsing transcript lines
// ---------------------------------------------------------------------------------------------

fn wn_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?:^|[\s;&|(`'"])(?:[\w./~-]*/)?wn(?:\s+-{1,2}[A-Za-z][\w-]*(?:=\S+|\s+[^\s-]\S*)?)*\s+ask(?:\s|$|\\)"#,
        )
        .expect("valid regex")
    })
}

fn patch_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\*\*\* (?:Update File|Add File|Delete File|Move to): ([^\n\\"]+)"#)
            .expect("valid regex")
    })
}

fn js_cmd_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"cmd"?\s*:\s*"((?:[^"\\]|\\.)*)""#).expect("valid regex"))
}

/// Whether a shell command runs `wn ask`.
pub fn is_wn_ask(command: &str) -> bool {
    wn_call_re().is_match(command)
}

fn is_mcp_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("where_next") || n.contains("where-next")
}

/// Parses an RFC 3339 UTC timestamp (`2026-09-29T20:53:13.725Z`) to Unix seconds.
pub fn parse_ts(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

fn line_ts(v: &Value) -> u64 {
    v.get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or(0)
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn patch_paths(text: &str) -> Vec<String> {
    patch_file_re()
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .collect()
}

/// The script of a `["bash", "-lc", script]` style argv, else the argv joined.
fn argv_command(argv: &[Value]) -> String {
    let parts: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    match parts.iter().position(|p| *p == "-lc" || *p == "-c") {
        Some(i) if i + 1 < parts.len() => parts[i + 1].to_string(),
        _ => parts.join(" "),
    }
}

fn unescape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

const CLAUDE_EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

fn claude_tool(name: &str, input: &Value) -> Followed {
    let mut f = Followed::default();
    match name {
        "Read" => f
            .reads
            .extend(str_field(input, "file_path").map(str::to_string)),
        "Grep" => f.reads.extend(str_field(input, "path").map(str::to_string)),
        "Bash" => f
            .shell
            .extend(str_field(input, "command").map(str::to_string)),
        n if CLAUDE_EDIT_TOOLS.contains(&n) => f.edits.extend(
            ["file_path", "notebook_path"]
                .iter()
                .filter_map(|k| str_field(input, k).map(str::to_string)),
        ),
        _ => {}
    }
    f
}

/// Items in one Claude Code transcript line. With `everything` false only wn calls and start
/// hints are returned (and lines that cannot hold one are not parsed at all).
pub fn claude_items(line: &str, everything: bool) -> Vec<Item> {
    let tool_use = line.contains(r#""tool_use""#);
    let maybe_call = tool_use
        && (line.contains("wn") || line.contains("where_next") || line.contains("where-next"));
    let maybe_hook = line.contains(HOOK_MARKER) && line.contains(r#""type":"attachment""#);
    let maybe_user = everything && line.contains(r#""type":"user""#);
    if !(maybe_call || maybe_hook || maybe_user || (everything && tool_use)) {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let ts = line_ts(&v);
    match str_field(&v, "type") {
        // The context itself (`hook_additional_context`); `hook_success` repeats the hook's raw
        // stdout for the same injection and is not counted twice.
        Some("attachment")
            if maybe_hook
                && v.pointer("/attachment/type").and_then(Value::as_str)
                    != Some("hook_success") =>
        {
            vec![Item::Call {
                ts,
                source: Source::Hook,
            }]
        }
        Some("assistant") => v
            .pointer("/message/content")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|b| str_field(b, "type") == Some("tool_use"))
            .filter_map(|b| {
                let name = str_field(b, "name").unwrap_or("");
                let input = b.get("input").unwrap_or(&Value::Null);
                if is_mcp_name(name) {
                    return Some(Item::Call {
                        ts,
                        source: Source::Mcp,
                    });
                }
                if name == "Bash" && is_wn_ask(str_field(input, "command").unwrap_or("")) {
                    return Some(Item::Call {
                        ts,
                        source: Source::Cli,
                    });
                }
                everything.then(|| Item::Tool(claude_tool(name, input)))
            })
            .collect(),
        Some("user") if everything => {
            if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                return Vec::new();
            }
            let spoke = match v.pointer("/message/content") {
                Some(Value::String(s)) => !s.trim().is_empty(),
                Some(Value::Array(blocks)) => {
                    let kind = |t: &str| blocks.iter().any(|b| str_field(b, "type") == Some(t));
                    kind("text") && !kind("tool_result")
                }
                _ => false,
            };
            if spoke {
                vec![Item::UserTurn]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn codex_tool(p: &Value) -> Followed {
    let mut f = Followed::default();
    let name = str_field(p, "name").unwrap_or("");
    if let Some(input) = str_field(p, "input") {
        // Custom tools: `exec` (a JS cell calling tools.exec_command / tools.apply_patch) or a
        // bare `apply_patch`.
        f.edits.extend(patch_paths(input));
        if name != "apply_patch" {
            f.shell.extend(
                js_cmd_re()
                    .captures_iter(input)
                    .filter_map(|c| c.get(1).map(|m| unescape_js(m.as_str()))),
            );
        }
    }
    let args = str_field(p, "arguments").and_then(|a| serde_json::from_str::<Value>(a).ok());
    for v in args.iter().chain(p.get("action")) {
        match v.get("command") {
            Some(Value::Array(argv)) => f.shell.push(argv_command(argv)),
            Some(Value::String(s)) => f.shell.push(s.clone()),
            _ => {}
        }
        if let Some(cmd) = str_field(v, "cmd") {
            f.shell.push(cmd.to_string());
        }
        if let Some(patch) = str_field(v, "input").or_else(|| str_field(v, "patch")) {
            f.edits.extend(patch_paths(patch));
        }
    }
    f
}

/// Items in one Codex rollout line (same contract as [`claude_items`]).
pub fn codex_items(line: &str, everything: bool) -> Vec<Item> {
    let call = line.contains(r#""response_item""#)
        && (line.contains(r#""custom_tool_call""#)
            || line.contains(r#""function_call""#)
            || line.contains(r#""local_shell_call""#));
    let maybe_call =
        call && (line.contains("wn") || line.contains("where_next") || line.contains("where-next"));
    let maybe_user = everything && line.contains(r#""role":"user""#);
    // Hook context (`additionalContext`) arrives as a developer or user message.
    let maybe_hook = line.contains(HOOK_MARKER) && line.contains(r#""message""#);
    if !(maybe_call || maybe_user || maybe_hook || (everything && call)) {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let ts = line_ts(&v);
    let Some(p) = v.get("payload") else {
        return Vec::new();
    };
    match str_field(p, "type") {
        Some("custom_tool_call" | "function_call" | "local_shell_call") => {
            if is_mcp_name(str_field(p, "name").unwrap_or("")) {
                return vec![Item::Call {
                    ts,
                    source: Source::Mcp,
                }];
            }
            let tool = codex_tool(p);
            if tool.shell.iter().any(|c| is_wn_ask(c)) {
                return vec![Item::Call {
                    ts,
                    source: Source::Cli,
                }];
            }
            if everything {
                vec![Item::Tool(tool)]
            } else {
                Vec::new()
            }
        }
        Some("message")
            if maybe_hook && matches!(str_field(p, "role"), Some("developer" | "user")) =>
        {
            vec![Item::Call {
                ts,
                source: Source::Hook,
            }]
        }
        Some("message") if everything && str_field(p, "role") == Some("user") => {
            vec![Item::UserTurn]
        }
        _ => Vec::new(),
    }
}

/// Walks one transcript's text.
pub fn observe(agent: Agent, session: &str, text: &str) -> Vec<Observation> {
    let mut t = Tracker::new(agent, session);
    for line in text.lines() {
        let items = match agent {
            Agent::ClaudeCode => claude_items(line, t.wants_everything()),
            Agent::Codex => codex_items(line, t.wants_everything()),
        };
        for item in items {
            let _ = t.feed(item);
        }
    }
    t.finish()
}
// ---------------------------------------------------------------------------------------------
// Finding transcripts
// ---------------------------------------------------------------------------------------------

/// Where transcripts live.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    /// Claude Code projects directory (`$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`).
    pub claude: Option<PathBuf>,
    /// Codex sessions directory (`$CODEX_HOME/sessions`, else `~/.codex/sessions`).
    pub codex: Option<PathBuf>,
}

impl Roots {
    /// The default locations for this user.
    pub fn detect() -> Roots {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".claude")))
            .map(|d| d.join("projects"));
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".codex")))
            .map(|d| d.join("sessions"));
        Roots { claude, codex }
    }
}

fn jsonl_files(dir: &Path, depth: usize, since: u64, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let path = e.path();
        if meta.is_dir() {
            if depth > 0 {
                jsonl_files(&path, depth - 1, since, out);
            }
            continue;
        }
        if path.extension().map_or(true, |x| x != "jsonl") {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if mtime >= since {
            out.push((path, meta.len()));
        }
    }
}

/// Transcripts modified at or after `since`, with their agent.
pub fn transcripts(roots: &Roots, since: u64) -> Vec<(Agent, PathBuf)> {
    let mut out = Vec::new();
    for (agent, dir, depth) in [
        (Agent::ClaudeCode, &roots.claude, 5),
        (Agent::Codex, &roots.codex, 4),
    ] {
        let Some(dir) = dir else { continue };
        let mut files = Vec::new();
        jsonl_files(dir, depth, since, &mut files);
        // Biggest first, so the parallel scan finishes evenly.
        files.sort_by_key(|f| std::cmp::Reverse(f.1));
        out.extend(files.into_iter().map(|(p, _)| (agent, p)));
    }
    out
}

/// Reads and walks transcripts in parallel. Only files containing a possible wn call are parsed.
pub fn scan(files: &[(Agent, PathBuf)], since: u64) -> Vec<Observation> {
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(16);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut all: Vec<Observation> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut found = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((agent, path)) = files.get(i) else {
                            break;
                        };
                        found.extend(scan_file(*agent, path));
                    }
                    found
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    });
    all.retain(|o| o.ts >= since);
    all.sort_by(|a, b| (a.ts, &a.session).cmp(&(b.ts, &b.session)));
    all
}

fn scan_file(agent: Agent, path: &Path) -> Vec<Observation> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let possible = [b"wn".as_slice(), b"where_next", b"where-next"]
        .iter()
        .any(|n| memchr::memmem::find(&bytes, n).is_some());
    if !possible {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let session = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    observe(agent, &session, &text)
}

// ---------------------------------------------------------------------------------------------
// What the agent touched
// ---------------------------------------------------------------------------------------------

/// What the agent did with a file (ordered weakest to strongest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Opened or searched it (`Read`, `cat`, `sed -n`, `rg`, `git diff`, …).
    Read,
    /// Ran or tested it (`pytest`, `cargo test`, `python`, `node`, …).
    Ran,
    /// Changed it (`Edit`/`Write`, `sed -i`, `>` redirects, `tee`, patches).
    Edited,
}

/// Commands whose file arguments are run or tested.
const RUN_COMMANDS: &[&str] = &[
    "pytest", "py.test", "python", "python3", "node", "deno", "bun", "ruby", "php", "bash", "sh",
    "zsh", "go", "cargo", "npm", "pnpm", "yarn", "jest", "vitest", "mocha", "rspec", "tox", "nox",
    "make", "just", "java", "swift", "dotnet", "mvn", "gradle", "gradlew", "tsx", "ts-node",
    "phpunit", "lua", "mix", "elixir", "dart", "flutter", "Rscript", "julia",
];
/// Words that only wrap the real command.
const WRAPPERS: &[&str] = &[
    "sudo", "time", "env", "nice", "command", "exec", "xargs", "npx", "bunx",
];
/// Tools whose `run`/`exec` subcommand wraps the real command (`uv run pytest …`).
const RUNNERS: &[&str] = &[
    "uv", "poetry", "pipenv", "pdm", "hatch", "rye", "pnpm", "yarn", "npm",
];

fn segments(cmd: &str) -> Vec<String> {
    cmd.replace("&&", "\n")
        .replace("||", "\n")
        .split(['\n', ';', '|', '&'])
        .map(str::to_string)
        .collect()
}

fn words(segment: &str) -> Vec<String> {
    let spaced: String = segment
        .chars()
        .flat_map(|c| match c {
            '"' | '\'' | '`' | '(' | ')' | '{' | '}' => vec![' '],
            '>' | '<' => vec![' ', c, ' '],
            c => vec![c],
        })
        .collect();
    // Re-join `> >` into `>>`.
    let mut out: Vec<String> = Vec::new();
    for w in spaced.split_whitespace() {
        if w == ">" && out.last().is_some_and(|l| l == ">") {
            continue;
        }
        out.push(w.to_string());
    }
    out
}

/// A command-line token as a candidate path, or `None` (flags, URLs, globs, numbers).
pub fn clean_token(w: &str) -> Option<String> {
    let w = w.trim_end_matches([',', ';', ')', ':']);
    if w.is_empty() || w.contains("://") || w.contains(['*', '?', '[', ']', '$']) {
        return None;
    }
    let w = w.split("::").next().unwrap_or(w);
    let w = w.split('#').next().unwrap_or(w);
    // `path:12` or `path:12:3`.
    let w = match w.split_once(':') {
        Some((p, rest))
            if rest
                .split(':')
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) =>
        {
            p
        }
        _ => w,
    };
    let w = w.strip_prefix("./").unwrap_or(w);
    if w.is_empty() || w == "." || w == ".." || w.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(w.to_string())
}

fn join_base(base: &Path, p: &str) -> PathBuf {
    let p = match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    };
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// Files a shell command names, with what it does to them. Paths are relative to the directory
/// the command started in unless absolute; `cd` inside the command is followed.
pub fn shell_touches(cmd: &str) -> Vec<(PathBuf, Action)> {
    let mut base = PathBuf::new();
    let mut out = Vec::new();
    for seg in segments(cmd) {
        let w = words(&seg);
        let mut i = 0;
        while let Some(word) = w.get(i) {
            let ident = word.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
            let runner = RUNNERS.contains(&word.as_str())
                && matches!(w.get(i + 1).map(String::as_str), Some("run" | "exec"));
            if ident || WRAPPERS.contains(&word.as_str()) {
                i += 1;
            } else if word == "timeout" || runner {
                // `timeout 30 cmd`, `uv run cmd`.
                i += 2;
            } else {
                break;
            }
        }
        let Some(first) = w.get(i) else { continue };
        let name = first.rsplit('/').next().unwrap_or(first).to_string();
        if name == "cd" {
            if let Some(dir) = w.get(i + 1) {
                base = join_base(&base, dir);
            }
            continue;
        }
        let args = &w[i + 1..];
        let edits_in_place = match name.as_str() {
            "sed" => args
                .iter()
                .any(|a| a.starts_with("-i") || a == "--in-place"),
            "perl" => args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('i')),
            "tee" | "touch" | "truncate" => true,
            _ => false,
        };
        let default = if edits_in_place {
            Action::Edited
        } else if RUN_COMMANDS.contains(&name.as_str()) {
            Action::Ran
        } else {
            Action::Read
        };
        if first.contains('/') {
            // `./script.py`, `bin/run`: running a file directly.
            if let Some(p) = clean_token(first) {
                out.push((join_base(&base, &p), Action::Ran));
            }
        }
        let mut j = 0;
        while let Some(a) = args.get(j) {
            j += 1;
            let (token, action) = match a.as_str() {
                ">" => match args.get(j) {
                    Some(t) => {
                        j += 1;
                        (t.as_str(), Action::Edited)
                    }
                    None => break,
                },
                "<" => match args.get(j) {
                    Some(t) => {
                        j += 1;
                        (t.as_str(), Action::Read)
                    }
                    None => break,
                },
                flag if flag.starts_with('-') => match flag.split_once('=') {
                    Some((_, v)) => (v, default),
                    None => continue,
                },
                t => (t, default),
            };
            if let Some(p) = clean_token(token) {
                out.push((join_base(&base, &p), action));
            }
        }
    }
    out
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// A touched path relative to `root`, when it is inside it. Relative paths are taken from the
/// root (agents run from the repository), after any `cd` in the same command.
pub fn relative_to(root: &Path, p: &Path) -> Option<String> {
    let full = normalize(&if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    });
    // macOS: /tmp is /private/tmp, and either spelling can appear on either side.
    let alt = match root.strip_prefix("/private") {
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => Path::new("/private").join(root.strip_prefix("/").unwrap_or(root)),
    };
    let rel = full
        .strip_prefix(root)
        .or_else(|_| full.strip_prefix(&alt))
        .ok()?;
    let s = rel.to_string_lossy().into_owned();
    (!s.is_empty()).then_some(s)
}

/// Files touched in the calls after an answer, in first-touch order, each with the strongest
/// action seen. Paths inside `root` are relative; files outside it (another repository, a
/// scratch file) are kept as absolute paths so the answer still counts as "elsewhere". `exists`
/// checks a path (files named in a shell command must exist to count; hinted files always
/// count, even if since deleted).
pub fn touched(
    calls: &[Followed],
    root: &Path,
    hinted: &[String],
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<(String, Action)> {
    let mut order: Vec<(String, Action)> = Vec::new();
    let mut add = |key: String, action: Action| match order.iter_mut().find(|(p, _)| *p == key) {
        Some((_, a)) => *a = (*a).max(action),
        None => order.push((key, action)),
    };
    let key = |p: &Path| -> Option<String> {
        relative_to(root, p).or_else(|| {
            p.is_absolute()
                .then(|| normalize(p).to_string_lossy().into_owned())
        })
    };
    for c in calls {
        for p in &c.reads {
            if let Some(k) = key(Path::new(p)) {
                add(k, Action::Read);
            }
        }
        for cmd in &c.shell {
            for (p, action) in shell_touches(cmd) {
                let Some(k) = key(&p) else { continue };
                let full = if k.starts_with('/') {
                    PathBuf::from(&k)
                } else {
                    root.join(&k)
                };
                if hinted.contains(&k) || exists(&full) {
                    add(k, action);
                }
            }
        }
        for p in &c.edits {
            if let Some(k) = key(Path::new(p)) {
                add(k, Action::Edited);
            }
        }
    }
    order
}

/// How close what the agent touched came to the hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Similarity {
    /// It touched a hinted file.
    Exact,
    /// It touched a file in a hint's directory, or a hint's test ↔ source pair.
    Near,
    /// It touched files, none of them close.
    Elsewhere,
    /// It touched no files before moving on.
    None,
}

fn stem_of(path: &str) -> (String, bool) {
    let p = Path::new(path);
    let file = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut stem = file.split('.').next().unwrap_or("").to_string();
    let mut test = false;
    for marker in [".test.", ".spec."] {
        test |= file.contains(marker);
    }
    for prefix in ["test_"] {
        if let Some(s) = stem.strip_prefix(prefix) {
            stem = s.to_string();
            test = true;
        }
    }
    for suffix in ["_test", "_spec", "Tests", "Test"] {
        if let Some(s) = stem.strip_suffix(suffix) {
            if !s.is_empty() {
                stem = s.to_string();
                test = true;
                break;
            }
        }
    }
    test |= p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("tests" | "test" | "__tests__" | "spec")
        )
    });
    (stem, test)
}

/// Whether `a` is near hint `h`: same (non-root) directory, or a test ↔ source pair.
pub fn is_near(a: &str, h: &str) -> bool {
    if a == h {
        return true;
    }
    let dir = |p: &str| {
        Path::new(p)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    };
    if !dir(a).as_os_str().is_empty() && dir(a) == dir(h) {
        return true;
    }
    let (sa, ta) = stem_of(a);
    let (sh, th) = stem_of(h);
    !sa.is_empty() && sa == sh && ta != th
}

/// Scores one answer: its similarity, the strongest action on a hinted file, and the rank of
/// the first hinted file touched.
pub fn classify(
    touched: &[(String, Action)],
    hinted: &[String],
) -> (Similarity, Option<Action>, Option<usize>) {
    let exact: Vec<&(String, Action)> =
        touched.iter().filter(|(p, _)| hinted.contains(p)).collect();
    if let Some((first, _)) = exact.first() {
        let rank = hinted.iter().position(|h| h == first).map(|i| i + 1);
        let action = exact.iter().map(|(_, a)| *a).max();
        return (Similarity::Exact, action, rank);
    }
    let inside = |p: &String| !p.starts_with('/');
    let similarity = if touched
        .iter()
        .any(|(p, _)| inside(p) && hinted.iter().any(|h| is_near(p, h)))
    {
        Similarity::Near
    } else if touched.is_empty() {
        Similarity::None
    } else {
        Similarity::Elsewhere
    };
    (similarity, None, None)
}

/// Agent counts for one repository. `exact + near + elsewhere + none == answered` and
/// `exact_by` sums to `exact`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentCounts {
    /// Calls matched to a logged answer in this repository.
    pub calls: usize,
    /// Of those, answers with hints (not abstained).
    pub answered: usize,
    /// Answers per similarity.
    pub similarity: BTreeMap<Similarity, usize>,
    /// Exact answers per strongest action on a hinted file.
    pub exact_by: BTreeMap<Action, usize>,
    /// Exact answers whose first touched hint was ranked #1.
    pub first_pick: usize,
    /// Sessions with at least one matched call.
    pub sessions: BTreeSet<(Agent, String)>,
    /// Of `answered`, hints injected by a hook…
    pub hook_answered: usize,
    /// …and of those, answers after which the agent touched a hinted file.
    pub hook_exact: usize,
}

impl AgentCounts {
    /// Answers with this similarity.
    pub fn count(&self, s: Similarity) -> usize {
        self.similarity.get(&s).copied().unwrap_or(0)
    }

    /// Exact answers with this strongest action.
    pub fn exact_action(&self, a: Action) -> usize {
        self.exact_by.get(&a).copied().unwrap_or(0)
    }

    /// Adds another repository's counts.
    pub fn add(&mut self, o: &AgentCounts) {
        self.calls += o.calls;
        self.answered += o.answered;
        for (k, v) in &o.similarity {
            *self.similarity.entry(*k).or_insert(0) += v;
        }
        for (k, v) in &o.exact_by {
            *self.exact_by.entry(*k).or_insert(0) += v;
        }
        self.first_pick += o.first_pick;
        self.sessions.extend(o.sessions.iter().cloned());
        self.hook_answered += o.hook_answered;
        self.hook_exact += o.hook_exact;
    }

    /// Sessions per agent.
    pub fn sessions_by_agent(&self) -> BTreeMap<Agent, usize> {
        let mut m = BTreeMap::new();
        for (a, _) in &self.sessions {
            *m.entry(*a).or_insert(0) += 1;
        }
        m
    }
}

/// Everything learned from transcripts.
#[derive(Debug, Clone, Default)]
pub struct AgentScore {
    /// Per repository root (as recorded in the usage log).
    pub per_root: BTreeMap<PathBuf, AgentCounts>,
    /// wn calls found in transcripts with no logged answer (logging off, another machine, or an
    /// answer from a different wn home).
    pub unmatched: usize,
}

/// Matches each observation to the closest logged answer (each answer used once) and scores it.
pub fn score(
    observations: &[Observation],
    usage: &[RepoUsage],
    exists: &dyn Fn(&Path) -> bool,
) -> AgentScore {
    let events: Vec<(&Path, &QueryEvent)> = usage
        .iter()
        .filter_map(|r| r.root.as_deref().map(|root| (root, r)))
        .flat_map(|(root, r)| r.queries.iter().map(move |q| (root, q)))
        .collect();
    let mut used = vec![false; events.len()];
    let mut out = AgentScore::default();
    for o in observations {
        let best = events
            .iter()
            .enumerate()
            .filter(|(i, (_, q))| {
                !used[*i]
                    && q.ts + MATCH_BEFORE_S >= o.ts
                    && q.ts <= o.ts.saturating_add(MATCH_AFTER_S)
            })
            .min_by_key(|(_, (_, q))| q.ts.abs_diff(o.ts));
        let Some((i, (root, q))) = best else {
            out.unmatched += 1;
            continue;
        };
        used[i] = true;
        let c = out.per_root.entry(root.to_path_buf()).or_default();
        c.calls += 1;
        c.sessions.insert((o.agent, o.session.clone()));
        if q.state != "ok" || q.hinted.is_empty() {
            continue;
        }
        c.answered += 1;
        let t = touched(&o.calls, root, &q.hinted, exists);
        let (similarity, action, rank) = classify(&t, &q.hinted);
        *c.similarity.entry(similarity).or_insert(0) += 1;
        if let Some(a) = action {
            *c.exact_by.entry(a).or_insert(0) += 1;
        }
        c.first_pick += usize::from(rank == Some(1));
        if o.source == Source::Hook {
            c.hook_answered += 1;
            c.hook_exact += usize::from(similarity == Similarity::Exact);
        }
    }
    out
}

/// Reads transcripts under `roots` from `since` on and scores them. Files older than the first
/// logged answer cannot hold a matching call and are skipped.
pub fn collect(roots: &Roots, usage: &[RepoUsage], since: u64) -> AgentScore {
    let first = usage
        .iter()
        .flat_map(|r| r.queries.iter().map(|q| q.ts))
        .min();
    let Some(first) = first else {
        return AgentScore::default();
    };
    let since = since.max(first.saturating_sub(MATCH_AFTER_S));
    let files = transcripts(roots, since);
    score(&scan(&files, since), usage, &|p: &Path| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(cmd: &str) -> Item {
        Item::Tool(Followed {
            shell: vec![cmd.into()],
            ..Followed::default()
        })
    }

    fn fshell(cmd: &str) -> Followed {
        Followed {
            shell: vec![cmd.into()],
            ..Followed::default()
        }
    }

    fn call(ts: u64) -> Item {
        Item::Call {
            ts,
            source: Source::Cli,
        }
    }

    #[test]
    fn table_is_unique_and_done_is_terminal() {
        for (i, (a, e, _)) in TRANSITIONS.iter().enumerate() {
            assert!(
                !TRANSITIONS[i + 1..]
                    .iter()
                    .any(|(b, f, _)| a == b && e == f),
                "duplicate {a:?} × {e:?}"
            );
        }
        for e in TRACK_EVENTS {
            assert!(step(TRANSITIONS, TrackState::Done, e).is_err());
        }
    }

    #[test]
    fn every_state_event_pair_is_decided() {
        // Exhaustive: each (state, event) is either a listed transition or rejected unchanged.
        for s in TRACK_STATES {
            for e in TRACK_EVENTS {
                let listed = TRANSITIONS.iter().any(|(f, on, _)| *f == s && *on == e);
                let r = step(TRANSITIONS, s, e);
                assert_eq!(listed, r.is_ok(), "{s:?} × {e:?}");
                if let Err(i) = r {
                    assert_eq!(i.state, s);
                }
                if e == TrackEvent::Limit {
                    assert_eq!(r.is_ok(), s == TrackState::Following);
                }
                if e == TrackEvent::Eof {
                    assert_eq!(r.is_ok(), s != TrackState::Done);
                }
            }
        }
    }

    #[test]
    fn follows_until_user_turn() {
        let mut t = Tracker::new(Agent::ClaudeCode, "s");
        t.feed(shell("before")).unwrap();
        t.feed(call(10)).unwrap();
        t.feed(shell("cat src/a.rs")).unwrap();
        t.feed(Item::UserTurn).unwrap();
        t.feed(shell("after")).unwrap();
        let obs = t.finish();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].calls.len(), 1);
        assert_eq!(obs[0].ts, 10);
    }

    #[test]
    fn stops_after_limit_and_splits_on_next_call() {
        let mut t = Tracker::new(Agent::Codex, "s");
        t.feed(call(1)).unwrap();
        for i in 0..FOLLOW_CALLS + 3 {
            t.feed(shell(&format!("t{i}"))).unwrap();
        }
        assert_eq!(t.state(), TrackState::Seeking);
        t.feed(call(2)).unwrap();
        t.feed(shell("x")).unwrap();
        t.feed(call(3)).unwrap();
        let obs = t.finish();
        let lens: Vec<usize> = obs.iter().map(|o| o.calls.len()).collect();
        assert_eq!(lens, vec![FOLLOW_CALLS, 1, 0]);
    }

    #[test]
    fn hook_waits_through_the_prompt() {
        let mut t = Tracker::new(Agent::ClaudeCode, "s");
        t.feed(Item::Call {
            ts: 5,
            source: Source::Hook,
        })
        .unwrap();
        t.feed(Item::UserTurn).unwrap();
        assert_eq!(t.state(), TrackState::Armed);
        t.feed(shell("cat src/a.rs")).unwrap();
        assert_eq!(t.state(), TrackState::Following);
        let obs = t.finish();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].calls.len(), 1);
        assert_eq!(obs[0].source, Source::Hook);
    }

    #[test]
    fn recognises_wn_ask_commands() {
        for yes in [
            r#"wn ask "where is x""#,
            r#"cd ~/r && wn --json ask "x""#,
            r#"wn --path ~/src/p --json ask x"#,
            r#"/tmp/b/target/debug/wn ask -k 5 x"#,
            r#"WN_NO_DAEMON=1 wn ask x"#,
            r#"cargo test 2>&1 | wn ask "fix" --context-file -"#,
        ] {
            assert!(is_wn_ask(yes), "{yes}");
        }
        for no in [
            "wn status",
            "wn init",
            "own ask",
            "echo known ask",
            "wn skill sync",
            "cat wn/ask.rs",
        ] {
            assert!(!is_wn_ask(no), "{no}");
        }
    }

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_ts("2026-09-29T20:53:13.725Z"), Some(1_790_715_193));
        assert_eq!(parse_ts("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_ts("nope"), None);
    }

    fn touches(cmd: &str) -> Vec<(String, Action)> {
        shell_touches(cmd)
            .into_iter()
            .map(|(p, a)| (p.to_string_lossy().into_owned(), a))
            .collect()
    }

    #[test]
    fn shell_commands_name_files_with_actions() {
        use Action::*;
        let s = |p: &str, a| (p.to_string(), a);
        assert_eq!(touches("cat src/a.rs"), vec![s("src/a.rs", Read)]);
        assert_eq!(
            touches("grep -n foo src/bar.py | head"),
            vec![s("foo", Read), s("src/bar.py", Read)]
        );
        assert_eq!(
            touches("uv run pytest tests/test_x.py::test_y -q"),
            vec![s("tests/test_x.py", Ran)]
        );
        assert_eq!(
            touches("python src/foo.py --n=3"),
            vec![s("src/foo.py", Ran), s("3", Ran)]
                .into_iter()
                .filter(|(p, _)| p != "3")
                .collect::<Vec<_>>()
        );
        assert_eq!(
            touches("git diff src/a.rs"),
            vec![s("diff", Read), s("src/a.rs", Read)]
        );
        assert_eq!(
            touches("sed -i 's/a/b/' src/a.rs"),
            vec![s("s/a/b/", Edited), s("src/a.rs", Edited)]
        );
        assert_eq!(
            touches("echo x > out/log.txt"),
            vec![s("x", Read), s("out/log.txt", Edited)]
        );
        assert_eq!(
            touches("cd sub && cat ./a.rs:12"),
            vec![s("sub/a.rs", Read)]
        );
        assert_eq!(
            touches("./scripts/check.sh"),
            vec![s("scripts/check.sh", Ran)]
        );
        assert!(touches("ls src/*.rs").is_empty());
        assert!(touches("curl https://x.io/a.rs").is_empty());
        assert_eq!(
            touches("RUST_LOG=1 cargo test -p wn"),
            vec![s("test", Ran), s("wn", Ran)]
        );
    }

    #[test]
    fn cleans_tokens() {
        assert_eq!(clean_token("src/a.rs:12:3").as_deref(), Some("src/a.rs"));
        assert_eq!(
            clean_token("tests/t.py::test_x").as_deref(),
            Some("tests/t.py")
        );
        assert_eq!(clean_token("./a.rs,").as_deref(), Some("a.rs"));
        assert_eq!(clean_token("src/a.rs#L10").as_deref(), Some("src/a.rs"));
        assert_eq!(clean_token("12"), None);
        assert_eq!(clean_token("src/*.rs"), None);
        assert_eq!(clean_token("$HOME/x"), None);
    }

    #[test]
    fn relative_paths() {
        let root = Path::new("/r");
        assert_eq!(
            relative_to(root, Path::new("src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            relative_to(root, Path::new("/r/src/../b.rs")).as_deref(),
            Some("b.rs")
        );
        assert_eq!(relative_to(root, Path::new("/elsewhere/a.rs")), None);
        assert_eq!(
            relative_to(Path::new("/tmp/r"), Path::new("/private/tmp/r/a.rs")).as_deref(),
            Some("a.rs")
        );
        assert_eq!(
            relative_to(Path::new("/private/tmp/r"), Path::new("/tmp/r/a.rs")).as_deref(),
            Some("a.rs")
        );
    }

    #[test]
    fn touched_files_need_to_exist_unless_hinted() {
        let root = Path::new("/r");
        let exists = |p: &Path| p == Path::new("/r/src/b.rs");
        let calls = vec![
            fshell("grep -n parse src/b.rs src/gone.rs"),
            Followed {
                reads: vec!["/r/src/a.rs".into()],
                ..Followed::default()
            },
            Followed {
                edits: vec!["/r/src/b.rs".into()],
                ..Followed::default()
            },
        ];
        let t = touched(&calls, root, &["src/gone.rs".into()], &exists);
        assert_eq!(
            t,
            vec![
                ("src/b.rs".into(), Action::Edited),
                ("src/gone.rs".into(), Action::Read),
                ("src/a.rs".into(), Action::Read),
            ]
        );
        // Another repository's file counts (as an absolute path), a relative miss does not.
        let other = |p: &Path| p == Path::new("/other/tests/test_a.rs");
        let t = touched(
            &[fshell("cd /other && cat tests/test_a.rs nope.rs")],
            root,
            &[],
            &other,
        );
        assert_eq!(t, vec![("/other/tests/test_a.rs".into(), Action::Read)]);
        let (sim, _, _) = classify(&t, &["src/a.rs".into()]);
        assert_eq!(sim, Similarity::Elsewhere, "outside files are never near");
    }

    #[test]
    fn near_means_same_dir_or_test_pair() {
        assert!(is_near("src/parse/b.rs", "src/parse/a.rs"));
        assert!(is_near("tests/test_foo.py", "pkg/foo.py"));
        assert!(is_near("pkg/foo_test.go", "cmd/foo.go"));
        assert!(is_near("src/Foo.java", "test/FooTest.java"));
        assert!(is_near("web/foo.test.ts", "lib/foo.ts"));
        assert!(
            !is_near("README.md", "Cargo.toml"),
            "root files are not near"
        );
        assert!(
            !is_near("a/mod.rs", "b/mod.rs"),
            "same name, neither a test"
        );
    }

    #[test]
    fn classifies_answers() {
        use Similarity::{Elsewhere, Exact, Near};
        let h = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        let t = |v: &[(&str, Action)]| -> Vec<(String, Action)> {
            v.iter().map(|(p, a)| (p.to_string(), *a)).collect()
        };
        assert_eq!(
            classify(
                &t(&[
                    ("x.rs", Action::Read),
                    ("src/b.rs", Action::Read),
                    ("src/a.rs", Action::Edited)
                ]),
                &h
            ),
            (Exact, Some(Action::Edited), Some(2))
        );
        assert_eq!(
            classify(&t(&[("src/c.rs", Action::Read)]), &h),
            (Near, None, None)
        );
        assert_eq!(
            classify(&t(&[("docs/x.md", Action::Read)]), &h),
            (Elsewhere, None, None)
        );
        assert_eq!(classify(&[], &h), (Similarity::None, None, None));
    }

    #[test]
    fn claude_lines() {
        let call = r#"{"type":"assistant","timestamp":"1970-01-01T00:00:10Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"wn ask \"where is x\""}}]}}"#;
        assert_eq!(
            claude_items(call, false),
            vec![Item::Call {
                ts: 10,
                source: Source::Cli
            }]
        );
        let mcp = r#"{"type":"assistant","timestamp":"1970-01-01T00:00:11Z","message":{"content":[{"type":"tool_use","name":"mcp__where-next__where_next","input":{"query":"x"}}]}}"#;
        assert_eq!(
            claude_items(mcp, false),
            vec![Item::Call {
                ts: 11,
                source: Source::Mcp
            }]
        );
        let read = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/r/src/a.rs"}}]}}"#;
        assert!(
            claude_items(read, false).is_empty(),
            "not needed while seeking"
        );
        assert_eq!(
            claude_items(read, true),
            vec![Item::Tool(Followed {
                reads: vec!["/r/src/a.rs".into()],
                ..Followed::default()
            })]
        );
        let write = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/r/src/a.rs","old_string":"a","new_string":"b"}}]}}"#;
        assert_eq!(
            claude_items(write, true),
            vec![Item::Tool(Followed {
                edits: vec!["/r/src/a.rs".into()],
                ..Followed::default()
            })]
        );
        let user = r#"{"type":"user","message":{"role":"user","content":"next task"}}"#;
        assert_eq!(claude_items(user, true), vec![Item::UserTurn]);
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}"#;
        assert!(claude_items(result, true).is_empty());
        let meta =
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"skill text"}}"#;
        assert!(claude_items(meta, true).is_empty());
        let hook = format!(
            r#"{{"type":"attachment","timestamp":"1970-01-01T00:00:09Z","attachment":{{"content":"{HOOK_MARKER} these files"}}}}"#
        );
        assert_eq!(
            claude_items(&hook, false),
            vec![Item::Call {
                ts: 9,
                source: Source::Hook
            }]
        );
        let quoted = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":"{HOOK_MARKER}"}}]}}}}"#
        );
        assert!(
            claude_items(&quoted, false).is_empty(),
            "quoted marker is not a hook"
        );
    }

    #[test]
    fn codex_lines() {
        let call = r#"{"timestamp":"1970-01-01T00:00:20Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.exec_command({cmd:\"cd /r && wn ask \\\"where is x\\\"\"}));"}}"#;
        assert_eq!(
            codex_items(call, false),
            vec![Item::Call {
                ts: 20,
                source: Source::Cli
            }]
        );
        let shell = r#"{"timestamp":"1970-01-01T00:00:21Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"wn --json ask x\"]}"}}"#;
        assert_eq!(codex_items(shell, false).len(), 1);
        let exec = r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.exec_command({cmd:\"sed -n 1,40p /r/src/a.rs\",\"max_output_tokens\":800}));"}}"#;
        assert_eq!(
            codex_items(exec, true),
            vec![Item::Tool(Followed {
                shell: vec!["sed -n 1,40p /r/src/a.rs".into()],
                ..Followed::default()
            })]
        );
        let patch = r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.apply_patch(\"*** Begin Patch\\n*** Update File: /r/src/a.rs\\n@@\\n-a\\n+b\\n*** End Patch\"));"}}"#;
        let items = codex_items(patch, true);
        let [Item::Tool(f)] = items.as_slice() else {
            panic!()
        };
        assert_eq!(f.edits, vec!["/r/src/a.rs".to_string()]);
        let user = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]}}"#;
        assert_eq!(codex_items(user, true), vec![Item::UserTurn]);
        assert!(codex_items(user, false).is_empty());
    }

    fn q(ts: u64, state: &str, hinted: &[&str]) -> QueryEvent {
        QueryEvent {
            ts,
            kind: "request".into(),
            state: state.into(),
            ms: 10,
            model: "m".into(),
            adapter: true,
            files: 100,
            hinted: hinted.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn obs(ts: u64, cmds: &[&str]) -> Observation {
        Observation {
            agent: Agent::ClaudeCode,
            session: format!("s{ts}"),
            ts,
            source: Source::Cli,
            calls: cmds.iter().map(|c| fshell(c)).collect(),
        }
    }

    #[test]
    fn scores_against_the_closest_logged_answer() {
        let usage = vec![RepoUsage {
            root: Some(PathBuf::from("/r")),
            queries: vec![
                q(100, "ok", &["src/a.rs", "src/b.rs"]),
                q(200, "ok", &["src/c.rs"]),
                q(300, "abstain", &[]),
                q(400, "ok", &["src/d.rs"]),
                q(500, "ok", &["src/e.rs"]),
            ],
            index: None,
            bench: None,
        }];
        let exists = |_: &Path| true;
        let observations = vec![
            obs(99, &["cat src/b.rs", "sed -i s/a/b/ src/a.rs"]), // exact, edited, first #2
            obs(199, &["cat src/other.rs"]),                      // near (same dir)
            obs(299, &[]),                                        // abstained
            obs(399, &["cat docs/x.md"]),                         // elsewhere
            obs(499, &["ls"]),                                    // none
            obs(5_000, &[]),                                      // nothing logged
        ];
        let s = score(&observations, &usage, &exists);
        let c = &s.per_root[Path::new("/r")];
        assert_eq!((c.calls, c.answered), (5, 4));
        assert_eq!(
            [
                Similarity::Exact,
                Similarity::Near,
                Similarity::Elsewhere,
                Similarity::None
            ]
            .map(|x| c.count(x)),
            [1, 1, 1, 1]
        );
        assert_eq!(c.exact_action(Action::Edited), 1);
        assert_eq!(c.first_pick, 0);
        assert_eq!(s.unmatched, 1);
    }

    #[test]
    fn a_search_hook_injection_is_counted_once_and_scored_as_a_hook() {
        // Claude Code writes the context (`hook_additional_context`) and the raw stdout
        // (`hook_success`) for the same PostToolUse hook; only the first is an injection.
        let context = format!(
            r#"{{"type":"attachment","timestamp":"1970-01-01T00:01:40Z","attachment":{{"type":"hook_additional_context","content":["{HOOK_MARKER} these files for the search `retry`; verify before relying on them:\n- src/retry.rs"],"hookName":"PostToolUse:Bash","hookEvent":"PostToolUse"}}}}"#
        );
        let success = format!(
            r#"{{"type":"attachment","timestamp":"1970-01-01T00:01:40Z","attachment":{{"type":"hook_success","hookName":"PostToolUse:Bash","stdout":"{{\"hookSpecificOutput\":{{\"additionalContext\":\"{HOOK_MARKER} these files\"}}}}","exitCode":0}}}}"#
        );
        assert!(claude_items(&success, false).is_empty());
        let lines = [
            r#"{"type":"user","message":{"content":"fix the retries"}}"#.to_string(),
            r#"{"type":"assistant","timestamp":"1970-01-01T00:01:39Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"rg RetryPolicy"}}]}}"#.to_string(),
            context,
            success,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/r/src/retry.rs"}}]}}"#.to_string(),
        ];
        let o = observe(Agent::ClaudeCode, "s", &lines.join("\n"));
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!((o[0].ts, o[0].source), (100, Source::Hook));
        let usage = vec![RepoUsage {
            root: Some(PathBuf::from("/r")),
            queries: vec![q(100, "ok", &["src/retry.rs"])],
            index: None,
            bench: None,
        }];
        let c = &score(&o, &usage, &|_: &Path| true).per_root[Path::new("/r")];
        assert_eq!((c.hook_answered, c.hook_exact), (1, 1));
    }

    #[test]
    fn codex_hook_context_is_a_hook() {
        let dev = format!(
            r#"{{"timestamp":"1970-01-01T00:00:30Z","type":"response_item","payload":{{"type":"message","role":"developer","content":[{{"type":"input_text","text":"{HOOK_MARKER} starting with these files"}}]}}}}"#
        );
        assert_eq!(
            codex_items(&dev, false),
            vec![Item::Call {
                ts: 30,
                source: Source::Hook
            }]
        );
        let output = format!(
            r#"{{"timestamp":"1970-01-01T00:00:31Z","type":"response_item","payload":{{"type":"function_call_output","output":"{HOOK_MARKER}"}}}}"#
        );
        assert!(
            codex_items(&output, false).is_empty(),
            "tool output quoting the marker"
        );
    }

    #[test]
    fn observes_a_whole_claude_transcript() {
        let lines = [
            r#"{"type":"user","message":{"content":"find the parser"}}"#,
            r#"{"type":"assistant","timestamp":"1970-01-01T00:01:40Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"wn ask \"where is the parser\""}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"src/parse.rs"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/r/src/parse.rs"}}]}}"#,
            r#"{"type":"user","message":{"content":"thanks"}}"#,
        ];
        let o = observe(Agent::ClaudeCode, "s", &lines.join("\n"));
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].ts, 100);
        let hinted = vec!["src/parse.rs".to_string()];
        let t = touched(&o[0].calls, Path::new("/r"), &hinted, &|_| false);
        assert_eq!(
            classify(&t, &hinted),
            (Similarity::Exact, Some(Action::Read), Some(1))
        );
    }
}
